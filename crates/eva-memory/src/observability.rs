//! Audit and metric wiring for memory and knowledge operations.

use crate::memory_service::MemoryVisibility;
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, MetricKind, MetricLabels, MetricName,
    MetricPoint, MetricSink, SpanId, TraceFields,
};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "memory operation audit and metric wiring";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOperation {
    Write,
    Read,
    Search,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryObservation {
    pub operation: MemoryOperation,
    pub outcome: AuditOutcome,
    pub trace: TraceFields,
    pub agent_id: Option<AgentId>,
    pub request_id: Option<RequestId>,
    pub visibility: Option<MemoryVisibility>,
    pub key: Option<String>,
    pub query_len: Option<usize>,
    pub item_count: usize,
    pub redaction_count: usize,
}

impl MemoryOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Write => "memory.write",
            Self::Read => "memory.read",
            Self::Search => "memory.search",
            Self::Context => "memory.context",
        }
    }

    pub const fn audit_action(self) -> AuditAction {
        match self {
            Self::Write => AuditAction::MemoryWrite,
            Self::Read => AuditAction::MemoryRead,
            Self::Search => AuditAction::MemorySearch,
            Self::Context => AuditAction::MemoryContext,
        }
    }

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

    pub fn with_outcome(mut self, outcome: AuditOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    pub fn with_visibility(mut self, visibility: MemoryVisibility) -> Self {
        self.visibility = Some(visibility);
        self
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn with_query_len(mut self, query_len: usize) -> Self {
        self.query_len = Some(query_len);
        self
    }

    pub fn with_item_count(mut self, item_count: usize) -> Self {
        self.item_count = item_count;
        self
    }

    pub fn with_redaction_count(mut self, redaction_count: usize) -> Self {
        self.redaction_count = redaction_count;
        self
    }
}

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
    struct TestSink {
        audit: InMemoryAuditSink,
        metrics: InMemoryMetricSink,
    }

    impl AuditSink for TestSink {
        fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
            self.audit.record(event)
        }
    }

    impl MetricSink for TestSink {
        fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
            self.metrics.record(point)
        }
    }

    #[test]
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
