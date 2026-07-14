//! 基于文件系统存储的持久 EventBus 实现。
//! Durable EventBus implementation backed by filesystem storage.

use crate::bus::{EventBus, EventReceipt};
use crate::dead_letter::{DeadLetterRecord, RedrivePolicy};
use eva_core::{
    AgentId, ErrorKind, EvaError, Event, EventId, EventMetadata, EventPayload, EventTarget,
    GenerationId, RequestId, Topic, TraceContext,
};
use eva_storage::{
    DurableBackendLayout, EventLog, EventLogRecord, EventLogStatus, FileSystemEventLog,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// 本模块的架构职责：持久化事件发布状态，并提供可查询、可重驱的死信记录。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable event publication and dead-letter redrive";

/// 事件目录下存放死信记录的固定子目录。
const DEAD_LETTER_DIR: &str = "dead_letters";
/// 死信记录文件的扩展名。
const DEAD_LETTER_EXTENSION: &str = "dead";
/// 当前支持的死信磁盘格式版本。
const DEAD_LETTER_VERSION: &str = "1";

/// V1.6 持久运行时路径使用的文件系统 EventBus。
/// Filesystem-backed EventBus used by V1.6 durable runtime paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableEventBus {
    /// 追加发布并更新 ack/fail 状态的持久事件日志。
    log: FileSystemEventLog,
    /// 与事件日志分离存储的死信队列。
    dead_letter_store: FileSystemDeadLetterStore,
    /// 本进程自打开后产生的发布回执；重开时不会从磁盘重建。
    receipts: Vec<EventReceipt>,
}

/// 可查询记录的文件系统死信队列。
/// Filesystem-backed dead-letter queue with queryable records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemDeadLetterStore {
    /// 每个事件一份死信文件的根目录。
    root: PathBuf,
    /// 从磁盘加载并按事件标识排序的当前内存视图。
    records: Vec<DeadLetterRecord>,
}

impl DurableEventBus {
    /// 以读写模式打开事件日志和死信存储，必要时创建目录。
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Ok(Self {
            log: FileSystemEventLog::open(layout)?,
            dead_letter_store: FileSystemDeadLetterStore::open(layout)?,
            receipts: Vec::new(),
        })
    }

    /// 以只读模式打开持久状态；缺失死信目录视为空队列。
    pub fn open_read_only(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Ok(Self {
            log: FileSystemEventLog::open_read_only(layout)?,
            dead_letter_store: FileSystemDeadLetterStore::open_read_only(layout)?,
            receipts: Vec::new(),
        })
    }

    /// 借用持久事件日志以执行查询或重放。
    pub fn log(&self) -> &FileSystemEventLog {
        &self.log
    }

    /// 返回本进程本次打开期间产生的发布回执。
    pub fn receipts(&self) -> &[EventReceipt] {
        &self.receipts
    }

    /// 返回当前按事件标识排序的死信快照。
    pub fn dead_letters(&self) -> &[DeadLetterRecord] {
        self.dead_letter_store.records()
    }

    /// 查询下一次尝试时间不晚于指定毫秒值的死信。
    pub fn due_dead_letters(&self, ready_at_ms: u64) -> Vec<DeadLetterRecord> {
        self.dead_letters()
            .iter()
            .filter(|record| record.redrive.next_attempt_after_ms <= ready_at_ms)
            .cloned()
            .collect()
    }

    /// 按事件标识查找最新加载的事件日志记录。
    pub fn event_log_record(&self, event_id: &EventId) -> Option<&EventLogRecord> {
        self.log
            .records()
            .iter()
            .find(|record| record.event.event_id() == event_id)
    }

    /// 返回指定事件的持久日志状态。
    pub fn event_log_status(&self, event_id: &EventId) -> Option<EventLogStatus> {
        self.event_log_record(event_id).map(|record| record.status)
    }

    /// 按约定的 replay 后缀查找原事件最近一次重驱记录。
    pub fn latest_replay_record(&self, original_event_id: &EventId) -> Option<&EventLogRecord> {
        let replay_prefix = format!("{}:replay-", original_event_id.as_str());
        self.log
            .records()
            .iter()
            .rev()
            .find(|record| record.event.event_id().as_str().starts_with(&replay_prefix))
    }

    /// 将事件及失败原因持久化为唯一死信记录。
    pub fn dead_letter(
        &mut self,
        event: Event,
        reason: EvaError,
    ) -> Result<DeadLetterRecord, EvaError> {
        self.dead_letter_store.push(event, reason)
    }

    /// 更新指定死信的退避策略，并在修改内存视图前持久化。
    pub fn set_dead_letter_redrive_policy(
        &mut self,
        event_id: &EventId,
        redrive: RedrivePolicy,
    ) -> Result<DeadLetterRecord, EvaError> {
        self.dead_letter_store.set_redrive_policy(event_id, redrive)
    }

    /// 为当前全部死信生成新的子事件并逐一发布。
    ///
    /// 每个死信的 replay 计数会在对应发布前单独持久化；若中途发布失败，之前项目
    /// 已经提交，后续项目不会执行，因此本方法不具备批量事务语义。重复调用会产生
    /// 新 replay id，而不会复用旧事件标识。
    pub fn replay_dead_letters(&mut self) -> Result<Vec<EventReceipt>, EvaError> {
        let events = self.dead_letter_store.replay_all_for_publish()?;
        events
            .into_iter()
            .map(|event| self.publish(event))
            .collect()
    }

    /// 重驱指定死信并把生成的子事件写入持久事件日志。
    pub fn redrive_dead_letter(&mut self, event_id: &EventId) -> Result<EventReceipt, EvaError> {
        let event = self.dead_letter_store.redrive_for_publish(event_id)?;
        self.publish(event)
    }
}

