//! 基于 durable state 目录的 Memory 与可重建 Knowledge 文件存储。
//! Durable memory and knowledge stores backed by the durable state directory.

use crate::knowledge_service::{
    InMemoryKnowledgeService, KnowledgeId, KnowledgeItem, KnowledgeSource,
};
use crate::memory_service::{
    InMemoryMemoryService, MemoryCompression, MemoryRecord, MemoryRetention, MemoryVisibility,
    MemoryWrite,
};
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, AuditSink, TraceFields};
use eva_storage::{DurableBackendLayout, StateVersion};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本模块的架构职责：持久化 Memory，并从独立 Knowledge 文件安全重建检索索引。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable memory and rebuildable knowledge persistence";

#[derive(Debug, Clone, PartialEq, Eq)]
/// 每个复合 Memory 键一份文件的持久存储。
pub struct FileSystemMemoryStore {
    /// Memory 记录、索引锁和 GC 检查点的共同根目录。
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 每个 KnowledgeId 一份文件的可重建存储。
pub struct FileSystemKnowledgeStore {
    /// Knowledge 文件、索引锁和重建检查点的共同根目录。
    root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
/// 通过排他锁文件保护索引读写和维护的 RAII Guard。
pub struct DurableIndexLockGuard {
    /// Guard 析构时删除的锁文件路径。
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 一次 TTL 物理压缩维护的统计和恢复证据。
pub struct MemoryCompactionReport {
    /// 维护完成后的状态。
    pub status: String,
    /// 本次持有的索引锁路径。
    pub lock_path: String,
    /// 两阶段 GC 检查点路径。
    pub checkpoint_path: String,
    /// 扫描到的记录数。
    pub scanned_records: usize,
    /// 尚未到期而保留的记录数。
    pub records_kept: usize,
    /// 已物理删除的到期记录数。
    pub expired_removed: usize,
    /// 是否从过期 started 检查点恢复维护。
    pub recovered_checkpoint: bool,
    /// 锁、扫描、删除和检查点审计记录。
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Knowledge 索引重建检查点的结果证据。
pub struct KnowledgeRebuildCheckpointReport {
    /// 重建完成后的状态。
    pub status: String,
    /// 本次持有的索引锁路径。
    pub lock_path: String,
    /// 两阶段重建检查点路径。
    pub checkpoint_path: String,
    /// 从文件成功加入索引的条目数。
    pub items_indexed: usize,
    /// 是否恢复了中断维护遗留状态。
    pub recovered_checkpoint: bool,
    /// 锁、重建和检查点审计记录。
    pub audit: Vec<String>,
}

/// Memory 与 Knowledge 存储共用的排他索引锁文件名。
const INDEX_LOCK_FILE: &str = "index.lock";
/// Memory TTL GC 检查点文件名。
const MEMORY_GC_CHECKPOINT_FILE: &str = "memory-gc.checkpoint";
/// Knowledge 重建检查点文件名。
const KNOWLEDGE_REBUILD_CHECKPOINT_FILE: &str = "knowledge-rebuild.checkpoint";
/// started 检查点和锁可被视为中断状态前的最短毫秒数。
const INDEX_LOCK_STALE_AFTER_MS: u128 = 60_000;

impl FileSystemMemoryStore {
    /// 创建指向指定 Memory 根目录的存储。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// 从 durable backend 标准 state 布局构造 Memory 存储。
    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self::new(layout.state_dir.join("memory"))
    }

    /// 以跨进程排他创建语义尝试获取索引锁。
    pub fn try_acquire_index_lock(&self) -> Result<DurableIndexLockGuard, EvaError> {
        DurableIndexLockGuard::acquire(&self.root)
    }

    /// 在同一锁内加载最新索引、计算下一版本并原子替换记录文件。
    ///
    /// 锁覆盖“读旧版本 -> 生成新版本 -> 写盘”全过程，防止并发写丢失版本递增。文件
    /// 提交失败时内存服务只是临时值，不会向调用方返回成功记录。
    pub fn write(&mut self, write: MemoryWrite) -> Result<MemoryRecord, EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        let mut service = self.load_unlocked()?;
        let record = service.write(write)?;
        self.write_record_unlocked(&record)?;
        Ok(record)
    }

    /// 在索引锁内直接持久化一份完整记录。
    pub fn write_record(&mut self, record: &MemoryRecord) -> Result<(), EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.write_record_unlocked(record)
    }

    /// 在索引锁内从全部 `.memory` 文件重建进程内服务。
    pub fn load(&self) -> Result<InMemoryMemoryService, EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.load_unlocked()
    }

