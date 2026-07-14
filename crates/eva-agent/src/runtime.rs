//! 中文：Agent 事件处理边界，统一拥有取消、超时和重试语义。
//! Agent event handling boundary and timeout ownership.

use crate::lifecycle::{AgentLifecycle, AgentLifecycleState};
use crate::queue::AgentQueue;
use eva_core::{AgentId, EvaError, Event, EventId, Topic};
use std::time::{Duration, Instant};

/// 中文：本模块负责围绕一次 Agent 处理操作应用队列、生命周期和执行控制。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent event handling boundary and timeout ownership";

/// 中文：Lua 宿主或注入测试处理器返回的规范化结果。
/// Handler output produced by Lua host or a test handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHandlerOutput {
    /// 中文：处理器定义的稳定业务状态。
    pub status: String,
    /// 中文：可选的文本结果；没有结果并不表示处理失败。
    pub output: Option<String>,
}

/// 中文：一个事件在 Agent 中完成处理后的完整执行记录。
/// Result of one Agent event handling attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunRecord {
    /// 中文：执行该事件的 Agent 标识。
    pub agent_id: AgentId,
    /// 中文：被消费事件的稳定标识。
    pub event_id: EventId,
    /// 中文：事件主题快照，便于事件出队后继续诊断。
    pub topic: Topic,
    /// 中文：运行时归一化后的最终处理状态。
    pub status: AgentRunStatus,
    /// 中文：实际调用处理器的尝试次数；执行前取消时为零。
    pub attempts: usize,
    /// 中文：处理成功时由处理器返回的业务状态。
    pub handler_status: Option<String>,
    /// 中文：处理成功时的可选输出。
    pub output: Option<String>,
    /// 中文：取消、超时或处理失败时保留的结构化错误。
    pub error: Option<EvaError>,
}

/// 中文：应用于单次 Agent 事件处理的运行控制参数。
/// Runtime controls applied around one Agent event handling operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunControl {
    /// 中文：单次处理的可选时长预算；零时长表示立即超时且不调用处理器。
    pub timeout: Option<Duration>,
    /// 中文：是否在执行处理器前取消本次运行。
    pub cancel_requested: bool,
    /// 中文：可选取消令牌，仅用于关联诊断和审计上下文。
    pub cancel_token: Option<String>,
    /// 中文：调用方提供的绝对截止时间毫秒值，用于保留操作员上下文。
    pub deadline_at_ms: Option<u128>,
    /// 中文：包括首次执行在内的最大尝试次数，内部保证至少为一。
    pub max_attempts: usize,
}

/// 中文：运行时对处理结果归一化后的稳定状态。
/// Stable handling status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRunStatus {
    /// 中文：处理器成功返回结果。
    Handled,
    /// 中文：处理器以非超时错误结束，且不再允许重试。
    Failed,
    /// 中文：在调用处理器前收到取消请求。
    Cancelled,
    /// 中文：处理超过配置的时间预算。
    TimedOut,
}

/// 中文：最小同步 Agent 运行时，拥有一个生命周期守卫和一条私有事件队列。
/// Minimal synchronous Agent runtime for the V0.4 loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntime {
    /// 中文：当前运行时代表的 Agent 标识。
    agent_id: AgentId,
    /// 中文：控制接收和执行资格的生命周期状态机。
    lifecycle: AgentLifecycle,
    /// 中文：等待该 Agent 处理的有界事件队列。
    queue: AgentQueue,
}

impl AgentHandlerOutput {
    /// 中文：从业务状态和可选输出构造处理器结果。
    pub fn new(status: impl Into<String>, output: Option<String>) -> Self {
        Self {
            status: status.into(),
            output,
        }
    }
}

impl AgentRunStatus {
    /// 中文：返回用于日志和协议输出的稳定状态字符串。
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
    /// 中文：默认不取消、不限时且只尝试一次，保持最保守的执行语义。
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
    /// 中文：设置单次处理的时长预算。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 中文：设置执行前取消标志。
    pub fn with_cancel_requested(mut self, cancel_requested: bool) -> Self {
        self.cancel_requested = cancel_requested;
        self
    }

    /// 中文：附加用于追踪取消来源的令牌。
    pub fn with_cancel_token(mut self, cancel_token: impl Into<String>) -> Self {
        self.cancel_token = Some(cancel_token.into());
        self
    }

