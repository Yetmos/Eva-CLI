//! Durable EventLog 的内存/文件系统实现、状态覆盖、重放和严格磁盘解码。
//! Durable event log implementations.

/// 本模块的架构职责：为事件分配稳定序号，持久化消费状态，并从 watermark 重放。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable event log interfaces and replay boundaries";

use crate::{atomic_write, DurableBackendLayout, DurableWriterGuard};
use eva_core::{
    AdapterId, AgentId, CapabilityName, ErrorKind, EvaError, Event, EventId, EventMetadata,
    EventPayload, EventTarget, GenerationId, RequestId, Topic, TraceContext,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// Durable event 根下保存序号文件的固定子目录。
const EVENT_LOG_DIR: &str = "log";
/// 事件记录文件扩展名；加载器忽略其他旁路文件。
const EVENT_RECORD_EXTENSION: &str = "event";
/// 当前逐行字段磁盘格式版本；未知版本严格拒绝。
const EVENT_RECORD_VERSION: &str = "1";

/// 单条事件记录的持久化消费生命周期。
/// Lifecycle state for one event log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventLogStatus {
    /// 已追加但尚未被消费者确认。
    Appended,
    /// 指定消费者已成功处理。
    Acked,
    /// 指定消费者处理失败，并携带结构化错误。
    Failed,
}

/// EventBus 与 Agent 消费者共享的序号记录；ack/fail 会覆盖同一序号文件的状态。
/// Append-only event log record used by EventBus and Agent consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogRecord {
    /// 从 1 开始单调递增的日志序号。
    pub sequence: u64,
    /// 完整强类型事件及 metadata。
    pub event: Event,
    /// 当前消费状态。
    pub status: EventLogStatus,
    /// ack/fail 后的可选消费者 Agent ID。
    pub consumer: Option<AgentId>,
    /// Failed 状态的可选结构化错误；其他状态应为空。
    pub error: Option<EvaError>,
}

/// 运行时需要的 EventLog 追加、状态转换与重放行为。
/// Event log behavior required by the V0.4 runtime loop.
pub trait EventLog {
    /// 追加唯一 event ID 并返回分配后的记录；重复 ID 返回 Conflict。
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError>;
    /// 将既有记录转换为 Acked，记录消费者并清除旧错误。
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError>;
    /// 将既有记录转换为 Failed，保留消费者与完整结构化错误。
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError>;
    /// 克隆返回 sequence 大于等于游标的记录；游标语义是包含式。
    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord>;
    /// 返回当前最高已提交序号，空日志为 0。
    fn watermark(&self) -> u64;
}

/// 测试和 V0.4 基础运行时路径使用的内存日志。
/// In-memory log used by tests and the V0.4 basic runtime path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryEventLog {
    /// 当前最高已分配序号。
    next_sequence: u64,
    /// 按追加顺序保存的记录。
    records: Vec<EventLogRecord>,
}

/// 位于 durable backend event 子树下的文件系统事件日志。
/// Filesystem-backed event log rooted under the durable backend event dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemEventLog {
    /// `.event` 文件所在目录。
    root: PathBuf,
    /// 重开时从最高有效记录恢复的 watermark。
    next_sequence: u64,
    /// 按 sequence 排序的内存索引。
    records: Vec<EventLogRecord>,
    /// 可选 runtime writer；持有时所有 mutation 都在 fencing 写锁内重读权威磁盘状态。
    writer: Option<DurableWriterGuard>,
}

impl EventLogStatus {
    /// 返回稳定磁盘状态码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Appended => "appended",
            Self::Acked => "acked",
            Self::Failed => "failed",
        }
    }

    /// 严格解析磁盘状态；未知值返回 Conflict。
    fn from_storage(value: &str) -> Result<Self, EvaError> {
        match value {
            "appended" => Ok(Self::Appended),
            "acked" => Ok(Self::Acked),
            "failed" => Ok(Self::Failed),
            _ => {
                Err(EvaError::conflict("event log status is invalid").with_context("status", value))
            }
        }
    }
}

impl EventLogRecord {
    /// 创建新追加记录，初态没有消费者或错误。
    fn appended(sequence: u64, event: Event) -> Self {
        Self {
            sequence,
            event,
            status: EventLogStatus::Appended,
            consumer: None,
            error: None,
        }
    }
}

/// 将记录单调推进到 Acked；同一消费者重复确认幂等，其他消费者不能改写归属。
fn transition_to_acked(record: &mut EventLogRecord, consumer: AgentId) -> Result<bool, EvaError> {
    if record.status == EventLogStatus::Acked {
        if record.consumer.as_ref() == Some(&consumer) {
            return Ok(false);
        }
        return Err(
            EvaError::conflict("acked event cannot be reassigned to another consumer")
                .with_context("event_id", record.event.event_id().as_str())
                .with_context("consumer", consumer.as_str()),
        );
    }

    record.status = EventLogStatus::Acked;
    record.consumer = Some(consumer);
    record.error = None;
    Ok(true)
}