    /// 删除指定时间已到期的记录并写入两阶段维护检查点。
    ///
    /// 开始前仅在严格确认旧 started 检查点及其锁均过期时恢复遗留锁；获取新锁后先
    /// 写 started，再逐文件严格解析并删除到期项，最后写 completed 和审计。删除是逐
    /// 文件提交，不是目录事务；中途失败会留下 started 检查点和剩余记录，下一次维护
    /// 可在超时后恢复。审计 Sink 失败发生在 completed 后，因此不会回滚已完成 GC。
    pub fn compact_expired_at(
        &mut self,
        now_ms: u128,
        audit_sink: &mut impl AuditSink,
        trace: &TraceFields,
    ) -> Result<MemoryCompactionReport, EvaError> {
        let recovered_checkpoint = recover_interrupted_maintenance_lock(
            &self.root,
            &self.memory_checkpoint_path(),
            now_ms,
        )?;
        let lock = self.try_acquire_index_lock()?;
        write_memory_checkpoint(
            &self.memory_checkpoint_path(),
            now_ms,
            "started",
            0,
            0,
            0,
            recovered_checkpoint,
        )?;

        let mut scanned_records = 0;
        let mut records_kept = 0;
        let mut expired_removed = 0;
        for path in list_files_with_extension(&self.root, "memory")? {
            let data = fs::read_to_string(&path).map_err(|error| {
                filesystem_error("failed to read durable memory record", &path, error)
            })?;
            let record = memory_record_from_storage(&data)
                .map_err(|error| error.with_context("path", path.display().to_string()))?;
            scanned_records += 1;
            if record.is_expired_at(now_ms) {
                fs::remove_file(&path).map_err(|error| {
                    filesystem_error(
                        "failed to remove expired durable memory record",
                        &path,
                        error,
                    )
                })?;
                expired_removed += 1;
            } else {
                records_kept += 1;
            }
        }

        let report = MemoryCompactionReport {
            status: "ready".to_owned(),
            lock_path: display_path(lock.path()),
            checkpoint_path: display_path(&self.memory_checkpoint_path()),
            scanned_records,
            records_kept,
            expired_removed,
            recovered_checkpoint,
            audit: vec![
                "memory.gc:index_lock_acquired".to_owned(),
                format!("memory.gc:scanned:{scanned_records}"),
                format!("memory.gc:expired_removed:{expired_removed}"),
                "memory.gc:checkpoint_written".to_owned(),
            ],
        };
        write_memory_checkpoint(
            &self.memory_checkpoint_path(),
            now_ms,
            "completed",
            scanned_records,
            records_kept,
            expired_removed,
            recovered_checkpoint,
        )?;
        audit_sink.record(
            AuditEvent::new(
                AuditAction::MemoryMaintenance,
                AuditOutcome::Ok,
                trace.clone(),
            )
            .with_message("durable memory TTL GC completed")
            .with_field("store", display_path(&self.root))
            .with_field("scanned_records", scanned_records.to_string())
            .with_field("expired_removed", expired_removed.to_string())
            .with_field(
                "checkpoint_path",
                display_path(&self.memory_checkpoint_path()),
            ),
        )?;
        Ok(report)
    }

    /// 假定调用方持锁，原子替换单条 Memory 文件。
    fn write_record_unlocked(&self, record: &MemoryRecord) -> Result<(), EvaError> {
        let path = memory_record_path(&self.root, record)?;
        write_text_atomically(
            &path,
            &memory_record_to_storage(record),
            "failed to write durable memory record",
        )
    }

    /// 假定调用方持锁，从排序文件严格重建 Memory 索引。
    ///
    /// 任一损坏记录阻止整个加载，避免以缺项索引继续运行；同一复合键后读文件覆盖
    /// 前值的可能性由文件命名的一键一文件不变量消除。
    fn load_unlocked(&self) -> Result<InMemoryMemoryService, EvaError> {
        let mut service = InMemoryMemoryService::new();
        for path in list_files_with_extension(&self.root, "memory")? {
            let data = fs::read_to_string(&path).map_err(|error| {
                filesystem_error("failed to read durable memory record", &path, error)
            })?;
            let record = memory_record_from_storage(&data)
                .map_err(|error| error.with_context("path", path.display().to_string()))?;
            service.insert_record(record)?;
        }
        Ok(service)
    }

    /// 返回 Memory 存储根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 返回 Memory GC 检查点路径。
    pub fn memory_checkpoint_path(&self) -> PathBuf {
        self.root.join(MEMORY_GC_CHECKPOINT_FILE)
    }
}

