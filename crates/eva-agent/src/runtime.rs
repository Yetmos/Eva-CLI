//! Agent event handling boundary and timeout ownership.

use crate::lifecycle::{AgentLifecycle, AgentLifecycleState};
use crate::queue::AgentQueue;
use eva_core::{AgentId, EvaError, Event, EventId, Topic};
use std::time::{Duration, Instant};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent event handling boundary and timeout ownership";

/// Handler output produced by Lua host or a test handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHandlerOutput {
    pub status: String,
    pub output: Option<String>,
}

/// Result of one Agent event handling attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunRecord {
    pub agent_id: AgentId,
    pub event_id: EventId,
    pub topic: Topic,
    pub status: AgentRunStatus,
    pub attempts: usize,
    pub handler_status: Option<String>,
    pub output: Option<String>,
    pub error: Option<EvaError>,
}

/// Runtime controls applied around one Agent event handling operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunControl {
    pub timeout: Option<Duration>,
    pub cancel_requested: bool,
    pub cancel_token: Option<String>,
    pub deadline_at_ms: Option<u128>,
    pub max_attempts: usize,
}

/// Stable handling status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRunStatus {
    Handled,
    Failed,
    Cancelled,
    TimedOut,
}

/// Minimal synchronous Agent runtime for the V0.4 loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntime {
    agent_id: AgentId,
    lifecycle: AgentLifecycle,
    queue: AgentQueue,
}

impl AgentHandlerOutput {
    pub fn new(status: impl Into<String>, output: Option<String>) -> Self {
        Self {
            status: status.into(),
            output,
        }
    }
}

impl AgentRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Handled => "handled",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

impl Default for AgentRunControl {
    fn default() -> Self {
        Self {
            timeout: None,
            cancel_requested: false,
            cancel_token: None,
            deadline_at_ms: None,
            max_attempts: 1,
        }
    }
}

impl AgentRunControl {
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_cancel_requested(mut self, cancel_requested: bool) -> Self {
        self.cancel_requested = cancel_requested;
        self
    }

    pub fn with_cancel_token(mut self, cancel_token: impl Into<String>) -> Self {
        self.cancel_token = Some(cancel_token.into());
        self
    }

    pub fn with_deadline_at_ms(mut self, deadline_at_ms: u128) -> Self {
        self.deadline_at_ms = Some(deadline_at_ms);
        self
    }

    pub fn with_max_attempts(mut self, max_attempts: usize) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }
}

impl AgentRuntime {
    pub fn new(agent_id: AgentId, queue_capacity: usize) -> Result<Self, EvaError> {
        Ok(Self {
            agent_id,
            lifecycle: AgentLifecycle::new(),
            queue: AgentQueue::new(queue_capacity)?,
        })
    }

    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    pub fn state(&self) -> AgentLifecycleState {
        self.lifecycle.state()
    }

    pub fn start(&mut self) -> Result<(), EvaError> {
        self.lifecycle.start()
    }

    pub fn accept(&mut self, event: Event) -> Result<(), EvaError> {
        if self.lifecycle.state() != AgentLifecycleState::Running {
            return Err(EvaError::conflict("agent runtime is not running")
                .with_context("agent_id", self.agent_id.as_str())
                .with_context("state", self.lifecycle.state().as_str()));
        }
        self.queue.enqueue(event)
    }

    pub fn run_next<F>(&mut self, handler: F) -> Option<AgentRunRecord>
    where
        F: FnMut(&AgentId, &Event) -> Result<AgentHandlerOutput, EvaError>,
    {
        self.run_next_with_control(AgentRunControl::default(), handler)
    }

    pub fn run_next_with_control<F>(
        &mut self,
        control: AgentRunControl,
        mut handler: F,
    ) -> Option<AgentRunRecord>
    where
        F: FnMut(&AgentId, &Event) -> Result<AgentHandlerOutput, EvaError>,
    {
        let event = self.queue.dequeue()?;
        let event_id = event.event_id().clone();
        let topic = event.topic().clone();

        if control.cancel_requested {
            let mut error = EvaError::conflict("agent run was cancelled")
                .with_context("agent_id", self.agent_id.as_str())
                .with_retryable(false);
            if let Some(cancel_token) = &control.cancel_token {
                error = error.with_context("cancel_token", cancel_token);
            }
            if let Some(deadline_at_ms) = control.deadline_at_ms {
                error = error.with_context("deadline_at_ms", deadline_at_ms.to_string());
            }
            return Some(AgentRunRecord {
                agent_id: self.agent_id.clone(),
                event_id,
                topic,
                status: AgentRunStatus::Cancelled,
                attempts: 0,
                handler_status: None,
                output: None,
                error: Some(error),
            });
        }

        let max_attempts = control.max_attempts.max(1);

        for attempt in 1..=max_attempts {
            let error = if matches!(control.timeout, Some(timeout) if timeout.is_zero()) {
                Some(
                    EvaError::timeout("agent run exceeded timeout budget")
                        .with_context("agent_id", self.agent_id.as_str())
                        .with_context("timeout_ms", "0"),
                )
            } else {
                let started = Instant::now();
                match handler(&self.agent_id, &event) {
                    Ok(_output) if exceeded_timeout(control.timeout, started) => {
                        Some(timeout_error(&self.agent_id, control.timeout))
                    }
                    Ok(output) => {
                        return Some(AgentRunRecord {
                            agent_id: self.agent_id.clone(),
                            event_id,
                            topic,
                            status: AgentRunStatus::Handled,
                            attempts: attempt,
                            handler_status: Some(output.status),
                            output: output.output,
                            error: None,
                        });
                    }
                    Err(error) => Some(error),
                }
            }
            .expect("attempt always records an error or returns success");

            if !error.is_retryable() || attempt == max_attempts {
                let status = if error.kind() == eva_core::ErrorKind::Timeout {
                    AgentRunStatus::TimedOut
                } else {
                    AgentRunStatus::Failed
                };
                return Some(AgentRunRecord {
                    agent_id: self.agent_id.clone(),
                    event_id,
                    topic,
                    status,
                    attempts: attempt,
                    handler_status: None,
                    output: None,
                    error: Some(error),
                });
            }
        }

        None
    }

    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }
}

