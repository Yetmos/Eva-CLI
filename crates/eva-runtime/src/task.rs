//! V0.5 task status, log, cancellation, and replay report types.

use eva_core::{EvaError, RequestId};
use eva_storage::{
    TaskStateDeadLetterSnapshot, TaskStateLogSnapshot, TaskStateReplaySnapshot, TaskStateSnapshot,
};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "task status, logs, cancellation, retry, and replay reporting";

/// Stable task state exposed by CLI task commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// One task log entry. V0.5 keeps logs in-memory/report form; CLI persists the
/// latest report under `.eva/tasks` for follow-up inspection commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLogEntry {
    pub sequence: u64,
    pub level: TaskLogLevel,
    pub message: String,
}

/// Stable log level labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskLogLevel {
    Info,
    Warning,
    Error,
}

/// Retry policy applied to one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: usize,
}

/// Cancellation marker returned by `task cancel` and embedded in run reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CancellationRecord {
    pub requested: bool,
    pub accepted: bool,
    pub reason: Option<String>,
}

/// V0.5 task summary shared by `run`, `task status`, `task logs`, and
/// `task cancel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReport {
    pub task_id: RequestId,
    pub status: TaskStatus,
    pub attempts: usize,
    pub retry_policy: RetryPolicy,
    pub cancellation: CancellationRecord,
    pub logs: Vec<TaskLogEntry>,
    pub dead_letters: Vec<DeadLetterSummary>,
    pub replayed_events: Vec<ReplaySummary>,
    pub error: Option<EvaError>,
}

/// Dead-letter summary exposed without leaking full payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterSummary {
    pub event_id: String,
    pub topic: String,
    pub reason_kind: String,
    pub reason: String,
    pub replay_count: usize,
}

/// Replay summary for dead-letter events republished in V0.5 diagnostic flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySummary {
    pub event_id: String,
    pub sequence: u64,
    pub topic: String,
}

impl TaskStatus {
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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl TaskLogEntry {
    pub fn info(sequence: u64, message: impl Into<String>) -> Self {
        Self {
            sequence,
            level: TaskLogLevel::Info,
            message: message.into(),
        }
    }

    pub fn warning(sequence: u64, message: impl Into<String>) -> Self {
        Self {
            sequence,
            level: TaskLogLevel::Warning,
            message: message.into(),
        }
    }

    pub fn error(sequence: u64, message: impl Into<String>) -> Self {
        Self {
            sequence,
            level: TaskLogLevel::Error,
            message: message.into(),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 1 }
    }
}

impl RetryPolicy {
    pub fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
        }
    }
}

impl CancellationRecord {
    pub fn requested(reason: impl Into<String>) -> Self {
        Self {
            requested: true,
            accepted: true,
            reason: Some(reason.into()),
        }
    }
}

impl TaskReport {
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

    pub fn push_log(&mut self, level: TaskLogLevel, message: impl Into<String>) {
        let sequence = self.logs.len() as u64 + 1;
        self.logs.push(TaskLogEntry {
            sequence,
            level,
            message: message.into(),
        });
    }

    pub fn complete(&mut self, attempts: usize) {
        self.status = TaskStatus::Completed;
        self.attempts = attempts;
        self.error = None;
    }

    pub fn fail(&mut self, attempts: usize, error: EvaError) {
        self.status = if error.kind() == eva_core::ErrorKind::Timeout {
            TaskStatus::TimedOut
        } else {
            TaskStatus::Failed
        };
        self.attempts = attempts;
        self.error = Some(error);
    }

    pub fn cancel(&mut self, reason: impl Into<String>) {
        self.status = TaskStatus::Cancelled;
        self.cancellation = CancellationRecord::requested(reason);
    }
}

impl From<&TaskReport> for TaskStateSnapshot {
    fn from(report: &TaskReport) -> Self {
        Self {
            task_id: report.task_id.as_str().to_owned(),
            status: report.status.as_str().to_owned(),
            attempts: report.attempts,
            retry_max_attempts: report.retry_policy.max_attempts,
            cancel_requested: report.cancellation.requested,
            cancel_accepted: report.cancellation.accepted,
            cancel_reason: report.cancellation.reason.clone(),
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
