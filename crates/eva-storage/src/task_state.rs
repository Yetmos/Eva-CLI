//! 跨进程任务快照、生命周期状态、日志/dead-letter/replay 与文件系统存储实现。
//! Durable task state contracts and filesystem implementation.

use crate::artifact_store::{sha256_digest, validate_filesystem_artifact_key};
use crate::durable_backend::{
    acquire_record_write_lock, atomic_write, DurableWriterGuard, FileSystemDurableBackend,
    WriterGeneration,
};
use crate::state_store::StateVersion;
use crate::DurableBackendLayout;
use eva_core::{AgentId, EvaError, EventId, RequestId};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：让 runtime 与 CLI 通过稳定快照共享任务状态，而不共享进程内对象。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable task state interfaces and process-boundary snapshots";

const TASK_STATE_FORMAT_V2: &str = "eva.task-state.v2";
const TASK_STATE_FORMAT_V3: &str = "eva.task-state.v3";
const TASK_STATE_FORMAT_V4: &str = "eva.task-state.v4";
const TASK_STATE_FORMAT_V5: &str = "eva.task-state.v5";
const REPLAY_DELIVERY_TASK_PREFIX: &str = "replay-delivery-";
const MAX_TASK_KIND_BYTES: usize = 128;
const MAX_INLINE_TASK_INPUT_BYTES: usize = 1024 * 1024;
const MAX_TASK_EXECUTION_OWNER_BYTES: usize = 512;
const MAX_TASK_CANCEL_TOKEN_BYTES: usize = 256;
const TASK_STATE_CAS_RETRY_LIMIT: usize = 32;
const COMMITTED_EFFECT_RECOVERY_LOG_PREFIX: &str =
    "task recovered from committed effect: operation_digest=";
const UNKNOWN_EFFECT_RECOVERY_REASON_PREFIX: &str =
    "non-idempotent effect outcome is unknown; operator reconciliation required; operation_digest=";

/// Default age at which a running task is reported as degraded when no newer
/// fenced heartbeat has been persisted.
pub const DEFAULT_TASK_HEARTBEAT_DEGRADED_AFTER_MS: u128 = 5_000;
/// Default age at which a running task is reported as stale and becomes
/// eligible for a later recovery decision.
pub const DEFAULT_TASK_HEARTBEAT_STALE_AFTER_MS: u128 = 15_000;

/// Derived liveness classification for a task attempt. This is observational
/// metadata; it never changes the durable lifecycle status by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskFreshness {
    /// A running/cancelling attempt has a recent heartbeat.
    Live,
    /// The heartbeat is late but still inside the reclaim grace window.
    Degraded,
    /// The heartbeat is absent or past the stale threshold.
    Stale,
    /// The task is queued, recovering, or terminal and has no active lease.
    NotApplicable,
}

impl TaskFreshness {
    /// Stable wire/text spelling used by CLI and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Degraded => "degraded",
            Self::Stale => "stale",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// 一次任务允许执行的固定宽度、可持久化策略。
/// Durable per-task attempt policy with platform-independent integer widths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAttemptPolicySnapshot {
    /// 包括首次执行在内的最大尝试次数，必须至少为一。
    pub max_attempts: u32,
    /// 可重试失败后再次领取前的固定退避毫秒数。
    pub retry_backoff_ms: u64,
    /// 单次 attempt 的可选超时；显式值必须大于零。
    pub attempt_timeout_ms: Option<u64>,
}

/// 任务输入的互斥持久表示；inline 保存原始字节，artifact 保存稳定引用。
/// Mutually exclusive durable task input representation.
#[derive(Clone, PartialEq, Eq)]
pub enum TaskInputSnapshot {
    /// 状态记录内直接保存的小型、不透明输入。
    Inline {
        /// 原始输入字节；磁盘格式使用十六进制，避免文本转码破坏 payload。
        bytes: Vec<u8>,
        /// 原始字节的 canonical SHA-256。
        digest: String,
    },
    /// 由执行边界读取并重新校验的 artifact 引用。
    Artifact {
        /// 与 `FileSystemArtifactStore` 相同语法的稳定相对 key。
        artifact_ref: String,
        /// 执行前必须与 artifact bytes 匹配的 canonical SHA-256。
        digest: String,
    },
}

impl fmt::Debug for TaskInputSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline { bytes, digest } => formatter
                .debug_struct("TaskInputSnapshot")
                .field("kind", &"inline")
                .field("bytes", &"<redacted>")
                .field("size_bytes", &bytes.len())
                .field("digest", digest)
                .finish(),
            Self::Artifact {
                artifact_ref,
                digest,
            } => formatter
                .debug_struct("TaskInputSnapshot")
                .field("kind", &"artifact")
                .field("artifact_ref", artifact_ref)
                .field("digest", digest)
                .finish(),
        }
    }
}

/// 与生命周期状态一同提交、之后不可变的任务业务信封。
/// Immutable durable task business envelope stored beside lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEnvelopeSnapshot {
    /// 供后续 handler registry 精确匹配的点分 task kind。
    pub kind: String,
    /// 提交时选定且通过核心 ID 语法校验的 Agent。
    pub agent_id: String,
    /// inline bytes 或带摘要的 artifact 引用，恰好一种。
    pub input: TaskInputSnapshot,
    /// 副作用 ledger 后续使用的稳定幂等键；本项不执行去重。
    pub idempotency_key: String,
    /// 从提交到恢复均保持不变的 attempt policy。
    pub attempt_policy: TaskAttemptPolicySnapshot,
}

/// Durable identity proving that a task is one scheduler replay delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReplayDeliverySnapshot {
    /// Persisted replay event whose payload is bound into the task envelope.
    pub replay_event_id: String,
    /// Stable position in the ordered handler-owner delivery plan.
    pub delivery_index: usize,
}

/// CLI task 命令与 runtime 跨进程使用的完整任务状态快照。
/// Stored task summary used by CLI task commands across process boundaries.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskStateSnapshot {
    /// 权威 task ID 文件的持久 CAS 版本；零表示尚未创建/legacy 无版本记录。
    pub record_version: StateVersion,
    /// 提交该版本的 runtime writer generation；传统 `.eva/tasks` 路径使用零。
    pub owner_generation: WriterGeneration,
    /// 同时作为 RequestId 和文件名主键的任务 ID。
    pub task_id: String,
    /// 新提交任务的完整不可变信封；None 仅表示 legacy/v2 状态没有可恢复 payload。
    pub envelope: Option<TaskEnvelopeSnapshot>,
    /// Internal replay-delivery identity; absent for operator-submitted tasks.
    pub replay_delivery: Option<TaskReplayDeliverySnapshot>,
    /// queued/running/cancelling/终态等稳定状态文本。
    pub status: String,
    /// 已执行尝试次数。
    pub attempts: usize,
    /// 当前/最后一次 attempt 的 daemon worker owner；与 writer generation 含义独立。
    pub execution_owner: Option<String>,
    /// 兼容 v2/现有 CLI 的重试上限镜像；v3+ 必须等于 envelope attempt policy。
    pub retry_max_attempts: usize,
    /// 是否收到取消请求。
    pub cancel_requested: bool,
    /// 取消请求是否在非终态被接受。
    pub cancel_accepted: bool,
    /// 可选取消原因。
    pub cancel_reason: Option<String>,
    /// 最近心跳 epoch 毫秒。
    pub heartbeat_at_ms: Option<u128>,
    /// 可选任务 deadline epoch 毫秒。
    pub deadline_at_ms: Option<u128>,
    /// Runtime 取消传播使用的可选 token。
    pub cancel_token: Option<String>,
    /// 成功结果 bytes 的 canonical SHA-256；不在任务快照中内联结果 bytes。
    pub result_digest: Option<String>,
    /// 成功结果 bytes 的长度；必须与 result_digest 成对出现。
    pub result_size_bytes: Option<usize>,
    /// 中断或恢复原因。
    pub interrupted_reason: Option<String>,
    /// 失败/超时的稳定错误分类文本。
    pub error_kind: Option<String>,
    /// 失败/超时的人类可读消息。
    pub error_message: Option<String>,
    /// Handler-provided retry classification; None preserves legacy kind defaults.
    pub error_retryable: Option<bool>,
    /// Earliest epoch millisecond at which a failed attempt may be requeued.
    pub retry_ready_at_ms: Option<u128>,
    /// 有序生命周期与执行日志。
    pub logs: Vec<TaskStateLogSnapshot>,
    /// 未能处理的事件摘要。
    pub dead_letters: Vec<TaskStateDeadLetterSnapshot>,
    /// 已重放事件摘要。
    pub replayed_events: Vec<TaskStateReplaySnapshot>,
}

impl fmt::Debug for TaskStateSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskStateSnapshot")
            .field("record_version", &self.record_version)
            .field("owner_generation", &self.owner_generation)
            .field("task_id", &self.task_id)
            .field("envelope", &self.envelope)
            .field("replay_delivery", &self.replay_delivery)
            .field("status", &self.status)
            .field("attempts", &self.attempts)
            .field(
                "execution_owner",
                &self.execution_owner.as_ref().map(|_| "<redacted>"),
            )
            .field("retry_max_attempts", &self.retry_max_attempts)
            .field("cancel_requested", &self.cancel_requested)
            .field("cancel_accepted", &self.cancel_accepted)
            .field("cancel_reason", &self.cancel_reason)
            .field("heartbeat_at_ms", &self.heartbeat_at_ms)
            .field("deadline_at_ms", &self.deadline_at_ms)
            .field(
                "cancel_token",
                &self.cancel_token.as_ref().map(|_| "<redacted>"),
            )
            .field("result_digest", &self.result_digest)
            .field("result_size_bytes", &self.result_size_bytes)
            .field("interrupted_reason", &self.interrupted_reason)
            .field("error_kind", &self.error_kind)
            .field("error_message", &self.error_message)
            .field("error_retryable", &self.error_retryable)
            .field("retry_ready_at_ms", &self.retry_ready_at_ms)
            .field("logs", &self.logs)
            .field("dead_letters", &self.dead_letters)
            .field("replayed_events", &self.replayed_events)
            .finish()
    }
}

/// 一个已提交 attempt 的完整 fencing identity；cancel token 在 Debug 中始终脱敏。
#[derive(Clone, PartialEq, Eq)]
pub struct TaskAttemptFence {
    task_id: String,
    owner_generation: WriterGeneration,
    execution_owner: String,
    attempt: usize,
    cancel_token: String,
}

impl TaskAttemptFence {
    /// 返回 attempt 所属任务。
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// 返回提交 claim 的 durable writer generation。
    pub const fn owner_generation(&self) -> WriterGeneration {
        self.owner_generation
    }

    /// 返回不可授权的 worker owner 摘要身份。
    pub fn execution_owner(&self) -> &str {
        &self.execution_owner
    }

    /// 返回从一开始的 attempt 序号。
    pub const fn attempt(&self) -> usize {
        self.attempt
    }

    /// 返回当前 attempt 的取消 fencing token。
    pub fn cancel_token(&self) -> &str {
        &self.cancel_token
    }
}

impl fmt::Debug for TaskAttemptFence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskAttemptFence")
            .field("task_id", &self.task_id)
            .field("owner_generation", &self.owner_generation)
            .field("execution_owner", &"<redacted>")
            .field("attempt", &self.attempt)
            .field("cancel_token", &"<redacted>")
            .finish()
    }
}

/// 已持久 claim 的快照及其 finish fence。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskExecutionClaim {
    snapshot: TaskStateSnapshot,
    fence: TaskAttemptFence,
}

impl TaskExecutionClaim {
    /// 返回 store 已 stamp 的 running 快照。
    pub fn snapshot(&self) -> &TaskStateSnapshot {
        &self.snapshot
    }

    /// 返回 finish/cancel 使用的完整 fencing identity。
    pub fn fence(&self) -> &TaskAttemptFence {
        &self.fence
    }
}

/// handler 完成后由 storage 在最新 record version 上提交的终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAttemptOutcome {
    /// handler 成功；仅持久化结果 bytes 的摘要和长度。
    Completed {
        /// canonical SHA-256。
        result_digest: String,
        /// 原始结果长度。
        result_size_bytes: usize,
    },
    /// handler 返回稳定结构化错误或 panic 被隔离。
    Failed {
        /// `ErrorKind::as_str()` 风格分类。
        error_kind: String,
        /// 不含 payload/result/secret 的稳定消息。
        error_message: String,
        /// Exact retry classification returned by the handler boundary.
        retryable: bool,
    },
    /// handler 返回 timeout 或完成时 deadline 已经过期。
    TimedOut {
        /// 判定 timeout 的 epoch 毫秒。
        observed_at_ms: u128,
        /// Exact retry classification for the timeout outcome.
        retryable: bool,
    },
}

/// 一条持久化任务日志。
/// Stored task log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateLogSnapshot {
    /// 从 1 开始、按当前日志长度分配的序号。
    pub sequence: u64,
    /// info/warning/error 等级文本。
    pub level: String,
    /// 百分号编码后写入磁盘的日志消息。
    pub message: String,
}

/// 一条持久化 dead-letter 摘要。
/// Stored dead-letter summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateDeadLetterSnapshot {
    /// 未处理事件 ID。
    pub event_id: String,
    /// 原事件 topic。
    pub topic: String,
    /// 失败原因分类。
    pub reason_kind: String,
    /// 失败原因文本。
    pub reason: String,
    /// 已尝试重放次数。
    pub replay_count: usize,
}

/// 一条成功/已记录 replay 摘要。
/// Stored replay summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateReplaySnapshot {
    /// 被重放事件 ID。
    pub event_id: String,
    /// 重放来源日志序号。
    pub sequence: u64,
    /// 被重放事件 topic。
    pub topic: String,
}

/// CLI/runtime 边界所需的 durable task state 行为。
/// Durable task state behavior required by CLI/runtime boundaries.
pub trait TaskStateStore {
    /// 创建指定任务快照并更新 latest 别名；既有 ID 必须走显式 CAS。
    fn write(&mut self, snapshot: &TaskStateSnapshot) -> Result<(), EvaError>;
    /// 按 task ID 读取，None 表示读取 latest 别名。
    fn read(&self, task_id: Option<&str>) -> Result<TaskStateSnapshot, EvaError>;
}

/// 保留既有 `.eva/tasks` 兼容布局的文件系统任务状态存储。
/// Filesystem-backed task state store that preserves the existing `.eva/tasks` layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemTaskStateStore {
    /// 项目或 durable backend 根，用于报告所属边界。
    project_root: PathBuf,
    /// 实际 `.task` 文件目录。
    task_dir: PathBuf,
    /// Durable backend 路径是否要求显式 writer ownership 才能 mutation。
    durable_writer_required: bool,
    /// 可写 durable store 持有的长期 ownership；clone 共享同一进程 mutex 和 OS lock。
    writer: Option<DurableWriterGuard>,
}

#[derive(Clone, Copy)]
enum TaskStateCommitMode {
    Create,
    CompareAndSet,
    AttemptOutcome,
    Heartbeat,
    RetryRequeue,
    RestartRecovery,
}

impl TaskStateCommitMode {
    const fn create_only(self) -> bool {
        matches!(self, Self::Create)
    }

    const fn allow_attempt_outcome(self) -> bool {
        matches!(self, Self::AttemptOutcome)
    }

    const fn allow_heartbeat(self) -> bool {
        matches!(self, Self::AttemptOutcome | Self::Heartbeat)
    }

    const fn allow_retry_requeue(self) -> bool {
        matches!(self, Self::RetryRequeue)
    }

    const fn allow_restart_recovery(self) -> bool {
        matches!(self, Self::RestartRecovery)
    }
}

impl TaskAttemptPolicySnapshot {
    /// 创建经校验的 attempt policy；外部零值不会被静默提升。
    pub fn new(
        max_attempts: u32,
        retry_backoff_ms: u64,
        attempt_timeout_ms: Option<u64>,
    ) -> Result<Self, EvaError> {
        let policy = Self {
            max_attempts,
            retry_backoff_ms,
            attempt_timeout_ms,
        };
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), EvaError> {
        if self.max_attempts == 0 {
            return Err(EvaError::invalid_argument(
                "task attempt policy max_attempts must be at least one",
            ));
        }
        if self.attempt_timeout_ms == Some(0) {
            return Err(EvaError::invalid_argument(
                "task attempt timeout must be greater than zero when present",
            ));
        }
        Ok(())
    }
}

impl TaskInputSnapshot {
    /// 构造 inline 输入并绑定原始字节摘要。
    pub fn inline(bytes: impl Into<Vec<u8>>) -> Result<Self, EvaError> {
        let bytes = bytes.into();
        if bytes.len() > MAX_INLINE_TASK_INPUT_BYTES {
            return Err(EvaError::invalid_argument(
                "inline task input exceeds the durable size limit",
            )
            .with_context("size_bytes", bytes.len().to_string())
            .with_context("max_size_bytes", MAX_INLINE_TASK_INPUT_BYTES.to_string()));
        }
        let digest = sha256_digest(&bytes);
        Ok(Self::Inline { bytes, digest })
    }

    /// 构造 artifact 引用；这里只校验 key/digest 语法，真实 bytes 在执行边界重验。
    pub fn artifact(
        artifact_ref: impl Into<String>,
        digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let input = Self::Artifact {
            artifact_ref: artifact_ref.into(),
            digest: digest.into(),
        };
        input.validate()?;
        Ok(input)
    }

    /// 返回 inline bytes 或 artifact bytes 声明绑定的 canonical digest。
    pub fn digest(&self) -> &str {
        match self {
            Self::Inline { digest, .. } | Self::Artifact { digest, .. } => digest,
        }
    }

    fn validate(&self) -> Result<(), EvaError> {
        match self {
            Self::Inline { bytes, digest } => {
                if bytes.len() > MAX_INLINE_TASK_INPUT_BYTES {
                    return Err(EvaError::invalid_argument(
                        "inline task input exceeds the durable size limit",
                    )
                    .with_context("size_bytes", bytes.len().to_string())
                    .with_context("max_size_bytes", MAX_INLINE_TASK_INPUT_BYTES.to_string()));
                }
                validate_canonical_sha256(digest, "task input digest")?;
                let actual = sha256_digest(bytes);
                if *digest != actual {
                    return Err(EvaError::invalid_argument(
                        "inline task input digest does not match payload bytes",
                    )
                    .with_context("expected_digest", digest)
                    .with_context("actual_digest", actual));
                }
                Ok(())
            }
            Self::Artifact {
                artifact_ref,
                digest,
            } => {
                validate_filesystem_artifact_key(artifact_ref.clone()).map(|_| ())?;
                validate_canonical_sha256(digest, "task artifact digest")
            }
        }
    }
}

impl TaskEnvelopeSnapshot {
    /// 对外复用持久格式的 task-kind 语法，而不判断 handler 是否已注册。
    pub fn validate_kind(value: &str) -> Result<(), EvaError> {
        validate_task_kind(value)
    }

    /// 创建带 inline bytes 的完整任务信封。
    pub fn inline(
        kind: impl Into<String>,
        agent_id: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        idempotency_key: impl Into<String>,
        attempt_policy: TaskAttemptPolicySnapshot,
    ) -> Result<Self, EvaError> {
        Self::new(
            kind,
            agent_id,
            TaskInputSnapshot::inline(bytes)?,
            idempotency_key,
            attempt_policy,
        )
    }

    /// 创建带 artifact ref/digest 的完整任务信封。
    pub fn artifact(
        kind: impl Into<String>,
        agent_id: impl Into<String>,
        artifact_ref: impl Into<String>,
        digest: impl Into<String>,
        idempotency_key: impl Into<String>,
        attempt_policy: TaskAttemptPolicySnapshot,
    ) -> Result<Self, EvaError> {
        Self::new(
            kind,
            agent_id,
            TaskInputSnapshot::artifact(artifact_ref, digest)?,
            idempotency_key,
            attempt_policy,
        )
    }

