//! 中文：V0.5 任务状态、日志、取消、重试和死信重放报告契约。
//! V0.5 task status, log, cancellation, and replay report types.

use eva_core::{AgentId, EvaError, RequestId};
use eva_storage::{
    StateVersion, TaskAttemptPolicySnapshot, TaskEnvelopeSnapshot, TaskInputSnapshot,
    TaskStateDeadLetterSnapshot, TaskStateLogSnapshot, TaskStateReplaySnapshot, TaskStateSnapshot,
    WriterGeneration,
};
use std::fmt;

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

/// Handler registry 后续使用的稳定点分 task kind；合法但未注册的值仍可持久化。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskKind(String);

/// Effect ledger 后续使用的稳定幂等键；W1-L02 只校验并持久化，不执行去重。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

/// 单次任务从提交到恢复均保持不变的尝试策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAttemptPolicy {
    /// 包括首次执行在内的最大 attempt 数。
    pub max_attempts: u32,
    /// retryable failure 后的固定退避毫秒数。
    pub retry_backoff_ms: u64,
    /// 每次 attempt 的可选超时毫秒数。
    pub attempt_timeout_ms: Option<u64>,
}

/// 执行时解析并重新核验的 artifact key/digest 对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskArtifactRef {
    key: String,
    digest: String,
}

/// 完整任务输入；inline 与 artifact 引用在类型层互斥。
#[derive(Clone, PartialEq, Eq)]
pub enum TaskInput {
    /// 直接随任务信封持久化的不透明 bytes 与其摘要。
    Inline {
        /// 原始 payload bytes。
        bytes: Vec<u8>,
        /// 构造时计算并在重开时重验的 canonical SHA-256。
        digest: String,
    },
    /// 延迟到 handler 执行边界读取的 artifact 引用。
    Artifact(TaskArtifactRef),
}

impl fmt::Debug for TaskInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline { bytes, digest } => formatter
                .debug_struct("TaskInput")
                .field("kind", &"inline")
                .field("bytes", &"<redacted>")
                .field("size_bytes", &bytes.len())
                .field("digest", digest)
                .finish(),
            Self::Artifact(reference) => formatter
                .debug_struct("TaskInput")
                .field("kind", &"artifact")
                .field("artifact_ref", &reference.key())
                .field("digest", &reference.digest())
                .finish(),
        }
    }
}

/// 任务业务身份；生命周期状态变化不得修改其中任一字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEnvelope {
    kind: TaskKind,
    agent_id: AgentId,
    input: TaskInput,
    idempotency_key: IdempotencyKey,
    attempt_policy: TaskAttemptPolicy,
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
    /// 完整提交信封；legacy/basic-run 报告没有可恢复 handler payload 时为 None。
    pub envelope: Option<TaskEnvelope>,
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

impl TaskKind {
    /// 解析持久 handler key；只做语法校验，unknown-handler 判定属于 W1-L05。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        TaskEnvelopeSnapshot::validate_kind(value)?;
        Ok(Self(value.to_owned()))
    }

    /// 返回经过校验的点分 kind。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl IdempotencyKey {
    /// 复用稳定 ID 字符集创建幂等键，但不检查跨任务唯一性。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        RequestId::parse(value).map_err(|error| {
            EvaError::invalid_argument("task idempotency key is invalid")
                .with_context("idempotency_key", value)
                .with_context("cause", error.message())
        })?;
        Ok(Self(value.to_owned()))
    }

    /// 返回持久化使用的稳定键。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TaskAttemptPolicy {
    /// 创建严格策略；与兼容 `RetryPolicy::new` 不同，零值直接失败。
    pub fn new(
        max_attempts: u32,
        retry_backoff_ms: u64,
        attempt_timeout_ms: Option<u64>,
    ) -> Result<Self, EvaError> {
        let snapshot =
            TaskAttemptPolicySnapshot::new(max_attempts, retry_backoff_ms, attempt_timeout_ms)?;
        Ok(Self::from(snapshot))
    }

    fn to_snapshot(&self) -> TaskAttemptPolicySnapshot {
        TaskAttemptPolicySnapshot {
            max_attempts: self.max_attempts,
            retry_backoff_ms: self.retry_backoff_ms,
            attempt_timeout_ms: self.attempt_timeout_ms,
        }
    }
}

