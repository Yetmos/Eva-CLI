//! 使用 OpenTelemetry OTLP/HTTP SDK 执行一次有界 trace 与 metrics 导出并生成降级报告。
//!
//! 配置错误直接返回失败；collector 构建、刷新或关闭失败则记录降级并返回报告。指标先按
//! 批大小选择保留头部或尾部，再限制每点标签数；trace 的 SDK 批处理队列同样受批大小约束，
//! `force_flush` 与 `shutdown` 均成功后才计为已导出。
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

/// 定义 `DEFAULT_ENDPOINT` 常量。
const DEFAULT_ENDPOINT: &str = "http://localhost:4318";
/// 定义 `DEFAULT_BATCH_SIZE` 常量。
const DEFAULT_BATCH_SIZE: usize = 32;
/// 定义 `DEFAULT_TIMEOUT_MS` 常量。
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
/// 定义 `DEFAULT_MAX_METRIC_LABELS` 常量。
const DEFAULT_MAX_METRIC_LABELS: usize = 8;

/// 定义指标输入超过本次批大小时保留哪一侧的数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTelemetryDropPolicy {
    /// 保留最早的一个批次，丢弃其后的新指标点。
    DropNew,
    /// 丢弃最早的超额点，保留最新的一个批次。
    DropOldest,
}

/// 定义 OTLP/HTTP 端点、认证、批处理和指标基数上限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTelemetryExporterConfig {
    /// 记录 `endpoint` 字段对应的值。
    pub endpoint: String,
    /// 仅作为 `authorization` 请求头传给 exporter；报告只暴露是否配置，不暴露原值。
    pub auth_header: Option<String>,
    /// 同时限制本轮指标点数量及 trace SDK 队列/导出批大小。
    pub batch_size: usize,
    /// 记录 `timeout_ms` 字段对应的值。
    pub timeout_ms: u64,
    /// 仅决定超额指标点保留头部还是尾部，不改变 SDK 对 span 队列的内部策略。
    pub drop_policy: OpenTelemetryDropPolicy,
    /// 每个指标点最多导出的标签数，超额标签计入报告但不使导出失败。
    pub max_metric_labels: usize,
}

/// 表示 `OpenTelemetryExporterReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTelemetryExporterReport {
    /// 记录 `endpoint` 字段对应的值。
    pub endpoint: String,
    /// 记录 `protocol` 字段对应的值。
    pub protocol: String,
    /// 记录 `sdk` 字段对应的值。
    pub sdk: String,
    /// 记录 `auth_configured` 字段对应的值。
    pub auth_configured: bool,
    /// 记录 `batch_size` 字段对应的值。
    pub batch_size: usize,
    /// 记录 `timeout_ms` 字段对应的值。
    pub timeout_ms: u64,
    /// 记录 `drop_policy` 字段对应的值。
    pub drop_policy: String,
    /// 记录 `max_metric_labels` 字段对应的值。
    pub max_metric_labels: usize,
    /// 记录 `spans_attempted` 字段对应的值。
    pub spans_attempted: usize,
    /// 记录 `spans_exported` 字段对应的值。
    pub spans_exported: usize,
    /// 记录 `metric_points_attempted` 字段对应的值。
    pub metric_points_attempted: usize,
    /// 记录 `metric_points_exported` 字段对应的值。
    pub metric_points_exported: usize,
    /// 记录 `metric_points_dropped` 字段对应的值。
    pub metric_points_dropped: usize,
    /// 记录 `metric_labels_exported` 字段对应的值。
    pub metric_labels_exported: usize,
    /// 记录 `metric_labels_dropped` 字段对应的值。
    pub metric_labels_dropped: usize,
    /// 记录 `degraded` 字段对应的值。
    pub degraded: bool,
    /// 记录 `degraded_reasons` 字段对应的值。
    pub degraded_reasons: Vec<String>,
    /// 记录 `continuity_key` 字段对应的值。
    pub continuity_key: Option<String>,
}

/// 表示 `PreparedMetricPoint` 数据结构。
#[derive(Debug, Clone)]
struct PreparedMetricPoint {
    /// 记录 `point` 字段对应的值。
    point: MetricPoint,
    /// 记录 `labels` 字段对应的值。
    labels: MetricLabels,
    /// 记录 `dropped_labels` 字段对应的值。
    dropped_labels: usize,
}

