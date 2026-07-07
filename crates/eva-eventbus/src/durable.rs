//! Durable EventBus implementation backed by filesystem storage.

use crate::bus::{EventBus, EventReceipt};
use crate::dead_letter::{DeadLetterRecord, RedrivePolicy};
use eva_core::{
    AgentId, ErrorKind, EvaError, Event, EventId, EventMetadata, EventPayload, EventTarget,
    GenerationId, RequestId, Topic, TraceContext,
};
use eva_storage::{DurableBackendLayout, EventLog, EventLogRecord, FileSystemEventLog};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable event publication and dead-letter redrive";

const DEAD_LETTER_DIR: &str = "dead_letters";
const DEAD_LETTER_EXTENSION: &str = "dead";
const DEAD_LETTER_VERSION: &str = "1";

/// Filesystem-backed EventBus used by V1.6 durable runtime paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableEventBus {
    log: FileSystemEventLog,
    dead_letter_store: FileSystemDeadLetterStore,
    receipts: Vec<EventReceipt>,
}

/// Filesystem-backed dead-letter queue with queryable records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemDeadLetterStore {
    root: PathBuf,
    records: Vec<DeadLetterRecord>,
}

impl DurableEventBus {
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Ok(Self {
            log: FileSystemEventLog::open(layout)?,
            dead_letter_store: FileSystemDeadLetterStore::open(layout)?,
            receipts: Vec::new(),
        })
    }

    pub fn log(&self) -> &FileSystemEventLog {
        &self.log
    }

    pub fn receipts(&self) -> &[EventReceipt] {
        &self.receipts
    }

    pub fn dead_letters(&self) -> &[DeadLetterRecord] {
        self.dead_letter_store.records()
    }

    pub fn dead_letter(
        &mut self,
        event: Event,
        reason: EvaError,
    ) -> Result<DeadLetterRecord, EvaError> {
        self.dead_letter_store.push(event, reason)
    }

    pub fn replay_dead_letters(&mut self) -> Result<Vec<EventReceipt>, EvaError> {
        let events = self.dead_letter_store.replay_all_for_publish()?;
        events
            .into_iter()
            .map(|event| self.publish(event))
            .collect()
    }
}

impl EventBus for DurableEventBus {
    fn publish(&mut self, event: Event) -> Result<EventReceipt, EvaError> {
        let record = self.log.append(event)?;
        let receipt = EventReceipt::from_record(&record);
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        self.log.ack(event_id, consumer)
    }

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
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::open_dir(layout.event_dir.join(DEAD_LETTER_DIR))
    }

    pub fn open_dir(root: impl Into<PathBuf>) -> Result<Self, EvaError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| {
            EvaError::internal("failed to create durable dead-letter directory")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;

        let mut records = load_dead_letters(&root)?;
        records.sort_by(|left, right| left.event_id().cmp(right.event_id()));
        validate_records(&records)?;

        Ok(Self { root, records })
    }

    pub fn records(&self) -> &[DeadLetterRecord] {
        &self.records
    }

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

    pub fn replay_all_for_publish(&mut self) -> Result<Vec<Event>, EvaError> {
        let mut events = Vec::with_capacity(self.records.len());
        for index in 0..self.records.len() {
            let mut record = self.records[index].clone();
            record.replay_count += 1;
            record.redrive.next_attempt_after_ms = record
                .redrive
                .retry_delay_ms
                .saturating_mul(record.replay_count as u64);
            let replay_id = EventId::parse(&format!(
                "{}:replay-{}",
                record.event.event_id().as_str(),
                record.replay_count
            ))?;
            let event = record
                .event
                .child_event(
                    replay_id,
                    record.event.topic().clone(),
                    record.event.payload().clone(),
                )
                .with_target(record.event.target().clone());
            self.persist(&record)?;
            self.records[index] = record;
            events.push(event);
        }
        Ok(events)
    }

    fn record_path(&self, event_id: &EventId) -> PathBuf {
        self.root.join(format!(
            "{}.{}",
            encode_path_segment(event_id.as_str()),
            DEAD_LETTER_EXTENSION
        ))
    }

    fn persist(&self, record: &DeadLetterRecord) -> Result<(), EvaError> {
        let path = self.record_path(record.event_id());
        fs::write(&path, dead_letter_to_storage(record)).map_err(|error| {
            EvaError::internal("failed to write durable dead-letter record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })
    }
}

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
        EvaError::conflict("dead-letter record field is missing").with_context("field", key)
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

fn optional_u64(fields: &BTreeMap<String, String>, key: &str) -> Result<Option<u64>, EvaError> {
    match fields.get(key).map(String::as_str) {
        Some(value) if !value.is_empty() => parse_u64(value, key).map(Some),
        _ => Ok(None),
    }
}

fn parse_usize(value: &str, field: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::conflict("dead-letter numeric field is invalid").with_context("field", field)
    })
}

fn parse_u64(value: &str, field: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("dead-letter numeric field is invalid").with_context("field", field)
    })
}

fn optional_u32(fields: &BTreeMap<String, String>, key: &str) -> Result<Option<u32>, EvaError> {
    match fields.get(key).map(String::as_str) {
        Some(value) if !value.is_empty() => value.parse::<u32>().map(Some).map_err(|_| {
            EvaError::conflict("dead-letter numeric field is invalid").with_context("field", key)
        }),
        _ => Ok(None),
    }
}

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

fn decode_string(value: &str) -> Result<String, EvaError> {
    String::from_utf8(hex_decode(value)?)
        .map_err(|_| EvaError::conflict("dead-letter string field is not utf-8"))
}

fn encode_path_segment(value: &str) -> String {
    hex_encode(value.as_bytes())
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
    use eva_core::{EventPayload, Topic};
    use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

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
                "eva-eventbus-durable-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
