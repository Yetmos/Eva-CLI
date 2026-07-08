use super::{
    exit_code_for_error, json_array, json_string, option_json, parse_common_options,
    required_option, success_envelope, trace_for, write_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_core::EvaError;
use eva_observability::TraceFields;
use eva_runtime::BasicRunReport;
use eva_storage::{
    DurableBackendOptions, FileSystemDurableBackend, FileSystemTaskStateStore,
    TaskStateDeadLetterSnapshot, TaskStateLogSnapshot, TaskStateReplaySnapshot, TaskStateSnapshot,
    TaskStateStore,
};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TaskCommand {
    Status(TaskOptions),
    Logs(TaskOptions),
    Cancel(TaskOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskOptions {
    common: CommonOptions,
    task_id: Option<String>,
    reason: Option<String>,
    durable_backend: Option<PathBuf>,
}

pub(super) fn parse_task_command(args: &[String]) -> Result<TaskCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing task subcommand"))?;
    let options = parse_task_options(rest)?;
    match subcommand.as_str() {
        "status" => Ok(TaskCommand::Status(options)),
        "logs" => Ok(TaskCommand::Logs(options)),
        "cancel" => Ok(TaskCommand::Cancel(options)),
        value => {
            Err(EvaError::unsupported("unknown task subcommand").with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_task<W, E>(
    command: TaskCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        TaskCommand::Status(options) => {
            let trace = trace_for("cli.task.status");
            match read_task_snapshot(
                &options.common.project_root,
                options.durable_backend.as_deref(),
                options.task_id.as_deref(),
            ) {
                Ok(snapshot) => {
                    write_task_status(stdout, options.common.output, &snapshot, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_task_error(stderr, options.common.output, "task.status", &error, &trace)
                }
            }
        }
        TaskCommand::Logs(options) => {
            let trace = trace_for("cli.task.logs");
            match read_task_snapshot(
                &options.common.project_root,
                options.durable_backend.as_deref(),
                options.task_id.as_deref(),
            ) {
                Ok(snapshot) => {
                    write_task_logs(stdout, options.common.output, &snapshot, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_task_error(stderr, options.common.output, "task.logs", &error, &trace)
                }
            }
        }
        TaskCommand::Cancel(options) => {
            let trace = trace_for("cli.task.cancel");
            match cancel_task_snapshot(
                &options.common.project_root,
                options.durable_backend.as_deref(),
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
                    write_task_error(stderr, options.common.output, "task.cancel", &error, &trace)
                }
            }
        }
    }
}

pub(super) fn write_task_snapshot(
    project_root: &Path,
    durable_backend: Option<&Path>,
    report: &BasicRunReport,
) -> Result<(), EvaError> {
    let snapshot = TaskStateSnapshot::from(&report.task);
    let mut store = open_task_state_store(project_root, durable_backend, TaskStoreAccess::Write)?;
    store.write(&snapshot)
}

pub(super) fn task_snapshot_json_from_report(report: &BasicRunReport) -> String {
    task_snapshot_json(&TaskStateSnapshot::from(&report.task))
}

fn parse_task_options(args: &[String]) -> Result<TaskOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut task_id = None;
    let mut reason = None;
    let mut durable_backend = None;
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
            "--durable-backend" | "--durable-backend-root" => {
                index += 1;
                durable_backend = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "durable backend option",
                )?));
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(TaskOptions {
        common: parse_common_options(&passthrough)?,
        task_id,
        reason,
        durable_backend,
    })
}

fn read_task_snapshot(
    project_root: &Path,
    durable_backend: Option<&Path>,
    task_id: Option<&str>,
) -> Result<TaskStateSnapshot, EvaError> {
    open_task_state_store(project_root, durable_backend, TaskStoreAccess::Read)?.read(task_id)
}

fn cancel_task_snapshot(
    project_root: &Path,
    durable_backend: Option<&Path>,
    task_id: Option<&str>,
    reason: &str,
) -> Result<TaskStateSnapshot, EvaError> {
    let mut snapshot = read_task_snapshot(project_root, durable_backend, task_id)?;
    snapshot.cancel_requested = true;
    snapshot.cancel_reason = Some(reason.to_owned());
    if snapshot.is_terminal() {
        snapshot.cancel_accepted = false;
        snapshot.push_log(
            "warning",
            "cancel requested after task reached a terminal state",
        );
    } else {
        snapshot.cancel_accepted = true;
        snapshot.status = "cancelled".to_owned();
        snapshot.push_log("warning", format!("cancel accepted: {reason}"));
    }

    let mut store = open_task_state_store(project_root, durable_backend, TaskStoreAccess::Write)?;
    store.write(&snapshot)?;
    Ok(snapshot)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskStoreAccess {
    Read,
    Write,
}

fn open_task_state_store(
    project_root: &Path,
    durable_backend: Option<&Path>,
    access: TaskStoreAccess,
) -> Result<FileSystemTaskStateStore, EvaError> {
    let Some(root) = durable_backend else {
        return Ok(FileSystemTaskStateStore::new(project_root));
    };

    let options = match access {
        TaskStoreAccess::Read => DurableBackendOptions::read_only(root),
        TaskStoreAccess::Write => DurableBackendOptions::read_write(root),
    };
    let backend = FileSystemDurableBackend::open(options)?;
    Ok(FileSystemTaskStateStore::from_durable_layout(
        backend.layout(),
    ))
}

fn write_task_error<W: Write>(
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

fn write_task_status<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    snapshot: &TaskStateSnapshot,
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
    snapshot: &TaskStateSnapshot,
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
    snapshot: &TaskStateSnapshot,
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

fn task_logs_json(snapshot: &TaskStateSnapshot) -> String {
    format!(
        "{{\"task_id\":{},\"status\":{},\"logs\":{}}}",
        json_string(&snapshot.task_id),
        json_string(&snapshot.status),
        json_array(snapshot.logs.iter().map(task_log_json))
    )
}

fn task_snapshot_json(snapshot: &TaskStateSnapshot) -> String {
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

fn task_error_json(snapshot: &TaskStateSnapshot) -> String {
    match (&snapshot.error_kind, &snapshot.error_message) {
        (Some(kind), Some(message)) => format!(
            "{{\"kind\":{},\"message\":{}}}",
            json_string(kind),
            json_string(message)
        ),
        _ => "null".to_owned(),
    }
}

fn task_log_json(entry: &TaskStateLogSnapshot) -> String {
    format!(
        "{{\"sequence\":{},\"level\":{},\"message\":{}}}",
        entry.sequence,
        json_string(&entry.level),
        json_string(&entry.message)
    )
}

fn dead_letter_json(entry: &TaskStateDeadLetterSnapshot) -> String {
    format!(
        "{{\"event_id\":{},\"topic\":{},\"reason_kind\":{},\"reason\":{},\"replay_count\":{}}}",
        json_string(&entry.event_id),
        json_string(&entry.topic),
        json_string(&entry.reason_kind),
        json_string(&entry.reason),
        entry.replay_count
    )
}

fn replay_json(entry: &TaskStateReplaySnapshot) -> String {
    format!(
        "{{\"event_id\":{},\"sequence\":{},\"topic\":{}}}",
        json_string(&entry.event_id),
        entry.sequence,
        json_string(&entry.topic)
    )
}