impl FileSystemKnowledgeStore {
    /// 创建指向指定 Knowledge 根目录的存储。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// 从 durable backend 标准 state 布局构造 Knowledge 存储。
    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self::new(layout.state_dir.join("knowledge"))
    }

    /// 尝试获取 Knowledge 索引排他锁。
    pub fn try_acquire_index_lock(&self) -> Result<DurableIndexLockGuard, EvaError> {
        DurableIndexLockGuard::acquire(&self.root)
    }

    /// 在锁内原子替换单个 Knowledge 文件。
    pub fn write_item(&mut self, item: &KnowledgeItem) -> Result<(), EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.write_item_unlocked(item)
    }

    /// 在锁内从全部独立条目文件重建检索索引。
    pub fn load_index(&self) -> Result<InMemoryKnowledgeService, EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.load_index_unlocked()
    }

    /// 执行一次带 started/completed 检查点的索引重建演练。
    ///
    /// Knowledge 文件是真实来源，内存索引可随时重建；重复 id 或任一损坏文件会使
    /// 重建失败并保留 started 检查点。completed 在审计之前落盘，因此审计 Sink 失败
    /// 不会把已验证索引错误标记为未完成。
    pub fn rebuild_checkpoint(
        &mut self,
        audit_sink: &mut impl AuditSink,
        trace: &TraceFields,
    ) -> Result<KnowledgeRebuildCheckpointReport, EvaError> {
        let now_ms = current_time_ms();
        let recovered_checkpoint = recover_interrupted_maintenance_lock(
            &self.root,
            &self.rebuild_checkpoint_path(),
            now_ms,
        )?;
        let lock = self.try_acquire_index_lock()?;
        write_knowledge_checkpoint(
            &self.rebuild_checkpoint_path(),
            "started",
            0,
            recovered_checkpoint,
        )?;
        let service = self.load_index_unlocked()?;
        let items_indexed = service.len();
        let report = KnowledgeRebuildCheckpointReport {
            status: "ready".to_owned(),
            lock_path: display_path(lock.path()),
            checkpoint_path: display_path(&self.rebuild_checkpoint_path()),
            items_indexed,
            recovered_checkpoint,
            audit: vec![
                "knowledge.rebuild:index_lock_acquired".to_owned(),
                format!("knowledge.rebuild:items_indexed:{items_indexed}"),
                "knowledge.rebuild:checkpoint_written".to_owned(),
            ],
        };
        write_knowledge_checkpoint(
            &self.rebuild_checkpoint_path(),
            "completed",
            items_indexed,
            recovered_checkpoint,
        )?;
        audit_sink.record(
            AuditEvent::new(
                AuditAction::MemoryMaintenance,
                AuditOutcome::Ok,
                trace.clone(),
            )
            .with_message("durable knowledge index rebuild checkpoint completed")
            .with_field("store", display_path(&self.root))
            .with_field("items_indexed", items_indexed.to_string())
            .with_field(
                "checkpoint_path",
                display_path(&self.rebuild_checkpoint_path()),
            ),
        )?;
        Ok(report)
    }

    /// 假定调用方持锁，原子替换单个 Knowledge 文件。
    fn write_item_unlocked(&self, item: &KnowledgeItem) -> Result<(), EvaError> {
        let path = self.root.join(format!("{}.knowledge", item.id.as_str()));
        write_text_atomically(
            &path,
            &knowledge_item_to_storage(item),
            "failed to write durable knowledge item",
        )
    }

    /// 假定调用方持锁，严格解析全部文件并重建唯一索引。
    fn load_index_unlocked(&self) -> Result<InMemoryKnowledgeService, EvaError> {
        let mut items = Vec::new();
        for path in list_files_with_extension(&self.root, "knowledge")? {
            let data = fs::read_to_string(&path).map_err(|error| {
                filesystem_error("failed to read durable knowledge item", &path, error)
            })?;
            items.push(
                knowledge_item_from_storage(&data)
                    .map_err(|error| error.with_context("path", path.display().to_string()))?,
            );
        }
        InMemoryKnowledgeService::rebuild_from_items(items)
    }

    /// 返回 Knowledge 存储根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 返回 Knowledge 重建检查点路径。
    pub fn rebuild_checkpoint_path(&self) -> PathBuf {
        self.root.join(KNOWLEDGE_REBUILD_CHECKPOINT_FILE)
    }
}

impl DurableIndexLockGuard {
    /// 创建目录并用 `create_new` 原子获取跨进程锁。
    ///
    /// AlreadyExists 映射为冲突；载荷写入失败会删除刚创建的锁，避免无有效时间元数据
    /// 的永久锁。正常释放依赖 RAII Drop。
    fn acquire(root: &Path) -> Result<Self, EvaError> {
        fs::create_dir_all(root).map_err(|error| {
            filesystem_error("failed to create durable index directory", root, error)
        })?;
        let path = root.join(INDEX_LOCK_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    EvaError::conflict("durable index lock already exists")
                        .with_context("path", path.display().to_string())
                } else {
                    filesystem_error("failed to create durable index lock", &path, error)
                }
            })?;
        file.write_all(
            format!(
                "format=eva.durable.index-lock.v1\npid={}\ncreated_at_ms={}\n",
                std::process::id(),
                current_time_ms()
            )
            .as_bytes(),
        )
        .map_err(|error| {
            let _ = fs::remove_file(&path);
            filesystem_error("failed to write durable index lock", &path, error)
        })?;
        Ok(Self { path })
    }

    /// 返回当前持有的锁文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DurableIndexLockGuard {
    /// 尽力删除锁文件；析构路径不传播清理错误。
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// 按 v1 固定字段顺序编码 Memory 记录，并仅压缩 value。
///
/// 所有自由文本随后十六进制编码；压缩元数据与编码后的值绑定，读取端先解码字段再
/// 解压。内存中的 record.value 始终保持逻辑明文。
fn memory_record_to_storage(record: &MemoryRecord) -> String {
    let stored_value = match record.compression {
        MemoryCompression::None => record.value.clone(),
        MemoryCompression::RunLength => run_length_encode(&record.value),
    };
    let mut lines = vec![
        "format=eva.memory.v1".to_owned(),
        format!("key={}", encode_field(&record.key)),
        format!("value={}", encode_field(&stored_value)),
        format!("visibility={}", record.visibility.as_str()),
        format!(
            "owner_agent={}",
            record
                .owner_agent
                .as_ref()
                .map(|agent| encode_field(agent.as_str()))
                .unwrap_or_default()
        ),
        format!("retention={}", record.retention.as_str()),
        format!("version={}", record.version.0),
        format!(
            "request_id={}",
            record
                .request_id
                .as_ref()
                .map(|request| encode_field(request.as_str()))
                .unwrap_or_default()
        ),
        format!("audit_reason={}", encode_field(&record.audit_reason)),
        format!("created_at_ms={}", record.created_at_ms),
        format!(
            "expires_at_ms={}",
            record
                .expires_at_ms
                .map(|value| value.to_string())
                .unwrap_or_default()
        ),
        format!("compression={}", record.compression.as_str()),
    ];
    lines.push(String::new());
    lines.join("\n")
}

