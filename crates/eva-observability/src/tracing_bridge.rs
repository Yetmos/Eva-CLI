//! Bridge from `tracing` spans/events into Eva observability contracts.

use crate::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, SpanId,
    TraceFields,
};
use eva_core::{
    AdapterId, AgentId, CapabilityName, EvaError, EventId, GenerationId, RequestId, Topic,
};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{prelude::*, Registry};

const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TracingBridgeSink {
    Jsonl { backend_root: PathBuf },
    DevConsole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracingBridgeReport {
    pub sink: String,
    pub spans: usize,
    pub events: usize,
    pub audit_events: usize,
    pub exported_spans: usize,
    pub duplicate_span_ids: usize,
    pub degraded: bool,
    pub degraded_reasons: Vec<String>,
    pub dev_console_lines: usize,
    pub dev_console_preview: Vec<String>,
    pub continuity_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TracingBridgeLayer {
    state: Arc<Mutex<TracingBridgeState>>,
}

#[derive(Debug)]
struct TracingBridgeState {
    sink: BridgeSink,
    spans: usize,
    events: usize,
    audit_events: usize,
    exported_spans: usize,
    duplicate_span_ids: usize,
    next_span_sequence: u64,
    used_span_ids: BTreeSet<String>,
    dev_console_lines: Vec<String>,
    continuity_key: Option<String>,
}

#[derive(Debug)]
enum BridgeSink {
    Jsonl(BestEffortObservabilityPipeline),
    DevConsole,
}

#[derive(Debug, Clone)]
struct BridgeSpanData {
    trace: TraceFields,
}

#[derive(Debug, Default)]
struct FieldVisitor {
    fields: Vec<(String, String)>,
}

impl TracingBridgeSink {
    pub fn jsonl(root: impl AsRef<Path>) -> Self {
        Self::Jsonl {
            backend_root: root.as_ref().to_path_buf(),
        }
    }

    pub fn dev_console() -> Self {
        Self::DevConsole
    }
}

impl TracingBridgeLayer {
    pub fn new(sink: TracingBridgeSink, continuity_key: Option<String>) -> Self {
        let sink = match sink {
            TracingBridgeSink::Jsonl { backend_root } => {
                BridgeSink::Jsonl(BestEffortObservabilityPipeline::open(backend_root))
            }
            TracingBridgeSink::DevConsole => BridgeSink::DevConsole,
        };
        Self {
            state: Arc::new(Mutex::new(TracingBridgeState {
                sink,
                spans: 0,
                events: 0,
                audit_events: 0,
                exported_spans: 0,
                duplicate_span_ids: 0,
                next_span_sequence: 1,
                used_span_ids: BTreeSet::new(),
                dev_console_lines: Vec::new(),
                continuity_key,
            })),
        }
    }

    pub fn report(&self) -> TracingBridgeReport {
        self.state
            .lock()
            .map(|state| state.report())
            .unwrap_or_else(|_| TracingBridgeReport {
                sink: "poisoned".to_owned(),
                spans: 0,
                events: 0,
                audit_events: 0,
                exported_spans: 0,
                duplicate_span_ids: 0,
                degraded: true,
                degraded_reasons: vec!["tracing bridge state lock poisoned".to_owned()],
                dev_console_lines: 0,
                dev_console_preview: Vec::new(),
                continuity_key: None,
            })
    }
}

impl<S> Layer<S> for TracingBridgeLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let fields = collect_span_fields(attrs);
        let parent_trace = span_parent_trace(attrs, &ctx);
        let mut trace = trace_from_fields(parent_trace.unwrap_or_default(), &fields);
        let attributes = tracing_attributes(attrs.metadata(), &fields);

        if let Ok(mut state) = self.state.lock() {
            let span_id =
                state.next_span_id(field_value(&fields, "span_id"), attrs.metadata().name());
            trace.span_id = Some(span_id);
            state.record_span(attrs.metadata().name(), &trace, &attributes);
        }

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(BridgeSpanData { trace });
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let fields = collect_event_fields(event);
        let parent_trace = event_parent_trace(event, &ctx).unwrap_or_default();
        let trace = trace_from_fields(parent_trace, &fields);
        let action = audit_action_from_fields(&fields).unwrap_or(AuditAction::RuntimeControl);
        let outcome = audit_outcome_from_fields(&fields).unwrap_or(AuditOutcome::Ok);
        let message = field_value(&fields, "message")
            .map(str::to_owned)
            .unwrap_or_else(|| event.metadata().name().to_owned());
        let mut audit = AuditEvent::new(action, outcome, trace.clone()).with_message(message);

        for (key, value) in &fields {
            if !is_reserved_event_field(key) {
                audit = audit.with_field(key, value);
            }
        }
        audit = audit
            .with_field("tracing.target", event.metadata().target())
            .with_field("tracing.level", event.metadata().level().to_string());

        if let Ok(mut state) = self.state.lock() {
            state.record_event(&audit);
        }
    }
}

