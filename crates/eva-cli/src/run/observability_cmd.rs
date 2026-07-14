//! 可观测性烟测子命令：验证本地 pipeline、tracing bridge 与可选 OpenTelemetry exporter。

use super::{
    json_array, json_string, option_json, parse_common_options, parse_u64_option,
    parse_usize_option, required_option, success_envelope, trace_for, write_command_error,
    write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_observability::{
    run_opentelemetry_exporter_smoke, run_tracing_bridge_smoke, AuditAction, AuditEvent,
    AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind, MetricLabels, MetricName,
    MetricPoint, MetricSink, ObservabilitySmokeReport, OpenTelemetryDropPolicy,
    OpenTelemetryExporterConfig, OpenTelemetryExporterReport, SpanId, TraceFields,
    TracingBridgeReport, TracingBridgeSink,
};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Observability 子命令集合。
pub(super) enum ObservabilityCommand {
    /// 写入代表性 audit、metric 和 span，并报告各 sink 状态。
    Smoke(
        /// 已解析的后端路径、追踪接收端与公共选项。
        ObservabilitySmokeOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 可观测性烟测选项。
pub(super) struct ObservabilitySmokeOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 本地可观测性数据根目录。
    backend: PathBuf,
    /// tracing bridge 的输出 sink。
    tracing_sink: ObservabilityTracingSink,
    /// 仅在显式配置 endpoint 后启用的 OpenTelemetry exporter 配置。
    otel_config: Option<OpenTelemetryExporterConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// tracing bridge 支持的输出目标。
enum ObservabilityTracingSink {
    /// 将 bridge 事件持久化为 JSONL。
    Jsonl,
    /// 将格式化事件写入开发控制台报告。
    DevConsole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 一次烟测的组合结果，区分核心 pipeline、bridge 和可选 exporter。
struct ObservabilitySmokeRun {
    /// 本地 best-effort pipeline 汇总。
    report: ObservabilitySmokeReport,
    /// tracing bridge 的投递报告。
    tracing_bridge: TracingBridgeReport,
    /// 可选 OpenTelemetry exporter 报告。
    otel_exporter: Option<OpenTelemetryExporterReport>,
}

/// 解析唯一受支持的 `observability smoke` 子命令。
pub(super) fn parse_observability_command(
    args: &[String],
) -> Result<ObservabilityCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing observability subcommand"))?;
    match subcommand.as_str() {
        "smoke" => Ok(ObservabilityCommand::Smoke(
            parse_observability_smoke_options(rest)?,
        )),
        value => Err(EvaError::unsupported("unknown observability subcommand")
            .with_context("subcommand", value)),
    }
}

/// 执行烟测并用固定请求 ID 关联所有观测信号。
pub(super) fn execute_observability<W, E>(
    command: ObservabilityCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        ObservabilityCommand::Smoke(options) => {
            let trace = trace_for("cli.observability.smoke")
                .with_request_id(RequestId::parse("req-observability-smoke")?);
            match run_observability_smoke(&options, &trace) {
                Ok(report) => {
                    write_observability_smoke(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "observability.smoke",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// 解析后端、bridge sink 和 OpenTelemetry 限额。
/// 只有显式 endpoint 才启用 exporter，避免仅设置辅助参数时意外产生外部发送。
fn parse_observability_smoke_options(
    args: &[String],
) -> Result<ObservabilitySmokeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut backend = PathBuf::from(".eva/observability");
    let mut tracing_sink = ObservabilityTracingSink::Jsonl;
    let mut otel_config = OpenTelemetryExporterConfig::default();
    let mut otel_endpoint_configured = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--backend" | "--observability-backend" => {
                index += 1;
                backend = PathBuf::from(required_option(args, index, "backend option")?);
            }
            "--tracing-sink" => {
                index += 1;
                tracing_sink =
                    parse_tracing_sink(required_option(args, index, "tracing sink option")?)?;
            }
            "--otel-endpoint" => {
                index += 1;
                let endpoint = required_option(args, index, "OpenTelemetry endpoint option")?;
                otel_config = otel_config.with_endpoint(endpoint);
                otel_endpoint_configured = true;
            }
            "--otel-auth-header" => {
                index += 1;
                let value = required_option(args, index, "OpenTelemetry auth header option")?;
                otel_config = otel_config.with_auth_header(value);
            }
            "--otel-batch-size" => {
                index += 1;
                let value = parse_usize_option(
                    "otel_batch_size",
                    required_option(args, index, "OpenTelemetry batch size option")?,
                )?;
                otel_config = otel_config.with_batch_size(value);
            }
            "--otel-timeout-ms" => {
                index += 1;
                let value = parse_u64_option(
                    "otel_timeout_ms",
                    required_option(args, index, "OpenTelemetry timeout option")?,
                )?;
                otel_config = otel_config.with_timeout_ms(value);
            }
            "--otel-drop-policy" => {
                index += 1;
                let value = OpenTelemetryDropPolicy::parse(required_option(
                    args,
                    index,
                    "OpenTelemetry drop policy option",
                )?)?;
                otel_config = otel_config.with_drop_policy(value);
            }
            "--otel-max-metric-labels" => {
                index += 1;
                let value = parse_usize_option(
                    "otel_max_metric_labels",
                    required_option(args, index, "OpenTelemetry metric label limit option")?,
                )?;
                otel_config = otel_config.with_max_metric_labels(value);
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(ObservabilitySmokeOptions {
        common: parse_common_options(&passthrough)?,
        backend,
        tracing_sink,
        otel_config: otel_endpoint_configured.then_some(otel_config),
    })
}

/// 解析 tracing bridge sink 的稳定文本值。
fn parse_tracing_sink(value: &str) -> Result<ObservabilityTracingSink, EvaError> {
    match value {
        "jsonl" => Ok(ObservabilityTracingSink::Jsonl),
        "dev-console" => Ok(ObservabilityTracingSink::DevConsole),
        _ => Err(EvaError::invalid_argument("unknown tracing sink")
            .with_context("value", value)
            .with_context("expected", "jsonl|dev-console")),
    }
}

/// 向各可观测性边界写入代表性信号并收集降级报告。
///
/// 本地 pipeline 采用 best-effort 语义，但配置/编码错误仍返回失败；OpenTelemetry 仅在配置
/// 存在时运行。所有 span 从同一 trace 派生，以验证跨 CLI、runtime 和 provider 的连续性。
fn run_observability_smoke(
    options: &ObservabilitySmokeOptions,
    trace: &TraceFields,
) -> Result<ObservabilitySmokeRun, EvaError> {
    let backend_root = options.backend.display().to_string();
    let mut pipeline = BestEffortObservabilityPipeline::open(&options.backend);
    let runtime_trace = trace.child_span(SpanId::parse("runtime.observability.smoke")?);
    let provider_trace = runtime_trace
        .clone()
        .with_adapter_id(AdapterId::parse("codex-cli")?)
        .with_capability(CapabilityName::parse("code.review")?)
        .with_provider("codex-cli");
    let metric_points = vec![
        MetricPoint::new(
            MetricName::parse("runtime.events.accepted")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::runtime("in_memory_v1.0", "active")),
        MetricPoint::new(
            MetricName::parse("provider.invocations")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::provider(
            "codex-cli",
            "code.review",
            "codex-cli",
        )),
        MetricPoint::new(
            MetricName::parse("task.completed")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::task("completed", "root-agent")),
    ];

    AuditSink::record(
        &mut pipeline,
        AuditEvent::new(
            AuditAction::RuntimeStarted,
            AuditOutcome::Ok,
            runtime_trace.clone(),
        )
        .with_message("observability smoke recorded")
        .with_field("backend", &backend_root),
    )?;
    for point in &metric_points {
        MetricSink::record(&mut pipeline, point.clone())?;
    }
    pipeline.export_span(
        "cli.observability.smoke",
        trace,
        &[("component", "cli"), ("command", "observability.smoke")],
    )?;
    pipeline.export_span(
        "runtime.provider.smoke",
        &provider_trace,
        &[("component", "provider"), ("adapter_id", "codex-cli")],
    )?;
    let bridge_sink = match options.tracing_sink {
        ObservabilityTracingSink::Jsonl => TracingBridgeSink::jsonl(&options.backend),
        ObservabilityTracingSink::DevConsole => TracingBridgeSink::dev_console(),
    };
    let tracing_bridge = run_tracing_bridge_smoke(bridge_sink, trace)?;
    let otel_exporter = options
        .otel_config
        .clone()
        .map(|config| run_opentelemetry_exporter_smoke(config, trace, &metric_points))
        .transpose()?;

    Ok(ObservabilitySmokeRun {
        report: pipeline.smoke_report(backend_root, trace.continuity_key()),
        tracing_bridge,
        otel_exporter,
    })
}

/// 输出 pipeline、bridge 和 exporter 的计数及降级状态。
fn write_observability_smoke<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    run: &ObservabilitySmokeRun,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva observability smoke").map_err(write_error_kind)?;
            writeln!(writer, "backend_root: {}", run.report.backend_root)
                .map_err(write_error_kind)?;
            writeln!(writer, "degraded: {}", run.report.degraded).map_err(write_error_kind)?;
            writeln!(writer, "audit_events: {}", run.report.audit_events)
                .map_err(write_error_kind)?;
            writeln!(writer, "metric_points: {}", run.report.metric_points)
                .map_err(write_error_kind)?;
            writeln!(writer, "otel_spans: {}", run.report.otel_spans).map_err(write_error_kind)?;
            writeln!(writer, "tracing_bridge_sink: {}", run.tracing_bridge.sink)
                .map_err(write_error_kind)?;
            writeln!(writer, "tracing_bridge_spans: {}", run.tracing_bridge.spans)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "tracing_bridge_events: {}",
                run.tracing_bridge.events
            )
            .map_err(write_error_kind)?;
            if let Some(report) = &run.otel_exporter {
                writeln!(writer, "otel_exporter_endpoint: {}", report.endpoint)
                    .map_err(write_error_kind)?;
                writeln!(writer, "otel_exporter_degraded: {}", report.degraded)
                    .map_err(write_error_kind)?;
                writeln!(writer, "otel_exporter_spans: {}", report.spans_exported)
                    .map_err(write_error_kind)?;
                writeln!(
                    writer,
                    "otel_exporter_metric_points: {}",
                    report.metric_points_exported
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "observability.smoke",
                EXIT_OK,
                &observability_smoke_json(run),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 将组合烟测报告编码为稳定 JSON。
fn observability_smoke_json(run: &ObservabilitySmokeRun) -> String {
    format!(
        "{{\"backend_root\":{},\"degraded\":{},\"degraded_reasons\":{},\"audit_events\":{},\"metric_points\":{},\"otel_spans\":{},\"continuity_key\":{},\"tracing_bridge\":{},\"otel_exporter\":{}}}",
        json_string(&run.report.backend_root),
        run.report.degraded,
        json_array(run.report.degraded_reasons.iter().map(|entry| json_string(entry))),
        run.report.audit_events,
        run.report.metric_points,
        run.report.otel_spans,
        option_json(run.report.continuity_key.as_deref()),
        tracing_bridge_json(&run.tracing_bridge),
        run.otel_exporter
            .as_ref()
            .map(otel_exporter_json)
            .unwrap_or_else(|| "null".to_owned())
    )
}

/// 将 tracing bridge 的 span/event/audit 统计和连续性字段编码为 JSON。
fn tracing_bridge_json(report: &TracingBridgeReport) -> String {
    format!(
        "{{\"sink\":{},\"spans\":{},\"events\":{},\"audit_events\":{},\"exported_spans\":{},\"duplicate_span_ids\":{},\"degraded\":{},\"degraded_reasons\":{},\"dev_console_lines\":{},\"continuity_key\":{}}}",
        json_string(&report.sink),
        report.spans,
        report.events,
        report.audit_events,
        report.exported_spans,
        report.duplicate_span_ids,
        report.degraded,
        json_array(report.degraded_reasons.iter().map(|entry| json_string(entry))),
        report.dev_console_lines,
        option_json(report.continuity_key.as_deref())
    )
}

/// 将 exporter 配置、尝试/成功/丢弃计数与降级原因编码为 JSON。
fn otel_exporter_json(report: &OpenTelemetryExporterReport) -> String {
    format!(
        "{{\"endpoint\":{},\"protocol\":{},\"sdk\":{},\"auth_configured\":{},\"batch_size\":{},\"timeout_ms\":{},\"drop_policy\":{},\"max_metric_labels\":{},\"spans_attempted\":{},\"spans_exported\":{},\"metric_points_attempted\":{},\"metric_points_exported\":{},\"metric_points_dropped\":{},\"metric_labels_exported\":{},\"metric_labels_dropped\":{},\"degraded\":{},\"degraded_reasons\":{},\"continuity_key\":{}}}",
        json_string(&report.endpoint),
        json_string(&report.protocol),
        json_string(&report.sdk),
        report.auth_configured,
        report.batch_size,
        report.timeout_ms,
        json_string(&report.drop_policy),
        report.max_metric_labels,
        report.spans_attempted,
        report.spans_exported,
        report.metric_points_attempted,
        report.metric_points_exported,
        report.metric_points_dropped,
        report.metric_labels_exported,
        report.metric_labels_dropped,
        report.degraded,
        json_array(report.degraded_reasons.iter().map(|entry| json_string(entry))),
        option_json(report.continuity_key.as_deref())
    )
}
