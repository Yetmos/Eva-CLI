use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{EvaError, RequestId};
use eva_observability::TraceFields;
use eva_runtime::{
    send_daemon_control_request, start_daemon, DaemonControlOperation, DaemonControlRequest,
    DaemonControlResponse, DaemonPathReport, DaemonPolicyReport, DaemonStartOptions,
    DaemonStartReport, DaemonStateRecord,
};
use eva_storage::DurableBackendReport;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DaemonCommand {
    Start(DaemonCliOptions),
    Control {
        options: DaemonCliOptions,
        operation: DaemonControlOperation,
        command: &'static str,
        span: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DaemonCliOptions {
    common: CommonOptions,
    durable_backend: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    lock_dir: Option<PathBuf>,
    pid_dir: Option<PathBuf>,
    observability_backend: Option<PathBuf>,
    request_id: Option<String>,
    task_id: Option<String>,
    reason: Option<String>,
    plan_id: Option<String>,
    generation_id: Option<String>,
    control_timeout_ms: u64,
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
        "status" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::Status,
            command: "daemon.status",
            span: "cli.daemon.status",
        }),
        "stop" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::Shutdown,
            command: "daemon.stop",
            span: "cli.daemon.stop",
        }),
        "shutdown" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::Shutdown,
            command: "daemon.shutdown",
            span: "cli.daemon.shutdown",
        }),
        "submit" | "submit-task" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::SubmitTask,
            command: "daemon.submit",
            span: "cli.daemon.submit",
        }),
        "cancel" | "cancel-task" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::CancelTask,
            command: "daemon.cancel",
            span: "cli.daemon.cancel",
        }),
        "drain" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::Drain,
            command: "daemon.drain",
            span: "cli.daemon.drain",
        }),
        "reload" | "reload-plan" => Ok(DaemonCommand::Control {
            options: parse_daemon_options(rest)?,
            operation: DaemonControlOperation::ReloadPlan,
            command: "daemon.reload",
            span: "cli.daemon.reload",
        }),
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
        DaemonCommand::Control {
            options,
            operation,
            command,
            span,
        } => {
            let request_id = RequestId::parse(
                options
                    .request_id
                    .as_deref()
                    .unwrap_or(default_control_request_id(operation)),
            )?;
            let trace = trace_for(span).with_request_id(request_id.clone());
            match load_project_config(&options.common.project_root).and_then(|project| {
                let daemon_options = daemon_options_from_cli(&project, &options)?;
                let request = daemon_control_request(operation, &options, request_id, &trace);
                send_daemon_control_request(&daemon_options, request, options.control_timeout_ms)
            }) {
                Ok(report) => {
                    write_daemon_control(stdout, options.common.output, command, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.common.output, command, &error, &trace)
                }
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
    let mut request_id = None;
    let mut task_id = None;
    let mut reason = None;
    let mut plan_id = None;
    let mut generation_id = None;
    let mut control_timeout_ms = 5_000;
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
            "--request-id" => {
                index += 1;
                request_id = Some(required_option(args, index, "request id option")?.clone());
            }
            "--task" | "--task-id" => {
                index += 1;
                task_id = Some(required_option(args, index, "task id option")?.clone());
            }
            "--reason" => {
                index += 1;
                reason = Some(required_option(args, index, "reason option")?.clone());
            }
            "--plan" | "--plan-id" => {
                index += 1;
                plan_id = Some(required_option(args, index, "plan id option")?.clone());
            }
            "--generation" | "--generation-id" => {
                index += 1;
                generation_id = Some(required_option(args, index, "generation option")?.clone());
            }
            "--control-timeout-ms" | "--timeout-ms" => {
                index += 1;
                control_timeout_ms = required_option(args, index, "control timeout option")?
                    .parse::<u64>()
                    .map_err(|_| {
                        EvaError::invalid_argument("control timeout must be an integer")
                    })?;
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
        request_id,
        task_id,
        reason,
        plan_id,
        generation_id,
        control_timeout_ms,
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

fn default_control_request_id(operation: DaemonControlOperation) -> &'static str {
    match operation {
        DaemonControlOperation::Status => "req-daemon-status",
        DaemonControlOperation::Shutdown => "req-daemon-shutdown",
        DaemonControlOperation::SubmitTask => "req-daemon-submit",
        DaemonControlOperation::CancelTask => "req-daemon-cancel",
        DaemonControlOperation::Drain => "req-daemon-drain",
        DaemonControlOperation::ReloadPlan => "req-daemon-reload",
    }
}

fn daemon_control_request(
    operation: DaemonControlOperation,
    options: &DaemonCliOptions,
    request_id: RequestId,
    trace: &TraceFields,
) -> DaemonControlRequest {
    let mut request = DaemonControlRequest::new(request_id, trace, operation);
    if let Some(value) = &options.task_id {
        request = request.with_task_id(value.clone());
    }
    if let Some(value) = &options.reason {
        request = request.with_reason(value.clone());
    }
    if let Some(value) = &options.plan_id {
        request = request.with_plan_id(value.clone());
    }
    if let Some(value) = &options.generation_id {
        request = request.with_generation_id(value.clone());
    }
    request
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
                "recovery_scanned_tasks: {}",
                report.recovery.scanned_tasks
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "recovery_scanned_provider_processes: {}",
                report.recovery.scanned_provider_processes
            )
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

fn write_daemon_control<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    command: &str,
    report: &DaemonControlResponse,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Daemon control").map_err(write_error_kind)?;
            writeln!(writer, "operation: {}", report.operation.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "accepted: {}", report.accepted).map_err(write_error_kind)?;
            writeln!(writer, "daemon_available: {}", report.daemon_available)
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", report.mutation_executed)
                .map_err(write_error_kind)?;
            writeln!(writer, "trace_id: {}", report.trace_id).map_err(write_error_kind)?;
            writeln!(writer, "message: {}", report.message).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(command, EXIT_OK, &daemon_control_json(report), trace)
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
        "{{\"status\":{},\"mode\":{},\"pid\":{},\"generation_id\":{},\"project_root\":{},\"foreground\":{},\"dev_mode\":{},\"provider_processes_started\":{},\"paths\":{},\"durable_backend\":{},\"recovery\":{},\"policy\":{},\"observability\":{},\"shutdown\":{},\"audit\":{}}}",
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
        recovery_json(&report.recovery),
        daemon_policy_json(&report.policy),
        observability_json(&report.observability),
        shutdown,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn recovery_json(report: &eva_runtime::RuntimeRecoveryReport) -> String {
    format!(
        "{{\"scanned_tasks\":{},\"recovered_tasks\":{},\"unchanged_tasks\":{},\"redriven_events\":{},\"skipped_redrive_events\":{},\"scanned_provider_processes\":{},\"recovered_provider_processes\":{},\"unchanged_provider_processes\":{},\"provider_backoff_tasks\":{},\"skipped_provider_tasks\":{},\"audit\":{}}}",
        report.scanned_tasks,
        json_array(report.recovered_tasks.iter().map(recovered_task_json)),
        json_array(report.unchanged_tasks.iter().map(|entry| json_string(entry))),
        json_array(report.redriven_events.iter().map(recovered_event_json)),
        json_array(
            report
                .skipped_redrive_events
                .iter()
                .map(skipped_redrive_event_json)
        ),
        report.scanned_provider_processes,
        json_array(
            report
                .recovered_provider_processes
                .iter()
                .map(recovered_provider_process_json)
        ),
        json_array(
            report
                .unchanged_provider_processes
                .iter()
                .map(|entry| json_string(entry))
        ),
        json_array(
            report
                .provider_backoff_tasks
                .iter()
                .map(provider_backoff_task_json)
        ),
        json_array(
            report
                .skipped_provider_tasks
                .iter()
                .map(skipped_provider_task_json)
        ),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn recovered_task_json(task: &eva_runtime::RecoveredTask) -> String {
    format!(
        "{{\"task_id\":{},\"previous_status\":{},\"status\":{},\"redrive_candidate\":{}}}",
        json_string(&task.task_id),
        json_string(&task.previous_status),
        json_string(&task.status),
        task.redrive_candidate
    )
}

fn recovered_event_json(event: &eva_runtime::RecoveredEvent) -> String {
    format!(
        "{{\"task_id\":{},\"event_id\":{},\"replay_event_id\":{},\"sequence\":{},\"topic\":{}}}",
        json_string(&event.task_id),
        json_string(&event.event_id),
        json_string(&event.replay_event_id),
        event.sequence,
        json_string(&event.topic)
    )
}

fn skipped_redrive_event_json(event: &eva_runtime::SkippedRedriveEvent) -> String {
    format!(
        "{{\"task_id\":{},\"event_id\":{},\"reason\":{}}}",
        json_string(&event.task_id),
        json_string(&event.event_id),
        json_string(&event.reason)
    )
}

fn recovered_provider_process_json(process: &eva_runtime::RecoveredProviderProcess) -> String {
    format!(
        "{{\"session_id\":{},\"provider_process_id\":{},\"request_id\":{},\"adapter_id\":{},\"previous_health\":{},\"health\":{},\"task_id\":{},\"task_status\":{},\"retry_scheduled\":{}}}",
        json_string(&process.session_id),
        json_string(&process.provider_process_id),
        json_string(&process.request_id),
        json_string(&process.adapter_id),
        json_string(&process.previous_health),
        json_string(&process.health),
        json_string(&process.task_id),
        option_json(process.task_status.as_deref()),
        process.retry_scheduled
    )
}

fn provider_backoff_task_json(task: &eva_runtime::ProviderBackoffTask) -> String {
    format!(
        "{{\"task_id\":{},\"session_id\":{},\"next_attempt\":{},\"due_after_ms\":{},\"reason\":{}}}",
        json_string(&task.task_id),
        json_string(&task.session_id),
        task.next_attempt,
        task.due_after_ms,
        json_string(&task.reason)
    )
}

fn skipped_provider_task_json(task: &eva_runtime::SkippedProviderTask) -> String {
    format!(
        "{{\"task_id\":{},\"session_id\":{},\"reason\":{}}}",
        json_string(&task.task_id),
        json_string(&task.session_id),
        json_string(&task.reason)
    )
}

fn daemon_control_json(report: &DaemonControlResponse) -> String {
    let state = report
        .state
        .as_ref()
        .map(daemon_state_json)
        .unwrap_or_else(|| "null".to_owned());
    let shutdown = report
        .shutdown
        .as_ref()
        .map(shutdown_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"request_id\":{},\"trace_id\":{},\"operation\":{},\"accepted\":{},\"daemon_available\":{},\"status\":{},\"mutation_executed\":{},\"request_file\":{},\"response_file\":{},\"state\":{},\"task_id\":{},\"plan_id\":{},\"generation_id\":{},\"message\":{},\"shutdown\":{},\"audit\":{}}}",
        json_string(report.request_id.as_str()),
        json_string(&report.trace_id),
        json_string(report.operation.as_str()),
        report.accepted,
        report.daemon_available,
        json_string(&report.status),
        report.mutation_executed,
        json_string(&report.request_file),
        json_string(&report.response_file),
        state,
        option_json(report.task_id.as_deref()),
        option_json(report.plan_id.as_deref()),
        option_json(report.generation_id.as_deref()),
        json_string(&report.message),
        shutdown,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn daemon_paths_json(paths: &DaemonPathReport) -> String {
    format!(
        "{{\"durable_backend_root\":{},\"observability_backend_root\":{},\"state_dir\":{},\"lock_dir\":{},\"pid_dir\":{},\"control_request_dir\":{},\"control_response_dir\":{},\"state_file\":{},\"lock_file\":{},\"pid_file\":{}}}",
        json_string(&paths.durable_backend_root),
        json_string(&paths.observability_backend_root),
        json_string(&paths.state_dir),
        json_string(&paths.lock_dir),
        json_string(&paths.pid_dir),
        json_string(&paths.control_request_dir),
        json_string(&paths.control_response_dir),
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
