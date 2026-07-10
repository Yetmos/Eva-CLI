//! Best-effort file and OpenTelemetry-style observability backend adapters.

use crate::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, InMemoryAuditSink, InMemoryMetricSink,
    MetricPoint, MetricSink, ObservabilityCorruptRecordPolicy, ObservabilityRetentionPolicy,
    ObservabilityRetentionReport, ObservabilitySinkPolicyKind, TraceFields,
};
use eva_core::EvaError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "best-effort production observability backend adapters";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileObservabilitySink {
    root: PathBuf,
    retention_policy: Option<ObservabilityRetentionPolicy>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BestEffortObservabilityPipeline {
    primary: Option<FileObservabilitySink>,
    fallback_audit: InMemoryAuditSink,
    fallback_metrics: InMemoryMetricSink,
    degraded_reasons: Vec<String>,
}

const JSONL_FILES: [&str; 3] = ["audit.jsonl", "metrics.jsonl", "otel-spans.jsonl"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilitySmokeReport {
    pub backend_root: String,
    pub degraded: bool,
    pub degraded_reasons: Vec<String>,
    pub audit_events: usize,
    pub metric_points: usize,
    pub otel_spans: usize,
    pub continuity_key: Option<String>,
}

impl FileObservabilitySink {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, EvaError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|error| {
            EvaError::internal("failed to create observability backend directory")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        Ok(Self {
            root,
            retention_policy: None,
        })
    }

    pub fn open_with_policy(
        root: impl AsRef<Path>,
        policy: ObservabilityRetentionPolicy,
    ) -> Result<Self, EvaError> {
        policy.validate()?;
        if policy.sink_kind != ObservabilitySinkPolicyKind::JsonlFile {
            return Err(EvaError::invalid_argument(
                "file observability sink requires jsonl-file retention policy",
            )
            .with_context("sink_kind", policy.sink_kind.as_str()));
        }
        let mut sink = Self::open(root)?;
        sink.retention_policy = Some(policy);
        Ok(sink)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn export_span(
        &mut self,
        name: &str,
        trace: &TraceFields,
        attributes: &[(&str, &str)],
    ) -> Result<(), EvaError> {
        self.append_jsonl("otel-spans.jsonl", &otel_span_json(name, trace, attributes))
    }

    pub fn audit_path(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }

    pub fn metrics_path(&self) -> PathBuf {
        self.root.join("metrics.jsonl")
    }

    pub fn apply_retention_policy(&self) -> Result<ObservabilityRetentionReport, EvaError> {
        self.apply_retention_policy_at(system_time_millis(SystemTime::now()) as u64)
    }

    pub fn apply_retention_policy_at(
        &self,
        now_ms: u64,
    ) -> Result<ObservabilityRetentionReport, EvaError> {
        let policy = self.retention_policy.clone().unwrap_or_default();
        apply_jsonl_retention_policy_at(&self.root, &policy, now_ms)
    }

    fn append_jsonl(&mut self, file_name: &str, line: &str) -> Result<(), EvaError> {
        let path = self.root.join(file_name);
        if let Some(policy) = &self.retention_policy {
            rotate_if_needed(&path, policy, line.len().saturating_add(1) as u64)?;
        }
        append_line(&path, line)
    }
}

impl AuditSink for FileObservabilitySink {
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        self.append_jsonl("audit.jsonl", &audit_event_json(&event))
    }
}

impl MetricSink for FileObservabilitySink {
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
        self.append_jsonl("metrics.jsonl", &metric_point_json(&point))
    }
}

impl BestEffortObservabilityPipeline {
    pub fn open(root: impl AsRef<Path>) -> Self {
        let backend_root = root.as_ref().display().to_string();
        match FileObservabilitySink::open(root) {
            Ok(primary) => Self {
                primary: Some(primary),
                fallback_audit: InMemoryAuditSink::default(),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: Vec::new(),
            },
            Err(error) => Self {
                primary: None,
                fallback_audit: degraded_sink_event(format!(
                    "observability backend unavailable at {backend_root}: {}",
                    error.message()
                )),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: vec![error.message().to_owned()],
            },
        }
    }

    pub fn open_with_policy(root: impl AsRef<Path>, policy: ObservabilityRetentionPolicy) -> Self {
        let backend_root = root.as_ref().display().to_string();
        match FileObservabilitySink::open_with_policy(root, policy) {
            Ok(primary) => Self {
                primary: Some(primary),
                fallback_audit: InMemoryAuditSink::default(),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: Vec::new(),
            },
            Err(error) => Self {
                primary: None,
                fallback_audit: degraded_sink_event(format!(
                    "observability backend unavailable at {backend_root}: {}",
                    error.message()
                )),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: vec![error.message().to_owned()],
            },
        }
    }

    pub fn degraded(&self) -> bool {
        !self.degraded_reasons.is_empty()
    }

    pub fn degraded_reasons(&self) -> &[String] {
        &self.degraded_reasons
    }

    pub fn fallback_audit(&self) -> &InMemoryAuditSink {
        &self.fallback_audit
    }

    pub fn fallback_metrics(&self) -> &InMemoryMetricSink {
        &self.fallback_metrics
    }

    pub fn backend_root(&self) -> String {
        self.primary
            .as_ref()
            .map(|sink| sink.root().display().to_string())
            .unwrap_or_else(|| "degraded".to_owned())
    }

    pub fn export_span(
        &mut self,
        name: &str,
        trace: &TraceFields,
        attributes: &[(&str, &str)],
    ) -> Result<(), EvaError> {
        if let Some(primary) = &mut self.primary {
            if let Err(error) = primary.export_span(name, trace, attributes) {
                self.record_degradation(error.message().to_owned());
            }
        } else {
            self.record_degradation("observability backend is unavailable".to_owned());
        }
        Ok(())
    }

    pub fn smoke_report(
        &self,
        backend_root: impl Into<String>,
        continuity_key: Option<String>,
    ) -> ObservabilitySmokeReport {
        ObservabilitySmokeReport {
            backend_root: backend_root.into(),
            degraded: self.degraded(),
            degraded_reasons: self.degraded_reasons.clone(),
            audit_events: count_lines(self.primary.as_ref().map(|sink| sink.audit_path()))
                .unwrap_or(self.fallback_audit.events.len()),
            metric_points: count_lines(self.primary.as_ref().map(|sink| sink.metrics_path()))
                .unwrap_or(self.fallback_metrics.points.len()),
            otel_spans: count_lines(
                self.primary
                    .as_ref()
                    .map(|sink| sink.root().join("otel-spans.jsonl")),
            )
            .unwrap_or(0),
            continuity_key,
        }
    }

    fn record_degradation(&mut self, reason: String) {
        if !self.degraded_reasons.contains(&reason) {
            self.degraded_reasons.push(reason.clone());
        }
        let _ = self.fallback_audit.record(
            AuditEvent::new(
                AuditAction::RuntimeRecovered,
                AuditOutcome::Planned,
                TraceFields::default(),
            )
            .with_message("observability backend degraded")
            .with_field("reason", reason),
        );
    }
}

fn apply_jsonl_retention_policy_at(
    root: &Path,
    policy: &ObservabilityRetentionPolicy,
    now_ms: u64,
) -> Result<ObservabilityRetentionReport, EvaError> {
    policy.validate()?;
    if policy.sink_kind != ObservabilitySinkPolicyKind::JsonlFile {
        return Err(EvaError::invalid_argument(
            "JSONL retention policy requires jsonl-file sink kind",
        )
        .with_context("sink_kind", policy.sink_kind.as_str()));
    }
    let mut report = ObservabilityRetentionReport::new(policy.sink_kind);
    if !root.exists() {
        return Ok(report);
    }

    let mut rotated_by_signal: Vec<(String, u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        EvaError::internal("failed to read observability backend directory")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read observability backend entry")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };

        if JSONL_FILES.contains(&file_name) {
            inspect_jsonl_file(&path, policy, &mut report)?;
            report.retained_files += 1;
            continue;
        }

        if let Some((signal, rotated_at_ms)) = parse_rotated_jsonl_name(file_name) {
            report.rotated_files += 1;
            inspect_jsonl_file(&path, policy, &mut report)?;
            if rotated_at_ms.saturating_add(policy.retain_for_ms) < now_ms {
                fs::remove_file(&path).map_err(|error| {
                    EvaError::internal("failed to delete expired observability file")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
                report.deleted_files += 1;
            } else {
                report.retained_files += 1;
                rotated_by_signal.push((signal, rotated_at_ms, path));
            }
        }
    }

    for signal in ["audit", "metrics", "otel-spans"] {
        let count = rotated_by_signal
            .iter()
            .filter(|(candidate, _, _)| candidate == signal)
            .count();
        if count > policy.max_rotated_files {
            report.warn(format!(
                "{signal} rotated file count {count} exceeds max_rotated_files {}",
                policy.max_rotated_files
            ));
        }
    }

    Ok(report)
}

impl AuditSink for BestEffortObservabilityPipeline {
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        if let Some(primary) = &mut self.primary {
            if let Err(error) = AuditSink::record(primary, event.clone()) {
                self.record_degradation(error.message().to_owned());
                self.fallback_audit.record(event)?;
            }
        } else {
            self.fallback_audit.record(event)?;
        }
        Ok(())
    }
}

impl MetricSink for BestEffortObservabilityPipeline {
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
        if let Some(primary) = &mut self.primary {
            if let Err(error) = MetricSink::record(primary, point.clone()) {
                self.record_degradation(error.message().to_owned());
                self.fallback_metrics.record(point)?;
            }
        } else {
            self.fallback_metrics.record(point)?;
        }
        Ok(())
    }
}

fn degraded_sink_event(reason: String) -> InMemoryAuditSink {
    let mut sink = InMemoryAuditSink::default();
    let _ = sink.record(
        AuditEvent::new(
            AuditAction::RuntimeRecovered,
            AuditOutcome::Planned,
            TraceFields::default(),
        )
        .with_message("observability backend degraded")
        .with_field("reason", reason),
    );
    sink
}

fn append_line(path: &Path, line: &str) -> Result<(), EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            EvaError::internal("failed to create observability output directory")
                .with_context("path", parent.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            EvaError::internal("failed to open observability output")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    writeln!(file, "{line}").map_err(|error| {
        EvaError::internal("failed to write observability output")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

fn rotate_if_needed(
    path: &Path,
    policy: &ObservabilityRetentionPolicy,
    incoming_bytes: u64,
) -> Result<(), EvaError> {
    policy.validate()?;
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() == 0 || metadata.len().saturating_add(incoming_bytes) <= policy.max_file_bytes
    {
        return Ok(());
    }

    let rotated = next_rotated_path(path, system_time_millis(SystemTime::now()) as u64)?;
    fs::rename(path, &rotated).map_err(|error| {
        EvaError::internal("failed to rotate observability output")
            .with_context("from", path.display().to_string())
            .with_context("to", rotated.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

fn next_rotated_path(path: &Path, rotated_at_ms: u64) -> Result<PathBuf, EvaError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| EvaError::invalid_argument("observability output path has no file name"))?;
    let signal = file_name
        .strip_suffix(".jsonl")
        .ok_or_else(|| EvaError::invalid_argument("observability output path is not JSONL"))?;

    for suffix in 0..1000 {
        let candidate = if suffix == 0 {
            parent.join(format!("{signal}.{rotated_at_ms}.jsonl"))
        } else {
            parent.join(format!("{signal}.{rotated_at_ms}.{suffix}.jsonl"))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(EvaError::conflict(
        "failed to allocate unique observability rotation path",
    ))
}

fn parse_rotated_jsonl_name(file_name: &str) -> Option<(String, u64)> {
    let base = file_name.strip_suffix(".jsonl")?;
    let parts = base.split('.').collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let signal = parts[0].to_owned();
    if !matches!(signal.as_str(), "audit" | "metrics" | "otel-spans") {
        return None;
    }
    let rotated_at_ms = parts.get(1)?.parse::<u64>().ok()?;
    Some((signal, rotated_at_ms))
}

fn inspect_jsonl_file(
    path: &Path,
    policy: &ObservabilityRetentionPolicy,
    report: &mut ObservabilityRetentionReport,
) -> Result<(), EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::internal("failed to read observability JSONL file")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let corrupt = data
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !looks_like_json_object(line))
        .count();
    if corrupt == 0 {
        return Ok(());
    }

    match policy.corrupt_record_policy {
        ObservabilityCorruptRecordPolicy::SkipAndReport => {
            report.record_corrupt_file(path.display().to_string(), corrupt);
            Ok(())
        }
        ObservabilityCorruptRecordPolicy::FailFast => Err(EvaError::conflict(
            "observability JSONL file contains corrupt records",
        )
        .with_context("path", path.display().to_string())
        .with_context("corrupt_records", corrupt.to_string())),
    }
}

fn looks_like_json_object(line: &str) -> bool {
    let value = line.trim();
    value.starts_with('{') && value.ends_with('}')
}

fn audit_event_json(event: &AuditEvent) -> String {
    format!(
        "{{\"recorded_at_ms\":{},\"action\":{},\"outcome\":{},\"trace\":{},\"message\":{},\"fields\":{}}}",
        system_time_millis(event.recorded_at),
        json_string(event.action.as_str()),
        json_string(event.outcome.as_str()),
        trace_json(&event.trace),
        option_json(event.message.as_deref()),
        pairs_json(event.fields.iter().map(|(key, value)| (key.as_str(), value.as_str())))
    )
}

fn metric_point_json(point: &MetricPoint) -> String {
    format!(
        "{{\"name\":{},\"kind\":{},\"value\":{},\"labels\":{}}}",
        json_string(point.name.as_str()),
        json_string(point.kind.as_str()),
        point.value,
        pairs_json(point.labels.entries())
    )
}

fn otel_span_json(name: &str, trace: &TraceFields, attributes: &[(&str, &str)]) -> String {
    format!(
        "{{\"exporter\":\"opentelemetry-jsonl\",\"name\":{},\"trace\":{},\"attributes\":{}}}",
        json_string(name),
        trace_json(trace),
        pairs_json(attributes.iter().copied())
    )
}

fn trace_json(trace: &TraceFields) -> String {
    pairs_json(
        trace
            .entries()
            .iter()
            .map(|(key, value)| (*key, value.as_str())),
    )
}

fn pairs_json<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let entries = pairs
        .into_iter()
        .map(|(key, value)| format!("{}:{}", json_string(key), json_string(value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", entries.join(","))
}

fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

fn json_string(value: &str) -> String {
    let mut escaped = String::new();
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn count_lines(path: Option<PathBuf>) -> Option<usize> {
    let path = path?;
    let data = fs::read_to_string(path).ok()?;
    Some(data.lines().filter(|line| !line.trim().is_empty()).count())
}

fn system_time_millis(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MetricKind, MetricLabels, MetricName, SpanId};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "eva-observability-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn file_observability_sink_writes_audit_metrics_and_otel_span() {
        let root = temp_root("file");
        let mut sink = FileObservabilitySink::open(&root).unwrap();
        let trace = TraceFields::default().with_span_id(SpanId::parse("span-obs").unwrap());

        AuditSink::record(
            &mut sink,
            AuditEvent::new(AuditAction::RuntimeStarted, AuditOutcome::Ok, trace.clone())
                .with_message("runtime observed"),
        )
        .unwrap();
        MetricSink::record(
            &mut sink,
            MetricPoint::new(
                MetricName::parse("runtime.events.accepted").unwrap(),
                MetricKind::Counter,
                1.0,
            )
            .with_labels(MetricLabels::runtime("basic", "active")),
        )
        .unwrap();
        sink.export_span("runtime.start", &trace, &[("component", "runtime")])
            .unwrap();

        assert!(root.join("audit.jsonl").is_file());
        assert!(root.join("metrics.jsonl").is_file());
        assert!(root.join("otel-spans.jsonl").is_file());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn best_effort_pipeline_degrades_without_failing() {
        let root = temp_root("degraded");
        fs::write(&root, b"not a directory").unwrap();
        let mut pipeline = BestEffortObservabilityPipeline::open(&root);

        AuditSink::record(
            &mut pipeline,
            AuditEvent::new(
                AuditAction::RuntimeStarted,
                AuditOutcome::Ok,
                TraceFields::default(),
            ),
        )
        .unwrap();

        assert!(pipeline.degraded());
        assert!(!pipeline.fallback_audit().events.is_empty());
        fs::remove_file(root).ok();
    }

    #[test]
    fn file_observability_policy_rotates_and_continues_writing() {
        let root = temp_root("rotation");
        let policy = ObservabilityRetentionPolicy::jsonl_file()
            .with_max_file_bytes(160)
            .with_max_rotated_files(4);
        let mut sink = FileObservabilitySink::open_with_policy(&root, policy).unwrap();
        let trace = TraceFields::default().with_span_id(SpanId::parse("span-rotate").unwrap());

        for index in 0..4 {
            AuditSink::record(
                &mut sink,
                AuditEvent::new(AuditAction::RuntimeStarted, AuditOutcome::Ok, trace.clone())
                    .with_message(format!("runtime observed {index}")),
            )
            .unwrap();
        }

        assert!(root.join("audit.jsonl").is_file());
        let rotated = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("audit."))
            .count();
        assert!(rotated > 0, "expected rotated audit JSONL file");
        let report = sink.apply_retention_policy().unwrap();
        assert_eq!(report.deleted_files, 0);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn jsonl_retention_deletes_only_expired_observability_files_and_reports_corrupt_records() {
        let root = temp_root("retention");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("audit.jsonl"), "{\"active\":true}\n").unwrap();
        fs::write(root.join("audit.1.jsonl"), "{\"expired\":true}\n").unwrap();
        fs::write(root.join("metrics.9000.jsonl"), "not-json\n{\"ok\":true}\n").unwrap();
        fs::write(root.join("unrelated.1.jsonl"), "{\"keep\":true}\n").unwrap();
        let sink = FileObservabilitySink::open_with_policy(
            &root,
            ObservabilityRetentionPolicy::jsonl_file()
                .with_retain_for_ms(5_000)
                .with_corrupt_record_policy(ObservabilityCorruptRecordPolicy::SkipAndReport),
        )
        .unwrap();

        let report = sink.apply_retention_policy_at(10_000).unwrap();

        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.rotated_files, 2);
        assert_eq!(report.skipped_corrupt_records, 1);
        assert!(report
            .corrupt_files
            .iter()
            .any(|path| path.ends_with("metrics.9000.jsonl")));
        assert!(!root.join("audit.1.jsonl").exists());
        assert!(root.join("audit.jsonl").exists());
        assert!(root.join("metrics.9000.jsonl").exists());
        assert!(root.join("unrelated.1.jsonl").exists());
        fs::remove_dir_all(root).ok();
    }
}