/// 将未完成记录标记为 Failed；Acked 是终态，迟到失败不得覆盖成功事实。
fn transition_to_failed(
    record: &mut EventLogRecord,
    consumer: AgentId,
    error: EvaError,
) -> Result<(), EvaError> {
    if record.status == EventLogStatus::Acked {
        return Err(
            EvaError::conflict("acked event cannot transition to failed")
                .with_context("event_id", record.event.event_id().as_str())
                .with_context("consumer", consumer.as_str()),
        );
    }

    record.status = EventLogStatus::Failed;
    record.consumer = Some(consumer);
    record.error = Some(error);
    Ok(())
}

impl InMemoryEventLog {
    /// 创建空内存日志。
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回当前记录切片。
    pub fn records(&self) -> &[EventLogRecord] {
        &self.records
    }

    /// 按 event ID 查找可变记录；缺失返回 NotFound。
    fn find_mut(&mut self, event_id: &EventId) -> Result<&mut EventLogRecord, EvaError> {
        self.records
            .iter_mut()
            .find(|record| record.event.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("event log record does not exist")
                    .with_context("event_id", event_id.as_str())
            })
    }

    /// 精确判断 event ID 是否已追加，用于幂等冲突保护。
    fn contains_event(&self, event_id: &EventId) -> bool {
        self.records
            .iter()
            .any(|record| record.event.event_id() == event_id)
    }
}

