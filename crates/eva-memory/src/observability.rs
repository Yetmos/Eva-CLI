//! 中文：记忆与知识操作的审计、追踪和指标写入连接层。
//! Audit and metric wiring for memory and knowledge operations.

use crate::memory_service::MemoryVisibility;
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, MetricKind, MetricLabels, MetricName,
    MetricPoint, MetricSink, SpanId, TraceFields,
};

/// 中文：本模块把一次记忆操作规范化为一条审计事件和两个低基数指标点。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "memory operation audit and metric wiring";

/// 中文：可观察性层识别的记忆操作类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOperation {
    /// 中文：写入或更新记忆记录。
    Write,
    /// 中文：按键读取记忆记录。
    Read,
    /// 中文：检索记忆或知识条目。
    Search,
    /// 中文：构建供模型使用的上下文窗口。
    Context,
}

/// 中文：记录一次记忆操作所需的结果、追踪和非敏感统计字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryObservation {
    /// 中文：本次观测对应的操作类别。
    pub operation: MemoryOperation,
    /// 中文：操作的审计结果。
    pub outcome: AuditOutcome,
    /// 中文：调用方已有的追踪字段基线。
    pub trace: TraceFields,
    /// 中文：执行或拥有数据的可选 Agent。
    pub agent_id: Option<AgentId>,
    /// 中文：关联整个调用链的可选请求标识。
    pub request_id: Option<RequestId>,
    /// 中文：记忆记录的可见性范围。
    pub visibility: Option<MemoryVisibility>,
    /// 中文：可选记录键；调用方应确保其本身不含秘密。
    pub key: Option<String>,
    /// 中文：检索查询长度，只记录数量而不记录查询原文。
    pub query_len: Option<usize>,
    /// 中文：读取、检索或上下文中涉及的条目数量。
    pub item_count: usize,
    /// 中文：在输出进入上下文前执行的敏感值替换次数。
    pub redaction_count: usize,
}

impl MemoryOperation {
    /// 中文：返回审计和指标标签使用的稳定操作名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Write => "memory.write",
            Self::Read => "memory.read",
            Self::Search => "memory.search",
            Self::Context => "memory.context",
        }
    }

    /// 中文：把记忆操作映射到对应的全局审计动作。
    pub const fn audit_action(self) -> AuditAction {
        match self {
            Self::Write => AuditAction::MemoryWrite,
            Self::Read => AuditAction::MemoryRead,
            Self::Search => AuditAction::MemorySearch,
            Self::Context => AuditAction::MemoryContext,
        }
    }

    /// 中文：返回该操作使用的稳定 Span 标识。
    pub const fn span_id(self) -> &'static str {
        match self {
            Self::Write => "memory.write",
            Self::Read => "memory.read",
            Self::Search => "memory.search",
            Self::Context => "memory.context",
        }
    }
}

impl MemoryObservation {
    /// 中文：创建默认成功、无可选主体字段和零计数的观测。
    pub fn new(operation: MemoryOperation, trace: TraceFields) -> Self {
        Self {
            operation,
            outcome: AuditOutcome::Ok,
            trace,
            agent_id: None,
            request_id: None,
            visibility: None,
            key: None,
            query_len: None,
            item_count: 0,
            redaction_count: 0,
        }
    }

