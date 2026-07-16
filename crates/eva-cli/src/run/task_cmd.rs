//! 持久化任务状态、日志和取消子命令；同时支持项目本地与 durable backend 存储。

use super::{
    exit_code_for_error, json_array, json_string, option_json, parse_common_options,
    required_option, success_envelope, trace_for, write_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_core::EvaError;
use eva_observability::TraceFields;
use eva_runtime::BasicRunReport;
use eva_storage::{
    DurableBackendOptions, FileSystemDurableBackend, FileSystemTaskStateStore, TaskInputSnapshot,
    TaskStateDeadLetterSnapshot, TaskStateLogSnapshot, TaskStateReplaySnapshot, TaskStateSnapshot,
    TaskStateStore,
};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Task 子命令及其共享选项。
pub(super) enum TaskCommand {
    /// 读取任务状态快照。
    Status(
        /// 已解析的请求标识、后端路径与公共选项。
        TaskOptions,
    ),
    /// 读取任务日志、dead-letter 与 replay 记录。
    Logs(
        /// 已解析的请求标识、后端路径与公共选项。
        TaskOptions,
    ),
    /// 请求取消任务并持久化更新后的快照。
    Cancel(
        /// 已解析的请求标识、后端路径与公共选项。
        TaskOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 任务查询和取消的共享选项。
pub(super) struct TaskOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 可选任务 ID；缺省由 store 选择最新任务。
    task_id: Option<String>,
    /// 取消命令使用的可选原因。
    reason: Option<String>,
    /// 可选 durable backend 根；缺省使用项目本地任务目录。
    durable_backend: Option<PathBuf>,
}

/// 解析 `task status|logs|cancel` 和共享选项。
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

/// 执行任务读取或取消，并按错误分类返回稳定退出码。
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

/// 将基础运行报告中的任务状态写入所选 store；run 命令成功前必须完成此步骤。
pub(super) fn write_task_snapshot(
    project_root: &Path,
    durable_backend: Option<&Path>,
    report: &BasicRunReport,
) -> Result<(), EvaError> {
    let mut snapshot = TaskStateSnapshot::from(&report.task);
    let mut store = open_task_state_store(project_root, durable_backend, TaskStoreAccess::Write)?;
    match store.read(Some(&snapshot.task_id)) {
        Ok(current) => {
            snapshot.record_version = current.record_version;
            snapshot.owner_generation = current.owner_generation;
            store.compare_and_set(&snapshot).map(|_| ())
        }
        Err(error) if error.kind() == eva_core::ErrorKind::NotFound => {
            store.create(&snapshot).map(|_| ())
        }
        Err(error) => Err(error),
    }
}

/// 将基础运行报告的任务部分转换为与 task 命令相同的 JSON 契约。
pub(super) fn task_snapshot_json_from_report(report: &BasicRunReport) -> String {
    task_snapshot_json(&TaskStateSnapshot::from(&report.task))
}

/// 解析任务 ID、取消原因和后端路径，并委托公共选项解析器处理其余参数。
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

/// 以只读访问模式打开所选 store 并读取指定或最新任务。
fn read_task_snapshot(
    project_root: &Path,
    durable_backend: Option<&Path>,
    task_id: Option<&str>,
) -> Result<TaskStateSnapshot, EvaError> {
    open_task_state_store(project_root, durable_backend, TaskStoreAccess::Read)?.read(task_id)
}

/// 在最新 record version 上应用取消请求并与并发 claim/finish 做 CAS 合并。
fn cancel_task_snapshot(
    project_root: &Path,
    durable_backend: Option<&Path>,
    task_id: Option<&str>,
    reason: &str,
) -> Result<TaskStateSnapshot, EvaError> {
    let mut store = open_task_state_store(project_root, durable_backend, TaskStoreAccess::Write)?;
    let selected = store.read(task_id)?;
    store.request_cancellation(&selected.task_id, reason)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 打开 durable backend 时所需的最小访问权限。
enum TaskStoreAccess {
    /// 查询路径使用只读后端，禁止隐式迁移或写入。
    Read,
    /// 快照保存和取消路径需要读写后端。
    Write,
}

/// 根据可选 durable root 打开任务 store，并按操作选择只读或读写后端选项。
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
    match access {
        TaskStoreAccess::Read => Ok(FileSystemTaskStateStore::from_durable_layout(
            backend.layout(),
        )),
        TaskStoreAccess::Write => FileSystemTaskStateStore::from_writable_backend(&backend),
    }
}

/// 将任务错误映射为退出码并写出统一错误信封。
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

/// 输出任务状态、重试、错误和取消摘要。
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
            if let Some(envelope) = &snapshot.envelope {
                let input_kind = match &envelope.input {
                    TaskInputSnapshot::Inline { .. } => "inline",
                    TaskInputSnapshot::Artifact { .. } => "artifact",
                };
                writeln!(
                    writer,
                    "envelope: kind={} agent={} input={} digest={} idempotency_key={}",
                    envelope.kind,
                    envelope.agent_id,
                    input_kind,
                    envelope.input.digest(),
                    envelope.idempotency_key
                )
                .map_err(write_error_kind)?;
            }
            if let Some(message) = &snapshot.error_message {
                writeln!(writer, "error: {message}").map_err(write_error_kind)?;
            }
            if let Some(owner) = &snapshot.execution_owner {
                writeln!(
                    writer,
                    "execution: owner={} heartbeat_at_ms={} deadline_at_ms={}",
                    owner,
                    snapshot
                        .heartbeat_at_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "none".to_owned()),
                    snapshot
                        .deadline_at_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "none".to_owned())
                )
                .map_err(write_error_kind)?;
            }
            if let (Some(digest), Some(size_bytes)) =
                (&snapshot.result_digest, snapshot.result_size_bytes)
            {
                writeln!(writer, "result: digest={digest} size_bytes={size_bytes}")
                    .map_err(write_error_kind)?;
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

/// 输出任务日志以及 dead-letter/replay 记录。
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

/// 输出取消是否被接受及取消后的任务快照。
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

/// 将任务日志视图编码为 JSON。
fn task_logs_json(snapshot: &TaskStateSnapshot) -> String {
    format!(
        "{{\"task_id\":{},\"status\":{},\"logs\":{}}}",
        json_string(&snapshot.task_id),
        json_string(&snapshot.status),
        json_array(snapshot.logs.iter().map(task_log_json))
    )
}

/// 将完整任务状态快照编码为稳定 JSON。
fn task_snapshot_json(snapshot: &TaskStateSnapshot) -> String {
    let envelope = task_envelope_json(snapshot);
    let execution = task_execution_json(snapshot);
    let (retry_backoff_ms, attempt_timeout_ms) = snapshot
        .envelope
        .as_ref()
        .map(|envelope| {
            (
                envelope.attempt_policy.retry_backoff_ms,
                envelope.attempt_policy.attempt_timeout_ms,
            )
        })
        .unwrap_or((0, None));
    format!(
        "{{\"task_id\":{},\"task_envelope\":{},\"status\":{},\"attempts\":{},\"retry_policy\":{{\"max_attempts\":{},\"retry_backoff_ms\":{},\"attempt_timeout_ms\":{}}},\"execution\":{},\"cancellation\":{{\"requested\":{},\"accepted\":{},\"reason\":{}}},\"error\":{},\"logs\":{},\"dead_letters\":{},\"replayed_events\":{}}}",
        json_string(&snapshot.task_id),
        envelope,
        json_string(&snapshot.status),
        snapshot.attempts,
        snapshot.retry_max_attempts,
        retry_backoff_ms,
        attempt_timeout_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        execution,
        snapshot.cancel_requested,
        snapshot.cancel_accepted,
        option_json(snapshot.cancel_reason.as_deref()),
        task_error_json(snapshot),
        json_array(snapshot.logs.iter().map(task_log_json)),
        json_array(snapshot.dead_letters.iter().map(dead_letter_json)),
        json_array(snapshot.replayed_events.iter().map(replay_json))
    )
}

fn task_execution_json(snapshot: &TaskStateSnapshot) -> String {
    format!(
        "{{\"owner\":{},\"heartbeat_at_ms\":{},\"deadline_at_ms\":{},\"result_digest\":{},\"result_size_bytes\":{}}}",
        option_json(snapshot.execution_owner.as_deref()),
        snapshot
            .heartbeat_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        snapshot
            .deadline_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        option_json(snapshot.result_digest.as_deref()),
        snapshot
            .result_size_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned())
    )
}