impl From<TaskAttemptPolicySnapshot> for TaskAttemptPolicy {
    fn from(value: TaskAttemptPolicySnapshot) -> Self {
        Self {
            max_attempts: value.max_attempts,
            retry_backoff_ms: value.retry_backoff_ms,
            attempt_timeout_ms: value.attempt_timeout_ms,
        }
    }
}

impl TaskArtifactRef {
    /// 创建语法安全的相对 artifact key 与 canonical SHA-256 声明。
    pub fn new(key: impl Into<String>, digest: impl Into<String>) -> Result<Self, EvaError> {
        let snapshot = TaskInputSnapshot::artifact(key, digest)?;
        match snapshot {
            TaskInputSnapshot::Artifact {
                artifact_ref,
                digest,
            } => Ok(Self {
                key: artifact_ref,
                digest,
            }),
            TaskInputSnapshot::Inline { .. } => unreachable!("artifact constructor is typed"),
        }
    }

    /// 返回 artifact store 的稳定相对 key。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 返回执行前必须重验的 canonical SHA-256。
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl TaskInput {
    /// 创建 inline payload 并立即计算摘要；超出 durable 上限时失败。
    pub fn inline(bytes: impl Into<Vec<u8>>) -> Result<Self, EvaError> {
        match TaskInputSnapshot::inline(bytes)? {
            TaskInputSnapshot::Inline { bytes, digest } => Ok(Self::Inline { bytes, digest }),
            TaskInputSnapshot::Artifact { .. } => unreachable!("inline constructor is typed"),
        }
    }

    /// 包装已经过校验的 artifact 引用。
    pub fn artifact(reference: TaskArtifactRef) -> Self {
        Self::Artifact(reference)
    }

    /// 返回 inline/artifact payload 绑定的摘要。
    pub fn digest(&self) -> &str {
        match self {
            Self::Inline { digest, .. } => digest,
            Self::Artifact(reference) => reference.digest(),
        }
    }

    fn to_snapshot(&self) -> TaskInputSnapshot {
        match self {
            Self::Inline { bytes, digest } => TaskInputSnapshot::Inline {
                bytes: bytes.clone(),
                digest: digest.clone(),
            },
            Self::Artifact(reference) => TaskInputSnapshot::Artifact {
                artifact_ref: reference.key.clone(),
                digest: reference.digest.clone(),
            },
        }
    }
}

impl TaskEnvelope {
    /// 从全部强类型字段创建不可变信封，并在进入 runtime 前复验 storage 契约。
    pub fn new(
        kind: TaskKind,
        agent_id: AgentId,
        input: TaskInput,
        idempotency_key: IdempotencyKey,
        attempt_policy: TaskAttemptPolicy,
    ) -> Result<Self, EvaError> {
        let envelope = Self {
            kind,
            agent_id,
            input,
            idempotency_key,
            attempt_policy,
        };
        envelope.to_snapshot().validate()?;
        Ok(envelope)
    }

    /// 返回 handler registry key。
    pub fn kind(&self) -> &TaskKind {
        &self.kind
    }

    /// 返回提交时绑定的 Agent。
    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    /// 返回不透明 inline bytes 或 artifact ref；调用方不应写入日志。
    pub fn input(&self) -> &TaskInput {
        &self.input
    }

    /// 返回副作用 ledger 后续使用的幂等键。
    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    /// 返回不可变 attempt policy。
    pub fn attempt_policy(&self) -> &TaskAttemptPolicy {
        &self.attempt_policy
    }

    /// 映射为 storage DTO；所有字段在构造时已校验且保持私有不可变。
    pub fn to_snapshot(&self) -> TaskEnvelopeSnapshot {
        TaskEnvelopeSnapshot {
            kind: self.kind.0.clone(),
            agent_id: self.agent_id.as_str().to_owned(),
            input: self.input.to_snapshot(),
            idempotency_key: self.idempotency_key.0.clone(),
            attempt_policy: self.attempt_policy.to_snapshot(),
        }
    }
}

impl TryFrom<TaskEnvelopeSnapshot> for TaskEnvelope {
    type Error = EvaError;

