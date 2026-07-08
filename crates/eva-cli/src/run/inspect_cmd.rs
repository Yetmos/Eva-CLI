use super::{
    json_string, parse_common_options, parse_u64_option, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use crate::inspect::{inspect_project, InspectReport};
use eva_config::load_project_config;
use eva_core::EvaError;
use eva_observability::TraceFields;
use eva_runtime::{inspect_durable_backend, DurableDiagnosticsOptions, DurableDiagnosticsReport};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InspectOptions {
    common: CommonOptions,
    subject: InspectSubject,
    durable_backend: Option<PathBuf>,
    redrive_ready_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectSubject {
    Project,
    Durable,
}

pub(super) fn parse_inspect_options(args: &[String]) -> Result<InspectOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut subject = InspectSubject::Project;
    let mut durable_backend = None;
    let mut redrive_ready_at_ms = 0;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "all" | "config" | "runtime" | "routes" | "policy" | "agents" | "adapters"
            | "capabilities" => subject = InspectSubject::Project,
            "durable" | "durable-backend" => subject = InspectSubject::Durable,
            "--durable-backend" | "--durable-backend-root" => {
                index += 1;
                durable_backend = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "durable backend option",
                )?));
            }
            "--redrive-ready-at-ms" => {
                index += 1;
                redrive_ready_at_ms = parse_u64_option(
                    "redrive_ready_at_ms",
                    required_option(args, index, "redrive ready option")?,
                )?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(InspectOptions {
        common: parse_common_options(&passthrough)?,
        subject,
        durable_backend,
        redrive_ready_at_ms,
    })
}

pub(super) fn execute_inspect<W, E>(
    options: InspectOptions,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match options.subject {
        InspectSubject::Project => {
            let trace = trace_for("cli.inspect");
            match load_project_config(&options.common.project_root)
                .and_then(|project| inspect_project(&project))
            {
                Ok(report) => {
                    write_inspect(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.common.output, "inspect", &error, &trace)
                }
            }
        }
        InspectSubject::Durable => {
            let trace = trace_for("cli.inspect.durable");
            let result = options
                .durable_backend
                .as_deref()
                .ok_or_else(|| {
                    EvaError::invalid_argument("inspect durable requires --durable-backend")
                        .with_context(
                            "suggestion",
                            "run `eva inspect durable --durable-backend <path>`",
                        )
                })
                .and_then(|root| {
                    inspect_durable_backend(
                        root,
                        DurableDiagnosticsOptions {
                            redrive_ready_at_ms: options.redrive_ready_at_ms,
                        },
                    )
                });
            match result {
                Ok(report) => {
                    write_durable_inspect(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "inspect.durable",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn write_inspect<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &InspectReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva inspect").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(writer, "environment: {}", report.environment).map_err(write_error_kind)?;
            writeln!(writer, "hot_reload: {}", report.hot_reload).map_err(write_error_kind)?;
            writeln!(writer, "agents:").map_err(write_error_kind)?;
            for agent in &report.agents {
                writeln!(
                    writer,
                    "  - {} enabled={} subscriptions={}",
                    agent.id,
                    agent.enabled,
                    agent.subscriptions.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "adapters:").map_err(write_error_kind)?;
            for adapter in &report.adapters {
                writeln!(
                    writer,
                    "  - {} transport={} enabled={} capabilities={}",
                    adapter.id,
                    adapter.transport,
                    adapter.enabled,
                    adapter.capabilities.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "capabilities:").map_err(write_error_kind)?;
            for capability in &report.capabilities {
                writeln!(
                    writer,
                    "  - {} capability={} kind={} enabled={} providers={}",
                    capability.id,
                    capability.capability,
                    capability.kind,
                    capability.enabled,
                    capability.providers.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "routes:").map_err(write_error_kind)?;
            for route in &report.routes {
                writeln!(
                    writer,
                    "  - {} delivery={} agents={}",
                    route.pattern,
                    route.delivery,
                    route.agents.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(
                writer,
                "runtime: mode={} status={} generation={}",
                report.runtime.mode, report.runtime.status, report.runtime.generation_id
            )
            .map_err(write_error_kind)?;
            for service in &report.runtime.services {
                writeln!(
                    writer,
                    "  - {} state={} detail={}",
                    service.name, service.state, service.detail
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("inspect", EXIT_OK, &report.to_json(), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_durable_inspect<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &DurableDiagnosticsReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva durable inspect").map_err(write_error_kind)?;
            writeln!(writer, "backend_path: {}", report.backend_path).map_err(write_error_kind)?;
            writeln!(writer, "backend_mode: {}", report.backend_mode).map_err(write_error_kind)?;
            writeln!(writer, "schema_version: {}", report.schema_version)
                .map_err(write_error_kind)?;
            writeln!(writer, "layout_version: {}", report.layout_version)
                .map_err(write_error_kind)?;
            writeln!(writer, "migration_status: {}", report.migration_status)
                .map_err(write_error_kind)?;
            writeln!(writer, "migration_locked: {}", report.migration_locked)
                .map_err(write_error_kind)?;
            writeln!(writer, "event_log_records: {}", report.event_log_records)
                .map_err(write_error_kind)?;
            writeln!(writer, "dead_letter_count: {}", report.dead_letter_count)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "pending_redrive_count: {}",
                report.pending_redrive_count
            )
            .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "inspect.durable",
                EXIT_OK,
                &durable_diagnostics_json(report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn durable_diagnostics_json(report: &DurableDiagnosticsReport) -> String {
    format!(
        "{{\"backend_path\":{},\"backend_mode\":{},\"schema_version\":{},\"layout_version\":{},\"migration_status\":{},\"migration_locked\":{},\"event_log_records\":{},\"dead_letter_count\":{},\"pending_redrive_count\":{}}}",
        json_string(&report.backend_path),
        json_string(&report.backend_mode),
        report.schema_version,
        json_string(&report.layout_version),
        json_string(&report.migration_status),
        report.migration_locked,
        report.event_log_records,
        report.dead_letter_count,
        report.pending_redrive_count
    )
}