    /// 从已区分的 input 创建信封，并统一执行所有持久契约校验。
    pub fn new(
        kind: impl Into<String>,
        agent_id: impl Into<String>,
        input: TaskInputSnapshot,
        idempotency_key: impl Into<String>,
        attempt_policy: TaskAttemptPolicySnapshot,
    ) -> Result<Self, EvaError> {
        let envelope = Self {
            kind: kind.into(),
            agent_id: agent_id.into(),
            input,
            idempotency_key: idempotency_key.into(),
            attempt_policy,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// 校验公开字段构造的信封，供 store 在落盘前再次 fail closed。
    pub fn validate(&self) -> Result<(), EvaError> {
        validate_task_kind(&self.kind)?;
        AgentId::parse(&self.agent_id)?;
        RequestId::parse(&self.idempotency_key).map_err(|error| {
            EvaError::invalid_argument("task idempotency key is invalid")
                .with_context("idempotency_key", &self.idempotency_key)
                .with_context("cause", error.message())
        })?;
        self.input.validate()?;
        self.attempt_policy.validate()
    }
}

impl TaskReplayDeliverySnapshot {
    /// Creates a validated replay-delivery marker.
    pub fn new(
        replay_event_id: impl Into<String>,
        delivery_index: usize,
    ) -> Result<Self, EvaError> {
        let marker = Self {
            replay_event_id: replay_event_id.into(),
            delivery_index,
        };
        marker.validate()?;
        Ok(marker)
    }

    fn validate(&self) -> Result<(), EvaError> {
        EventId::parse(&self.replay_event_id)?;
        Ok(())
    }
}

impl TaskStateSnapshot {
    /// 创建已校验 task ID 的 queued 初始快照，重试上限默认为一次。
    pub fn queued(task_id: impl Into<String>) -> Result<Self, EvaError> {
        let task_id = task_id.into();
        RequestId::parse(&task_id)?;
        Ok(Self {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id,
            envelope: None,
            replay_delivery: None,
            status: "queued".to_owned(),
            attempts: 0,
            execution_owner: None,
            retry_max_attempts: 1,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            result_digest: None,
            result_size_bytes: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            error_retryable: None,
            retry_ready_at_ms: None,
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        })
    }

    /// 创建携带完整任务信封的 queued 快照，并同步兼容重试上限镜像。
    pub fn queued_with_envelope(
        task_id: impl Into<String>,
        envelope: TaskEnvelopeSnapshot,
    ) -> Result<Self, EvaError> {
        envelope.validate()?;
        let mut snapshot = Self::queued(task_id)?;
        snapshot.retry_max_attempts = envelope.attempt_policy.max_attempts as usize;
        snapshot.envelope = Some(envelope);
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Creates a queued task with an immutable scheduler replay-delivery identity.
    pub fn queued_with_replay_delivery(
        task_id: impl Into<String>,
        envelope: TaskEnvelopeSnapshot,
        replay_event_id: impl Into<String>,
        delivery_index: usize,
    ) -> Result<Self, EvaError> {
        envelope.validate()?;
        let mut snapshot = Self::queued(task_id)?;
        snapshot.retry_max_attempts = envelope.attempt_policy.max_attempts as usize;
        snapshot.envelope = Some(envelope);
        snapshot.replay_delivery = Some(TaskReplayDeliverySnapshot::new(
            replay_event_id,
            delivery_index,
        )?);
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// 校验公共字段构造的快照；读盘与每次 commit 都调用，禁止绕过构造器。
    pub fn validate(&self) -> Result<(), EvaError> {
        RequestId::parse(&self.task_id)?;
        if self.status.is_empty() {
            return Err(EvaError::invalid_argument(
                "task state status cannot be empty",
            ));
        }
        if self.retry_max_attempts == 0 {
            return Err(EvaError::invalid_argument(
                "task retry_max_attempts must be at least one",
            ));
        }
        if self.attempts > self.retry_max_attempts {
            return Err(EvaError::invalid_argument(
                "task attempts cannot exceed retry_max_attempts",
            )
            .with_context("attempts", self.attempts.to_string())
            .with_context("retry_max_attempts", self.retry_max_attempts.to_string()));
        }
        if let Some(execution_owner) = self.execution_owner.as_deref() {
            validate_execution_owner(execution_owner)?;
            if self.envelope.is_none() || self.attempts == 0 || self.cancel_token.is_none() {
                return Err(EvaError::invalid_argument(
                    "task execution owner requires an envelope, active attempt, and cancel token",
                ));
            }
        }
        if let Some(cancel_token) = self.cancel_token.as_deref() {
            validate_cancel_token(cancel_token)?;
        }
        if let Some(replay_delivery) = &self.replay_delivery {
            replay_delivery.validate()?;
            if !is_replay_delivery_task_id(&self.task_id) {
                return Err(EvaError::invalid_argument(
                    "replay delivery marker requires a reserved deterministic task id",
                )
                .with_context("task_id", &self.task_id));
            }
            let envelope = self.envelope.as_ref().ok_or_else(|| {
                EvaError::invalid_argument("replay delivery marker requires a task envelope")
            })?;
            if envelope.attempt_policy.max_attempts != u32::MAX
                || envelope.attempt_policy.attempt_timeout_ms.is_some()
            {
                return Err(EvaError::invalid_argument(
                    "replay delivery envelope does not match its internal retry contract",
                )
                .with_context("task_id", &self.task_id));
            }
        }
        if self.error_retryable.is_some()
            && (!matches!(self.status.as_str(), "failed" | "timed_out")
                || self.error_kind.is_none()
                || self.error_message.is_none())
        {
            return Err(EvaError::invalid_argument(
                "task retry classification requires a failed or timed-out outcome",
            ));
        }
        if self.retry_ready_at_ms.is_some()
            && (!matches!(self.status.as_str(), "failed" | "timed_out")
                || self.attempts >= self.retry_max_attempts
                || self.error_retryable == Some(false))
        {
            return Err(EvaError::invalid_argument(
                "task retry readiness requires a retryable outcome with remaining attempts",
            ));
        }
        match (&self.result_digest, self.result_size_bytes) {
            (None, None) => {}
            (Some(digest), Some(_)) if self.status == "completed" && self.envelope.is_some() => {
                validate_canonical_sha256(digest, "task result digest")?;
            }
            (Some(_), Some(_)) => {
                return Err(EvaError::invalid_argument(
                    "task result metadata requires completed status",
                ))
            }
            _ => {
                return Err(EvaError::invalid_argument(
                    "task result digest and size must appear together",
                ))
            }
        }
        if self.execution_owner.is_some() {
            let execution_metadata_valid = match self.status.as_str() {
                "running" | "cancelling" => {
                    self.result_digest.is_none()
                        && self.error_kind.is_none()
                        && self.error_message.is_none()
                        && self.error_retryable.is_none()
                        && self.retry_ready_at_ms.is_none()
                        && self.interrupted_reason.is_none()
                }
                "completed" => {
                    self.result_digest.is_some()
                        && self.error_kind.is_none()
                        && self.error_message.is_none()
                        && self.error_retryable.is_none()
                        && self.retry_ready_at_ms.is_none()
                        && self.interrupted_reason.is_none()
                }
                "failed" => {
                    self.result_digest.is_none()
                        && self.error_kind.is_some()
                        && self.error_message.is_some()
                        && self.interrupted_reason.is_none()
                }
                "timed_out" => {
                    self.result_digest.is_none()
                        && self.error_kind.as_deref() == Some("timeout")
                        && self.error_message.is_some()
                        && self.heartbeat_at_ms.is_some()
                        && self.interrupted_reason.is_none()
                }
                "cancelled" => {
                    self.result_digest.is_none()
                        && self.error_kind.is_none()
                        && self.error_message.is_none()
                        && self.error_retryable.is_none()
                        && self.retry_ready_at_ms.is_none()
                        && self.interrupted_reason.is_none()
                }
                "interrupted" | "recovering" => self.result_digest.is_none(),
                _ => true,
            };
            if !execution_metadata_valid {
                return Err(EvaError::invalid_argument(
                    "task execution metadata does not match lifecycle status",
                )
                .with_context("status", &self.status));
            }
        }
        if let Some(envelope) = &self.envelope {
            envelope.validate()?;
            if self.retry_max_attempts != envelope.attempt_policy.max_attempts as usize {
                return Err(EvaError::invalid_argument(
                    "task retry policy mirror does not match immutable envelope",
                )
                .with_context("retry_max_attempts", self.retry_max_attempts.to_string())
                .with_context(
                    "envelope_max_attempts",
                    envelope.attempt_policy.max_attempts.to_string(),
                ));
            }
        }
        Ok(())
    }

    /// 序列化为逐行任务格式。
    /// 标量字段唯一；log/dead_letter/replay 以重复复合行保存顺序。特殊字符做百分号编码，
    /// 可选值用空串表示 None。新记录带 format、record version 与 owner generation；旧记录
    /// 缺少这些字段时按 version/generation 零读取，并只能通过一次成功 CAS 升级。
    pub fn to_storage(&self) -> String {
        let format = if self.envelope.is_some() {
            TASK_STATE_FORMAT_V5
        } else {
            TASK_STATE_FORMAT_V2
        };
        let mut lines = vec![
            format!("format={format}"),
            format!("record_version={}", self.record_version.0),
            format!("owner_generation={}", self.owner_generation.0),
            format!("task_id={}", encode_field(&self.task_id)),
            format!("status={}", encode_field(&self.status)),
            format!("attempts={}", self.attempts),
            format!("retry_max_attempts={}", self.retry_max_attempts),
        ];
        if let Some(envelope) = &self.envelope {
            let (input_kind, inline_input_hex, artifact_ref, input_digest) = match &envelope.input {
                TaskInputSnapshot::Inline { bytes, digest } => {
                    ("inline", encode_hex(bytes), String::new(), digest.clone())
                }
                TaskInputSnapshot::Artifact {
                    artifact_ref,
                    digest,
                } => (
                    "artifact",
                    String::new(),
                    encode_field(artifact_ref),
                    digest.clone(),
                ),
            };
            lines.extend([
                format!("envelope_kind={}", encode_field(&envelope.kind)),
                format!("envelope_agent_id={}", encode_field(&envelope.agent_id)),
                format!("envelope_input_kind={input_kind}"),
                format!("envelope_inline_input_hex={inline_input_hex}"),
                format!("envelope_artifact_ref={artifact_ref}"),
                format!("envelope_input_digest={input_digest}"),
                format!(
                    "envelope_idempotency_key={}",
                    encode_field(&envelope.idempotency_key)
                ),
                format!(
                    "envelope_max_attempts={}",
                    envelope.attempt_policy.max_attempts
                ),
                format!(
                    "envelope_retry_backoff_ms={}",
                    envelope.attempt_policy.retry_backoff_ms
                ),
                format!(
                    "envelope_attempt_timeout_ms={}",
                    envelope
                        .attempt_policy
                        .attempt_timeout_ms
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                ),
                format!(
                    "execution_owner={}",
                    self.execution_owner
                        .as_ref()
                        .map(|value| encode_field(value))
                        .unwrap_or_default()
                ),
                format!(
                    "result_digest={}",
                    self.result_digest.as_deref().unwrap_or_default()
                ),
                format!(
                    "result_size_bytes={}",
                    self.result_size_bytes
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                ),
                format!(
                    "replay_event_id={}",
                    self.replay_delivery
                        .as_ref()
                        .map(|value| encode_field(&value.replay_event_id))
                        .unwrap_or_default()
                ),
                format!(
                    "replay_delivery_index={}",
                    self.replay_delivery
                        .as_ref()
                        .map(|value| value.delivery_index.to_string())
                        .unwrap_or_default()
                ),
                format!(
                    "error_retryable={}",
                    self.error_retryable
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                ),
                format!(
                    "retry_ready_at_ms={}",
                    self.retry_ready_at_ms
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                ),
            ]);
        }
        lines.extend([
            format!("cancel_requested={}", self.cancel_requested),
            format!("cancel_accepted={}", self.cancel_accepted),
            format!(
                "cancel_reason={}",
                self.cancel_reason
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "heartbeat_at_ms={}",
                self.heartbeat_at_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "deadline_at_ms={}",
                self.deadline_at_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "cancel_token={}",
                self.cancel_token
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "interrupted_reason={}",
                self.interrupted_reason
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "error_kind={}",
                self.error_kind
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "error_message={}",
                self.error_message
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
        ]);
        lines.extend(self.logs.iter().map(|entry| {
            format!(
                "log={}|{}|{}",
                entry.sequence,
                encode_field(&entry.level),
                encode_field(&entry.message)
            )
        }));
        lines.extend(self.dead_letters.iter().map(|entry| {
            format!(
                "dead_letter={}|{}|{}|{}|{}",
                encode_field(&entry.event_id),
                encode_field(&entry.topic),
                encode_field(&entry.reason_kind),
                encode_field(&entry.reason),
                entry.replay_count
            )
        }));
        lines.extend(self.replayed_events.iter().map(|entry| {
            format!(
                "replay={}|{}|{}",
                encode_field(&entry.event_id),
                entry.sequence,
                encode_field(&entry.topic)
            )
        }));
        lines.push(String::new());
        lines.join("\n")
    }

    /// 解析任务快照并验证必填 task_id/status。
    ///
    /// 数值和复合字段 arity 严格校验；布尔值只有字面量 `true` 被视为 true；未知行被忽略以
    /// 支持前向兼容。缺核心字段或损坏已知字段返回 InvalidArgument，不返回部分快照。
    pub fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut snapshot = Self {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id: String::new(),
            envelope: None,
            replay_delivery: None,
            status: String::new(),
            attempts: 0,
            execution_owner: None,
            retry_max_attempts: 1,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            result_digest: None,
            result_size_bytes: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            error_retryable: None,
            retry_ready_at_ms: None,
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        };
        let mut format = None;
        let mut envelope_kind = None;
        let mut envelope_agent_id = None;
        let mut envelope_input_kind = None;
        let mut envelope_inline_input_hex = None;
        let mut envelope_artifact_ref = None;
        let mut envelope_input_digest = None;
        let mut envelope_idempotency_key = None;
        let mut envelope_max_attempts = None;
        let mut envelope_retry_backoff_ms = None;
        let mut envelope_attempt_timeout_ms: Option<Option<u64>> = None;
        let mut replay_event_id: Option<Option<String>> = None;
        let mut replay_delivery_index: Option<Option<usize>> = None;
        let mut seen_scalars = BTreeSet::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            if let Some((key, _)) = line.split_once('=') {
                if is_single_task_field(key) && !seen_scalars.insert(key.to_owned()) {
                    return Err(EvaError::invalid_argument(
                        "task state contains a duplicate scalar field",
                    )
                    .with_context("field", key));
                }
            }
            if let Some(value) = line.strip_prefix("format=") {
                format = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("record_version=") {
                snapshot.record_version = StateVersion(parse_stored_u64("record_version", value)?);
            } else if let Some(value) = line.strip_prefix("owner_generation=") {
                snapshot.owner_generation =
                    WriterGeneration(parse_stored_u64("owner_generation", value)?);
            } else if let Some(value) = line.strip_prefix("task_id=") {
                snapshot.task_id = decode_field(value);
            } else if let Some(value) = line.strip_prefix("status=") {
                snapshot.status = decode_field(value);
            } else if let Some(value) = line.strip_prefix("attempts=") {
                snapshot.attempts = parse_stored_usize("attempts", value)?;
            } else if let Some(value) = line.strip_prefix("execution_owner=") {
                snapshot.execution_owner = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("retry_max_attempts=") {
                snapshot.retry_max_attempts = parse_stored_usize("retry_max_attempts", value)?;
            } else if let Some(value) = line.strip_prefix("envelope_kind=") {
                envelope_kind = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("envelope_agent_id=") {
                envelope_agent_id = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("envelope_input_kind=") {
                envelope_input_kind = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("envelope_inline_input_hex=") {
                envelope_inline_input_hex = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("envelope_artifact_ref=") {
                envelope_artifact_ref = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("envelope_input_digest=") {
                envelope_input_digest = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("envelope_idempotency_key=") {
                envelope_idempotency_key = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("envelope_max_attempts=") {
                envelope_max_attempts = Some(parse_stored_u32("envelope_max_attempts", value)?);
            } else if let Some(value) = line.strip_prefix("envelope_retry_backoff_ms=") {
                envelope_retry_backoff_ms =
                    Some(parse_stored_u64("envelope_retry_backoff_ms", value)?);
            } else if let Some(value) = line.strip_prefix("envelope_attempt_timeout_ms=") {
                envelope_attempt_timeout_ms = Some(parse_optional_stored_u64(
                    "envelope_attempt_timeout_ms",
                    value,
                )?);
            } else if let Some(value) = line.strip_prefix("replay_event_id=") {
                replay_event_id = Some(decode_optional_field(value));
            } else if let Some(value) = line.strip_prefix("replay_delivery_index=") {
                replay_delivery_index =
                    Some(parse_optional_stored_usize("replay_delivery_index", value)?);
            } else if let Some(value) = line.strip_prefix("error_retryable=") {
                snapshot.error_retryable = parse_optional_stored_bool("error_retryable", value)?;
            } else if let Some(value) = line.strip_prefix("retry_ready_at_ms=") {
                snapshot.retry_ready_at_ms =
                    parse_optional_stored_u128("retry_ready_at_ms", value)?;
            } else if let Some(value) = line.strip_prefix("cancel_requested=") {
                snapshot.cancel_requested = value == "true";
            } else if let Some(value) = line.strip_prefix("cancel_accepted=") {
                snapshot.cancel_accepted = value == "true";
            } else if let Some(value) = line.strip_prefix("cancel_reason=") {
                snapshot.cancel_reason = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("heartbeat_at_ms=") {
                snapshot.heartbeat_at_ms = parse_optional_stored_u128("heartbeat_at_ms", value)?;
            } else if let Some(value) = line.strip_prefix("deadline_at_ms=") {
                snapshot.deadline_at_ms = parse_optional_stored_u128("deadline_at_ms", value)?;
            } else if let Some(value) = line.strip_prefix("cancel_token=") {
                snapshot.cancel_token = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("result_digest=") {
                snapshot.result_digest = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("result_size_bytes=") {
                snapshot.result_size_bytes =
                    parse_optional_stored_usize("result_size_bytes", value)?;
            } else if let Some(value) = line.strip_prefix("interrupted_reason=") {
                snapshot.interrupted_reason = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("error_kind=") {
                snapshot.error_kind = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("error_message=") {
                snapshot.error_message = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("log=") {
                let parts = split_stored_fields(value, 3, "log")?;
                snapshot.logs.push(TaskStateLogSnapshot {
                    sequence: parse_stored_u64("log.sequence", &parts[0])?,
                    level: decode_field(&parts[1]),
                    message: decode_field(&parts[2]),
                });
            } else if let Some(value) = line.strip_prefix("dead_letter=") {
                let parts = split_stored_fields(value, 5, "dead_letter")?;
                snapshot.dead_letters.push(TaskStateDeadLetterSnapshot {
                    event_id: decode_field(&parts[0]),
                    topic: decode_field(&parts[1]),
                    reason_kind: decode_field(&parts[2]),
                    reason: decode_field(&parts[3]),
                    replay_count: parse_stored_usize("dead_letter.replay_count", &parts[4])?,
                });
            } else if let Some(value) = line.strip_prefix("replay=") {
                let parts = split_stored_fields(value, 3, "replay")?;
                snapshot.replayed_events.push(TaskStateReplaySnapshot {
                    event_id: decode_field(&parts[0]),
                    sequence: parse_stored_u64("replay.sequence", &parts[1])?,
                    topic: decode_field(&parts[2]),
                });
            }
        }

        if snapshot.task_id.is_empty() || snapshot.status.is_empty() {
            return Err(EvaError::invalid_argument("task state file is incomplete"));
        }
        let has_envelope_fields = envelope_kind.is_some()
            || envelope_agent_id.is_some()
            || envelope_input_kind.is_some()
            || envelope_inline_input_hex.is_some()
            || envelope_artifact_ref.is_some()
            || envelope_input_digest.is_some()
            || envelope_idempotency_key.is_some()
            || envelope_max_attempts.is_some()
            || envelope_retry_backoff_ms.is_some()
            || envelope_attempt_timeout_ms.is_some();
        let execution_fields = ["execution_owner", "result_digest", "result_size_bytes"];
        let has_execution_fields = execution_fields
            .iter()
            .any(|field| seen_scalars.contains(*field));
        let v5_fields = [
            "replay_event_id",
            "replay_delivery_index",
            "error_retryable",
            "retry_ready_at_ms",
        ];
        let has_v5_fields = v5_fields.iter().any(|field| seen_scalars.contains(*field));
        match format.as_deref() {
            None if snapshot.record_version == StateVersion::ZERO
                && snapshot.owner_generation == WriterGeneration::ZERO
                && !has_envelope_fields
                && !has_execution_fields
                && !has_v5_fields => {}
            Some(TASK_STATE_FORMAT_V2)
                if snapshot.record_version != StateVersion::ZERO
                    || snapshot.owner_generation == WriterGeneration::ZERO =>
            {
                if has_envelope_fields || has_execution_fields || has_v5_fields {
                    return Err(EvaError::invalid_argument(
                        "v2 task state cannot contain envelope or execution fields",
                    ));
                }
            }
            Some(task_format)
                if matches!(
                    task_format,
                    TASK_STATE_FORMAT_V3 | TASK_STATE_FORMAT_V4 | TASK_STATE_FORMAT_V5
                ) && (snapshot.record_version != StateVersion::ZERO
                    || snapshot.owner_generation == WriterGeneration::ZERO) =>
            {
                if task_format == TASK_STATE_FORMAT_V3 && (has_execution_fields || has_v5_fields) {
                    return Err(EvaError::invalid_argument(
                        "v3 task state cannot contain later execution fields",
                    ));
                }
                if task_format == TASK_STATE_FORMAT_V4 && has_v5_fields {
                    return Err(EvaError::invalid_argument(
                        "v4 task state cannot contain v5 replay or retry fields",
                    ));
                }
                if matches!(task_format, TASK_STATE_FORMAT_V4 | TASK_STATE_FORMAT_V5) {
                    for field in execution_fields {
                        if !seen_scalars.contains(field) {
                            return Err(EvaError::invalid_argument(
                                "task state is missing an execution scalar field",
                            )
                            .with_context("field", field));
                        }
                    }
                }
                if task_format == TASK_STATE_FORMAT_V5 {
                    for field in v5_fields {
                        if !seen_scalars.contains(field) {
                            return Err(EvaError::invalid_argument(
                                "v5 task state is missing a replay or retry scalar field",
                            )
                            .with_context("field", field));
                        }
                    }
                }
                let kind = required_stored_field(envelope_kind, "envelope_kind")?;
                let agent_id = required_stored_field(envelope_agent_id, "envelope_agent_id")?;
                let input_kind = required_stored_field(envelope_input_kind, "envelope_input_kind")?;
                let inline_input_hex =
                    required_stored_field(envelope_inline_input_hex, "envelope_inline_input_hex")?;
                let artifact_ref =
                    required_stored_field(envelope_artifact_ref, "envelope_artifact_ref")?;
                let digest = required_stored_field(envelope_input_digest, "envelope_input_digest")?;
                let idempotency_key =
                    required_stored_field(envelope_idempotency_key, "envelope_idempotency_key")?;
                let max_attempts =
                    required_stored_field(envelope_max_attempts, "envelope_max_attempts")?;
                let retry_backoff_ms =
                    required_stored_field(envelope_retry_backoff_ms, "envelope_retry_backoff_ms")?;
                let attempt_timeout_ms = envelope_attempt_timeout_ms.ok_or_else(|| {
                    EvaError::invalid_argument("task state is missing an envelope scalar field")
                        .with_context("field", "envelope_attempt_timeout_ms")
                })?;
                let input = match input_kind.as_str() {
                    "inline" if artifact_ref.is_empty() => TaskInputSnapshot::Inline {
                        bytes: decode_hex(&inline_input_hex, "envelope_inline_input_hex")?,
                        digest,
                    },
                    "artifact" if inline_input_hex.is_empty() => TaskInputSnapshot::Artifact {
                        artifact_ref,
                        digest,
                    },
                    "inline" | "artifact" => {
                        return Err(EvaError::invalid_argument(
                            "task envelope input discriminator has conflicting fields",
                        )
                        .with_context("input_kind", input_kind))
                    }
                    _ => {
                        return Err(EvaError::invalid_argument(
                            "task envelope input discriminator is unsupported",
                        )
                        .with_context("input_kind", input_kind))
                    }
                };
                snapshot.envelope = Some(TaskEnvelopeSnapshot::new(
                    kind,
                    agent_id,
                    input,
                    idempotency_key,
                    TaskAttemptPolicySnapshot::new(
                        max_attempts,
                        retry_backoff_ms,
                        attempt_timeout_ms,
                    )?,
                )?);
                if task_format == TASK_STATE_FORMAT_V5 {
                    match (
                        replay_event_id.ok_or_else(|| {
                            EvaError::invalid_argument("v5 task state is missing replay_event_id")
                        })?,
                        replay_delivery_index.ok_or_else(|| {
                            EvaError::invalid_argument(
                                "v5 task state is missing replay_delivery_index",
                            )
                        })?,
                    ) {
                        (None, None) => {}
                        (Some(event_id), Some(delivery_index)) => {
                            snapshot.replay_delivery =
                                Some(TaskReplayDeliverySnapshot::new(event_id, delivery_index)?);
                        }
                        _ => {
                            return Err(EvaError::invalid_argument(
                                "replay delivery identity fields must appear together",
                            ));
                        }
                    }
                }
            }
            Some(TASK_STATE_FORMAT_V2)
            | Some(TASK_STATE_FORMAT_V3)
            | Some(TASK_STATE_FORMAT_V4)
            | Some(TASK_STATE_FORMAT_V5) => {
                return Err(EvaError::invalid_argument(
                    "uncommitted task state cannot have a durable owner generation",
                ))
            }
            None => {
                return Err(EvaError::invalid_argument(
                    "legacy task state cannot contain version metadata",
                ))
            }
            Some(value) => {
                return Err(
                    EvaError::invalid_argument("task state format is unsupported")
                        .with_context("format", value),
                )
            }
        }
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// 在末尾追加日志，并以当前长度+1 分配序号。
    /// 调用方应保持已有序号连续；本方法不扫描或修复外部构造的重复序号。
    pub fn push_log(&mut self, level: impl Into<String>, message: impl Into<String>) {
        self.logs.push(TaskStateLogSnapshot {
            sequence: self.logs.len() as u64 + 1,
            level: level.into(),
            message: message.into(),
        });
    }

    /// 将一个可执行 queued 快照绑定到唯一 worker owner、attempt 和 cancel token。
    pub fn claim_for_execution(
        &mut self,
        execution_owner: impl Into<String>,
        heartbeat_at_ms: u128,
        deadline_at_ms: Option<u128>,
        cancel_token: impl Into<String>,
    ) -> Result<usize, EvaError> {
        if self.status != "queued" {
            return Err(EvaError::conflict("only a queued task can be claimed")
                .with_context("task_id", &self.task_id)
                .with_context("status", &self.status));
        }
        if self.envelope.is_none() {
            return Err(
                EvaError::conflict("task claim requires a recoverable task envelope")
                    .with_context("task_id", &self.task_id),
            );
        }
        if self.cancel_requested {
            return Err(
                EvaError::conflict("cancelled queued task cannot be claimed")
                    .with_context("task_id", &self.task_id),
            );
        }
        if !self.dead_letters.is_empty() {
            return Err(EvaError::conflict(
                "task with dead letters requires the retry owner before execution",
            )
            .with_context("task_id", &self.task_id));
        }
        if self.execution_owner.is_some() || self.cancel_token.is_some() {
            return Err(
                EvaError::conflict("queued task already carries an execution claim")
                    .with_context("task_id", &self.task_id),
            );
        }
        let execution_owner = execution_owner.into();
        let cancel_token = cancel_token.into();
        validate_execution_owner(&execution_owner)?;
        validate_cancel_token(&cancel_token)?;
        let attempt = self
            .attempts
            .checked_add(1)
            .ok_or_else(|| EvaError::conflict("task attempt counter is exhausted"))?;
        if attempt > self.retry_max_attempts {
            return Err(
                EvaError::conflict("task retry policy has no remaining attempt")
                    .with_context("task_id", &self.task_id)
                    .with_context("attempt", attempt.to_string())
                    .with_context("max_attempts", self.retry_max_attempts.to_string()),
            );
        }

        self.attempts = attempt;
        self.execution_owner = Some(execution_owner.clone());
        self.result_digest = None;
        self.result_size_bytes = None;
        self.interrupted_reason = None;
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.mark_running(heartbeat_at_ms, deadline_at_ms, cancel_token);
        self.push_log("info", format!("task attempt {attempt} claimed by worker"));
        self.validate()?;
        Ok(attempt)
    }

    /// 校验迟到完成者仍然拥有同一 attempt；token 只用于 fencing，不是授权 secret。
    pub fn verify_execution_claim(
        &self,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
    ) -> Result<(), EvaError> {
        if self.execution_owner.as_deref() == Some(execution_owner)
            && self.attempts == attempt
            && self.cancel_token.as_deref() == Some(cancel_token)
        {
            return Ok(());
        }
        Err(
            EvaError::conflict("task execution claim no longer matches the worker")
                .with_context("task_id", &self.task_id)
                .with_context("attempt", attempt.to_string()),
        )
    }

    /// 只有仍为 running 的匹配 attempt 才能提交成功结果摘要。
    pub fn complete_execution(
        &mut self,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
        result_digest: impl Into<String>,
        result_size_bytes: usize,
    ) -> Result<(), EvaError> {
        self.verify_execution_claim(execution_owner, attempt, cancel_token)?;
        if self.status != "running" {
            return Err(EvaError::conflict("task is no longer running")
                .with_context("task_id", &self.task_id)
                .with_context("status", &self.status));
        }
        let result_digest = result_digest.into();
        validate_canonical_sha256(&result_digest, "task result digest")?;
        self.status = "completed".to_owned();
        self.result_digest = Some(result_digest);
        self.result_size_bytes = Some(result_size_bytes);
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log("info", "task completed");
        self.validate()
    }

    /// Completes an effectful attempt after its external result is already durably committed.
    ///
    /// A cancellation that arrives after the effect prepare boundary cannot revoke the committed
    /// business fact. The cancellation request remains visible for audit, but is no longer marked
    /// accepted once the task result is reconciled.
    fn complete_committed_effect_execution(
        &mut self,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
        result_digest: impl Into<String>,
        result_size_bytes: usize,
    ) -> Result<(), EvaError> {
        self.verify_execution_claim(execution_owner, attempt, cancel_token)?;
        if !matches!(self.status.as_str(), "running" | "cancelling") {
            return Err(
                EvaError::conflict("committed effect task is no longer active")
                    .with_context("task_id", &self.task_id)
                    .with_context("status", &self.status),
            );
        }
        let result_digest = result_digest.into();
        validate_canonical_sha256(&result_digest, "task result digest")?;
        if self.status == "cancelling" {
            self.cancel_accepted = false;
            self.push_log(
                "warning",
                "task cancellation was superseded by a committed effect",
            );
        }
        self.status = "completed".to_owned();
        self.result_digest = Some(result_digest);
        self.result_size_bytes = Some(result_size_bytes);
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log("info", "task completed from committed effect");
        self.validate()
    }

    /// 只有仍为 running 的匹配 attempt 才能提交稳定失败分类和消息。
    pub fn fail_execution(
        &mut self,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
        error_kind: impl Into<String>,
        error_message: impl Into<String>,
        retryable: bool,
    ) -> Result<(), EvaError> {
        self.verify_execution_claim(execution_owner, attempt, cancel_token)?;
        if self.status != "running" {
            return Err(EvaError::conflict("task is no longer running")
                .with_context("task_id", &self.task_id)
                .with_context("status", &self.status));
        }
        self.result_digest = None;
        self.result_size_bytes = None;
        self.mark_failed(attempt, error_kind, error_message, retryable);
        self.validate()
    }

    /// 将匹配的 running attempt 以稳定 timeout 终态收口。
    pub fn time_out_execution(
        &mut self,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
        now_ms: u128,
        retryable: bool,
    ) -> Result<(), EvaError> {
        self.verify_execution_claim(execution_owner, attempt, cancel_token)?;
        if self.status != "running" {
            return Err(EvaError::conflict("task is no longer running")
                .with_context("task_id", &self.task_id)
                .with_context("status", &self.status));
        }
        self.result_digest = None;
        self.result_size_bytes = None;
        self.mark_timed_out(now_ms, retryable);
        self.validate()
    }

    /// durable cancel CAS 获胜后，只有匹配 attempt 才能把 cancelling 收口为 cancelled。
    pub fn cancel_execution(
        &mut self,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
    ) -> Result<(), EvaError> {
        self.verify_execution_claim(execution_owner, attempt, cancel_token)?;
        if self.status != "cancelling" {
            return Err(EvaError::conflict("task is not awaiting cancellation")
                .with_context("task_id", &self.task_id)
                .with_context("status", &self.status));
        }
        self.result_digest = None;
        self.result_size_bytes = None;
        self.mark_cancelled();
        self.validate()
    }

    /// 将任务标为 running，设置首个心跳、可选 deadline 和取消 token，并追加日志。
    pub fn mark_running(
        &mut self,
        heartbeat_at_ms: u128,
        deadline_at_ms: Option<u128>,
        cancel_token: impl Into<String>,
    ) {
        self.status = "running".to_owned();
        self.heartbeat_at_ms = Some(heartbeat_at_ms);
        self.deadline_at_ms = deadline_at_ms;
        self.cancel_token = Some(cancel_token.into());
        self.push_log("info", "task marked running");
    }

    /// 更新最近心跳并记录其时间；不自动改变任务状态。
    pub fn record_heartbeat(&mut self, heartbeat_at_ms: u128) {
        self.heartbeat_at_ms = Some(heartbeat_at_ms);
        self.push_log("info", format!("task heartbeat at {heartbeat_at_ms}"));
    }

    /// Update the heartbeat timestamp without appending an unbounded log entry.
    /// The fenced store API is responsible for proving the attempt owner.
    fn touch_heartbeat(&mut self, heartbeat_at_ms: u128) {
        self.heartbeat_at_ms = Some(
            self.heartbeat_at_ms
                .map(|previous| previous.max(heartbeat_at_ms))
                .unwrap_or(heartbeat_at_ms),
        );
    }

    /// 记录取消请求。
    /// 非终态转换为 cancelling 且接受；终态保持原 status 并拒绝迟到取消，但仍保存请求与原因。
    pub fn request_cancel(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.cancel_requested = true;
        self.cancel_reason = Some(reason.clone());
        if self.is_terminal() {
            self.cancel_accepted = false;
            self.push_log(
                "warning",
                "cancel requested after task reached a terminal state",
            );
        } else {
            self.cancel_accepted = true;
            self.status = "cancelling".to_owned();
            self.push_log("warning", format!("cancel requested: {reason}"));
        }
    }

    /// 将任务转换为 cancelled 终态，并确保取消请求/接受标志一致。
    pub fn mark_cancelled(&mut self) {
        self.status = "cancelled".to_owned();
        self.cancel_requested = true;
        self.cancel_accepted = true;
        self.result_digest = None;
        self.result_size_bytes = None;
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log("warning", "task marked cancelled");
    }

    /// 将任务转换为 timed_out，记录超时时刻和稳定 timeout 错误。
    pub fn mark_timed_out(&mut self, now_ms: u128, retryable: bool) {
        self.status = "timed_out".to_owned();
        self.heartbeat_at_ms = Some(now_ms);
        self.result_digest = None;
        self.result_size_bytes = None;
        self.error_kind = Some("timeout".to_owned());
        self.error_message = Some("task deadline exceeded".to_owned());
        self.error_retryable = Some(retryable);
        self.retry_ready_at_ms = None;
        self.push_log("error", format!("task timed out at {now_ms}"));
    }

    /// 将任务转换为 completed，更新尝试次数并清除旧错误。
    pub fn mark_completed(&mut self, attempts: usize) {
        self.status = "completed".to_owned();
        self.attempts = attempts;
        self.result_digest = None;
        self.result_size_bytes = None;
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log("info", "task completed");
    }

    /// 将任务转换为 failed，保存最终尝试次数和调用方提供的错误分类/消息。
    pub fn mark_failed(
        &mut self,
        attempts: usize,
        error_kind: impl Into<String>,
        error_message: impl Into<String>,
        retryable: bool,
    ) {
        self.status = "failed".to_owned();
        self.attempts = attempts;
        self.result_digest = None;
        self.result_size_bytes = None;
        self.error_kind = Some(error_kind.into());
        self.error_message = Some(error_message.into());
        self.error_retryable = Some(retryable);
        self.retry_ready_at_ms = None;
        self.push_log("error", "task failed");
    }

    /// 将任务标为 interrupted 终态并保存恢复诊断原因。
    pub fn mark_interrupted(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = "interrupted".to_owned();
        self.result_digest = None;
        self.result_size_bytes = None;
        self.interrupted_reason = Some(reason.clone());
        self.push_log("warning", format!("task interrupted: {reason}"));
    }

    /// 将任务标为 recovering 非终态，保留触发恢复的中断原因。
    pub fn mark_recovering(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = "recovering".to_owned();
        self.result_digest = None;
        self.result_size_bytes = None;
        self.interrupted_reason = Some(reason.clone());
        self.push_log("warning", format!("task recovering: {reason}"));
    }

    /// Clears an abandoned replay attempt so a later writer generation can claim it again.
    pub fn mark_abandoned_replay_delivery_queued(&mut self) -> Result<(), EvaError> {
        if self.replay_delivery.is_none()
            || !matches!(
                self.status.as_str(),
                "running" | "interrupted" | "recovering"
            )
            || self.cancel_requested
            || self.cancel_accepted
            || !self.dead_letters.is_empty()
            || self.attempts >= self.retry_max_attempts
            || self.requires_operator_reconciliation()
        {
            return Err(EvaError::conflict(
                "task is not an abandoned replay delivery eligible for recovery",
            )
            .with_context("task_id", &self.task_id)
            .with_context("status", &self.status));
        }
        let previous_status = self.status.clone();
        self.status = "queued".to_owned();
        self.execution_owner = None;
        self.cancel_token = None;
        self.heartbeat_at_ms = None;
        self.deadline_at_ms = None;
        self.result_digest = None;
        self.result_size_bytes = None;
        self.interrupted_reason = None;
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log(
            "warning",
            format!(
                "abandoned replay delivery recovered from {previous_status} after writer turnover"
            ),
        );
        self.validate()
    }

    /// Clears a safely retryable abandoned task after writer-generation turnover.
    fn mark_abandoned_task_queued(&mut self) -> Result<(), EvaError> {
        if self.replay_delivery.is_some()
            || self.envelope.is_none()
            || !matches!(
                self.status.as_str(),
                "running" | "interrupted" | "recovering"
            )
            || self.cancel_requested
            || self.cancel_accepted
            || !self.dead_letters.is_empty()
            || self.attempts >= self.retry_max_attempts
            || self.requires_operator_reconciliation()
        {
            return Err(EvaError::conflict(
                "task is not an abandoned task eligible for safe recovery",
            )
            .with_context("task_id", &self.task_id)
            .with_context("status", &self.status));
        }
        let previous_status = self.status.clone();
        self.status = "queued".to_owned();
        self.execution_owner = None;
        self.cancel_token = None;
        self.heartbeat_at_ms = None;
        self.deadline_at_ms = None;
        self.result_digest = None;
        self.result_size_bytes = None;
        self.interrupted_reason = None;
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log(
            "warning",
            format!("abandoned task recovered from {previous_status} for a safe retry"),
        );
        self.validate()
    }

    /// Reconciles task state to an immutable result already committed by the effect ledger.
    fn recover_committed_effect(
        &mut self,
        operation_digest: &str,
        result_digest: &str,
        result_size_bytes: usize,
    ) -> Result<(), EvaError> {
        validate_canonical_sha256(operation_digest, "effect operation digest")?;
        validate_canonical_sha256(result_digest, "task result digest")?;
        if self.envelope.is_none() {
            return Err(
                EvaError::conflict("committed effect recovery requires a task envelope")
                    .with_context("task_id", &self.task_id),
            );
        }
        if self.status == "completed" {
            if self.result_digest.as_deref() == Some(result_digest)
                && self.result_size_bytes == Some(result_size_bytes)
            {
                return Ok(());
            }
            return Err(EvaError::conflict(
                "committed effect conflicts with the existing task result",
            )
            .with_context("task_id", &self.task_id)
            .with_context("operation_digest", operation_digest));
        }
        if !matches!(
            self.status.as_str(),
            "queued"
                | "running"
                | "cancelling"
                | "failed"
                | "timed_out"
                | "cancelled"
                | "interrupted"
                | "recovering"
        ) {
            return Err(EvaError::conflict(
                "task status cannot be reconciled from a committed effect",
            )
            .with_context("task_id", &self.task_id)
            .with_context("status", &self.status));
        }
        self.status = "completed".to_owned();
        self.cancel_accepted = false;
        self.result_digest = Some(result_digest.to_owned());
        self.result_size_bytes = Some(result_size_bytes);
        self.interrupted_reason = None;
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log(
            "info",
            format!("{COMMITTED_EFFECT_RECOVERY_LOG_PREFIX}{operation_digest}"),
        );
        self.validate()
    }

    /// Converts an unresolved prepared effect into a stable operator-owned terminal block.
    fn block_unknown_effect(&mut self, operation_digest: &str) -> Result<(), EvaError> {
        validate_canonical_sha256(operation_digest, "effect operation digest")?;
        let reason = format!("{UNKNOWN_EFFECT_RECOVERY_REASON_PREFIX}{operation_digest}");
        if self.status == "interrupted" && self.interrupted_reason.as_deref() == Some(&reason) {
            return Ok(());
        }
        if self.status == "completed" {
            return Err(EvaError::conflict(
                "completed task cannot be blocked by a prepared effect",
            )
            .with_context("task_id", &self.task_id)
            .with_context("operation_digest", operation_digest));
        }
        if self.envelope.is_none() {
            return Err(
                EvaError::conflict("prepared effect recovery requires a task envelope")
                    .with_context("task_id", &self.task_id),
            );
        }
        if !matches!(
            self.status.as_str(),
            "queued"
                | "running"
                | "cancelling"
                | "failed"
                | "timed_out"
                | "cancelled"
                | "interrupted"
                | "recovering"
        ) {
            return Err(
                EvaError::conflict("task status cannot be blocked by a prepared effect")
                    .with_context("task_id", &self.task_id)
                    .with_context("status", &self.status),
            );
        }
        self.status = "interrupted".to_owned();
        self.cancel_accepted = false;
        self.result_digest = None;
        self.result_size_bytes = None;
        self.interrupted_reason = Some(reason.clone());
        self.error_kind = None;
        self.error_message = None;
        self.error_retryable = None;
        self.retry_ready_at_ms = None;
        self.push_log("error", reason);
        self.validate()
    }

    /// 判断 now 是否到达或超过 deadline；无 deadline 永不超时。
    pub fn deadline_expired(&self, now_ms: u128) -> bool {
        self.deadline_at_ms
            .map(|deadline| now_ms >= deadline)
            .unwrap_or(false)
    }

    /// Return the non-negative age of the latest persisted heartbeat.
    pub fn heartbeat_age_ms(&self, now_ms: u128) -> Option<u128> {
        self.heartbeat_at_ms
            .map(|heartbeat| now_ms.saturating_sub(heartbeat))
    }

    /// Derive a liveness classification without mutating the task lifecycle.
    /// Missing heartbeats are treated conservatively as stale. Thresholds are
    /// caller-owned so daemon and tests can use a shorter controlled window.
    pub fn freshness_at(
        &self,
        now_ms: u128,
        degraded_after_ms: u128,
        stale_after_ms: u128,
    ) -> TaskFreshness {
        if !matches!(self.status.as_str(), "running" | "cancelling") {
            return TaskFreshness::NotApplicable;
        }
        if self.execution_owner.is_none() || self.cancel_token.is_none() || self.attempts == 0 {
            return TaskFreshness::Stale;
        }
        let Some(age_ms) = self.heartbeat_age_ms(now_ms) else {
            return TaskFreshness::Stale;
        };
        if degraded_after_ms > stale_after_ms {
            return TaskFreshness::Stale;
        }
        if age_ms < degraded_after_ms {
            TaskFreshness::Live
        } else if age_ms < stale_after_ms {
            TaskFreshness::Degraded
        } else {
            TaskFreshness::Stale
        }
    }

    /// Derive liveness using the repository's default task heartbeat windows.
    pub fn default_freshness_at(&self, now_ms: u128) -> TaskFreshness {
        self.freshness_at(
            now_ms,
            DEFAULT_TASK_HEARTBEAT_DEGRADED_AFTER_MS,
            DEFAULT_TASK_HEARTBEAT_STALE_AFTER_MS,
        )
    }

    /// 判断状态是否禁止继续正常执行；interrupted 视为终态，recovering 不是。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "timed_out" | "interrupted"
        )
    }

    /// Whether restart recovery has delegated an unknown non-idempotent result to an operator.
    pub fn requires_operator_reconciliation(&self) -> bool {
        self.status == "interrupted"
            && self
                .interrupted_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with(UNKNOWN_EFFECT_RECOVERY_REASON_PREFIX))
    }
}

impl FileSystemTaskStateStore {
    /// 使用传统项目布局 `<root>/.eva/tasks` 创建 store。
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        let project_root = project_root.as_ref().to_path_buf();
        let task_dir = project_root.join(".eva").join("tasks");
        Self {
            project_root,
            task_dir,
            durable_writer_required: false,
            writer: None,
        }
    }

    /// 使用 durable backend 的 task_dir 创建 store。
    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self {
            project_root: layout.root.clone(),
            task_dir: layout.task_dir.clone(),
            durable_writer_required: true,
            writer: None,
        }
    }

    /// 使用与 layout 同根的 runtime writer ownership 创建可写 durable task store。
    pub fn from_runtime_writer(
        layout: &DurableBackendLayout,
        writer: DurableWriterGuard,
    ) -> Result<Self, EvaError> {
        if writer.root() != layout.root {
            return Err(EvaError::conflict(
                "durable task writer belongs to a different backend root",
            )
            .with_context("layout_root", layout.root.display().to_string())
            .with_context("writer_root", writer.root().display().to_string()));
        }
        writer.verify_current()?;
        Ok(Self {
            project_root: layout.root.clone(),
            task_dir: layout.task_dir.clone(),
            durable_writer_required: true,
            writer: Some(writer),
        })
    }

    /// 从读写 backend 获取新的 runtime ownership 并构造可写 task store。
    pub fn from_writable_backend(backend: &FileSystemDurableBackend) -> Result<Self, EvaError> {
        Self::from_runtime_writer(backend.layout(), backend.acquire_runtime_writer()?)
    }

    /// 返回 store 所属项目/backend 根。
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Runtime writer generation used by cross-record transactions and restart fencing.
    pub fn runtime_writer_generation(&self) -> Option<WriterGeneration> {
        self.writer.as_ref().map(DurableWriterGuard::generation)
    }

    /// 克隆返回任务目录，供调用方检查或传递路径所有权。
    pub fn task_dir(&self) -> PathBuf {
        self.task_dir.clone()
    }

    /// 按文件名排序加载所有 ID 快照，显式排除 `latest-basic.task` 别名避免重复。
    /// 目录缺失返回空集合；任一 `.task` 文件损坏使列表整体失败，避免恢复漏掉任务。
    pub fn list_snapshots(&self) -> Result<Vec<TaskStateSnapshot>, EvaError> {
        self.list_records()
    }

    /// 按文件名排序加载所有带 record version/owner generation 的权威 ID 记录。
    pub fn list_records(&self) -> Result<Vec<TaskStateSnapshot>, EvaError> {
        let dir = self.task_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(EvaError::internal("failed to read task state directory")
                    .with_context("path", dir.display().to_string())
                    .with_context("io_error", error.to_string()));
            }
        };

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                EvaError::internal("failed to read task state directory entry")
                    .with_context("path", dir.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some("latest-basic.task") {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) == Some("task") {
                paths.push(path);
            }
        }
        paths.sort();

        paths
            .into_iter()
            .map(|path| {
                let data = fs::read_to_string(&path).map_err(|error| {
                    EvaError::internal("failed to read task state")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
                let snapshot = TaskStateSnapshot::from_storage(&data)
                    .map_err(|error| error.with_context("path", path.display().to_string()))?;
                let expected_task_id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        EvaError::conflict("task state file name is not valid UTF-8")
                            .with_context("path", path.display().to_string())
                    })?;
                if snapshot.task_id != expected_task_id {
                    return Err(
                        EvaError::conflict("task state file key does not match record")
                            .with_context("path", path.display().to_string())
                            .with_context("expected_task_id", expected_task_id)
                            .with_context("actual_task_id", &snapshot.task_id),
                    );
                }
                Ok(snapshot)
            })
            .collect()
    }

    /// 尝试领取一个 queued 任务；正常 CAS 竞争或不可领取状态返回 `Ok(None)`。
    pub fn try_claim_queued(
        &mut self,
        task_id: &str,
        execution_owner: &str,
        cancel_token: &str,
        observed_at_ms: u128,
    ) -> Result<Option<TaskExecutionClaim>, EvaError> {
        validate_execution_owner(execution_owner)?;
        validate_cancel_token(cancel_token)?;
        let snapshot = self.read(Some(task_id))?;
        if snapshot.status != "queued"
            || snapshot.envelope.is_none()
            || snapshot.cancel_requested
            || !snapshot.dead_letters.is_empty()
            || snapshot.attempts >= snapshot.retry_max_attempts
            || snapshot.execution_owner.is_some()
            || snapshot.cancel_token.is_some()
        {
            return Ok(None);
        }
        let deadline_at_ms = snapshot
            .envelope
            .as_ref()
            .and_then(|envelope| envelope.attempt_policy.attempt_timeout_ms)
            .map(|timeout_ms| {
                observed_at_ms
                    .checked_add(u128::from(timeout_ms))
                    .ok_or_else(|| {
                        EvaError::conflict("task attempt deadline is out of range")
                            .with_context("task_id", task_id)
                    })
            })
            .transpose()?;
        let mut candidate = snapshot.clone();
        let attempt = candidate.claim_for_execution(
            execution_owner,
            observed_at_ms,
            deadline_at_ms,
            cancel_token,
        )?;

        match self.compare_and_set(&candidate) {
            Ok(committed) => Ok(Some(task_execution_claim(committed)?)),
            Err(error) => {
                let current = self.read(Some(task_id))?;
                if current.record_version == snapshot.record_version {
                    return Err(error);
                }
                if current.status == "running"
                    && current.execution_owner.as_deref() == Some(execution_owner)
                    && current.attempts == attempt
                    && current.cancel_token.as_deref() == Some(cancel_token)
                {
                    let current = self.refresh_latest(task_id)?;
                    return Ok(Some(task_execution_claim(current)?));
                }
                Ok(None)
            }
        }
    }

    /// Schedules or performs the retry transition for a retryable terminal attempt.
    ///
    /// A non-zero envelope backoff is first persisted as `retry_ready_at_ms`; the task remains
    /// failed until a later observation reaches that boundary. This is the only terminal rollback
    /// entry point, and every CAS loser reloads the authoritative record before retrying.
    pub fn requeue_retryable(
        &mut self,
        task_id: &str,
        observed_at_ms: u128,
    ) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(task_id)?;
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let current = self.read(Some(task_id))?;
            let Some(candidate) = task_retry_transition_candidate(&current, observed_at_ms) else {
                return Ok(current);
            };
            let expected_version = current.record_version;
            match self.compare_and_set_retry_requeue(&candidate) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(task_id))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                    if task_retry_transition_candidate(&observed, observed_at_ms).is_none() {
                        return Ok(observed);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("task retry requeue exceeded the CAS retry limit")
                .with_context("task_id", task_id),
        )
    }

    /// Requeues one abandoned internal replay delivery after writer-generation turnover.
    ///
    /// Operator tasks cannot enter this path because the immutable v5 replay marker is required.
    /// A running task owned by the current writer generation is never reclaimed.
    pub fn recover_abandoned_replay_delivery(
        &mut self,
        task_id: &str,
    ) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(task_id)?;
        let writer_generation = self
            .writer
            .as_ref()
            .ok_or_else(|| {
                EvaError::conflict("replay delivery recovery requires runtime writer ownership")
            })?
            .generation();
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let current = self.read(Some(task_id))?;
            if current.replay_delivery.is_none() {
                return Err(
                    EvaError::conflict("task is not a persisted replay delivery")
                        .with_context("task_id", task_id),
                );
            }
            if current.status == "running" && current.owner_generation == writer_generation {
                return Err(EvaError::conflict(
                    "current writer cannot reclaim its own running replay delivery",
                )
                .with_context("task_id", task_id));
            }
            let Some(candidate) = task_replay_recovery_candidate(&current) else {
                return Ok(current);
            };
            let expected_version = current.record_version;
            match self.compare_and_set_retry_requeue(&candidate) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(task_id))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                    if task_replay_recovery_candidate(&observed).is_none() {
                        return Ok(observed);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("replay delivery recovery exceeded the CAS retry limit")
                .with_context("task_id", task_id),
        )
    }

    /// Requeues an abandoned task only after the caller has proved retry is side-effect safe.
    pub fn recover_abandoned_task_for_retry(
        &mut self,
        task_id: &str,
    ) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(task_id)?;
        let writer_generation = self.recovery_writer_generation()?;
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let current = self.read(Some(task_id))?;
            if current.replay_delivery.is_some() {
                return Err(EvaError::conflict(
                    "operator task recovery cannot mutate a replay delivery",
                )
                .with_context("task_id", task_id));
            }
            if current.status == "queued" {
                return Ok(current);
            }
            let Some(candidate) = task_abandoned_recovery_candidate(&current) else {
                return Ok(current);
            };
            reject_current_generation_recovery(&current, writer_generation)?;
            let expected_version = current.record_version;
            match self.compare_and_set_retry_requeue(&candidate) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(task_id))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                    if observed.status == "queued"
                        || task_abandoned_recovery_candidate(&observed).is_none()
                    {
                        return Ok(observed);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("abandoned task recovery exceeded the CAS retry limit")
                .with_context("task_id", task_id),
        )
    }

    /// Repairs any contradictory task lifecycle from an immutable committed effect result.
    pub fn recover_task_from_committed_effect(
        &mut self,
        task_id: &str,
        operation_digest: &str,
        result_digest: &str,
        result_size_bytes: usize,
    ) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(task_id)?;
        validate_canonical_sha256(operation_digest, "effect operation digest")?;
        validate_canonical_sha256(result_digest, "task result digest")?;
        let writer_generation = self.recovery_writer_generation()?;
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let current = self.read(Some(task_id))?;
            let mut candidate = current.clone();
            candidate.recover_committed_effect(
                operation_digest,
                result_digest,
                result_size_bytes,
            )?;
            if candidate == current {
                return Ok(current);
            }
            reject_current_generation_recovery(&current, writer_generation)?;
            let expected_version = current.record_version;
            match self.compare_and_set_restart_recovery(&candidate) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(task_id))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("committed effect task recovery exceeded the CAS retry limit")
                .with_context("task_id", task_id)
                .with_context("operation_digest", operation_digest),
        )
    }

    /// Persists a terminal manual block for a prepared effect whose outcome is unknowable.
    pub fn block_task_for_unknown_effect(
        &mut self,
        task_id: &str,
        operation_digest: &str,
    ) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(task_id)?;
        validate_canonical_sha256(operation_digest, "effect operation digest")?;
        let writer_generation = self.recovery_writer_generation()?;
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let current = self.read(Some(task_id))?;
            let mut candidate = current.clone();
            candidate.block_unknown_effect(operation_digest)?;
            if candidate == current {
                return Ok(current);
            }
            reject_current_generation_recovery(&current, writer_generation)?;
            let expected_version = current.record_version;
            match self.compare_and_set_restart_recovery(&candidate) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(task_id))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("unknown effect task block exceeded the CAS retry limit")
                .with_context("task_id", task_id)
                .with_context("operation_digest", operation_digest),
        )
    }

