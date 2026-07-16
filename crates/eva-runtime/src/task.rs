//! 中文：V0.5 任务状态、日志、取消、重试和死信重放报告契约。
//! V0.5 task status, log, cancellation, and replay report types.

use eva_core::{EvaError, RequestId};
use eva_storage::{
    StateVersion, TaskStateDeadLetterSnapshot, TaskStateLogSnapshot, TaskStateReplaySnapshot,
    TaskStateSnapshot, WriterGeneration,
};

/// 中文：本模块统一任务执行报告与持久化快照之间的稳定映射。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "task status, logs, cancellation, retry, and replay reporting";

/// 中文：CLI 任务命令对外暴露的稳定状态。
/// Stable task state exposed by CLI task commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    /// 中文：任务已登记但尚未开始执行。
    Queued,
    /// 中文：任务正在执行。
    Running,
    /// 中文：任务成功完成。
    Completed,
    /// 中文：任务因非超时错误失败。
    Failed,
    /// 中文：任务响应取消请求结束。
    Cancelled,
    /// 中文：任务超过执行时限。
    TimedOut,
}

/// 中文：一条具备稳定序号和级别的任务日志。V0.5 在运行时报告中保存日志，CLI 会把
/// 最新报告持久化到 `.eva/tasks`，供后续检查命令读取。
/// One task log entry. V0.5 keeps logs in-memory/report form; CLI persists the
/// latest report under `.eva/tasks` for follow-up inspection commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLogEntry {
    /// 中文：任务内部从一开始单调递增的日志序号。
    pub sequence: u64,
    /// 中文：用于过滤和展示的稳定日志级别。
    pub level: TaskLogLevel,
    /// 中文：面向操作员的日志文本。
    pub message: String,
}

/// 中文：任务日志使用的稳定级别标签。
/// Stable log level labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskLogLevel {
    /// 中文：普通进度和结果信息。
    Info,
    /// 中文：不终止任务但需要关注的异常情况。
    Warning,
    /// 中文：与失败或不可继续状态有关的信息。
    Error,
}

/// 中文：应用于单个任务的有限重试策略。
/// Retry policy applied to one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 中文：包括首次执行在内的最大尝试次数，始终至少为一。
    pub max_attempts: usize,
}

/// 中文：`task cancel` 返回并嵌入运行报告的取消结果。
/// Cancellation marker returned by `task cancel` and embedded in run reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CancellationRecord {
    /// 中文：调用方是否提出取消请求。
    pub requested: bool,
    /// 中文：运行时是否接受该取消请求。
    pub accepted: bool,
    /// 中文：可选的操作员取消原因。
    pub reason: Option<String>,
}

/// 中文：由 `run`、`task status`、`task logs` 和 `task cancel` 共享的任务摘要。
/// V0.5 task summary shared by `run`, `task status`, `task logs`, and
/// `task cancel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReport {
    /// 中文：任务使用的稳定请求标识。
    pub task_id: RequestId,
    /// 中文：当前归一化任务状态。
    pub status: TaskStatus,
    /// 中文：已经执行的尝试次数。
    pub attempts: usize,
    /// 中文：本任务采用的重试上限。
    pub retry_policy: RetryPolicy,
    /// 中文：取消请求和接受结果。
    pub cancellation: CancellationRecord,
    /// 中文：按序号排列的任务日志。
    pub logs: Vec<TaskLogEntry>,
    /// 中文：任务产生或关联的死信摘要。
    pub dead_letters: Vec<DeadLetterSummary>,
    /// 中文：从死信重新发布的事件摘要。
    pub replayed_events: Vec<ReplaySummary>,
    /// 中文：失败或超时时保留的结构化错误。
    pub error: Option<EvaError>,
}

/// 中文：不暴露完整载荷的死信摘要。
/// Dead-letter summary exposed without leaking full payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterSummary {
    /// 中文：原始死信事件标识。
    pub event_id: String,
    /// 中文：事件主题文本。
    pub topic: String,
    /// 中文：结构化错误类型名称。
    pub reason_kind: String,
    /// 中文：面向操作员的失败原因。
    pub reason: String,
    /// 中文：该死信已经被重放的次数。
    pub replay_count: usize,
}

/// 中文：V0.5 诊断流程重新发布死信时生成的回放摘要。
/// Replay summary for dead-letter events republished in V0.5 diagnostic flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySummary {
    /// 中文：新生成的重放事件标识。
    pub event_id: String,
    /// 中文：事件日志为重放事件分配的序号。
    pub sequence: u64,
    /// 中文：重放事件主题。
    pub topic: String,
}

impl TaskStatus {
    /// 中文：返回用于 CLI、JSON 和持久化快照的稳定状态名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

impl TaskLogLevel {
    /// 中文：返回用于 CLI 和持久化快照的稳定日志级别名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl TaskLogEntry {
    /// 中文：创建信息级任务日志。
    pub fn info(sequence: u64, message: impl Into<String>) -> Self {
        Self {
            sequence,
            level: TaskLogLevel::Info,
            message: message.into(),
        }
    }

