//! CLI command parsing, output envelopes, and process exit mapping.

use crate::doctor::{doctor_project, CheckStatus, DoctorReport};
use crate::inspect::{inspect_project, InspectReport};
use eva_adapter::{AdapterInvocation, AdapterInvokeReport, AdapterProbeReport, AdapterRuntime};
use eva_backup::{
    BackupEntry, BackupPlan, BackupResult, BackupScope, BackupService, MigrationPackageManifest,
    MigrationPackageService, MigrationPreflight, ReleaseSnapshot, ReleaseSnapshotService,
    RestorePlan, SnapshotRole,
};
use eva_config::{load_project_config, schema_paths, AdapterTransport, ProjectConfig};
use eva_core::{
    AdapterId, AgentId, CapabilityName, ErrorKind, EvaError, GenerationId, InvokeStatus, RequestId,
};
use eva_discovery::{DiscoveryCandidate, DiscoveryService};
use eva_hardware::{discover_project_devices, DeviceCandidate, HardwareDiscoveryReport};
use eva_lifecycle::{
    DrainCoordinator, DrainPlan, GenerationState, InMemorySupervisor, RollbackCoordinator,
    RollbackPlan, RuntimeGeneration, SupervisorReport,
};
use eva_mcp::{InMemoryMcpClient, McpAllowlist, McpProbeReport};
use eva_memory::{
    BuiltContext, ContextBudget, ContextBuilder, ContextRequest, InMemoryKnowledgeService,
    InMemoryMemoryService, KnowledgeId, KnowledgeItem, KnowledgeSource, MemoryWrite,
};
use eva_observability::{SpanId, TraceFields};
use eva_runtime::{BasicRunOptions, BasicRunReport, RuntimeBuilder, TaskLogEntry};
use eva_storage::InMemoryArtifactStore;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
const RELEASE_LABEL: &str = "V1.4 backup and lifecycle planning";
const RELEASE_RUNTIME_MODE: &str =
    "in_memory_v1.0 + external_capability_v1.1 + context_v1.2 + hardware_v1.3 + lifecycle_v1.4";
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
    "hardware list/probe/bind",
    "backup create",
    "snapshot create",
    "restore plan",
    "upgrade check",
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
        Command::Version(options) => {
            let trace = trace_for("cli.version");
            write_version(stdout, options.output, &trace)?;
            Ok(EXIT_OK)
        }
        Command::Doctor(options) => {
            let trace = trace_for("cli.doctor");
            let report = doctor_project(&options.project_root);
            let exit_code = if report.has_errors() {
                EXIT_CONFIG
            } else {
                EXIT_OK
            };
            write_doctor(stdout, options.output, exit_code, &report, &trace)?;
            Ok(exit_code)
        }
        Command::ConfigValidate(options) => {
            let trace = trace_for("cli.config.validate");
            match load_project_config(&options.project_root) {
                Ok(project) => {
                    let report = ValidationReport::from_project(&project);
                    write_validation(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = EXIT_CONFIG;
                    write_error(
                        stderr,
                        options.output,
                        "config.validate",
                        exit_code,
                        &error,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        Command::Inspect(options) => {
            let trace = trace_for("cli.inspect");
            match load_project_config(&options.project_root)
                .and_then(|project| inspect_project(&project))
            {
                Ok(report) => {
                    write_inspect(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(stderr, options.output, "inspect", exit_code, &error, &trace)?;
                    Ok(exit_code)
                }
            }
        }
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
        Command::Task(command) => execute_task(command, stdout, stderr),
        Command::Adapter(command) => execute_adapter(command, stdout, stderr),
        Command::Mcp(command) => execute_mcp(command, stdout, stderr),
        Command::Skill(command) => execute_skill(command, stdout, stderr),
        Command::Discovery(command) => execute_discovery(command, stdout, stderr),
        Command::Memory(command) => execute_memory(command, stdout, stderr),
        Command::Hardware(command) => execute_hardware(command, stdout, stderr),
        Command::Backup(command) => execute_backup(command, stdout, stderr),
        Command::Snapshot(command) => execute_snapshot(command, stdout, stderr),
        Command::Restore(command) => execute_restore(command, stdout, stderr),
        Command::Upgrade(command) => execute_upgrade(command, stdout, stderr),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Version(CommonOptions),
    Doctor(CommonOptions),
    ConfigValidate(CommonOptions),
    Inspect(CommonOptions),
    Run(RunOptions),
    Task(TaskCommand),
    Adapter(AdapterCommand),
    Mcp(McpCommand),
    Skill(SkillCommand),
    Discovery(DiscoveryCommand),
    Memory(MemoryCommand),
    Hardware(HardwareCommand),
    Backup(BackupCommand),
    Snapshot(SnapshotCommand),
    Restore(RestoreCommand),
    Upgrade(UpgradeCommand),
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
    timeout_ms: Option<u64>,
    cancel_requested: bool,
    retry_attempts: usize,
    replay_dead_letters: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskCommand {
    Status(TaskOptions),
    Logs(TaskOptions),
    Cancel(TaskOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AdapterCommand {
    List(CommonOptions),
    Probe(AdapterProbeOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum McpCommand {
    List(CommonOptions),
    Probe(McpProbeOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SkillCommand {
    List(CommonOptions),
    Run(SkillRunOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscoveryCommand {
    Scan(CommonOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemoryCommand {
    Context(MemoryContextOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HardwareCommand {
    List(CommonOptions),
    Probe(HardwareProbeOptions),
    Bind(HardwareBindOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BackupCommand {
    Create(BackupCreateOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotCommand {
    Create(SnapshotCreateOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RestoreCommand {
    Plan(RestorePlanOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpgradeCommand {
    Check(UpgradeCheckOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskOptions {
    common: CommonOptions,
    task_id: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdapterProbeOptions {
    common: CommonOptions,
    adapter_id: Option<String>,
    capability: Option<String>,
    provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpProbeOptions {
    common: CommonOptions,
    adapter_id: String,
    tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillRunOptions {
    common: CommonOptions,
    adapter_id: Option<String>,
    skill_id: Option<String>,
    capability: Option<String>,
    input: String,
    request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryContextOptions {
    common: CommonOptions,
    agent_id: String,
    query: String,
    request_id: String,
    private_limit: usize,
    global_limit: usize,
    knowledge_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HardwareProbeOptions {
    common: CommonOptions,
    adapter_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HardwareBindOptions {
    common: CommonOptions,
    adapter_id: String,
    request_id: String,
    apply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackupCreateOptions {
    common: CommonOptions,
    artifact_id: String,
    request_id: String,
    project_id: String,
    reason: String,
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotCreateOptions {
    common: CommonOptions,
    snapshot_id: String,
    request_id: String,
    release_ref: String,
    role: SnapshotRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestorePlanOptions {
    common: CommonOptions,
    snapshot_id: String,
    request_id: String,
    release_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpgradeCheckOptions {
    common: CommonOptions,
    from_generation: String,
    to_generation: String,
    from_release: String,
    to_release: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationReport {
    project_root: String,
    eva_config_path: String,
    environment: String,
    hot_reload: bool,
    agents_total: usize,
    agents_enabled: usize,
    adapters_total: usize,
    adapters_enabled: usize,
    capabilities_total: usize,
    capabilities_enabled: usize,
    policies_total: usize,
    routes_total: usize,
    schema_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskSnapshot {
    task_id: String,
    status: String,
    attempts: usize,
    retry_max_attempts: usize,
    cancel_requested: bool,
    cancel_accepted: bool,
    cancel_reason: Option<String>,
    error_kind: Option<String>,
    error_message: Option<String>,
    logs: Vec<TaskLogSnapshot>,
    dead_letters: Vec<TaskDeadLetterSnapshot>,
    replayed_events: Vec<TaskReplaySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskLogSnapshot {
    sequence: u64,
    level: String,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDeadLetterSnapshot {
    event_id: String,
    topic: String,
    reason_kind: String,
    reason: String,
    replay_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskReplaySnapshot {
    event_id: String,
    sequence: u64,
    topic: String,
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
        "doctor" => Ok(Command::Doctor(parse_common_options(&args[1..])?)),
        "config" => parse_config_command(&args[1..]),
        "inspect" => Ok(Command::Inspect(parse_inspect_options(&args[1..])?)),
        "run" => Ok(Command::Run(parse_run_options(&args[1..])?)),
        "task" => parse_task_command(&args[1..]),
        "adapter" => parse_adapter_command(&args[1..]),
        "mcp" => parse_mcp_command(&args[1..]),
        "skill" => parse_skill_command(&args[1..]),
        "discovery" => parse_discovery_command(&args[1..]),
        "memory" => parse_memory_command(&args[1..]),
        "hardware" => parse_hardware_command(&args[1..]),
        "backup" => parse_backup_command(&args[1..]),
        "snapshot" => parse_snapshot_command(&args[1..]),
        "restore" => parse_restore_command(&args[1..]),
        "upgrade" => parse_upgrade_command(&args[1..]),
        unknown => Err(EvaError::unsupported("unknown command").with_context("command", unknown)),
    }
}

fn parse_run_options(args: &[String]) -> Result<RunOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut example = None;
    let mut task_id = None;
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
        timeout_ms,
        cancel_requested,
        retry_attempts,
        replay_dead_letters,
    })
}

fn parse_task_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing task subcommand"))?;
    let options = parse_task_options(rest)?;
    match subcommand.as_str() {
        "status" => Ok(Command::Task(TaskCommand::Status(options))),
        "logs" => Ok(Command::Task(TaskCommand::Logs(options))),
        "cancel" => Ok(Command::Task(TaskCommand::Cancel(options))),
        value => {
            Err(EvaError::unsupported("unknown task subcommand").with_context("subcommand", value))
        }
    }
}

fn parse_task_options(args: &[String]) -> Result<TaskOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut task_id = None;
    let mut reason = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--task" | "--task-id" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for task option"))?;
                task_id = Some(value.clone());
            }
            "--reason" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for reason option"))?;
                reason = Some(value.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(TaskOptions {
        common: parse_common_options(&passthrough)?,
        task_id,
        reason,
    })
}

fn parse_adapter_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing adapter subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(Command::Adapter(AdapterCommand::List(
            parse_common_options(rest)?,
        ))),
        "probe" => Ok(Command::Adapter(AdapterCommand::Probe(
            parse_adapter_probe_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown adapter subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_adapter_probe_options(args: &[String]) -> Result<AdapterProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut capability = None;
    let mut provider = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            "--capability" => {
                index += 1;
                capability = Some(required_option(args, index, "capability option")?.clone());
            }
            "--provider" => {
                index += 1;
                provider = Some(required_option(args, index, "provider option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(AdapterProbeOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
        capability,
        provider,
    })
}

fn parse_mcp_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing mcp subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(Command::Mcp(McpCommand::List(parse_common_options(rest)?))),
        "probe" => Ok(Command::Mcp(McpCommand::Probe(parse_mcp_probe_options(
            rest,
        )?))),
        value => {
            Err(EvaError::unsupported("unknown mcp subcommand").with_context("subcommand", value))
        }
    }
}

fn parse_mcp_probe_options(args: &[String]) -> Result<McpProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut tool = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            "--tool" => {
                index += 1;
                tool = Some(required_option(args, index, "tool option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(McpProbeOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id: adapter_id.unwrap_or_else(|| "github-mcp".to_owned()),
        tool,
    })
}

fn parse_skill_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing skill subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(Command::Skill(SkillCommand::List(parse_common_options(
            rest,
        )?))),
        "run" => Ok(Command::Skill(SkillCommand::Run(parse_skill_run_options(
            rest,
        )?))),
        value => {
            Err(EvaError::unsupported("unknown skill subcommand").with_context("subcommand", value))
        }
    }
}

fn parse_skill_run_options(args: &[String]) -> Result<SkillRunOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut skill_id = None;
    let mut capability = None;
    let mut input = "{}".to_owned();
    let mut request_id = "req-skill-1".to_owned();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            "--skill" | "--skill-id" => {
                index += 1;
                skill_id = Some(required_option(args, index, "skill option")?.clone());
            }
            "--capability" => {
                index += 1;
                capability = Some(required_option(args, index, "capability option")?.clone());
            }
            "--input" => {
                index += 1;
                input = required_option(args, index, "input option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    RequestId::parse(&request_id)?;
    Ok(SkillRunOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
        skill_id,
        capability,
        input,
        request_id,
    })
}

fn parse_discovery_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing discovery subcommand"))?;
    match subcommand.as_str() {
        "scan" => Ok(Command::Discovery(DiscoveryCommand::Scan(
            parse_common_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown discovery subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_memory_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing memory subcommand"))?;
    match subcommand.as_str() {
        "context" => Ok(Command::Memory(MemoryCommand::Context(
            parse_memory_context_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown memory subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_memory_context_options(args: &[String]) -> Result<MemoryContextOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut agent_id = "root-agent".to_owned();
    let mut query = "memory".to_owned();
    let mut request_id = "req-memory-1".to_owned();
    let mut private_limit = 8;
    let mut global_limit = 8;
    let mut knowledge_limit = 8;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--agent" | "--agent-id" => {
                index += 1;
                agent_id = required_option(args, index, "agent option")?.clone();
            }
            "--query" => {
                index += 1;
                query = required_option(args, index, "query option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--private-limit" => {
                index += 1;
                private_limit = parse_usize_option(
                    "private_limit",
                    required_option(args, index, "private limit option")?,
                )?;
            }
            "--global-limit" => {
                index += 1;
                global_limit = parse_usize_option(
                    "global_limit",
                    required_option(args, index, "global limit option")?,
                )?;
            }
            "--knowledge-limit" => {
                index += 1;
                knowledge_limit = parse_usize_option(
                    "knowledge_limit",
                    required_option(args, index, "knowledge limit option")?,
                )?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    AgentId::parse(&agent_id)?;
    RequestId::parse(&request_id)?;
    Ok(MemoryContextOptions {
        common: parse_common_options(&passthrough)?,
        agent_id,
        query,
        request_id,
        private_limit,
        global_limit,
        knowledge_limit,
    })
}

fn parse_hardware_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing hardware subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(Command::Hardware(HardwareCommand::List(
            parse_common_options(rest)?,
        ))),
        "probe" => Ok(Command::Hardware(HardwareCommand::Probe(
            parse_hardware_probe_options(rest)?,
        ))),
        "bind" => Ok(Command::Hardware(HardwareCommand::Bind(
            parse_hardware_bind_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown hardware subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_hardware_probe_options(args: &[String]) -> Result<HardwareProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    if let Some(adapter_id) = &adapter_id {
        AdapterId::parse(adapter_id)?;
    }
    Ok(HardwareProbeOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
    })
}

fn parse_hardware_bind_options(args: &[String]) -> Result<HardwareBindOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = "scale-main".to_owned();
    let mut request_id = "req-hardware-1".to_owned();
    let mut apply = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = required_option(args, index, "adapter option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--apply" => apply = true,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    AdapterId::parse(&adapter_id)?;
    RequestId::parse(&request_id)?;
    Ok(HardwareBindOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
        request_id,
        apply,
    })
}

fn parse_backup_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing backup subcommand"))?;
    match subcommand.as_str() {
        "create" => Ok(Command::Backup(BackupCommand::Create(
            parse_backup_create_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown backup subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_backup_create_options(args: &[String]) -> Result<BackupCreateOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut artifact_id = "backup-v14".to_owned();
    let mut request_id = "req-backup-1".to_owned();
    let mut project_id = "eva-cli".to_owned();
    let mut reason = "pre-upgrade safety checkpoint".to_owned();
    let mut dry_run = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--artifact" | "--artifact-id" => {
                index += 1;
                artifact_id = required_option(args, index, "artifact option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--project-id" => {
                index += 1;
                project_id = required_option(args, index, "project id option")?.clone();
            }
            "--reason" => {
                index += 1;
                reason = required_option(args, index, "reason option")?.clone();
            }
            "--dry-run" => dry_run = true,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    RequestId::parse(&request_id)?;
    Ok(BackupCreateOptions {
        common: parse_common_options(&passthrough)?,
        artifact_id,
        request_id,
        project_id,
        reason,
        dry_run,
    })
}

fn parse_snapshot_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing snapshot subcommand"))?;
    match subcommand.as_str() {
        "create" => Ok(Command::Snapshot(SnapshotCommand::Create(
            parse_snapshot_create_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown snapshot subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_snapshot_create_options(args: &[String]) -> Result<SnapshotCreateOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut snapshot_id = "snapshot-v14".to_owned();
    let mut request_id = "req-snapshot-1".to_owned();
    let mut release_ref = "1.4.0".to_owned();
    let mut role = SnapshotRole::PreRelease;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--snapshot" | "--snapshot-id" => {
                index += 1;
                snapshot_id = required_option(args, index, "snapshot option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--release" | "--release-ref" => {
                index += 1;
                release_ref = required_option(args, index, "release option")?.clone();
            }
            "--role" => {
                index += 1;
                role = parse_snapshot_role(required_option(args, index, "role option")?)?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    RequestId::parse(&request_id)?;
    Ok(SnapshotCreateOptions {
        common: parse_common_options(&passthrough)?,
        snapshot_id,
        request_id,
        release_ref,
        role,
    })
}

fn parse_restore_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing restore subcommand"))?;
    match subcommand.as_str() {
        "plan" => Ok(Command::Restore(RestoreCommand::Plan(
            parse_restore_plan_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown restore subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_restore_plan_options(args: &[String]) -> Result<RestorePlanOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut snapshot_id = "snapshot-v14".to_owned();
    let mut request_id = "req-restore-1".to_owned();
    let mut release_ref = "1.4.0".to_owned();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--snapshot" | "--snapshot-id" => {
                index += 1;
                snapshot_id = required_option(args, index, "snapshot option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--release" | "--release-ref" => {
                index += 1;
                release_ref = required_option(args, index, "release option")?.clone();
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    RequestId::parse(&request_id)?;
    Ok(RestorePlanOptions {
        common: parse_common_options(&passthrough)?,
        snapshot_id,
        request_id,
        release_ref,
    })
}

fn parse_upgrade_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing upgrade subcommand"))?;
    match subcommand.as_str() {
        "check" => Ok(Command::Upgrade(UpgradeCommand::Check(
            parse_upgrade_check_options(rest)?,
        ))),
        value => {
            Err(EvaError::unsupported("unknown upgrade subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_upgrade_check_options(args: &[String]) -> Result<UpgradeCheckOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut from_generation = "gen-v13".to_owned();
    let mut to_generation = "gen-v14".to_owned();
    let mut from_release = "1.3.0".to_owned();
    let mut to_release = "1.4.0".to_owned();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from-generation" => {
                index += 1;
                from_generation = required_option(args, index, "from generation option")?.clone();
            }
            "--to-generation" => {
                index += 1;
                to_generation = required_option(args, index, "to generation option")?.clone();
            }
            "--from-release" => {
                index += 1;
                from_release = required_option(args, index, "from release option")?.clone();
            }
            "--to-release" => {
                index += 1;
                to_release = required_option(args, index, "to release option")?.clone();
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    GenerationId::parse(&from_generation)?;
    GenerationId::parse(&to_generation)?;
    Ok(UpgradeCheckOptions {
        common: parse_common_options(&passthrough)?,
        from_generation,
        to_generation,
        from_release,
        to_release,
    })
}

fn parse_snapshot_role(value: &str) -> Result<SnapshotRole, EvaError> {
    match value {
        "pre_release" | "pre-release" | "pre" => Ok(SnapshotRole::PreRelease),
        "post_release" | "post-release" | "post" => Ok(SnapshotRole::PostRelease),
        _ => Err(EvaError::unsupported("unknown snapshot role").with_context("role", value)),
    }
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
                    write_task_snapshot(&options.common.project_root, &report)?;
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

fn execute_task<W, E>(command: TaskCommand, stdout: &mut W, stderr: &mut E) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        TaskCommand::Status(options) => {
            let trace = trace_for("cli.task.status");
            match read_task_snapshot(&options.common.project_root, options.task_id.as_deref()) {
                Ok(snapshot) => {
                    write_task_status(stdout, options.common.output, &snapshot, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(
                        stderr,
                        options.common.output,
                        "task.status",
                        exit_code,
                        &error,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        TaskCommand::Logs(options) => {
            let trace = trace_for("cli.task.logs");
            match read_task_snapshot(&options.common.project_root, options.task_id.as_deref()) {
                Ok(snapshot) => {
                    write_task_logs(stdout, options.common.output, &snapshot, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(
                        stderr,
                        options.common.output,
                        "task.logs",
                        exit_code,
                        &error,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        TaskCommand::Cancel(options) => {
            let trace = trace_for("cli.task.cancel");
            match cancel_task_snapshot(
                &options.common.project_root,
                options.task_id.as_deref(),
                options
                    .reason
                    .as_deref()
                    .unwrap_or("cancel requested by CLI"),
            ) {
                Ok(snapshot) => {
                    write_task_cancel(stdout, options.common.output, &snapshot, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(
                        stderr,
                        options.common.output,
                        "task.cancel",
                        exit_code,
                        &error,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
    }
}

fn execute_adapter<W, E>(
    command: AdapterCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        AdapterCommand::List(options) => {
            let trace = trace_for("cli.adapter.list");
            match load_project_config(&options.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
            {
                Ok(runtime) => {
                    write_adapter_list(stdout, options.output, &runtime, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "adapter.list", &error, &trace)
                }
            }
        }
        AdapterCommand::Probe(options) => {
            let trace = trace_for("cli.adapter.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
                .and_then(|runtime| probe_adapter_runtime(&runtime, &options))
            {
                Ok(report) => {
                    write_adapter_probe(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "adapter.probe",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn execute_mcp<W, E>(command: McpCommand, stdout: &mut W, stderr: &mut E) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        McpCommand::List(options) => {
            let trace = trace_for("cli.mcp.list");
            match load_project_config(&options.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
            {
                Ok(runtime) => {
                    write_mcp_list(stdout, options.output, &runtime, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "mcp.list", &error, &trace)
                }
            }
        }
        McpCommand::Probe(options) => {
            let trace = trace_for("cli.mcp.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
                .and_then(|runtime| probe_mcp_runtime(&runtime, &options))
            {
                Ok(report) => {
                    write_mcp_probe(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.common.output, "mcp.probe", &error, &trace)
                }
            }
        }
    }
}

fn execute_skill<W, E>(
    command: SkillCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        SkillCommand::List(options) => {
            let trace = trace_for("cli.skill.list");
            match load_project_config(&options.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
            {
                Ok(runtime) => {
                    write_skill_list(stdout, options.output, &runtime, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "skill.list", &error, &trace)
                }
            }
        }
        SkillCommand::Run(options) => {
            let trace = trace_for("cli.skill.run");
            match load_project_config(&options.common.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
                .and_then(|runtime| run_skill_runtime(&runtime, &options))
            {
                Ok(report) => {
                    write_adapter_invoke(
                        stdout,
                        options.common.output,
                        "skill.run",
                        &report,
                        &trace,
                    )?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.common.output, "skill.run", &error, &trace)
                }
            }
        }
    }
}

fn execute_discovery<W, E>(
    command: DiscoveryCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        DiscoveryCommand::Scan(options) => {
            let trace = trace_for("cli.discovery.scan");
            match load_project_config(&options.project_root) {
                Ok(project) => {
                    let mut service = DiscoveryService::new();
                    let report = service.scan_project(&project);
                    write_discovery_scan(stdout, options.output, &report.candidates, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "discovery.scan", &error, &trace)
                }
            }
        }
    }
}

fn execute_memory<W, E>(
    command: MemoryCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        MemoryCommand::Context(options) => {
            let trace = trace_for("cli.memory.context");
            match load_project_config(&options.common.project_root)
                .and_then(|project| build_memory_context(&project, &options))
            {
                Ok(context) => {
                    write_memory_context(stdout, options.common.output, &context, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "memory.context",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn execute_hardware<W, E>(
    command: HardwareCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        HardwareCommand::List(options) => {
            let trace = trace_for("cli.hardware.list");
            match load_project_config(&options.project_root)
                .and_then(|project| discover_project_devices(&project))
            {
                Ok(report) => {
                    write_hardware_list(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "hardware.list", &error, &trace)
                }
            }
        }
        HardwareCommand::Probe(options) => {
            let trace = trace_for("cli.hardware.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| discover_project_devices(&project))
                .and_then(|report| probe_hardware_candidates(report, options.adapter_id.as_deref()))
            {
                Ok(candidates) => {
                    write_hardware_probe(stdout, options.common.output, &candidates, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "hardware.probe",
                    &error,
                    &trace,
                ),
            }
        }
        HardwareCommand::Bind(options) => {
            let trace = trace_for("cli.hardware.bind");
            match load_project_config(&options.common.project_root)
                .and_then(|project| hardware_bind_plan(&project, &options))
            {
                Ok(plan) => {
                    write_hardware_bind(stdout, options.common.output, &plan, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "hardware.bind",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn execute_backup<W, E>(
    command: BackupCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        BackupCommand::Create(options) => {
            let trace = trace_for("cli.backup.create");
            match create_backup_result(&options) {
                Ok(result) => {
                    write_backup_create(stdout, options.common.output, &result, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "backup.create",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn execute_snapshot<W, E>(
    command: SnapshotCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        SnapshotCommand::Create(options) => {
            let trace = trace_for("cli.snapshot.create");
            match create_snapshot_result(&options) {
                Ok((backup, snapshot)) => {
                    write_snapshot_create(
                        stdout,
                        options.common.output,
                        &backup,
                        &snapshot,
                        &trace,
                    )?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "snapshot.create",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn execute_restore<W, E>(
    command: RestoreCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        RestoreCommand::Plan(options) => {
            let trace = trace_for("cli.restore.plan");
            match create_restore_plan(&options) {
                Ok((snapshot, plan)) => {
                    write_restore_plan(stdout, options.common.output, &snapshot, &plan, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "restore.plan",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn execute_upgrade<W, E>(
    command: UpgradeCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        UpgradeCommand::Check(options) => {
            let trace = trace_for("cli.upgrade.check");
            match create_upgrade_check(&options) {
                Ok(report) => {
                    write_upgrade_check(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "upgrade.check",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HardwareBindPlan {
    adapter_id: AdapterId,
    request_id: RequestId,
    status: String,
    apply: bool,
    device: Option<DeviceCandidate>,
    steps: Vec<String>,
    risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpgradeCheckReport {
    status: String,
    supervisor: SupervisorReport,
    drain: DrainPlan,
    rollback: RollbackPlan,
    migration: MigrationPreflight,
    steps: Vec<String>,
    risks: Vec<String>,
}

fn probe_hardware_candidates(
    report: HardwareDiscoveryReport,
    adapter_id: Option<&str>,
) -> Result<Vec<DeviceCandidate>, EvaError> {
    let mut candidates = report.candidates;
    if let Some(adapter_id) = adapter_id {
        candidates.retain(|candidate| candidate.identity.adapter_id.as_str() == adapter_id);
        if candidates.is_empty() {
            return Err(
                EvaError::not_found("hardware adapter candidate does not exist")
                    .with_context("adapter_id", adapter_id),
            );
        }
    }
    Ok(candidates)
}

fn hardware_bind_plan(
    project: &ProjectConfig,
    options: &HardwareBindOptions,
) -> Result<HardwareBindPlan, EvaError> {
    let adapter_id = AdapterId::parse(&options.adapter_id)?;
    let request_id = RequestId::parse(&options.request_id)?;
    let report = discover_project_devices(project)?;
    let device = report
        .candidates
        .into_iter()
        .find(|candidate| candidate.identity.adapter_id == adapter_id);
    let status = match &device {
        Some(candidate) if candidate.rejected_reason.is_none() && options.apply => "ready_to_apply",
        Some(candidate) if candidate.rejected_reason.is_none() => "planned",
        Some(_) => "blocked",
        None => "missing",
    }
    .to_owned();
    let mut risks = vec!["hardware binding is plan-first; no raw I/O is opened by CLI".to_owned()];
    if options.apply {
        risks.push(
            "--apply only validates the logical plan in V1.3; physical claims remain runtime-gated"
                .to_owned(),
        );
    }
    if let Some(candidate) = &device {
        if let Some(reason) = &candidate.rejected_reason {
            risks.push(reason.clone());
        }
    }
    Ok(HardwareBindPlan {
        adapter_id,
        request_id,
        status,
        apply: options.apply,
        device,
        steps: vec![
            "discover hardware manifest candidate".to_owned(),
            "verify adapter manifest and policy boundary".to_owned(),
            "create logical DeviceRegistry lease".to_owned(),
            "route invocation through AdapterRuntime hardware transport".to_owned(),
            "release logical lease and emit audit".to_owned(),
        ],
        risks,
    })
}

fn create_backup_result(options: &BackupCreateOptions) -> Result<BackupResult, EvaError> {
    let scope = BackupScope::new(
        options.project_id.clone(),
        vec![
            BackupEntry::new("config/eva.yaml", "runtime: in_memory_v1.0")?,
            BackupEntry::new("config/adapters/hardware/scale-main.yaml", "enabled: false")?,
            BackupEntry::new("state/release-pointer", options.project_id.as_bytes())?.redacted(),
        ],
    )?;
    let mut plan = BackupPlan::new(
        options.artifact_id.clone(),
        RequestId::parse(&options.request_id)?,
        GenerationId::parse("gen-v14")?,
        "cli",
        options.reason.clone(),
        scope,
    )?;
    if options.dry_run {
        plan = plan.dry_run();
    }
    let mut store = InMemoryArtifactStore::new();
    BackupService.create(plan, &mut store)
}

fn create_snapshot_result(
    options: &SnapshotCreateOptions,
) -> Result<(BackupResult, ReleaseSnapshot), EvaError> {
    let backup_options = BackupCreateOptions {
        common: options.common.clone(),
        artifact_id: format!("backup-for-{}", options.snapshot_id),
        request_id: options.request_id.clone(),
        project_id: "eva-cli".to_owned(),
        reason: "snapshot capture requires verified backup artifact".to_owned(),
        dry_run: false,
    };
    let backup = create_backup_result(&backup_options)?;
    let snapshot = ReleaseSnapshotService.create(
        options.snapshot_id.clone(),
        options.role,
        options.release_ref.clone(),
        RequestId::parse(&options.request_id)?,
        &backup.manifest,
        "healthy",
    )?;
    Ok((backup, snapshot))
}

fn create_restore_plan(
    options: &RestorePlanOptions,
) -> Result<(ReleaseSnapshot, RestorePlan), EvaError> {
    let snapshot_options = SnapshotCreateOptions {
        common: options.common.clone(),
        snapshot_id: options.snapshot_id.clone(),
        request_id: options.request_id.clone(),
        release_ref: options.release_ref.clone(),
        role: SnapshotRole::PreRelease,
    };
    let (_backup, snapshot) = create_snapshot_result(&snapshot_options)?;
    let plan = ReleaseSnapshotService.restore_plan(&snapshot);
    Ok((snapshot, plan))
}

fn create_upgrade_check(options: &UpgradeCheckOptions) -> Result<UpgradeCheckReport, EvaError> {
    let active = RuntimeGeneration::new(
        GenerationId::parse(&options.from_generation)?,
        options.from_release.clone(),
        GenerationState::Active,
    )?;
    let mut supervisor = InMemorySupervisor::new(active)?;
    let candidate_id = GenerationId::parse(&options.to_generation)?;
    supervisor.start_candidate(candidate_id.clone(), options.to_release.clone())?;
    let supervisor_report = supervisor.report();
    let migration_manifest = MigrationPackageManifest::new(
        "migration-v14",
        options.from_release.clone(),
        options.to_release.clone(),
        vec![
            "backup_manifest".to_owned(),
            "runtime_generation".to_owned(),
        ],
    )?;
    let migration =
        MigrationPackageService.verify_preflight(&migration_manifest, &options.from_release)?;
    let drain = DrainCoordinator.plan(GenerationId::parse(&options.from_generation)?, 0, 30_000)?;
    let rollback = RollbackCoordinator.plan_failed_handoff(
        candidate_id,
        GenerationId::parse(&options.from_generation)?,
        "candidate health or migration preflight failure",
        None,
    )?;
    Ok(UpgradeCheckReport {
        status: if migration.status == "blocked" {
            "blocked".to_owned()
        } else {
            "ready".to_owned()
        },
        supervisor: supervisor_report,
        drain,
        rollback,
        migration,
        steps: vec![
            "create pre-release backup".to_owned(),
            "capture release snapshot".to_owned(),
            "start candidate runtime generation".to_owned(),
            "verify migration package preflight".to_owned(),
            "plan drain and rollback before apply".to_owned(),
        ],
        risks: vec![
            "upgrade check is diagnostic; no runtime process is started by CLI".to_owned(),
            "rollback remains planned until lifecycle apply is explicitly authorized".to_owned(),
        ],
    })
}

fn build_memory_context(
    project: &ProjectConfig,
    options: &MemoryContextOptions,
) -> Result<BuiltContext, EvaError> {
    let agent_id = AgentId::parse(&options.agent_id)?;
    if !project.agents.iter().any(|agent| agent.id == agent_id) {
        return Err(
            EvaError::not_found("Agent does not exist for memory context")
                .with_context("agent_id", agent_id.as_str()),
        );
    }
    let request_id = RequestId::parse(&options.request_id)?;
    let mut memory = InMemoryMemoryService::new();
    memory.write(
        MemoryWrite::private(
            agent_id.clone(),
            "agent.identity",
            format!("agent {} owns this private context", agent_id),
        )
        .with_request_id(request_id.clone()),
    )?;
    memory.write(
        MemoryWrite::private(
            agent_id.clone(),
            "project.agent_count",
            project.agents.len().to_string(),
        )
        .with_request_id(request_id.clone()),
    )?;
    memory.write(
        MemoryWrite::global("release.checkpoint", "V1.2 memory and knowledge context")
            .with_request_id(request_id.clone()),
    )?;
    memory.write(
        MemoryWrite::global("workspace.root", display_path(&project.project_root))
            .with_request_id(request_id.clone()),
    )?;

    let mut knowledge = InMemoryKnowledgeService::new();
    index_context_knowledge(&mut knowledge, request_id.clone())?;
    ContextBuilder::new(&memory, &knowledge).build(
        ContextRequest::new(request_id, agent_id, options.query.clone()).with_budget(
            ContextBudget {
                private_memory: options.private_limit,
                global_memory: options.global_limit,
                knowledge: options.knowledge_limit,
            },
        ),
    )
}

fn index_context_knowledge(
    knowledge: &mut InMemoryKnowledgeService,
    request_id: RequestId,
) -> Result<(), EvaError> {
    let items = [
        (
            "memory-service",
            "MemoryService",
            "Agent private memory is isolated by agent_id; global memory is shared and audited.",
            "v1.2 memory private global audit context",
        ),
        (
            "context-builder",
            "ContextBuilder",
            "ContextBuilder assembles private memory, global memory, and knowledge under request budgets.",
            "v1.2 context budget knowledge lua controlled api",
        ),
        (
            "knowledge-service",
            "KnowledgeService",
            "KnowledgeService indexes traceable documents and code snippets with lightweight digests.",
            "v1.2 knowledge index search citation digest",
        ),
    ];
    for (id, title, summary, content) in items {
        knowledge.index(
            KnowledgeItem::new(
                KnowledgeId::parse(id)?,
                KnowledgeSource::new(format!("docs/{id}.md"), title, content.as_bytes()),
                summary,
                content,
            )?
            .with_tag("v1.2")
            .with_request_id(request_id.clone()),
        )?;
    }
    Ok(())
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

fn probe_adapter_runtime(
    runtime: &AdapterRuntime,
    options: &AdapterProbeOptions,
) -> Result<AdapterProbeReport, EvaError> {
    if let Some(adapter_id) = &options.adapter_id {
        return runtime.probe_adapter(&AdapterId::parse(adapter_id)?);
    }
    let capability = options
        .capability
        .as_deref()
        .map(CapabilityName::parse)
        .transpose()?
        .unwrap_or_else(|| CapabilityName::parse("workflow.code_review").unwrap());
    let provider = options
        .provider
        .as_deref()
        .map(AdapterId::parse)
        .transpose()?;
    runtime.probe_capability(capability, provider)
}

fn probe_mcp_runtime(
    runtime: &AdapterRuntime,
    options: &McpProbeOptions,
) -> Result<McpProbeReport, EvaError> {
    let adapter_id = AdapterId::parse(&options.adapter_id)?;
    let handle = runtime.registry().get(&adapter_id).ok_or_else(|| {
        EvaError::not_found("MCP adapter does not exist")
            .with_context("adapter_id", adapter_id.as_str())
    })?;
    if handle.transport != AdapterTransport::Mcp {
        return Err(
            EvaError::invalid_argument("adapter is not an MCP transport")
                .with_context("adapter_id", handle.id.as_str())
                .with_context("transport", handle.transport.as_str()),
        );
    }
    let tool = options
        .tool
        .clone()
        .or_else(|| handle.mcp_tools.first().cloned())
        .ok_or_else(|| {
            EvaError::not_found("MCP adapter has no allowlisted tools")
                .with_context("adapter_id", handle.id.as_str())
        })?;
    let client = InMemoryMcpClient::new(
        handle.id.clone(),
        McpAllowlist::from_tools(handle.mcp_tools.iter().cloned())?,
    );
    Ok(client.probe_tool(&tool))
}

fn run_skill_runtime(
    runtime: &AdapterRuntime,
    options: &SkillRunOptions,
) -> Result<AdapterInvokeReport, EvaError> {
    let capability = options
        .capability
        .as_deref()
        .map(CapabilityName::parse)
        .transpose()?
        .unwrap_or_else(|| CapabilityName::parse("workflow.code_review").unwrap());
    let provider = if let Some(adapter_id) = &options.adapter_id {
        Some(AdapterId::parse(adapter_id)?)
    } else if let Some(skill_id) = &options.skill_id {
        runtime
            .list()
            .into_iter()
            .find(|handle| {
                handle.transport == AdapterTransport::Skill
                    && handle.skill_name() == Some(skill_id.as_str())
            })
            .map(|handle| handle.id.clone())
    } else {
        None
    };
    let invocation = AdapterInvocation::new(RequestId::parse(&options.request_id)?, capability)
        .with_input(options.input.clone());
    let invocation = if let Some(provider) = provider {
        invocation.with_provider(provider)
    } else {
        invocation
    };
    runtime.invoke(invocation)
}

fn parse_config_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing config subcommand"))?;
    match subcommand.as_str() {
        "validate" => Ok(Command::ConfigValidate(parse_common_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown config subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_inspect_options(args: &[String]) -> Result<CommonOptions, EvaError> {
    let filtered = args
        .iter()
        .filter(|arg| {
            !matches!(
                arg.as_str(),
                "all"
                    | "config"
                    | "runtime"
                    | "routes"
                    | "policy"
                    | "agents"
                    | "adapters"
                    | "capabilities"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    parse_common_options(&filtered)
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

impl ValidationReport {
    fn from_project(project: &ProjectConfig) -> Self {
        let schemas = schema_paths(&project.roots);
        Self {
            project_root: display_path(&project.project_root),
            eva_config_path: display_path(&project.eva_config_path),
            environment: project.eva.runtime.env.clone(),
            hot_reload: project.eva.runtime.hot_reload,
            agents_total: project.agents.len(),
            agents_enabled: project.agents.iter().filter(|agent| agent.enabled).count(),
            adapters_total: project.adapters.len(),
            adapters_enabled: project
                .adapters
                .iter()
                .filter(|adapter| adapter.enabled)
                .count(),
            capabilities_total: project.capabilities.len(),
            capabilities_enabled: project
                .capabilities
                .iter()
                .filter(|capability| capability.enabled)
                .count(),
            policies_total: project.policies.len(),
            routes_total: project.routes.routes.len(),
            schema_files: vec![
                display_path(&schemas.eva),
                display_path(&schemas.agent),
                display_path(&schemas.adapter),
                display_path(&schemas.capability),
                display_path(&schemas.policy),
                display_path(&schemas.routes),
            ],
        }
    }
}

impl TaskSnapshot {
    fn from_report(report: &BasicRunReport) -> Self {
        Self {
            task_id: report.task.task_id.as_str().to_owned(),
            status: report.task.status.as_str().to_owned(),
            attempts: report.task.attempts,
            retry_max_attempts: report.task.retry_policy.max_attempts,
            cancel_requested: report.task.cancellation.requested,
            cancel_accepted: report.task.cancellation.accepted,
            cancel_reason: report.task.cancellation.reason.clone(),
            error_kind: report
                .task
                .error
                .as_ref()
                .map(|error| error.kind().as_str().to_owned()),
            error_message: report
                .task
                .error
                .as_ref()
                .map(|error| error.message().to_owned()),
            logs: report.task.logs.iter().map(TaskLogSnapshot::from).collect(),
            dead_letters: report
                .task
                .dead_letters
                .iter()
                .map(|entry| TaskDeadLetterSnapshot {
                    event_id: entry.event_id.clone(),
                    topic: entry.topic.clone(),
                    reason_kind: entry.reason_kind.clone(),
                    reason: entry.reason.clone(),
                    replay_count: entry.replay_count,
                })
                .collect(),
            replayed_events: report
                .task
                .replayed_events
                .iter()
                .map(|entry| TaskReplaySnapshot {
                    event_id: entry.event_id.clone(),
                    sequence: entry.sequence,
                    topic: entry.topic.clone(),
                })
                .collect(),
        }
    }

    fn to_storage(&self) -> String {
        let mut lines = vec![
            format!("task_id={}", encode_field(&self.task_id)),
            format!("status={}", encode_field(&self.status)),
            format!("attempts={}", self.attempts),
            format!("retry_max_attempts={}", self.retry_max_attempts),
            format!("cancel_requested={}", self.cancel_requested),
            format!("cancel_accepted={}", self.cancel_accepted),
            format!(
                "cancel_reason={}",
                self.cancel_reason
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "error_kind={}",
                self.error_kind
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "error_message={}",
                self.error_message
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
        ];
        lines.extend(self.logs.iter().map(|entry| {
            format!(
                "log={}|{}|{}",
                entry.sequence,
                encode_field(&entry.level),
                encode_field(&entry.message)
            )
        }));
        lines.extend(self.dead_letters.iter().map(|entry| {
            format!(
                "dead_letter={}|{}|{}|{}|{}",
                encode_field(&entry.event_id),
                encode_field(&entry.topic),
                encode_field(&entry.reason_kind),
                encode_field(&entry.reason),
                entry.replay_count
            )
        }));
        lines.extend(self.replayed_events.iter().map(|entry| {
            format!(
                "replay={}|{}|{}",
                encode_field(&entry.event_id),
                entry.sequence,
                encode_field(&entry.topic)
            )
        }));
        lines.push(String::new());
        lines.join("\n")
    }
}

impl From<&TaskLogEntry> for TaskLogSnapshot {
    fn from(entry: &TaskLogEntry) -> Self {
        Self {
            sequence: entry.sequence,
            level: entry.level.as_str().to_owned(),
            message: entry.message.clone(),
        }
    }
}

fn write_task_snapshot(project_root: &Path, report: &BasicRunReport) -> Result<(), EvaError> {
    let snapshot = TaskSnapshot::from_report(report);
    let dir = task_dir(project_root);
    fs::create_dir_all(&dir).map_err(|error| {
        EvaError::internal("failed to create task state directory")
            .with_context("path", dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let data = snapshot.to_storage();
    fs::write(task_path(project_root, &snapshot.task_id)?, data.as_bytes()).map_err(|error| {
        EvaError::internal("failed to write task state")
            .with_context("task_id", snapshot.task_id.as_str())
            .with_context("io_error", error.to_string())
    })?;
    fs::write(latest_task_path(project_root), data.as_bytes()).map_err(|error| {
        EvaError::internal("failed to write latest task state")
            .with_context("task_id", snapshot.task_id.as_str())
            .with_context("io_error", error.to_string())
    })
}

fn read_task_snapshot(
    project_root: &Path,
    task_id: Option<&str>,
) -> Result<TaskSnapshot, EvaError> {
    let path = match task_id {
        Some(task_id) => task_path(project_root, task_id)?,
        None => latest_task_path(project_root),
    };
    let data = fs::read_to_string(&path).map_err(|error| {
        EvaError::not_found("task state does not exist")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
            .with_context("suggestion", "run `eva run --example basic` first")
    })?;
    parse_task_snapshot(&data)
}

fn cancel_task_snapshot(
    project_root: &Path,
    task_id: Option<&str>,
    reason: &str,
) -> Result<TaskSnapshot, EvaError> {
    let mut snapshot = read_task_snapshot(project_root, task_id)?;
    snapshot.cancel_requested = true;
    snapshot.cancel_reason = Some(reason.to_owned());
    if is_terminal_task_status(&snapshot.status) {
        snapshot.cancel_accepted = false;
        snapshot.logs.push(TaskLogSnapshot {
            sequence: snapshot.logs.len() as u64 + 1,
            level: "warning".to_owned(),
            message: "cancel requested after task reached a terminal state".to_owned(),
        });
    } else {
        snapshot.cancel_accepted = true;
        snapshot.status = "cancelled".to_owned();
        snapshot.logs.push(TaskLogSnapshot {
            sequence: snapshot.logs.len() as u64 + 1,
            level: "warning".to_owned(),
            message: format!("cancel accepted: {reason}"),
        });
    }

    let dir = task_dir(project_root);
    fs::create_dir_all(&dir).map_err(|error| {
        EvaError::internal("failed to create task state directory")
            .with_context("path", dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let data = snapshot.to_storage();
    fs::write(task_path(project_root, &snapshot.task_id)?, data.as_bytes()).map_err(|error| {
        EvaError::internal("failed to write task state")
            .with_context("task_id", snapshot.task_id.as_str())
            .with_context("io_error", error.to_string())
    })?;
    fs::write(latest_task_path(project_root), data.as_bytes()).map_err(|error| {
        EvaError::internal("failed to write latest task state")
            .with_context("task_id", snapshot.task_id.as_str())
            .with_context("io_error", error.to_string())
    })?;
    Ok(snapshot)
}

fn task_dir(project_root: &Path) -> PathBuf {
    project_root.join(".eva").join("tasks")
}

fn latest_task_path(project_root: &Path) -> PathBuf {
    task_dir(project_root).join("latest-basic.task")
}

fn task_path(project_root: &Path, task_id: &str) -> Result<PathBuf, EvaError> {
    eva_core::RequestId::parse(task_id)?;
    Ok(task_dir(project_root).join(format!("{task_id}.task")))
}

fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "timed_out")
}

fn parse_task_snapshot(data: &str) -> Result<TaskSnapshot, EvaError> {
    let mut snapshot = TaskSnapshot {
        task_id: String::new(),
        status: String::new(),
        attempts: 0,
        retry_max_attempts: 1,
        cancel_requested: false,
        cancel_accepted: false,
        cancel_reason: None,
        error_kind: None,
        error_message: None,
        logs: Vec::new(),
        dead_letters: Vec::new(),
        replayed_events: Vec::new(),
    };

    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        if let Some(value) = line.strip_prefix("task_id=") {
            snapshot.task_id = decode_field(value);
        } else if let Some(value) = line.strip_prefix("status=") {
            snapshot.status = decode_field(value);
        } else if let Some(value) = line.strip_prefix("attempts=") {
            snapshot.attempts = parse_stored_usize("attempts", value)?;
        } else if let Some(value) = line.strip_prefix("retry_max_attempts=") {
            snapshot.retry_max_attempts = parse_stored_usize("retry_max_attempts", value)?;
        } else if let Some(value) = line.strip_prefix("cancel_requested=") {
            snapshot.cancel_requested = value == "true";
        } else if let Some(value) = line.strip_prefix("cancel_accepted=") {
            snapshot.cancel_accepted = value == "true";
        } else if let Some(value) = line.strip_prefix("cancel_reason=") {
            snapshot.cancel_reason = decode_optional_field(value);
        } else if let Some(value) = line.strip_prefix("error_kind=") {
            snapshot.error_kind = decode_optional_field(value);
        } else if let Some(value) = line.strip_prefix("error_message=") {
            snapshot.error_message = decode_optional_field(value);
        } else if let Some(value) = line.strip_prefix("log=") {
            let parts = split_stored_fields(value, 3, "log")?;
            snapshot.logs.push(TaskLogSnapshot {
                sequence: parse_stored_u64("log.sequence", &parts[0])?,
                level: decode_field(&parts[1]),
                message: decode_field(&parts[2]),
            });
        } else if let Some(value) = line.strip_prefix("dead_letter=") {
            let parts = split_stored_fields(value, 5, "dead_letter")?;
            snapshot.dead_letters.push(TaskDeadLetterSnapshot {
                event_id: decode_field(&parts[0]),
                topic: decode_field(&parts[1]),
                reason_kind: decode_field(&parts[2]),
                reason: decode_field(&parts[3]),
                replay_count: parse_stored_usize("dead_letter.replay_count", &parts[4])?,
            });
        } else if let Some(value) = line.strip_prefix("replay=") {
            let parts = split_stored_fields(value, 3, "replay")?;
            snapshot.replayed_events.push(TaskReplaySnapshot {
                event_id: decode_field(&parts[0]),
                sequence: parse_stored_u64("replay.sequence", &parts[1])?,
                topic: decode_field(&parts[2]),
            });
        }
    }

    if snapshot.task_id.is_empty() || snapshot.status.is_empty() {
        return Err(EvaError::invalid_argument("task state file is incomplete"));
    }
    Ok(snapshot)
}

fn split_stored_fields(
    value: &str,
    expected: usize,
    field: &'static str,
) -> Result<Vec<String>, EvaError> {
    let parts = value.split('|').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != expected {
        return Err(
            EvaError::invalid_argument("task state field has invalid arity")
                .with_context("field", field)
                .with_context("expected", expected.to_string())
                .with_context("actual", parts.len().to_string()),
        );
    }
    Ok(parts)
}

fn parse_stored_usize(name: &'static str, value: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn parse_stored_u64(name: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn decode_optional_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(decode_field(value))
    }
}

fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
        .replace('|', "%7C")
        .replace('=', "%3D")
}

fn decode_field(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%09", "\t")
        .replace("%7C", "|")
        .replace("%3D", "=")
        .replace("%25", "%")
}

fn write_validation<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &ValidationReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK config validated").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(writer, "eva_config: {}", report.eva_config_path).map_err(write_error_kind)?;
            writeln!(writer, "environment: {}", report.environment).map_err(write_error_kind)?;
            writeln!(writer, "hot_reload: {}", report.hot_reload).map_err(write_error_kind)?;
            writeln!(
                writer,
                "agents: {} total, {} enabled",
                report.agents_total, report.agents_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "adapters: {} total, {} enabled",
                report.adapters_total, report.adapters_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "capabilities: {} total, {} enabled",
                report.capabilities_total, report.capabilities_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "policies: {}", report.policies_total).map_err(write_error_kind)?;
            writeln!(writer, "routes: {}", report.routes_total).map_err(write_error_kind)?;
            Ok(())
        }
        OutputFormat::Json => {
            let data = format!(
                "{{\"project_root\":{},\"eva_config_path\":{},\"environment\":{},\"hot_reload\":{},\"counts\":{{\"agents_total\":{},\"agents_enabled\":{},\"adapters_total\":{},\"adapters_enabled\":{},\"capabilities_total\":{},\"capabilities_enabled\":{},\"policies_total\":{},\"routes_total\":{}}},\"schema_files\":{}}}",
                json_string(&report.project_root),
                json_string(&report.eva_config_path),
                json_string(&report.environment),
                report.hot_reload,
                report.agents_total,
                report.agents_enabled,
                report.adapters_total,
                report.adapters_enabled,
                report.capabilities_total,
                report.capabilities_enabled,
                report.policies_total,
                report.routes_total,
                json_array(report.schema_files.iter().map(|path| json_string(path))),
            );
            writeln!(
                writer,
                "{}",
                success_envelope("config.validate", EXIT_OK, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}

fn write_doctor<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    report: &DoctorReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva doctor").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            for check in &report.checks {
                writeln!(
                    writer,
                    "[{}] {} - {}",
                    check.status.as_str(),
                    check.name,
                    check.message
                )
                .map_err(write_error_kind)?;
                if let Some(path) = &check.path {
                    writeln!(writer, "  path: {path}").map_err(write_error_kind)?;
                }
                if let Some(suggestion) = &check.suggestion {
                    writeln!(writer, "  suggestion: {suggestion}").map_err(write_error_kind)?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => {
            let checks = report
                .checks
                .iter()
                .map(|check| {
                    let mut fields = vec![
                        format!("\"name\":{}", json_string(&check.name)),
                        format!("\"status\":{}", json_string(check.status.as_str())),
                        format!("\"message\":{}", json_string(&check.message)),
                    ];
                    if let Some(path) = &check.path {
                        fields.push(format!("\"path\":{}", json_string(path)));
                    }
                    if let Some(suggestion) = &check.suggestion {
                        fields.push(format!("\"suggestion\":{}", json_string(suggestion)));
                    }
                    format!("{{{}}}", fields.join(","))
                })
                .collect::<Vec<_>>();
            let data = format!(
                "{{\"project_root\":{},\"checks\":{},\"error_count\":{},\"warning_count\":{}}}",
                json_string(&report.project_root),
                json_array(checks),
                report
                    .checks
                    .iter()
                    .filter(|check| check.status == CheckStatus::Error)
                    .count(),
                report
                    .checks
                    .iter()
                    .filter(|check| check.status == CheckStatus::Warning)
                    .count(),
            );
            writeln!(
                writer,
                "{}",
                success_envelope("doctor", exit_code, &data, trace)
            )
            .map_err(write_error_kind)
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

fn write_version<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "eva {CLI_VERSION}").map_err(write_error_kind)?;
            writeln!(writer, "release: {RELEASE_LABEL}").map_err(write_error_kind)?;
            writeln!(writer, "runtime_mode: {RELEASE_RUNTIME_MODE}").map_err(write_error_kind)?;
            writeln!(writer, "contracts: {}", RELEASE_CONTRACTS.join(", "))
                .map_err(write_error_kind)
        }
        OutputFormat::Json => {
            let data = format!(
                "{{\"version\":{},\"release\":{},\"runtime_mode\":{},\"contracts\":{}}}",
                json_string(CLI_VERSION),
                json_string(RELEASE_LABEL),
                json_string(RELEASE_RUNTIME_MODE),
                json_array(RELEASE_CONTRACTS.iter().copied().map(json_string))
            );
            writeln!(
                writer,
                "{}",
                success_envelope("version", EXIT_OK, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}

fn write_adapter_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    runtime: &AdapterRuntime,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva adapters").map_err(write_error_kind)?;
            for handle in runtime.list() {
                writeln!(
                    writer,
                    "  - {} transport={} enabled={} health={} capabilities={}",
                    handle.id,
                    handle.transport.as_str(),
                    handle.enabled,
                    handle.health().as_str(),
                    join_capabilities(&handle.capabilities)
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("adapter.list", EXIT_OK, &adapter_list_json(runtime), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_adapter_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &AdapterProbeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Adapter probe").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", report.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "transport: {}", report.transport.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "capabilities: {}",
                join_capabilities(&report.capabilities)
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "detail: {}", report.detail).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("adapter.probe", EXIT_OK, &adapter_probe_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_mcp_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    runtime: &AdapterRuntime,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva MCP adapters").map_err(write_error_kind)?;
            for handle in runtime
                .list()
                .into_iter()
                .filter(|handle| handle.transport == AdapterTransport::Mcp)
            {
                writeln!(
                    writer,
                    "  - {} tools={} capabilities={}",
                    handle.id,
                    handle.mcp_tools.join(","),
                    join_capabilities(&handle.capabilities)
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("mcp.list", EXIT_OK, &mcp_list_json(runtime), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_mcp_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &McpProbeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "MCP probe").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", report.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "tool: {}", report.tool).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status.as_str()).map_err(write_error_kind)?;
            writeln!(writer, "detail: {}", report.message).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("mcp.probe", EXIT_OK, &mcp_probe_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_skill_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    runtime: &AdapterRuntime,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva skills").map_err(write_error_kind)?;
            for handle in runtime
                .list()
                .into_iter()
                .filter(|handle| handle.transport == AdapterTransport::Skill)
            {
                writeln!(
                    writer,
                    "  - {} skill={} gate={} capabilities={}",
                    handle.id,
                    handle.skill_name().unwrap_or(""),
                    handle.skill_runtime_gate.as_deref().unwrap_or(""),
                    join_capabilities(&handle.capabilities)
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("skill.list", EXIT_OK, &skill_list_json(runtime), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_adapter_invoke<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    command: &str,
    report: &AdapterInvokeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK {command}").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", report.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "capability: {}", report.capability).map_err(write_error_kind)?;
            writeln!(writer, "transport: {}", report.transport.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "output: {}", report.output).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(command, EXIT_OK, &adapter_invoke_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_discovery_scan<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    candidates: &[DiscoveryCandidate],
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva discovery candidates").map_err(write_error_kind)?;
            for candidate in candidates {
                writeln!(
                    writer,
                    "  - {} kind={} source={} trust={} handle_granted={}",
                    candidate.id,
                    candidate.kind.as_str(),
                    candidate.source,
                    candidate.trust.as_str(),
                    candidate.handle_granted
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "discovery.scan",
                EXIT_OK,
                &discovery_scan_json(candidates),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_memory_context<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    context: &BuiltContext,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva memory context").map_err(write_error_kind)?;
            writeln!(writer, "request: {}", context.request_id).map_err(write_error_kind)?;
            writeln!(writer, "agent: {}", context.agent_id).map_err(write_error_kind)?;
            writeln!(writer, "query: {}", context.query).map_err(write_error_kind)?;
            writeln!(writer, "private_memory: {}", context.memory.len())
                .map_err(write_error_kind)?;
            writeln!(writer, "global_memory: {}", context.global_memory.len())
                .map_err(write_error_kind)?;
            writeln!(writer, "knowledge: {}", context.knowledge.len()).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "memory.context",
                EXIT_OK,
                &memory_context_json(context),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_hardware_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &HardwareDiscoveryReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva hardware candidates").map_err(write_error_kind)?;
            for candidate in &report.candidates {
                writeln!(
                    writer,
                    "  - {} adapter={} bus={} health={} trust={} handle_granted={}",
                    candidate.identity.id.as_str(),
                    candidate.identity.adapter_id,
                    candidate.identity.bus.as_str(),
                    candidate.health.as_str(),
                    candidate.identity.trust.as_str(),
                    candidate.handle_granted
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "hardware.list",
                EXIT_OK,
                &hardware_candidates_json(&report.candidates),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_hardware_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    candidates: &[DeviceCandidate],
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva hardware probe").map_err(write_error_kind)?;
            for candidate in candidates {
                writeln!(
                    writer,
                    "  - {} status={} reason={}",
                    candidate.identity.id.as_str(),
                    candidate.health.as_str(),
                    candidate.rejected_reason.as_deref().unwrap_or("ok")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "hardware.probe",
                EXIT_OK,
                &hardware_candidates_json(candidates),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_hardware_bind<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    plan: &HardwareBindPlan,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Hardware bind plan").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", plan.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", plan.status).map_err(write_error_kind)?;
            writeln!(writer, "apply: {}", plan.apply).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "hardware.bind",
                EXIT_OK,
                &hardware_bind_plan_json(plan),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_backup_create<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    result: &BackupResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Backup artifact created").map_err(write_error_kind)?;
            writeln!(writer, "artifact: {}", result.manifest.artifact_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "digest: {}", result.manifest.digest).map_err(write_error_kind)?;
            writeln!(writer, "verified: {}", result.verification.verified).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("backup.create", EXIT_OK, &backup_result_json(result), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_snapshot_create<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    backup: &BackupResult,
    snapshot: &ReleaseSnapshot,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Release snapshot created").map_err(write_error_kind)?;
            writeln!(writer, "snapshot: {}", snapshot.snapshot_id).map_err(write_error_kind)?;
            writeln!(writer, "backup: {}", backup.manifest.artifact_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "role: {}", snapshot.role.as_str()).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "snapshot.create",
                EXIT_OK,
                &snapshot_create_json(backup, snapshot),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_restore_plan<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    snapshot: &ReleaseSnapshot,
    plan: &RestorePlan,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Restore plan").map_err(write_error_kind)?;
            writeln!(writer, "snapshot: {}", snapshot.snapshot_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", plan.status).map_err(write_error_kind)?;
            writeln!(writer, "apply_allowed: {}", plan.apply_allowed).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "restore.plan",
                EXIT_OK,
                &restore_plan_json(snapshot, plan),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_upgrade_check<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &UpgradeCheckReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Upgrade check").map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "active: {}", report.supervisor.active_generation)
                .map_err(write_error_kind)?;
            writeln!(writer, "migration: {}", report.migration.status).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("upgrade.check", EXIT_OK, &upgrade_check_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn adapter_list_json(runtime: &AdapterRuntime) -> String {
    let entries = runtime.list().into_iter().map(|handle| {
        format!(
            "{{\"id\":{},\"name\":{},\"version\":{},\"enabled\":{},\"health\":{},\"transport\":{},\"capabilities\":{},\"mcp_tools\":{},\"skill_id\":{},\"source_path\":{}}}",
            json_string(handle.id.as_str()),
            json_string(&handle.name),
            json_string(&handle.version),
            handle.enabled,
            json_string(handle.health().as_str()),
            json_string(handle.transport.as_str()),
            json_array(handle.capabilities.iter().map(|capability| json_string(capability.as_str()))),
            json_array(handle.mcp_tools.iter().map(|tool| json_string(tool))),
            option_json(handle.skill_name()),
            json_string(&handle.source_path)
        )
    });
    format!("{{\"adapters\":{}}}", json_array(entries))
}

fn adapter_probe_json(report: &AdapterProbeReport) -> String {
    format!(
        "{{\"adapter_id\":{},\"transport\":{},\"status\":{},\"capabilities\":{},\"detail\":{}}}",
        json_string(report.adapter_id.as_str()),
        json_string(report.transport.as_str()),
        json_string(&report.status),
        json_array(
            report
                .capabilities
                .iter()
                .map(|capability| json_string(capability.as_str()))
        ),
        json_string(&report.detail)
    )
}

fn mcp_list_json(runtime: &AdapterRuntime) -> String {
    let entries = runtime
        .list()
        .into_iter()
        .filter(|handle| handle.transport == AdapterTransport::Mcp)
        .map(|handle| {
            format!(
                "{{\"adapter_id\":{},\"tools\":{},\"capabilities\":{},\"enabled\":{}}}",
                json_string(handle.id.as_str()),
                json_array(handle.mcp_tools.iter().map(|tool| json_string(tool))),
                json_array(
                    handle
                        .capabilities
                        .iter()
                        .map(|capability| json_string(capability.as_str()))
                ),
                handle.enabled
            )
        });
    format!("{{\"mcp_adapters\":{}}}", json_array(entries))
}

fn mcp_probe_json(report: &McpProbeReport) -> String {
    format!(
        "{{\"adapter_id\":{},\"tool\":{},\"status\":{},\"message\":{}}}",
        json_string(report.adapter_id.as_str()),
        json_string(&report.tool),
        json_string(report.status.as_str()),
        json_string(&report.message)
    )
}

fn skill_list_json(runtime: &AdapterRuntime) -> String {
    let entries = runtime
        .list()
        .into_iter()
        .filter(|handle| handle.transport == AdapterTransport::Skill)
        .map(|handle| {
            format!(
                "{{\"adapter_id\":{},\"skill_id\":{},\"skill_kind\":{},\"runtime_gate\":{},\"capabilities\":{},\"enabled\":{}}}",
                json_string(handle.id.as_str()),
                option_json(handle.skill_name()),
                option_json(handle.skill_kind.as_deref()),
                option_json(handle.skill_runtime_gate.as_deref()),
                json_array(handle.capabilities.iter().map(|capability| json_string(capability.as_str()))),
                handle.enabled
            )
        });
    format!("{{\"skills\":{}}}", json_array(entries))
}

fn adapter_invoke_json(report: &AdapterInvokeReport) -> String {
    format!(
        "{{\"request_id\":{},\"adapter_id\":{},\"transport\":{},\"capability\":{},\"status\":{},\"output\":{},\"audit\":{}}}",
        json_string(report.request_id.as_str()),
        json_string(report.adapter_id.as_str()),
        json_string(report.transport.as_str()),
        json_string(report.capability.as_str()),
        json_string(&report.status),
        json_string(&report.output),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn discovery_scan_json(candidates: &[DiscoveryCandidate]) -> String {
    let entries = candidates.iter().map(|candidate| {
        format!(
            "{{\"id\":{},\"kind\":{},\"source\":{},\"trust\":{},\"adapter_id\":{},\"capability\":{},\"handle_granted\":{},\"rejected_reason\":{}}}",
            json_string(&candidate.id),
            json_string(candidate.kind.as_str()),
            json_string(&candidate.source),
            json_string(candidate.trust.as_str()),
            option_json(candidate.adapter_id.as_ref().map(|id| id.as_str())),
            option_json(candidate.capability.as_ref().map(|capability| capability.as_str())),
            candidate.handle_granted,
            option_json(candidate.rejected_reason.as_deref())
        )
    });
    format!(
        "{{\"candidate_count\":{},\"candidates\":{}}}",
        candidates.len(),
        json_array(entries)
    )
}

fn memory_context_json(context: &BuiltContext) -> String {
    format!(
        "{{\"request_id\":{},\"agent_id\":{},\"query\":{},\"totals\":{{\"items\":{},\"private_memory\":{},\"global_memory\":{},\"knowledge\":{}}},\"memory\":{},\"global_memory\":{},\"knowledge\":{},\"lua_context\":{},\"audit\":{}}}",
        json_string(context.request_id.as_str()),
        json_string(context.agent_id.as_str()),
        json_string(&context.query),
        context.total_items(),
        context.memory.len(),
        context.global_memory.len(),
        context.knowledge.len(),
        json_array(context.memory.iter().map(memory_record_json)),
        json_array(context.global_memory.iter().map(memory_record_json)),
        json_array(context.knowledge.iter().map(knowledge_result_json)),
        lua_context_json(context),
        json_array(context.audit.iter().map(|entry| json_string(entry)))
    )
}

fn memory_record_json(record: &eva_memory::MemoryRecord) -> String {
    format!(
        "{{\"key\":{},\"value\":{},\"visibility\":{},\"owner_agent\":{},\"retention\":{},\"version\":{},\"audit_reason\":{}}}",
        json_string(&record.key),
        json_string(&record.value),
        json_string(record.visibility.as_str()),
        option_json(record.owner_agent.as_ref().map(|agent| agent.as_str())),
        json_string(record.retention.as_str()),
        record.version.0,
        json_string(&record.audit_reason)
    )
}

fn knowledge_result_json(result: &eva_memory::KnowledgeSearchResult) -> String {
    format!(
        "{{\"id\":{},\"title\":{},\"source\":{},\"digest\":{},\"summary\":{},\"score\":{},\"matched_by\":{}}}",
        json_string(result.item.id.as_str()),
        json_string(&result.item.source.title),
        json_string(&result.item.source.uri),
        json_string(&result.item.source.digest),
        json_string(&result.item.summary),
        result.score,
        json_array(result.matched_by.iter().map(|entry| json_string(entry)))
    )
}

fn lua_context_json(context: &BuiltContext) -> String {
    let snapshot = context.lua_summary();
    format!(
        "{{\"private_memory_count\":{},\"global_memory_count\":{},\"knowledge_count\":{},\"audit\":{}}}",
        snapshot.private_memory_count,
        snapshot.global_memory_count,
        snapshot.knowledge_count,
        json_array(snapshot.audit.iter().map(|entry| json_string(entry)))
    )
}

fn hardware_candidates_json(candidates: &[DeviceCandidate]) -> String {
    let entries = candidates.iter().map(hardware_candidate_json);
    format!(
        "{{\"candidate_count\":{},\"candidates\":{}}}",
        candidates.len(),
        json_array(entries)
    )
}

fn hardware_candidate_json(candidate: &DeviceCandidate) -> String {
    format!(
        "{{\"device_id\":{},\"adapter_id\":{},\"logical_name\":{},\"device_class\":{},\"bus\":{},\"trust\":{},\"health\":{},\"vendor_id\":{},\"product_id\":{},\"serial\":{},\"protocol\":{},\"handle_granted\":{},\"rejected_reason\":{},\"source_path\":{}}}",
        json_string(candidate.identity.id.as_str()),
        json_string(candidate.identity.adapter_id.as_str()),
        json_string(&candidate.identity.logical_name),
        json_string(&candidate.identity.device_class),
        json_string(candidate.identity.bus.as_str()),
        json_string(candidate.identity.trust.as_str()),
        json_string(candidate.health.as_str()),
        option_json(candidate.vendor_id.as_deref()),
        option_json(candidate.product_id.as_deref()),
        option_json(candidate.serial.as_deref()),
        option_json(candidate.protocol.as_deref()),
        candidate.handle_granted,
        option_json(candidate.rejected_reason.as_deref()),
        json_string(&candidate.source_path)
    )
}

fn hardware_bind_plan_json(plan: &HardwareBindPlan) -> String {
    format!(
        "{{\"adapter_id\":{},\"request_id\":{},\"status\":{},\"apply\":{},\"device\":{},\"steps\":{},\"risks\":{}}}",
        json_string(plan.adapter_id.as_str()),
        json_string(plan.request_id.as_str()),
        json_string(&plan.status),
        plan.apply,
        plan.device
            .as_ref()
            .map(hardware_candidate_json)
            .unwrap_or_else(|| "null".to_owned()),
        json_array(plan.steps.iter().map(|step| json_string(step))),
        json_array(plan.risks.iter().map(|risk| json_string(risk)))
    )
}

fn backup_result_json(result: &BackupResult) -> String {
    format!(
        "{{\"artifact_id\":{},\"request_id\":{},\"runtime_generation\":{},\"project_id\":{},\"digest\":{},\"verified\":{},\"entries\":{},\"risks\":{},\"audit\":{}}}",
        json_string(&result.manifest.artifact_id),
        json_string(result.manifest.request_id.as_str()),
        json_string(result.manifest.runtime_generation.as_str()),
        json_string(&result.manifest.project_id),
        json_string(&result.manifest.digest),
        result.verification.verified,
        json_array(result.manifest.entries.iter().map(|entry| {
            format!(
                "{{\"path\":{},\"size_bytes\":{},\"redacted\":{}}}",
                json_string(&entry.path),
                entry.size_bytes,
                entry.redacted
            )
        })),
        json_array(result.plan.risks.iter().map(|risk| json_string(risk))),
        json_array(result.manifest.audit.iter().map(|entry| json_string(entry)))
    )
}

fn snapshot_json(snapshot: &ReleaseSnapshot) -> String {
    format!(
        "{{\"snapshot_id\":{},\"role\":{},\"release_ref\":{},\"request_id\":{},\"runtime_generation\":{},\"backup_artifact_id\":{},\"backup_digest\":{},\"health_status\":{},\"audit\":{}}}",
        json_string(&snapshot.snapshot_id),
        json_string(snapshot.role.as_str()),
        json_string(&snapshot.release_ref),
        json_string(snapshot.request_id.as_str()),
        json_string(snapshot.runtime_generation.as_str()),
        json_string(&snapshot.backup_artifact_id),
        json_string(&snapshot.backup_digest),
        json_string(&snapshot.health_status),
        json_array(snapshot.audit.iter().map(|entry| json_string(entry)))
    )
}

fn snapshot_create_json(backup: &BackupResult, snapshot: &ReleaseSnapshot) -> String {
    format!(
        "{{\"snapshot\":{},\"backup\":{}}}",
        snapshot_json(snapshot),
        backup_result_json(backup)
    )
}

fn restore_plan_json(snapshot: &ReleaseSnapshot, plan: &RestorePlan) -> String {
    format!(
        "{{\"snapshot\":{},\"plan\":{{\"snapshot_id\":{},\"status\":{},\"apply_allowed\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}}}",
        snapshot_json(snapshot),
        json_string(&plan.snapshot_id),
        json_string(&plan.status),
        plan.apply_allowed,
        json_array(plan.steps.iter().map(|step| json_string(step))),
        json_array(plan.risks.iter().map(|risk| json_string(risk))),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
    )
}

fn upgrade_check_json(report: &UpgradeCheckReport) -> String {
    format!(
        "{{\"status\":{},\"supervisor\":{},\"drain\":{},\"rollback\":{},\"migration\":{},\"steps\":{},\"risks\":{}}}",
        json_string(&report.status),
        supervisor_report_json(&report.supervisor),
        drain_plan_json(&report.drain),
        rollback_plan_json(&report.rollback),
        migration_preflight_json(&report.migration),
        json_array(report.steps.iter().map(|step| json_string(step))),
        json_array(report.risks.iter().map(|risk| json_string(risk)))
    )
}

fn supervisor_report_json(report: &SupervisorReport) -> String {
    format!(
        "{{\"active_generation\":{},\"candidate_generation\":{},\"healthy\":{},\"audit\":{}}}",
        json_string(report.active_generation.as_str()),
        option_json(report.candidate_generation.as_ref().map(|id| id.as_str())),
        report.healthy,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn drain_plan_json(plan: &DrainPlan) -> String {
    format!(
        "{{\"generation_id\":{},\"inflight_tasks\":{},\"timeout_ms\":{},\"accepts_new_work\":{},\"status\":{},\"audit\":{}}}",
        json_string(plan.generation_id.as_str()),
        plan.inflight_tasks,
        plan.timeout_ms,
        plan.accepts_new_work,
        json_string(plan.status.as_str()),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
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

fn migration_preflight_json(report: &MigrationPreflight) -> String {
    format!(
        "{{\"package_id\":{},\"status\":{},\"warnings\":{},\"audit\":{}}}",
        json_string(&report.package_id),
        json_string(&report.status),
        json_array(report.warnings.iter().map(|warning| json_string(warning))),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
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
    let capability_response = report
        .capability_response
        .as_ref()
        .map(capability_response_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"runtime_mode\":{},\"generation_id\":{},\"project_root\":{},\"task\":{},\"event_id\":{},\"topic\":{},\"receipt\":{{\"event_id\":{},\"sequence\":{},\"topic\":{},\"target\":{}}},\"deliveries\":{},\"agent_runs\":{},\"lua_results\":{},\"lua_generation\":{{\"generation_id\":{},\"script_count\":{}}},\"capability_response\":{},\"audit\":{}}}",
        json_string(&report.runtime_mode),
        json_string(&report.generation_id),
        json_string(&report.project_root),
        task_snapshot_json(&TaskSnapshot::from_report(report)),
        json_string(&report.event_id),
        json_string(&report.topic),
        json_string(report.receipt.event_id.as_str()),
        report.receipt.sequence,
        json_string(report.receipt.topic.as_str()),
        json_string(&format!("{:?}", report.receipt.target)),
        json_array(deliveries),
        json_array(agent_runs),
        json_array(lua_results),
        json_string(report.lua_generation.generation_id.as_str()),
        report.lua_generation.script_count,
        capability_response,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn write_task_status<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    snapshot: &TaskSnapshot,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(
                writer,
                "task {} status={} attempts={}/{}",
                snapshot.task_id, snapshot.status, snapshot.attempts, snapshot.retry_max_attempts
            )
            .map_err(write_error_kind)?;
            if let Some(message) = &snapshot.error_message {
                writeln!(writer, "error: {message}").map_err(write_error_kind)?;
            }
            writeln!(writer, "dead_letters: {}", snapshot.dead_letters.len())
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "replayed_events: {}",
                snapshot.replayed_events.len()
            )
            .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("task.status", EXIT_OK, &task_snapshot_json(snapshot), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_task_logs<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    snapshot: &TaskSnapshot,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "task {} logs", snapshot.task_id).map_err(write_error_kind)?;
            for entry in &snapshot.logs {
                writeln!(
                    writer,
                    "{} [{}] {}",
                    entry.sequence, entry.level, entry.message
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("task.logs", EXIT_OK, &task_logs_json(snapshot), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_task_cancel<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    snapshot: &TaskSnapshot,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => writeln!(
            writer,
            "task {} cancel_requested={} accepted={} status={}",
            snapshot.task_id, snapshot.cancel_requested, snapshot.cancel_accepted, snapshot.status
        )
        .map_err(write_error_kind),
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("task.cancel", EXIT_OK, &task_snapshot_json(snapshot), trace)
        )
        .map_err(write_error_kind),
    }
}

fn task_logs_json(snapshot: &TaskSnapshot) -> String {
    format!(
        "{{\"task_id\":{},\"status\":{},\"logs\":{}}}",
        json_string(&snapshot.task_id),
        json_string(&snapshot.status),
        json_array(snapshot.logs.iter().map(task_log_json))
    )
}

fn task_snapshot_json(snapshot: &TaskSnapshot) -> String {
    format!(
        "{{\"task_id\":{},\"status\":{},\"attempts\":{},\"retry_policy\":{{\"max_attempts\":{}}},\"cancellation\":{{\"requested\":{},\"accepted\":{},\"reason\":{}}},\"error\":{},\"logs\":{},\"dead_letters\":{},\"replayed_events\":{}}}",
        json_string(&snapshot.task_id),
        json_string(&snapshot.status),
        snapshot.attempts,
        snapshot.retry_max_attempts,
        snapshot.cancel_requested,
        snapshot.cancel_accepted,
        option_json(snapshot.cancel_reason.as_deref()),
        task_error_json(snapshot),
        json_array(snapshot.logs.iter().map(task_log_json)),
        json_array(snapshot.dead_letters.iter().map(dead_letter_json)),
        json_array(snapshot.replayed_events.iter().map(replay_json))
    )
}

fn task_error_json(snapshot: &TaskSnapshot) -> String {
    match (&snapshot.error_kind, &snapshot.error_message) {
        (Some(kind), Some(message)) => format!(
            "{{\"kind\":{},\"message\":{}}}",
            json_string(kind),
            json_string(message)
        ),
        _ => "null".to_owned(),
    }
}

fn task_log_json(entry: &TaskLogSnapshot) -> String {
    format!(
        "{{\"sequence\":{},\"level\":{},\"message\":{}}}",
        entry.sequence,
        json_string(&entry.level),
        json_string(&entry.message)
    )
}

fn dead_letter_json(entry: &TaskDeadLetterSnapshot) -> String {
    format!(
        "{{\"event_id\":{},\"topic\":{},\"reason_kind\":{},\"reason\":{},\"replay_count\":{}}}",
        json_string(&entry.event_id),
        json_string(&entry.topic),
        json_string(&entry.reason_kind),
        json_string(&entry.reason),
        entry.replay_count
    )
}

fn replay_json(entry: &TaskReplaySnapshot) -> String {
    format!(
        "{{\"event_id\":{},\"sequence\":{},\"topic\":{}}}",
        json_string(&entry.event_id),
        entry.sequence,
        json_string(&entry.topic)
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
    "Eva CLI\n\nUSAGE:\n  eva --version\n  eva version [--output text|json]\n  eva doctor [--project <path>] [--output text|json]\n  eva config validate [--project <path>] [--output text|json]\n  eva inspect [all|config|runtime] [--project <path>] [--output text|json]\n  eva run --example basic [--project <path>] [--task-id <id>] [--output text|json] [--timeout-ms <ms>] [--retry-attempts <n>] [--cancel] [--replay-dead-letters]\n  eva task status [--project <path>] [--task <id>] [--output text|json]\n  eva task logs [--project <path>] [--task <id>] [--output text|json]\n  eva task cancel [--project <path>] [--task <id>] [--reason <text>] [--output text|json]\n  eva adapter list [--project <path>] [--output text|json]\n  eva adapter probe [--adapter <id>|--capability <name>] [--provider <id>] [--project <path>] [--output text|json]\n  eva mcp list [--project <path>] [--output text|json]\n  eva mcp probe [--adapter <id>] [--tool <name>] [--project <path>] [--output text|json]\n  eva skill list [--project <path>] [--output text|json]\n  eva skill run [--skill <id>|--adapter <id>] [--capability <name>] [--input <json>] [--request-id <id>] [--project <path>] [--output text|json]\n  eva discovery scan [--project <path>] [--output text|json]\n  eva memory context [--agent <id>] [--query <text>] [--private-limit <n>] [--global-limit <n>] [--knowledge-limit <n>] [--project <path>] [--output text|json]\n  eva hardware list [--project <path>] [--output text|json]\n  eva hardware probe [--adapter <id>] [--project <path>] [--output text|json]\n  eva hardware bind [--adapter <id>] [--request-id <id>] [--apply] [--project <path>] [--output text|json]\n  eva backup create [--artifact-id <id>] [--request-id <id>] [--reason <text>] [--dry-run] [--project <path>] [--output text|json]\n  eva snapshot create [--snapshot-id <id>] [--release <ref>] [--role pre_release|post_release] [--project <path>] [--output text|json]\n  eva restore plan [--snapshot-id <id>] [--release <ref>] [--project <path>] [--output text|json]\n  eva upgrade check [--from-generation <id>] [--to-generation <id>] [--from-release <ref>] [--to-release <ref>] [--project <path>] [--output text|json]\n\nCommands:\n  version          Print the V1.4 release version and supported contracts.\n  doctor           Check workspace, configuration roots, schema files, and runtime boundaries.\n  config validate  Load eva.yaml plus split manifests and report stable diagnostics.\n  inspect          Show agents, adapters, capabilities, routes, policy summary, and runtime status.\n  run              Execute the V1.0-compatible in-memory basic event loop and persist the latest task report under .eva/tasks.\n  task             Inspect or cancel the latest persisted basic task report.\n  adapter          List and probe authorized Adapter handles derived from manifests.\n  mcp              List and probe allowlisted MCP tools without starting external servers.\n  skill            List and run controlled workflow skill envelopes.\n  discovery        Scan trusted configuration sources and return candidates without granting runtime handles.\n  memory           Build request-scoped private/global memory plus knowledge context for one Agent.\n  hardware         List, probe, and plan hardware bindings without opening raw I/O.\n  backup           Create and verify a V1.4 backup artifact in an in-memory ArtifactStore.\n  snapshot         Capture a release snapshot linked to a verified backup artifact.\n  restore          Produce a plan-first restore plan; no destructive mutation is executed.\n  upgrade          Check generation, migration, drain, and rollback readiness without starting processes.\n\nExit codes:\n  0 success\n  2 configuration or validation error\n  3 policy denied\n  4 runtime unavailable or unsupported in this version\n  5 external capability unavailable\n  64 command usage error\n"
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(stdout.contains("\"capability_response\""));
        assert!(stdout.contains("config.lint") || stdout.contains("valid"));
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
    fn version_text_and_json_report_v14_lifecycle() {
        let (text_exit, text_stdout, text_stderr) = run_cli(&["--version"]);
        assert_eq!(text_exit, EXIT_OK, "{text_stderr}");
        assert!(text_stdout.contains("eva 1.4.0"));
        assert!(text_stdout.contains("V1.4 backup and lifecycle planning"));

        let (json_exit, json_stdout, json_stderr) = run_cli(&["version", "--output", "json"]);
        assert_eq!(json_exit, EXIT_OK, "{json_stderr}");
        assert!(json_stdout.contains("\"command\":\"version\""));
        assert!(json_stdout.contains("\"version\":\"1.4.0\""));
        assert!(json_stdout.contains("lifecycle_v1.4"));
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
}
