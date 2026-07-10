//! Durable audit sink backed by the filesystem durable backend layout.

use crate::DurableBackendLayout;
use eva_core::EvaError;
use eva_observability::{
    AuditEvent, AuditSink, ObservabilityCorruptRecordPolicy, ObservabilityRetentionPolicy,
    ObservabilityRetentionReport, ObservabilitySinkPolicyKind,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable audit event storage and trace lookup";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub sequence: u64,
    pub recorded_at_ms: u128,
    pub action: String,
    pub outcome: String,
    pub message: Option<String>,
    pub trace: Vec<(String, String)>,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemAuditSink {
    audit_dir: PathBuf,
    records: Vec<AuditRecord>,
    next_sequence: u64,
}

impl FileSystemAuditSink {
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::from_audit_dir(&layout.audit_dir)
    }

    pub fn open_with_policy(
        layout: &DurableBackendLayout,
        policy: &ObservabilityRetentionPolicy,
        now_ms: u128,
    ) -> Result<(Self, ObservabilityRetentionReport), EvaError> {
        Self::from_audit_dir_with_policy(&layout.audit_dir, policy, now_ms)
    }

    pub fn from_audit_dir(audit_dir: impl AsRef<Path>) -> Result<Self, EvaError> {
        let audit_dir = audit_dir.as_ref().to_path_buf();
        fs::create_dir_all(&audit_dir).map_err(|error| {
            EvaError::internal("failed to create audit directory")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let records = load_records(&audit_dir)?;
        let next_sequence = next_sequence_after(&audit_dir, &records)?;
        Ok(Self {
            audit_dir,
            records,
            next_sequence,
        })
    }

    pub fn from_audit_dir_with_policy(
        audit_dir: impl AsRef<Path>,
        policy: &ObservabilityRetentionPolicy,
        now_ms: u128,
    ) -> Result<(Self, ObservabilityRetentionReport), EvaError> {
        policy.validate()?;
        if policy.sink_kind != ObservabilitySinkPolicyKind::DurableAudit {
            return Err(EvaError::invalid_argument(
                "filesystem audit sink requires durable-audit retention policy",
            )
            .with_context("sink_kind", policy.sink_kind.as_str()));
        }
        let audit_dir = audit_dir.as_ref().to_path_buf();
        fs::create_dir_all(&audit_dir).map_err(|error| {
            EvaError::internal("failed to create audit directory")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let (records, report) = load_records_with_policy(&audit_dir, policy, now_ms)?;
        let next_sequence = next_sequence_after(&audit_dir, &records)?;
        Ok((
            Self {
                audit_dir,
                records,
                next_sequence,
            },
            report,
        ))
    }

    pub fn audit_dir(&self) -> &Path {
        &self.audit_dir
    }

    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }

    pub fn query_by_trace_id(&self, trace_id: &str) -> Vec<AuditRecord> {
        self.records
            .iter()
            .filter(|record| {
                record.trace.iter().any(|(key, value)| {
                    matches!(
                        key.as_str(),
                        "span_id" | "request_id" | "event_id" | "correlation_id" | "causation_id"
                    ) && value == trace_id
                })
            })
            .cloned()
            .collect()
    }

    fn persist_record(&self, record: &AuditRecord) -> Result<(), EvaError> {
        let path = record_path(&self.audit_dir, record.sequence);
        fs::write(&path, record.to_storage()).map_err(|error| {
            EvaError::internal("failed to write audit record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })
    }
}

impl AuditSink for FileSystemAuditSink {
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let record = AuditRecord::from_event(sequence, &event);
        self.persist_record(&record)?;
        self.records.push(record);
        Ok(())
    }
}

impl AuditRecord {
    fn from_event(sequence: u64, event: &AuditEvent) -> Self {
        Self {
            sequence,
            recorded_at_ms: system_time_millis(event.recorded_at),
            action: event.action.as_str().to_owned(),
            outcome: event.outcome.as_str().to_owned(),
            message: event.message.clone(),
            trace: event
                .trace
                .entries()
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
            fields: event.fields.clone(),
        }
    }

    fn to_storage(&self) -> String {
        let mut lines = vec![
            "version=1".to_owned(),
            format!("sequence={}", self.sequence),
            format!("recorded_at_ms={}", self.recorded_at_ms),
            format!("action={}", encode_field(&self.action)),
            format!("outcome={}", encode_field(&self.outcome)),
            format!(
                "message={}",
                self.message
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
        ];
        lines.extend(
            self.trace
                .iter()
                .map(|(key, value)| format!("trace={}|{}", encode_field(key), encode_field(value))),
        );
        lines.extend(
            self.fields
                .iter()
                .map(|(key, value)| format!("field={}|{}", encode_field(key), encode_field(value))),
        );
        lines.push(String::new());
        lines.join("\n")
    }

    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut version = None;
        let mut sequence = None;
        let mut recorded_at_ms = None;
        let mut action = None;
        let mut outcome = None;
        let mut message = None;
        let mut trace = Vec::new();
        let mut fields = Vec::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            if let Some(value) = line.strip_prefix("version=") {
                version = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("sequence=") {
                sequence = Some(parse_u64("sequence", value)?);
            } else if let Some(value) = line.strip_prefix("recorded_at_ms=") {
                recorded_at_ms = Some(parse_u128("recorded_at_ms", value)?);
            } else if let Some(value) = line.strip_prefix("action=") {
                action = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("outcome=") {
                outcome = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("message=") {
                message = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("trace=") {
                trace.push(parse_pair("trace", value)?);
            } else if let Some(value) = line.strip_prefix("field=") {
                fields.push(parse_pair("field", value)?);
            } else {
                return Err(EvaError::conflict("audit record has unknown field"));
            }
        }

        if version.as_deref() != Some("1") {
            return Err(EvaError::conflict("audit record version is unsupported"));
        }

        Ok(Self {
            sequence: sequence
                .ok_or_else(|| EvaError::conflict("audit record missing sequence"))?,
            recorded_at_ms: recorded_at_ms
                .ok_or_else(|| EvaError::conflict("audit record missing recorded_at_ms"))?,
            action: action.ok_or_else(|| EvaError::conflict("audit record missing action"))?,
            outcome: outcome.ok_or_else(|| EvaError::conflict("audit record missing outcome"))?,
            message,
            trace,
            fields,
        })
    }
}

fn load_records(audit_dir: &Path) -> Result<Vec<AuditRecord>, EvaError> {
    let mut records = Vec::new();
    for entry in fs::read_dir(audit_dir).map_err(|error| {
        EvaError::internal("failed to read audit directory")
            .with_context("path", audit_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read audit directory entry")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("audit") {
            continue;
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::internal("failed to read audit record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        records.push(
            AuditRecord::from_storage(&data)
                .map_err(|error| error.with_context("path", path.display().to_string()))?,
        );
    }
    records.sort_by_key(|record| record.sequence);
    Ok(records)
}

fn load_records_with_policy(
    audit_dir: &Path,
    policy: &ObservabilityRetentionPolicy,
    now_ms: u128,
) -> Result<(Vec<AuditRecord>, ObservabilityRetentionReport), EvaError> {
    let mut records = Vec::new();
    let mut report = ObservabilityRetentionReport::new(policy.sink_kind);
    for entry in fs::read_dir(audit_dir).map_err(|error| {
        EvaError::internal("failed to read audit directory")
            .with_context("path", audit_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read audit directory entry")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("audit") {
            continue;
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::internal("failed to read audit record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let record = match AuditRecord::from_storage(&data) {
            Ok(record) => record,
            Err(error) => match policy.corrupt_record_policy {
                ObservabilityCorruptRecordPolicy::SkipAndReport => {
                    report.record_corrupt_file(path.display().to_string(), 1);
                    continue;
                }
                ObservabilityCorruptRecordPolicy::FailFast => {
                    return Err(error.with_context("path", path.display().to_string()));
                }
            },
        };
        if record
            .recorded_at_ms
            .saturating_add(policy.retain_for_ms as u128)
            < now_ms
        {
            fs::remove_file(&path).map_err(|error| {
                EvaError::internal("failed to delete expired audit record")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            report.deleted_files += 1;
            continue;
        }
        report.retained_files += 1;
        records.push(record);
    }
    records.sort_by_key(|record| record.sequence);
    Ok((records, report))
}

fn record_path(audit_dir: &Path, sequence: u64) -> PathBuf {
    audit_dir.join(format!("{sequence:020}.audit"))
}

fn next_sequence_after(audit_dir: &Path, records: &[AuditRecord]) -> Result<u64, EvaError> {
    let mut max_sequence = records
        .iter()
        .map(|record| record.sequence)
        .max()
        .unwrap_or(0);
    for entry in fs::read_dir(audit_dir).map_err(|error| {
        EvaError::internal("failed to read audit directory")
            .with_context("path", audit_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read audit directory entry")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("audit") {
            continue;
        }
        if let Some(sequence) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<u64>().ok())
        {
            max_sequence = max_sequence.max(sequence);
        }
    }
    Ok(max_sequence + 1)
}

fn system_time_millis(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn parse_pair(field: &'static str, value: &str) -> Result<(String, String), EvaError> {
    let parts = value.split('|').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err(EvaError::conflict("audit record pair has invalid arity")
            .with_context("field", field)
            .with_context("actual", parts.len().to_string()));
    }
    Ok((decode_field(&parts[0]), decode_field(&parts[1])))
}

fn parse_u64(field: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("audit record number is invalid")
            .with_context("field", field)
            .with_context("value", value)
    })
}

fn parse_u128(field: &'static str, value: &str) -> Result<u128, EvaError> {
    value.parse::<u128>().map_err(|_| {
        EvaError::conflict("audit record number is invalid")
            .with_context("field", field)
            .with_context("value", value)
    })
}

fn decode_optional_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(decode_field(value))
    }
}

fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
        .replace('|', "%7C")
        .replace('=', "%3D")
}

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
mod tests {
    use super::*;
    use eva_observability::{AuditAction, AuditOutcome, SpanId, TraceFields};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn filesystem_audit_sink_round_trips_and_queries_trace() {
        let root = test_root("round-trip");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let trace = TraceFields::default()
            .with_request_id(eva_core::RequestId::parse("req-audit-1").unwrap())
            .with_span_id(SpanId::parse("span-audit-1").unwrap());
        let event = AuditEvent::new(AuditAction::RuntimeStarted, AuditOutcome::Ok, trace)
            .with_message("runtime started")
            .with_field("generation", "basic-v1.0");
        let mut sink = FileSystemAuditSink::open(backend.layout()).unwrap();

        sink.record(event).unwrap();
        let reopened = FileSystemAuditSink::open(backend.layout()).unwrap();
        let records = reopened.query_by_trace_id("span-audit-1");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[0].action, "runtime.started");
        assert_eq!(records[0].outcome, "ok");
        assert_eq!(records[0].message.as_deref(), Some("runtime started"));
        assert_eq!(
            records[0].fields,
            vec![("generation".to_owned(), "basic-v1.0".to_owned())]
        );
        assert!(backend
            .layout()
            .audit_dir
            .join("00000000000000000001.audit")
            .is_file());
    }

    #[test]
    fn filesystem_audit_sink_rejects_corrupt_record() {
        let root = test_root("corrupt");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        fs::write(
            backend
                .layout()
                .audit_dir
                .join("00000000000000000001.audit"),
            "version=1\nsequence=1\nrecorded_at_ms=1\naction=runtime.started\n",
        )
        .unwrap();

        let error = FileSystemAuditSink::open(backend.layout()).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn filesystem_audit_policy_skips_corrupt_and_deletes_only_expired_records() {
        let root = test_root("policy");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let audit_dir = &backend.layout().audit_dir;
        let old = AuditRecord {
            sequence: 1,
            recorded_at_ms: 1,
            action: "runtime.started".to_owned(),
            outcome: "ok".to_owned(),
            message: None,
            trace: Vec::new(),
            fields: Vec::new(),
        };
        let fresh = AuditRecord {
            sequence: 2,
            recorded_at_ms: 9_000,
            action: "runtime.started".to_owned(),
            outcome: "ok".to_owned(),
            message: None,
            trace: Vec::new(),
            fields: Vec::new(),
        };
        fs::write(
            audit_dir.join("00000000000000000001.audit"),
            old.to_storage(),
        )
        .unwrap();
        fs::write(
            audit_dir.join("00000000000000000002.audit"),
            fresh.to_storage(),
        )
        .unwrap();
        fs::write(
            audit_dir.join("00000000000000000003.audit"),
            "version=1\nsequence=3\n",
        )
        .unwrap();
        fs::write(audit_dir.join("notes.txt"), "not audit").unwrap();

        let policy = ObservabilityRetentionPolicy::durable_audit()
            .with_retain_for_ms(5_000)
            .with_corrupt_record_policy(ObservabilityCorruptRecordPolicy::SkipAndReport);
        let (mut sink, report) =
            FileSystemAuditSink::open_with_policy(backend.layout(), &policy, 10_000).unwrap();

        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.skipped_corrupt_records, 1);
        assert_eq!(sink.records().len(), 1);
        assert_eq!(sink.records()[0].sequence, 2);
        assert!(!audit_dir.join("00000000000000000001.audit").exists());
        assert!(audit_dir.join("00000000000000000002.audit").exists());
        assert!(audit_dir.join("00000000000000000003.audit").exists());
        assert!(audit_dir.join("notes.txt").exists());

        sink.record(AuditEvent::new(
            AuditAction::RuntimeStarted,
            AuditOutcome::Ok,
            TraceFields::default(),
        ))
        .unwrap();
        assert!(
            audit_dir.join("00000000000000000004.audit").exists(),
            "next sequence must not overwrite skipped corrupt record"
        );
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
                "eva-storage-audit-store-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