/// 严格解析 v1 Memory 记录并按声明算法解压 value。
///
/// 格式、必填字段、标识、数值、编码或压缩任一无效均返回冲突；不会用默认值掩盖
/// 损坏持久记录。
fn memory_record_from_storage(data: &str) -> Result<MemoryRecord, EvaError> {
    let fields = parse_fields(data)?;
    if required_raw(&fields, "format")? != "eva.memory.v1" {
        return Err(EvaError::conflict(
            "unsupported durable memory record format",
        ));
    }
    let compression = MemoryCompression::parse(required_raw(&fields, "compression")?)?;
    let stored_value = decode_field(required_raw(&fields, "value")?)?;
    let value = match compression {
        MemoryCompression::None => stored_value,
        MemoryCompression::RunLength => run_length_decode(&stored_value)?,
    };
    Ok(MemoryRecord {
        key: decode_field(required_raw(&fields, "key")?)?,
        value,
        visibility: MemoryVisibility::parse(required_raw(&fields, "visibility")?)?,
        owner_agent: optional_agent(fields.get("owner_agent").map(String::as_str))?,
        retention: MemoryRetention::parse(required_raw(&fields, "retention")?)?,
        version: StateVersion(parse_u64(required_raw(&fields, "version")?, "version")?),
        request_id: optional_request(fields.get("request_id").map(String::as_str))?,
        audit_reason: decode_field(required_raw(&fields, "audit_reason")?)?,
        created_at_ms: parse_u128(required_raw(&fields, "created_at_ms")?, "created_at_ms")?,
        expires_at_ms: optional_u128(
            fields.get("expires_at_ms").map(String::as_str),
            "expires_at_ms",
        )?,
        compression,
    })
}

/// 按 v1 固定字段顺序编码 Knowledge 条目，标签使用重复键。
fn knowledge_item_to_storage(item: &KnowledgeItem) -> String {
    let mut lines = vec![
        "format=eva.knowledge.v1".to_owned(),
        format!("id={}", encode_field(item.id.as_str())),
        format!("uri={}", encode_field(&item.source.uri)),
        format!("title={}", encode_field(&item.source.title)),
        format!("digest={}", encode_field(&item.source.digest)),
        format!("summary={}", encode_field(&item.summary)),
        format!("content={}", encode_field(&item.content)),
        format!(
            "request_id={}",
            item.request_id
                .as_ref()
                .map(|request| encode_field(request.as_str()))
                .unwrap_or_default()
        ),
    ];
    lines.extend(
        item.tags
            .iter()
            .map(|tag| format!("tag={}", encode_field(tag))),
    );
    lines.push(String::new());
    lines.join("\n")
}

/// 严格解析 v1 Knowledge 条目并恢复排序去重标签。
///
/// 持久来源 digest 原样恢复，因为它代表采集时指纹；文件完整性由格式和字段解码
/// 校验保证，不在加载时用 content 重算后静默改写。
fn knowledge_item_from_storage(data: &str) -> Result<KnowledgeItem, EvaError> {
    let fields = parse_multimap(data)?;
    if required_multi_raw(&fields, "format")? != "eva.knowledge.v1" {
        return Err(EvaError::conflict(
            "unsupported durable knowledge item format",
        ));
    }
    let id = KnowledgeId::parse(&decode_field(required_multi_raw(&fields, "id")?)?)?;
    let uri = decode_field(required_multi_raw(&fields, "uri")?)?;
    let title = decode_field(required_multi_raw(&fields, "title")?)?;
    let digest = decode_field(required_multi_raw(&fields, "digest")?)?;
    let summary = decode_field(required_multi_raw(&fields, "summary")?)?;
    let content = decode_field(required_multi_raw(&fields, "content")?)?;
    let request_id = optional_request(
        fields
            .get("request_id")
            .and_then(|values| values.first().map(String::as_str)),
    )?;
    let mut item =
        KnowledgeItem::new(id, KnowledgeSource { uri, title, digest }, summary, content)?;
    if let Some(values) = fields.get("tag") {
        for tag in values {
            item = item.with_tag(decode_field(tag)?);
        }
    }
    if let Some(request_id) = request_id {
        item = item.with_request_id(request_id);
    }
    Ok(item)
}

