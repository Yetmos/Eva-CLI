//! 将 `tracing` span/event 字段映射为 Eva trace、审计事件及 JSONL 或开发控制台输出。
//!
//! span 继承显式父级或当前上下文，再由同名字段覆盖可解析的 trace 标识；审计保留字段不会
//! 重复写入普通 fields。所有字段先脱敏后进入共享状态。状态由单一互斥锁保护，锁中毒时
//! report 明确降级，而当前 span/event 回调会跳过记录以避免观测失败影响业务线程。
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

/// 定义 `REDACTED` 常量。
const REDACTED: &str = "[REDACTED]";

/// 定义 `TracingBridgeSink` 可取的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TracingBridgeSink {
    /// 表示 `Jsonl` 枚举分支。
    Jsonl {
        /// 保存尽力 JSONL 管道的根目录。
        backend_root: PathBuf,
    },
    /// 表示 `DevConsole` 枚举分支。
    DevConsole,
}

/// 表示 `TracingBridgeReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracingBridgeReport {
    /// 记录 `sink` 字段对应的值。
    pub sink: String,
    /// 记录 `spans` 字段对应的值。
    pub spans: usize,
    /// 记录 `events` 字段对应的值。
    pub events: usize,
    /// 记录 `audit_events` 字段对应的值。
    pub audit_events: usize,
    /// 记录 `exported_spans` 字段对应的值。
    pub exported_spans: usize,
    /// 记录 `duplicate_span_ids` 字段对应的值。
    pub duplicate_span_ids: usize,
    /// 记录 `degraded` 字段对应的值。
    pub degraded: bool,
    /// 记录 `degraded_reasons` 字段对应的值。
    pub degraded_reasons: Vec<String>,
    /// 记录 `dev_console_lines` 字段对应的值。
    pub dev_console_lines: usize,
    /// 记录 `dev_console_preview` 字段对应的值。
    pub dev_console_preview: Vec<String>,
    /// 记录 `continuity_key` 字段对应的值。
    pub continuity_key: Option<String>,
}

/// 可克隆的 tracing layer；所有克隆共享同一计数、span 标识集合和 sink 锁。
#[derive(Debug, Clone)]
pub struct TracingBridgeLayer {
    /// 串行保护 span 标识分配与输出；持锁期间可能执行文件 I/O。
    state: Arc<Mutex<TracingBridgeState>>,
}

/// 表示 `TracingBridgeState` 数据结构。
#[derive(Debug)]
struct TracingBridgeState {
    /// 记录 `sink` 字段对应的值。
    sink: BridgeSink,
    /// 记录 `spans` 字段对应的值。
    spans: usize,
    /// 记录 `events` 字段对应的值。
    events: usize,
    /// 记录 `audit_events` 字段对应的值。
    audit_events: usize,
    /// 记录 `exported_spans` 字段对应的值。
    exported_spans: usize,
    /// 记录 `duplicate_span_ids` 字段对应的值。
    duplicate_span_ids: usize,
    /// 记录 `next_span_sequence` 字段对应的值。
    next_span_sequence: u64,
    /// 记录 `used_span_ids` 字段对应的值。
    used_span_ids: BTreeSet<String>,
    /// 记录 `dev_console_lines` 字段对应的值。
    dev_console_lines: Vec<String>,
    /// 记录 `continuity_key` 字段对应的值。
    continuity_key: Option<String>,
}

/// 定义 `BridgeSink` 可取的状态。
#[derive(Debug)]
enum BridgeSink {
    /// 表示 `Jsonl` 枚举分支。
    Jsonl(
        /// 保存不会把后端错误传播到 tracing 回调的尽力管道。
        BestEffortObservabilityPipeline,
    ),
    /// 表示 `DevConsole` 枚举分支。
    DevConsole,
}

/// 表示 `BridgeSpanData` 数据结构。
#[derive(Debug, Clone)]
struct BridgeSpanData {
    /// 记录 `trace` 字段对应的值。
    trace: TraceFields,
}

/// 表示 `FieldVisitor` 数据结构。
#[derive(Debug, Default)]
struct FieldVisitor {
    /// 记录 `fields` 字段对应的值。
    fields: Vec<(String, String)>,
}

