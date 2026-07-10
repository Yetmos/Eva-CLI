//! OpenTelemetry SDK exporter smoke wiring.

use crate::{MetricKind, MetricLabels, MetricPoint, TraceFields};
use eva_core::EvaError;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::trace::{SpanKind, Tracer, TracerProvider};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "http://localhost:4318";
const DEFAULT_BATCH_SIZE: usize = 32;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_METRIC_LABELS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTelemetryDropPolicy {
    DropNew,
    DropOldest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTelemetryExporterConfig {
    pub endpoint: String,
    pub auth_header: Option<String>,
    pub batch_size: usize,
    pub timeout_ms: u64,
    pub drop_policy: OpenTelemetryDropPolicy,
    pub max_metric_labels: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTelemetryExporterReport {
    pub endpoint: String,
    pub protocol: String,
    pub sdk: String,
    pub auth_configured: bool,
    pub batch_size: usize,
    pub timeout_ms: u64,
    pub drop_policy: String,
    pub max_metric_labels: usize,
    pub spans_attempted: usize,
    pub spans_exported: usize,
    pub metric_points_attempted: usize,
    pub metric_points_exported: usize,
    pub metric_points_dropped: usize,
    pub metric_labels_exported: usize,
    pub metric_labels_dropped: usize,
    pub degraded: bool,
    pub degraded_reasons: Vec<String>,
    pub continuity_key: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedMetricPoint {
    point: MetricPoint,
    labels: MetricLabels,
    dropped_labels: usize,
}

impl Default for OpenTelemetryExporterConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            auth_header: None,
            batch_size: DEFAULT_BATCH_SIZE,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            drop_policy: OpenTelemetryDropPolicy::DropNew,
            max_metric_labels: DEFAULT_MAX_METRIC_LABELS,
        }
    }
}

