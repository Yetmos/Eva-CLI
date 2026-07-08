//! CLI command parsing, output envelopes, and process exit mapping.

mod adapter_cmd;
mod backup_cmd;
mod config_cmd;
mod discovery_cmd;
mod doctor_cmd;
mod hardware_cmd;
mod inspect_cmd;
mod mcp_cmd;
mod memory_cmd;
mod observability_cmd;
mod release_cmd;
mod restore_cmd;
mod skill_cmd;
mod snapshot_cmd;
mod task_cmd;
mod upgrade_cmd;
mod version_cmd;

use adapter_cmd::AdapterCommand;
use backup_cmd::BackupCommand;
use discovery_cmd::DiscoveryCommand;
use eva_config::load_project_config;
use eva_core::{CapabilityName, ErrorKind, EvaError, InvokeStatus};
use eva_lifecycle::RollbackPlan;
use eva_observability::{SpanId, TraceFields};
use eva_runtime::{BasicRunOptions, BasicRunReport, RuntimeBuilder};
use hardware_cmd::HardwareCommand;
use inspect_cmd::InspectOptions;
use mcp_cmd::McpCommand;
use memory_cmd::MemoryCommand;
use observability_cmd::ObservabilityCommand;
use release_cmd::ReleaseCommand;
use restore_cmd::RestoreCommand;
use skill_cmd::SkillCommand;
use snapshot_cmd::SnapshotCommand;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use task_cmd::TaskCommand;
use upgrade_cmd::UpgradeCommand;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "parse Eva CLI commands and map results to stable output and exit codes";

const EXIT_OK: i32 = 0;
const EXIT_INTERNAL: i32 = 1;
const EXIT_CONFIG: i32 = 2;
const EXIT_POLICY: i32 = 3;
const EXIT_RUNTIME_UNAVAILABLE: i32 = 4;
const EXIT_EXTERNAL_UNAVAILABLE: i32 = 5;
const EXIT_USAGE: i32 = 64;
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_STATUS: &str = "alpha";
const RELEASE_LABEL: &str = "V1.7.4-alpha";
const RELEASE_RUNTIME_MODE: &str =
    "in_memory_v1.0 + external_capability_v1.1 + context_v1.2 + hardware_v1.3 + lifecycle_v1.4 + release_v1.5 + durable_backend_v1.6.1 + durable_eventbus_v1.6.2 + durable_task_audit_artifact_v1.6.3 + durable_runtime_recovery_v1.6.4 + durable_diagnostics_v1.6.5 + lua_vm_execution_v1.7.1 + lua_host_bindings_v1.7.2 + lua_resource_limits_v1.7.3 + lua_hot_reload_lifecycle_v1.7.4";
const RELEASE_CONTRACTS: &[&str] = &[
    "doctor",
    "config validate",
    "inspect",
    "run --example basic",
    "task status/logs/cancel",
    "adapter list/probe",
    "mcp list/probe",
    "skill list/run",
    "discovery scan",
    "memory context",
    "observability smoke",
    "hardware list/probe/bind",
    "backup create",
    "snapshot create",
    "snapshot promote",
    "restore plan",
    "restore apply",
    "upgrade check",
    "upgrade apply",
    "release check",
    "release security",
    "release perf",
    "release migration",
];

/// Process entry point for the root binary shim.
pub fn run() {
    let exit_code = run_with_args(env::args_os().skip(1), &mut io::stdout(), &mut io::stderr());
    std::process::exit(exit_code);
}

/// Testable CLI entry point.
pub fn run_with_args<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    let command = match parse_command(args) {
        Ok(Command::Help) => {
            let _ = stdout.write_all(help_text().as_bytes());
            return EXIT_OK;
        }
        Ok(command) => command,
        Err(error) => {
            let trace = trace_for("cli.parse");
            let exit_code = EXIT_USAGE;
            let _ = write_error(
                stderr,
                OutputFormat::Text,
                "parse",
                exit_code,
                &error,
                &trace,
            );
            return exit_code;
        }
    };

    match execute(command, stdout, stderr) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            let trace = trace_for("cli.execute");
            let exit_code = exit_code_for_error(&error);
            let _ = write_error(
                stderr,
                OutputFormat::Text,
                "execute",
                exit_code,
                &error,
                &trace,
            );
            exit_code
        }
    }
}

fn execute<W, E>(command: Command, stdout: &mut W, stderr: &mut E) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        Command::Help => unreachable!("help is handled before execution"),
        Command::Version(options) => version_cmd::execute_version(options, stdout),
        Command::Doctor(options) => doctor_cmd::execute_doctor(options, stdout),
        Command::ConfigValidate(options) => {
            config_cmd::execute_config_validate(options, stdout, stderr)
        }
        Command::Inspect(options) => inspect_cmd::execute_inspect(options, stdout, stderr),
        Command::Run(options) => {
            let trace = trace_for("cli.run");
            match execute_run(options, stdout, stderr, &trace) {
                Ok(exit_code) => Ok(exit_code),
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(stderr, OutputFormat::Text, "run", exit_code, &error, &trace)?;
                    Ok(exit_code)
                }
            }
        }
        Command::Task(command) => task_cmd::execute_task(command, stdout, stderr),
        Command::Adapter(command) => adapter_cmd::execute_adapter(command, stdout, stderr),
        Command::Mcp(command) => mcp_cmd::execute_mcp(command, stdout, stderr),
        Command::Skill(command) => skill_cmd::execute_skill(command, stdout, stderr),
        Command::Discovery(command) => discovery_cmd::execute_discovery(command, stdout, stderr),
        Command::Memory(command) => memory_cmd::execute_memory(command, stdout, stderr),
        Command::Observability(command) => {
            observability_cmd::execute_observability(command, stdout, stderr)
        }
        Command::Hardware(command) => hardware_cmd::execute_hardware(command, stdout, stderr),
        Command::Backup(command) => backup_cmd::execute_backup(command, stdout, stderr),
        Command::Snapshot(command) => snapshot_cmd::execute_snapshot(command, stdout, stderr),
        Command::Restore(command) => restore_cmd::execute_restore(command, stdout, stderr),
        Command::Upgrade(command) => upgrade_cmd::execute_upgrade(command, stdout, stderr),
        Command::Release(command) => release_cmd::execute_release(command, stdout, stderr),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Version(CommonOptions),
    Doctor(CommonOptions),
    ConfigValidate(CommonOptions),
    Inspect(InspectOptions),
    Run(RunOptions),
    Task(TaskCommand),
    Adapter(AdapterCommand),
    Mcp(McpCommand),
    Skill(SkillCommand),
    Discovery(DiscoveryCommand),
    Memory(MemoryCommand),
    Observability(ObservabilityCommand),
    Hardware(HardwareCommand),
    Backup(BackupCommand),
    Snapshot(SnapshotCommand),
    Restore(RestoreCommand),
    Upgrade(UpgradeCommand),
    Release(ReleaseCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommonOptions {
    project_root: PathBuf,
    output: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunOptions {
    common: CommonOptions,
    example: Option<String>,
    task_id: Option<String>,
    durable_backend: Option<PathBuf>,
    timeout_ms: Option<u64>,
    cancel_requested: bool,
    retry_attempts: usize,
    replay_dead_letters: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactStoreRef {
    kind: String,
    path: Option<String>,
}

fn parse_command<I>(args: I) -> Result<Command, EvaError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| EvaError::invalid_argument("command-line argument is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Command::Help);
    }

    if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V") {
        return Ok(Command::Version(default_common_options(
            OutputFormat::Text,
        )?));
    }

    match args[0].as_str() {
        "help" => Ok(Command::Help),
        "version" => Ok(Command::Version(parse_common_options(&args[1..])?)),
        "doctor" => Ok(Command::Doctor(doctor_cmd::parse_doctor_options(
            &args[1..],
        )?)),
        "config" => Ok(Command::ConfigValidate(config_cmd::parse_config_command(
            &args[1..],
        )?)),
        "inspect" => Ok(Command::Inspect(inspect_cmd::parse_inspect_options(
            &args[1..],
        )?)),
        "run" => Ok(Command::Run(parse_run_options(&args[1..])?)),
        "task" => Ok(Command::Task(task_cmd::parse_task_command(&args[1..])?)),
        "adapter" => Ok(Command::Adapter(adapter_cmd::parse_adapter_command(
            &args[1..],
        )?)),
        "mcp" => Ok(Command::Mcp(mcp_cmd::parse_mcp_command(&args[1..])?)),
        "skill" => Ok(Command::Skill(skill_cmd::parse_skill_command(&args[1..])?)),
        "discovery" => Ok(Command::Discovery(discovery_cmd::parse_discovery_command(
            &args[1..],
        )?)),
        "memory" => Ok(Command::Memory(memory_cmd::parse_memory_command(
            &args[1..],
        )?)),
        "observability" => Ok(Command::Observability(
            observability_cmd::parse_observability_command(&args[1..])?,
        )),
        "hardware" => Ok(Command::Hardware(hardware_cmd::parse_hardware_command(
            &args[1..],
        )?)),
        "backup" => Ok(Command::Backup(backup_cmd::parse_backup_command(
            &args[1..],
        )?)),
        "snapshot" => Ok(Command::Snapshot(snapshot_cmd::parse_snapshot_command(
            &args[1..],
        )?)),
        "restore" => Ok(Command::Restore(restore_cmd::parse_restore_command(
            &args[1..],
        )?)),
        "upgrade" => Ok(Command::Upgrade(upgrade_cmd::parse_upgrade_command(
            &args[1..],
        )?)),
        "release" => Ok(Command::Release(release_cmd::parse_release_command(
            &args[1..],
        )?)),
        unknown => Err(EvaError::unsupported("unknown command").with_context("command", unknown)),
    }
}

fn parse_run_options(args: &[String]) -> Result<RunOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut example = None;
    let mut task_id = None;
    let mut durable_backend = None;
    let mut timeout_ms = Some(30_000);
    let mut cancel_requested = false;
    let mut retry_attempts = 1;
    let mut replay_dead_letters = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--example" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for example option")
                })?;
                example = Some(value.clone());
            }
            "--task-id" | "--task" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for task id option")
                })?;
                eva_core::RequestId::parse(value)?;
                task_id = Some(value.clone());
            }
            "--durable-backend" | "--durable-backend-root" => {
                index += 1;
                durable_backend = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "durable backend option",
                )?));
            }
            "--timeout-ms" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for timeout option")
                })?;
                timeout_ms = Some(parse_u64_option("timeout_ms", value)?);
            }
            "--no-timeout" => timeout_ms = None,
            "--cancel" => cancel_requested = true,
            "--retry-attempts" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for retry option"))?;
                retry_attempts = parse_usize_option("retry_attempts", value)?.max(1);
            }
            "--replay-dead-letters" => replay_dead_letters = true,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(RunOptions {
        common: parse_common_options(&passthrough)?,
        example,
        task_id,
        durable_backend,
        timeout_ms,
        cancel_requested,
        retry_attempts,
        replay_dead_letters,
    })
}