    /// 在最新 record version 上验证完整 attempt fence，并提交结果或合并并发取消。
    pub fn finish_execution(
        &mut self,
        fence: &TaskAttemptFence,
        outcome: &TaskAttemptOutcome,
    ) -> Result<TaskStateSnapshot, EvaError> {
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let mut current = self.read(Some(fence.task_id()))?;
            verify_task_attempt_fence(&current, fence)?;
            if task_attempt_outcome_is_committed(&current, outcome) || current.status == "cancelled"
            {
                return self.refresh_latest(fence.task_id());
            }
            match current.status.as_str() {
                "running" => apply_task_attempt_outcome(&mut current, fence, outcome)?,
                "cancelling" => current.cancel_execution(
                    fence.execution_owner(),
                    fence.attempt(),
                    fence.cancel_token(),
                )?,
                _ => {
                    return Err(
                        EvaError::conflict("task attempt terminal result was superseded")
                            .with_context("task_id", fence.task_id())
                            .with_context("status", &current.status),
                    )
                }
            }
            let expected_version = current.record_version;
            match self.compare_and_set_attempt_outcome(&current) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(fence.task_id()))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("task attempt finish exceeded the CAS retry limit")
                .with_context("task_id", fence.task_id()),
        )
    }

    /// Finishes an active task from a result already committed in the effect ledger.
    ///
    /// Unlike ordinary handler completion, this narrow path lets a committed effect outrank a
    /// cancellation that raced after prepare. It still requires the exact live attempt fence and
    /// never rewrites an existing terminal outcome.
    pub fn finish_committed_effect_execution(
        &mut self,
        fence: &TaskAttemptFence,
        result_digest: &str,
        result_size_bytes: usize,
    ) -> Result<TaskStateSnapshot, EvaError> {
        let outcome = TaskAttemptOutcome::Completed {
            result_digest: result_digest.to_owned(),
            result_size_bytes,
        };
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let mut current = self.read(Some(fence.task_id()))?;
            verify_task_attempt_fence(&current, fence)?;
            if task_attempt_outcome_is_committed(&current, &outcome) {
                return self.refresh_latest(fence.task_id());
            }
            if !matches!(current.status.as_str(), "running" | "cancelling") {
                return Err(
                    EvaError::conflict("committed effect task outcome was superseded")
                        .with_context("task_id", fence.task_id())
                        .with_context("status", &current.status),
                );
            }
            current.complete_committed_effect_execution(
                fence.execution_owner(),
                fence.attempt(),
                fence.cancel_token(),
                result_digest,
                result_size_bytes,
            )?;
            let expected_version = current.record_version;
            match self.compare_and_set_attempt_outcome(&current) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(fence.task_id()))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("committed effect task finish exceeded the CAS retry limit")
                .with_context("task_id", fence.task_id()),
        )
    }

    /// Requests shutdown cancellation only while the exact fenced attempt is still active.
    ///
    /// Unlike the operator-facing task-ID API, this path never annotates a terminal task with a
    /// late cancellation after the worker has already committed its result.
    pub fn request_execution_cancellation(
        &mut self,
        fence: &TaskAttemptFence,
        reason: impl Into<String>,
    ) -> Result<TaskStateSnapshot, EvaError> {
        let reason = reason.into();
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let mut current = self.read(Some(fence.task_id()))?;
            verify_task_attempt_fence(&current, fence)?;
            if current.is_terminal() {
                return self.refresh_latest(fence.task_id());
            }
            if current.cancel_requested {
                return self.refresh_latest(fence.task_id());
            }
            if !matches!(current.status.as_str(), "running" | "cancelling") {
                return Err(
                    EvaError::conflict("task attempt cannot enter shutdown cancellation")
                        .with_context("task_id", fence.task_id())
                        .with_context("status", &current.status),
                );
            }
            current.request_cancel(reason.clone());
            let expected_version = current.record_version;
            match self.compare_and_set(&current) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(fence.task_id()))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                    if observed.is_terminal() || observed.cancel_requested {
                        return self.refresh_latest(fence.task_id());
                    }
                }
            }
        }
        Err(
            EvaError::conflict("task shutdown cancellation exceeded the CAS retry limit")
                .with_context("task_id", fence.task_id()),
        )
    }

    /// Converts the exact active attempt to an operator-owned block after a prepared effect's
    /// handler outlives the daemon drain deadline. The detached handler owns no ledger permit, so
    /// its late return cannot commit or rewrite this terminal state.
    pub fn block_prepared_effect_execution(
        &mut self,
        fence: &TaskAttemptFence,
        operation_digest: &str,
    ) -> Result<TaskStateSnapshot, EvaError> {
        validate_canonical_sha256(operation_digest, "effect operation digest")?;
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let current = self.read(Some(fence.task_id()))?;
            verify_task_attempt_fence(&current, fence)?;
            let mut candidate = current.clone();
            candidate.block_unknown_effect(operation_digest)?;
            if candidate == current {
                return self.refresh_latest(fence.task_id());
            }
            if !matches!(current.status.as_str(), "running" | "cancelling") {
                return Err(
                    EvaError::conflict("prepared effect attempt is no longer active")
                        .with_context("task_id", fence.task_id())
                        .with_context("status", &current.status),
                );
            }
            let expected_version = current.record_version;
            match self.compare_and_set_attempt_outcome(&candidate) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(fence.task_id()))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                    if observed.requires_operator_reconciliation() {
                        if observed.interrupted_reason == candidate.interrupted_reason {
                            return self.refresh_latest(fence.task_id());
                        }
                        return Err(EvaError::conflict(
                            "prepared effect shutdown block conflicts with another operation",
                        )
                        .with_context("task_id", fence.task_id())
                        .with_context("operation_digest", operation_digest));
                    }
                }
            }
        }
        Err(
            EvaError::conflict("prepared effect shutdown block exceeded the CAS retry limit")
                .with_context("task_id", fence.task_id())
                .with_context("operation_digest", operation_digest),
        )
    }

    /// Persist a monotonic heartbeat for the exact claimed attempt.
    ///
    /// The owner, writer generation, attempt number, and cancel token are
    /// verified on every CAS retry. A heartbeat arriving after a terminal
    /// transition is rejected rather than reviving or rewriting that outcome.
    pub fn heartbeat_execution(
        &mut self,
        fence: &TaskAttemptFence,
        heartbeat_at_ms: u128,
    ) -> Result<TaskStateSnapshot, EvaError> {
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let mut current = self.read(Some(fence.task_id()))?;
            verify_task_attempt_fence(&current, fence)?;
            if !matches!(current.status.as_str(), "running" | "cancelling") {
                return Err(
                    EvaError::conflict("task heartbeat cannot renew a terminal attempt")
                        .with_context("task_id", fence.task_id())
                        .with_context("status", &current.status),
                );
            }
            let previous_heartbeat = current.heartbeat_at_ms.unwrap_or_default();
            if heartbeat_at_ms <= previous_heartbeat {
                return Ok(current);
            }
            current.touch_heartbeat(heartbeat_at_ms);
            let expected_version = current.record_version;
            match self.compare_and_set_heartbeat(&current) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(fence.task_id()))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("task heartbeat exceeded the CAS retry limit")
                .with_context("task_id", fence.task_id()),
        )
    }

    /// 以重读-CAS 循环提交取消，避免与 claim/finish 竞争时丢失控制请求。
    pub fn request_cancellation(
        &mut self,
        task_id: &str,
        reason: impl Into<String>,
    ) -> Result<TaskStateSnapshot, EvaError> {
        let reason = reason.into();
        for _ in 0..TASK_STATE_CAS_RETRY_LIMIT {
            let mut current = self.read(Some(task_id))?;
            if current.cancel_requested && current.cancel_reason.as_deref() == Some(&reason) {
                return self.refresh_latest(task_id);
            }
            let was_queued = current.status == "queued";
            current.request_cancel(reason.clone());
            if was_queued {
                current.mark_cancelled();
            }
            let expected_version = current.record_version;
            match self.compare_and_set(&current) {
                Ok(committed) => return Ok(committed),
                Err(error) => {
                    let observed = self.read(Some(task_id))?;
                    if observed.record_version == expected_version {
                        return Err(error);
                    }
                    if observed.cancel_requested
                        && observed.cancel_reason.as_deref() == Some(&reason)
                    {
                        return self.refresh_latest(task_id);
                    }
                }
            }
        }
        Err(
            EvaError::conflict("task cancellation exceeded the CAS retry limit")
                .with_context("task_id", task_id),
        )
    }

    /// 对指定任务执行带版本的读-改-CAS，并返回由 store stamp 的已提交快照。
    pub fn update_snapshot<F>(
        &mut self,
        task_id: &str,
        update: F,
    ) -> Result<TaskStateSnapshot, EvaError>
    where
        F: FnOnce(&mut TaskStateSnapshot) -> Result<(), EvaError>,
    {
        let mut snapshot = self.read(Some(task_id))?;
        let expected_version = snapshot.record_version;
        let expected_generation = snapshot.owner_generation;
        update(&mut snapshot)?;
        if snapshot.record_version != expected_version
            || snapshot.owner_generation != expected_generation
        {
            return Err(EvaError::invalid_argument(
                "task update cannot modify record version or owner generation",
            ));
        }
        self.compare_and_set(&snapshot)
    }

    /// 只在权威 ID 文件不存在时创建 version 1 记录。
    pub fn create(&mut self, snapshot: &TaskStateSnapshot) -> Result<TaskStateSnapshot, EvaError> {
        if snapshot.record_version != StateVersion::ZERO {
            return Err(EvaError::invalid_argument(
                "new task state must start at record version zero",
            )
            .with_context("task_id", &snapshot.task_id)
            .with_context("actual", snapshot.record_version.0.to_string()));
        }
        self.commit_snapshot(snapshot, StateVersion::ZERO, TaskStateCommitMode::Create)
    }

    /// 使用 snapshot 携带的 record version 作为 expected 值执行持久 CAS。
    pub fn compare_and_set(
        &mut self,
        snapshot: &TaskStateSnapshot,
    ) -> Result<TaskStateSnapshot, EvaError> {
        self.commit_snapshot(
            snapshot,
            snapshot.record_version,
            TaskStateCommitMode::CompareAndSet,
        )
    }

    fn compare_and_set_attempt_outcome(
        &mut self,
        snapshot: &TaskStateSnapshot,
    ) -> Result<TaskStateSnapshot, EvaError> {
        self.commit_snapshot(
            snapshot,
            snapshot.record_version,
            TaskStateCommitMode::AttemptOutcome,
        )
    }

    fn compare_and_set_heartbeat(
        &mut self,
        snapshot: &TaskStateSnapshot,
    ) -> Result<TaskStateSnapshot, EvaError> {
        self.commit_snapshot(
            snapshot,
            snapshot.record_version,
            TaskStateCommitMode::Heartbeat,
        )
    }

    fn compare_and_set_retry_requeue(
        &mut self,
        snapshot: &TaskStateSnapshot,
    ) -> Result<TaskStateSnapshot, EvaError> {
        self.commit_snapshot(
            snapshot,
            snapshot.record_version,
            TaskStateCommitMode::RetryRequeue,
        )
    }

    fn compare_and_set_restart_recovery(
        &mut self,
        snapshot: &TaskStateSnapshot,
    ) -> Result<TaskStateSnapshot, EvaError> {
        self.commit_snapshot(
            snapshot,
            snapshot.record_version,
            TaskStateCommitMode::RestartRecovery,
        )
    }

    fn recovery_writer_generation(&self) -> Result<WriterGeneration, EvaError> {
        self.writer
            .as_ref()
            .map(DurableWriterGuard::generation)
            .ok_or_else(|| {
                EvaError::conflict("task restart recovery requires runtime writer ownership")
            })
    }

    /// 从权威 ID 记录原子重建 latest 派生别名，不改变 record version。
    pub fn refresh_latest(&mut self, task_id: &str) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(task_id)?;
        self.with_writer_transaction(|writer, _generation| {
            let _record_lock = acquire_record_write_lock(&self.task_store_lock_path())?;
            if let Some(writer) = writer {
                writer.verify_current()?;
            }
            let snapshot = self.read(Some(task_id))?;
            atomic_write(&self.latest_task_path(), snapshot.to_storage().as_bytes()).map_err(
                |error| {
                    EvaError::internal("failed to atomically refresh latest task state")
                        .with_context("task_id", task_id)
                        .with_context("io_error", error.to_string())
                },
            )?;
            Ok(snapshot.clone())
        })
    }

    fn commit_snapshot(
        &self,
        snapshot: &TaskStateSnapshot,
        expected: StateVersion,
        mode: TaskStateCommitMode,
    ) -> Result<TaskStateSnapshot, EvaError> {
        snapshot.validate()?;
        let dir = self.task_dir();
        fs::create_dir_all(&dir).map_err(|error| {
            EvaError::internal("failed to create task state directory")
                .with_context("path", dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        self.with_writer_transaction(|writer, generation| {
            let _record_lock = acquire_record_write_lock(&self.task_store_lock_path())?;
            if let Some(writer) = writer {
                writer.verify_current()?;
            }
            let canonical_path = self.task_path(&snapshot.task_id)?;
            let current = read_optional_task_snapshot(&canonical_path)?;
            if let Some(current) = &current {
                if current.task_id != snapshot.task_id {
                    return Err(
                        EvaError::conflict("task state file key does not match record")
                            .with_context("path", canonical_path.display().to_string())
                            .with_context("expected_task_id", &snapshot.task_id)
                            .with_context("actual_task_id", &current.task_id),
                    );
                }
                if current.owner_generation != snapshot.owner_generation {
                    return Err(EvaError::conflict(
                        "task state owner generation does not match current record",
                    )
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("expected", current.owner_generation.0.to_string())
                    .with_context("actual", snapshot.owner_generation.0.to_string()));
                }
                if current.envelope != snapshot.envelope
                    || current.replay_delivery != snapshot.replay_delivery
                {
                    return Err(EvaError::conflict(
                        "task lifecycle update cannot modify immutable task identity",
                    )
                    .with_context("task_id", &snapshot.task_id));
                }
                validate_task_state_transition(
                    current,
                    snapshot,
                    mode.allow_attempt_outcome(),
                    mode.allow_heartbeat(),
                    mode.allow_retry_requeue(),
                    mode.allow_restart_recovery(),
                )?;
            }
            if mode.create_only() && current.is_some() {
                return Err(EvaError::conflict("task state already exists")
                    .with_context("task_id", &snapshot.task_id));
            }
            let actual = current
                .as_ref()
                .map(|record| record.record_version)
                .unwrap_or(StateVersion::ZERO);
            if actual != expected {
                return Err(EvaError::conflict("task state version conflict")
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("expected", expected.0.to_string())
                    .with_context("actual", actual.0.to_string()));
            }
            let mut committed = snapshot.clone();
            committed.record_version = actual.checked_next()?;
            committed.owner_generation = generation;
            committed.validate()?;
            let data = committed.to_storage();
            atomic_write(&canonical_path, data.as_bytes()).map_err(|error| {
                EvaError::internal("failed to atomically write task state")
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("path", canonical_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            atomic_write(&self.latest_task_path(), data.as_bytes()).map_err(|error| {
                EvaError::internal("failed to atomically write latest task state")
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("canonical_committed", "true")
                    .with_context("current_version", committed.record_version.0.to_string())
                    .with_context("io_error", error.to_string())
            })?;
            Ok(committed)
        })
    }

    fn with_writer_transaction<T>(
        &self,
        operation: impl FnOnce(Option<&DurableWriterGuard>, WriterGeneration) -> Result<T, EvaError>,
    ) -> Result<T, EvaError> {
        match self.writer.clone() {
            Some(writer) => {
                writer.with_write_lock(|generation| operation(Some(&writer), generation))
            }
            None if self.durable_writer_required => Err(EvaError::conflict(
                "durable task mutation requires runtime writer ownership",
            )
            .with_context("root", self.project_root.display().to_string())),
            None => operation(None, WriterGeneration::ZERO),
        }
    }

    /// 返回兼容 CLI “最近任务”查询的固定别名路径。
    fn latest_task_path(&self) -> PathBuf {
        self.task_dir().join("latest-basic.task")
    }

    /// 校验 task ID 为 RequestId 后构造单层文件名，防止路径穿越。
    fn task_path(&self, task_id: &str) -> Result<PathBuf, EvaError> {
        RequestId::parse(task_id)?;
        Ok(self.task_dir().join(format!("{task_id}.task")))
    }

    fn task_store_lock_path(&self) -> PathBuf {
        self.task_dir().join("task-state.cas.lock")
    }
}

impl TaskStateStore for FileSystemTaskStateStore {
    /// 兼容入口只允许创建，禁止以无版本 blind upsert 覆盖既有任务。
    fn write(&mut self, snapshot: &TaskStateSnapshot) -> Result<(), EvaError> {
        self.create(snapshot).map(|_| ())
    }

    /// 读取 ID 快照或 latest 别名并严格解析。
    /// 缺失映射为 NotFound；其他 I/O 与解析错误保持原分类并携带路径。
    fn read(&self, task_id: Option<&str>) -> Result<TaskStateSnapshot, EvaError> {
        let path = match task_id {
            Some(task_id) => self.task_path(task_id)?,
            None => self.latest_task_path(),
        };
        let snapshot = read_task_snapshot(&path)?;
        if let Some(expected_task_id) = task_id {
            if snapshot.task_id != expected_task_id {
                return Err(
                    EvaError::conflict("task state file key does not match record")
                        .with_context("path", path.display().to_string())
                        .with_context("expected_task_id", expected_task_id)
                        .with_context("actual_task_id", &snapshot.task_id),
                );
            }
        }
        Ok(snapshot)
    }
}

fn read_optional_task_snapshot(path: &Path) -> Result<Option<TaskStateSnapshot>, EvaError> {
    match fs::read_to_string(path) {
        Ok(data) => TaskStateSnapshot::from_storage(&data)
            .map(Some)
            .map_err(|error| error.with_context("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(EvaError::internal("failed to read task state")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())),
    }
}

fn read_task_snapshot(path: &Path) -> Result<TaskStateSnapshot, EvaError> {
    read_optional_task_snapshot(path)?.ok_or_else(|| {
        EvaError::not_found("task state does not exist")
            .with_context("path", path.display().to_string())
            .with_context("suggestion", "run `eva run --example basic` first")
    })
}

fn task_execution_claim(snapshot: TaskStateSnapshot) -> Result<TaskExecutionClaim, EvaError> {
    let execution_owner = snapshot.execution_owner.clone().ok_or_else(|| {
        EvaError::internal("committed task claim is missing execution owner")
            .with_context("task_id", &snapshot.task_id)
    })?;
    let cancel_token = snapshot.cancel_token.clone().ok_or_else(|| {
        EvaError::internal("committed task claim is missing cancel token")
            .with_context("task_id", &snapshot.task_id)
    })?;
    if snapshot.status != "running" || snapshot.attempts == 0 {
        return Err(
            EvaError::internal("committed task claim is not a running attempt")
                .with_context("task_id", &snapshot.task_id)
                .with_context("status", &snapshot.status),
        );
    }
    let fence = TaskAttemptFence {
        task_id: snapshot.task_id.clone(),
        owner_generation: snapshot.owner_generation,
        execution_owner,
        attempt: snapshot.attempts,
        cancel_token,
    };
    Ok(TaskExecutionClaim { snapshot, fence })
}

fn verify_task_attempt_fence(
    snapshot: &TaskStateSnapshot,
    fence: &TaskAttemptFence,
) -> Result<(), EvaError> {
    if snapshot.task_id != fence.task_id {
        return Err(EvaError::conflict(
            "task attempt fence belongs to another task",
        ));
    }
    if snapshot.owner_generation != fence.owner_generation {
        return Err(
            EvaError::conflict("task attempt fence belongs to another writer generation")
                .with_context("task_id", fence.task_id()),
        );
    }
    snapshot.verify_execution_claim(
        fence.execution_owner(),
        fence.attempt(),
        fence.cancel_token(),
    )
}

fn apply_task_attempt_outcome(
    snapshot: &mut TaskStateSnapshot,
    fence: &TaskAttemptFence,
    outcome: &TaskAttemptOutcome,
) -> Result<(), EvaError> {
    match outcome {
        TaskAttemptOutcome::Completed {
            result_digest,
            result_size_bytes,
        } => snapshot.complete_execution(
            fence.execution_owner(),
            fence.attempt(),
            fence.cancel_token(),
            result_digest.clone(),
            *result_size_bytes,
        ),
        TaskAttemptOutcome::Failed {
            error_kind,
            error_message,
            retryable,
        } => snapshot.fail_execution(
            fence.execution_owner(),
            fence.attempt(),
            fence.cancel_token(),
            error_kind.clone(),
            error_message.clone(),
            *retryable,
        ),
        TaskAttemptOutcome::TimedOut {
            observed_at_ms,
            retryable,
        } => snapshot.time_out_execution(
            fence.execution_owner(),
            fence.attempt(),
            fence.cancel_token(),
            *observed_at_ms,
            *retryable,
        ),
    }
}

fn task_attempt_outcome_is_committed(
    snapshot: &TaskStateSnapshot,
    outcome: &TaskAttemptOutcome,
) -> bool {
    match outcome {
        TaskAttemptOutcome::Completed {
            result_digest,
            result_size_bytes,
        } => {
            snapshot.status == "completed"
                && snapshot.result_digest.as_ref() == Some(result_digest)
                && snapshot.result_size_bytes == Some(*result_size_bytes)
        }
        TaskAttemptOutcome::Failed {
            error_kind,
            error_message,
            retryable,
        } => {
            snapshot.status == "failed"
                && snapshot.error_kind.as_ref() == Some(error_kind)
                && snapshot.error_message.as_ref() == Some(error_message)
                && snapshot.error_retryable == Some(*retryable)
        }
        TaskAttemptOutcome::TimedOut {
            observed_at_ms,
            retryable,
        } => {
            snapshot.status == "timed_out"
                && snapshot.heartbeat_at_ms == Some(*observed_at_ms)
                && snapshot.error_retryable == Some(*retryable)
        }
    }
}

fn task_retry_transition_candidate(
    current: &TaskStateSnapshot,
    observed_at_ms: u128,
) -> Option<TaskStateSnapshot> {
    if !task_retry_is_eligible(current) {
        return None;
    }
    if let Some(ready_at_ms) = current.retry_ready_at_ms {
        return (observed_at_ms >= ready_at_ms)
            .then(|| task_retry_requeue_candidate(current))
            .flatten();
    }
    let retry_backoff_ms = current
        .envelope
        .as_ref()
        .map(|envelope| envelope.attempt_policy.retry_backoff_ms)?;
    if retry_backoff_ms == 0 {
        task_retry_requeue_candidate(current)
    } else {
        let ready_at_ms = observed_at_ms.saturating_add(u128::from(retry_backoff_ms));
        task_retry_schedule_candidate(current, ready_at_ms)
    }
}

fn task_retry_is_eligible(current: &TaskStateSnapshot) -> bool {
    matches!(current.status.as_str(), "failed" | "timed_out")
        && current.attempts < current.retry_max_attempts
        && current.envelope.is_some()
        && !current.cancel_requested
        && !current.cancel_accepted
        && stored_task_error_is_retryable(current)
}

fn stored_task_error_is_retryable(current: &TaskStateSnapshot) -> bool {
    current.error_retryable.unwrap_or({
        matches!(
            current.error_kind.as_deref(),
            Some("timeout" | "unavailable")
        )
    })
}

fn task_retry_schedule_candidate(
    current: &TaskStateSnapshot,
    ready_at_ms: u128,
) -> Option<TaskStateSnapshot> {
    if !task_retry_is_eligible(current) || current.retry_ready_at_ms.is_some() {
        return None;
    }
    let mut candidate = current.clone();
    candidate.retry_ready_at_ms = Some(ready_at_ms);
    candidate.push_log(
        "warning",
        format!(
            "task attempt {} retry scheduled for {ready_at_ms}",
            current.attempts
        ),
    );
    Some(candidate)
}

/// 构造专用 retry 重排候选；失败消息来自已经过 handler 边界脱敏的 durable outcome。
fn task_retry_requeue_candidate(current: &TaskStateSnapshot) -> Option<TaskStateSnapshot> {
    if !task_retry_is_eligible(current) {
        return None;
    }

    let previous_status = current.status.clone();
    let previous_error_kind = current.error_kind.as_deref().unwrap_or("unknown");
    let previous_error_message = current
        .error_message
        .as_deref()
        .unwrap_or("failure details unavailable");
    let mut candidate = current.clone();
    candidate.status = "queued".to_owned();
    candidate.execution_owner = None;
    candidate.cancel_token = None;
    candidate.heartbeat_at_ms = None;
    candidate.deadline_at_ms = None;
    candidate.result_digest = None;
    candidate.result_size_bytes = None;
    candidate.interrupted_reason = None;
    candidate.error_kind = None;
    candidate.error_message = None;
    candidate.error_retryable = None;
    candidate.retry_ready_at_ms = None;
    candidate.push_log(
        "warning",
        format!(
            "task attempt {} queued for retry after {}: kind={}; message={}",
            current.attempts, previous_status, previous_error_kind, previous_error_message
        ),
    );
    Some(candidate)
}

fn task_replay_recovery_candidate(current: &TaskStateSnapshot) -> Option<TaskStateSnapshot> {
    let mut candidate = current.clone();
    candidate
        .mark_abandoned_replay_delivery_queued()
        .ok()
        .map(|_| candidate)
}

fn task_abandoned_recovery_candidate(current: &TaskStateSnapshot) -> Option<TaskStateSnapshot> {
    let mut candidate = current.clone();
    candidate
        .mark_abandoned_task_queued()
        .ok()
        .map(|_| candidate)
}

fn reject_current_generation_recovery(
    current: &TaskStateSnapshot,
    writer_generation: WriterGeneration,
) -> Result<(), EvaError> {
    if current.owner_generation == writer_generation {
        return Err(EvaError::conflict(
            "task restart recovery cannot reclaim the current writer generation",
        )
        .with_context("task_id", &current.task_id)
        .with_context("status", &current.status)
        .with_context("writer_generation", writer_generation.0.to_string()));
    }
    Ok(())
}

fn task_restart_recovery_transition_matches(
    current: &TaskStateSnapshot,
    proposed: &TaskStateSnapshot,
) -> bool {
    if proposed.logs.len() != current.logs.len().saturating_add(1)
        || proposed.logs[..current.logs.len()] != current.logs
    {
        return false;
    }
    let Some(log) = proposed.logs.last() else {
        return false;
    };

    if log.level == "info" {
        let Some(operation_digest) = log
            .message
            .strip_prefix(COMMITTED_EFFECT_RECOVERY_LOG_PREFIX)
        else {
            return false;
        };
        let (Some(result_digest), Some(result_size_bytes)) =
            (&proposed.result_digest, proposed.result_size_bytes)
        else {
            return false;
        };
        let mut expected = current.clone();
        return expected
            .recover_committed_effect(operation_digest, result_digest, result_size_bytes)
            .is_ok()
            && expected == *proposed;
    }

    if log.level == "error" {
        let Some(operation_digest) = log
            .message
            .strip_prefix(UNKNOWN_EFFECT_RECOVERY_REASON_PREFIX)
        else {
            return false;
        };
        let mut expected = current.clone();
        return expected.block_unknown_effect(operation_digest).is_ok() && expected == *proposed;
    }

    false
}

fn task_retry_special_transition_matches(
    current: &TaskStateSnapshot,
    proposed: &TaskStateSnapshot,
) -> bool {
    task_retry_requeue_candidate(current).as_ref() == Some(proposed)
        || proposed.retry_ready_at_ms.is_some_and(|ready_at_ms| {
            task_retry_schedule_candidate(current, ready_at_ms).as_ref() == Some(proposed)
        })
        || task_replay_recovery_candidate(current).as_ref() == Some(proposed)
        || task_abandoned_recovery_candidate(current).as_ref() == Some(proposed)
}

fn validate_task_state_transition(
    current: &TaskStateSnapshot,
    proposed: &TaskStateSnapshot,
    allow_attempt_outcome: bool,
    allow_heartbeat: bool,
    allow_retry_requeue: bool,
    allow_restart_recovery: bool,
) -> Result<(), EvaError> {
    let next_attempt = current.attempts.checked_add(1);
    let claim_deadline_valid = match (
        current
            .envelope
            .as_ref()
            .and_then(|envelope| envelope.attempt_policy.attempt_timeout_ms),
        proposed.heartbeat_at_ms,
        proposed.deadline_at_ms,
    ) {
        (None, Some(_), None) => true,
        (Some(timeout_ms), Some(heartbeat_at_ms), Some(deadline_at_ms)) => {
            heartbeat_at_ms.checked_add(u128::from(timeout_ms)) == Some(deadline_at_ms)
        }
        _ => false,
    };
    let is_claim = current.status == "queued"
        && proposed.status == "running"
        && next_attempt == Some(proposed.attempts)
        && current.envelope.is_some()
        && !current.cancel_requested
        && !proposed.cancel_requested
        && current.dead_letters.is_empty()
        && current.attempts < current.retry_max_attempts
        && current.execution_owner.is_none()
        && current.cancel_token.is_none()
        && proposed.execution_owner.is_some()
        && proposed.cancel_token.is_some()
        && proposed.result_digest.is_none()
        && proposed.result_size_bytes.is_none()
        && proposed.interrupted_reason.is_none()
        && proposed.error_kind.is_none()
        && proposed.error_message.is_none()
        && claim_deadline_valid;
    let is_retry_requeue =
        allow_retry_requeue && task_retry_special_transition_matches(current, proposed);
    let is_restart_recovery =
        allow_restart_recovery && task_restart_recovery_transition_matches(current, proposed);

    if (current.attempts != proposed.attempts
        || current.execution_owner != proposed.execution_owner
        || current.cancel_token != proposed.cancel_token)
        && !is_claim
        && !is_retry_requeue
        && !is_restart_recovery
    {
        return Err(
            EvaError::conflict("task execution fence can change only during queued claim")
                .with_context("task_id", &current.task_id)
                .with_context("current_status", &current.status)
                .with_context("proposed_status", &proposed.status),
        );
    }
    if current.execution_owner.is_some()
        && current.deadline_at_ms != proposed.deadline_at_ms
        && !is_retry_requeue
        && !is_restart_recovery
    {
        return Err(
            EvaError::conflict("task attempt deadline is immutable after claim")
                .with_context("task_id", &current.task_id),
        );
    }
    if current.execution_owner.is_some()
        && matches!(current.status.as_str(), "running" | "cancelling")
        && current.heartbeat_at_ms != proposed.heartbeat_at_ms
        && !allow_heartbeat
        && !is_retry_requeue
        && !is_restart_recovery
    {
        return Err(
            EvaError::conflict("claimed task heartbeat requires the fenced heartbeat API")
                .with_context("task_id", &current.task_id)
                .with_context("status", &current.status),
        );
    }
    if current.execution_owner.is_some()
        && matches!(current.status.as_str(), "running" | "cancelling")
        && matches!(
            proposed.status.as_str(),
            "completed" | "failed" | "timed_out" | "cancelled"
        )
        && !allow_attempt_outcome
        && !is_restart_recovery
    {
        return Err(
            EvaError::conflict("claimed task outcome requires the fenced finish API")
                .with_context("task_id", &current.task_id)
                .with_context("proposed_status", &proposed.status),
        );
    }
    if current.is_terminal()
        && !is_retry_requeue
        && !is_restart_recovery
        && (current.result_digest != proposed.result_digest
            || current.result_size_bytes != proposed.result_size_bytes
            || current.error_kind != proposed.error_kind
            || current.error_message != proposed.error_message
            || current.error_retryable != proposed.error_retryable
            || current.retry_ready_at_ms != proposed.retry_ready_at_ms
            || current.interrupted_reason != proposed.interrupted_reason
            || current.heartbeat_at_ms != proposed.heartbeat_at_ms)
    {
        return Err(
            EvaError::conflict("task terminal outcome metadata is immutable")
                .with_context("task_id", &current.task_id)
                .with_context("status", &current.status),
        );
    }

    let allowed = match current.status.as_str() {
        "queued" => matches!(
            proposed.status.as_str(),
            "queued" | "running" | "cancelling" | "cancelled" | "interrupted" | "recovering"
        ),
        "running" => matches!(
            proposed.status.as_str(),
            "running"
                | "cancelling"
                | "completed"
                | "failed"
                | "cancelled"
                | "timed_out"
                | "interrupted"
                | "recovering"
        ),
        "cancelling" => matches!(
            proposed.status.as_str(),
            "cancelling" | "completed" | "cancelled" | "interrupted" | "recovering"
        ),
        "recovering" => matches!(
            proposed.status.as_str(),
            "recovering" | "cancelling" | "cancelled" | "interrupted"
        ),
        "completed" | "failed" | "cancelled" | "timed_out" | "interrupted" => {
            proposed.status == current.status
        }
        _ => proposed.status == current.status,
    };
    if (!allowed && !is_retry_requeue && !is_restart_recovery)
        || (proposed.status == "running" && !is_claim && current.status != "running")
    {
        return Err(
            EvaError::conflict("task lifecycle transition is not allowed")
                .with_context("task_id", &current.task_id)
                .with_context("current_status", &current.status)
                .with_context("proposed_status", &proposed.status),
        );
    }
    Ok(())
}

fn is_single_task_field(field: &str) -> bool {
    matches!(
        field,
        "format"
            | "record_version"
            | "owner_generation"
            | "task_id"
            | "status"
            | "attempts"
            | "execution_owner"
            | "retry_max_attempts"
            | "envelope_kind"
            | "envelope_agent_id"
            | "envelope_input_kind"
            | "envelope_inline_input_hex"
            | "envelope_artifact_ref"
            | "envelope_input_digest"
            | "envelope_idempotency_key"
            | "envelope_max_attempts"
            | "envelope_retry_backoff_ms"
            | "envelope_attempt_timeout_ms"
            | "replay_event_id"
            | "replay_delivery_index"
            | "error_retryable"
            | "retry_ready_at_ms"
            | "cancel_requested"
            | "cancel_accepted"
            | "cancel_reason"
            | "heartbeat_at_ms"
            | "deadline_at_ms"
            | "cancel_token"
            | "result_digest"
            | "result_size_bytes"
            | "interrupted_reason"
            | "error_kind"
            | "error_message"
    )
}

fn required_stored_field<T>(value: Option<T>, field: &'static str) -> Result<T, EvaError> {
    value.ok_or_else(|| {
        EvaError::invalid_argument("task state is missing an envelope scalar field")
            .with_context("field", field)
    })
}

/// task kind 是未来 handler registry 的语法键；这里只校验稳定点分格式，不判断是否注册。
fn validate_task_kind(value: &str) -> Result<(), EvaError> {
    if value.is_empty() || value.trim() != value || value.len() > MAX_TASK_KIND_BYTES {
        return Err(
            EvaError::invalid_argument("task kind must be a stable non-empty dotted name")
                .with_context("task_kind", value),
        );
    }
    for segment in value.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(
                EvaError::invalid_argument("task kind contains an invalid dotted segment")
                    .with_context("task_kind", value),
            );
        }
    }
    Ok(())
}

fn is_replay_delivery_task_id(task_id: &str) -> bool {
    task_id
        .strip_prefix(REPLAY_DELIVERY_TASK_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == 64
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}

fn validate_execution_owner(value: &str) -> Result<(), EvaError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_TASK_EXECUTION_OWNER_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(EvaError::invalid_argument(
            "task execution owner is not a stable bounded identity",
        ));
    }
    Ok(())
}

fn validate_cancel_token(value: &str) -> Result<(), EvaError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_TASK_CANCEL_TOKEN_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(EvaError::invalid_argument(
            "task cancel token is not a stable bounded fence",
        ));
    }
    Ok(())
}

fn validate_canonical_sha256(value: &str, field: &'static str) -> Result<(), EvaError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(
            EvaError::invalid_argument("task digest must use canonical lowercase SHA-256")
                .with_context("field", field)
                .with_context("digest", value),
        );
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(
            EvaError::invalid_argument("task digest must use canonical lowercase SHA-256")
                .with_context("field", field)
                .with_context("digest", value),
        );
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(value: &str, field: &'static str) -> Result<Vec<u8>, EvaError> {
    if !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(EvaError::invalid_argument(
            "task state binary field is not canonical lowercase hex",
        )
        .with_context("field", field));
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        decoded.push((hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]));
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => unreachable!("decode_hex validates every nibble"),
    }
}