impl FileSystemEventLog {
    /// 从 durable layout 以可创建模式打开 event log。
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::open_dir_with_mode(layout.event_dir.join(EVENT_LOG_DIR), true, None)
    }

    /// 在 runtime writer ownership 下打开 event log；后续 mutation 会受 generation fence 保护。
    pub fn open_with_writer(
        layout: &DurableBackendLayout,
        writer: DurableWriterGuard,
    ) -> Result<Self, EvaError> {
        if writer.root() != layout.root {
            return Err(EvaError::conflict(
                "durable event log writer belongs to a different backend root",
            )
            .with_context("layout_root", layout.root.display().to_string())
            .with_context("writer_root", writer.root().display().to_string()));
        }
        let root = layout.event_dir.join(EVENT_LOG_DIR);
        writer.with_write_lock(|_| Self::open_dir_with_mode(root, true, Some(writer.clone())))
    }

    /// 只读打开；目录缺失时返回空视图且不创建任何路径。
    pub fn open_read_only(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::open_dir_with_mode(layout.event_dir.join(EVENT_LOG_DIR), false, None)
    }

    /// 从显式记录目录以可创建模式打开，供独立测试或嵌入使用。
    pub fn open_dir(root: impl Into<PathBuf>) -> Result<Self, EvaError> {
        Self::open_dir_with_mode(root, true, None)
    }

    /// 打开目录、加载全部目标扩展名记录、排序并验证 sequence/event ID 唯一性。
    /// 任何目标记录不可读或损坏都会使整个 open 失败，不返回部分日志。
    fn open_dir_with_mode(
        root: impl Into<PathBuf>,
        create_if_missing: bool,
        writer: Option<DurableWriterGuard>,
    ) -> Result<Self, EvaError> {
        let root = root.into();
        if root.exists() {
            if !root.is_dir() {
                return Err(
                    EvaError::conflict("durable event log path is not a directory")
                        .with_context("path", root.display().to_string()),
                );
            }
        } else if create_if_missing {
            fs::create_dir_all(&root).map_err(|error| {
                EvaError::internal("failed to create durable event log directory")
                    .with_context("path", root.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        } else {
            return Ok(Self {
                root,
                next_sequence: 0,
                records: Vec::new(),
                writer,
            });
        }

        let (next_sequence, records) = load_authoritative_records(&root)?;

        Ok(Self {
            root,
            next_sequence,
            records,
            writer,
        })
    }

    /// 返回按 sequence 排序的已加载记录。
    pub fn records(&self) -> &[EventLogRecord] {
        &self.records
    }

    /// Reloads the authoritative on-disk index without performing a mutation.
    ///
    /// Writer-backed instances take the same serialized writer lock used by mutations. Read-only
    /// instances preserve the open-time convention that a missing directory is an empty log.
    pub fn refresh(&mut self) -> Result<(), EvaError> {
        if let Some(writer) = self.writer.clone() {
            return writer.with_write_lock(|_| self.reload_authoritative_records());
        }
        if !self.root.exists() {
            self.next_sequence = 0;
            self.records.clear();
            return Ok(());
        }
        self.reload_authoritative_records()
    }

    /// 以 20 位零填充序号生成记录路径，使字典序与重放顺序一致。
    fn record_path(&self, sequence: u64) -> PathBuf {
        self.root
            .join(format!("{sequence:020}.{EVENT_RECORD_EXTENSION}"))
    }

    /// 以临时文件 + 原子替换提交记录；append 创建，ack/fail 覆盖同一路径。
    fn persist_record(&self, record: &EventLogRecord) -> Result<(), EvaError> {
        let path = self.record_path(record.sequence);
        atomic_write(&path, record_to_storage(record).as_bytes()).map_err(|error| {
            EvaError::internal("failed to atomically write durable event log record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })
    }

    /// 在内存索引中检查重复 event ID。
    fn contains_event(&self, event_id: &EventId) -> bool {
        self.records
            .iter()
            .any(|record| record.event.event_id() == event_id)
    }

    /// 返回指定 event ID 的内存位置；缺失返回 NotFound，状态转换不会隐式追加。
    fn record_position(&self, event_id: &EventId) -> Result<usize, EvaError> {
        self.records
            .iter()
            .position(|record| record.event.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("event log record does not exist")
                    .with_context("event_id", event_id.as_str())
            })
    }

    /// 从磁盘重建内存索引；仅在完整加载和校验成功后替换当前视图。
    fn reload_authoritative_records(&mut self) -> Result<(), EvaError> {
        let (next_sequence, records) = load_authoritative_records(&self.root)?;
        self.next_sequence = next_sequence;
        self.records = records;
        Ok(())
    }

    /// writer 模式在同一 owner 的串行写锁内重读权威状态，防止陈旧实例覆盖新提交。
    fn with_mutation<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, EvaError>,
    ) -> Result<T, EvaError> {
        if let Some(writer) = self.writer.clone() {
            writer.with_write_lock(|_| {
                self.reload_authoritative_records()?;
                operation(self)
            })
        } else {
            operation(self)
        }
    }
}

impl EventLog for InMemoryEventLog {
    /// 检查 ID 唯一后分配下一个序号并追加内存记录。
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError> {
        if self.contains_event(event.event_id()) {
            return Err(EvaError::conflict("event already exists in log")
                .with_context("event_id", event.event_id().as_str()));
        }

        self.next_sequence += 1;
        let record = EventLogRecord::appended(self.next_sequence, event);
        self.records.push(record.clone());
        Ok(record)
    }

    /// 原地标记成功消费并清除可能存在的旧失败错误。
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        let record = self.find_mut(event_id)?;
        transition_to_acked(record, consumer)?;
        Ok(record.clone())
    }

    /// 原地标记消费失败并保存结构化错误。
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        let record = self.find_mut(event_id)?;
        transition_to_failed(record, consumer, error)?;
        Ok(record.clone())
    }

    /// 按追加顺序返回包含游标的记录克隆。
    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord> {
        self.records
            .iter()
            .filter(|record| record.sequence >= sequence)
            .cloned()
            .collect()
    }

    /// 返回内存日志最高分配序号。
    fn watermark(&self) -> u64 {
        self.next_sequence
    }
}

impl EventLog for FileSystemEventLog {
    /// 在持久化成功后才推进 watermark 和内存索引。
    /// writer 模式先在串行写锁内重读权威索引，I/O 失败时不会推进内存视图。
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError> {
        self.with_mutation(|log| {
            if log.contains_event(event.event_id()) {
                return Err(EvaError::conflict("event already exists in log")
                    .with_context("event_id", event.event_id().as_str()));
            }

            let sequence = log.next_sequence + 1;
            let record = EventLogRecord::appended(sequence, event);
            log.persist_record(&record)?;
            log.next_sequence = sequence;
            log.records.push(record.clone());
            Ok(record)
        })
    }

    /// 克隆旧记录、先持久化 Acked 状态，再替换内存项；写失败保留旧内存状态。
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        self.with_mutation(|log| {
            let position = log.record_position(event_id)?;
            let mut record = log.records[position].clone();
            if transition_to_acked(&mut record, consumer)? {
                log.persist_record(&record)?;
                log.records[position] = record.clone();
            }
            Ok(record)
        })
    }

    /// 先持久化 Failed 状态与错误，再提交内存替换；失败不产生内存/磁盘成功假象。
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        self.with_mutation(|log| {
            let position = log.record_position(event_id)?;
            let mut record = log.records[position].clone();
            transition_to_failed(&mut record, consumer, error)?;
            log.persist_record(&record)?;
            log.records[position] = record.clone();
            Ok(record)
        })
    }

    /// 从已加载内存索引按包含式游标返回记录。
    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord> {
        self.records
            .iter()
            .filter(|record| record.sequence >= sequence)
            .cloned()
            .collect()
    }

    /// 返回最高已成功持久化的序号。
    fn watermark(&self) -> u64 {
        self.next_sequence
    }
}

/// 加载、排序并校验磁盘权威记录，同时恢复最高已提交 sequence。
fn load_authoritative_records(root: &Path) -> Result<(u64, Vec<EventLogRecord>), EvaError> {
    let mut records = load_records(root)?;
    records.sort_by_key(|record| record.sequence);
    validate_loaded_records(&records)?;
    let next_sequence = records.last().map(|record| record.sequence).unwrap_or(0);
    Ok((next_sequence, records))
}

