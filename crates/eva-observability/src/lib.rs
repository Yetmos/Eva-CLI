//! 中文：追踪、指标、审计、后端桥接和保留策略的统一可观察性边界。
//! Observability boundary for tracing, metrics, and audit.

pub mod audit;
pub mod backend;
pub mod metrics;
pub mod opentelemetry_exporter;
pub mod retention;
pub mod trace;
pub mod tracing_bridge;

pub use audit::{AuditAction, AuditEvent, AuditOutcome, AuditSink, InMemoryAuditSink};
pub use backend::{
    BestEffortObservabilityPipeline, FileObservabilitySink, ObservabilitySmokeReport,
};
pub use metrics::{
    InMemoryMetricSink, MetricKind, MetricLabels, MetricName, MetricPoint, MetricSink,
};
pub use opentelemetry_exporter::{
    run_opentelemetry_exporter_smoke, OpenTelemetryDropPolicy, OpenTelemetryExporterConfig,
    OpenTelemetryExporterReport,
};
pub use retention::{
    ObservabilityCorruptRecordPolicy, ObservabilityRetentionPolicy, ObservabilityRetentionReport,
    ObservabilitySinkPolicyKind,
};
pub use trace::{SpanId, TraceFields};
pub use tracing_bridge::{
    run_tracing_bridge_smoke, TracingBridgeLayer, TracingBridgeReport, TracingBridgeSink,
};