fn exceeded_timeout(timeout: Option<Duration>, started: Instant) -> bool {
    timeout
        .map(|budget| started.elapsed() > budget)
        .unwrap_or(false)
}

fn timeout_error(agent_id: &AgentId, timeout: Option<Duration>) -> EvaError {
    let timeout_ms = timeout
        .map(|timeout| timeout.as_millis().to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    EvaError::timeout("agent run exceeded timeout budget")
        .with_context("agent_id", agent_id.as_str())
        .with_context("timeout_ms", timeout_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventPayload, Topic};

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    #[test]
    fn runtime_requires_running_state_before_accepting_events() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();

        assert!(runtime.accept(event("evt-1")).is_err());

        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();
        assert_eq!(runtime.queued_len(), 1);
    }

    #[test]
    fn runtime_runs_injected_handler() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();
        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();

        let record = runtime
            .run_next(|agent_id, event| {
                Ok(AgentHandlerOutput::new(
                    format!("{}:{}", agent_id, event.topic()),
                    Some("ok".to_owned()),
                ))
            })
            .unwrap();

        assert_eq!(record.status, AgentRunStatus::Handled);
        assert_eq!(record.attempts, 1);
        assert_eq!(
            record.handler_status.as_deref(),
            Some("root-agent:/input/user")
        );
    }

    #[test]
    fn runtime_records_cancelled_run_before_handler() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();
        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();

        let record = runtime
            .run_next_with_control(
                AgentRunControl::default().with_cancel_requested(true),
                |_agent_id, _event| Ok(AgentHandlerOutput::new("unreachable", None)),
            )
            .unwrap();

        assert_eq!(record.status, AgentRunStatus::Cancelled);
        assert_eq!(record.attempts, 0);
        assert_eq!(record.error.unwrap().kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn runtime_records_cancel_token_and_deadline_on_cancel() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();
        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();

        let record = runtime
            .run_next_with_control(
                AgentRunControl::default()
                    .with_cancel_requested(true)
                    .with_cancel_token("cancel-token-1")
                    .with_deadline_at_ms(123),
                |_agent_id, _event| Ok(AgentHandlerOutput::new("unreachable", None)),
            )
            .unwrap();
        assert_eq!(record.status, AgentRunStatus::Cancelled);
        let error = record.error.unwrap();
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "cancel_token" && value == "cancel-token-1"));
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "deadline_at_ms" && value == "123"));
    }

    #[test]
    fn runtime_records_timeout_without_invoking_handler() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();
        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();

        let record = runtime
            .run_next_with_control(
                AgentRunControl::default().with_timeout(Duration::ZERO),
                |_agent_id, _event| Ok(AgentHandlerOutput::new("unreachable", None)),
            )
            .unwrap();

        assert_eq!(record.status, AgentRunStatus::TimedOut);
        assert_eq!(record.attempts, 1);
        assert_eq!(record.error.unwrap().kind(), eva_core::ErrorKind::Timeout);
    }

    #[test]
    fn runtime_retries_retryable_handler_error() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();
        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();
        let mut calls = 0;

        let record = runtime
            .run_next_with_control(
                AgentRunControl::default().with_max_attempts(2),
                |_agent_id, _event| {
                    calls += 1;
                    if calls == 1 {
                        Err(EvaError::unavailable("temporary handler failure"))
                    } else {
                        Ok(AgentHandlerOutput::new("accepted", Some("ok".to_owned())))
                    }
                },
            )
            .unwrap();

        assert_eq!(record.status, AgentRunStatus::Handled);
        assert_eq!(record.attempts, 2);
        assert_eq!(calls, 2);
    }
}