impl TracingBridgeState {
    fn next_span_id(&mut self, requested: Option<&str>, span_name: &str) -> SpanId {
        let mut candidate = requested
            .and_then(valid_span_id_string)
            .or_else(|| valid_span_id_string(span_name))
            .unwrap_or_else(|| self.generated_span_id());

        if self.used_span_ids.contains(&candidate) {
            self.duplicate_span_ids += 1;
            candidate = format!("{candidate}.{}", self.next_span_sequence);
            self.next_span_sequence += 1;
        }

        if SpanId::parse(&candidate).is_err() {
            candidate = self.generated_span_id();
        }

        self.used_span_ids.insert(candidate.clone());
        SpanId::parse(&candidate).expect("generated tracing span id is stable")
    }

    fn generated_span_id(&mut self) -> String {
        let value = format!("tracing.span.{}", self.next_span_sequence);
        self.next_span_sequence += 1;
        value
    }

    fn record_span(&mut self, name: &str, trace: &TraceFields, attributes: &[(String, String)]) {
        self.spans += 1;
        self.exported_spans += 1;
        match &mut self.sink {
            BridgeSink::Jsonl(pipeline) => {
                let attribute_refs = attributes
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect::<Vec<_>>();
                let _ = pipeline.export_span(name, trace, &attribute_refs);
            }
            BridgeSink::DevConsole => {
                self.dev_console_lines
                    .push(dev_console_span_line(name, trace, attributes));
            }
        }
    }

    fn record_event(&mut self, audit: &AuditEvent) {
        self.events += 1;
        self.audit_events += 1;
        match &mut self.sink {
            BridgeSink::Jsonl(pipeline) => {
                let _ = AuditSink::record(pipeline, audit.clone());
            }
            BridgeSink::DevConsole => {
                self.dev_console_lines.push(dev_console_event_line(audit));
            }
        }
    }

    fn report(&self) -> TracingBridgeReport {
        let (sink, degraded, degraded_reasons) = match &self.sink {
            BridgeSink::Jsonl(pipeline) => (
                pipeline.backend_root(),
                pipeline.degraded(),
                pipeline.degraded_reasons().to_vec(),
            ),
            BridgeSink::DevConsole => ("dev-console".to_owned(), false, Vec::new()),
        };
        TracingBridgeReport {
            sink,
            spans: self.spans,
            events: self.events,
            audit_events: self.audit_events,
            exported_spans: self.exported_spans,
            duplicate_span_ids: self.duplicate_span_ids,
            degraded,
            degraded_reasons,
            dev_console_lines: self.dev_console_lines.len(),
            dev_console_preview: self.dev_console_lines.iter().take(4).cloned().collect(),
            continuity_key: self.continuity_key.clone(),
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_owned());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }
}

impl FieldVisitor {
    fn push(&mut self, field: &Field, value: String) {
        self.fields.push((
            field.name().to_owned(),
            sanitize_value(field.name(), &value),
        ));
    }
}

pub fn run_tracing_bridge_smoke(
    sink: TracingBridgeSink,
    trace: &TraceFields,
) -> Result<TracingBridgeReport, EvaError> {
    let layer = TracingBridgeLayer::new(sink, trace.continuity_key());
    let subscriber = Registry::default().with(layer.clone());
    let request_id = trace
        .request_id
        .as_ref()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_else(|| "req-tracing-bridge".to_owned());

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "cli.observability.tracing_bridge",
            request_id = %request_id,
            span_id = "cli.observability.tracing_bridge",
            component = "cli",
            secret_token = "sk-tracing-bridge"
        );
        let _entered = span.enter();
        tracing::info!(
            audit_action = "runtime.control",
            audit_outcome = "ok",
            provider = "observability",
            token = "sk-bridge-secret",
            "tracing bridge smoke"
        );
    });

    Ok(layer.report())
}

fn collect_span_fields(attrs: &tracing::span::Attributes<'_>) -> Vec<(String, String)> {
    let mut visitor = FieldVisitor::default();
    attrs.record(&mut visitor);
    visitor.fields
}

fn collect_event_fields(event: &Event<'_>) -> Vec<(String, String)> {
    let mut visitor = FieldVisitor::default();
    event.record(&mut visitor);
    visitor.fields
}

