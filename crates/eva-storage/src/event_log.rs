//! Durable event log implementations.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable event log interfaces and replay boundaries";

use crate::DurableBackendLayout;
use eva_core::{
    AdapterId, AgentId, CapabilityName, ErrorKind, EvaError, Event, EventId, EventMetadata,
    EventPayload, EventTarget, GenerationId, RequestId, Topic, TraceContext,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const EVENT_LOG_DIR: &str = "log";
const EVENT_RECORD_EXTENSION: &str = "event";
const EVENT_RECORD_VERSION: &str = "1";

/// Lifecycle state for one event log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventLogStatus {
    Appended,
    Acked,
    Failed,
}

/// Append-only event log record used by EventBus and Agent consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogRecord {
    pub sequence: u64,
    pub event: Event,
    pub status: EventLogStatus,
    pub consumer: Option<AgentId>,
    pub error: Option<EvaError>,
}

/// Event log behavior required by the V0.4 runtime loop.
pub trait EventLog {
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError>;
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError>;
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError>;
    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord>;
    fn watermark(&self) -> u64;
}

/// In-memory log used by tests and the V0.4 basic runtime path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryEventLog {
    next_sequence: u64,
    records: Vec<EventLogRecord>,
}

/// Filesystem-backed event log rooted under the durable backend event dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemEventLog {
    root: PathBuf,
    next_sequence: u64,
    records: Vec<EventLogRecord>,
}

impl EventLogStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Appended => "appended",
            Self::Acked => "acked",
            Self::Failed => "failed",
        }
    }

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

impl InMemoryEventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> &[EventLogRecord] {
        &self.records
    }

    fn find_mut(&mut self, event_id: &EventId) -> Result<&mut EventLogRecord, EvaError> {
        self.records
            .iter_mut()
            .find(|record| record.event.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("event log record does not exist")
                    .with_context("event_id", event_id.as_str())
            })
    }

    fn contains_event(&self, event_id: &EventId) -> bool {
        self.records
            .iter()
            .any(|record| record.event.event_id() == event_id)
    }
}

impl FileSystemEventLog {
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::open_dir(layout.event_dir.join(EVENT_LOG_DIR))
    }

    pub fn open_dir(root: impl Into<PathBuf>) -> Result<Self, EvaError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| {
            EvaError::internal("failed to create durable event log directory")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;

        let mut records = load_records(&root)?;
        records.sort_by_key(|record| record.sequence);
        validate_loaded_records(&records)?;
        let next_sequence = records.last().map(|record| record.sequence).unwrap_or(0);

        Ok(Self {
            root,
            next_sequence,
            records,
        })
    }

    pub fn records(&self) -> &[EventLogRecord] {
        &self.records
    }

    fn record_path(&self, sequence: u64) -> PathBuf {
        self.root
            .join(format!("{sequence:020}.{EVENT_RECORD_EXTENSION}"))
    }

    fn persist_record(&self, record: &EventLogRecord) -> Result<(), EvaError> {
        let path = self.record_path(record.sequence);
        fs::write(&path, record_to_storage(record)).map_err(|error| {
            EvaError::internal("failed to write durable event log record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })
    }

    fn contains_event(&self, event_id: &EventId) -> bool {
        self.records
            .iter()
            .any(|record| record.event.event_id() == event_id)
    }

    fn record_position(&self, event_id: &EventId) -> Result<usize, EvaError> {
        self.records
            .iter()
            .position(|record| record.event.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("event log record does not exist")
                    .with_context("event_id", event_id.as_str())
            })
    }
}

impl EventLog for InMemoryEventLog {
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

    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        let record = self.find_mut(event_id)?;
        record.status = EventLogStatus::Acked;
        record.consumer = Some(consumer);
        record.error = None;
        Ok(record.clone())
    }

    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        let record = self.find_mut(event_id)?;
        record.status = EventLogStatus::Failed;
        record.consumer = Some(consumer);
        record.error = Some(error);
        Ok(record.clone())
    }

    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord> {
        self.records
            .iter()
            .filter(|record| record.sequence >= sequence)
            .cloned()
            .collect()
    }

    fn watermark(&self) -> u64 {
        self.next_sequence
    }
}