/// 为相关类型实现其约定的行为与方法。
impl TracingBridgeSink {
    /// 执行 `jsonl` 对应的处理逻辑。
    pub fn jsonl(root: impl AsRef<Path>) -> Self {
        Self::Jsonl {
            backend_root: root.as_ref().to_path_buf(),
        }
    }

    /// 执行 `dev_console` 对应的处理逻辑。
    pub fn dev_console() -> Self {
        Self::DevConsole
    }
}

/// 为相关类型实现其约定的行为与方法。
impl TracingBridgeLayer {
    /// 创建并初始化当前类型的实例。
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

    /// 返回一致的共享快照；锁中毒时不 panic，而是构造显式 degraded 报告。
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

/// 为相关类型实现其约定的行为与方法。
impl<S> Layer<S> for TracingBridgeLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    /// 收集并脱敏字段，继承父 trace，分配唯一 span id 后输出，并把 trace 附到 span 扩展。
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

        // 即使 sink 锁中毒，仍附加已解析的 trace，使子事件尽可能保持上下文连续。
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(BridgeSpanData { trace });
        }
    }

    /// 将事件字段映射为审计动作、结果和 trace；未知动作/结果分别回退到控制与成功语义。
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

/// 为相关类型实现其约定的行为与方法。
impl TracingBridgeState {
    /// 优先采用有效显式 id 或 span 名；重复项追加序号，无效结果回退到内部稳定序列。
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

    /// 执行 `generated_span_id` 对应的处理逻辑。
    fn generated_span_id(&mut self) -> String {
        let value = format!("tracing.span.{}", self.next_span_sequence);
        self.next_span_sequence += 1;
        value
    }

    /// 在锁内更新计数并同步写 sink；尽力管道失败只反映在后续 degraded 报告中。
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

    /// 在锁内累计事件并输出审计；回调不传播 sink 错误，避免 tracing 影响业务控制流。
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

    /// 返回 `report` 对应的数据视图。
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

/// 为相关类型实现其约定的行为与方法。
impl Visit for FieldVisitor {
    /// 写入或导出 `record_debug` 对应的可观测性记录。
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    /// 写入或导出 `record_str` 对应的可观测性记录。
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_owned());
    }

    /// 写入或导出 `record_bool` 对应的可观测性记录。
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }

    /// 写入或导出 `record_i64` 对应的可观测性记录。
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    /// 写入或导出 `record_u64` 对应的可观测性记录。
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }
}

/// 为相关类型实现其约定的行为与方法。
impl FieldVisitor {
    /// 在保存字段前按字段名和常见凭据值模式脱敏，后续所有映射只接触安全值。
    fn push(&mut self, field: &Field, value: String) {
        self.fields.push((
            field.name().to_owned(),
            sanitize_value(field.name(), &value),
        ));
    }
}

/// 执行 `run_tracing_bridge_smoke` 对应的受控流程。
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

/// 执行 `collect_span_fields` 对应的受控流程。
fn collect_span_fields(attrs: &tracing::span::Attributes<'_>) -> Vec<(String, String)> {
    let mut visitor = FieldVisitor::default();
    attrs.record(&mut visitor);
    visitor.fields
}

/// 执行 `collect_event_fields` 对应的受控流程。
fn collect_event_fields(event: &Event<'_>) -> Vec<(String, String)> {
    let mut visitor = FieldVisitor::default();
    event.record(&mut visitor);
    visitor.fields
}

/// 显式父 span 优先于当前 span；两者都缺少桥接扩展时由调用方使用空 trace。
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

/// 执行 `event_parent_trace` 对应的处理逻辑。
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

/// 执行 `span_trace_by_id` 对应的处理逻辑。
fn span_trace_by_id<S>(ctx: &Context<'_, S>, id: &Id) -> Option<TraceFields>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    ctx.span(id)
        .and_then(|span| span.extensions().get::<BridgeSpanData>().cloned())
        .map(|data| data.trace)
}

/// 执行 `tracing_attributes` 对应的处理逻辑。
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

/// 用可成功解析的保留字段覆盖继承 trace；无效标识被忽略并保留父级值。
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

