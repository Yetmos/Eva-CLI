//! 跨进程任务快照、生命周期状态、日志/dead-letter/replay 与文件系统存储实现。
//! Durable task state contracts and filesystem implementation.

use crate::durable_backend::{
    acquire_record_write_lock, atomic_write, DurableWriterGuard, FileSystemDurableBackend,
    WriterGeneration,
};
use crate::state_store::StateVersion;
use crate::DurableBackendLayout;
use eva_core::{EvaError, RequestId};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：让 runtime 与 CLI 通过稳定快照共享任务状态，而不共享进程内对象。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable task state interfaces and process-boundary snapshots";

/// CLI task 命令与 runtime 跨进程使用的完整任务状态快照。
/// Stored task summary used by CLI task commands across process boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateSnapshot {
    /// 权威 task ID 文件的持久 CAS 版本；零表示尚未创建/legacy 无版本记录。
    pub record_version: StateVersion,
    /// 提交该版本的 runtime writer generation；传统 `.eva/tasks` 路径使用零。
    pub owner_generation: WriterGeneration,
    /// 同时作为 RequestId 和文件名主键的任务 ID。
    pub task_id: String,
    /// queued/running/cancelling/终态等稳定状态文本。
    pub status: String,
    /// 已执行尝试次数。
    pub attempts: usize,
    /// 重试策略允许的最大尝试次数。
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
    /// 中断或恢复原因。
    pub interrupted_reason: Option<String>,
    /// 失败/超时的稳定错误分类文本。
    pub error_kind: Option<String>,
    /// 失败/超时的人类可读消息。
    pub error_message: Option<String>,
    /// 有序生命周期与执行日志。
    pub logs: Vec<TaskStateLogSnapshot>,
    /// 未能处理的事件摘要。
    pub dead_letters: Vec<TaskStateDeadLetterSnapshot>,
    /// 已重放事件摘要。
    pub replayed_events: Vec<TaskStateReplaySnapshot>,
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

impl TaskStateSnapshot {
    /// 创建已校验 task ID 的 queued 初始快照，重试上限默认为一次。
    pub fn queued(task_id: impl Into<String>) -> Result<Self, EvaError> {
        let task_id = task_id.into();
        RequestId::parse(&task_id)?;
        Ok(Self {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id,
            status: "queued".to_owned(),
            attempts: 0,
            retry_max_attempts: 1,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        })
    }