/// 从可见性、owner 和十六进制 key 生成唯一 Memory 文件路径。
///
/// 私有记录缺少 owner 会失败；key 编码避免路径分隔符逃逸根目录，不同 owner 的同名
/// 私有记录及同名全局记录落在不同文件。
fn memory_record_path(root: &Path, record: &MemoryRecord) -> Result<PathBuf, EvaError> {
    let key = encode_field(&record.key);
    let name = match record.visibility {
        MemoryVisibility::Private => {
            let owner = record.owner_agent.as_ref().ok_or_else(|| {
                EvaError::invalid_argument("private memory record missing owner agent")
            })?;
            format!("private__{}__{key}.memory", owner.as_str())
        }
        MemoryVisibility::Global => format!("global__{key}.memory"),
    };
    Ok(root.join(name))
}

/// 列出根目录下指定扩展名的文件并排序；目录不存在视为空存储。
fn list_files_with_extension(root: &Path, extension: &str) -> Result<Vec<PathBuf>, EvaError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(filesystem_error(
                "failed to read durable memory directory",
                root,
                error,
            ));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            filesystem_error("failed to read durable directory entry", root, error)
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// 先写同目录临时文件，再 rename 提交文本更新。
///
/// 为兼容 Windows，目标存在时先删除再 rename，因此删除与重命名之间并非严格原子；
/// 失败会返回带路径错误，但当前实现不清理临时文件也不 fsync。调用方依赖索引锁防止
/// 并发观察，并依赖检查点识别中断维护。
fn write_text_atomically(path: &Path, data: &str, message: &str) -> Result<(), EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            filesystem_error("failed to create durable storage directory", parent, error)
        })?;
    }
    let temp_path =
        path.with_extension(format!("tmp-{}-{}", std::process::id(), current_time_ms()));
    fs::write(&temp_path, data).map_err(|error| filesystem_error(message, &temp_path, error))?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| filesystem_error(message, path, error))?;
    }
    fs::rename(&temp_path, path).map_err(|error| filesystem_error(message, path, error))
}

/// 原子替换 Memory GC 检查点及统计字段。
fn write_memory_checkpoint(
    path: &Path,
    now_ms: u128,
    status: &str,
    scanned_records: usize,
    records_kept: usize,
    expired_removed: usize,
    recovered_checkpoint: bool,
) -> Result<(), EvaError> {
    write_text_atomically(
        path,
        &format!(
            "format=eva.memory.gc-checkpoint.v1\nstatus={status}\nnow_ms={now_ms}\nscanned_records={scanned_records}\nrecords_kept={records_kept}\nexpired_removed={expired_removed}\nrecovered_checkpoint={recovered_checkpoint}\nupdated_at_ms={}\n",
            current_time_ms()
        ),
        "failed to write durable memory GC checkpoint",
    )
}

/// 原子替换 Knowledge 重建检查点及条目计数。
fn write_knowledge_checkpoint(
    path: &Path,
    status: &str,
    items_indexed: usize,
    recovered_checkpoint: bool,
) -> Result<(), EvaError> {
    write_text_atomically(
        path,
        &format!(
            "format=eva.knowledge.rebuild-checkpoint.v1\nstatus={status}\nitems_indexed={items_indexed}\nrecovered_checkpoint={recovered_checkpoint}\nupdated_at_ms={}\n",
            current_time_ms()
        ),
        "failed to write durable knowledge rebuild checkpoint",
    )
}

/// 在开始新维护前恢复确属旧 started 检查点的过期锁。
///
/// 仅检查点超过阈值还不够：锁也必须过期，且 created_at 不晚于检查点 updated_at，
/// 才能证明二者属于同一次中断维护。否则保留锁并让正常 acquire 返回冲突，避免偷走
/// 另一个较新进程持有的锁。
fn recover_interrupted_maintenance_lock(
    root: &Path,
    checkpoint_path: &Path,
    now_ms: u128,
) -> Result<bool, EvaError> {
    let Some(checkpoint_updated_at_ms) =
        stale_started_checkpoint_updated_at(checkpoint_path, now_ms)?
    else {
        return Ok(false);
    };
    let lock_path = root.join(INDEX_LOCK_FILE);
    if !lock_path.exists() {
        return Ok(true);
    }
    if !maintenance_lock_belongs_to_checkpoint(&lock_path, checkpoint_updated_at_ms, now_ms)? {
        return Ok(false);
    }
    fs::remove_file(&lock_path).map_err(|error| {
        filesystem_error(
            "failed to recover interrupted durable maintenance lock",
            &lock_path,
            error,
        )
    })?;
    Ok(true)
}

