//! Best-effort file and OpenTelemetry-style observability backend adapters.

use crate::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, InMemoryAuditSink, InMemoryMetricSink,
    MetricPoint, MetricSink, TraceFields,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct BestEffortObservabilityPipeline {
    primary: Option<FileObservabilitySink>,
    fallback_audit: InMemoryAuditSink,
    fallback_metrics: InMemoryMetricSink,
    degraded_reasons: Vec<String>,
}

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
        Ok(Self { root })
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
        append_line(
            &self.root.join("otel-spans.jsonl"),
            &otel_span_json(name, trace, attributes),
        )
    }

    pub fn audit_path(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }

    pub fn metrics_path(&self) -> PathBuf {
        self.root.join("metrics.jsonl")
    }
}

impl AuditSink for FileObservabilitySink {
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        append_line(&self.audit_path(), &audit_event_json(&event))
    }
}

impl MetricSink for FileObservabilitySink {
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
        append_line(&self.metrics_path(), &metric_point_json(&point))
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
}