    /// 中文：创建警告级任务日志。
    pub fn warning(sequence: u64, message: impl Into<String>) -> Self {
        Self {
            sequence,
            level: TaskLogLevel::Warning,
            message: message.into(),
        }
    }

    /// 中文：创建错误级任务日志。
    pub fn error(sequence: u64, message: impl Into<String>) -> Self {
        Self {
            sequence,
            level: TaskLogLevel::Error,
            message: message.into(),
        }
    }
}

impl Default for RetryPolicy {
    /// 中文：默认只执行一次，不隐式重复具有副作用的任务。
    fn default() -> Self {
        Self { max_attempts: 1 }
    }
}

impl RetryPolicy {
    /// 中文：创建有限重试策略；输入零会被提升为一次以避免任务永不执行。
    pub fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
        }
    }
}

impl CancellationRecord {
    /// 中文：创建已经提出并被运行时接受的取消记录。
    pub fn requested(reason: impl Into<String>) -> Self {
        Self {
            requested: true,
            accepted: true,
            reason: Some(reason.into()),
        }
    }
}

impl TaskReport {
    /// 中文：创建处于排队态、尚无执行副作用的新任务报告。
    pub fn new(task_id: RequestId, retry_policy: RetryPolicy) -> Self {
        Self {
            task_id,
            status: TaskStatus::Queued,
            attempts: 0,
            retry_policy,
            cancellation: CancellationRecord::default(),
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
            error: None,
        }
    }

    /// 中文：追加日志并按当前长度分配从一开始的稳定序号。
    pub fn push_log(&mut self, level: TaskLogLevel, message: impl Into<String>) {
        let sequence = self.logs.len() as u64 + 1;
        self.logs.push(TaskLogEntry {
            sequence,
            level,
            message: message.into(),
        });
    }

    /// 中文：把任务标记为完成，记录尝试次数并清除先前错误。
    pub fn complete(&mut self, attempts: usize) {
        self.status = TaskStatus::Completed;
        self.attempts = attempts;
        self.error = None;
    }

    /// 中文：记录任务失败；超时错误映射为专用状态，其他错误映射为普通失败。
    pub fn fail(&mut self, attempts: usize, error: EvaError) {
        self.status = if error.kind() == eva_core::ErrorKind::Timeout {
            TaskStatus::TimedOut
        } else {
            TaskStatus::Failed
        };
        self.attempts = attempts;
        self.error = Some(error);
    }

    /// 中文：把任务标记为已取消，并保存被接受的取消原因。
    pub fn cancel(&mut self, reason: impl Into<String>) {
        self.status = TaskStatus::Cancelled;
        self.cancellation = CancellationRecord::requested(reason);
    }
}

impl From<&TaskReport> for TaskStateSnapshot {
    /// 中文：把内存任务报告完整映射为持久化快照，同时保留日志和死信诊断信息。
    fn from(report: &TaskReport) -> Self {
        Self {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id: report.task_id.as_str().to_owned(),
            status: report.status.as_str().to_owned(),
            attempts: report.attempts,
            retry_max_attempts: report.retry_policy.max_attempts,
            cancel_requested: report.cancellation.requested,
            cancel_accepted: report.cancellation.accepted,
            cancel_reason: report.cancellation.reason.clone(),
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
            error_kind: report
                .error
                .as_ref()
                .map(|error| error.kind().as_str().to_owned()),
            error_message: report
                .error
                .as_ref()
                .map(|error| error.message().to_owned()),
            logs: report
                .logs
                .iter()
                .map(|entry| TaskStateLogSnapshot {
                    sequence: entry.sequence,
                    level: entry.level.as_str().to_owned(),
                    message: entry.message.clone(),
                })
                .collect(),
            dead_letters: report
                .dead_letters
                .iter()
                .map(|entry| TaskStateDeadLetterSnapshot {
                    event_id: entry.event_id.clone(),
                    topic: entry.topic.clone(),
                    reason_kind: entry.reason_kind.clone(),
                    reason: entry.reason.clone(),
                    replay_count: entry.replay_count,
                })
                .collect(),
            replayed_events: report
                .replayed_events
                .iter()
                .map(|entry| TaskStateReplaySnapshot {
                    event_id: entry.event_id.clone(),
                    sequence: entry.sequence,
                    topic: entry.topic.clone(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 中文：验证任务完成时保留尝试次数和单调日志序号。
    fn task_report_tracks_logs_and_completion() {
        let mut report = TaskReport::new(
            RequestId::parse("req-basic-1").unwrap(),
            RetryPolicy::new(2),
        );

        report.push_log(TaskLogLevel::Info, "accepted");
        report.complete(2);

        assert_eq!(report.status, TaskStatus::Completed);
        assert_eq!(report.attempts, 2);
        assert_eq!(report.logs[0].sequence, 1);
    }

    #[test]
    /// 中文：验证超时错误会映射为 `TimedOut` 而非普通失败。
    fn task_report_maps_timeout_failure() {
        let mut report = TaskReport::new(
            RequestId::parse("req-basic-1").unwrap(),
            RetryPolicy::default(),
        );

        report.fail(1, EvaError::timeout("agent timed out"));

        assert_eq!(report.status, TaskStatus::TimedOut);
        assert!(report.error.unwrap().is_retryable());
    }
}