/// 若检查点状态为 started 且更新时间已超过阈值，返回其更新时间。
fn stale_started_checkpoint_updated_at(
    path: &Path,
    now_ms: u128,
) -> Result<Option<u128>, EvaError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(filesystem_error(
                "failed to read durable maintenance checkpoint",
                path,
                error,
            ))
        }
    };
    let fields = parse_fields(&data)?;
    if fields.get("status").map(String::as_str) != Some("started") {
        return Ok(None);
    }
    let Some(updated_at_ms) = fields
        .get("updated_at_ms")
        .map(|value| parse_u128(value, "updated_at_ms"))
        .transpose()?
    else {
        return Ok(None);
    };
    if now_ms.saturating_sub(updated_at_ms) >= INDEX_LOCK_STALE_AFTER_MS {
        Ok(Some(updated_at_ms))
    } else {
        Ok(None)
    }
}

/// 判断锁的创建时间是否与过期检查点相容且本身已过期。
fn maintenance_lock_belongs_to_checkpoint(
    path: &Path,
    checkpoint_updated_at_ms: u128,
    now_ms: u128,
) -> Result<bool, EvaError> {
    let data = fs::read_to_string(path)
        .map_err(|error| filesystem_error("failed to read durable index lock", path, error))?;
    let fields = parse_fields(&data)?;
    let Some(created_at_ms) = fields
        .get("created_at_ms")
        .map(|value| parse_u128(value, "created_at_ms"))
        .transpose()?
    else {
        return Ok(false);
    };
    Ok(created_at_ms <= checkpoint_updated_at_ms
        && now_ms.saturating_sub(created_at_ms) >= INDEX_LOCK_STALE_AFTER_MS)
}

/// 返回当前 Unix epoch 毫秒；系统时间早于 epoch 时保守返回零。
fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// 将平台路径转换为诊断字符串。
fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// 解析不允许重复语义的行式字段；后出现键覆盖前值。
fn parse_fields(data: &str) -> Result<std::collections::BTreeMap<String, String>, EvaError> {
    let mut fields = std::collections::BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::conflict("durable memory record has invalid line"));
        };
        fields.insert(key.to_owned(), value.to_owned());
    }
    Ok(fields)
}

/// 解析允许标签等重复键的行式多值字段。
fn parse_multimap(data: &str) -> Result<std::collections::BTreeMap<String, Vec<String>>, EvaError> {
    let mut fields = std::collections::BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::conflict(
                "durable knowledge item has invalid line",
            ));
        };
        fields
            .entry(key.to_owned())
            .or_insert_with(Vec::new)
            .push(value.to_owned());
    }
    Ok(fields)
}

/// 读取 Memory 持久格式必填字段。
fn required_raw<'a>(
    fields: &'a std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, EvaError> {
    fields.get(key).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("durable memory record is missing field").with_context("field", key)
    })
}

/// 读取 Knowledge 多值格式中必填字段的第一个值。
fn required_multi_raw<'a>(
    fields: &'a std::collections::BTreeMap<String, Vec<String>>,
    key: &str,
) -> Result<&'a str, EvaError> {
    fields
        .get(key)
        .and_then(|values| values.first())
        .map(String::as_str)
        .ok_or_else(|| {
            EvaError::conflict("durable knowledge item is missing field").with_context("field", key)
        })
}

/// 解码可选 AgentId；缺失或空字段为 `None`。
fn optional_agent(value: Option<&str>) -> Result<Option<AgentId>, EvaError> {
    match value {
        Some(value) if !value.is_empty() => Ok(Some(AgentId::parse(&decode_field(value)?)?)),
        _ => Ok(None),
    }
}

/// 解码可选 RequestId；缺失或空字段为 `None`。
fn optional_request(value: Option<&str>) -> Result<Option<RequestId>, EvaError> {
    match value {
        Some(value) if !value.is_empty() => Ok(Some(RequestId::parse(&decode_field(value)?)?)),
        _ => Ok(None),
    }
}

/// 解析可选 `u128`；缺失或空字段为 `None`。
fn optional_u128(value: Option<&str>, field: &str) -> Result<Option<u128>, EvaError> {
    match value {
        Some(value) if !value.is_empty() => Ok(Some(parse_u128(value, field)?)),
        _ => Ok(None),
    }
}

/// 解析持久格式中的 `u64` 数值。
fn parse_u64(value: &str, field: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("invalid durable memory integer").with_context("field", field)
    })
}

/// 解析时间和 TTL 使用的 `u128` 数值。
fn parse_u128(value: &str, field: &str) -> Result<u128, EvaError> {
    value.parse::<u128>().map_err(|_| {
        EvaError::conflict("invalid durable memory integer").with_context("field", field)
    })
}