/// 加载目录内所有 `.event` 文件；其他扩展名被忽略。
/// 目标文件读取或解析失败会附加路径并终止 open，避免返回缺记录的伪完整 watermark。
fn load_records(root: &Path) -> Result<Vec<EventLogRecord>, EvaError> {
    let mut records = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        EvaError::internal("failed to read durable event log directory")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read durable event log entry")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(EVENT_RECORD_EXTENSION) {
            continue;
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::conflict("failed to read durable event log record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        records.push(
            record_from_storage(&data)
                .map_err(|error| error.with_context("path", path.display().to_string()))?,
        );
    }
    Ok(records)
}

/// 验证已加载记录的 sequence 与 event ID 分别唯一。
/// 不要求序号连续，允许持久化失败或运维留下空洞；重复值会使重放/CAS 语义含糊，故拒绝。
fn validate_loaded_records(records: &[EventLogRecord]) -> Result<(), EvaError> {
    let mut sequences = BTreeSet::new();
    let mut event_ids = BTreeSet::new();
    for record in records {
        if !sequences.insert(record.sequence) {
            return Err(
                EvaError::conflict("durable event log sequence is duplicated")
                    .with_context("sequence", record.sequence.to_string()),
            );
        }
        if !event_ids.insert(record.event.event_id().clone()) {
            return Err(
                EvaError::conflict("durable event log event id is duplicated")
                    .with_context("event_id", record.event.event_id().as_str()),
            );
        }
    }
    Ok(())
}

/// 将记录序列化为 version=1 的唯一 key 逐行格式。
fn record_to_storage(record: &EventLogRecord) -> String {
    let mut data = String::new();
    push_field(&mut data, "version", EVENT_RECORD_VERSION);
    push_field(&mut data, "sequence", &record.sequence.to_string());
    push_field(&mut data, "status", record.status.as_str());
    push_event_fields(&mut data, &record.event);
    push_optional_string(
        &mut data,
        "consumer",
        record.consumer.as_ref().map(AgentId::as_str),
    );
    push_error_fields(&mut data, "error", record.error.as_ref());
    data
}

/// 严格解析单条记录，并通过核心强类型重新校验 ID、Topic、Capability 与 metadata。
/// 未知版本/字段损坏最终以 Conflict 返回，不允许跳过字段后构造降级事件。
fn record_from_storage(data: &str) -> Result<EventLogRecord, EvaError> {
    let fields = parse_fields(data)?;
    let version = require_field(&fields, "version")?;
    if version != EVENT_RECORD_VERSION {
        return Err(
            EvaError::conflict("event log record version is unsupported")
                .with_context("version", version),
        );
    }

    let sequence = parse_u64(require_field(&fields, "sequence")?, "sequence")?;
    let status = EventLogStatus::from_storage(require_field(&fields, "status")?)?;
    let event = event_from_fields(&fields)?;
    let consumer = optional_decoded_string(&fields, "consumer")?
        .map(|value| AgentId::parse(&value))
        .transpose()?;
    let error = error_from_fields(&fields, "error")?;

    Ok(EventLogRecord {
        sequence,
        event,
        status,
        consumer,
        error,
    })
}

/// 将完整 Event 展平到稳定字段集合。
/// 字符串使用 hex 以避免换行和 `=` 破坏格式；二进制 payload 直接 hex；SystemTime 采用
/// 秒+纳秒保持精度，早于 epoch 时回退 epoch。
fn push_event_fields(data: &mut String, event: &Event) {
    push_encoded_string(data, "event_id", event.event_id().as_str());
    push_encoded_string(data, "topic", event.topic().as_str());
    match event.target() {
        EventTarget::Broadcast => {
            push_field(data, "target_kind", "broadcast");
            push_optional_string(data, "target_value", None);
        }
        EventTarget::Agent(value) => {
            push_field(data, "target_kind", "agent");
            push_optional_string(data, "target_value", Some(value.as_str()));
        }
        EventTarget::Capability(value) => {
            push_field(data, "target_kind", "capability");
            push_optional_string(data, "target_value", Some(value.as_str()));
        }
        EventTarget::Adapter(value) => {
            push_field(data, "target_kind", "adapter");
            push_optional_string(data, "target_value", Some(value.as_str()));
        }
    }

    match event.payload() {
        EventPayload::Empty => {
            push_field(data, "payload_kind", "empty");
            push_field(data, "payload_value", "");
        }
        EventPayload::Text(value) => {
            push_field(data, "payload_kind", "text");
            push_encoded_string(data, "payload_value", value);
        }
        EventPayload::Bytes(value) => {
            push_field(data, "payload_kind", "bytes");
            push_field(data, "payload_value", &hex_encode(value));
        }
    }

    let created_at = event
        .metadata()
        .created_at()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    push_field(data, "created_at_secs", &created_at.as_secs().to_string());
    push_field(
        data,
        "created_at_nanos",
        &created_at.subsec_nanos().to_string(),
    );
    push_optional_string(
        data,
        "request_id",
        event.metadata().request_id().map(RequestId::as_str),
    );
    push_optional_string(
        data,
        "generation_id",
        event.metadata().generation_id().map(GenerationId::as_str),
    );
    push_optional_string(
        data,
        "correlation_id",
        event
            .metadata()
            .trace()
            .correlation_id()
            .map(EventId::as_str),
    );
    push_optional_string(
        data,
        "causation_id",
        event.metadata().trace().causation_id().map(EventId::as_str),
    );
}