fn required_option<'a>(
    args: &'a [String],
    index: usize,
    name: &'static str,
) -> Result<&'a String, EvaError> {
    args.get(index)
        .ok_or_else(|| EvaError::invalid_argument(format!("missing value for {name}")))
}

fn parse_u64_option(name: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::invalid_argument("option must be an unsigned integer")
            .with_context("option", name)
            .with_context("value", value)
    })
}

fn parse_usize_option(name: &'static str, value: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::invalid_argument("option must be an unsigned integer")
            .with_context("option", name)
            .with_context("value", value)
    })
}

fn execute_run<W, E>(
    options: RunOptions,
    stdout: &mut W,
    stderr: &mut E,
    trace: &TraceFields,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match options.example.as_deref() {
        Some("basic") => {
            let project_root = options.common.project_root.join("examples").join("basic");
            let mut run_options = BasicRunOptions {
                timeout_ms: options.timeout_ms,
                cancel_requested: options.cancel_requested,
                retry_attempts: options.retry_attempts,
                replay_dead_letters: options.replay_dead_letters,
                ..BasicRunOptions::default()
            };
            if let Some(task_id) = &options.task_id {
                run_options.request_id = eva_core::RequestId::parse(task_id)?;
            }
            match load_project_config(&project_root).and_then(|project| {
                let runtime = RuntimeBuilder::in_memory_v10().build(&project)?;
                runtime
                    .run_basic(&project, run_options)
                    .map(|report| (project, runtime, report))
            }) {
                Ok((_project, _runtime, report)) => {
                    task_cmd::write_task_snapshot(
                        &options.common.project_root,
                        options.durable_backend.as_deref(),
                        &report,
                    )?;
                    write_run(stdout, options.common.output, &report, trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(
                        stderr,
                        options.common.output,
                        "run",
                        exit_code,
                        &error,
                        trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        Some(example) => {
            let error = EvaError::unsupported("unknown run example")
                .with_context("example", example)
                .with_context("supported", "basic");
            let exit_code = EXIT_USAGE;
            write_error(
                stderr,
                options.common.output,
                "run",
                exit_code,
                &error,
                trace,
            )?;
            Ok(exit_code)
        }
        None => {
            let error = EvaError::unsupported("eva run requires an example in V1.0 core")
                .with_context("suggestion", "use `eva run --example basic`");
            let exit_code = EXIT_RUNTIME_UNAVAILABLE;
            write_error(
                stderr,
                options.common.output,
                "run",
                exit_code,
                &error,
                trace,
            )?;
            Ok(exit_code)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockStoreRef {
    kind: String,
    path: Option<String>,
}

fn artifact_store_ref(path: Option<&Path>) -> ArtifactStoreRef {
    match path {
        Some(path) => ArtifactStoreRef {
            kind: "filesystem".to_owned(),
            path: Some(path.display().to_string()),
        },
        None => ArtifactStoreRef {
            kind: "in_memory".to_owned(),
            path: None,
        },
    }
}

fn lock_store_ref(path: Option<&Path>) -> LockStoreRef {
    match path {
        Some(path) => LockStoreRef {
            kind: "filesystem".to_owned(),
            path: Some(path.display().to_string()),
        },
        None => LockStoreRef {
            kind: "in_memory".to_owned(),
            path: None,
        },
    }
}

fn write_command_error<W: Write>(
    stderr: &mut W,
    output: OutputFormat,
    command: &str,
    error: &EvaError,
    trace: &TraceFields,
) -> Result<i32, EvaError> {
    let exit_code = exit_code_for_error(error);
    write_error(stderr, output, command, exit_code, error, trace)?;
    Ok(exit_code)
}

fn parse_common_options(args: &[String]) -> Result<CommonOptions, EvaError> {
    let mut options = default_common_options(OutputFormat::Text)?;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--project" | "--project-root" | "-p" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for project option")
                })?;
                options.project_root = PathBuf::from(value);
            }
            "--output" | "-o" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for output option"))?;
                options.output = OutputFormat::parse(value)?;
            }
            unknown => {
                return Err(EvaError::unsupported("unknown option").with_context("option", unknown));
            }
        }
        index += 1;
    }

    Ok(options)
}

fn default_common_options(output: OutputFormat) -> Result<CommonOptions, EvaError> {
    let project_root = env::current_dir().map_err(|error| {
        EvaError::internal("failed to read current directory")
            .with_context("io_error", error.to_string())
    })?;
    Ok(CommonOptions {
        project_root,
        output,
    })
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "text" | "human" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err(EvaError::unsupported("unsupported output format")
                .with_context("output", value)
                .with_context("supported", "text,json")),
        }
    }
}

fn write_artifact_store_ref<W: Write>(
    writer: &mut W,
    artifact_store: &ArtifactStoreRef,
) -> Result<(), EvaError> {
    writeln!(writer, "artifact_store: {}", artifact_store.kind).map_err(write_error_kind)?;
    if let Some(path) = &artifact_store.path {
        writeln!(writer, "artifact_store_path: {path}").map_err(write_error_kind)?;
    }
    Ok(())
}

fn write_lock_store_ref<W: Write>(
    writer: &mut W,
    lock_store: &LockStoreRef,
) -> Result<(), EvaError> {
    writeln!(writer, "lock_store: {}", lock_store.kind).map_err(write_error_kind)?;
    if let Some(path) = &lock_store.path {
        writeln!(writer, "lock_store_path: {path}").map_err(write_error_kind)?;
    }
    Ok(())
}

fn artifact_store_ref_json(artifact_store: &ArtifactStoreRef) -> String {
    format!(
        "{{\"kind\":{},\"path\":{}}}",
        json_string(&artifact_store.kind),
        option_json(artifact_store.path.as_deref())
    )
}

fn lock_store_ref_json(lock_store: &LockStoreRef) -> String {
    format!(
        "{{\"kind\":{},\"path\":{}}}",
        json_string(&lock_store.kind),
        option_json(lock_store.path.as_deref())
    )
}

fn rollback_plan_json(plan: &RollbackPlan) -> String {
    format!(
        "{{\"from_generation\":{},\"to_generation\":{},\"snapshot_id\":{},\"reason\":{},\"status\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}",
        json_string(plan.from_generation.as_str()),
        json_string(plan.to_generation.as_str()),
        option_json(plan.snapshot_id.as_deref()),
        json_string(&plan.reason),
        json_string(&plan.status),
        json_array(plan.steps.iter().map(|step| json_string(step))),
        json_array(plan.risks.iter().map(|risk| json_string(risk))),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
    )
}

fn join_capabilities(capabilities: &[CapabilityName]) -> String {
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn write_run<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &BasicRunReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK run example=basic").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(
                writer,
                "runtime: mode={} generation={}",
                report.runtime_mode, report.generation_id
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "task: id={} status={} attempts={}/{}",
                report.task.task_id,
                report.task.status.as_str(),
                report.task.attempts,
                report.task.retry_policy.max_attempts
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "event: {} topic={} sequence={}",
                report.event_id, report.topic, report.receipt.sequence
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "deliveries:").map_err(write_error_kind)?;
            for delivery in &report.deliveries {
                writeln!(
                    writer,
                    "  - agent={} delivery={}",
                    delivery.agent_id,
                    delivery.delivery.as_str()
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "agent_runs:").map_err(write_error_kind)?;
            for run in &report.agent_runs {
                writeln!(
                    writer,
                    "  - agent={} status={} handler_status={}",
                    run.agent_id,
                    run.status.as_str(),
                    run.handler_status.as_deref().unwrap_or("")
                )
                .map_err(write_error_kind)?;
            }
            if let Some(response) = &report.capability_response {
                writeln!(
                    writer,
                    "capability: status={} output={}",
                    invoke_status(response.status()),
                    response
                        .output()
                        .and_then(|output| output.as_text())
                        .unwrap_or("")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("run", EXIT_OK, &run_report_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn run_report_json(report: &BasicRunReport) -> String {
    let deliveries = report.deliveries.iter().map(|delivery| {
        format!(
            "{{\"agent_id\":{},\"delivery\":{}}}",
            json_string(delivery.agent_id.as_str()),
            json_string(delivery.delivery.as_str())
        )
    });
    let agent_runs = report.agent_runs.iter().map(|run| {
        let error = run
            .error
            .as_ref()
            .map(|error| json_string(error.message()))
            .unwrap_or_else(|| "null".to_owned());
        format!(
            "{{\"agent_id\":{},\"event_id\":{},\"topic\":{},\"status\":{},\"attempts\":{},\"handler_status\":{},\"output\":{},\"error\":{}}}",
            json_string(run.agent_id.as_str()),
            json_string(run.event_id.as_str()),
            json_string(run.topic.as_str()),
            json_string(run.status.as_str()),
            run.attempts,
            option_json(run.handler_status.as_deref()),
            option_json(run.output.as_deref()),
            error
        )
    });
    let lua_results = report.lua_results.iter().map(|result| {
        format!(
            "{{\"agent_id\":{},\"status\":{},\"topic\":{},\"note\":{},\"capability\":{},\"capability_input\":{}}}",
            json_string(result.agent_id.as_str()),
            json_string(&result.status),
            json_string(result.topic.as_str()),
            option_json(result.note.as_deref()),
            result
                .capability
                .as_ref()
                .map(|capability| json_string(capability.as_str()))
                .unwrap_or_else(|| "null".to_owned()),
            option_json(result.capability_input.as_deref())
        )
    });
    let lua_observability = report.lua_observability.iter().map(|observation| {
        let fields = observation.fields.iter().map(|(key, value)| {
            format!(
                "{{\"key\":{},\"value\":{}}}",
                json_string(key),
                json_string(value)
            )
        });
        format!(
            "{{\"action\":{},\"outcome\":{},\"message\":{},\"fields\":{},\"trace\":{}}}",
            json_string(observation.action.as_str()),
            json_string(observation.outcome.as_str()),
            option_json(observation.message.as_deref()),
            json_array(fields),
            trace_json(&observation.trace)
        )
    });
    let capability_response = report
        .capability_response
        .as_ref()
        .map(capability_response_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"runtime_mode\":{},\"generation_id\":{},\"project_root\":{},\"task\":{},\"event_id\":{},\"topic\":{},\"receipt\":{{\"event_id\":{},\"sequence\":{},\"topic\":{},\"target\":{}}},\"deliveries\":{},\"agent_runs\":{},\"lua_results\":{},\"lua_observability\":{},\"lua_generation\":{{\"generation_id\":{},\"script_count\":{}}},\"capability_response\":{},\"audit\":{}}}",
        json_string(&report.runtime_mode),
        json_string(&report.generation_id),
        json_string(&report.project_root),
        task_cmd::task_snapshot_json_from_report(report),
        json_string(&report.event_id),
        json_string(&report.topic),
        json_string(report.receipt.event_id.as_str()),
        report.receipt.sequence,
        json_string(report.receipt.topic.as_str()),
        json_string(&format!("{:?}", report.receipt.target)),
        json_array(deliveries),
        json_array(agent_runs),
        json_array(lua_results),
        json_array(lua_observability),
        json_string(report.lua_generation.generation_id.as_str()),
        report.lua_generation.script_count,
        capability_response,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn capability_response_json(response: &eva_core::InvokeResponse) -> String {
    format!(
        "{{\"request_id\":{},\"status\":{},\"output\":{},\"error\":{}}}",
        json_string(response.request_id().as_str()),
        json_string(invoke_status(response.status())),
        response
            .output()
            .and_then(|output| output.as_text())
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned()),
        response
            .error()
            .map(|error| json_string(error.message()))
            .unwrap_or_else(|| "null".to_owned())
    )
}

fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
    }
}

fn write_error<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    command: &str,
    exit_code: i32,
    error: &EvaError,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(
                writer,
                "ERROR {command} [{}] {}",
                error.kind().as_str(),
                error.message()
            )
            .map_err(write_error_kind)?;
            for (key, value) in error.context().entries() {
                writeln!(writer, "{key}: {value}").map_err(write_error_kind)?;
            }
            writeln!(writer, "suggestion: {}", suggestion_for_error(error))
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            error_envelope(command, exit_code, error, trace)
        )
        .map_err(write_error_kind),
    }
}