impl EventBus for DurableEventBus {
    /// 先追加持久日志，再创建并保存进程内发布回执。
    ///
    /// 日志写入失败时不会产生回执，避免内存状态宣称未持久化事件已发布。
    fn publish(&mut self, event: Event) -> Result<EventReceipt, EvaError> {
        let record = self.log.append(event)?;
        let receipt = EventReceipt::from_record(&record);
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    /// 在持久日志中把事件标记为指定消费者已确认。
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        self.log.ack(event_id, consumer)
    }

    /// 在持久日志中记录消费者失败及结构化错误。
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        self.log.fail(event_id, consumer, error)
    }
}

impl FileSystemDeadLetterStore {
    /// 从后端布局的标准死信目录以读写模式打开存储。
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::open_dir_with_mode(layout.event_dir.join(DEAD_LETTER_DIR), true)
    }

    /// 从标准死信目录只读加载；目录不存在时返回空存储。
    pub fn open_read_only(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::open_dir_with_mode(layout.event_dir.join(DEAD_LETTER_DIR), false)
    }

    /// 从显式目录以读写模式打开死信存储。
    pub fn open_dir(root: impl Into<PathBuf>) -> Result<Self, EvaError> {
        Self::open_dir_with_mode(root, true)
    }

    /// 按模式处理目录存在性，加载、排序并验证所有记录。
    ///
    /// 任一匹配扩展名的记录损坏都会阻止整个存储打开，采用失败关闭而非静默跳过；
    /// 只读模式下目录不存在视为空，但路径存在且不是目录仍返回冲突。
    fn open_dir_with_mode(
        root: impl Into<PathBuf>,
        create_if_missing: bool,
    ) -> Result<Self, EvaError> {
        let root = root.into();
        if root.exists() {
            if !root.is_dir() {
                return Err(
                    EvaError::conflict("durable dead-letter path is not a directory")
                        .with_context("path", root.display().to_string()),
                );
            }
        } else if create_if_missing {
            fs::create_dir_all(&root).map_err(|error| {
                EvaError::internal("failed to create durable dead-letter directory")
                    .with_context("path", root.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        } else {
            return Ok(Self {
                root,
                records: Vec::new(),
            });
        }

        let mut records = load_dead_letters(&root)?;
        records.sort_by(|left, right| left.event_id().cmp(right.event_id()));
        validate_records(&records)?;

        Ok(Self { root, records })
    }

    /// 返回当前已验证死信记录快照。
    pub fn records(&self) -> &[DeadLetterRecord] {
        &self.records
    }

    /// 为新事件创建唯一死信记录，先持久化再更新内存索引。
    ///
    /// 重复事件标识被拒绝；磁盘写入失败时内存保持不变，避免同一进程看到并不存在
    /// 于持久层的死信。
    pub fn push(&mut self, event: Event, reason: EvaError) -> Result<DeadLetterRecord, EvaError> {
        if self
            .records
            .iter()
            .any(|record| record.event_id() == event.event_id())
        {
            return Err(EvaError::conflict("dead-letter event already exists")
                .with_context("event_id", event.event_id().as_str()));
        }

        let record = DeadLetterRecord::new(event, reason);
        self.persist(&record)?;
        self.records.push(record.clone());
        self.records
            .sort_by(|left, right| left.event_id().cmp(right.event_id()));
        Ok(record)
    }

    /// 更新指定记录的退避参数，先覆盖磁盘文件再替换内存副本。
    pub fn set_redrive_policy(
        &mut self,
        event_id: &EventId,
        redrive: RedrivePolicy,
    ) -> Result<DeadLetterRecord, EvaError> {
        let index = self
            .records
            .iter()
            .position(|record| record.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("dead-letter event does not exist")
                    .with_context("event_id", event_id.as_str())
            })?;
        let mut record = self.records[index].clone();
        record.redrive = redrive;
        self.persist(&record)?;
        self.records[index] = record.clone();
        Ok(record)
    }

    /// 以稳定的当前记录列表为基准，为每个死信准备一次重驱。
    pub fn replay_all_for_publish(&mut self) -> Result<Vec<Event>, EvaError> {
        let event_ids = self
            .records
            .iter()
            .map(|record| record.event_id().clone())
            .collect::<Vec<_>>();
        event_ids
            .iter()
            .map(|event_id| self.redrive_for_publish(event_id))
            .collect()
    }

    /// 递增 replay 计数、计算下一尝试时间并持久化后返回子事件。
    ///
    /// 计数先落盘再交给 EventBus 发布，因此发布失败也会消耗一次重驱序号；这保证
    /// 重试不会复用相同事件 id，牺牲“仅成功才计数”以换取幂等标识和崩溃可恢复性。
    /// 退避乘法使用饱和计算，极端计数不会整数溢出回绕为过早重试。
    pub fn redrive_for_publish(&mut self, event_id: &EventId) -> Result<Event, EvaError> {
        let index = self
            .records
            .iter()
            .position(|record| record.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("dead-letter event does not exist")
                    .with_context("event_id", event_id.as_str())
            })?;
        let mut record = self.records[index].clone();
        record.replay_count += 1;
        record.redrive.next_attempt_after_ms = record
            .redrive
            .retry_delay_ms
            .saturating_mul(record.replay_count as u64);
        let event = replay_event_for_publish(&record)?;
        self.persist(&record)?;
        self.records[index] = record;
        Ok(event)
    }

    /// 将事件标识十六进制编码为不会逃逸目录的记录路径。
    fn record_path(&self, event_id: &EventId) -> PathBuf {
        self.root.join(format!(
            "{}.{}",
            encode_path_segment(event_id.as_str()),
            DEAD_LETTER_EXTENSION
        ))
    }

    /// 以当前 v1 行式格式覆盖写入一条死信记录。
    ///
    /// 单文件写入不使用临时文件或 fsync，不承诺崩溃原子性；读取端会严格解析并在
    /// 截断/损坏时拒绝打开，调用方必须把写入错误视为未提交。
    fn persist(&self, record: &DeadLetterRecord) -> Result<(), EvaError> {
        let path = self.record_path(record.event_id());
        fs::write(&path, dead_letter_to_storage(record)).map_err(|error| {
            EvaError::internal("failed to write durable dead-letter record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })
    }
}

/// 从原死信创建保留请求/代际/关联链的重驱子事件。
///
/// 新 id 由原 id 与已持久化 replay_count 派生，causation 指向原事件，避免重驱被
/// 误认为首次投递。
fn replay_event_for_publish(record: &DeadLetterRecord) -> Result<Event, EvaError> {
    let replay_id = EventId::parse(&format!(
        "{}:replay-{}",
        record.event.event_id().as_str(),
        record.replay_count
    ))?;
    Ok(record
        .event
        .child_event(
            replay_id,
            record.event.topic().clone(),
            record.event.payload().clone(),
        )
        .with_target(record.event.target().clone()))
}

/// 读取所有 `.dead` 文件并严格解析，忽略其他目录内容。
fn load_dead_letters(root: &Path) -> Result<Vec<DeadLetterRecord>, EvaError> {
    let mut records = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        EvaError::internal("failed to read durable dead-letter directory")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read durable dead-letter entry")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(DEAD_LETTER_EXTENSION) {
            continue;
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::conflict("failed to read durable dead-letter record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        records.push(
            dead_letter_from_storage(&data)
                .map_err(|error| error.with_context("path", path.display().to_string()))?,
        );
    }
    Ok(records)
}

/// 验证加载后的记录不存在重复事件标识。
fn validate_records(records: &[DeadLetterRecord]) -> Result<(), EvaError> {
    let mut event_ids = BTreeSet::new();
    for record in records {
        if !event_ids.insert(record.event_id().clone()) {
            return Err(
                EvaError::conflict("durable dead-letter event id is duplicated")
                    .with_context("event_id", record.event_id().as_str()),
            );
        }
    }
    Ok(())
}

/// 以固定字段顺序编码一条 v1 死信记录。
fn dead_letter_to_storage(record: &DeadLetterRecord) -> String {
    let mut data = String::new();
    push_field(&mut data, "version", DEAD_LETTER_VERSION);
    push_event_fields(&mut data, &record.event);
    push_error_fields(&mut data, "reason", Some(&record.reason));
    push_field(&mut data, "replay_count", &record.replay_count.to_string());
    push_field(
        &mut data,
        "retry_delay_ms",
        &record.redrive.retry_delay_ms.to_string(),
    );
    push_field(
        &mut data,
        "next_attempt_after_ms",
        &record.redrive.next_attempt_after_ms.to_string(),
    );
    data
}

/// 从 v1 字段集合恢复死信，并兼容早期缺少退避字段的记录。
///
/// `retry_delay_ms` 和 `next_attempt_after_ms` 缺失时默认为零，保持 v1 初始格式可读；
/// 版本、事件、原因和 replay_count 仍是必填项，损坏值不会被宽松接受。
fn dead_letter_from_storage(data: &str) -> Result<DeadLetterRecord, EvaError> {
    let fields = parse_fields(data)?;
    let version = require_field(&fields, "version")?;
    if version != DEAD_LETTER_VERSION {
        return Err(
            EvaError::conflict("dead-letter record version is unsupported")
                .with_context("version", version),
        );
    }

    let event = event_from_fields(&fields)?;
    let reason = error_from_fields(&fields, "reason")?
        .ok_or_else(|| EvaError::conflict("dead-letter reason is missing"))?;
    let replay_count = parse_usize(require_field(&fields, "replay_count")?, "replay_count")?;
    let retry_delay_ms = optional_u64(&fields, "retry_delay_ms")?.unwrap_or(0);
    let next_attempt_after_ms = optional_u64(&fields, "next_attempt_after_ms")?.unwrap_or(0);

    Ok(DeadLetterRecord {
        event,
        reason,
        replay_count,
        redrive: RedrivePolicy {
            retry_delay_ms,
            next_attempt_after_ms,
        },
    })
}

/// 将事件标识、路由目标、载荷、元数据和可选错误编码到字段缓冲区。
///
/// 自由文本一律十六进制编码，避免换行和 `=` 破坏行式格式；时间戳拆成秒和纳秒，
/// 关联/因果标识独立保存以便重开后完整恢复追踪链。
fn push_event_fields(data: &mut String, event: &Event) {
    push_encoded_string(data, "event_id", event.event_id().as_str());
    push_encoded_string(data, "topic", event.topic().as_str());
    match event.target() {
        EventTarget::Broadcast => {
            push_field(data, "target_kind", "broadcast");
            push_field(data, "target_value", "");
        }
        EventTarget::Agent(value) => {
            push_field(data, "target_kind", "agent");
            push_encoded_string(data, "target_value", value.as_str());
        }
        EventTarget::Capability(value) => {
            push_field(data, "target_kind", "capability");
            push_encoded_string(data, "target_value", value.as_str());
        }
        EventTarget::Adapter(value) => {
            push_field(data, "target_kind", "adapter");
            push_encoded_string(data, "target_value", value.as_str());
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

/// 从字段集合严格重建事件、目标、载荷和元数据。
///
/// 未知 target/payload kind、无效标识或错误编码都会失败；不会以默认值掩盖核心事件
/// 内容损坏。仅新增的可选元数据字段可通过专用兼容读取器缺省。
fn event_from_fields(fields: &BTreeMap<String, String>) -> Result<Event, EvaError> {
    let event_id = EventId::parse(&decoded_required_string(fields, "event_id")?)?;
    let topic = Topic::parse(&decoded_required_string(fields, "topic")?)?;
    let target = match require_field(fields, "target_kind")? {
        "broadcast" => EventTarget::Broadcast,
        "agent" => EventTarget::Agent(AgentId::parse(&decoded_required_string(
            fields,
            "target_value",
        )?)?),
        "capability" => EventTarget::Capability(eva_core::CapabilityName::parse(
            &decoded_required_string(fields, "target_value")?,
        )?),
        "adapter" => EventTarget::Adapter(eva_core::AdapterId::parse(&decoded_required_string(
            fields,
            "target_value",
        )?)?),
        value => {
            return Err(EvaError::conflict("dead-letter target kind is invalid")
                .with_context("target_kind", value))
        }
    };
    let payload = match require_field(fields, "payload_kind")? {
        "empty" => EventPayload::empty(),
        "text" => EventPayload::text(decoded_required_string(fields, "payload_value")?),
        "bytes" => EventPayload::bytes(hex_decode(require_field(fields, "payload_value")?)?),
        value => {
            return Err(EvaError::conflict("dead-letter payload kind is invalid")
                .with_context("payload_kind", value))
        }
    };
    Ok(Event::new(event_id, topic, payload)
        .with_target(target)
        .with_metadata(event_metadata_from_fields(fields)?))
}

/// 将可选结构化错误编码为带前缀的 kind/message/retryable 字段。
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
    } else {
        push_field(data, &format!("{prefix}_kind"), "");
        push_field(data, &format!("{prefix}_message"), "");
        push_field(data, &format!("{prefix}_retryable"), "");
    }
}

/// 从带前缀字段恢复可选结构化错误。
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
    Ok(Some(EvaError::new(kind, message).with_retryable(retryable)))
}

/// 将稳定磁盘错误类别字符串映射回 `ErrorKind`。
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
        _ => Err(EvaError::conflict("dead-letter error kind is invalid")
            .with_context("error_kind", value)),
    }
}