impl OpenTelemetryExporterConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            ..Self::default()
        }
    }

    pub fn with_endpoint(mut self, value: impl Into<String>) -> Self {
        self.endpoint = value.into();
        self
    }

    pub fn with_auth_header(mut self, value: impl Into<String>) -> Self {
        self.auth_header = Some(value.into());
        self
    }

    pub fn with_batch_size(mut self, value: usize) -> Self {
        self.batch_size = value;
        self
    }

    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = value;
        self
    }

    pub fn with_drop_policy(mut self, value: OpenTelemetryDropPolicy) -> Self {
        self.drop_policy = value;
        self
    }

    pub fn with_max_metric_labels(mut self, value: usize) -> Self {
        self.max_metric_labels = value;
        self
    }

    pub fn validate(&self) -> Result<(), EvaError> {
        if self.endpoint.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "OpenTelemetry endpoint cannot be empty",
            ));
        }
        if self.batch_size == 0 {
            return Err(EvaError::invalid_argument(
                "OpenTelemetry batch size must be greater than zero",
            ));
        }
        if self.timeout_ms == 0 {
            return Err(EvaError::invalid_argument(
                "OpenTelemetry timeout must be greater than zero",
            ));
        }
        if self.max_metric_labels == 0 {
            return Err(EvaError::invalid_argument(
                "OpenTelemetry max metric labels must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl OpenTelemetryDropPolicy {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "drop-new" | "drop_new" => Ok(Self::DropNew),
            "drop-oldest" | "drop_oldest" => Ok(Self::DropOldest),
            _ => Err(
                EvaError::invalid_argument("unknown OpenTelemetry drop policy")
                    .with_context("value", value)
                    .with_context("expected", "drop-new|drop-oldest"),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DropNew => "drop-new",
            Self::DropOldest => "drop-oldest",
        }
    }
}

impl fmt::Display for OpenTelemetryDropPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn run_opentelemetry_exporter_smoke(
    config: OpenTelemetryExporterConfig,
    trace: &TraceFields,
    metric_points: &[MetricPoint],
) -> Result<OpenTelemetryExporterReport, EvaError> {
    config.validate()?;

    let mut report = OpenTelemetryExporterReport {
        endpoint: config.endpoint.clone(),
        protocol: "http/protobuf".to_owned(),
        sdk: "opentelemetry-otlp".to_owned(),
        auth_configured: config
            .auth_header
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        batch_size: config.batch_size,
        timeout_ms: config.timeout_ms,
        drop_policy: config.drop_policy.as_str().to_owned(),
        max_metric_labels: config.max_metric_labels,
        spans_attempted: 1,
        spans_exported: 0,
        metric_points_attempted: metric_points.len(),
        metric_points_exported: 0,
        metric_points_dropped: 0,
        metric_labels_exported: 0,
        metric_labels_dropped: 0,
        degraded: false,
        degraded_reasons: Vec::new(),
        continuity_key: trace.continuity_key(),
    };

    let (prepared_metrics, dropped_points) = prepare_metric_points(metric_points, &config);
    report.metric_points_dropped = dropped_points;
    report.metric_labels_exported = prepared_metrics
        .iter()
        .map(|point| point.labels.entries().count())
        .sum();
    report.metric_labels_dropped = prepared_metrics
        .iter()
        .map(|point| point.dropped_labels)
        .sum();

    export_trace(&config, trace, &mut report);
    export_metrics(&config, &prepared_metrics, &mut report);

    report.degraded = !report.degraded_reasons.is_empty();
    Ok(report)
}

fn export_trace(
    config: &OpenTelemetryExporterConfig,
    trace: &TraceFields,
    report: &mut OpenTelemetryExporterReport,
) {
    let timeout = Duration::from_millis(config.timeout_ms);
    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(signal_endpoint(&config.endpoint, "v1/traces"))
        .with_timeout(timeout)
        .with_headers(headers(config))
        .build();
    let span_exporter = match span_exporter {
        Ok(exporter) => exporter,
        Err(error) => {
            record_degradation(report, format!("trace exporter build failed: {error}"));
            return;
        }
    };

    let batch_config = BatchConfigBuilder::default()
        .with_max_queue_size(config.batch_size.max(1))
        .with_max_export_batch_size(config.batch_size.max(1))
        .with_scheduled_delay(timeout)
        .build();
    let processor = BatchSpanProcessor::builder(span_exporter)
        .with_batch_config(batch_config)
        .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .build();
    let tracer = provider.tracer("eva.observability");
    let attributes = trace
        .entries()
        .into_iter()
        .map(|(key, value)| KeyValue::new(key, value))
        .chain([
            KeyValue::new("component", "observability"),
            KeyValue::new("exporter", "opentelemetry-otlp"),
        ])
        .collect::<Vec<_>>();

    tracer.in_span_with_builder(
        tracer
            .span_builder("eva.observability.otlp_smoke")
            .with_kind(SpanKind::Client)
            .with_attributes(attributes),
        |_| {},
    );

    match provider.force_flush().and_then(|_| provider.shutdown()) {
        Ok(()) => report.spans_exported = 1,
        Err(error) => record_degradation(report, format!("trace export failed: {error}")),
    }
}

fn export_metrics(
    config: &OpenTelemetryExporterConfig,
    metric_points: &[PreparedMetricPoint],
    report: &mut OpenTelemetryExporterReport,
) {
    if metric_points.is_empty() {
        return;
    }

    let timeout = Duration::from_millis(config.timeout_ms);
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(signal_endpoint(&config.endpoint, "v1/metrics"))
        .with_timeout(timeout)
        .with_headers(headers(config))
        .build();
    let metric_exporter = match metric_exporter {
        Ok(exporter) => exporter,
        Err(error) => {
            record_degradation(report, format!("metrics exporter build failed: {error}"));
            return;
        }
    };

    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .build();
    let meter = provider.meter("eva.observability");

    for prepared in metric_points {
        let labels = prepared
            .labels
            .entries()
            .map(|(key, value)| KeyValue::new(key.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        let name = prepared.point.name.as_str().to_owned();
        match prepared.point.kind {
            MetricKind::Counter => meter
                .f64_counter(name)
                .build()
                .add(prepared.point.value, &labels),
            MetricKind::Gauge => meter
                .f64_gauge(name)
                .build()
                .record(prepared.point.value, &labels),
            MetricKind::Histogram => meter
                .f64_histogram(name)
                .build()
                .record(prepared.point.value, &labels),
        }
    }

    match provider.force_flush().and_then(|_| provider.shutdown()) {
        Ok(()) => report.metric_points_exported = metric_points.len(),
        Err(error) => record_degradation(report, format!("metrics export failed: {error}")),
    }
}

fn prepare_metric_points(
    metric_points: &[MetricPoint],
    config: &OpenTelemetryExporterConfig,
) -> (Vec<PreparedMetricPoint>, usize) {
    let dropped_points = metric_points.len().saturating_sub(config.batch_size);
    let start = match config.drop_policy {
        OpenTelemetryDropPolicy::DropNew => 0,
        OpenTelemetryDropPolicy::DropOldest => dropped_points,
    };

    let prepared = metric_points
        .iter()
        .skip(start)
        .take(config.batch_size)
        .map(|point| {
            let (labels, dropped_labels) = point.labels.limited(config.max_metric_labels);
            PreparedMetricPoint {
                point: point.clone(),
                labels,
                dropped_labels,
            }
        })
        .collect::<Vec<_>>();

    (prepared, dropped_points)
}

fn headers(config: &OpenTelemetryExporterConfig) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    if let Some(value) = config
        .auth_header
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        headers.insert("authorization".to_owned(), value.to_owned());
    }
    headers
}

fn signal_endpoint(base: &str, signal_path: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with(signal_path) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/{signal_path}")
    }
}

