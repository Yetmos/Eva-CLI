//! Durable memory and knowledge stores backed by the durable state directory.

use crate::knowledge_service::{
    InMemoryKnowledgeService, KnowledgeId, KnowledgeItem, KnowledgeSource,
};
use crate::memory_service::{
    InMemoryMemoryService, MemoryCompression, MemoryRecord, MemoryRetention, MemoryVisibility,
    MemoryWrite,
};
use eva_core::{AgentId, EvaError, RequestId};
use eva_storage::{DurableBackendLayout, StateVersion};
use std::fs;
use std::path::{Path, PathBuf};

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

impl FileSystemMemoryStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self::new(layout.state_dir.join("memory"))
    }

    pub fn write(&mut self, write: MemoryWrite) -> Result<MemoryRecord, EvaError> {
        let mut service = self.load()?;
        let record = service.write(write)?;
        self.write_record(&record)?;
        Ok(record)
    }

    pub fn write_record(&mut self, record: &MemoryRecord) -> Result<(), EvaError> {
        let path = memory_record_path(&self.root, record)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                filesystem_error("failed to create durable memory directory", parent, error)
            })?;
        }
        fs::write(&path, memory_record_to_storage(record)).map_err(|error| {
            filesystem_error("failed to write durable memory record", &path, error)
        })
    }

    pub fn load(&self) -> Result<InMemoryMemoryService, EvaError> {
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

    pub fn write_item(&mut self, item: &KnowledgeItem) -> Result<(), EvaError> {
        let path = self.root.join(format!("{}.knowledge", item.id.as_str()));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                filesystem_error(
                    "failed to create durable knowledge directory",
                    parent,
                    error,
                )
            })?;
        }
        fs::write(&path, knowledge_item_to_storage(item)).map_err(|error| {
            filesystem_error("failed to write durable knowledge item", &path, error)
        })
    }

    pub fn load_index(&self) -> Result<InMemoryKnowledgeService, EvaError> {
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
}