/// 解析行式字段映射。
///
/// 每行必须包含 `=` 且键已裁剪；当前实现后出现的重复键会覆盖前值，这是 v1 解析
/// 语义，写入器始终生成唯一键。所有自由文本值在更高层按十六进制解码。
fn parse_fields(data: &str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::conflict("dead-letter record field is invalid"));
        };
        if key.trim().is_empty() || key.trim() != key {
            return Err(
                EvaError::conflict("dead-letter record field key is invalid")
                    .with_context("field", key),
            );
        }
        fields.insert(key.to_owned(), value.to_owned());
    }
    Ok(fields)
}

/// 向行式记录追加一个原始键值字段。
fn push_field(data: &mut String, key: &str, value: &str) {
    data.push_str(key);
    data.push('=');
    data.push_str(value);
    data.push('\n');
}

/// 将 UTF-8 文本按字节十六进制编码后追加字段。
fn push_encoded_string(data: &mut String, key: &str, value: &str) {
    push_field(data, key, &hex_encode(value.as_bytes()));
}

/// 编码可选字符串；缺失值使用空字段表示。
fn push_optional_string(data: &mut String, key: &str, value: Option<&str>) {
    match value {
        Some(value) => push_encoded_string(data, key, value),
        None => push_field(data, key, ""),
    }
}