/// 为相关类型实现其约定的行为与方法。
impl Default for OpenTelemetryExporterConfig {
    /// 创建采用该类型默认配置的实例。
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

/// 为相关类型实现其约定的行为与方法。
impl OpenTelemetryExporterConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            ..Self::default()
        }
    }

    /// 设置 `endpoint` 并返回更新后的实例。
    pub fn with_endpoint(mut self, value: impl Into<String>) -> Self {
        self.endpoint = value.into();
        self
    }

    /// 设置 `auth_header` 并返回更新后的实例。
    pub fn with_auth_header(mut self, value: impl Into<String>) -> Self {
        self.auth_header = Some(value.into());
        self
    }

    /// 设置 `batch_size` 并返回更新后的实例。
    pub fn with_batch_size(mut self, value: usize) -> Self {
        self.batch_size = value;
        self
    }

    /// 设置 `timeout_ms` 并返回更新后的实例。
    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = value;
        self
    }

    /// 设置 `drop_policy` 并返回更新后的实例。
    pub fn with_drop_policy(mut self, value: OpenTelemetryDropPolicy) -> Self {
        self.drop_policy = value;
        self
    }

    /// 设置 `max_metric_labels` 并返回更新后的实例。
    pub fn with_max_metric_labels(mut self, value: usize) -> Self {
        self.max_metric_labels = value;
        self
    }

    /// 判断 `validate` 对应的条件是否成立。
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

/// 为相关类型实现其约定的行为与方法。
impl OpenTelemetryDropPolicy {
    /// 解析或检查 `parse` 对应的数据，并报告无效格式。
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

    /// 执行 `as_str` 对应的处理逻辑。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DropNew => "drop-new",
            Self::DropOldest => "drop-oldest",
        }
    }
}

/// 为相关类型实现其约定的行为与方法。
impl fmt::Display for OpenTelemetryDropPolicy {
    /// 执行 `fmt` 对应的处理逻辑。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 校验配置、预处理指标，再分别尝试 trace 和 metrics 导出；单个信号失败不阻止另一信号。
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

/// 构建有界批处理 span processor，创建单个 smoke span，并同步刷新、关闭 provider。
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

    // 只有队列刷新和 provider 关闭都成功，才能确认该 span 已离开本进程。
    match provider.force_flush().and_then(|_| provider.shutdown()) {
        Ok(()) => report.spans_exported = 1,
        Err(error) => record_degradation(report, format!("trace export failed: {error}")),
    }
}

/// 将预处理后的指标按类型记录到 SDK，并以刷新和关闭结果决定整批是否计为导出。
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

/// 按丢弃策略截取至单批大小，并对每个保留点独立裁剪标签。
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

/// 返回 `headers` 对应的数据视图。
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

/// 返回 `signal_endpoint` 对应的数据视图。
fn signal_endpoint(base: &str, signal_path: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with(signal_path) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/{signal_path}")
    }
}

/// 写入或导出 `record_degradation` 对应的可观测性记录。
fn record_degradation(report: &mut OpenTelemetryExporterReport, reason: String) {
    if !report.degraded_reasons.contains(&reason) {
        report.degraded_reasons.push(reason);
    }
}

/// 声明 `tests` 子模块。
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

    /// 表示 `FakeCollectorRecord` 数据结构。
    #[derive(Debug, Clone, Default)]
    struct FakeCollectorRecord {
        /// 记录 `path` 字段对应的值。
        path: String,
        /// 记录 `authorization` 字段对应的值。
        authorization: Option<String>,
        /// 记录 `body_len` 字段对应的值。
        body_len: usize,
    }

    /// 执行 `sample_trace` 对应的处理逻辑。
    fn sample_trace() -> TraceFields {
        TraceFields::default()
            .with_request_id(RequestId::parse("req-otel-smoke").unwrap())
            .with_span_id(SpanId::parse("otel-smoke").unwrap())
    }

    /// 执行 `sample_metrics` 对应的处理逻辑。
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

    /// 执行 `start_fake_collector` 对应的处理逻辑。
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

    /// 执行 `handle_connection` 对应的受控流程。
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

    /// 执行 `request_complete` 对应的处理逻辑。
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

    /// 验证 `opentelemetry_exporter_smoke_reaches_fake_collector` 场景下的预期行为。
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

    /// 验证 `opentelemetry_exporter_degrades_when_collector_is_unavailable` 场景下的预期行为。
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

    /// 验证 `opentelemetry_exporter_applies_batch_and_label_limits` 场景下的预期行为。
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
