//! Agent event handling boundary and timeout ownership.

use crate::lifecycle::{AgentLifecycle, AgentLifecycleState};
use crate::queue::AgentQueue;
use eva_core::{AgentId, EvaError, Event, EventId, Topic};

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
    pub handler_status: Option<String>,
    pub output: Option<String>,
    pub error: Option<EvaError>,
}

/// Stable handling status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRunStatus {
    Handled,
    Failed,
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
        }
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

    pub fn run_next<F>(&mut self, mut handler: F) -> Option<AgentRunRecord>
    where
        F: FnMut(&AgentId, &Event) -> Result<AgentHandlerOutput, EvaError>,
    {
        let event = self.queue.dequeue()?;
        let event_id = event.event_id().clone();
        let topic = event.topic().clone();
        let result = handler(&self.agent_id, &event);
        Some(match result {
            Ok(output) => AgentRunRecord {
                agent_id: self.agent_id.clone(),
                event_id,
                topic,
                status: AgentRunStatus::Handled,
                handler_status: Some(output.status),
                output: output.output,
                error: None,
            },
            Err(error) => AgentRunRecord {
                agent_id: self.agent_id.clone(),
                event_id,
                topic,
                status: AgentRunStatus::Failed,
                handler_status: None,
                output: None,
                error: Some(error),
            },
        })
    }

    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }
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
        assert_eq!(
            record.handler_status.as_deref(),
            Some("root-agent:/input/user")
        );
    }
}