fn record_degradation(report: &mut OpenTelemetryExporterReport, reason: String) {
    if !report.degraded_reasons.contains(&reason) {
        report.degraded_reasons.push(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MetricName, SpanId};
    use eva_core::RequestId;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Debug, Clone, Default)]
    struct FakeCollectorRecord {
        path: String,
        authorization: Option<String>,
        body_len: usize,
    }

    fn sample_trace() -> TraceFields {
        TraceFields::default()
            .with_request_id(RequestId::parse("req-otel-smoke").unwrap())
            .with_span_id(SpanId::parse("otel-smoke").unwrap())
    }

    fn sample_metrics() -> Vec<MetricPoint> {
        vec![
            MetricPoint::new(
                MetricName::parse("runtime.events.accepted").unwrap(),
                MetricKind::Counter,
                1.0,
            )
            .with_labels(
                MetricLabels::runtime("daemon", "gen-active")
                    .with("zeta", "drop")
                    .with("alpha", "keep"),
            ),
            MetricPoint::new(
                MetricName::parse("provider.invocations").unwrap(),
                MetricKind::Gauge,
                2.0,
            )
            .with_labels(MetricLabels::provider(
                "codex-cli",
                "code.review",
                "codex-cli",
            )),
            MetricPoint::new(
                MetricName::parse("task.completed").unwrap(),
                MetricKind::Histogram,
                3.0,
            )
            .with_labels(MetricLabels::task("completed", "root-agent")),
        ]
    }

    fn start_fake_collector(
        expected_requests: usize,
    ) -> (
        String,
        Arc<Mutex<Vec<FakeCollectorRecord>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let records = Arc::new(Mutex::new(Vec::new()));
        let thread_records = Arc::clone(&records);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if thread_records.lock().unwrap().len() >= expected_requests {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => handle_connection(stream, &thread_records),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        (endpoint, records, handle)
    }

    fn handle_connection(mut stream: TcpStream, records: &Arc<Mutex<Vec<FakeCollectorRecord>>>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut data = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    data.extend_from_slice(&buffer[..read]);
                    if request_complete(&data) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        let header_end = data
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap_or(data.len());
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let path = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("")
            .to_owned();
        let authorization = headers.lines().find_map(|line| {
            line.strip_prefix("authorization:")
                .or_else(|| line.strip_prefix("Authorization:"))
                .map(|value| value.trim().to_owned())
        });
        records.lock().unwrap().push(FakeCollectorRecord {
            path,
            authorization,
            body_len: data.len().saturating_sub(header_end + 4),
        });

        let _ =
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    }

    fn request_complete(data: &[u8]) -> bool {
        let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .or_else(|| {
                headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        data.len() >= header_end + 4 + content_length
    }

    #[test]
    fn opentelemetry_exporter_smoke_reaches_fake_collector() {
        let (endpoint, records, handle) = start_fake_collector(3);
        let report = run_opentelemetry_exporter_smoke(
            OpenTelemetryExporterConfig::new(endpoint).with_auth_header("Bearer test-token"),
            &sample_trace(),
            &sample_metrics()[..2],
        )
        .unwrap();

        handle.join().unwrap();
        let records = records.lock().unwrap();

        assert!(!report.degraded, "{report:?}");
        assert_eq!(report.spans_exported, 1);
        assert_eq!(report.metric_points_exported, 2);
        assert!(
            records.iter().any(|record| record.path == "/v1/traces"),
            "{records:?}"
        );
        assert!(
            records.iter().any(|record| record.path == "/v1/metrics"),
            "{records:?}"
        );
        assert!(records.iter().all(|record| record.body_len > 0));
        assert!(records
            .iter()
            .all(|record| record.authorization.as_deref() == Some("Bearer test-token")));
    }

    #[test]
    fn opentelemetry_exporter_degrades_when_collector_is_unavailable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let report = run_opentelemetry_exporter_smoke(
            OpenTelemetryExporterConfig::new(endpoint).with_timeout_ms(100),
            &sample_trace(),
            &sample_metrics()[..1],
        )
        .unwrap();

        assert!(report.degraded, "{report:?}");
        assert!(!report.degraded_reasons.is_empty());
    }

    #[test]
    fn opentelemetry_exporter_applies_batch_and_label_limits() {
        let (prepared, dropped_points) = prepare_metric_points(
            &sample_metrics(),
            &OpenTelemetryExporterConfig::default()
                .with_batch_size(2)
                .with_drop_policy(OpenTelemetryDropPolicy::DropOldest)
                .with_max_metric_labels(2),
        );

        assert_eq!(dropped_points, 1);
        assert_eq!(prepared.len(), 2);
        assert_eq!(
            prepared[0].point.name.as_str(),
            "provider.invocations",
            "drop-oldest keeps the newest metric points"
        );
        assert!(prepared
            .iter()
            .all(|point| point.labels.entries().count() <= 2));
        assert!(
            prepared
                .iter()
                .map(|point| point.dropped_labels)
                .sum::<usize>()
                > 0
        );
    }
}