/// 读取必填字段，缺失时返回包含字段名的冲突错误。
fn require_field<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, EvaError> {
    fields.get(key).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("dead-letter record field is missing").with_context("field", key)
    })
}

/// 读取并十六进制解码必填 UTF-8 字符串。
fn decoded_required_string(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<String, EvaError> {
    decode_string(require_field(fields, key)?).map_err(|error| error.with_context("field", key))
}

/// 读取可选十六进制 UTF-8 字符串；缺失或空字段均为 `None`。
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

/// 从兼容字段恢复事件时间、请求/代际标识及追踪上下文。
///
/// 旧记录缺少新增元数据时使用 Unix epoch 和空标识；纳秒必须小于十亿。存在但非法
/// 的标识仍失败关闭，兼容性只覆盖字段缺失，不覆盖损坏数据。
fn event_metadata_from_fields(
    fields: &BTreeMap<String, String>,
) -> Result<EventMetadata, EvaError> {
    let created_at_secs = optional_u64(fields, "created_at_secs")?.unwrap_or(0);
    let created_at_nanos = optional_u32(fields, "created_at_nanos")?.unwrap_or(0);
    if created_at_nanos >= 1_000_000_000 {
        return Err(
            EvaError::conflict("dead-letter created_at_nanos is invalid")
                .with_context("field", "created_at_nanos"),
        );
    }

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
    Ok(metadata)
}

/// 解析可选 `u64` 字段，缺失或空值返回 `None`。
fn optional_u64(fields: &BTreeMap<String, String>, key: &str) -> Result<Option<u64>, EvaError> {
    match fields.get(key).map(String::as_str) {
        Some(value) if !value.is_empty() => parse_u64(value, key).map(Some),
        _ => Ok(None),
    }
}

/// 解析平台宽度的非负计数字段。
fn parse_usize(value: &str, field: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::conflict("dead-letter numeric field is invalid").with_context("field", field)
    })
}