fn span_parent_trace<S>(
    attrs: &tracing::span::Attributes<'_>,
    ctx: &Context<'_, S>,
) -> Option<TraceFields>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    attrs
        .parent()
        .and_then(|parent| span_trace_by_id(ctx, parent))
        .or_else(|| {
            ctx.lookup_current()
                .and_then(|span| span.extensions().get::<BridgeSpanData>().cloned())
                .map(|data| data.trace)
        })
}

fn event_parent_trace<S>(event: &Event<'_>, ctx: &Context<'_, S>) -> Option<TraceFields>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    ctx.event_scope(event).and_then(|scope| {
        scope
            .from_root()
            .last()
            .and_then(|span| span.extensions().get::<BridgeSpanData>().cloned())
            .map(|data| data.trace)
    })
}

fn span_trace_by_id<S>(ctx: &Context<'_, S>, id: &Id) -> Option<TraceFields>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    ctx.span(id)
        .and_then(|span| span.extensions().get::<BridgeSpanData>().cloned())
        .map(|data| data.trace)
}

fn tracing_attributes(
    metadata: &tracing::Metadata<'_>,
    fields: &[(String, String)],
) -> Vec<(String, String)> {
    let mut attributes = vec![
        ("tracing.target".to_owned(), metadata.target().to_owned()),
        ("tracing.level".to_owned(), metadata.level().to_string()),
    ];
    attributes.extend(fields.iter().cloned());
    attributes
}

fn trace_from_fields(mut trace: TraceFields, fields: &[(String, String)]) -> TraceFields {
    if let Some(value) = field_value(fields, "event_id").and_then(parse_event_id) {
        trace.event_id = Some(value);
    }
    if let Some(value) = field_value(fields, "request_id").and_then(parse_request_id) {
        trace.request_id = Some(value);
    }
    if let Some(value) = field_value(fields, "topic").and_then(parse_topic) {
        trace.topic = Some(value);
    }
    if let Some(value) = field_value(fields, "agent_id").and_then(parse_agent_id) {
        trace.agent_id = Some(value);
    }
    if let Some(value) = field_value(fields, "adapter_id").and_then(parse_adapter_id) {
        trace.adapter_id = Some(value);
    }
    if let Some(value) = field_value(fields, "capability").and_then(parse_capability) {
        trace.capability = Some(value);
    }
    if let Some(value) = field_value(fields, "provider") {
        trace.provider = Some(value.to_owned());
    }
    if let Some(value) = field_value(fields, "correlation_id").and_then(parse_event_id) {
        trace.correlation_id = Some(value);
    }
    if let Some(value) = field_value(fields, "causation_id").and_then(parse_event_id) {
        trace.causation_id = Some(value);
    }
    if let Some(value) = field_value(fields, "generation_id").and_then(parse_generation_id) {
        trace.generation_id = Some(value);
    }
    if let Some(value) = field_value(fields, "span_id").and_then(parse_span_id) {
        trace.span_id = Some(value);
    }
    trace
}

fn audit_action_from_fields(fields: &[(String, String)]) -> Option<AuditAction> {
    field_value(fields, "audit_action").and_then(AuditAction::from_stable_name)
}

fn audit_outcome_from_fields(fields: &[(String, String)]) -> Option<AuditOutcome> {
    match field_value(fields, "audit_outcome")? {
        "ok" => Some(AuditOutcome::Ok),
        "planned" => Some(AuditOutcome::Planned),
        "blocked" => Some(AuditOutcome::Blocked),
        "failed" => Some(AuditOutcome::Failed),
        _ => None,
    }
}

fn field_value<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn is_reserved_event_field(key: &str) -> bool {
    matches!(key, "audit_action" | "audit_outcome" | "message") || is_trace_key(key)
}

fn is_trace_key(key: &str) -> bool {
    matches!(
        key,
        "event_id"
            | "request_id"
            | "topic"
            | "agent_id"
            | "adapter_id"
            | "capability"
            | "provider"
            | "correlation_id"
            | "causation_id"
            | "generation_id"
            | "span_id"
    )
}

fn sanitize_value(key: &str, value: &str) -> String {
    let key = key.to_ascii_lowercase();
    let value_lower = value.to_ascii_lowercase();
    if key_contains_secret(&key) || value_contains_secret(&value_lower) {
        REDACTED.to_owned()
    } else {
        value.to_owned()
    }
}

fn key_contains_secret(key: &str) -> bool {
    [
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|fragment| key.contains(fragment))
}

fn value_contains_secret(value: &str) -> bool {
    value.starts_with("sk-")
        || value.contains(" sk-")
        || value.contains("token=")
        || value.contains("token:")
        || value.contains("secret=")
        || value.contains("secret:")
        || value.contains("password=")
        || value.contains("password:")
        || value.contains("authorization=")
        || value.contains("authorization:")
}

