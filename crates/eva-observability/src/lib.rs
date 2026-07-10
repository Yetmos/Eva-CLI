//! Observability boundary for tracing, metrics, and audit.

pub mod audit;
pub mod backend;
pub mod metrics;
pub mod trace;
pub mod tracing_bridge;

pub use audit::{AuditAction, AuditEvent, AuditOutcome, AuditSink, InMemoryAuditSink};
pub use backend::{
    BestEffortObservabilityPipeline, FileObservabilitySink, ObservabilitySmokeReport,
};
pub use metrics::{
    InMemoryMetricSink, MetricKind, MetricLabels, MetricName, MetricPoint, MetricSink,
};
pub use trace::{SpanId, TraceFields};
pub use tracing_bridge::{
    run_tracing_bridge_smoke, TracingBridgeLayer, TracingBridgeReport, TracingBridgeSink,
};