/// 解析无符号 64 位数值字段。
fn parse_u64(value: &str, field: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("dead-letter numeric field is invalid").with_context("field", field)
    })
}

/// 解析可选 `u32` 字段，缺失或空值返回 `None`。
fn optional_u32(fields: &BTreeMap<String, String>, key: &str) -> Result<Option<u32>, EvaError> {
    match fields.get(key).map(String::as_str) {
        Some(value) if !value.is_empty() => value.parse::<u32>().map(Some).map_err(|_| {
            EvaError::conflict("dead-letter numeric field is invalid").with_context("field", key)
        }),
        _ => Ok(None),
    }
}

/// 严格解析 `true` 或 `false` 布尔字段。
fn parse_bool(value: &str, field: &str) -> Result<bool, EvaError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => {
            Err(EvaError::conflict("dead-letter boolean field is invalid")
                .with_context("field", field))
        }
    }
}

/// 十六进制解码并验证结果是 UTF-8 文本。
fn decode_string(value: &str) -> Result<String, EvaError> {
    String::from_utf8(hex_decode(value)?)
        .map_err(|_| EvaError::conflict("dead-letter string field is not utf-8"))
}

/// 将任意事件标识编码为安全的单一路径段。
fn encode_path_segment(value: &str) -> String {
    hex_encode(value.as_bytes())
}