fn valid_span_id_string(value: &str) -> Option<String> {
    SpanId::parse(value).ok().map(|_| value.to_owned())
}

fn parse_span_id(value: &str) -> Option<SpanId> {
    SpanId::parse(value).ok()
}

fn parse_event_id(value: &str) -> Option<EventId> {
    EventId::parse(value).ok()
}

fn parse_request_id(value: &str) -> Option<RequestId> {
    RequestId::parse(value).ok()
}

fn parse_agent_id(value: &str) -> Option<AgentId> {
    AgentId::parse(value).ok()
}

fn parse_adapter_id(value: &str) -> Option<AdapterId> {
    AdapterId::parse(value).ok()
}

fn parse_generation_id(value: &str) -> Option<GenerationId> {
    GenerationId::parse(value).ok()
}

fn parse_capability(value: &str) -> Option<CapabilityName> {
    CapabilityName::parse(value).ok()
}

fn parse_topic(value: &str) -> Option<Topic> {
    Topic::parse(value).ok()
}

fn dev_console_span_line(
    name: &str,
    trace: &TraceFields,
    attributes: &[(String, String)],
) -> String {
    format!(
        "span name={} trace={} attributes={}",
        name,
        entries_text(trace.entries()),
        pairs_text(
            attributes
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
        )
    )
}

fn dev_console_event_line(event: &AuditEvent) -> String {
    format!(
        "event action={} outcome={} trace={} message={} fields={}",
        event.action.as_str(),
        event.outcome.as_str(),
        entries_text(event.trace.entries()),
        event.message.as_deref().unwrap_or_default(),
        pairs_text(
            event
                .fields
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
        )
    )
}

fn entries_text(entries: Vec<(&'static str, String)>) -> String {
    pairs_text(entries.iter().map(|(key, value)| (*key, value.as_str())))
}

fn pairs_text<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "eva-tracing-bridge-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn tracing_bridge_writes_jsonl_audit_and_spans() {
        let root = temp_root("jsonl");
        let trace =
            TraceFields::default().with_request_id(RequestId::parse("req-tracing-jsonl").unwrap());

        let report = run_tracing_bridge_smoke(TracingBridgeSink::jsonl(&root), &trace).unwrap();

        assert_eq!(report.spans, 1);
        assert_eq!(report.events, 1);
        assert_eq!(report.audit_events, 1);
        assert_eq!(report.exported_spans, 1);
        assert_eq!(report.duplicate_span_ids, 0);
        assert!(!report.degraded);
        assert_eq!(
            report.continuity_key.as_deref(),
            Some("request_id:req-tracing-jsonl")
        );
        let audit = fs::read_to_string(root.join("audit.jsonl")).unwrap();
        let spans = fs::read_to_string(root.join("otel-spans.jsonl")).unwrap();
        assert!(audit.contains("\"action\":\"runtime.control\""));
        assert!(audit.contains("\"request_id\":\"req-tracing-jsonl\""));
        assert!(!audit.contains("sk-bridge-secret"));
        assert!(audit.contains(REDACTED));
        assert!(spans.contains("\"name\":\"cli.observability.tracing_bridge\""));
        assert!(!spans.contains("sk-tracing-bridge"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tracing_bridge_dev_console_is_redacted() {
        let trace =
            TraceFields::default().with_request_id(RequestId::parse("req-tracing-dev").unwrap());

        let report = run_tracing_bridge_smoke(TracingBridgeSink::dev_console(), &trace).unwrap();

        assert_eq!(report.sink, "dev-console");
        assert_eq!(report.dev_console_lines, 2);
        let preview = report.dev_console_preview.join("\n");
        assert!(preview.contains("request_id=req-tracing-dev"));
        assert!(preview.contains(REDACTED));
        assert!(!preview.contains("sk-"));
    }

    #[test]
    fn tracing_bridge_uniquifies_duplicate_span_ids() {
        let layer = TracingBridgeLayer::new(TracingBridgeSink::dev_console(), None);
        let subscriber = Registry::default().with(layer.clone());

        tracing::subscriber::with_default(subscriber, || {
            let first = tracing::info_span!("runtime.duplicate", span_id = "runtime.same");
            let _first = first.enter();
            let second = tracing::info_span!("runtime.duplicate.child", span_id = "runtime.same");
            let _second = second.enter();
            tracing::info!(audit_action = "runtime.control", "duplicate span id smoke");
        });

        let report = layer.report();
        assert_eq!(report.spans, 2);
        assert_eq!(report.duplicate_span_ids, 1);
        assert_eq!(report.events, 1);
    }
}