    fn try_from(value: TaskEnvelopeSnapshot) -> Result<Self, Self::Error> {
        value.validate()?;
        let input = match value.input {
            TaskInputSnapshot::Inline { bytes, digest } => TaskInput::Inline { bytes, digest },
            TaskInputSnapshot::Artifact {
                artifact_ref,
                digest,
            } => TaskInput::artifact(TaskArtifactRef {
                key: artifact_ref,
                digest,
            }),
        };
        Self::new(
            TaskKind::parse(&value.kind)?,
            AgentId::parse(&value.agent_id)?,
            input,
            IdempotencyKey::parse(&value.idempotency_key)?,
            TaskAttemptPolicy::from(value.attempt_policy),
        )
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
            envelope: None,
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

    /// 绑定完整任务信封；其 attempt policy 必须与兼容 retry policy 一致。
    pub fn with_envelope(mut self, envelope: TaskEnvelope) -> Result<Self, EvaError> {
        if self.retry_policy.max_attempts != envelope.attempt_policy.max_attempts as usize {
            return Err(EvaError::invalid_argument(
                "task report retry policy does not match task envelope",
            ));
        }
        self.envelope = Some(envelope);
        Ok(self)
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
            envelope: report.envelope.as_ref().map(TaskEnvelope::to_snapshot),
            replay_delivery: None,
            status: report.status.as_str().to_owned(),
            attempts: report.attempts,
            execution_owner: None,
            retry_max_attempts: report.retry_policy.max_attempts,
            cancel_requested: report.cancellation.requested,
            cancel_accepted: report.cancellation.accepted,
            cancel_reason: report.cancellation.reason.clone(),
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            result_digest: None,
            result_size_bytes: None,
            interrupted_reason: None,
            error_kind: report
                .error
                .as_ref()
                .map(|error| error.kind().as_str().to_owned()),
            error_message: report
                .error
                .as_ref()
                .map(|error| error.message().to_owned()),
            error_retryable: report.error.as_ref().map(EvaError::is_retryable),
            retry_ready_at_ms: None,
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
    /// 强类型 runtime 信封与 storage DTO 往返时不丢 kind、Agent、bytes、幂等键和策略。
    fn task_envelope_round_trips_storage_snapshot() {
        let input = b"runtime-debug-secret".to_vec();
        let leaked_bytes = format!("{input:?}");
        let envelope = TaskEnvelope::new(
            TaskKind::parse("runtime.echo").unwrap(),
            eva_core::AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(input).unwrap(),
            IdempotencyKey::parse("idem-runtime-envelope").unwrap(),
            TaskAttemptPolicy::new(3, 250, Some(5_000)).unwrap(),
        )
        .unwrap();
        let input_debug = format!("{:?}", envelope.input());
        let envelope_debug = format!("{envelope:?}");

        let stored = envelope.to_snapshot();
        let reopened = TaskEnvelope::try_from(stored).unwrap();

        assert!(!input_debug.contains(&leaked_bytes));
        assert!(!envelope_debug.contains(&leaked_bytes));
        assert!(input_debug.contains("bytes: \"<redacted>\""));
        assert!(input_debug.contains("size_bytes: 20"));
        assert_eq!(reopened, envelope);
        assert_eq!(reopened.kind().as_str(), "runtime.echo");
        assert_eq!(reopened.agent_id().as_str(), "root-agent");
        assert_eq!(reopened.attempt_policy().max_attempts, 3);
    }

    #[test]
    /// 外部 attempt 零值与非 canonical artifact digest 必须失败，不能被静默修正。
    fn task_envelope_rejects_invalid_attempt_policy_and_artifact_digest() {
        let zero = TaskAttemptPolicy::new(0, 0, None).unwrap_err();
        assert_eq!(zero.kind(), eva_core::ErrorKind::InvalidArgument);

        let digest = TaskArtifactRef::new("tasks/input-1", "sha256:BAD").unwrap_err();
        assert_eq!(digest.kind(), eva_core::ErrorKind::InvalidArgument);
    }

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