    /// 中文：附加调用方计算的截止时间，供错误上下文和审计使用。
    pub fn with_deadline_at_ms(mut self, deadline_at_ms: u128) -> Self {
        self.deadline_at_ms = Some(deadline_at_ms);
        self
    }

    /// 中文：设置最大尝试次数；输入零会被提升为一次，避免事件出队后完全不处理。
    pub fn with_max_attempts(mut self, max_attempts: usize) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }
}

impl AgentRuntime {
    /// 中文：创建指定 Agent 的运行时；队列容量无效时构造失败且不产生半初始化实例。
    pub fn new(agent_id: AgentId, queue_capacity: usize) -> Result<Self, EvaError> {
        Ok(Self {
            agent_id,
            lifecycle: AgentLifecycle::new(),
            queue: AgentQueue::new(queue_capacity)?,
        })
    }

    /// 中文：返回当前 Agent 的稳定标识。
    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    /// 中文：返回当前生命周期状态。
    pub fn state(&self) -> AgentLifecycleState {
        self.lifecycle.state()
    }

    /// 中文：按生命周期规则启动或重新启动 Agent。
    pub fn start(&mut self) -> Result<(), EvaError> {
        self.lifecycle.start()
    }

    /// 中文：仅在运行态接收事件，并把队列容量错误原样传给调用方。
    pub fn accept(&mut self, event: Event) -> Result<(), EvaError> {
        if self.lifecycle.state() != AgentLifecycleState::Running {
            return Err(EvaError::conflict("agent runtime is not running")
                .with_context("agent_id", self.agent_id.as_str())
                .with_context("state", self.lifecycle.state().as_str()));
        }
        self.queue.enqueue(event)
    }

    /// 中文：使用默认执行控制处理队首事件；队列为空时不调用处理器并返回 `None`。
    pub fn run_next<F>(&mut self, handler: F) -> Option<AgentRunRecord>
    where
        F: FnMut(&AgentId, &Event) -> Result<AgentHandlerOutput, EvaError>,
    {
        self.run_next_with_control(AgentRunControl::default(), handler)
    }

    /// 中文：取出并处理一个事件，同时应用取消、超时和有限重试策略。
    ///
    /// 事件一旦出队便由本次调用独占。取消请求在处理器前短路且尝试次数为零；处理器
    /// 只有返回可重试错误时才继续尝试。当前同步实现只能在调用前识别零预算，并在调用
    /// 返回后比较耗时，不能强制中断正在运行的处理器，因此处理器自身仍需遵守截止时间。
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

        // 中文：取消必须先于任何处理器副作用，同时把关联令牌和截止时间保留到错误上下文。
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

        // 中文：仅重试明确标记为可重试的错误；成功、不可重试错误或次数耗尽都会立即结束。
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

    /// 中文：返回尚未处理的队列长度。
    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }
}

/// 中文：判断同步处理调用返回时是否已经超过可选预算；未配置预算时永不超时。
fn exceeded_timeout(timeout: Option<Duration>, started: Instant) -> bool {
    timeout
        .map(|budget| started.elapsed() > budget)
        .unwrap_or(false)
}

/// 中文：构造带 Agent 标识和预算毫秒数的统一超时错误。
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

    /// 中文：构造 Agent 运行时测试使用的文本事件。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    #[test]
    /// 中文：验证只有运行态 Agent 才能接收事件。
    fn runtime_requires_running_state_before_accepting_events() {
        let mut runtime = AgentRuntime::new(AgentId::parse("root-agent").unwrap(), 2).unwrap();

        assert!(runtime.accept(event("evt-1")).is_err());

        runtime.start().unwrap();
        runtime.accept(event("evt-1")).unwrap();
        assert_eq!(runtime.queued_len(), 1);
    }

    #[test]
    /// 中文：验证运行时把 Agent 和事件传给注入处理器并记录成功结果。
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
    /// 中文：验证执行前取消不会调用处理器，且记录零次尝试。
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
    /// 中文：验证取消令牌和截止时间会进入结构化错误上下文。
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
    /// 中文：验证零预算会立即产生超时记录而不调用处理器。
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
    /// 中文：验证可重试错误会在尝试上限内再次调用处理器。
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