fn task_envelope_json(snapshot: &TaskStateSnapshot) -> String {
    let Some(envelope) = &snapshot.envelope else {
        return "null".to_owned();
    };
    let input = match &envelope.input {
        TaskInputSnapshot::Inline { bytes, digest } => format!(
            "{{\"input_kind\":\"inline\",\"size_bytes\":{},\"artifact_ref\":null,\"digest\":{}}}",
            bytes.len(),
            json_string(digest)
        ),
        TaskInputSnapshot::Artifact {
            artifact_ref,
            digest,
        } => format!(
            "{{\"input_kind\":\"artifact\",\"size_bytes\":null,\"artifact_ref\":{},\"digest\":{}}}",
            json_string(artifact_ref),
            json_string(digest)
        ),
    };
    format!(
        "{{\"kind\":{},\"agent_id\":{},\"input\":{},\"idempotency_key\":{},\"attempt_policy\":{{\"max_attempts\":{},\"retry_backoff_ms\":{},\"attempt_timeout_ms\":{}}}}}",
        json_string(&envelope.kind),
        json_string(&envelope.agent_id),
        input,
        json_string(&envelope.idempotency_key),
        envelope.attempt_policy.max_attempts,
        envelope.attempt_policy.retry_backoff_ms,
        envelope
            .attempt_policy
            .attempt_timeout_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned())
    )
}

/// 将任务的可选结构化错误字段编码为 JSON 或 `null`。
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

/// 将一条任务日志记录编码为 JSON。
fn task_log_json(entry: &TaskStateLogSnapshot) -> String {
    format!(
        "{{\"sequence\":{},\"level\":{},\"message\":{}}}",
        entry.sequence,
        json_string(&entry.level),
        json_string(&entry.message)
    )
}

/// 将一条 dead-letter 快照编码为 JSON。
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

/// 将一条 dead-letter replay 结果编码为 JSON。
fn replay_json(entry: &TaskStateReplaySnapshot) -> String {
    format!(
        "{{\"event_id\":{},\"sequence\":{},\"topic\":{}}}",
        json_string(&entry.event_id),
        entry.sequence,
        json_string(&entry.topic)
    )
}