/// 按 `|` 拆分复合磁盘字段，并要求精确 arity。
/// 字段内容中的 `|` 已百分号编码，因此额外分段明确表示损坏格式。
fn split_stored_fields(
    value: &str,
    expected: usize,
    field: &'static str,
) -> Result<Vec<String>, EvaError> {
    let parts = value.split('|').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != expected {
        return Err(
            EvaError::invalid_argument("task state field has invalid arity")
                .with_context("field", field)
                .with_context("expected", expected.to_string())
                .with_context("actual", parts.len().to_string()),
        );
    }
    Ok(parts)
}

/// 严格解析 usize 计数，并在错误中保留字段名和原值。
fn parse_stored_usize(name: &'static str, value: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn parse_optional_stored_usize(name: &'static str, value: &str) -> Result<Option<usize>, EvaError> {
    if value.is_empty() {
        return Ok(None);
    }
    parse_stored_usize(name, value).map(Some)
}

fn parse_stored_u32(name: &'static str, value: &str) -> Result<u32, EvaError> {
    value.parse::<u32>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned 32-bit integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

/// 严格解析日志/replay 序号。
fn parse_stored_u64(name: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn parse_optional_stored_u64(name: &'static str, value: &str) -> Result<Option<u64>, EvaError> {
    if value.is_empty() {
        return Ok(None);
    }
    parse_stored_u64(name, value).map(Some)
}

/// 将空串解析为 None，否则严格解析 epoch 毫秒 u128。
fn parse_optional_stored_u128(name: &'static str, value: &str) -> Result<Option<u128>, EvaError> {
    if value.is_empty() {
        return Ok(None);
    }
    value.parse::<u128>().map(Some).map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn parse_optional_stored_bool(name: &'static str, value: &str) -> Result<Option<bool>, EvaError> {
    match value {
        "" => Ok(None),
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        _ => Err(
            EvaError::invalid_argument("task state contains an invalid optional boolean")
                .with_context("field", name)
                .with_context("value", value),
        ),
    }
}

/// 将磁盘空串恢复为 None，非空值百分号解码。
fn decode_optional_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(decode_field(value))
    }
}

/// 百分号编码换行、制表、`|`、`=` 和 `%`，保护逐行复合字段格式。
fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
        .replace('|', "%7C")
        .replace('=', "%3D")
}

