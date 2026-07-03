//! CLI command parsing, output envelopes, and process exit mapping.

use crate::doctor::{doctor_project, CheckStatus, DoctorReport};
use crate::inspect::{inspect_project, InspectReport};
use eva_config::{load_project_config, schema_paths, ProjectConfig};
use eva_core::{ErrorKind, EvaError, InvokeStatus};
use eva_observability::{SpanId, TraceFields};
use eva_runtime::{BasicRunOptions, BasicRunReport, RuntimeBuilder, TaskLogEntry};
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
const EXIT_USAGE: i32 = 64;

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
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Doctor(CommonOptions),
    ConfigValidate(CommonOptions),
    Inspect(CommonOptions),
    Run(RunOptions),
    Task(TaskCommand),
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
struct TaskOptions {
    common: CommonOptions,
    task_id: Option<String>,
    reason: Option<String>,
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

    match args[0].as_str() {
        "help" => Ok(Command::Help),
        "doctor" => Ok(Command::Doctor(parse_common_options(&args[1..])?)),
        "config" => parse_config_command(&args[1..]),
        "inspect" => Ok(Command::Inspect(parse_inspect_options(&args[1..])?)),
        "run" => Ok(Command::Run(parse_run_options(&args[1..])?)),
        "task" => parse_task_command(&args[1..]),
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
                let runtime = RuntimeBuilder::in_memory_v05().build(&project)?;
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
            let error = EvaError::unsupported("eva run requires an example in V0.5")
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
    let mut project_root = env::current_dir().map_err(|error| {
        EvaError::internal("failed to read current directory")
            .with_context("io_error", error.to_string())
    })?;
    let mut output = OutputFormat::Text;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--project" | "--project-root" | "-p" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for project option")
                })?;
                project_root = PathBuf::from(value);
            }
            "--output" | "-o" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for output option"))?;
                output = OutputFormat::parse(value)?;
            }
            unknown => {
                return Err(EvaError::unsupported("unknown option").with_context("option", unknown));
            }
        }
        index += 1;
    }

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
        ErrorKind::Timeout | ErrorKind::Unavailable => EXIT_RUNTIME_UNAVAILABLE,
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
            "该能力在当前版本不可用；先运行 eva doctor、eva inspect 或 eva task logs 查看 V0.5 诊断。"
                .to_owned()
        }
        ErrorKind::Internal => "查看上方上下文并保留命令输出作为缺陷报告证据。".to_owned(),
    }
}

fn write_error_kind(error: io::Error) -> EvaError {
    EvaError::internal("failed to write CLI output").with_context("io_error", error.to_string())
}

fn help_text() -> &'static str {
    "Eva CLI\n\nUSAGE:\n  eva doctor [--project <path>] [--output text|json]\n  eva config validate [--project <path>] [--output text|json]\n  eva inspect [all|config|runtime] [--project <path>] [--output text|json]\n  eva run --example basic [--project <path>] [--task-id <id>] [--output text|json] [--timeout-ms <ms>] [--retry-attempts <n>] [--cancel] [--replay-dead-letters]\n  eva task status [--project <path>] [--task <id>] [--output text|json]\n  eva task logs [--project <path>] [--task <id>] [--output text|json]\n  eva task cancel [--project <path>] [--task <id>] [--reason <text>] [--output text|json]\n\nCommands:\n  doctor           Check workspace, configuration roots, schema files, and runtime boundaries.\n  config validate  Load eva.yaml plus split manifests and report stable diagnostics.\n  inspect          Show agents, adapters, capabilities, routes, policy summary, and runtime status.\n  run              Execute the V0.5 in-memory basic event loop and persist the latest task report under .eva/tasks.\n  task             Inspect or cancel the latest persisted V0.5 task report.\n\nExit codes:\n  0 success\n  2 configuration or validation error\n  3 policy denied\n  4 runtime unavailable or unsupported in this version\n  5 external capability unavailable\n  64 command usage error\n"
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
}
