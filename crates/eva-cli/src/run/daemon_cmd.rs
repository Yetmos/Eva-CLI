use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{EvaError, RequestId};
use eva_observability::TraceFields;
use eva_runtime::{
    daemon_status, start_daemon, stop_daemon, DaemonPathReport, DaemonPolicyReport,
    DaemonStartOptions, DaemonStartReport, DaemonStateRecord, DaemonStatusReport, DaemonStopReport,
};
use eva_storage::DurableBackendReport;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DaemonCommand {
    Start(DaemonCliOptions),
    Status(DaemonCliOptions),
    Stop(DaemonCliOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DaemonCliOptions {
    common: CommonOptions,
    durable_backend: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    lock_dir: Option<PathBuf>,
    pid_dir: Option<PathBuf>,
    observability_backend: Option<PathBuf>,
    foreground: bool,
    dev_mode: bool,
    shutdown_after_smoke: bool,
}

pub(super) fn parse_daemon_command(args: &[String]) -> Result<DaemonCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing daemon subcommand"))?;
    match subcommand.as_str() {
        "start" => Ok(DaemonCommand::Start(parse_daemon_options(rest)?)),
        "status" => Ok(DaemonCommand::Status(parse_daemon_options(rest)?)),
        "stop" | "shutdown" => Ok(DaemonCommand::Stop(parse_daemon_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown daemon subcommand")
                .with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_daemon<W, E>(
    command: DaemonCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        DaemonCommand::Start(options) => {
            let trace = trace_for("cli.daemon.start")
                .with_request_id(RequestId::parse("req-daemon-start")?);
            match load_project_config(&options.common.project_root).and_then(|project| {
                daemon_options_from_cli(&project, &options)
                    .and_then(|daemon_options| start_daemon(&project, daemon_options, &trace))
            }) {
                Ok(report) => {
                    write_daemon_start(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "daemon.start",
                    &error,
                    &trace,
                ),
            }
        }
        DaemonCommand::Status(options) => {
            let trace = trace_for("cli.daemon.status")
                .with_request_id(RequestId::parse("req-daemon-status")?);
            match load_project_config(&options.common.project_root).and_then(|project| {
                daemon_options_from_cli(&project, &options)
                    .and_then(|daemon_options| daemon_status(&daemon_options))
            }) {
                Ok(report) => {
                    write_daemon_status(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "daemon.status",
                    &error,
                    &trace,
                ),
            }
        }
        DaemonCommand::Stop(options) => {
            let trace =
                trace_for("cli.daemon.stop").with_request_id(RequestId::parse("req-daemon-stop")?);
            match load_project_config(&options.common.project_root).and_then(|project| {
                daemon_options_from_cli(&project, &options)
                    .and_then(|daemon_options| stop_daemon(&daemon_options))
            }) {
                Ok(report) => {
                    write_daemon_stop(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "daemon.stop",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn parse_daemon_options(args: &[String]) -> Result<DaemonCliOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut durable_backend = None;
    let mut state_dir = None;
    let mut lock_dir = None;
    let mut pid_dir = None;
    let mut observability_backend = None;
    let mut foreground = true;
    let mut dev_mode = false;
    let mut shutdown_after_smoke = true;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--durable-backend" | "--durable-backend-root" => {
                index += 1;
                durable_backend = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "durable backend option",
                )?));
            }
            "--state-dir" | "--state-store" => {
                index += 1;
                state_dir = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "state dir option",
                )?));
            }
            "--lock-dir" | "--lock-store" => {
                index += 1;
                lock_dir = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "lock dir option",
                )?));
            }
            "--pid-dir" => {
                index += 1;
                pid_dir = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "pid dir option",
                )?));
            }
            "--observability-backend" | "--observability-dir" => {
                index += 1;
                observability_backend = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "observability backend option",
                )?));
            }
            "--foreground" => foreground = true,
            "--background" => foreground = false,
            "--dev" | "--dev-mode" => dev_mode = true,
            "--shutdown-after-smoke" => shutdown_after_smoke = true,
            "--no-shutdown-after-smoke" => shutdown_after_smoke = false,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(DaemonCliOptions {
        common: parse_common_options(&passthrough)?,
        durable_backend,
        state_dir,
        lock_dir,
        pid_dir,
        observability_backend,
        foreground,
        dev_mode,
        shutdown_after_smoke,
    })
}

fn daemon_options_from_cli(
    project: &ProjectConfig,
    cli: &DaemonCliOptions,
) -> Result<DaemonStartOptions, EvaError> {
    let mut options = DaemonStartOptions::defaults(project);
    if let Some(path) = &cli.durable_backend {
        options.durable_backend = path.clone();
    }
    if let Some(path) = &cli.state_dir {
        options.state_dir = path.clone();
    }
    if let Some(path) = &cli.lock_dir {
        options.lock_dir = path.clone();
    }
    if let Some(path) = &cli.pid_dir {
        options.pid_dir = path.clone();
    }
    if let Some(path) = &cli.observability_backend {
        options.observability_backend = path.clone();
    }
    options.foreground = cli.foreground;
    options.dev_mode = cli.dev_mode;
    options.shutdown_after_smoke = cli.shutdown_after_smoke;
    Ok(options.resolve_against_project(&project.project_root))
}

