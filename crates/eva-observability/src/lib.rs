//! Observability boundary for tracing, metrics, and audit.

pub mod audit;
pub mod backend;
pub mod metrics;
pub mod trace;

pub use audit::{AuditAction, AuditEvent, AuditOutcome, AuditSink, InMemoryAuditSink};
pub use backend::{
    BestEffortObservabilityPipeline, FileObservabilitySink, ObservabilitySmokeReport,
};
pub use metrics::{
    InMemoryMetricSink, MetricKind, MetricLabels, MetricName, MetricPoint, MetricSink,
};
pub use trace::{SpanId, TraceFields};