/// 将 UTF-8 文本按字节编码为小写十六进制。
fn encode_field(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

/// 严格十六进制解码并验证 UTF-8。
fn decode_field(value: &str) -> Result<String, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict("encoded field length is invalid"));
    }
    let mut bytes = Vec::new();
    for chunk in value.as_bytes().chunks(2) {
        let hex = std::str::from_utf8(chunk)
            .map_err(|_| EvaError::conflict("encoded field is not utf8"))?;
        bytes.push(
            u8::from_str_radix(hex, 16)
                .map_err(|_| EvaError::conflict("encoded field is not hex"))?,
        );
    }
    String::from_utf8(bytes).map_err(|_| EvaError::conflict("encoded field is not utf8"))
}

/// 按 Unicode 标量游程编码为 `count:codepoint;` 序列。
fn run_length_encode(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        let mut count = 1usize;
        while chars.peek() == Some(&ch) {
            chars.next();
            count += 1;
        }
        output.push_str(&format!("{count}:{:x};", ch as u32));
    }
    output
}

/// 严格解码游程序列并拒绝非法计数或 Unicode codepoint。
///
/// 该压缩用于可逆持久格式，不设解压大小上限；只应读取受控本地存储，不能把不可信
/// 大计数输入当作网络协议直接解析。
fn run_length_decode(value: &str) -> Result<String, EvaError> {
    let mut output = String::new();
    for segment in value.split(';').filter(|segment| !segment.is_empty()) {
        let Some((count, codepoint)) = segment.split_once(':') else {
            return Err(EvaError::conflict("run_length memory value is invalid"));
        };
        let count = count
            .parse::<usize>()
            .map_err(|_| EvaError::conflict("run_length count is invalid"))?;
        let codepoint = u32::from_str_radix(codepoint, 16)
            .map_err(|_| EvaError::conflict("run_length codepoint is invalid"))?;
        let ch = char::from_u32(codepoint)
            .ok_or_else(|| EvaError::conflict("run_length codepoint is invalid"))?;
        for _ in 0..count {
            output.push(ch);
        }
    }
    Ok(output)
}