    /// 序列化为逐行任务格式。
    /// 标量字段唯一；log/dead_letter/replay 以重复复合行保存顺序。特殊字符做百分号编码，
    /// 可选值用空串表示 None。新记录带 format、record version 与 owner generation；旧记录
    /// 缺少这些字段时按 version/generation 零读取，并只能通过一次成功 CAS 升级。
    pub fn to_storage(&self) -> String {
        let mut lines = vec![
            "format=eva.task-state.v2".to_owned(),
            format!("record_version={}", self.record_version.0),
            format!("owner_generation={}", self.owner_generation.0),
            format!("task_id={}", encode_field(&self.task_id)),
            format!("status={}", encode_field(&self.status)),
            format!("attempts={}", self.attempts),
            format!("retry_max_attempts={}", self.retry_max_attempts),
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
        ];
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
            status: String::new(),
            attempts: 0,
            retry_max_attempts: 1,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        };
        let mut format = None;
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
            } else if let Some(value) = line.strip_prefix("retry_max_attempts=") {
                snapshot.retry_max_attempts = parse_stored_usize("retry_max_attempts", value)?;
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
        match format.as_deref() {
            None if snapshot.record_version == StateVersion::ZERO
                && snapshot.owner_generation == WriterGeneration::ZERO => {}
            Some("eva.task-state.v2")
                if snapshot.record_version != StateVersion::ZERO
                    || snapshot.owner_generation == WriterGeneration::ZERO => {}
            Some("eva.task-state.v2") => {
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
        self.push_log("warning", "task marked cancelled");
    }

    /// 将任务转换为 timed_out，记录超时时刻和稳定 timeout 错误。
    pub fn mark_timed_out(&mut self, now_ms: u128) {
        self.status = "timed_out".to_owned();
        self.heartbeat_at_ms = Some(now_ms);
        self.error_kind = Some("timeout".to_owned());
        self.error_message = Some("task deadline exceeded".to_owned());
        self.push_log("error", format!("task timed out at {now_ms}"));
    }

    /// 将任务转换为 completed，更新尝试次数并清除旧错误。
    pub fn mark_completed(&mut self, attempts: usize) {
        self.status = "completed".to_owned();
        self.attempts = attempts;
        self.error_kind = None;
        self.error_message = None;
        self.push_log("info", "task completed");
    }

    /// 将任务转换为 failed，保存最终尝试次数和调用方提供的错误分类/消息。
    pub fn mark_failed(
        &mut self,
        attempts: usize,
        error_kind: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        self.status = "failed".to_owned();
        self.attempts = attempts;
        self.error_kind = Some(error_kind.into());
        self.error_message = Some(error_message.into());
        self.push_log("error", "task failed");
    }

    /// 将任务标为 interrupted 终态并保存恢复诊断原因。
    pub fn mark_interrupted(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = "interrupted".to_owned();
        self.interrupted_reason = Some(reason.clone());
        self.push_log("warning", format!("task interrupted: {reason}"));
    }

    /// 将任务标为 recovering 非终态，保留触发恢复的中断原因。
    pub fn mark_recovering(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = "recovering".to_owned();
        self.interrupted_reason = Some(reason.clone());
        self.push_log("warning", format!("task recovering: {reason}"));
    }

    /// 判断 now 是否到达或超过 deadline；无 deadline 永不超时。
    pub fn deadline_expired(&self, now_ms: u128) -> bool {
        self.deadline_at_ms
            .map(|deadline| now_ms >= deadline)
            .unwrap_or(false)
    }

    /// 判断状态是否禁止继续正常执行；interrupted 视为终态，recovering 不是。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "timed_out" | "interrupted"
        )
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
        self.commit_snapshot(snapshot, StateVersion::ZERO, true)
    }

    /// 使用 snapshot 携带的 record version 作为 expected 值执行持久 CAS。
    pub fn compare_and_set(
        &mut self,
        snapshot: &TaskStateSnapshot,
    ) -> Result<TaskStateSnapshot, EvaError> {
        self.commit_snapshot(snapshot, snapshot.record_version, false)
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
        create_only: bool,
    ) -> Result<TaskStateSnapshot, EvaError> {
        RequestId::parse(&snapshot.task_id)?;
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
            }
            if create_only && current.is_some() {
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

fn is_single_task_field(field: &str) -> bool {
    matches!(
        field,
        "format"
            | "record_version"
            | "owner_generation"
            | "task_id"
            | "status"
            | "attempts"
            | "retry_max_attempts"
            | "cancel_requested"
            | "cancel_accepted"
            | "cancel_reason"
            | "heartbeat_at_ms"
            | "deadline_at_ms"
            | "cancel_token"
            | "interrupted_reason"
            | "error_kind"
            | "error_message"
    )
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

/// 严格解析日志/replay 序号。
fn parse_stored_u64(name: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
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
            .create(&sample_snapshot("req-task-state-stale"))
            .unwrap();
        let mut fresh = created.clone();
        let mut stale = created;
        fresh.status = "completed-fresh".to_owned();
        stale.status = "completed-stale".to_owned();

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
                snapshot.status = "migrated".to_owned();
                Ok(())
            })
            .unwrap();
        let error = store.compare_and_set(&stale).unwrap_err();

        assert_eq!(stale.record_version, StateVersion::ZERO);
        assert_eq!(committed.record_version, StateVersion(1));
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

        snapshot.mark_timed_out(250);

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

    /// 创建包含日志和 replay 的 completed 快照 fixture。
    fn sample_snapshot(task_id: &str) -> TaskStateSnapshot {
        TaskStateSnapshot {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id: task_id.to_owned(),
            status: "completed".to_owned(),
            attempts: 1,
            retry_max_attempts: 2,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
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