/// 从展平字段重建强类型 Event。
/// target/payload kind 必须为已知枚举，纳秒值由 Duration 校验，所有可选 ID 再次解析；
/// 任一字段损坏都阻止整个记录进入重放集合。
fn event_from_fields(fields: &BTreeMap<String, String>) -> Result<Event, EvaError> {
    let event_id = EventId::parse(&decoded_required_string(fields, "event_id")?)?;
    let topic = Topic::parse(&decoded_required_string(fields, "topic")?)?;
    let target = match require_field(fields, "target_kind")? {
        "broadcast" => EventTarget::Broadcast,
        "agent" => EventTarget::Agent(AgentId::parse(&decoded_required_string(
            fields,
            "target_value",
        )?)?),
        "capability" => EventTarget::Capability(CapabilityName::parse(&decoded_required_string(
            fields,
            "target_value",
        )?)?),
        "adapter" => EventTarget::Adapter(AdapterId::parse(&decoded_required_string(
            fields,
            "target_value",
        )?)?),
        value => {
            return Err(EvaError::conflict("event target kind is invalid")
                .with_context("target_kind", value))
        }
    };
    let payload = match require_field(fields, "payload_kind")? {
        "empty" => EventPayload::empty(),
        "text" => EventPayload::text(decoded_required_string(fields, "payload_value")?),
        "bytes" => EventPayload::bytes(hex_decode(require_field(fields, "payload_value")?)?),
        value => {
            return Err(EvaError::conflict("event payload kind is invalid")
                .with_context("payload_kind", value))
        }
    };

    let created_at_secs = parse_u64(require_field(fields, "created_at_secs")?, "created_at_secs")?;
    let created_at_nanos = parse_u32(
        require_field(fields, "created_at_nanos")?,
        "created_at_nanos",
    )?;
    let request_id = optional_decoded_string(fields, "request_id")?
        .map(|value| RequestId::parse(&value))
        .transpose()?;
    let generation_id = optional_decoded_string(fields, "generation_id")?
        .map(|value| GenerationId::parse(&value))
        .transpose()?;
    let correlation_id = optional_decoded_string(fields, "correlation_id")?
        .map(|value| EventId::parse(&value))
        .transpose()?;
    let causation_id = optional_decoded_string(fields, "causation_id")?
        .map(|value| EventId::parse(&value))
        .transpose()?;

    let metadata = EventMetadata::new()
        .with_created_at(UNIX_EPOCH + Duration::new(created_at_secs, created_at_nanos))
        .with_trace(TraceContext::new(correlation_id, causation_id));
    let metadata = if let Some(request_id) = request_id {
        metadata.with_request_id(request_id)
    } else {
        metadata
    };
    let metadata = if let Some(generation_id) = generation_id {
        metadata.with_generation_id(generation_id)
    } else {
        metadata
    };

    Ok(Event::new(event_id, topic, payload)
        .with_target(target)
        .with_metadata(metadata))
}

/// 将可选 EvaError 展平为带 prefix 的 kind/message/retry/provider/context 字段。
/// 无错误仍写空核心字段和 context_len=0，使磁盘 schema 固定且解析无需猜测字段缺失。
fn push_error_fields(data: &mut String, prefix: &str, error: Option<&EvaError>) {
    if let Some(error) = error {
        push_field(data, &format!("{prefix}_kind"), error.kind().as_str());
        push_encoded_string(data, &format!("{prefix}_message"), error.message());
        push_field(
            data,
            &format!("{prefix}_retryable"),
            if error.is_retryable() {
                "true"
            } else {
                "false"
            },
        );
        push_optional_string(
            data,
            &format!("{prefix}_provider_code"),
            error.provider_code().map(|value| value.as_str()),
        );
        push_field(
            data,
            &format!("{prefix}_context_len"),
            &error.context().entries().len().to_string(),
        );
        for (index, (key, value)) in error.context().entries().iter().enumerate() {
            push_encoded_string(data, &format!("{prefix}_context_{index}_key"), key);
            push_encoded_string(data, &format!("{prefix}_context_{index}_value"), value);
        }
    } else {
        push_field(data, &format!("{prefix}_kind"), "");
        push_field(data, &format!("{prefix}_message"), "");
        push_field(data, &format!("{prefix}_retryable"), "");
        push_field(data, &format!("{prefix}_provider_code"), "");
        push_field(data, &format!("{prefix}_context_len"), "0");
    }
}

