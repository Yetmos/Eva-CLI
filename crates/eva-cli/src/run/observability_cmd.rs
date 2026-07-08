use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind,
    MetricLabels, MetricName, MetricPoint, MetricSink, ObservabilitySmokeReport, SpanId,
    TraceFields,
};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ObservabilityCommand {
    Smoke(ObservabilitySmokeOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObservabilitySmokeOptions {
    common: CommonOptions,
    backend: PathBuf,
}

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

fn parse_observability_smoke_options(
    args: &[String],
) -> Result<ObservabilitySmokeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut backend = PathBuf::from(".eva/observability");
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--backend" | "--observability-backend" => {
                index += 1;
                backend = PathBuf::from(required_option(args, index, "backend option")?);
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(ObservabilitySmokeOptions {
        common: parse_common_options(&passthrough)?,
        backend,
    })
}

fn run_observability_smoke(
    options: &ObservabilitySmokeOptions,
    trace: &TraceFields,
) -> Result<ObservabilitySmokeReport, EvaError> {
    let backend_root = options.backend.display().to_string();
    let mut pipeline = BestEffortObservabilityPipeline::open(&options.backend);
    let runtime_trace = trace.child_span(SpanId::parse("runtime.observability.smoke")?);
    let provider_trace = runtime_trace
        .clone()
        .with_adapter_id(AdapterId::parse("codex-cli")?)
        .with_capability(CapabilityName::parse("code.review")?)
        .with_provider("codex-cli");

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
    MetricSink::record(
        &mut pipeline,
        MetricPoint::new(
            MetricName::parse("runtime.events.accepted")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::runtime("in_memory_v1.0", "active")),
    )?;
    MetricSink::record(
        &mut pipeline,
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
    )?;
    MetricSink::record(
        &mut pipeline,
        MetricPoint::new(
            MetricName::parse("task.completed")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::task("completed", "root-agent")),
    )?;
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

    Ok(pipeline.smoke_report(backend_root, trace.continuity_key()))
}

fn write_observability_smoke<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &ObservabilitySmokeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva observability smoke").map_err(write_error_kind)?;
            writeln!(writer, "backend_root: {}", report.backend_root).map_err(write_error_kind)?;
            writeln!(writer, "degraded: {}", report.degraded).map_err(write_error_kind)?;
            writeln!(writer, "audit_events: {}", report.audit_events).map_err(write_error_kind)?;
            writeln!(writer, "metric_points: {}", report.metric_points)
                .map_err(write_error_kind)?;
            writeln!(writer, "otel_spans: {}", report.otel_spans).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "observability.smoke",
                EXIT_OK,
                &observability_smoke_json(report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn observability_smoke_json(report: &ObservabilitySmokeReport) -> String {
    format!(
        "{{\"backend_root\":{},\"degraded\":{},\"degraded_reasons\":{},\"audit_events\":{},\"metric_points\":{},\"otel_spans\":{},\"continuity_key\":{}}}",
        json_string(&report.backend_root),
        report.degraded,
        json_array(report.degraded_reasons.iter().map(|entry| json_string(entry))),
        report.audit_events,
        report.metric_points,
        report.otel_spans,
        option_json(report.continuity_key.as_deref())
    )
}