/// 将文件系统错误映射为带路径和原始 I/O 消息的内部错误。
fn filesystem_error(message: &str, path: &Path, error: std::io::Error) -> EvaError {
    EvaError::internal(message)
        .with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

#[cfg(test)]
/// 持久往返、索引锁、TTL GC 和检查点恢复测试。
mod tests {
    use super::*;
    use crate::knowledge_service::KnowledgeSearch;
    use eva_observability::{InMemoryAuditSink, TraceFields};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 解析测试 Agent 标识。
    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    /// 创建进程和时间戳隔离的临时持久根目录。
    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("eva-memory-{name}-{}-{unique}", std::process::id()))
    }

    #[test]
    /// 验证私有/全局、TTL 和游程压缩记录可完整往返。
    fn durable_memory_round_trips_private_global_ttl_and_compression() {
        let root = temp_root("memory");
        let mut store = FileSystemMemoryStore::new(&root);
        let owner = agent("root-agent");
        store
            .write(
                MemoryWrite::private(owner.clone(), "secret", "aaaaabbbb token=secret")
                    .with_ttl_ms(100, 100)
                    .with_compression(MemoryCompression::RunLength),
            )
            .unwrap();
        store
            .write(MemoryWrite::global("release", "v1.9.4").with_created_at_ms(100))
            .unwrap();

        let loaded = FileSystemMemoryStore::new(&root).load().unwrap();
        let snapshot = loaded.snapshot_for_agent_at(&owner, 8, 8, 150);

        assert_eq!(snapshot.private[0].value, "aaaaabbbb token=secret");
        assert_eq!(
            snapshot.private[0].compression,
            MemoryCompression::RunLength
        );
        assert_eq!(snapshot.global[0].value, "v1.9.4");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证 Knowledge 检索索引可从独立文件重建。
    fn durable_knowledge_rebuilds_search_index_from_files() {
        let root = temp_root("knowledge");
        let mut store = FileSystemKnowledgeStore::new(&root);
        let item = KnowledgeItem::new(
            KnowledgeId::parse("memory-plan").unwrap(),
            KnowledgeSource::new("docs/memory.md", "Memory", b"durable memory"),
            "Durable memory",
            "Durable memory context index",
        )
        .unwrap()
        .with_tag("v1.9.4");
        store.write_item(&item).unwrap();

        let rebuilt = FileSystemKnowledgeStore::new(&root).load_index().unwrap();
        let results = rebuilt.search(&KnowledgeSearch::new("durable")).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.id.as_str(), "memory-plan");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证持锁期间并发 Memory 访问返回冲突，释放后恢复。
    fn durable_index_lock_blocks_concurrent_memory_access_until_released() {
        let root = temp_root("lock");
        let store = FileSystemMemoryStore::new(&root);
        let lock = store.try_acquire_index_lock().unwrap();

        let error = store.load().unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(lock.path().is_file());
        drop(lock);
        assert!(!root.join(INDEX_LOCK_FILE).exists());
        assert!(store.load().unwrap().is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证 TTL GC 删除到期记录并写 completed 审计检查点。
    fn durable_memory_compaction_removes_expired_records_and_writes_audit_checkpoint() {
        let root = temp_root("compact");
        let mut store = FileSystemMemoryStore::new(&root);
        let owner = agent("root-agent");
        store
            .write(MemoryWrite::private(owner.clone(), "fresh", "keep").with_ttl_ms(100, 100))
            .unwrap();
        store
            .write(MemoryWrite::private(owner.clone(), "expired", "drop").with_ttl_ms(100, 1))
            .unwrap();
        fs::write(store.memory_checkpoint_path(), "status=started\n").unwrap();
        let mut audit = InMemoryAuditSink::default();

        let report = store
            .compact_expired_at(150, &mut audit, &TraceFields::default())
            .unwrap();
        let loaded = store.load().unwrap();
        let snapshot = loaded.snapshot_for_agent_at(&owner, 8, 8, 150);

        assert_eq!(report.scanned_records, 2);
        assert_eq!(report.records_kept, 1);
        assert_eq!(report.expired_removed, 1);
        assert!(!report.recovered_checkpoint);
        assert!(store.memory_checkpoint_path().is_file());
        assert_eq!(audit.events.len(), 1);
        assert_eq!(audit.events[0].action, AuditAction::MemoryMaintenance);
        assert_eq!(snapshot.private.len(), 1);
        assert_eq!(snapshot.private[0].key, "fresh");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证过期 started 检查点对应的旧锁可被安全恢复。
    fn durable_memory_compaction_recovers_stale_started_checkpoint_lock() {
        let root = temp_root("stale-lock");
        fs::create_dir_all(&root).unwrap();
        let mut store = FileSystemMemoryStore::new(&root);
        fs::write(
            root.join(INDEX_LOCK_FILE),
            "format=eva.durable.index-lock.v1\npid=999999\ncreated_at_ms=1\n",
        )
        .unwrap();
        fs::write(
            store.memory_checkpoint_path(),
            "format=eva.memory.gc-checkpoint.v1\nstatus=started\nupdated_at_ms=1\n",
        )
        .unwrap();
        let mut audit = InMemoryAuditSink::default();

        let report = store
            .compact_expired_at(
                INDEX_LOCK_STALE_AFTER_MS + 2,
                &mut audit,
                &TraceFields::default(),
            )
            .unwrap();

        assert!(report.recovered_checkpoint);
        assert_eq!(report.status, "ready");
        assert!(!root.join(INDEX_LOCK_FILE).exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证旧检查点不能导致较新的索引锁被误删。
    fn durable_memory_compaction_does_not_steal_newer_index_lock_for_stale_checkpoint() {
        let root = temp_root("newer-lock");
        fs::create_dir_all(&root).unwrap();
        let mut store = FileSystemMemoryStore::new(&root);
        fs::write(
            root.join(INDEX_LOCK_FILE),
            format!(
                "format=eva.durable.index-lock.v1\npid=999999\ncreated_at_ms={}\n",
                INDEX_LOCK_STALE_AFTER_MS + 10
            ),
        )
        .unwrap();
        fs::write(
            store.memory_checkpoint_path(),
            "format=eva.memory.gc-checkpoint.v1\nstatus=started\nupdated_at_ms=1\n",
        )
        .unwrap();
        let mut audit = InMemoryAuditSink::default();

        let error = store
            .compact_expired_at(
                INDEX_LOCK_STALE_AFTER_MS + 20,
                &mut audit,
                &TraceFields::default(),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(root.join(INDEX_LOCK_FILE).exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证 Knowledge 重建检查点记录条目数和恢复标记。
    fn durable_knowledge_rebuild_checkpoint_records_item_count_and_recovery() {
        let root = temp_root("knowledge-checkpoint");
        let mut store = FileSystemKnowledgeStore::new(&root);
        let item = KnowledgeItem::new(
            KnowledgeId::parse("memory-plan").unwrap(),
            KnowledgeSource::new("docs/memory.md", "Memory", b"durable memory"),
            "Durable memory",
            "Durable memory context index",
        )
        .unwrap();
        store.write_item(&item).unwrap();
        fs::write(
            store.rebuild_checkpoint_path(),
            "format=eva.knowledge.rebuild-checkpoint.v1\nstatus=started\nupdated_at_ms=1\n",
        )
        .unwrap();
        let mut audit = InMemoryAuditSink::default();

        let report = store
            .rebuild_checkpoint(&mut audit, &TraceFields::default())
            .unwrap();
        let rebuilt = store.load_index().unwrap();

        assert_eq!(report.items_indexed, 1);
        assert!(report.recovered_checkpoint);
        assert!(store.rebuild_checkpoint_path().is_file());
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(audit.events.len(), 1);
        assert_eq!(audit.events[0].action, AuditAction::MemoryMaintenance);
        fs::remove_dir_all(root).ok();
    }
}