/// 执行 `audit_action_from_fields` 对应的处理逻辑。
fn audit_action_from_fields(fields: &[(String, String)]) -> Option<AuditAction> {
    field_value(fields, "audit_action").and_then(AuditAction::from_stable_name)
}

/// 执行 `audit_outcome_from_fields` 对应的处理逻辑。
fn audit_outcome_from_fields(fields: &[(String, String)]) -> Option<AuditOutcome> {
    match field_value(fields, "audit_outcome")? {
        "ok" => Some(AuditOutcome::Ok),
        "planned" => Some(AuditOutcome::Planned),
        "blocked" => Some(AuditOutcome::Blocked),
        "failed" => Some(AuditOutcome::Failed),
        _ => None,
    }
}

/// 返回 `field_value` 对应的数据视图。
fn field_value<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// 判断 `is_reserved_event_field` 对应的条件是否成立。
fn is_reserved_event_field(key: &str) -> bool {
    matches!(key, "audit_action" | "audit_outcome" | "message") || is_trace_key(key)
}

/// 判断 `is_trace_key` 对应的条件是否成立。
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

/// 按不区分大小写的键和值模式整值脱敏，不尝试保留凭据的任何前后缀。
fn sanitize_value(key: &str, value: &str) -> String {
    let key = key.to_ascii_lowercase();
    let value_lower = value.to_ascii_lowercase();
    if key_contains_secret(&key) || value_contains_secret(&value_lower) {
        REDACTED.to_owned()
    } else {
        value.to_owned()
    }
}

/// 判断 `key_contains_secret` 对应的条件是否成立。
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

/// 判断 `value_contains_secret` 对应的条件是否成立。
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

/// 判断 `valid_span_id_string` 对应的条件是否成立。
fn valid_span_id_string(value: &str) -> Option<String> {
    SpanId::parse(value).ok().map(|_| value.to_owned())
}

/// 解析或检查 `parse_span_id` 对应的数据，并报告无效格式。
fn parse_span_id(value: &str) -> Option<SpanId> {
    SpanId::parse(value).ok()
}

/// 解析或检查 `parse_event_id` 对应的数据，并报告无效格式。
fn parse_event_id(value: &str) -> Option<EventId> {
    EventId::parse(value).ok()
}

/// 解析或检查 `parse_request_id` 对应的数据，并报告无效格式。
fn parse_request_id(value: &str) -> Option<RequestId> {
    RequestId::parse(value).ok()
}

/// 解析或检查 `parse_agent_id` 对应的数据，并报告无效格式。
fn parse_agent_id(value: &str) -> Option<AgentId> {
    AgentId::parse(value).ok()
}

/// 解析或检查 `parse_adapter_id` 对应的数据，并报告无效格式。
fn parse_adapter_id(value: &str) -> Option<AdapterId> {
    AdapterId::parse(value).ok()
}

/// 解析或检查 `parse_generation_id` 对应的数据，并报告无效格式。
fn parse_generation_id(value: &str) -> Option<GenerationId> {
    GenerationId::parse(value).ok()
}

/// 解析或检查 `parse_capability` 对应的数据，并报告无效格式。
fn parse_capability(value: &str) -> Option<CapabilityName> {
    CapabilityName::parse(value).ok()
}

/// 解析或检查 `parse_topic` 对应的数据，并报告无效格式。
fn parse_topic(value: &str) -> Option<Topic> {
    Topic::parse(value).ok()
}

/// 执行 `dev_console_span_line` 对应的处理逻辑。
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

/// 执行 `dev_console_event_line` 对应的处理逻辑。
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

/// 按稳定格式生成 `entries_text` 对应的输出。
fn entries_text(entries: Vec<(&'static str, String)>) -> String {
    pairs_text(entries.iter().map(|(key, value)| (*key, value.as_str())))
}

/// 按稳定格式生成 `pairs_text` 对应的输出。
fn pairs_text<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 执行 `temp_root` 对应的处理逻辑。
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

    /// 验证 `tracing_bridge_writes_jsonl_audit_and_spans` 场景下的预期行为。
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

    /// 验证 `tracing_bridge_dev_console_is_redacted` 场景下的预期行为。
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

    /// 验证 `tracing_bridge_uniquifies_duplicate_span_ids` 场景下的预期行为。
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
