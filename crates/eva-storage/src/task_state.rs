//! 跨进程任务快照、生命周期状态、日志/dead-letter/replay 与文件系统存储实现。
//! Durable task state contracts and filesystem implementation.

use crate::DurableBackendLayout;
use eva_core::{EvaError, RequestId};
use std::fs;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：让 runtime 与 CLI 通过稳定快照共享任务状态，而不共享进程内对象。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable task state interfaces and process-boundary snapshots";

/// CLI task 命令与 runtime 跨进程使用的完整任务状态快照。
/// Stored task summary used by CLI task commands across process boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateSnapshot {
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
    /// 写入指定任务快照并更新 latest 别名；实现不承诺跨文件原子性。
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
}

impl TaskStateSnapshot {
    /// 创建已校验 task ID 的 queued 初始快照，重试上限默认为一次。
    pub fn queued(task_id: impl Into<String>) -> Result<Self, EvaError> {
        let task_id = task_id.into();
        RequestId::parse(&task_id)?;
        Ok(Self {
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
    /// 可选值用空串表示 None。格式当前无显式 version，新增字段需保持旧解析器可忽略。
    pub fn to_storage(&self) -> String {
        let mut lines = vec![
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

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            if let Some(value) = line.strip_prefix("task_id=") {
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
        }
    }

    /// 使用 durable backend 的 task_dir 创建 store。
    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self {
            project_root: layout.root.clone(),
            task_dir: layout.task_dir.clone(),
        }
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
                TaskStateSnapshot::from_storage(&data)
                    .map_err(|error| error.with_context("path", path.display().to_string()))
            })
            .collect()
    }

    /// 对指定任务执行读-改-写并返回已提交快照。
    /// 该操作没有 generation/CAS 或锁，多写者可能丢失更新；只适用于上层已串行化的任务 owner。
    /// Closure 失败时不写盘；write 失败时返回错误而不声称更新成功。
    pub fn update_snapshot<F>(
        &mut self,
        task_id: &str,
        update: F,
    ) -> Result<TaskStateSnapshot, EvaError>
    where
        F: FnOnce(&mut TaskStateSnapshot) -> Result<(), EvaError>,
    {
        let mut snapshot = self.read(Some(task_id))?;
        update(&mut snapshot)?;
        self.write(&snapshot)?;
        Ok(snapshot)
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
}

impl TaskStateStore for FileSystemTaskStateStore {
    /// 先写任务 ID 文件，再覆盖 latest 别名。
    ///
    /// 两次 `fs::write` 都直接写最终路径，没有临时文件 rename，也不是跨文件原子事务；第二步
    /// 失败时 ID 快照可能已更新而 latest 仍旧。调用方收到错误后应按 ID 核实，不能假定回滚。
    fn write(&mut self, snapshot: &TaskStateSnapshot) -> Result<(), EvaError> {
        RequestId::parse(&snapshot.task_id)?;
        let dir = self.task_dir();
        fs::create_dir_all(&dir).map_err(|error| {
            EvaError::internal("failed to create task state directory")
                .with_context("path", dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let data = snapshot.to_storage();
        fs::write(self.task_path(&snapshot.task_id)?, data.as_bytes()).map_err(|error| {
            EvaError::internal("failed to write task state")
                .with_context("task_id", snapshot.task_id.as_str())
                .with_context("io_error", error.to_string())
        })?;
        fs::write(self.latest_task_path(), data.as_bytes()).map_err(|error| {
            EvaError::internal("failed to write latest task state")
                .with_context("task_id", snapshot.task_id.as_str())
                .with_context("io_error", error.to_string())
        })
    }

    /// 读取 ID 快照或 latest 别名并严格解析。
    /// 所有 I/O 失败当前映射为 NotFound 并附带建议；解析错误保持 InvalidArgument。
    fn read(&self, task_id: Option<&str>) -> Result<TaskStateSnapshot, EvaError> {
        let path = match task_id {
            Some(task_id) => self.task_path(task_id)?,
            None => self.latest_task_path(),
        };
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::not_found("task state does not exist")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
                .with_context("suggestion", "run `eva run --example basic` first")
        })?;
        TaskStateSnapshot::from_storage(&data)
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

        writer.write(&snapshot).unwrap();
        let reader = FileSystemTaskStateStore::new(root.path());
        let by_id = reader.read(Some("req-task-state-1")).unwrap();
        let latest = reader.read(None).unwrap();

        assert_eq!(by_id, snapshot);
        assert_eq!(latest, snapshot);
        assert_eq!(reader.project_root(), root.path());
    }

    #[test]
    /// 验证跨进程读改写可持久化取消标志、原因和追加日志。
    fn filesystem_task_state_updates_cancel_log_across_process_boundary() {
        let root = test_root("cancel");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        let mut snapshot = sample_snapshot("req-task-state-2");
        writer.write(&snapshot).unwrap();

        let reader = FileSystemTaskStateStore::new(root.path());
        snapshot = reader.read(Some("req-task-state-2")).unwrap();
        snapshot.cancel_requested = true;
        snapshot.cancel_accepted = false;
        snapshot.cancel_reason = Some("too late".to_owned());
        snapshot.push_log("warning", "cancel requested after terminal state");
        writer.write(&snapshot).unwrap();

        let updated = reader.read(None).unwrap();

        assert!(updated.cancel_requested);
        assert_eq!(updated.cancel_reason.as_deref(), Some("too late"));
        assert_eq!(updated.logs.last().unwrap().level, "warning");
    }

    #[test]
    /// 验证 store 可直接使用 durable backend 的 task_dir。
    fn filesystem_task_state_can_use_durable_backend_layout() {
        let root = test_root("durable-layout");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut writer = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let snapshot = sample_snapshot("req-task-state-durable-1");

        writer.write(&snapshot).unwrap();
        let reader = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let by_id = reader.read(Some("req-task-state-durable-1")).unwrap();

        assert_eq!(by_id, snapshot);
        assert_eq!(reader.task_dir(), backend.layout().task_dir);
        assert!(backend
            .layout()
            .task_dir
            .join("req-task-state-durable-1.task")
            .is_file());
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