/// 将任意字节编码为小写十六进制文本。
fn hex_encode(bytes: &[u8]) -> String {
    /// 十六进制半字节到 ASCII 的查找表。
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// 将偶数长度的十六进制文本解码为原始字节。
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

/// 将一个 ASCII 十六进制字符解析为半字节。
fn hex_value(value: u8) -> Result<u8, EvaError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(EvaError::conflict("hex field contains invalid digit")),
    }
}

#[cfg(test)]
/// 持久发布、状态重开、死信重驱和旧格式兼容测试。
mod tests {
    use super::*;
    use eva_core::{EventPayload, Topic};
    use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// 构造具有固定主题和文本载荷的测试事件。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    /// 构造包含时间、请求、代际和追踪链的测试事件。
    fn traced_event(id: &str) -> Event {
        event(id).with_metadata(
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
    /// 验证 publish、ack 和 fail 状态在重开后仍可查询。
    fn durable_bus_persists_publish_ack_fail_across_reopen() {
        let root = test_root("publish-ack-fail");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let first = event("evt-1");
            let second = event("evt-2");

            bus.publish(first.clone()).unwrap();
            bus.publish(second.clone()).unwrap();
            bus.ack(first.event_id(), AgentId::parse("agent-a").unwrap())
                .unwrap();
            bus.fail(
                second.event_id(),
                AgentId::parse("agent-b").unwrap(),
                EvaError::timeout("handler timeout"),
            )
            .unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let bus = DurableEventBus::open(backend.layout()).unwrap();
            let records = bus.log().replay_from(1);

            assert_eq!(records.len(), 2);
            assert_eq!(records[0].status, eva_storage::EventLogStatus::Acked);
            assert_eq!(records[1].status, eva_storage::EventLogStatus::Failed);
            assert_eq!(
                records[1].error.as_ref().unwrap().kind(),
                ErrorKind::Timeout
            );
        }
    }

    #[test]
    /// 验证死信重开后可查询，且重驱事件保留追踪上下文。
    fn dead_letter_stays_queryable_after_reopen_and_redrives() {
        let root = test_root("dead-letter-redrive");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = traced_event("evt-1");
            bus.publish(event.clone()).unwrap();
            bus.dead_letter(event, EvaError::not_found("no route"))
                .unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();

            assert_eq!(bus.dead_letters().len(), 1);
            assert_eq!(bus.dead_letters()[0].event_id().as_str(), "evt-1");
            assert_eq!(
                bus.dead_letters()[0]
                    .event
                    .metadata()
                    .request_id()
                    .unwrap()
                    .as_str(),
                "req-1"
            );
            assert_eq!(
                bus.dead_letters()[0]
                    .event
                    .metadata()
                    .trace()
                    .correlation_id()
                    .unwrap()
                    .as_str(),
                "evt-root"
            );
            assert_eq!(bus.dead_letters()[0].redrive.retry_delay_ms, 0);
            assert_eq!(bus.dead_letters()[0].redrive.next_attempt_after_ms, 0);

            let receipts = bus.replay_dead_letters().unwrap();

            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].event_id.as_str(), "evt-1:replay-1");
            assert_eq!(bus.dead_letters()[0].replay_count, 1);
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let bus = DurableEventBus::open(backend.layout()).unwrap();
            let records = bus.log().replay_from(1);