/// 从带 prefix 字段重建可选结构化错误，并按声明长度逐项恢复上下文。
fn error_from_fields(
    fields: &BTreeMap<String, String>,
    prefix: &str,
) -> Result<Option<EvaError>, EvaError> {
    let kind_value = require_field(fields, &format!("{prefix}_kind"))?;
    if kind_value.is_empty() {
        return Ok(None);
    }

    let kind = error_kind_from_storage(kind_value)?;
    let message = decoded_required_string(fields, &format!("{prefix}_message"))?;
    let retryable = parse_bool(
        require_field(fields, &format!("{prefix}_retryable"))?,
        &format!("{prefix}_retryable"),
    )?;
    let mut error = EvaError::new(kind, message).with_retryable(retryable);
    if let Some(provider_code) =
        optional_decoded_string(fields, &format!("{prefix}_provider_code"))?
    {
        error = error.with_provider_code(provider_code);
    }
    let context_len = parse_usize(
        require_field(fields, &format!("{prefix}_context_len"))?,
        &format!("{prefix}_context_len"),
    )?;
    for index in 0..context_len {
        let key = decoded_required_string(fields, &format!("{prefix}_context_{index}_key"))?;
        let value = decoded_required_string(fields, &format!("{prefix}_context_{index}_value"))?;
        error = error.with_context(key, value);
    }
    Ok(Some(error))
}

/// 将稳定错误码映射回 ErrorKind；未知码表示记录版本内的损坏数据。
fn error_kind_from_storage(value: &str) -> Result<ErrorKind, EvaError> {
    match value {
        "invalid_argument" => Ok(ErrorKind::InvalidArgument),
        "not_found" => Ok(ErrorKind::NotFound),
        "conflict" => Ok(ErrorKind::Conflict),
        "permission_denied" => Ok(ErrorKind::PermissionDenied),
        "timeout" => Ok(ErrorKind::Timeout),
        "unavailable" => Ok(ErrorKind::Unavailable),
        "internal" => Ok(ErrorKind::Internal),
        "unsupported" => Ok(ErrorKind::Unsupported),
        _ => Err(EvaError::conflict("error kind is invalid").with_context("error_kind", value)),
    }
}

/// 解析唯一 `key=value` 行到有序 map。
/// key 必须非空且无边界空白；重复 key 由后值覆盖，当前序列化器不会产生重复字段。
fn parse_fields(data: &str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::conflict(
                "durable event log record field is invalid",
            ));
        };
        if key.trim().is_empty() || key.trim() != key {
            return Err(
                EvaError::conflict("durable event log record field key is invalid")
                    .with_context("field", key),
            );
        }
        fields.insert(key.to_owned(), value.to_owned());
    }
    Ok(fields)
}

/// 追加一条未编码 `key=value\n`；调用方负责 key/value 不破坏行格式。
fn push_field(data: &mut String, key: &str, value: &str) {
    data.push_str(key);
    data.push('=');
    data.push_str(value);
    data.push('\n');
}

/// 将 UTF-8 字符串编码为 hex 后追加，完全隔离分隔符和控制字符。
fn push_encoded_string(data: &mut String, key: &str, value: &str) {
    push_field(data, key, &hex_encode(value.as_bytes()));
}

/// 可选字符串以空字段表示 None，非空 Some 值按 hex 写入；空字符串与 None 在磁盘上等价。
fn push_optional_string(data: &mut String, key: &str, value: Option<&str>) {
    match value {
        Some(value) => push_encoded_string(data, key, value),
        None => push_field(data, key, ""),
    }
}

/// 获取必填原始字段；缺失返回带字段名的 Conflict。
fn require_field<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, EvaError> {
    fields.get(key).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("durable event log record field is missing").with_context("field", key)
    })
}

/// 获取并 hex/UTF-8 解码必填字符串，在失败上补充字段名。
fn decoded_required_string(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<String, EvaError> {
    decode_string(require_field(fields, key)?).map_err(|error| error.with_context("field", key))
}

/// 读取可选 hex 字符串；字段缺失或空值均视为 None，以兼容显式空可选字段。
fn optional_decoded_string(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<String>, EvaError> {
    match fields.get(key).map(String::as_str) {
        Some(value) if !value.is_empty() => decode_string(value)
            .map(Some)
            .map_err(|error| error.with_context("field", key)),
        _ => Ok(None),
    }
}

/// 严格解析 u64 磁盘字段并报告字段名。
fn parse_u64(value: &str, field: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("durable event log numeric field is invalid")
            .with_context("field", field)
    })
}

/// 严格解析 u32 字段，主要用于 SystemTime 纳秒部分。
fn parse_u32(value: &str, field: &str) -> Result<u32, EvaError> {
    value.parse::<u32>().map_err(|_| {
        EvaError::conflict("durable event log numeric field is invalid")
            .with_context("field", field)
    })
}

/// 严格解析平台大小计数，主要用于错误 context 长度。
fn parse_usize(value: &str, field: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::conflict("durable event log numeric field is invalid")
            .with_context("field", field)
    })
}