fn success_envelope(command: &str, exit_code: i32, data_json: &str, trace: &TraceFields) -> String {
    format!(
        "{{\"ok\":true,\"command\":{},\"exit_code\":{},\"data\":{},\"trace\":{}}}",
        json_string(command),
        exit_code,
        data_json,
        trace_json(trace)
    )
}

fn error_envelope(command: &str, exit_code: i32, error: &EvaError, trace: &TraceFields) -> String {
    let provider_code = error
        .provider_code()
        .map(|code| json_string(code.as_str()))
        .unwrap_or_else(|| "null".to_owned());
    let context = error
        .context()
        .entries()
        .iter()
        .map(|(key, value)| {
            format!(
                "{{\"key\":{},\"value\":{}}}",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "{{\"ok\":false,\"command\":{},\"exit_code\":{},\"error\":{{\"kind\":{},\"message\":{},\"retryable\":{},\"provider_code\":{},\"context\":{},\"suggestion\":{}}},\"trace\":{}}}",
        json_string(command),
        exit_code,
        json_string(error.kind().as_str()),
        json_string(error.message()),
        error.is_retryable(),
        provider_code,
        json_array(context),
        json_string(&suggestion_for_error(error)),
        trace_json(trace)
    )
}

fn trace_for(span_id: &str) -> TraceFields {
    TraceFields::default().with_span_id(
        SpanId::parse(span_id)
            .expect("static CLI span identifiers use the eva-observability character set"),
    )
}

fn trace_json(trace: &TraceFields) -> String {
    let fields = trace
        .entries()
        .into_iter()
        .map(|(key, value)| format!("{}:{}", json_string(key), json_string(&value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", fields.join(","))
}

pub(crate) fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value if value.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", value as u32));
            }
            value => escaped.push(value),
        }
    }
    escaped.push('"');
    escaped
}

pub(crate) fn json_array<I>(values: I) -> String
where
    I: IntoIterator<Item = String>,
{
    format!("[{}]", values.into_iter().collect::<Vec<_>>().join(","))
}

pub(crate) fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn exit_code_for_error(error: &EvaError) -> i32 {
    match error.kind() {
        ErrorKind::PermissionDenied => EXIT_POLICY,
        ErrorKind::Timeout | ErrorKind::Unavailable => EXIT_EXTERNAL_UNAVAILABLE,
        ErrorKind::Unsupported => EXIT_RUNTIME_UNAVAILABLE,
        ErrorKind::InvalidArgument | ErrorKind::NotFound | ErrorKind::Conflict => EXIT_CONFIG,
        ErrorKind::Internal => EXIT_INTERNAL,
    }
}

fn suggestion_for_error(error: &EvaError) -> String {
    if let Some((_, suggestion)) = error
        .context()
        .entries()
        .iter()
        .find(|(key, _)| key == "suggestion")
    {
        return suggestion.clone();
    }

    match error.kind() {
        ErrorKind::InvalidArgument | ErrorKind::NotFound | ErrorKind::Conflict => {
            "确认 --project 指向 Eva workspace，并检查 config/eva.yaml、manifest、routes 与 schema 路径。"
                .to_owned()
        }
        ErrorKind::PermissionDenied => {
            "检查 policy 和 manifest 权限声明，确认请求没有扩大 effective policy。".to_owned()
        }
        ErrorKind::Timeout | ErrorKind::Unavailable | ErrorKind::Unsupported => {
            "该能力当前不可用；先运行 eva adapter list/probe、eva mcp probe、eva discovery scan 或 eva task logs 查看诊断。"
                .to_owned()
        }
        ErrorKind::Internal => "查看上方上下文并保留命令输出作为缺陷报告证据。".to_owned(),
    }
}

fn write_error_kind(error: io::Error) -> EvaError {
    EvaError::internal("failed to write CLI output").with_context("io_error", error.to_string())
}

fn help_text() -> &'static str {
    concat!(
        "Eva CLI\n\n",
        "USAGE:\n",
        "  eva --version\n",
        "  eva version [--output text|json]\n",
        "  eva doctor [--project <path>] [--output text|json]\n",
        "  eva config validate [--project <path>] [--output text|json]\n",
        "  eva inspect [all|config|runtime] [--project <path>] [--output text|json]\n",
        "  eva inspect durable --durable-backend <path> [--redrive-ready-at-ms <ms>] [--output text|json]\n",
        "  eva run --example basic [--project <path>] [--task-id <id>] [--durable-backend <path>] [--output text|json] [--timeout-ms <ms>] [--retry-attempts <n>] [--cancel] [--replay-dead-letters]\n",
        "  eva task status [--project <path>] [--task <id>] [--durable-backend <path>] [--output text|json]\n",
        "  eva task logs [--project <path>] [--task <id>] [--durable-backend <path>] [--output text|json]\n",
        "  eva task cancel [--project <path>] [--task <id>] [--durable-backend <path>] [--reason <text>] [--output text|json]\n",
        "  eva adapter list [--project <path>] [--output text|json]\n",
        "  eva adapter probe [--adapter <id>|--capability <name>] [--provider <id>] [--project <path>] [--output text|json]\n",
        "  eva mcp list [--project <path>] [--output text|json]\n",
        "  eva mcp probe [--adapter <id>] [--tool <name>] [--project <path>] [--output text|json]\n",
        "  eva skill list [--project <path>] [--output text|json]\n",
        "  eva skill run [--skill <id>|--adapter <id>] [--capability <name>] [--input <json>] [--request-id <id>] [--project <path>] [--output text|json]\n",
        "  eva discovery scan [--project <path>] [--output text|json]\n",
        "  eva memory context [--agent <id>] [--query <text>] [--private-limit <n>] [--global-limit <n>] [--knowledge-limit <n>] [--durable-backend <path>] [--project <path>] [--output text|json]\n",
        "  eva observability smoke [--backend <path>] [--project <path>] [--output text|json]\n",
        "  eva hardware list [--project <path>] [--output text|json]\n",
        "  eva hardware probe [--adapter <id>] [--project <path>] [--output text|json]\n",
        "  eva hardware bind [--adapter <id>] [--request-id <id>] [--apply] [--project <path>] [--output text|json]\n",
        "  eva backup create [--artifact-id <id>] [--request-id <id>] [--reason <text>] [--artifact-store <path>] [--dry-run] [--encrypt] [--project <path>] [--output text|json]\n",
        "  eva snapshot create [--snapshot-id <id>] [--release <ref>] [--role pre_release|post_release] [--artifact-store <path>] [--project <path>] [--output text|json]\n",
        "  eva snapshot promote --snapshot-id <id> --confirm <snapshot_id> [--release <ref>] [--artifact-store <path>] [--project <path>] [--output text|json]\n",
        "  eva restore plan [--snapshot-id <id>] [--release <ref>] [--artifact-store <path>] [--project <path>] [--output text|json]\n",
        "  eva restore apply --plan <path> --confirm <plan_id> --artifact-store <path> --lock-store <path> [--dry-run] [--owner <id>] [--health healthy|failed] [--project <path>] [--output text|json]\n",
        "  eva upgrade check [--from-generation <id>] [--to-generation <id>] [--from-release <ref>] [--to-release <ref>] [--project <path>] [--output text|json]\n",
        "  eva upgrade apply --plan <path> --confirm <plan_id> --lock-store <path> [--state-store <path>] [--runtime-binary <path>] [--health healthy|failed|unavailable] [--owner <id>] [--project <path>] [--output text|json]\n",
        "  eva release check [--target all|windows|linux|macos] [--artifact-evidence <path>] [--distribution-evidence <path>] [--security-scan-evidence <path>] [--benchmark-evidence <path>] [--project <path>] [--output text|json]\n",
        "  eva release security [--project <path>] [--output text|json]\n",
        "  eva release perf [--benchmark-evidence <path>] [--project <path>] [--output text|json]\n",
        "  eva release migration [--from-version <semver>] [--to-version <semver>] [--project <path>] [--output text|json]\n\n",
        "Commands:\n",
        "  version          Print the V1.5 release version and supported contracts.\n",
        "  doctor           Check workspace, configuration roots, schema files, and runtime boundaries.\n",
        "  config validate  Load eva.yaml plus split manifests and report stable diagnostics.\n",
        "  inspect          Show project surfaces or durable backend diagnostics without mutating runtime state.\n",
        "  run              Execute the V1.0-compatible in-memory basic event loop and persist the latest task report under .eva/tasks or a durable backend task store.\n",
        "  task             Inspect or cancel the latest persisted basic task report from .eva/tasks or a durable backend task store.\n",
        "  adapter          List and probe authorized Adapter handles derived from manifests.\n",
        "  mcp              List and probe allowlisted MCP tools without starting external servers.\n",
        "  skill            List and run controlled workflow skill runners.\n",
        "  discovery        Scan trusted configuration sources and return candidates without granting runtime handles.\n",
        "  memory           Build request-scoped private/global memory plus knowledge context for one Agent.\n",
        "  hardware         List, probe, and plan hardware bindings without opening raw I/O.\n",
        "  backup           Create and verify a V1.4 backup artifact, optionally in a filesystem ArtifactStore.\n",
        "  snapshot         Capture or plan promotion for a release snapshot without moving release pointer.\n",
        "  restore          Produce a plan-first restore plan; no destructive mutation is executed.\n",
        "  upgrade          Check, lock, or commit policy-gated generation handoff state.\n",
        "  release          Run V1.5 cross-platform, security, performance, migration, and compatibility release gates.\n\n",
        "Exit codes:\n",
        "  0 success\n",
        "  2 configuration or validation error\n",
        "  3 policy denied\n",
        "  4 runtime unavailable or unsupported in this version\n",
        "  5 external capability unavailable\n",
        "  64 command usage error\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_storage::{ArtifactStore, FileSystemArtifactStore};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn run_cli(args: &[&str]) -> (i32, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run_with_args(args.iter().map(OsString::from), &mut stdout, &mut stderr);
        (
            exit_code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("eva-cli-{name}-{}-{now}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let source = entry.path();
            let destination = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&source, &destination);
            } else {
                fs::copy(&source, &destination).unwrap();
            }
        }
    }

    fn restore_apply_fixture(
        name: &str,
        plan_id: &str,
        backup_id: &str,
        pre_restore_id: &str,
    ) -> (PathBuf, PathBuf) {
        let artifact_root = test_temp_dir(name);
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes(format!("backup/{backup_id}"), b"ok".as_slice())
            .unwrap();
        let pre_restore = store
            .put_bytes(format!("backup/{pre_restore_id}"), b"before".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id={plan_id}\nbackup_artifact_id={backup_id}\nbackup_digest={}\npre_restore_backup_artifact_id={pre_restore_id}\npre_restore_backup_digest={}\n",
                artifact.digest, pre_restore.digest
            ),
        )
        .unwrap();
        (artifact_root, plan_path)
    }

    fn project_with_restore_apply_allowed(name: &str) -> PathBuf {
        let root = workspace_root();
        let fixture = test_temp_dir(name);
        copy_dir(&root.join("config"), &fixture.join("config"));
        fs::write(
            fixture.join("config/policies/restore-allow.yaml"),
            "runtime_policy:\n  allow_high_risk_actions:\n    - restore.apply\n",
        )
        .unwrap();
        fixture
    }

    fn project_with_upgrade_apply_allowed(name: &str) -> PathBuf {
        let root = workspace_root();
        let fixture = test_temp_dir(name);
        copy_dir(&root.join("config"), &fixture.join("config"));
        fs::write(
            fixture.join("config/policies/upgrade-allow.yaml"),
            "runtime_policy:\n  allow_high_risk_actions:\n    - supervisor.handoff\n    - release.pointer_mutation\n",
        )
        .unwrap();
        fixture
    }

    fn upgrade_apply_plan_fixture(name: &str, plan_id: &str) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let plan_path = root.join("upgrade.plan");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id={plan_id}\nfrom_generation=gen-v14\nto_generation=gen-v15\nfrom_release=1.4.0\nto_release=1.5.1\n"
            ),
        )
        .unwrap();
        (root, plan_path)
    }

    fn release_artifact_evidence_fixture(name: &str, signed: bool) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let evidence_path = root.join("release-artifact.evidence");
        fs::create_dir_all(&root).unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let key = eva_release::ReleaseArtifactSigningKey::local_development();
        let artifact = eva_release::ReleaseArtifactSubject::new(
            "eva-cli-1.7.4-alpha-x86_64-unknown-linux-gnu.tar.gz",
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "eva",
            "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df",
            4096,
            signed,
        )
        .unwrap();
        let provenance = eva_release::ReleaseProvenanceEvidence::new(
            "github-actions",
            commit,
            "cargo-build-release-locked-bin-eva",
            "release",
            "spdx:release-evidence/eva.spdx.json",
            "passed",
        )
        .unwrap();
        let signature = eva_release::ReleaseArtifactSignature::new(
            key.key_id(),
            eva_release::artifact::RELEASE_SIGNATURE_ALGORITHM,
            "pending",
        )
        .unwrap();
        let mut evidence = eva_release::ReleaseArtifactEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
            commit,
            artifact,
            provenance,
            signature,
        )
        .unwrap();
        evidence.signature = evidence.sign(&key);
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        (root, evidence_path)
    }

    fn release_distribution_evidence_fixture(
        name: &str,
        package_status: &str,
    ) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let evidence_path = root.join("release-distribution.evidence");
        fs::create_dir_all(&root).unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let install_doc = "docs/en/release/install-upgrade-uninstall.md";
        let smoke = |os: &str, target: &str, artifact: &str, package_format: &str| {
            eva_release::ReleaseInstallSmokeEvidence::new(
                os,
                target,
                artifact,
                package_format,
                format!("install {artifact}"),
                "eva --version",
                format!("uninstall {artifact}"),
                format!("upgrade {artifact}"),
                "passed",
            )
            .unwrap()
        };
        let dry_run = eva_release::ReleasePackageDryRunEvidence::new(
            "ghcr",
            "ghcr.io/yetmos/eva-cli",
            "linux/amd64+linux/arm64",
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.7.4-alpha",
            package_status,
        )
        .unwrap();
        let evidence = eva_release::ReleaseDistributionEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
            commit,
            install_doc,
            install_doc,
            install_doc,
            vec![
                smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.7.4-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                ),
                smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.7.4-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                ),
                smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.7.4-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                ),
            ],
            vec![dry_run],
        )
        .unwrap();
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        (root, evidence_path)
    }

    fn release_security_scan_evidence_fixture(
        name: &str,
        scan_status: &str,
        severity: Option<&str>,
    ) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let evidence_path = root.join("release-security-scan.evidence");
        fs::create_dir_all(&root).unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let findings = severity
            .map(|severity| {
                vec![eva_release::ReleaseSecurityScanFinding::new(
                    "RUSTSEC-0000-0000",
                    "demo-crate",
                    "1.0.0",
                    severity,
                    "demo advisory",
                    "upgrade demo-crate",
                )
                .unwrap()]
            })
            .unwrap_or_default();
        let evidence = eva_release::ReleaseSecurityScanEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
            commit,
            "cargo-audit",
            "1.0.0",
            scan_status,
            "cargo audit --json",
            findings,
        )
        .unwrap();
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        (root, evidence_path)
    }

    fn release_benchmark_evidence_fixture(
        name: &str,
        status: &str,
        observed_ms: u64,
    ) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let evidence_path = root.join("release-benchmark.evidence");
        fs::create_dir_all(&root).unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let measurement = eva_release::ReleaseBenchmarkMeasurement::new(
            "release.check",
            "cli release check wall time",
            200,
            observed_ms,
            3,
            "target/release/eva release check --output json",
            "github-actions-ubuntu-latest",
        )
        .unwrap();
        let evidence = eva_release::ReleaseBenchmarkEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
            commit,
            status,
            vec![measurement],
        )
        .unwrap();
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        (root, evidence_path)
    }

    #[cfg(unix)]
    fn executable_runtime_binary_fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let binary_path = root.join("eva-runtime-smoke.sh");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &binary_path,
            "#!/bin/sh\nprintf 'eva-runtime-smoke 1.0.0\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&binary_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary_path, permissions).unwrap();
        (root, binary_path)
    }

    #[test]
    fn config_validate_json_succeeds_for_sample_project() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "config",
            "validate",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"ok\":true"));
        assert!(stdout.contains("\"command\":\"config.validate\""));
        assert!(stdout.contains("\"agents_total\""));
        assert!(stderr.is_empty());
    }

    #[test]
    fn config_validate_json_reports_schema_rule_context() {
        let root = workspace_root();
        let fixture = test_temp_dir("config-schema-json");
        copy_dir(&root.join("config"), &fixture.join("config"));
        fs::write(
            fixture.join("config/routes/topics.yaml"),
            r#"routes:
  - pattern: /sys
    delivery: fanout
    agents:
      - root-agent
    extra: denied
"#,
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "config",
            "validate",
            "--project",
            fixture.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"config.validate\""));
        assert!(stderr.contains("\"kind\":\"invalid_argument\""));
        assert!(stderr.contains("\"key\":\"schema_rule\",\"value\":\"additionalProperties\""));
        assert!(stderr.contains("\"key\":\"field\",\"value\":\"routes[0].extra\""));

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn inspect_text_reports_noop_runtime() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) =
            run_cli(&["inspect", "--project", root.to_str().unwrap()]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("runtime: mode=noop"));
        assert!(stdout.contains("agents:"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn unknown_command_is_usage_error() {
        let (exit_code, _stdout, stderr) = run_cli(&["missing"]);

        assert_eq!(exit_code, EXIT_USAGE);
        assert!(stderr.contains("unknown command"));
    }

    #[test]
    fn run_basic_example_json_succeeds() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "run",
            "--example",
            "basic",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"run\""));
        assert!(stdout.contains("\"runtime_mode\":\"in_memory_v1.0\""));
        assert!(stdout.contains("\"generation_id\":\"basic-v1.0\""));
        assert!(stdout.contains("\"task\""));
        assert!(stdout.contains("\"status\":\"completed\""));
        assert!(stdout.contains("\"lua_observability\""));
        assert!(stdout.contains("lua.host.log"));
        assert!(stdout.contains("lua.host.audit"));
        assert!(stdout.contains("\"capability_response\""));
        assert!(stdout.contains("tool=completed"));
        assert!(stdout.contains("valid"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn task_status_logs_and_cancel_use_persisted_basic_report() {
        let root = workspace_root();
        let task_id = "req-test-v05";
        let (run_exit, _run_stdout, run_stderr) = run_cli(&[
            "run",
            "--example",
            "basic",
            "--project",
            root.to_str().unwrap(),
            "--task-id",
            task_id,
            "--output",
            "json",
        ]);
        assert_eq!(run_exit, EXIT_OK, "{run_stderr}");

        let (status_exit, status_stdout, status_stderr) = run_cli(&[
            "task",
            "status",
            "--project",
            root.to_str().unwrap(),
            "--task",
            task_id,
            "--output",
            "json",
        ]);
        assert_eq!(status_exit, EXIT_OK, "{status_stderr}");
        assert!(status_stdout.contains("\"command\":\"task.status\""));
        assert!(status_stdout.contains("\"status\":\"completed\""));

        let (logs_exit, logs_stdout, logs_stderr) = run_cli(&[
            "task",
            "logs",
            "--project",
            root.to_str().unwrap(),
            "--task",
            task_id,
            "--output",
            "json",
        ]);
        assert_eq!(logs_exit, EXIT_OK, "{logs_stderr}");
        assert!(logs_stdout.contains("event accepted"));

        let (cancel_exit, cancel_stdout, cancel_stderr) = run_cli(&[
            "task",
            "cancel",
            "--project",
            root.to_str().unwrap(),
            "--task",
            task_id,
            "--reason",
            "test cleanup",
            "--output",
            "json",
        ]);
        assert_eq!(cancel_exit, EXIT_OK, "{cancel_stderr}");
        assert!(cancel_stdout.contains("\"requested\":true"));
        assert!(cancel_stdout.contains("\"accepted\":false"));
    }

    #[test]
    fn task_commands_can_use_durable_backend_task_store() {
        let root = workspace_root();
        let durable_root = test_temp_dir("durable-task-store");
        let task_id = "req-test-durable-task-store";
        let (run_exit, _run_stdout, run_stderr) = run_cli(&[
            "run",
            "--example",
            "basic",
            "--project",
            root.to_str().unwrap(),
            "--task-id",
            task_id,
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(run_exit, EXIT_OK, "{run_stderr}");
        assert!(durable_root.join("backend.manifest").is_file());
        assert!(durable_root
            .join("tasks")
            .join(format!("{task_id}.task"))
            .is_file());

        let (status_exit, status_stdout, status_stderr) = run_cli(&[
            "task",
            "status",
            "--project",
            root.to_str().unwrap(),
            "--task",
            task_id,
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(status_exit, EXIT_OK, "{status_stderr}");
        assert!(status_stdout.contains("\"command\":\"task.status\""));
        assert!(status_stdout.contains("\"status\":\"completed\""));

        let (logs_exit, logs_stdout, logs_stderr) = run_cli(&[
            "task",
            "logs",
            "--project",
            root.to_str().unwrap(),
            "--task",
            task_id,
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(logs_exit, EXIT_OK, "{logs_stderr}");
        assert!(logs_stdout.contains("event accepted"));

        let (cancel_exit, cancel_stdout, cancel_stderr) = run_cli(&[
            "task",
            "cancel",
            "--project",
            root.to_str().unwrap(),
            "--task",
            task_id,
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--reason",
            "durable cleanup",
            "--output",
            "json",
        ]);
        assert_eq!(cancel_exit, EXIT_OK, "{cancel_stderr}");
        assert!(cancel_stdout.contains("\"requested\":true"));
        assert!(cancel_stdout.contains("\"accepted\":false"));
        assert!(cancel_stdout.contains("cancel requested after task reached a terminal state"));

        let _ = fs::remove_dir_all(&durable_root);
    }

    #[test]
    fn inspect_durable_reports_backend_diagnostics_json() {
        let root = workspace_root();
        let durable_root = test_temp_dir("durable-inspect");
        let (run_exit, _run_stdout, run_stderr) = run_cli(&[
            "run",
            "--example",
            "basic",
            "--project",
            root.to_str().unwrap(),
            "--task-id",
            "req-test-durable-inspect",
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(run_exit, EXIT_OK, "{run_stderr}");
        assert!(!durable_root.join("events").join("log").exists());
        assert!(!durable_root.join("events").join("dead_letters").exists());

        let (inspect_exit, inspect_stdout, inspect_stderr) = run_cli(&[
            "inspect",
            "durable",
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(inspect_exit, EXIT_OK, "{inspect_stderr}");
        assert!(inspect_stdout.contains("\"command\":\"inspect.durable\""));
        assert!(inspect_stdout.contains("\"backend_mode\":\"read_only\""));
        assert!(inspect_stdout.contains("\"schema_version\":1"));
        assert!(inspect_stdout.contains("\"layout_version\":\"eva.durable.v1\""));
        assert!(inspect_stdout.contains("\"migration_status\":\"idle\""));
        assert!(inspect_stdout.contains("\"pending_redrive_count\":0"));
        assert!(!durable_root.join("events").join("log").exists());
        assert!(!durable_root.join("events").join("dead_letters").exists());

        let _ = fs::remove_dir_all(&durable_root);
    }

    #[test]
    fn run_cancelled_basic_example_reports_cancelled_task() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "run",
            "--example",
            "basic",
            "--project",
            root.to_str().unwrap(),
            "--task-id",
            "req-test-cancel",
            "--cancel",
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"status\":\"cancelled\""));
        assert!(stdout.contains("\"dead_letters\""));
    }

    #[test]
    fn json_string_escapes_control_characters() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn version_text_and_json_report_v174_lua_hot_reload_lifecycle_alpha() {
        let (text_exit, text_stdout, text_stderr) = run_cli(&["--version"]);
        assert_eq!(text_exit, EXIT_OK, "{text_stderr}");
        assert!(text_stdout.contains("eva 1.7.4-alpha"));
        assert!(text_stdout.contains("V1.7.4-alpha"));
        assert!(text_stdout.contains("status: alpha"));

        let (json_exit, json_stdout, json_stderr) = run_cli(&["version", "--output", "json"]);
        assert_eq!(json_exit, EXIT_OK, "{json_stderr}");
        assert!(json_stdout.contains("\"command\":\"version\""));
        assert!(json_stdout.contains("\"version\":\"1.7.4-alpha\""));
        assert!(json_stdout.contains("\"release\":\"V1.7.4-alpha\""));
        assert!(json_stdout.contains("\"status\":\"alpha\""));
        assert!(json_stdout.contains("release_v1.5"));
        assert!(json_stdout.contains("durable_backend_v1.6.1"));
        assert!(json_stdout.contains("durable_eventbus_v1.6.2"));
        assert!(json_stdout.contains("durable_task_audit_artifact_v1.6.3"));
        assert!(json_stdout.contains("durable_runtime_recovery_v1.6.4"));
        assert!(json_stdout.contains("durable_diagnostics_v1.6.5"));
        assert!(json_stdout.contains("lua_vm_execution_v1.7.1"));
        assert!(json_stdout.contains("lua_host_bindings_v1.7.2"));
        assert!(json_stdout.contains("lua_resource_limits_v1.7.3"));
        assert!(json_stdout.contains("lua_hot_reload_lifecycle_v1.7.4"));
        assert!(json_stdout.contains("restore apply"));
        assert!(json_stdout.contains("release check"));
    }

    #[test]
    fn release_check_json_reports_recovery_gate() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-DURABLE-RECOVERY-001\""));
        assert!(stdout.contains("\"domain\":\"durable_runtime_recovery\""));
        assert!(stdout.contains("RuntimeRecoveryCoordinator"));
        assert!(stdout.contains("cargo test -p eva-cli recovery"));
        assert!(stdout.contains("durable_runtime_recovery_checkpoint_ready"));
        assert!(stdout.contains("\"id\":\"REL-DURABLE-DIAGNOSTICS-001\""));
        assert!(stdout.contains("\"domain\":\"durable_diagnostics\""));
        assert!(stdout.contains("inspect.durable"));
        assert!(stdout.contains("durable_diagnostics_smoke_ready"));
        assert!(stdout.contains("\"id\":\"REL-LUA-VM-EXECUTION-001\""));
        assert!(stdout.contains("\"domain\":\"lua_vm_execution\""));
        assert!(stdout.contains("LuaVmAdapter"));
        assert!(stdout.contains("lua_vm_execution_boundary_ready"));
        assert!(stdout.contains("\"id\":\"REL-LUA-HOST-BINDINGS-001\""));
        assert!(stdout.contains("\"domain\":\"lua_host_bindings\""));
        assert!(stdout.contains("ctx.tools.call"));
        assert!(stdout.contains("lua_host_bindings_ready"));
        assert!(stdout.contains("\"id\":\"REL-LUA-RESOURCE-LIMITS-001\""));
        assert!(stdout.contains("\"domain\":\"lua_resource_limits\""));
        assert!(stdout.contains("LuaExecutionLimits"));
        assert!(stdout.contains("lua_resource_limits_ready"));
        assert!(stdout.contains("\"id\":\"REL-LUA-HOT-RELOAD-001\""));
        assert!(stdout.contains("\"domain\":\"lua_hot_reload_lifecycle\""));
        assert!(stdout.contains("GenerationRouteGate"));
        assert!(stdout.contains("lua_hot_reload_lifecycle_ready"));
    }

    #[test]
    fn release_check_with_signed_artifact_evidence_passes_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_artifact_evidence_fixture("release-artifact-signed", true);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--artifact-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-ARTIFACT-PROVENANCE-001\""));
        assert!(stdout.contains("\"domain\":\"release_artifact_provenance\""));
        assert!(stdout.contains("\"status\":\"pass\""));
        assert!(stdout.contains("signed_artifact_provenance_verified"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_unsigned_artifact_evidence_blocks_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_artifact_evidence_fixture("release-artifact-unsigned", false);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--artifact-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-ARTIFACT-PROVENANCE-001\""));
        assert!(stdout.contains("\"domain\":\"release_artifact_provenance\""));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("release artifact is marked unsigned"));
        assert!(stdout.contains("signed_artifact_provenance_blocked"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_distribution_evidence_passes_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_distribution_evidence_fixture("release-distribution-passed", "passed");
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--distribution-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-DISTRIBUTION-001\""));
        assert!(stdout.contains("\"domain\":\"release_distribution\""));
        assert!(stdout.contains("\"status\":\"pass\""));
        assert!(stdout.contains("distribution_install_smoke_verified"));
        assert!(stdout.contains("package_dry_runs_verified:true"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_failed_distribution_evidence_blocks_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_distribution_evidence_fixture("release-distribution-failed", "failed");
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--distribution-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-DISTRIBUTION-001\""));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("package manager dry-run for ghcr"));
        assert!(stdout.contains("distribution_install_smoke_blocked"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_security_scan_evidence_passes_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_security_scan_evidence_fixture("release-security-scan-passed", "passed", None);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--security-scan-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-SECURITY-SCAN-001\""));
        assert!(stdout.contains("\"domain\":\"external_security_scan\""));
        assert!(stdout.contains("\"status\":\"pass\""));
        assert!(stdout.contains("external_security_scan_verified"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_high_security_scan_finding_blocks_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) = release_security_scan_evidence_fixture(
            "release-security-scan-high",
            "passed",
            Some("high"),
        );
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--security-scan-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-SECURITY-SCAN-001\""));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("security scanner finding RUSTSEC-0000-0000 is high severity"));
        assert!(stdout.contains("external_security_scan_blocked"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_benchmark_evidence_passes_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-benchmark-passed", "passed", 120);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--benchmark-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-BENCHMARK-001\""));
        assert!(stdout.contains("\"domain\":\"production_benchmark\""));
        assert!(stdout.contains("\"status\":\"pass\""));
        assert!(stdout.contains("production_benchmark_verified"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_check_with_benchmark_regression_blocks_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-benchmark-regression", "passed", 250);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--benchmark-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-BENCHMARK-001\""));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("benchmark release.check observed 250ms over 200ms budget"));
        assert!(stdout.contains("production_benchmark_blocked"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_perf_with_benchmark_evidence_uses_observed_measurements() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-perf-benchmark-passed", "passed", 120);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "perf",
            "--benchmark-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"release.perf\""));
        assert!(stdout.contains("\"status\":\"within_budget\""));
        assert!(stdout.contains("\"component\":\"release.check\""));
        assert!(stdout.contains("\"observed_ms\":120"));
        assert!(stdout.contains("performance:benchmark_evidence:v1.11.3"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_perf_with_benchmark_regression_returns_runtime_exit() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-perf-benchmark-regression", "passed", 250);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "perf",
            "--benchmark-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_RUNTIME_UNAVAILABLE, "{stderr}");
        assert!(stdout.contains("\"command\":\"release.perf\""));
        assert!(stdout.contains("\"status\":\"over_budget\""));
        assert!(stdout.contains("\"over_budget\":1"));
        assert!(stdout.contains("\"observed_ms\":250"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn release_perf_with_failed_benchmark_status_returns_runtime_exit() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-perf-benchmark-failed", "failed", 120);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "perf",
            "--benchmark-evidence",
            evidence_path.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_RUNTIME_UNAVAILABLE, "{stderr}");
        assert!(stdout.contains("\"command\":\"release.perf\""));
        assert!(stdout.contains("\"status\":\"over_budget\""));
        assert!(stdout.contains("benchmark_status:failed"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    fn v11_external_capability_commands_report_json() {
        let root = workspace_root();
        let root = root.to_str().unwrap();
        let commands = [
            vec!["adapter", "list", "--project", root, "--output", "json"],
            vec![
                "adapter",
                "probe",
                "--adapter",
                "github-mcp",
                "--project",
                root,
                "--output",
                "json",
            ],
            vec!["mcp", "list", "--project", root, "--output", "json"],
            vec![
                "mcp",
                "probe",
                "--adapter",
                "github-mcp",
                "--tool",
                "list_issues",
                "--project",
                root,
                "--output",
                "json",
            ],
            vec!["skill", "list", "--project", root, "--output", "json"],
            vec![
                "skill",
                "run",
                "--skill",
                "code-review",
                "--input",
                "{\"scope\":\"current_diff\"}",
                "--project",
                root,
                "--output",
                "json",
            ],
            vec!["discovery", "scan", "--project", root, "--output", "json"],
            vec![
                "memory",
                "context",
                "--agent",
                "root-agent",
                "--query",
                "memory",
                "--project",
                root,
                "--output",
                "json",
            ],
            vec!["hardware", "list", "--project", root, "--output", "json"],
            vec![
                "hardware",
                "probe",
                "--adapter",
                "scale-main",
                "--project",
                root,
                "--output",
                "json",
            ],
            vec![
                "hardware",
                "bind",
                "--adapter",
                "scale-main",
                "--project",
                root,
                "--output",
                "json",
            ],
        ];

        for command in commands {
            let (exit_code, stdout, stderr) = run_cli(&command);
            assert_eq!(exit_code, EXIT_OK, "command={command:?} stderr={stderr}");
            assert!(stdout.contains("\"ok\":true"), "{stdout}");
        }
    }

    #[test]
    fn mcp_probe_blocks_unlisted_tool_with_policy_exit() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "mcp",
            "probe",
            "--adapter",
            "github-mcp",
            "--tool",
            "delete_repo",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"status\":\"blocked\""));
    }

    #[test]
    fn discovery_scan_json_reports_source_statuses() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "discovery",
            "scan",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"candidate_count\""), "{stdout}");
        assert!(stdout.contains("\"candidates\""), "{stdout}");
        assert!(stdout.contains("\"source_report_count\""), "{stdout}");
        assert!(stdout.contains("\"source_reports\""), "{stdout}");
        assert!(
            stdout.contains("\"source_id\":\"path_commands\""),
            "{stdout}"
        );
        assert!(stdout.contains("\"source_id\":\"mcp\""), "{stdout}");
        assert!(
            stdout.contains("\"source_id\":\"external_registry\""),
            "{stdout}"
        );
        assert!(
            stdout.contains("\"rejected_reason\":\"external registry source is not configured\""),
            "{stdout}"
        );
    }

    #[test]
    fn skill_run_json_links_adapter_audit_to_invocation_trace() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "skill",
            "run",
            "--skill",
            "code-review",
            "--request-id",
            "req-trace-skill",
            "--input",
            "{\"scope\":\"current_diff\"}",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"audit\":[\"adapter.invoked:code-review-skill\""));
        assert!(stdout.contains("\"trace\":{\"request_id\":\"req-trace-skill\""));
        assert!(stdout.contains("\"adapter_id\":\"code-review-skill\""));
        assert!(stdout.contains("\"capability\":\"workflow.code_review\""));
        assert!(stdout.contains("\"provider\":\"code-review-skill\""));
    }

    #[test]
    fn memory_context_reports_private_global_and_knowledge() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "memory",
            "context",
            "--project",
            root.to_str().unwrap(),
            "--agent",
            "root-agent",
            "--query",
            "context",
            "--private-limit",
            "1",
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"memory.context\""));
        assert!(stdout.contains("\"private_memory\":1"));
        assert!(stdout.contains("\"global_memory\""));
        assert!(stdout.contains("\"knowledge\""));
        assert!(stdout.contains("\"lua_context\""));
    }

    #[test]
    fn memory_context_can_use_durable_backend_with_redaction_and_expiration() {
        let root = workspace_root();
        let durable_root = test_temp_dir("memory-durable");
        let (exit_code, stdout, stderr) = run_cli(&[
            "memory",
            "context",
            "--project",
            root.to_str().unwrap(),
            "--agent",
            "root-agent",
            "--query",
            "memory",
            "--private-limit",
            "8",
            "--durable-backend",
            durable_root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"memory.context\""));
        assert!(stdout.contains("\"key\":\"session.secret\""), "{stdout}");
        assert!(
            stdout.contains("\"value\":\"token=[REDACTED]\""),
            "{stdout}"
        );
        assert!(
            stdout.contains("\"compression\":\"run_length\""),
            "{stdout}"
        );
        assert!(
            stdout.contains("\"redaction\":1") || stdout.contains("redaction:1"),
            "{stdout}"
        );
        assert!(!stdout.contains("expired.note"), "{stdout}");
        assert!(!stdout.contains("expired-secret"), "{stdout}");
        assert!(durable_root.join("state").join("memory").is_dir());
        assert!(durable_root.join("state").join("knowledge").is_dir());
        fs::remove_dir_all(durable_root).ok();
    }

    #[test]
    fn observability_smoke_writes_backend_and_reports_degraded_mode() {
        let backend = test_temp_dir("observability");
        let (exit_code, stdout, stderr) = run_cli(&[
            "observability",
            "smoke",
            "--backend",
            backend.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"observability.smoke\""));
        assert!(stdout.contains("\"degraded\":false"), "{stdout}");
        assert!(stdout.contains("\"audit_events\":1"), "{stdout}");
        assert!(stdout.contains("\"metric_points\":3"), "{stdout}");
        assert!(stdout.contains("\"otel_spans\":2"), "{stdout}");
        assert!(backend.join("audit.jsonl").is_file());
        assert!(backend.join("metrics.jsonl").is_file());
        assert!(backend.join("otel-spans.jsonl").is_file());
        fs::remove_dir_all(&backend).ok();

        let degraded_path = test_temp_dir("observability-degraded");
        fs::write(&degraded_path, b"not a directory").unwrap();
        let (degraded_exit, degraded_stdout, degraded_stderr) = run_cli(&[
            "observability",
            "smoke",
            "--backend",
            degraded_path.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(degraded_exit, EXIT_OK, "{degraded_stderr}");
        assert!(
            degraded_stdout.contains("\"degraded\":true"),
            "{degraded_stdout}"
        );
        fs::remove_file(degraded_path).ok();
    }

    #[test]
    fn hardware_commands_report_candidates_and_bind_plan() {
        let root = workspace_root();
        let root = root.to_str().unwrap();
        let (list_exit, list_stdout, list_stderr) =
            run_cli(&["hardware", "list", "--project", root, "--output", "json"]);
        assert_eq!(list_exit, EXIT_OK, "{list_stderr}");
        assert!(list_stdout.contains("\"command\":\"hardware.list\""));
        assert!(list_stdout.contains("scale-main"));
        assert!(list_stdout.contains("\"handle_granted\":false"));

        let (bind_exit, bind_stdout, bind_stderr) = run_cli(&[
            "hardware",
            "bind",
            "--adapter",
            "scale-main",
            "--project",
            root,
            "--output",
            "json",
        ]);
        assert_eq!(bind_exit, EXIT_OK, "{bind_stderr}");
        assert!(bind_stdout.contains("\"command\":\"hardware.bind\""));
        assert!(bind_stdout.contains("\"status\":\"blocked\""));
        assert!(bind_stdout.contains("raw I/O"));
    }

    #[test]
    fn v14_backup_lifecycle_commands_report_json() {
        let root = workspace_root();
        let root = root.to_str().unwrap();
        let commands = [
            vec!["backup", "create", "--project", root, "--output", "json"],
            vec!["snapshot", "create", "--project", root, "--output", "json"],
            vec!["restore", "plan", "--project", root, "--output", "json"],
            vec!["upgrade", "check", "--project", root, "--output", "json"],
        ];

        for command in commands {
            let (exit_code, stdout, stderr) = run_cli(&command);
            assert_eq!(exit_code, EXIT_OK, "command={command:?} stderr={stderr}");
            assert!(stdout.contains("\"ok\":true"), "{stdout}");
        }

        let (_exit_code, restore_stdout, _stderr) =
            run_cli(&["restore", "plan", "--project", root, "--output", "json"]);
        assert!(restore_stdout.contains("\"apply_allowed\":false"));

        let (_exit_code, upgrade_stdout, _stderr) =
            run_cli(&["upgrade", "check", "--project", root, "--output", "json"]);
        assert!(upgrade_stdout.contains("\"status\":\"ready\""));
        assert!(upgrade_stdout.contains("rollback"));
    }

    #[test]
    fn snapshot_promote_plans_release_pointer_without_apply() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("snapshot-promote-ok");

        let (exit_code, stdout, stderr) = run_cli(&[
            "snapshot",
            "promote",
            "--snapshot-id",
            "snapshot-promote",
            "--confirm",
            "snapshot-promote",
            "--release",
            "1.7.4-alpha",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"snapshot.promote\""));
        assert!(stdout.contains("\"release_pointer_plan\""));
        assert!(stdout.contains("\"pointer_path\":\"state/release-pointer\""));
        assert!(stdout.contains("\"apply_allowed\":false"));
        assert!(stdout.contains("\"snapshot.promote:planned\""));
        assert!(stdout.contains("\"span_id\":\"cli.snapshot.promote\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn snapshot_promote_rejects_mismatched_confirmation() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("snapshot-promote-confirm");

        let (exit_code, stdout, stderr) = run_cli(&[
            "snapshot",
            "promote",
            "--snapshot-id",
            "snapshot-confirm",
            "--confirm",
            "wrong-snapshot",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_POLICY);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"snapshot.promote\""));
        assert!(stderr.contains("\"kind\":\"permission_denied\""));
        assert!(stderr.contains("\"span_id\":\"cli.snapshot.promote\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn snapshot_promote_requires_confirmation() {
        let root = workspace_root();

        let (exit_code, stdout, stderr) = run_cli(&[
            "snapshot",
            "promote",
            "--snapshot-id",
            "snapshot-missing-confirm",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"snapshot.promote\""));
        assert!(stderr.contains("\"required_option\",\"value\":\"--confirm\""));
    }

    #[test]
    fn restore_apply_requires_lock_store_before_policy_gate() {
        let root = workspace_root();
        let (artifact_root, plan_path) = restore_apply_fixture(
            "restore-apply-missing-lock",
            "plan-lock",
            "apply-lock",
            "pre-lock",
        );

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-lock",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"restore.apply\""));
        assert!(stderr.contains("\"kind\":\"invalid_argument\""));
        assert!(stderr.contains("\"required_option\",\"value\":\"--lock-store\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn restore_apply_default_policy_denies_without_locking() {
        let root = workspace_root();
        let (artifact_root, plan_path) = restore_apply_fixture(
            "restore-apply-policy-deny",
            "plan-deny",
            "apply-deny",
            "pre-deny",
        );
        let lock_root = test_temp_dir("restore-apply-policy-deny-lock");

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-deny",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_POLICY);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"restore.apply\""));
        assert!(stderr.contains("\"kind\":\"permission_denied\""));
        assert!(stderr.contains("\"action\",\"value\":\"restore.apply\""));
        assert!(!lock_root.join("plan-deny.restore.lock").exists());

        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(lock_root).ok();
    }

    #[test]
    fn restore_apply_gates_when_policy_lock_and_health_pass() {
        let project = project_with_restore_apply_allowed("restore-apply-allowed-project");
        let (artifact_root, plan_path) = restore_apply_fixture(
            "restore-apply-allowed",
            "plan-allowed",
            "apply-allowed",
            "pre-allowed",
        );
        let lock_root = test_temp_dir("restore-apply-allowed-lock");

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-allowed",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"restore.apply\""));
        assert!(stdout.contains("\"status\":\"gated\""));
        assert!(stdout.contains("\"apply_allowed\":true"));
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stdout.contains("\"lock_id\":\"restore-apply-plan-allowed\""));
        assert!(stdout.contains("\"health\":{\"healthy\":true"));
        assert!(stdout.contains("\"rollback_plan\":null"));
        assert!(lock_root.join("plan-allowed.restore.lock").exists());

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    fn restore_apply_health_failure_emits_rollback_plan() {
        let project = project_with_restore_apply_allowed("restore-apply-health-project");
        let (artifact_root, plan_path) = restore_apply_fixture(
            "restore-apply-health",
            "plan-health",
            "apply-health",
            "pre-health",
        );
        let lock_root = test_temp_dir("restore-apply-health-lock");

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-health",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--health",
            "failed",
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_RUNTIME_UNAVAILABLE, "{stderr}");
        assert!(stdout.contains("\"command\":\"restore.apply\""));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("\"apply_allowed\":false"));
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stdout.contains("\"rollback_plan\":{"));
        assert!(stdout.contains("\"rollback:planned\""));
        assert!(lock_root.join("plan-health.restore.lock").exists());

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    fn restore_apply_reports_lock_conflict() {
        let project = project_with_restore_apply_allowed("restore-apply-conflict-project");
        let (artifact_root, plan_path) = restore_apply_fixture(
            "restore-apply-conflict",
            "plan-conflict",
            "apply-conflict",
            "pre-conflict",
        );
        let lock_root = test_temp_dir("restore-apply-conflict-lock");

        let first = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-conflict",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(first.0, EXIT_OK, "{}", first.2);

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-conflict",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"conflict\""));
        assert!(stderr.contains("\"command\":\"restore.apply\""));
        assert!(stderr.contains("restore apply lock already exists"));

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    fn upgrade_apply_acquires_filesystem_lock() {
        let root = workspace_root();
        let lock_root = test_temp_dir("upgrade-apply-lock");
        let plan_path = lock_root.join("upgrade.plan");
        fs::create_dir_all(&lock_root).unwrap();
        fs::write(
            &plan_path,
            "plan_id=plan-upgrade\nfrom_generation=gen-v14\nto_generation=gen-v15\nfrom_release=1.4.0\nto_release=1.5.1\n",
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-upgrade",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"upgrade.apply\""));
        assert!(stdout.contains("\"status\":\"locked\""));
        assert!(stdout.contains("\"apply_allowed\":false"));
        assert!(stdout.contains("\"lock_id\":\"upgrade-apply-plan-upgrade\""));
        assert!(lock_root.join("plan-upgrade.lock").exists());

        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    fn upgrade_apply_handoff_commits_release_pointer_when_policy_health_and_state_store_pass() {
        let project = project_with_upgrade_apply_allowed("upgrade-apply-allowed-project");
        let (plan_root, plan_path) =
            upgrade_apply_plan_fixture("upgrade-apply-handoff", "plan-handoff");
        let lock_root = test_temp_dir("upgrade-apply-handoff-lock");
        let state_root = test_temp_dir("upgrade-apply-handoff-state");

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-handoff",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--state-store",
            state_root.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"upgrade.apply\""));
        assert!(stdout.contains("\"status\":\"committed\""));
        assert!(stdout.contains("\"apply_allowed\":true"));
        assert!(stdout.contains("\"mutation_executed\":true"));
        assert!(stdout.contains("\"release_pointer\":{"));
        assert!(stdout.contains("\"active_generation\":\"gen-v15\""));
        assert!(state_root.join("handoff.prepared").exists());
        assert!(state_root.join("handoff.committed").exists());
        assert!(state_root.join("state/release-pointer").exists());

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(plan_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
        fs::remove_dir_all(state_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn upgrade_apply_handoff_runs_runtime_binary_version_smoke() {
        let project = project_with_upgrade_apply_allowed("upgrade-apply-runtime-project");
        let (plan_root, plan_path) =
            upgrade_apply_plan_fixture("upgrade-apply-runtime", "plan-runtime");
        let lock_root = test_temp_dir("upgrade-apply-runtime-lock");
        let state_root = test_temp_dir("upgrade-apply-runtime-state");
        let (runtime_root, runtime_binary) =
            executable_runtime_binary_fixture("upgrade-apply-runtime-binary");

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-runtime",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--state-store",
            state_root.to_str().unwrap(),
            "--runtime-binary",
            runtime_binary.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"status\":\"committed\""));
        assert!(stdout.contains("\"mutation_executed\":true"));
        assert!(stdout.contains("\"runtime.binary:version_smoke\""));
        assert!(stdout.contains("\"runtime.binary.exit_code:0\""));
        assert!(state_root.join("state/release-pointer").exists());

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(plan_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
        fs::remove_dir_all(state_root).unwrap();
        fs::remove_dir_all(runtime_root).unwrap();
    }

    #[test]
    fn upgrade_apply_handoff_missing_runtime_binary_blocks_before_pointer_mutation() {
        let project = project_with_upgrade_apply_allowed("upgrade-apply-missing-runtime-project");
        let (plan_root, plan_path) =
            upgrade_apply_plan_fixture("upgrade-apply-missing-runtime", "plan-missing-runtime");
        let lock_root = test_temp_dir("upgrade-apply-missing-runtime-lock");
        let state_root = test_temp_dir("upgrade-apply-missing-runtime-state");
        let missing_binary = test_temp_dir("upgrade-apply-missing-runtime-bin").join("eva-runtime");

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-missing-runtime",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--state-store",
            state_root.to_str().unwrap(),
            "--runtime-binary",
            missing_binary.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stdout.contains("\"runtime_binary\":{"));
        assert!(stdout.contains("\"status\":\"unavailable\""));
        assert!(stdout.contains("\"runtime.binary:missing\""));
        assert!(stdout.contains("\"rollback_plan\":{"));
        assert!(!state_root.join("state/release-pointer").exists());
        assert!(state_root.join("handoff.prepared").exists());

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(plan_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn upgrade_apply_handoff_health_failure_emits_rollback_without_pointer_mutation() {
        let project = project_with_upgrade_apply_allowed("upgrade-apply-health-project");
        let (plan_root, plan_path) =
            upgrade_apply_plan_fixture("upgrade-apply-health", "plan-health");
        let lock_root = test_temp_dir("upgrade-apply-health-lock");
        let state_root = test_temp_dir("upgrade-apply-health-state");

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-health",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--state-store",
            state_root.to_str().unwrap(),
            "--health",
            "failed",
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stdout.contains("\"rollback_plan\":{"));
        assert!(!state_root.join("state/release-pointer").exists());
        assert!(state_root.join("handoff.prepared").exists());

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(plan_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn upgrade_apply_handoff_default_policy_denies_before_state_mutation() {
        let root = workspace_root();
        let (plan_root, plan_path) =
            upgrade_apply_plan_fixture("upgrade-apply-policy", "plan-policy");
        let lock_root = test_temp_dir("upgrade-apply-policy-lock");
        let state_root = test_temp_dir("upgrade-apply-policy-state");

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-policy",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--state-store",
            state_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_POLICY);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"upgrade.apply\""));
        assert!(stderr.contains("\"kind\":\"permission_denied\""));
        assert!(!state_root.join("state/release-pointer").exists());

        fs::remove_dir_all(plan_root).unwrap();
        fs::remove_dir_all(lock_root).ok();
        fs::remove_dir_all(state_root).ok();
    }

    #[test]
    fn upgrade_apply_reports_lock_conflict() {
        let root = workspace_root();
        let lock_root = test_temp_dir("upgrade-apply-conflict");
        let plan_path = lock_root.join("upgrade.plan");
        fs::create_dir_all(&lock_root).unwrap();
        fs::write(
            &plan_path,
            "plan_id=plan-conflict\nfrom_generation=gen-v14\nto_generation=gen-v15\nfrom_release=1.4.0\nto_release=1.5.1\n",
        )
        .unwrap();

        let first = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-conflict",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(first.0, EXIT_OK, "{}", first.2);

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-conflict",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"conflict\""));
        assert!(stderr.contains("\"command\":\"upgrade.apply\""));
        assert!(stderr.contains("\"span_id\":\"cli.upgrade.apply\""));

        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    fn upgrade_apply_rejects_mismatched_confirmation() {
        let root = workspace_root();
        let lock_root = test_temp_dir("upgrade-apply-confirm");
        let plan_path = lock_root.join("upgrade.plan");
        fs::create_dir_all(&lock_root).unwrap();
        fs::write(
            &plan_path,
            "plan_id=plan-confirm\nfrom_generation=gen-v14\nto_generation=gen-v15\nfrom_release=1.4.0\nto_release=1.5.1\n",
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "wrong-plan",
            "--lock-store",
            lock_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_POLICY);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"permission_denied\""));
        assert!(!lock_root.join("plan-confirm.lock").exists());

        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    fn upgrade_apply_plan_allows_utf8_bom() {
        let plan = upgrade_cmd::parse_upgrade_apply_plan(
            "\u{feff}plan_id=plan-bom\nfrom_generation=gen-v14\nto_generation=gen-v15\nfrom_release=1.4.0\nto_release=1.5.1\n",
        )
        .unwrap();

        assert_eq!(plan.plan_id, "plan-bom");
        assert_eq!(plan.lock_id(), "upgrade-apply-plan-bom");
    }

    #[test]
    fn restore_apply_dry_run_validates_durable_backup() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("restore-apply-ok");
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/apply-ok", b"ok".as_slice())
            .unwrap();
        let pre_restore = store
            .put_bytes("backup/pre-apply-ok", b"before".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-ok\nbackup_artifact_id=apply-ok\nbackup_digest={}\npre_restore_backup_artifact_id=pre-apply-ok\npre_restore_backup_digest={}\n",
                artifact.digest, pre_restore.digest
            ),
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-ok",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"restore.apply\""));
        assert!(stdout.contains("\"status\":\"dry_run_validated\""));
        assert!(stdout.contains("\"apply_allowed\":false"));
        assert!(stdout.contains("\"backup_artifact_key\":\"backup/apply-ok\""));
        assert!(stdout.contains("\"pre_restore_backup_artifact_key\":\"backup/pre-apply-ok\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn restore_apply_plan_allows_utf8_bom() {
        let plan = restore_cmd::parse_restore_apply_plan(
            "\u{feff}plan_id=plan-bom\nbackup_artifact_id=apply-bom\nbackup_digest=sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df\npre_restore_backup_artifact_id=pre-apply-bom\npre_restore_backup_digest=sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df\n",
        )
        .unwrap();

        assert_eq!(plan.plan_id, "plan-bom");
        assert_eq!(plan.backup_artifact_key(), "backup/apply-bom");
        assert_eq!(
            plan.pre_restore_backup
                .as_ref()
                .unwrap()
                .backup_artifact_key(),
            "backup/pre-apply-bom"
        );
    }

    #[test]
    fn restore_apply_dry_run_requires_pre_restore_evidence() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("restore-apply-no-pre");
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/apply-no-pre", b"ok".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-no-pre\nbackup_artifact_id=apply-no-pre\nbackup_digest={}\n",
                artifact.digest
            ),
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-no-pre",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"invalid_argument\""));
        assert!(stderr.contains("pre_restore_backup_artifact_id"));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn restore_apply_dry_run_reports_missing_backup() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("restore-apply-missing");
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let pre_restore = store
            .put_bytes("backup/pre-missing", b"before".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-missing\nbackup_artifact_id=missing\nbackup_digest=sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df\npre_restore_backup_artifact_id=pre-missing\npre_restore_backup_digest={}\n",
                pre_restore.digest
            ),
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-missing",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"not_found\""));
        assert!(stderr.contains("\"artifact_key\",\"value\":\"backup/missing\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn restore_apply_dry_run_reports_missing_pre_restore_backup() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("restore-apply-missing-pre");
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/apply-missing-pre", b"ok".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-missing-pre\nbackup_artifact_id=apply-missing-pre\nbackup_digest={}\npre_restore_backup_artifact_id=pre-missing\npre_restore_backup_digest=sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df\n",
                artifact.digest
            ),
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-missing-pre",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"not_found\""));
        assert!(stderr.contains("\"artifact_key\",\"value\":\"backup/pre-missing\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn restore_apply_dry_run_reports_digest_mismatch() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("restore-apply-mismatch");
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/apply-mismatch", b"ok".as_slice())
            .unwrap();
        let pre_restore = store
            .put_bytes("backup/pre-mismatch", b"before".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-mismatch\nbackup_artifact_id=apply-mismatch\nbackup_digest={}\npre_restore_backup_artifact_id=pre-mismatch\npre_restore_backup_digest={}\n",
                artifact.digest, pre_restore.digest
            ),
        )
        .unwrap();
        fs::write(
            artifact_root
                .join("objects")
                .join("backup")
                .join("apply-mismatch.artifact"),
            b"no",
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-mismatch",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"conflict\""));
        assert!(stderr.contains("\"expected_digest\""));
        assert!(stderr.contains("\"actual_digest\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn v14_backup_lifecycle_can_use_filesystem_artifact_store() {
        let root = workspace_root();
        let root = root.to_str().unwrap();
        let artifact_root = test_temp_dir("artifacts");
        let artifact_root_str = artifact_root.to_str().unwrap();

        let (backup_exit, backup_stdout, backup_stderr) = run_cli(&[
            "backup",
            "create",
            "--project",
            root,
            "--artifact-id",
            "durable-cli",
            "--artifact-store",
            artifact_root_str,
            "--output",
            "json",
        ]);
        assert_eq!(backup_exit, EXIT_OK, "{backup_stderr}");
        assert!(backup_stdout.contains("\"artifact_store\":{\"kind\":\"filesystem\""));
        assert!(artifact_root
            .join("objects")
            .join("backup")
            .join("durable-cli.artifact")
            .is_file());
        assert!(artifact_root
            .join("metadata")
            .join("backup")
            .join("durable-cli.metadata")
            .is_file());

        let (snapshot_exit, snapshot_stdout, snapshot_stderr) = run_cli(&[
            "snapshot",
            "create",
            "--project",
            root,
            "--snapshot-id",
            "durable-snap",
            "--artifact-store",
            artifact_root_str,
            "--output",
            "json",
        ]);
        assert_eq!(snapshot_exit, EXIT_OK, "{snapshot_stderr}");
        assert!(snapshot_stdout.contains("\"artifact_store\":{\"kind\":\"filesystem\""));

        let (restore_exit, restore_stdout, restore_stderr) = run_cli(&[
            "restore",
            "plan",
            "--project",
            root,
            "--snapshot-id",
            "durable-restore",
            "--artifact-store",
            artifact_root_str,
            "--output",
            "json",
        ]);
        assert_eq!(restore_exit, EXIT_OK, "{restore_stderr}");
        assert!(restore_stdout.contains("\"command\":\"restore.plan\""));
        assert!(restore_stdout.contains("\"artifact_store\":{\"kind\":\"filesystem\""));
        assert!(restore_stdout.contains("\"apply_allowed\":false"));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    fn v15_release_hardening_commands_report_json() {
        let root = workspace_root();
        let root = root.to_str().unwrap();
        let commands = [
            vec!["release", "check", "--project", root, "--output", "json"],
            vec!["release", "security", "--project", root, "--output", "json"],
            vec!["release", "perf", "--project", root, "--output", "json"],
            vec![
                "release",
                "migration",
                "--project",
                root,
                "--output",
                "json",
            ],
        ];

        for command in commands {
            let (exit_code, stdout, stderr) = run_cli(&command);
            assert_eq!(exit_code, EXIT_OK, "command={command:?} stderr={stderr}");
            assert!(stdout.contains("\"ok\":true"), "{stdout}");
        }

        let (_exit_code, check_stdout, _stderr) =
            run_cli(&["release", "check", "--project", root, "--output", "json"]);
        assert!(check_stdout.contains("\"command\":\"release.check\""));
        assert!(check_stdout.contains("\"status\":\"ready\""));
        assert!(check_stdout.contains("\"cross_platform\""));
        assert!(check_stdout.contains("\"blocking_gates\":0"));

        let (_exit_code, security_stdout, _stderr) =
            run_cli(&["release", "security", "--project", root, "--output", "json"]);
        assert!(security_stdout.contains("\"policy\""));
        assert!(security_stdout.contains("\"hardware\""));
        assert!(security_stdout.contains("\"blocking_findings\":0"));

        let (_exit_code, perf_stdout, _stderr) =
            run_cli(&["release", "perf", "--project", root, "--output", "json"]);
        assert!(perf_stdout.contains("\"status\":\"within_budget\""));
        assert!(perf_stdout.contains("\"component\":\"eventbus.publish\""));

        let (_exit_code, migration_stdout, _stderr) = run_cli(&[
            "release",
            "migration",
            "--project",
            root,
            "--output",
            "json",
        ]);
        assert!(migration_stdout.contains("\"from_version\":\"1.5.1\""));
        assert!(migration_stdout.contains("\"to_version\":\"1.7.4-alpha\""));
        assert!(migration_stdout.contains("\"breaking_changes\":[]"));
    }
}