fn write_daemon_start<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &DaemonStartReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Daemon start smoke").map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "mode: {}", report.mode).map_err(write_error_kind)?;
            writeln!(writer, "pid: {}", report.pid).map_err(write_error_kind)?;
            writeln!(
                writer,
                "provider_processes_started: {}",
                report.provider_processes_started
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "durable_backend: {}", report.durable_backend.root)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "observability_backend: {}",
                report.observability.backend_root
            )
            .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("daemon.start", EXIT_OK, &daemon_start_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_daemon_status<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &DaemonStatusReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Daemon status").map_err(write_error_kind)?;
            writeln!(writer, "available: {}", report.available).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "lock_present: {}", report.lock_present).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("daemon.status", EXIT_OK, &daemon_status_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_daemon_stop<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &DaemonStopReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Daemon stop").map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", report.mutation_executed)
                .map_err(write_error_kind)?;
            writeln!(writer, "lock_removed: {}", report.lock_removed).map_err(write_error_kind)?;
            writeln!(writer, "pid_removed: {}", report.pid_removed).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("daemon.stop", EXIT_OK, &daemon_stop_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn daemon_start_json(report: &DaemonStartReport) -> String {
    let shutdown = report
        .shutdown
        .as_ref()
        .map(shutdown_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"status\":{},\"mode\":{},\"pid\":{},\"generation_id\":{},\"project_root\":{},\"foreground\":{},\"dev_mode\":{},\"provider_processes_started\":{},\"paths\":{},\"durable_backend\":{},\"policy\":{},\"observability\":{},\"shutdown\":{},\"audit\":{}}}",
        json_string(&report.status),
        json_string(&report.mode),
        report.pid,
        json_string(&report.generation_id),
        json_string(&report.project_root),
        report.foreground,
        report.dev_mode,
        report.provider_processes_started,
        daemon_paths_json(&report.paths),
        durable_backend_json(&report.durable_backend),
        daemon_policy_json(&report.policy),
        observability_json(&report.observability),
        shutdown,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn daemon_status_json(report: &DaemonStatusReport) -> String {
    let state = report
        .state
        .as_ref()
        .map(daemon_state_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"available\":{},\"status\":{},\"lock_present\":{},\"paths\":{},\"state\":{}}}",
        report.available,
        json_string(&report.status),
        report.lock_present,
        daemon_paths_json(&report.paths),
        state
    )
}

fn daemon_stop_json(report: &DaemonStopReport) -> String {
    let state = report
        .state
        .as_ref()
        .map(daemon_state_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"status\":{},\"mutation_executed\":{},\"lock_removed\":{},\"pid_removed\":{},\"paths\":{},\"state\":{}}}",
        json_string(&report.status),
        report.mutation_executed,
        report.lock_removed,
        report.pid_removed,
        daemon_paths_json(&report.paths),
        state
    )
}

fn daemon_paths_json(paths: &DaemonPathReport) -> String {
    format!(
        "{{\"durable_backend_root\":{},\"observability_backend_root\":{},\"state_dir\":{},\"lock_dir\":{},\"pid_dir\":{},\"state_file\":{},\"lock_file\":{},\"pid_file\":{}}}",
        json_string(&paths.durable_backend_root),
        json_string(&paths.observability_backend_root),
        json_string(&paths.state_dir),
        json_string(&paths.lock_dir),
        json_string(&paths.pid_dir),
        json_string(&paths.state_file),
        json_string(&paths.lock_file),
        json_string(&paths.pid_file)
    )
}

fn daemon_policy_json(policy: &DaemonPolicyReport) -> String {
    format!(
        "{{\"status\":{},\"source_count\":{},\"effective_layers\":{}}}",
        json_string(&policy.status),
        policy.source_count,
        json_array(
            policy
                .effective_layers
                .iter()
                .map(|entry| json_string(entry))
        )
    )
}

fn durable_backend_json(report: &DurableBackendReport) -> String {
    format!(
        "{{\"schema_version\":{},\"layout_version\":{},\"mode\":{},\"migration_locked\":{},\"root\":{},\"event_dir\":{},\"state_dir\":{},\"task_dir\":{},\"audit_dir\":{},\"artifact_dir\":{}}}",
        report.schema_version,
        json_string(&report.layout_version),
        json_string(&report.mode),
        report.migration_locked,
        json_string(&report.root),
        json_string(&report.event_dir),
        json_string(&report.state_dir),
        json_string(&report.task_dir),
        json_string(&report.audit_dir),
        json_string(&report.artifact_dir)
    )
}

fn observability_json(report: &eva_observability::ObservabilitySmokeReport) -> String {
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

fn daemon_state_json(state: &DaemonStateRecord) -> String {
    format!(
        "{{\"status\":{},\"mode\":{},\"pid\":{},\"generation_id\":{},\"project_root\":{},\"started_at_ms\":{},\"stopped_at_ms\":{}}}",
        json_string(&state.status),
        json_string(&state.mode),
        state.pid,
        json_string(&state.generation_id),
        json_string(&state.project_root),
        state.started_at_ms,
        state
            .stopped_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned())
    )
}

fn shutdown_json(report: &eva_runtime::ShutdownReport) -> String {
    format!(
        "{{\"already_shutdown\":{},\"request_count\":{},\"phase\":{}}}",
        report.already_shutdown,
        report.request_count,
        json_string(&report.phase)
    )
}