/// 只接受小写 `true`/`false`，拒绝宽松数值或大小写变体。
fn parse_bool(value: &str, field: &str) -> Result<bool, EvaError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(
            EvaError::conflict("durable event log boolean field is invalid")
                .with_context("field", field),
        ),
    }
}

/// 将 hex 解码为字节后严格验证 UTF-8。
fn decode_string(value: &str) -> Result<String, EvaError> {
    String::from_utf8(hex_decode(value)?)
        .map_err(|_| EvaError::conflict("durable event log string field is not utf-8"))
}

/// 将任意字节编码为确定性小写 hex。
fn hex_encode(bytes: &[u8]) -> String {
    /// 小写半字节查找表，保证跨平台输出一致。
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// 解码偶数长度 hex；奇数长度或非法数字均作为磁盘 Conflict。
fn hex_decode(value: &str) -> Result<Vec<u8>, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict("hex field has odd length"));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let high = hex_value(chunk[0])?;
        let low = hex_value(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// 将单个 ASCII hex digit 转成半字节，兼容大小写输入。
fn hex_value(value: u8) -> Result<u8, EvaError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(EvaError::conflict("hex field contains invalid digit")),
    }
}

#[cfg(test)]
/// EventLog 序号、状态转换、结构化错误、重放和文件重开语义的回归测试。
mod tests {
    use super::*;
    use crate::{DurableBackendOptions, FileSystemDurableBackend};
    use eva_core::{EventPayload, EventTarget, Topic};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 创建最小文本广播事件 fixture。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    /// 创建覆盖二进制 payload、定向目标和完整 metadata 的事件 fixture。
    fn targeted_event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::bytes([1, 2, 3]),
        )
        .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()))
        .with_metadata(
            EventMetadata::new()
                .with_created_at(UNIX_EPOCH + Duration::new(42, 7))
                .with_request_id(RequestId::parse("req-1").unwrap())
                .with_generation_id(GenerationId::parse("gen-1").unwrap())
                .with_trace(TraceContext::new(
                    Some(EventId::parse("evt-root").unwrap()),
                    Some(EventId::parse("evt-parent").unwrap()),
                )),
        )
    }

    #[test]
    /// 验证 append 从 1 分配序号并同步推进 watermark。
    fn append_assigns_sequence_and_watermark() {
        let mut log = InMemoryEventLog::new();

        let first = log.append(event("evt-1")).unwrap();
        let second = log.append(event("evt-2")).unwrap();

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(log.watermark(), 2);
    }

    #[test]
    /// 验证 ack 保存消费者、清除错误并转换为 Acked。
    fn ack_marks_consumer() {
        let mut log = InMemoryEventLog::new();
        let event = event("evt-1");
        let event_id = event.event_id().clone();
        log.append(event).unwrap();

        let record = log
            .ack(&event_id, AgentId::parse("root-agent").unwrap())
            .unwrap();

        assert_eq!(record.status, EventLogStatus::Acked);
        assert_eq!(record.consumer.unwrap().as_str(), "root-agent");
    }

    #[test]
    /// 验证 Acked 是单调终态：同消费者重复 ACK 幂等，异消费者 ACK 和迟到 FAIL 冲突。
    fn in_memory_ack_is_idempotent_and_terminal() {
        let mut log = InMemoryEventLog::new();
        let event = event("evt-monotonic");
        let event_id = event.event_id().clone();
        let consumer = AgentId::parse("root-agent").unwrap();
        log.append(event).unwrap();

        let first = log.ack(&event_id, consumer.clone()).unwrap();
        let second = log.ack(&event_id, consumer).unwrap();
        assert_eq!(first, second);

        let ack_error = log
            .ack(&event_id, AgentId::parse("other-agent").unwrap())
            .unwrap_err();
        assert_eq!(ack_error.kind(), ErrorKind::Conflict);
        let fail_error = log
            .fail(
                &event_id,
                AgentId::parse("other-agent").unwrap(),
                EvaError::timeout("late handler failure"),
            )
            .unwrap_err();
        assert_eq!(fail_error.kind(), ErrorKind::Conflict);
        assert_eq!(log.records()[0], first);
    }

    #[test]
    /// 验证 fail 完整保留 kind、retry、provider code 和错误上下文。
    fn fail_preserves_structured_error() {
        let mut log = InMemoryEventLog::new();
        let event = event("evt-1");
        let event_id = event.event_id().clone();
        log.append(event).unwrap();

        let record = log
            .fail(
                &event_id,
                AgentId::parse("root-agent").unwrap(),
                EvaError::unavailable("handler offline"),
            )
            .unwrap();

        assert_eq!(record.status, EventLogStatus::Failed);
        assert!(record.error.unwrap().is_retryable());
    }

    #[test]
    /// 验证 replay 游标包含指定 sequence 且保持日志顺序。
    fn replay_returns_records_from_cursor() {
        let mut log = InMemoryEventLog::new();
        log.append(event("evt-1")).unwrap();
        log.append(event("evt-2")).unwrap();

        let replay = log.replay_from(2);

        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event.event_id().as_str(), "evt-2");
    }

    #[test]
    /// 验证文件日志重开后恢复事件 payload、target、metadata、序号和 watermark。
    fn filesystem_log_round_trip_survives_reopen() {
        let root = test_root("filesystem-round-trip");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut log = FileSystemEventLog::open(backend.layout()).unwrap();
            let event = targeted_event("evt-1");
            let event_id = event.event_id().clone();

            let appended = log.append(event).unwrap();
            let acked = log
                .ack(&event_id, AgentId::parse("root-agent").unwrap())
                .unwrap();

            assert_eq!(appended.sequence, 1);
            assert_eq!(acked.status, EventLogStatus::Acked);
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let log = FileSystemEventLog::open(backend.layout()).unwrap();
            let records = log.replay_from(1);

            assert_eq!(records.len(), 1);
            assert_eq!(records[0].sequence, 1);
            assert_eq!(records[0].status, EventLogStatus::Acked);
            assert_eq!(records[0].consumer.as_ref().unwrap().as_str(), "root-agent");
            assert_eq!(records[0].event.event_id().as_str(), "evt-1");
            assert_eq!(records[0].event.topic().as_str(), "/input/user");
            assert_eq!(records[0].event.payload().as_bytes(), Some(&[1, 2, 3][..]));
            assert_eq!(
                records[0].event.metadata().request_id().unwrap().as_str(),
                "req-1"
            );
            assert_eq!(
                records[0]
                    .event
                    .metadata()
                    .trace()
                    .correlation_id()
                    .unwrap()
                    .as_str(),
                "evt-root"
            );
        }
    }

    #[test]
    /// 验证 Failed 状态与结构化错误覆盖同一序号文件并在重开后保持。
    fn filesystem_log_persists_failures() {
        let root = test_root("filesystem-failure");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut log = FileSystemEventLog::open(backend.layout()).unwrap();
            let event = event("evt-1");
            let event_id = event.event_id().clone();
            log.append(event).unwrap();
            log.fail(
                &event_id,
                AgentId::parse("root-agent").unwrap(),
                EvaError::unavailable("handler offline").with_context("node", "agent-1"),
            )
            .unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let log = FileSystemEventLog::open(backend.layout()).unwrap();
            let record = &log.records()[0];
            let error = record.error.as_ref().unwrap();

            assert_eq!(record.status, EventLogStatus::Failed);
            assert_eq!(error.kind(), ErrorKind::Unavailable);
            assert!(error.is_retryable());
            assert_eq!(
                error.context().entries(),
                &[("node".to_owned(), "agent-1".to_owned())]
            );
        }
    }

    #[test]
    /// 验证共享 writer 的陈旧实例在 append 前重读磁盘，不会复用已提交 sequence。
    fn filesystem_writer_reloads_before_stale_append() {
        let root = test_root("writer-stale-append");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut first =
            FileSystemEventLog::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut stale =
            FileSystemEventLog::open_with_writer(backend.layout(), writer.clone()).unwrap();

        assert_eq!(first.append(event("evt-first")).unwrap().sequence, 1);
        assert_eq!(stale.append(event("evt-second")).unwrap().sequence, 2);
        assert_eq!(stale.watermark(), 2);

        drop(first);
        drop(stale);
        drop(writer);
        let reopened = FileSystemEventLog::open(backend.layout()).unwrap();
        assert_eq!(reopened.watermark(), 2);
        assert_eq!(reopened.records().len(), 2);
        assert_eq!(reopened.records()[0].event.event_id().as_str(), "evt-first");
        assert_eq!(
            reopened.records()[1].event.event_id().as_str(),
            "evt-second"
        );
    }

    #[test]
    /// 验证 ACK 胜出后，持同一 writer clone 的陈旧实例不能以迟到 FAIL 覆盖磁盘终态。
    fn filesystem_writer_prevents_stale_fail_from_overwriting_ack() {
        let root = test_root("writer-ack-vs-fail");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut ack_log =
            FileSystemEventLog::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let event = event("evt-raced");
        let event_id = event.event_id().clone();
        ack_log.append(event).unwrap();
        let mut stale_fail_log =
            FileSystemEventLog::open_with_writer(backend.layout(), writer.clone()).unwrap();

        let consumer = AgentId::parse("root-agent").unwrap();
        let acked = ack_log.ack(&event_id, consumer.clone()).unwrap();
        assert_eq!(ack_log.ack(&event_id, consumer).unwrap(), acked);
        let error = stale_fail_log
            .fail(
                &event_id,
                AgentId::parse("other-agent").unwrap(),
                EvaError::timeout("late handler failure"),
            )
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(stale_fail_log.records()[0], acked);

        drop(ack_log);
        drop(stale_fail_log);
        drop(writer);
        let reopened = FileSystemEventLog::open(backend.layout()).unwrap();
        assert_eq!(reopened.records()[0], acked);
    }

    /// 测试临时日志根目录所有者。
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
                "eva-storage-event-log-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