            assert_eq!(bus.dead_letters()[0].replay_count, 1);
            assert_eq!(records.len(), 2);
            assert_eq!(
                records[1]
                    .event
                    .metadata()
                    .trace()
                    .correlation_id()
                    .unwrap()
                    .as_str(),
                "evt-root"
            );
            assert_eq!(
                records[1]
                    .event
                    .metadata()
                    .trace()
                    .causation_id()
                    .unwrap()
                    .as_str(),
                "evt-1"
            );
        }
    }

    #[test]
    /// 验证按退避时间查询到期死信及最近重驱状态。
    fn due_dead_letters_and_latest_replay_status_are_queryable() {
        let root = test_root("due-redrive-query");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut bus = DurableEventBus::open(backend.layout()).unwrap();
        let due = event("evt-due");
        let future = event("evt-future");

        bus.publish(due.clone()).unwrap();
        bus.dead_letter(due.clone(), EvaError::timeout("handler timeout"))
            .unwrap();
        bus.publish(future.clone()).unwrap();
        bus.dead_letter(future.clone(), EvaError::timeout("handler timeout"))
            .unwrap();
        bus.set_dead_letter_redrive_policy(
            future.event_id(),
            RedrivePolicy {
                retry_delay_ms: 1_000,
                next_attempt_after_ms: 5_000,
            },
        )
        .unwrap();

        let due_records = bus.due_dead_letters(1_000);
        assert_eq!(due_records.len(), 1);
        assert_eq!(due_records[0].event_id().as_str(), "evt-due");
        assert!(bus.latest_replay_record(due.event_id()).is_none());

        let receipt = bus.redrive_dead_letter(due.event_id()).unwrap();
        bus.ack(&receipt.event_id, AgentId::parse("agent-a").unwrap())
            .unwrap();

        let replay = bus.latest_replay_record(due.event_id()).unwrap();
        assert_eq!(replay.event.event_id().as_str(), "evt-due:replay-1");
        assert_eq!(replay.status, eva_storage::EventLogStatus::Acked);
        assert_eq!(
            bus.event_log_status(&receipt.event_id),
            Some(eva_storage::EventLogStatus::Acked)
        );
    }

    #[test]
    /// 验证旧 v1 记录缺少退避字段时按零值兼容读取。
    fn dead_letter_backoff_fields_are_optional_for_compatibility() {
        let root = test_root("compat-backoff");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let store_dir = backend.layout().event_dir.join(DEAD_LETTER_DIR);
        fs::create_dir_all(&store_dir).unwrap();
        fs::write(
            store_dir.join(format!(
                "{}.{}",
                encode_path_segment("evt-legacy"),
                DEAD_LETTER_EXTENSION
            )),
            "version=1\n\
event_id=6576742d6c6567616379\n\
topic=2f696e7075742f75736572\n\
target_kind=broadcast\n\
target_value=\n\
payload_kind=empty\n\
payload_value=\n\
reason_kind=not_found\n\
reason_message=6e6f20726f757465\n\
reason_retryable=false\n\
replay_count=0\n",
        )
        .unwrap();

        let store = FileSystemDeadLetterStore::open(backend.layout()).unwrap();

        assert_eq!(store.records()[0].redrive.retry_delay_ms, 0);
        assert_eq!(store.records()[0].redrive.next_attempt_after_ms, 0);
    }

    /// 测试期间拥有并在析构时清理临时持久目录。
    struct TestRoot {
        /// 临时目录路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 借用临时目录路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 尽力递归删除测试目录，不掩盖原测试结果。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 创建进程和时间戳隔离的临时持久目录拥有者。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-eventbus-durable-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