impl EventLog for FileSystemEventLog {
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError> {
        if self.contains_event(event.event_id()) {
            return Err(EvaError::conflict("event already exists in log")
                .with_context("event_id", event.event_id().as_str()));
        }

        let sequence = self.next_sequence + 1;
        let record = EventLogRecord::appended(sequence, event);
        self.persist_record(&record)?;
        self.next_sequence = sequence;
        self.records.push(record.clone());
        Ok(record)
    }

    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        let position = self.record_position(event_id)?;
        let mut record = self.records[position].clone();
        record.status = EventLogStatus::Acked;
        record.consumer = Some(consumer);
        record.error = None;
        self.persist_record(&record)?;
        self.records[position] = record.clone();
        Ok(record)
    }

    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        let position = self.record_position(event_id)?;
        let mut record = self.records[position].clone();
        record.status = EventLogStatus::Failed;
        record.consumer = Some(consumer);
        record.error = Some(error);
        self.persist_record(&record)?;
        self.records[position] = record.clone();
        Ok(record)
    }

    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord> {
        self.records
            .iter()
            .filter(|record| record.sequence >= sequence)
            .cloned()
            .collect()
    }

    fn watermark(&self) -> u64 {
        self.next_sequence
    }
}

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

fn push_field(data: &mut String, key: &str, value: &str) {
    data.push_str(key);
    data.push('=');
    data.push_str(value);
    data.push('\n');
}

fn push_encoded_string(data: &mut String, key: &str, value: &str) {
    push_field(data, key, &hex_encode(value.as_bytes()));
}

fn push_optional_string(data: &mut String, key: &str, value: Option<&str>) {
    match value {
        Some(value) => push_encoded_string(data, key, value),
        None => push_field(data, key, ""),
    }
}

fn require_field<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, EvaError> {
    fields.get(key).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("durable event log record field is missing").with_context("field", key)
    })
}

fn decoded_required_string(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<String, EvaError> {
    decode_string(require_field(fields, key)?).map_err(|error| error.with_context("field", key))
}

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

fn parse_u64(value: &str, field: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("durable event log numeric field is invalid")
            .with_context("field", field)
    })
}

fn parse_u32(value: &str, field: &str) -> Result<u32, EvaError> {
    value.parse::<u32>().map_err(|_| {
        EvaError::conflict("durable event log numeric field is invalid")
            .with_context("field", field)
    })
}

fn parse_usize(value: &str, field: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::conflict("durable event log numeric field is invalid")
            .with_context("field", field)
    })
}

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

fn decode_string(value: &str) -> Result<String, EvaError> {
    String::from_utf8(hex_decode(value)?)
        .map_err(|_| EvaError::conflict("durable event log string field is not utf-8"))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

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

fn hex_value(value: u8) -> Result<u8, EvaError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(EvaError::conflict("hex field contains invalid digit")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurableBackendOptions, FileSystemDurableBackend};
    use eva_core::{EventPayload, EventTarget, Topic};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

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
    fn append_assigns_sequence_and_watermark() {
        let mut log = InMemoryEventLog::new();

        let first = log.append(event("evt-1")).unwrap();
        let second = log.append(event("evt-2")).unwrap();

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(log.watermark(), 2);
    }

    #[test]
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
    fn replay_returns_records_from_cursor() {
        let mut log = InMemoryEventLog::new();
        log.append(event("evt-1")).unwrap();
        log.append(event("evt-2")).unwrap();

        let replay = log.replay_from(2);

        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event.event_id().as_str(), "evt-2");
    }

    #[test]
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

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

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
