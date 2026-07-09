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

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable memory and rebuildable knowledge persistence";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemMemoryStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemKnowledgeStore {
    root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DurableIndexLockGuard {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCompactionReport {
    pub status: String,
    pub lock_path: String,
    pub checkpoint_path: String,
    pub scanned_records: usize,
    pub records_kept: usize,
    pub expired_removed: usize,
    pub recovered_checkpoint: bool,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeRebuildCheckpointReport {
    pub status: String,
    pub lock_path: String,
    pub checkpoint_path: String,
    pub items_indexed: usize,
    pub recovered_checkpoint: bool,
    pub audit: Vec<String>,
}

const INDEX_LOCK_FILE: &str = "index.lock";
const MEMORY_GC_CHECKPOINT_FILE: &str = "memory-gc.checkpoint";
const KNOWLEDGE_REBUILD_CHECKPOINT_FILE: &str = "knowledge-rebuild.checkpoint";
const INDEX_LOCK_STALE_AFTER_MS: u128 = 60_000;

impl FileSystemMemoryStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self::new(layout.state_dir.join("memory"))
    }

    pub fn try_acquire_index_lock(&self) -> Result<DurableIndexLockGuard, EvaError> {
        DurableIndexLockGuard::acquire(&self.root)
    }

    pub fn write(&mut self, write: MemoryWrite) -> Result<MemoryRecord, EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        let mut service = self.load_unlocked()?;
        let record = service.write(write)?;
        self.write_record_unlocked(&record)?;
        Ok(record)
    }

    pub fn write_record(&mut self, record: &MemoryRecord) -> Result<(), EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.write_record_unlocked(record)
    }

    pub fn load(&self) -> Result<InMemoryMemoryService, EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.load_unlocked()
    }

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

    fn write_record_unlocked(&self, record: &MemoryRecord) -> Result<(), EvaError> {
        let path = memory_record_path(&self.root, record)?;
        write_text_atomically(
            &path,
            &memory_record_to_storage(record),
            "failed to write durable memory record",
        )
    }

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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn memory_checkpoint_path(&self) -> PathBuf {
        self.root.join(MEMORY_GC_CHECKPOINT_FILE)
    }
}

impl FileSystemKnowledgeStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self::new(layout.state_dir.join("knowledge"))
    }

    pub fn try_acquire_index_lock(&self) -> Result<DurableIndexLockGuard, EvaError> {
        DurableIndexLockGuard::acquire(&self.root)
    }

    pub fn write_item(&mut self, item: &KnowledgeItem) -> Result<(), EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.write_item_unlocked(item)
    }

    pub fn load_index(&self) -> Result<InMemoryKnowledgeService, EvaError> {
        let _lock = self.try_acquire_index_lock()?;
        self.load_index_unlocked()
    }

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

    fn write_item_unlocked(&self, item: &KnowledgeItem) -> Result<(), EvaError> {
        let path = self.root.join(format!("{}.knowledge", item.id.as_str()));
        write_text_atomically(
            &path,
            &knowledge_item_to_storage(item),
            "failed to write durable knowledge item",
        )
    }

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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rebuild_checkpoint_path(&self) -> PathBuf {
        self.root.join(KNOWLEDGE_REBUILD_CHECKPOINT_FILE)
    }
}

impl DurableIndexLockGuard {
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

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DurableIndexLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

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

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

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

fn required_raw<'a>(
    fields: &'a std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, EvaError> {
    fields.get(key).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("durable memory record is missing field").with_context("field", key)
    })
}

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

fn optional_agent(value: Option<&str>) -> Result<Option<AgentId>, EvaError> {
    match value {
        Some(value) if !value.is_empty() => Ok(Some(AgentId::parse(&decode_field(value)?)?)),
        _ => Ok(None),
    }
}

fn optional_request(value: Option<&str>) -> Result<Option<RequestId>, EvaError> {
    match value {
        Some(value) if !value.is_empty() => Ok(Some(RequestId::parse(&decode_field(value)?)?)),
        _ => Ok(None),
    }
}

fn optional_u128(value: Option<&str>, field: &str) -> Result<Option<u128>, EvaError> {
    match value {
        Some(value) if !value.is_empty() => Ok(Some(parse_u128(value, field)?)),
        _ => Ok(None),
    }
}

fn parse_u64(value: &str, field: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("invalid durable memory integer").with_context("field", field)
    })
}

fn parse_u128(value: &str, field: &str) -> Result<u128, EvaError> {
    value.parse::<u128>().map_err(|_| {
        EvaError::conflict("invalid durable memory integer").with_context("field", field)
    })
}

fn encode_field(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

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

fn filesystem_error(message: &str, path: &Path, error: std::io::Error) -> EvaError {
    EvaError::internal(message)
        .with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_service::KnowledgeSearch;
    use eva_observability::{InMemoryAuditSink, TraceFields};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("eva-memory-{name}-{}-{unique}", std::process::id()))
    }

    #[test]
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