/// 以固定逆序恢复特殊字符，最后解码 `%25` 防止二次展开。
fn decode_field(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%09", "\t")
        .replace("%7C", "|")
        .replace("%3D", "=")
        .replace("%25", "%")
}

#[cfg(test)]
/// 任务快照重开、取消、durable 布局、列表、生命周期和损坏文件回归测试。
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    /// 验证按 ID 与 latest 跨 store 重建均得到相同快照。
    fn filesystem_task_state_survives_store_recreation() {
        let root = test_root("round-trip");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        let snapshot = sample_snapshot("req-task-state-1");

        let committed = writer.create(&snapshot).unwrap();
        let reader = FileSystemTaskStateStore::new(root.path());
        let by_id = reader.read(Some("req-task-state-1")).unwrap();
        let latest = reader.read(None).unwrap();

        assert_eq!(by_id, committed);
        assert_eq!(latest, committed);
        assert_eq!(committed.record_version, StateVersion(1));
        assert_eq!(reader.project_root(), root.path());
    }

    #[test]
    /// 新 v5 任务在 writer 释放并只读重开后仍完整恢复二进制 inline payload 与执行策略。
    fn task_envelope_reopens_with_exact_inline_payload() {
        let root = test_root("envelope-inline-round-trip");
        let input = vec![0, b'\n', b'%', b'|', b'=', 0xff];
        let expected_digest = sha256_digest(&input);
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            input.clone(),
            "idem-envelope-inline",
            TaskAttemptPolicySnapshot::new(3, 250, Some(5_000)).unwrap(),
        )
        .unwrap();
        let committed = {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        "req-task-envelope-inline",
                        envelope.clone(),
                    )
                    .unwrap(),
                )
                .unwrap()
        };

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_only(root.path()),
        )
        .unwrap();
        let reopened = FileSystemTaskStateStore::from_durable_layout(backend.layout())
            .read(Some("req-task-envelope-inline"))
            .unwrap();
        let leaked_bytes = format!("{input:?}");
        let snapshot_debug = format!("{reopened:?}");

        assert_eq!(reopened, committed);
        assert_eq!(reopened.envelope.as_ref(), Some(&envelope));
        assert!(!snapshot_debug.contains(&leaked_bytes));
        assert!(snapshot_debug.contains("bytes: \"<redacted>\""));
        assert!(snapshot_debug.contains("size_bytes: 6"));
        assert!(snapshot_debug.contains(&expected_digest));
        assert_eq!(
            reopened.envelope.unwrap().input,
            TaskInputSnapshot::Inline {
                bytes: input,
                digest: expected_digest,
            }
        );
    }

    #[test]
    /// 合法但尚未注册的 kind 与 artifact ref/digest 也必须跨 durable reopen 原样恢复。
    fn task_envelope_reopens_artifact_ref_with_unknown_kind() {
        let root = test_root("envelope-artifact-round-trip");
        let envelope = TaskEnvelopeSnapshot::artifact(
            "vendor.future-handler",
            "root-agent",
            "tasks/input-1",
            "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
            "idem-envelope-artifact",
            TaskAttemptPolicySnapshot::new(2, 500, None).unwrap(),
        )
        .unwrap();
        {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        "req-task-envelope-artifact",
                        envelope.clone(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_only(root.path()),
        )
        .unwrap();
        let reopened = FileSystemTaskStateStore::from_durable_layout(backend.layout())
            .read(Some("req-task-envelope-artifact"))
            .unwrap();

        assert_eq!(reopened.envelope, Some(envelope));
    }

    #[test]
    /// kind 只做语法校验；合法未知 kind 可保存，而非法 kind/digest 在落盘前失败。
    fn task_envelope_accepts_unknown_kind_and_rejects_invalid_kind_or_digest() {
        let policy = TaskAttemptPolicySnapshot::new(1, 0, None).unwrap();
        let unknown = TaskEnvelopeSnapshot::artifact(
            "vendor.future-handler",
            "root-agent",
            "tasks/input-1",
            "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
            "idem-future-handler",
            policy.clone(),
        )
        .unwrap();
        assert_eq!(unknown.kind, "vendor.future-handler");

        for invalid_kind in ["", " runtime.echo", "runtime..echo", "runtime/echo"] {
            let error = TaskEnvelopeSnapshot::inline(
                invalid_kind,
                "root-agent",
                b"payload".to_vec(),
                "idem-invalid-kind",
                policy.clone(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        }

        for invalid_digest in [
            "sha256:bad",
            "SHA256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
            "sha256:2689367B205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
            "md5:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
        ] {
            let error = TaskEnvelopeSnapshot::artifact(
                "runtime.echo",
                "root-agent",
                "tasks/input-1",
                invalid_digest,
                "idem-invalid-digest",
                policy.clone(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        }

        for invalid_ref in ["", "/absolute", "tasks/../input", "tasks\\input"] {
            let error = TaskEnvelopeSnapshot::artifact(
                "runtime.echo",
                "root-agent",
                invalid_ref,
                "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
                "idem-invalid-ref",
                policy.clone(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        }
    }

    #[test]
    /// v5 磁盘记录缺字段、摘要篡改、重复标量、未知 discriminator 或 policy 漂移均失败。
    fn task_envelope_v5_rejects_corrupt_persisted_fields() {
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"persisted-payload".to_vec(),
            "idem-corrupt-v3",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        let digest = envelope.input.digest().to_owned();
        let stored = TaskStateSnapshot::queued_with_envelope("req-task-corrupt-v3", envelope)
            .unwrap()
            .to_storage();
        let cases = [
            stored.replace("envelope_agent_id=root-agent\n", ""),
            stored.replace(
                &format!("envelope_input_digest={digest}"),
                "envelope_input_digest=sha256:0000000000000000000000000000000000000000000000000000000000000000",
            ),
            stored.replacen(
                "envelope_kind=runtime.echo\n",
                "envelope_kind=runtime.echo\nenvelope_kind=runtime.echo\n",
                1,
            ),
            stored.replace("envelope_input_kind=inline", "envelope_input_kind=unknown"),
            stored.replace("retry_max_attempts=1", "retry_max_attempts=2"),
            stored.replace("execution_owner=\n", ""),
            stored.replacen(
                "result_digest=\n",
                "result_digest=\nresult_digest=\n",
                1,
            ),
            stored.replace("result_size_bytes=\n", "result_size_bytes=1\n"),
            stored.replace(TASK_STATE_FORMAT_V5, TASK_STATE_FORMAT_V4),
        ];

        for corrupted in cases {
            let error = TaskStateSnapshot::from_storage(&corrupted).unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        }
    }

    #[test]
    /// claim/finish 持久绑定 owner、attempt、cancel token 和结果摘要，Debug 不泄露 fencing token。
    fn task_execution_claim_and_finish_round_trip_v5() {
        let root = test_root("execution-claim-finish");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"payload".to_vec(),
            "idem-execution-claim",
            TaskAttemptPolicySnapshot::new(2, 0, Some(500)).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-task-execution-claim",
                    envelope.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let owner = "daemon.100.7.0123456789abcdef";
        let token = "cancel.0123456789abcdef";

        let mut forged_claim = store.read(Some("req-task-execution-claim")).unwrap();
        forged_claim.attempts = 1;
        forged_claim.execution_owner = Some(owner.to_owned());
        forged_claim.mark_running(1_000, Some(9_999), token);
        let forged_claim_error = store.compare_and_set(&forged_claim).unwrap_err();
        assert_eq!(forged_claim_error.kind(), eva_core::ErrorKind::Conflict);

        let claim = store
            .try_claim_queued("req-task-execution-claim", owner, token, 1_000)
            .unwrap()
            .unwrap();

        assert_eq!(claim.snapshot().status, "running");
        assert_eq!(claim.snapshot().attempts, 1);
        assert_eq!(claim.snapshot().execution_owner.as_deref(), Some(owner));
        assert_eq!(claim.snapshot().cancel_token.as_deref(), Some(token));
        assert_eq!(claim.snapshot().heartbeat_at_ms, Some(1_000));
        assert_eq!(claim.snapshot().deadline_at_ms, Some(1_500));
        assert_eq!(claim.snapshot().envelope.as_ref(), Some(&envelope));
        assert_eq!(claim.snapshot().record_version, StateVersion(2));
        let claim_debug = format!("{claim:?}");
        assert!(!claim_debug.contains(owner));
        assert!(!claim_debug.contains(token));

        let mut replaced_owner = claim.snapshot().clone();
        replaced_owner.execution_owner = Some("daemon.100.7.other".to_owned());
        let replace_error = store.compare_and_set(&replaced_owner).unwrap_err();
        assert_eq!(replace_error.kind(), eva_core::ErrorKind::Conflict);

        let mut forged_completion = claim.snapshot().clone();
        forged_completion
            .complete_execution(
                claim.fence().execution_owner(),
                claim.fence().attempt(),
                claim.fence().cancel_token(),
                sha256_digest(b"forged"),
                6,
            )
            .unwrap();
        let forged_completion_error = store.compare_and_set(&forged_completion).unwrap_err();
        assert_eq!(
            forged_completion_error.kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(
            forged_completion_error.message(),
            "claimed task outcome requires the fenced finish API"
        );

        let result = b"result";
        let result_digest = sha256_digest(result);
        let completed = store
            .finish_execution(
                claim.fence(),
                &TaskAttemptOutcome::Completed {
                    result_digest: result_digest.clone(),
                    result_size_bytes: result.len(),
                },
            )
            .unwrap();

        assert_eq!(completed.status, "completed");
        assert_eq!(
            completed.result_digest.as_deref(),
            Some(result_digest.as_str())
        );
        assert_eq!(completed.result_size_bytes, Some(result.len()));
        assert_eq!(completed.record_version, StateVersion(3));
        assert!(
            fs::read_to_string(store.task_path("req-task-execution-claim").unwrap())
                .unwrap()
                .starts_with("format=eva.task-state.v5\n")
        );
    }

    #[test]
    fn shutdown_cancellation_and_prepared_block_require_the_exact_attempt_and_operation() {
        let root = test_root("execution-shutdown-prepared-block");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let task_id = "req-task-shutdown-prepared";
        let envelope = TaskEnvelopeSnapshot::inline(
            "vendor.shutdown-effect",
            "root-agent",
            b"payload".to_vec(),
            "idem-task-shutdown-prepared",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(&TaskStateSnapshot::queued_with_envelope(task_id, envelope).unwrap())
            .unwrap();
        let claim = store
            .try_claim_queued(
                task_id,
                "daemon.shutdown.owner",
                "cancel.shutdown.token",
                100,
            )
            .unwrap()
            .unwrap();

        let cancelling = store
            .request_execution_cancellation(claim.fence(), "daemon drain elapsed")
            .unwrap();
        assert_eq!(cancelling.status, "cancelling");
        let repeated_cancel = store
            .request_execution_cancellation(claim.fence(), "daemon drain elapsed")
            .unwrap();
        assert_eq!(repeated_cancel.record_version, cancelling.record_version);

        let operation_digest = sha256_digest(b"shutdown-operation-one");
        let conflicting_digest = sha256_digest(b"shutdown-operation-two");
        let blocked = store
            .block_prepared_effect_execution(claim.fence(), &operation_digest)
            .unwrap();
        assert_eq!(blocked.status, "interrupted");
        assert!(blocked.requires_operator_reconciliation());
        let repeated = store
            .block_prepared_effect_execution(claim.fence(), &operation_digest)
            .unwrap();
        assert_eq!(repeated.record_version, blocked.record_version);

        let error = store
            .block_prepared_effect_execution(claim.fence(), &conflicting_digest)
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        let unchanged = store.read(Some(task_id)).unwrap();
        assert_eq!(unchanged.record_version, blocked.record_version);
        assert_eq!(unchanged.interrupted_reason, blocked.interrupted_reason);
    }

    #[test]
    fn task_heartbeat_is_monotonic_fenced_and_drives_freshness() {
        let root = test_root("execution-heartbeat-freshness");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"heartbeat".to_vec(),
            "idem-execution-heartbeat",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope("req-task-execution-heartbeat", envelope)
                    .unwrap(),
            )
            .unwrap();
        let claim = store
            .try_claim_queued(
                "req-task-execution-heartbeat",
                "daemon.heartbeat.worker",
                "cancel.heartbeat",
                1_000,
            )
            .unwrap()
            .unwrap();
        let fence = claim.fence().clone();

        let renewed = store.heartbeat_execution(&fence, 2_000).unwrap();
        assert_eq!(renewed.heartbeat_at_ms, Some(2_000));
        assert_eq!(renewed.record_version, StateVersion(3));
        assert_eq!(
            renewed.freshness_at(6_999, 5_000, 15_000),
            TaskFreshness::Live
        );
        assert_eq!(
            renewed.freshness_at(7_000, 5_000, 15_000),
            TaskFreshness::Degraded
        );
        assert_eq!(
            renewed.freshness_at(16_999, 5_000, 15_000),
            TaskFreshness::Degraded
        );
        assert_eq!(
            renewed.freshness_at(17_000, 5_000, 15_000),
            TaskFreshness::Stale
        );
        let mut ownerless = TaskStateSnapshot::queued("req-ownerless-heartbeat").unwrap();
        ownerless.mark_running(2_000, None, "legacy-cancel-token");
        assert_eq!(
            ownerless.freshness_at(2_000, 5_000, 15_000),
            TaskFreshness::Stale
        );

        let unchanged = store.heartbeat_execution(&fence, 1_500).unwrap();
        assert_eq!(unchanged.heartbeat_at_ms, Some(2_000));
        assert_eq!(unchanged.record_version, StateVersion(3));

        let mut unfenced = renewed;
        unfenced.heartbeat_at_ms = Some(3_000);
        let unfenced_error = store.compare_and_set(&unfenced).unwrap_err();
        assert_eq!(unfenced_error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            unfenced_error.message(),
            "claimed task heartbeat requires the fenced heartbeat API"
        );

        let completed = store
            .finish_execution(
                &fence,
                &TaskAttemptOutcome::Completed {
                    result_digest: sha256_digest(b"done"),
                    result_size_bytes: 4,
                },
            )
            .unwrap();
        let late_error = store.heartbeat_execution(&fence, 4_000).unwrap_err();
        assert_eq!(late_error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            store.read(Some("req-task-execution-heartbeat")).unwrap(),
            completed
        );
    }

    #[test]
    fn task_heartbeat_linearizes_with_cancel_and_finish() {
        let root = test_root("execution-heartbeat-races");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"heartbeat-race".to_vec(),
            "idem-execution-heartbeat-race",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-task-execution-heartbeat-race",
                    envelope,
                )
                .unwrap(),
            )
            .unwrap();
        let claim = store
            .try_claim_queued(
                "req-task-execution-heartbeat-race",
                "daemon.heartbeat.race",
                "cancel.heartbeat.race",
                1_000,
            )
            .unwrap()
            .unwrap();
        let fence = claim.fence().clone();

        let start = Arc::new(Barrier::new(3));
        let heartbeat_start = Arc::clone(&start);
        let mut heartbeat_store = store.clone();
        let heartbeat_fence = fence.clone();
        let heartbeat = thread::spawn(move || {
            heartbeat_start.wait();
            heartbeat_store.heartbeat_execution(&heartbeat_fence, 2_000)
        });
        let cancel_start = Arc::clone(&start);
        let mut cancel_store = store.clone();
        let cancellation = thread::spawn(move || {
            cancel_start.wait();
            cancel_store.request_cancellation("req-task-execution-heartbeat-race", "operator stop")
        });
        start.wait();
        heartbeat.join().unwrap().unwrap();
        cancellation.join().unwrap().unwrap();

        let cancelling = store
            .read(Some("req-task-execution-heartbeat-race"))
            .unwrap();
        assert_eq!(cancelling.status, "cancelling");
        assert!(cancelling.cancel_requested);
        assert!(cancelling.cancel_accepted);
        assert_eq!(cancelling.heartbeat_at_ms, Some(2_000));

        let finish_start = Arc::new(Barrier::new(3));
        let late_heartbeat_start = Arc::clone(&finish_start);
        let mut late_heartbeat_store = store.clone();
        let late_heartbeat_fence = fence.clone();
        let late_heartbeat = thread::spawn(move || {
            late_heartbeat_start.wait();
            late_heartbeat_store.heartbeat_execution(&late_heartbeat_fence, 3_000)
        });
        let outcome_start = Arc::clone(&finish_start);
        let mut outcome_store = store.clone();
        let outcome_fence = fence.clone();
        let finish = thread::spawn(move || {
            outcome_start.wait();
            outcome_store.finish_execution(
                &outcome_fence,
                &TaskAttemptOutcome::Completed {
                    result_digest: sha256_digest(b"late-success"),
                    result_size_bytes: 12,
                },
            )
        });
        finish_start.wait();
        let heartbeat_result = late_heartbeat.join().unwrap();
        let finished = finish.join().unwrap().unwrap();
        if let Err(error) = heartbeat_result {
            assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        }

        assert_eq!(finished.status, "cancelled");
        assert!(finished.cancel_requested);
        assert!(finished.cancel_accepted);
        assert!(finished.result_digest.is_none());
        let persisted = store
            .read(Some("req-task-execution-heartbeat-race"))
            .unwrap();
        assert_eq!(persisted.status, "cancelled");
        assert!(
            persisted.heartbeat_at_ms == Some(2_000) || persisted.heartbeat_at_ms == Some(3_000)
        );
        let stale_fence_error = store.heartbeat_execution(&fence, 4_000).unwrap_err();
        assert_eq!(stale_fence_error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// queued cancel 阻止 claim；running cancel 获胜后迟到 handler 结果只能收口为 cancelled。
    fn task_cancellation_is_linearized_against_claim_and_finish() {
        let root = test_root("execution-cancel-races");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let policy = TaskAttemptPolicySnapshot::new(1, 0, None).unwrap();
        for task_id in ["req-cancel-before-claim", "req-cancel-after-claim"] {
            let envelope = TaskEnvelopeSnapshot::inline(
                "runtime.echo",
                "root-agent",
                b"payload".to_vec(),
                format!("idem-{task_id}"),
                policy.clone(),
            )
            .unwrap();
            store
                .create(&TaskStateSnapshot::queued_with_envelope(task_id, envelope).unwrap())
                .unwrap();
        }

        let cancelled = store
            .request_cancellation("req-cancel-before-claim", "operator stop")
            .unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert!(store
            .try_claim_queued(
                "req-cancel-before-claim",
                "daemon.1.1.worker",
                "cancel.before",
                100,
            )
            .unwrap()
            .is_none());

        let claim = store
            .try_claim_queued(
                "req-cancel-after-claim",
                "daemon.1.1.worker",
                "cancel.after",
                100,
            )
            .unwrap()
            .unwrap();
        let cancelling = store
            .request_cancellation("req-cancel-after-claim", "operator stop")
            .unwrap();
        assert_eq!(cancelling.status, "cancelling");
        let final_state = store
            .finish_execution(
                claim.fence(),
                &TaskAttemptOutcome::Completed {
                    result_digest: sha256_digest(b"late-result"),
                    result_size_bytes: 11,
                },
            )
            .unwrap();
        assert_eq!(final_state.status, "cancelled");
        assert!(final_state.result_digest.is_none());
    }

    #[test]
    fn terminal_outcome_metadata_cannot_be_rewritten_by_plain_cas() {
        let root = test_root("terminal-outcome-immutable");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let policy = TaskAttemptPolicySnapshot::new(1, 0, None).unwrap();
        for task_id in [
            "req-terminal-completed",
            "req-terminal-failed",
            "req-terminal-timed-out",
            "req-terminal-cancelled",
        ] {
            let envelope = TaskEnvelopeSnapshot::inline(
                "runtime.echo",
                "root-agent",
                task_id.as_bytes().to_vec(),
                format!("idem-{task_id}"),
                policy.clone(),
            )
            .unwrap();
            store
                .create(&TaskStateSnapshot::queued_with_envelope(task_id, envelope).unwrap())
                .unwrap();
        }

        let completed_claim = store
            .try_claim_queued(
                "req-terminal-completed",
                "daemon.terminal.worker",
                "cancel.terminal.completed",
                100,
            )
            .unwrap()
            .unwrap();
        let completed = store
            .finish_execution(
                completed_claim.fence(),
                &TaskAttemptOutcome::Completed {
                    result_digest: sha256_digest(b"result"),
                    result_size_bytes: 6,
                },
            )
            .unwrap();

        let failed_claim = store
            .try_claim_queued(
                "req-terminal-failed",
                "daemon.terminal.worker",
                "cancel.terminal.failed",
                100,
            )
            .unwrap()
            .unwrap();
        let failed = store
            .finish_execution(
                failed_claim.fence(),
                &TaskAttemptOutcome::Failed {
                    error_kind: "unavailable".to_owned(),
                    error_message: "handler unavailable".to_owned(),
                    retryable: true,
                },
            )
            .unwrap();

        let timed_out_claim = store
            .try_claim_queued(
                "req-terminal-timed-out",
                "daemon.terminal.worker",
                "cancel.terminal.timed-out",
                100,
            )
            .unwrap()
            .unwrap();
        let timed_out = store
            .finish_execution(
                timed_out_claim.fence(),
                &TaskAttemptOutcome::TimedOut {
                    observed_at_ms: 200,
                    retryable: true,
                },
            )
            .unwrap();
        let mismatched_timeout = store
            .finish_execution(
                timed_out_claim.fence(),
                &TaskAttemptOutcome::TimedOut {
                    observed_at_ms: 201,
                    retryable: true,
                },
            )
            .unwrap_err();
        assert_eq!(mismatched_timeout.kind(), eva_core::ErrorKind::Conflict);

        let cancelled_claim = store
            .try_claim_queued(
                "req-terminal-cancelled",
                "daemon.terminal.worker",
                "cancel.terminal.cancelled",
                100,
            )
            .unwrap()
            .unwrap();
        store
            .request_cancellation("req-terminal-cancelled", "operator stop")
            .unwrap();
        let cancelled = store
            .finish_execution(
                cancelled_claim.fence(),
                &TaskAttemptOutcome::Completed {
                    result_digest: sha256_digest(b"late"),
                    result_size_bytes: 4,
                },
            )
            .unwrap();

        let mut tampered_completed = completed;
        tampered_completed.result_digest = Some(sha256_digest(b"forged"));
        let mut tampered_failed = failed;
        tampered_failed.error_message = Some("forged failure".to_owned());
        let mut tampered_timed_out = timed_out;
        tampered_timed_out.heartbeat_at_ms = Some(201);
        let mut tampered_cancelled = cancelled;
        tampered_cancelled.heartbeat_at_ms = Some(101);

        for tampered in [
            tampered_completed,
            tampered_failed,
            tampered_timed_out,
            tampered_cancelled,
        ] {
            let error = store.compare_and_set(&tampered).unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
            assert_eq!(
                error.message(),
                "task terminal outcome metadata is immutable"
            );
        }
    }

    #[test]
    /// retry 重排只接受仍有配额的 failed/timed_out；普通 CAS 和其他状态均不能回退。
    fn task_retry_requeue_enforces_state_gates_and_preserves_failure_history() {
        let root = test_root("retry-requeue-gates");
        let mut store = FileSystemTaskStateStore::new(root.path());
        for (task_id, max_attempts) in [
            ("req-requeue-queued", 2),
            ("req-requeue-running", 2),
            ("req-requeue-completed", 2),
            ("req-requeue-cancelled", 2),
            ("req-requeue-exhausted", 1),
            ("req-requeue-cancel-requested", 2),
            ("req-requeue-failed", 2),
            ("req-requeue-timed-out", 2),
        ] {
            create_retry_test_task(&mut store, task_id, max_attempts, b"private-payload");
        }

        let running = store
            .try_claim_queued(
                "req-requeue-running",
                "daemon.requeue.running",
                "cancel.requeue.running",
                100,
            )
            .unwrap()
            .unwrap()
            .snapshot()
            .clone();
        let completed = finish_retry_test_task(
            &mut store,
            "req-requeue-completed",
            &TaskAttemptOutcome::Completed {
                result_digest: sha256_digest(b"done"),
                result_size_bytes: 4,
            },
        );
        let cancelled = store
            .request_cancellation("req-requeue-cancelled", "operator stop")
            .unwrap();
        let exhausted = finish_retry_test_task(
            &mut store,
            "req-requeue-exhausted",
            &TaskAttemptOutcome::Failed {
                error_kind: "unavailable".to_owned(),
                error_message: "retry budget exhausted".to_owned(),
                retryable: true,
            },
        );
        finish_retry_test_task(
            &mut store,
            "req-requeue-cancel-requested",
            &TaskAttemptOutcome::Failed {
                error_kind: "unavailable".to_owned(),
                error_message: "handler unavailable".to_owned(),
                retryable: true,
            },
        );
        let cancel_requested = store
            .request_cancellation("req-requeue-cancel-requested", "operator stop")
            .unwrap();
        let failed = finish_retry_test_task(
            &mut store,
            "req-requeue-failed",
            &TaskAttemptOutcome::Failed {
                error_kind: "unavailable".to_owned(),
                error_message: "handler unavailable".to_owned(),
                retryable: true,
            },
        );
        let timed_out = finish_retry_test_task(
            &mut store,
            "req-requeue-timed-out",
            &TaskAttemptOutcome::TimedOut {
                observed_at_ms: 250,
                retryable: true,
            },
        );

        let queued = store.read(Some("req-requeue-queued")).unwrap();
        for (task_id, expected) in [
            ("req-requeue-queued", queued),
            ("req-requeue-running", running),
            ("req-requeue-completed", completed),
            ("req-requeue-cancelled", cancelled),
            ("req-requeue-exhausted", exhausted),
            ("req-requeue-cancel-requested", cancel_requested),
        ] {
            assert_eq!(store.requeue_retryable(task_id, 0).unwrap(), expected);
            assert_eq!(store.read(Some(task_id)).unwrap(), expected);
        }

        let forged_requeue = task_retry_requeue_candidate(&failed).unwrap();
        let plain_cas_error = store.compare_and_set(&forged_requeue).unwrap_err();
        assert_eq!(plain_cas_error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(store.read(Some("req-requeue-failed")).unwrap(), failed);

        let failed_envelope = failed.envelope.clone();
        let requeued_failed = store.requeue_retryable("req-requeue-failed", 0).unwrap();
        assert_eq!(requeued_failed.status, "queued");
        assert_eq!(requeued_failed.attempts, failed.attempts);
        assert_eq!(requeued_failed.envelope, failed_envelope);
        assert!(requeued_failed.execution_owner.is_none());
        assert!(requeued_failed.cancel_token.is_none());
        assert!(requeued_failed.heartbeat_at_ms.is_none());
        assert!(requeued_failed.deadline_at_ms.is_none());
        assert!(requeued_failed.result_digest.is_none());
        assert!(requeued_failed.result_size_bytes.is_none());
        assert!(requeued_failed.interrupted_reason.is_none());
        assert!(requeued_failed.error_kind.is_none());
        assert!(requeued_failed.error_message.is_none());
        let history = &requeued_failed.logs.last().unwrap().message;
        assert!(history.contains("kind=unavailable"));
        assert!(history.contains("message=handler unavailable"));
        assert!(!history.contains("private-payload"));
        assert_eq!(
            store.requeue_retryable("req-requeue-failed", 0).unwrap(),
            requeued_failed
        );

        let timed_out_envelope = timed_out.envelope.clone();
        let requeued_timed_out = store.requeue_retryable("req-requeue-timed-out", 0).unwrap();
        assert_eq!(requeued_timed_out.status, "queued");
        assert_eq!(requeued_timed_out.attempts, timed_out.attempts);
        assert_eq!(requeued_timed_out.envelope, timed_out_envelope);
        assert!(requeued_timed_out.heartbeat_at_ms.is_none());
        assert!(requeued_timed_out.error_kind.is_none());
        assert!(requeued_timed_out.error_message.is_none());
        assert!(requeued_timed_out
            .logs
            .last()
            .unwrap()
            .message
            .contains("queued for retry after timed_out: kind=timeout"));
    }

    #[test]
    /// 两个 store 同时重排同一失败 attempt 时只有一个版本和一条历史日志被提交。
    fn task_retry_requeue_is_idempotent_across_competing_store_instances() {
        let root = test_root("retry-requeue-race");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        create_retry_test_task(&mut store, "req-requeue-race", 3, b"race-payload");
        let failed = finish_retry_test_task(
            &mut store,
            "req-requeue-race",
            &TaskAttemptOutcome::Failed {
                error_kind: "unavailable".to_owned(),
                error_message: "transient handler failure".to_owned(),
                retryable: true,
            },
        );
        let writer_generation = store.writer.as_ref().unwrap().generation();
        let mut first = store.clone();
        let mut second = store.clone();
        let start = Arc::new(Barrier::new(3));
        let first_start = Arc::clone(&start);
        let first_result = thread::spawn(move || {
            first_start.wait();
            first.requeue_retryable("req-requeue-race", 0)
        });
        let second_start = Arc::clone(&start);
        let second_result = thread::spawn(move || {
            second_start.wait();
            second.requeue_retryable("req-requeue-race", 0)
        });
        start.wait();

        let first = first_result.join().unwrap().unwrap();
        let second = second_result.join().unwrap().unwrap();
        let authoritative = store.read(Some("req-requeue-race")).unwrap();
        assert_eq!(first, authoritative);
        assert_eq!(second, authoritative);
        assert_eq!(authoritative.status, "queued");
        assert_eq!(authoritative.attempts, failed.attempts);
        assert_eq!(
            authoritative.record_version,
            StateVersion(failed.record_version.0 + 1)
        );
        assert_eq!(authoritative.owner_generation, writer_generation);
        assert_eq!(
            authoritative
                .logs
                .iter()
                .filter(|entry| entry.message.contains("queued for retry after failed"))
                .count(),
            1
        );
    }

    #[test]
    fn explicit_non_retryable_handler_failure_survives_reopen_and_never_requeues() {
        let root = test_root("retryable-override");
        {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let envelope = TaskEnvelopeSnapshot::inline(
                "runtime.echo",
                "root-agent",
                b"non-retryable".to_vec(),
                "idem-non-retryable",
                TaskAttemptPolicySnapshot::new(3, 500, None).unwrap(),
            )
            .unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope("req-non-retryable", envelope)
                        .unwrap(),
                )
                .unwrap();
            let failed = finish_retry_test_task(
                &mut store,
                "req-non-retryable",
                &TaskAttemptOutcome::Failed {
                    error_kind: "unavailable".to_owned(),
                    error_message: "provider permanently disabled".to_owned(),
                    retryable: false,
                },
            );
            assert_eq!(failed.error_retryable, Some(false));
        }

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let reopened = store.read(Some("req-non-retryable")).unwrap();
        assert_eq!(reopened.status, "failed");
        assert_eq!(reopened.error_retryable, Some(false));
        assert_eq!(
            store
                .requeue_retryable("req-non-retryable", 10_000)
                .unwrap(),
            reopened
        );
        assert!(store
            .read(Some("req-non-retryable"))
            .unwrap()
            .retry_ready_at_ms
            .is_none());
    }

    #[test]
    /// 失败记录释放 writer 后可由新 generation 重排，并在重开 store 中领取下一 attempt。
    fn task_retry_requeue_survives_reopen_and_can_be_claimed_again() {
        let root = test_root("retry-requeue-reopen");
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"reopen-payload".to_vec(),
            "idem-requeue-reopen",
            TaskAttemptPolicySnapshot::new(3, 10, Some(500)).unwrap(),
        )
        .unwrap();
        let first_generation = {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        "req-requeue-reopen",
                        envelope.clone(),
                    )
                    .unwrap(),
                )
                .unwrap();
            let failed = finish_retry_test_task(
                &mut store,
                "req-requeue-reopen",
                &TaskAttemptOutcome::Failed {
                    error_kind: "unavailable".to_owned(),
                    error_message: "transient handler failure".to_owned(),
                    retryable: true,
                },
            );
            failed.owner_generation
        };

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut reopened = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let second_generation = reopened.writer.as_ref().unwrap().generation();
        let scheduled = reopened
            .requeue_retryable("req-requeue-reopen", 100)
            .unwrap();
        assert_eq!(scheduled.status, "failed");
        assert_eq!(scheduled.retry_ready_at_ms, Some(110));
        assert_eq!(
            reopened
                .requeue_retryable("req-requeue-reopen", 109)
                .unwrap(),
            scheduled
        );
        let requeued = reopened
            .requeue_retryable("req-requeue-reopen", 110)
            .unwrap();
        assert_eq!(requeued.status, "queued");
        assert_eq!(requeued.attempts, 1);
        assert_eq!(requeued.envelope.as_ref(), Some(&envelope));
        assert_eq!(requeued.owner_generation, second_generation);
        assert_ne!(second_generation, first_generation);

        let claim = reopened
            .try_claim_queued(
                "req-requeue-reopen",
                "daemon.requeue.reopened",
                "cancel.requeue.reopened",
                1_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(claim.snapshot().status, "running");
        assert_eq!(claim.snapshot().attempts, 2);
        assert_eq!(claim.snapshot().envelope.as_ref(), Some(&envelope));
        assert_eq!(claim.fence().owner_generation(), second_generation);
    }

    #[test]
    fn v4_task_state_without_replay_retry_fields_remains_readable() {
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"legacy-v4".to_vec(),
            "idem-v4-compatible",
            TaskAttemptPolicySnapshot::new(2, 100, None).unwrap(),
        )
        .unwrap();
        let v4 =
            TaskStateSnapshot::queued_with_envelope("req-task-v4-compatible", envelope.clone())
                .unwrap()
                .to_storage()
                .replace(TASK_STATE_FORMAT_V5, TASK_STATE_FORMAT_V4)
                .lines()
                .filter(|line| {
                    !line.starts_with("replay_event_id=")
                        && !line.starts_with("replay_delivery_index=")
                        && !line.starts_with("error_retryable=")
                        && !line.starts_with("retry_ready_at_ms=")
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";

        let parsed = TaskStateSnapshot::from_storage(&v4).unwrap();
        assert_eq!(parsed.envelope, Some(envelope));
        assert!(parsed.replay_delivery.is_none());
        assert!(parsed.error_retryable.is_none());
        assert!(parsed.retry_ready_at_ms.is_none());
        assert!(parsed
            .to_storage()
            .starts_with("format=eva.task-state.v5\n"));
    }

    #[test]
    /// 旧 v3 queued 记录仍可读取，并在首次成功 claim 时惰性升级为 v5。
    fn task_envelope_v3_is_lazily_upgraded_on_claim() {
        let root = test_root("v3-lazy-claim");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"legacy-v3".to_vec(),
            "idem-v3-lazy-claim",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        let committed = store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-task-v3-lazy-claim",
                    envelope.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let v3 = committed
            .to_storage()
            .replace(TASK_STATE_FORMAT_V5, TASK_STATE_FORMAT_V3)
            .lines()
            .filter(|line| {
                !line.starts_with("execution_owner=")
                    && !line.starts_with("result_digest=")
                    && !line.starts_with("result_size_bytes=")
                    && !line.starts_with("replay_event_id=")
                    && !line.starts_with("replay_delivery_index=")
                    && !line.starts_with("error_retryable=")
                    && !line.starts_with("retry_ready_at_ms=")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(store.task_path("req-task-v3-lazy-claim").unwrap(), &v3).unwrap();
        fs::write(store.latest_task_path(), v3).unwrap();

        let reopened = store.read(Some("req-task-v3-lazy-claim")).unwrap();
        assert_eq!(reopened.envelope, Some(envelope));
        assert!(reopened.execution_owner.is_none());
        store
            .try_claim_queued(
                "req-task-v3-lazy-claim",
                "daemon.2.1.worker",
                "cancel.v3",
                200,
            )
            .unwrap()
            .unwrap();
        assert!(
            fs::read_to_string(store.task_path("req-task-v3-lazy-claim").unwrap())
                .unwrap()
                .starts_with("format=eva.task-state.v5\n")
        );
    }

    #[test]
    /// 生命周期 CAS 不得替换、删除或改写最初提交的 payload 与 attempt policy。
    fn task_envelope_is_immutable_across_lifecycle_cas() {
        let root = test_root("envelope-immutable");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"original".to_vec(),
            "idem-envelope-immutable",
            TaskAttemptPolicySnapshot::new(2, 100, Some(2_000)).unwrap(),
        )
        .unwrap();
        let created = store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-task-envelope-immutable",
                    envelope.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let mut changed = created.clone();
        changed.envelope = Some(
            TaskEnvelopeSnapshot::inline(
                "runtime.echo",
                "root-agent",
                b"changed".to_vec(),
                "idem-envelope-immutable",
                TaskAttemptPolicySnapshot::new(2, 100, Some(2_000)).unwrap(),
            )
            .unwrap(),
        );

        let error = store.compare_and_set(&changed).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            store
                .read(Some("req-task-envelope-immutable"))
                .unwrap()
                .envelope,
            Some(envelope)
        );
    }

    #[test]
    /// 验证跨进程读改写可持久化取消标志、原因和追加日志。
    fn filesystem_task_state_updates_cancel_log_across_process_boundary() {
        let root = test_root("cancel");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        let mut snapshot = sample_snapshot("req-task-state-2");
        writer.create(&snapshot).unwrap();

        let reader = FileSystemTaskStateStore::new(root.path());
        snapshot = reader.read(Some("req-task-state-2")).unwrap();
        snapshot.cancel_requested = true;
        snapshot.cancel_accepted = false;
        snapshot.cancel_reason = Some("too late".to_owned());
        snapshot.push_log("warning", "cancel requested after terminal state");
        let committed = writer.compare_and_set(&snapshot).unwrap();

        let updated = reader.read(None).unwrap();

        assert!(updated.cancel_requested);
        assert_eq!(updated.cancel_reason.as_deref(), Some("too late"));
        assert_eq!(updated.logs.last().unwrap().level, "warning");
        assert_eq!(committed.record_version, StateVersion(2));
    }

    #[test]
    /// 验证 store 可直接使用 durable backend 的 task_dir。
    fn filesystem_task_state_can_use_durable_backend_layout() {
        let root = test_root("durable-layout");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut writer = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let snapshot = sample_snapshot("req-task-state-durable-1");

        let committed = writer.create(&snapshot).unwrap();
        let reader = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let by_id = reader.read(Some("req-task-state-durable-1")).unwrap();

        assert_eq!(by_id, committed);
        assert_eq!(by_id.record_version, StateVersion(1));
        assert_eq!(
            by_id.owner_generation,
            writer.writer.as_ref().unwrap().generation()
        );
        assert_eq!(reader.task_dir(), backend.layout().task_dir);
        assert!(backend
            .layout()
            .task_dir
            .join("req-task-state-durable-1.task")
            .is_file());
    }

    #[test]
    /// 验证两份同版本快照中只有首个 CAS 成功，迟到版本不能覆盖权威 ID 或 latest。
    fn durable_task_state_rejects_stale_record_version() {
        let root = test_root("stale-cas");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut first_store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let mut second_store = first_store.clone();
        let created = first_store
            .create(&TaskStateSnapshot::queued("req-task-state-stale").unwrap())
            .unwrap();
        let mut fresh = created.clone();
        let mut stale = created;
        fresh.request_cancel("fresh cancellation");
        stale.mark_interrupted("stale recovery");

        let committed = first_store.compare_and_set(&fresh).unwrap();
        let error = second_store.compare_and_set(&stale).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(committed.record_version, StateVersion(2));
        assert_eq!(
            first_store.read(Some("req-task-state-stale")).unwrap(),
            committed
        );
        assert_eq!(first_store.read(None).unwrap(), committed);
    }

    #[test]
    /// 旧无版本记录只允许一次 version 0 -> 1 迁移，旧快照随后不能覆盖新状态。
    fn legacy_task_state_is_migrated_once_by_cas() {
        let root = test_root("legacy-version");
        let mut store = FileSystemTaskStateStore::new(root.path());
        fs::create_dir_all(store.task_dir()).unwrap();
        let legacy = sample_snapshot("req-task-state-legacy")
            .to_storage()
            .lines()
            .skip(3)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(store.task_path("req-task-state-legacy").unwrap(), &legacy).unwrap();
        fs::write(store.latest_task_path(), legacy).unwrap();
        let stale = store.read(Some("req-task-state-legacy")).unwrap();

        let committed = store
            .update_snapshot("req-task-state-legacy", |snapshot| {
                snapshot.push_log("info", "legacy record migrated through fenced CAS");
                Ok(())
            })
            .unwrap();
        let error = store.compare_and_set(&stale).unwrap_err();

        assert_eq!(stale.record_version, StateVersion::ZERO);
        assert!(stale.envelope.is_none());
        assert_eq!(committed.record_version, StateVersion(1));
        assert!(committed.envelope.is_none());
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            store.read(Some("req-task-state-legacy")).unwrap(),
            committed
        );
    }

    #[test]
    /// latest 替换失败时权威 ID 已提交且可通过显式 refresh 修复派生别名。
    fn latest_failure_reports_canonical_commit_and_can_be_repaired() {
        let root = test_root("latest-repair");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        fs::create_dir_all(store.latest_task_path()).unwrap();
        let task_id = "req-task-latest-repair";

        let error = store.create(&sample_snapshot(task_id)).unwrap_err();

        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "canonical_committed" && value == "true"));
        let canonical = store.read(Some(task_id)).unwrap();
        assert_eq!(canonical.record_version, StateVersion(1));
        fs::remove_dir(store.latest_task_path()).unwrap();
        assert_eq!(store.refresh_latest(task_id).unwrap(), canonical);
        assert_eq!(store.read(None).unwrap(), canonical);
    }

    #[test]
    /// 验证 durable layout 的只读构造不能绕过 runtime writer ownership 执行 mutation。
    fn durable_task_state_layout_only_store_is_read_only() {
        let root = test_root("layout-read-only");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());

        let error = store
            .create(&sample_snapshot("req-task-layout-read-only"))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证列表按文件名确定排序且不重复返回 latest 别名。
    fn filesystem_task_state_lists_snapshots_without_latest_duplicate() {
        let root = test_root("list");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        writer
            .write(&sample_snapshot("req-task-state-list-2"))
            .unwrap();
        writer
            .write(&sample_snapshot("req-task-state-list-1"))
            .unwrap();

        let snapshots = writer.list_snapshots().unwrap();

        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["req-task-state-list-1", "req-task-state-list-2"]
        );
    }

    #[test]
    /// 验证任务目录尚未创建时列表为空而非错误。
    fn filesystem_task_state_lists_empty_missing_directory() {
        let root = test_root("list-missing");
        let store = FileSystemTaskStateStore::new(root.path());

        assert!(store.list_snapshots().unwrap().is_empty());
    }

    #[test]
    /// 验证指定任务文件缺失映射为 NotFound。
    fn missing_task_state_is_not_found() {
        let root = test_root("missing");
        let store = FileSystemTaskStateStore::new(root.path());

        let error = store.read(Some("req-missing-task")).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
    }

    #[test]
    /// 验证缺少 status 的不完整文件被拒绝。
    fn invalid_task_state_rejects_incomplete_files() {
        let error = TaskStateSnapshot::from_storage("task_id=req-only\n").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    #[test]
    /// 验证 interrupted 被生命周期判断视为终态。
    fn interrupted_task_state_is_terminal() {
        let mut snapshot = sample_snapshot("req-task-state-interrupted");
        snapshot.status = "interrupted".to_owned();

        assert!(snapshot.is_terminal());
    }

    #[test]
    /// 验证 running、heartbeat、deadline、cancel 和 timeout 的字段/日志转换。
    fn task_lifecycle_tracks_heartbeat_deadline_cancel_and_timeout() {
        let mut snapshot = TaskStateSnapshot::queued("req-task-lifecycle").unwrap();

        snapshot.mark_running(100, Some(200), "cancel-token-1");
        snapshot.record_heartbeat(150);
        snapshot.request_cancel("operator requested stop");

        assert_eq!(snapshot.status, "cancelling");
        assert_eq!(snapshot.heartbeat_at_ms, Some(150));
        assert_eq!(snapshot.deadline_at_ms, Some(200));
        assert_eq!(snapshot.cancel_token.as_deref(), Some("cancel-token-1"));
        assert!(snapshot.cancel_requested);
        assert!(snapshot.cancel_accepted);
        assert!(!snapshot.deadline_expired(199));
        assert!(snapshot.deadline_expired(200));

        snapshot.mark_timed_out(250, true);

        assert_eq!(snapshot.status, "timed_out");
        assert!(snapshot.is_terminal());
        assert_eq!(snapshot.error_kind.as_deref(), Some("timeout"));
        assert!(snapshot.logs.iter().any(|entry| entry.level == "error"));
    }

    #[test]
    /// 验证无 CAS update 在单写者场景写回取消状态和生命周期日志。
    fn filesystem_task_state_update_appends_lifecycle_log() {
        let root = test_root("lifecycle-update");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let mut snapshot = TaskStateSnapshot::queued("req-task-lifecycle-store").unwrap();
        snapshot.mark_running(10, Some(20), "cancel-token-store");
        store.write(&snapshot).unwrap();

        let updated = store
            .update_snapshot("req-task-lifecycle-store", |snapshot| {
                snapshot.request_cancel("operator cancel");
                Ok(())
            })
            .unwrap();

        assert_eq!(updated.status, "cancelling");
        assert_eq!(updated.logs.len(), 2);
        let reread = store.read(Some("req-task-lifecycle-store")).unwrap();
        assert_eq!(reread.status, "cancelling");
        assert_eq!(reread.cancel_reason.as_deref(), Some("operator cancel"));
    }

    #[test]
    fn abandoned_task_recovery_requeues_only_after_writer_turnover() {
        let root = test_root("abandoned-restart-requeue");
        let task_id = "req-abandoned-restart-requeue";
        let first_generation = {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            create_retry_test_task(&mut store, task_id, 2, b"abandoned");
            let running = store
                .try_claim_queued(task_id, "daemon.old.worker", "cancel.old.worker", 100)
                .unwrap()
                .unwrap()
                .snapshot()
                .clone();
            let error = store.recover_abandoned_task_for_retry(task_id).unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
            running.owner_generation
        };

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let recovered = store.recover_abandoned_task_for_retry(task_id).unwrap();
        assert_eq!(recovered.status, "queued");
        assert_eq!(recovered.attempts, 1);
        assert!(recovered.execution_owner.is_none());
        assert!(recovered.cancel_token.is_none());
        assert_ne!(recovered.owner_generation, first_generation);

        let repeated = store.recover_abandoned_task_for_retry(task_id).unwrap();
        assert_eq!(repeated, recovered);
        let claimed = store
            .try_claim_queued(task_id, "daemon.new.worker", "cancel.new.worker", 200)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.snapshot().attempts, 2);
    }

    #[test]
    fn effect_recovery_cannot_mutate_current_writer_generation() {
        let root = test_root("effect-recovery-current-generation");
        let task_id = "req-effect-recovery-current-generation";
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        create_retry_test_task(&mut store, task_id, 2, b"current-generation");
        let original = store.read(Some(task_id)).unwrap();
        let operation_digest = sha256_digest(b"current-operation");

        let committed_error = store
            .recover_task_from_committed_effect(
                task_id,
                &operation_digest,
                &sha256_digest(b"current-result"),
                b"current-result".len(),
            )
            .unwrap_err();
        assert_eq!(committed_error.kind(), eva_core::ErrorKind::Conflict);
        let prepared_error = store
            .block_task_for_unknown_effect(task_id, &operation_digest)
            .unwrap_err();
        assert_eq!(prepared_error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(store.read(Some(task_id)).unwrap(), original);
    }

    #[test]
    fn committed_effect_recovery_outweighs_contradictory_terminal_state() {
        let root = test_root("committed-effect-recovery");
        let task_id = "req-committed-effect-recovery";
        {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            create_retry_test_task(&mut store, task_id, 1, b"effect");
            finish_retry_test_task(
                &mut store,
                task_id,
                &TaskAttemptOutcome::Failed {
                    error_kind: "unavailable".to_owned(),
                    error_message: "task checkpoint lost committed business fact".to_owned(),
                    retryable: false,
                },
            );
        }

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let operation_digest = sha256_digest(b"operation");
        let result_digest = sha256_digest(b"committed-result");
        let recovered = store
            .recover_task_from_committed_effect(
                task_id,
                &operation_digest,
                &result_digest,
                b"committed-result".len(),
            )
            .unwrap();
        assert_eq!(recovered.status, "completed");
        assert_eq!(
            recovered.result_digest.as_deref(),
            Some(result_digest.as_str())
        );
        assert_eq!(recovered.result_size_bytes, Some(b"committed-result".len()));
        assert!(recovered.error_kind.is_none());
        assert!(recovered.error_message.is_none());
        assert_eq!(
            store
                .recover_task_from_committed_effect(
                    task_id,
                    &operation_digest,
                    &result_digest,
                    b"committed-result".len(),
                )
                .unwrap(),
            recovered
        );
        let conflict = store
            .recover_task_from_committed_effect(
                task_id,
                &operation_digest,
                &sha256_digest(b"different-result"),
                b"different-result".len(),
            )
            .unwrap_err();
        assert_eq!(conflict.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(store.read(Some(task_id)).unwrap(), recovered);
    }

    #[test]
    fn committed_effect_recovery_repairs_active_timeout_cancel_and_recovering_matrix() {
        let root = test_root("committed-effect-recovery-matrix");
        let running_task_id = "req-committed-matrix-running";
        let timed_out_task_id = "req-committed-matrix-timed-out";
        let cancelled_task_id = "req-committed-matrix-cancelled";
        let recovering_task_id = "req-committed-matrix-recovering";
        {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            for task_id in [
                running_task_id,
                timed_out_task_id,
                cancelled_task_id,
                recovering_task_id,
            ] {
                create_retry_test_task(&mut store, task_id, 2, task_id.as_bytes());
            }
            store
                .try_claim_queued(
                    running_task_id,
                    "daemon.matrix.running",
                    "cancel.matrix.running",
                    100,
                )
                .unwrap()
                .unwrap();
            finish_retry_test_task(
                &mut store,
                timed_out_task_id,
                &TaskAttemptOutcome::TimedOut {
                    observed_at_ms: 200,
                    retryable: false,
                },
            );
            let cancelled_claim = store
                .try_claim_queued(
                    cancelled_task_id,
                    "daemon.matrix.cancelled",
                    "cancel.matrix.cancelled",
                    100,
                )
                .unwrap()
                .unwrap();
            store
                .request_cancellation(cancelled_task_id, "cancelled after effect commit")
                .unwrap();
            store
                .finish_execution(
                    cancelled_claim.fence(),
                    &TaskAttemptOutcome::Completed {
                        result_digest: sha256_digest(b"ignored-cancelled-result"),
                        result_size_bytes: b"ignored-cancelled-result".len(),
                    },
                )
                .unwrap();
            let recovering_claim = store
                .try_claim_queued(
                    recovering_task_id,
                    "daemon.matrix.recovering",
                    "cancel.matrix.recovering",
                    100,
                )
                .unwrap()
                .unwrap();
            let mut recovering = recovering_claim.snapshot().clone();
            recovering.mark_recovering("legacy recovery contradicted committed effect");
            store.compare_and_set(&recovering).unwrap();
        }

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let operation_digest = sha256_digest(b"matrix-operation");
        let result_digest = sha256_digest(b"matrix-result");
        for task_id in [
            running_task_id,
            timed_out_task_id,
            cancelled_task_id,
            recovering_task_id,
        ] {
            let recovered = store
                .recover_task_from_committed_effect(
                    task_id,
                    &operation_digest,
                    &result_digest,
                    b"matrix-result".len(),
                )
                .unwrap();
            assert_eq!(recovered.status, "completed", "task_id={task_id}");
            assert_eq!(
                recovered.result_digest.as_deref(),
                Some(result_digest.as_str())
            );
            assert_eq!(recovered.result_size_bytes, Some(b"matrix-result".len()));
        }
    }

    #[test]
    fn prepared_effect_recovery_creates_stable_operator_block() {
        let root = test_root("prepared-effect-recovery");
        let task_id = "req-prepared-effect-recovery";
        {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            create_retry_test_task(&mut store, task_id, 2, b"prepared");
            store
                .try_claim_queued(task_id, "daemon.old.worker", "cancel.old.prepared", 100)
                .unwrap()
                .unwrap();
        }

        let operation_digest = sha256_digest(b"prepared-operation");
        let blocked = {
            let backend = crate::FileSystemDurableBackend::open(
                crate::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let blocked = store
                .block_task_for_unknown_effect(task_id, &operation_digest)
                .unwrap();
            assert_eq!(blocked.status, "interrupted");
            assert!(blocked.is_terminal());
            assert!(blocked
                .interrupted_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("operator reconciliation required")));
            assert_eq!(
                store
                    .block_task_for_unknown_effect(task_id, &operation_digest)
                    .unwrap(),
                blocked
            );
            blocked
        };

        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        assert_eq!(
            store.recover_abandoned_task_for_retry(task_id).unwrap(),
            blocked
        );
    }

    /// 创建包含日志和 replay 的 completed 快照 fixture。
    fn sample_snapshot(task_id: &str) -> TaskStateSnapshot {
        TaskStateSnapshot {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id: task_id.to_owned(),
            envelope: None,
            replay_delivery: None,
            status: "completed".to_owned(),
            attempts: 1,
            execution_owner: None,
            retry_max_attempts: 2,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            result_digest: None,
            result_size_bytes: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            error_retryable: None,
            retry_ready_at_ms: None,
            logs: vec![TaskStateLogSnapshot {
                sequence: 1,
                level: "info".to_owned(),
                message: "event accepted: evt-basic-1".to_owned(),
            }],
            dead_letters: Vec::new(),
            replayed_events: vec![TaskStateReplaySnapshot {
                event_id: "evt-basic-1".to_owned(),
                sequence: 1,
                topic: "/input/user".to_owned(),
            }],
        }
    }

    fn create_retry_test_task(
        store: &mut FileSystemTaskStateStore,
        task_id: &str,
        max_attempts: u32,
        payload: &[u8],
    ) {
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            payload.to_vec(),
            format!("idem-{task_id}"),
            TaskAttemptPolicySnapshot::new(max_attempts, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(&TaskStateSnapshot::queued_with_envelope(task_id, envelope).unwrap())
            .unwrap();
    }

    fn finish_retry_test_task(
        store: &mut FileSystemTaskStateStore,
        task_id: &str,
        outcome: &TaskAttemptOutcome,
    ) -> TaskStateSnapshot {
        let claim = store
            .try_claim_queued(
                task_id,
                &format!("daemon.retry.{task_id}"),
                &format!("cancel.retry.{task_id}"),
                100,
            )
            .unwrap()
            .unwrap();
        store.finish_execution(claim.fence(), outcome).unwrap()
    }

    /// 测试临时项目根所有者。
    struct TestRoot {
        /// 唯一临时路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 返回临时路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 测试结束时尽力递归清理。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 用测试名、进程和时间构造并行安全路径。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-storage-task-state-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