    /// 中文：覆盖操作结果。
    pub fn with_outcome(mut self, outcome: AuditOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// 中文：附加 Agent 标识。
    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    /// 中文：附加请求标识。
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// 中文：附加记忆可见性范围。
    pub fn with_visibility(mut self, visibility: MemoryVisibility) -> Self {
        self.visibility = Some(visibility);
        self
    }

    /// 中文：附加非敏感记录键。
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// 中文：附加查询长度而不暴露查询文本。
    pub fn with_query_len(mut self, query_len: usize) -> Self {
        self.query_len = Some(query_len);
        self
    }

    /// 中文：设置操作涉及的条目数量。
    pub fn with_item_count(mut self, item_count: usize) -> Self {
        self.item_count = item_count;
        self
    }

    /// 中文：设置脱敏替换数量。
    pub fn with_redaction_count(mut self, redaction_count: usize) -> Self {
        self.redaction_count = redaction_count;
        self
    }
}

/// 中文：把观测依次写为审计事件、操作计数和脱敏计数指标。
///
/// 审计写入失败会阻止指标写入；第一个指标成功而第二个失败时不会回滚前者。调用方应把
/// 写入端视为尽力但可失败的边界，并依赖幂等后端或上层告警处理部分写入。
pub fn record_memory_observation<S>(
    sink: &mut S,
    observation: MemoryObservation,
) -> Result<(), EvaError>
where
    S: AuditSink + MetricSink,
{
    let trace = observation_trace(&observation)?;
    AuditSink::record(
        sink,
        audit_event(&observation, trace.clone()).with_message(observation.operation.as_str()),
    )?;
    MetricSink::record(
        sink,
        MetricPoint::new(
            MetricName::parse("memory.operation.count")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(metric_labels(&observation)),
    )?;
    MetricSink::record(
        sink,
        MetricPoint::new(
            MetricName::parse("memory.redaction.count")?,
            MetricKind::Counter,
            observation.redaction_count as f64,
        )
        .with_labels(metric_labels(&observation)),
    )
}

/// 中文：合并观测主体与已有追踪字段，并强制使用操作专属 Span。
fn observation_trace(observation: &MemoryObservation) -> Result<TraceFields, EvaError> {
    let mut trace = observation.trace.clone();
    if trace.request_id.is_none() {
        trace.request_id = observation.request_id.clone();
    }
    if trace.agent_id.is_none() {
        trace.agent_id = observation.agent_id.clone();
    }
    trace.span_id = Some(SpanId::parse(observation.operation.span_id())?);
    Ok(trace)
}

/// 中文：构造只包含非敏感元数据、计数和可见性的审计事件。
fn audit_event(observation: &MemoryObservation, trace: TraceFields) -> AuditEvent {
    let mut event = AuditEvent::new(
        observation.operation.audit_action(),
        observation.outcome,
        trace,
    )
    .with_field("operation", observation.operation.as_str())
    .with_field("item_count", observation.item_count.to_string())
    .with_field("redaction_count", observation.redaction_count.to_string());
    if let Some(agent_id) = &observation.agent_id {
        event = event.with_field("agent_id", agent_id.as_str());
    }
    if let Some(request_id) = &observation.request_id {
        event = event.with_field("request_id", request_id.as_str());
    }
    if let Some(visibility) = observation.visibility {
        event = event.with_field("visibility", visibility.as_str());
    }
    if let Some(key) = &observation.key {
        event = event.with_field("key", key);
    }
    if let Some(query_len) = observation.query_len {
        event = event.with_field("query_len", query_len.to_string());
    }
    event
}

/// 中文：构造操作、结果和有限主体字段组成的低基数指标标签。
fn metric_labels(observation: &MemoryObservation) -> MetricLabels {
    let mut labels = MetricLabels::new()
        .with("surface", "memory")
        .with("operation", observation.operation.as_str())
        .with("outcome", observation.outcome.as_str());
    if let Some(agent_id) = &observation.agent_id {
        labels = labels.with("agent_id", agent_id.as_str());
    }
    if let Some(visibility) = observation.visibility {
        labels = labels.with("visibility", visibility.as_str());
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_observability::{InMemoryAuditSink, InMemoryMetricSink};

    #[derive(Debug, Default)]
    /// 中文：同时收集审计和指标的组合测试写入端。
    struct TestSink {
        /// 中文：内存审计事件收集器。
        audit: InMemoryAuditSink,
        /// 中文：内存指标点收集器。
        metrics: InMemoryMetricSink,
    }

    impl AuditSink for TestSink {
        /// 中文：把审计事件转发到测试收集器。
        fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
            self.audit.record(event)
        }
    }

    impl MetricSink for TestSink {
        /// 中文：把指标点转发到测试收集器。
        fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
            self.metrics.record(point)
        }
    }

    #[test]
    /// 中文：验证一次上下文观测同时保留请求追踪并写入两个指标点。
    fn memory_observation_records_audit_and_metrics_with_trace() {
        let mut sink = TestSink::default();
        let request_id = RequestId::parse("req-memory-observe").unwrap();
        let agent_id = AgentId::parse("root-agent").unwrap();

        record_memory_observation(
            &mut sink,
            MemoryObservation::new(MemoryOperation::Context, TraceFields::default())
                .with_request_id(request_id.clone())
                .with_agent_id(agent_id.clone())
                .with_item_count(3)
                .with_redaction_count(1),
        )
        .unwrap();

        assert_eq!(sink.audit.events.len(), 1);
        assert_eq!(sink.audit.events[0].action, AuditAction::MemoryContext);
        assert_eq!(
            sink.audit.events[0].trace.request_id.as_ref(),
            Some(&request_id)
        );
        assert_eq!(
            sink.audit.events[0].trace.agent_id.as_ref(),
            Some(&agent_id)
        );
        assert_eq!(sink.metrics.points.len(), 2);
        assert_eq!(
            sink.metrics.points[0].name.as_str(),
            "memory.operation.count"
        );
        assert_eq!(sink.metrics.points[1].value, 1.0);
    }
}
