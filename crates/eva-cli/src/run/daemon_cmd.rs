//! 守护进程启动与 control mailbox 子命令；集中解析路径、请求和生命周期操作。

use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::TraceFields;
use eva_runtime::{
    send_daemon_control_request, start_daemon, DaemonControlOperation, DaemonControlRequest,
    DaemonControlResponse, DaemonPathReport, DaemonPolicyReport, DaemonStartOptions,
    DaemonStartReport, DaemonStateRecord, IdempotencyKey, TaskArtifactRef, TaskAttemptPolicy,
    TaskEnvelope, TaskInput, TaskKind,
};
use eva_storage::DurableBackendReport;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Daemon 顶层命令；控制类操作共享同一传输与输出路径。
pub(super) enum DaemonCommand {
    /// 启动并验证本地 daemon 边界。
    Start(
        /// 已解析的 daemon 路径、前台模式、烟测退出标志与公共选项。
        DaemonCliOptions,
    ),
    /// 向已运行 daemon 发送一条控制请求。
    Control {
        /// daemon 路径、请求和公共 CLI 选项。
        options: DaemonCliOptions,
        /// 协议层控制操作。
        operation: DaemonControlOperation,
        /// 输出信封使用的稳定命令名。
        command: &'static str,
        /// trace 使用的稳定 span ID。
        span: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Daemon 启动和控制共享的 CLI 选项。
pub(super) struct DaemonCliOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 持久化后端根目录覆盖。
    durable_backend: Option<PathBuf>,
    /// daemon 状态目录覆盖。
    state_dir: Option<PathBuf>,
    /// daemon 独占锁目录覆盖。
    lock_dir: Option<PathBuf>,
    /// daemon pid 文件目录覆盖。
    pid_dir: Option<PathBuf>,
    /// 可观测性后端目录覆盖。
    observability_backend: Option<PathBuf>,
    /// 控制请求的可选显式 ID。
    request_id: Option<String>,
    /// submit/cancel 操作的可选任务 ID。
    task_id: Option<String>,
    /// submit 信封的 handler kind。
    task_kind: Option<String>,
    /// submit 信封或 agent control 使用的 Agent ID。
    agent_id: Option<String>,
    /// submit 信封内直接持久化的 UTF-8 输入。
    task_input: Option<String>,
    /// submit 信封引用的 artifact key。
    task_artifact_ref: Option<String>,
    /// artifact 引用声明的 canonical SHA-256。
    task_artifact_digest: Option<String>,
    /// submit 信封的稳定幂等键。
    task_idempotency_key: Option<String>,
    /// submit attempt policy 的最大尝试次数。
    task_max_attempts: Option<u32>,
    /// submit attempt policy 的固定退避毫秒数。
    task_retry_backoff_ms: Option<u64>,
    /// submit attempt policy 的可选单次超时。
    task_attempt_timeout_ms: Option<u64>,
    /// cancel 操作的可选原因。
    reason: Option<String>,
    /// drain/reload 操作的可选计划 ID。
    plan_id: Option<String>,
    /// drain/reload 操作的可选代际 ID。
    generation_id: Option<String>,
    /// 等待 control mailbox 响应的毫秒预算。
    control_timeout_ms: u64,
    /// 是否以前台模式运行 daemon。
    foreground: bool,
    /// 是否启用开发模式边界。
    dev_mode: bool,
    /// 启动烟测后是否自动关闭。
    shutdown_after_smoke: bool,
}

/// 解析 daemon start/status/stop/submit/cancel/drain/reload，并为控制操作绑定稳定协议元数据。
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

/// 执行 daemon 启动或发送控制请求。
///
/// 启动路径先加载并解析项目相对路径，再进入 `start_daemon`；控制路径先创建 trace 与请求，
/// 再等待 mailbox 响应。两条路径都不会把 transport 细节泄漏到顶层分发器。
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
                let request =
                    daemon_control_request(operation, &project, &options, request_id, &trace)?;
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

/// 解析 daemon 路径、控制 payload、超时和运行模式，并预先校验显式请求 ID。
fn parse_daemon_options(args: &[String]) -> Result<DaemonCliOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut durable_backend = None;
    let mut state_dir = None;
    let mut lock_dir = None;
    let mut pid_dir = None;
    let mut observability_backend = None;
    let mut request_id = None;
    let mut task_id = None;
    let mut task_kind = None;
    let mut agent_id = None;
    let mut task_input = None;
    let mut task_artifact_ref = None;
    let mut task_artifact_digest = None;
    let mut task_idempotency_key = None;
    let mut task_max_attempts = None;
    let mut task_retry_backoff_ms = None;
    let mut task_attempt_timeout_ms = None;
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
            "--kind" | "--task-kind" => {
                index += 1;
                task_kind = Some(required_option(args, index, "task kind option")?.clone());
            }
            "--agent" | "--agent-id" => {
                index += 1;
                agent_id = Some(required_option(args, index, "agent id option")?.clone());
            }
            "--input" | "--task-input" => {
                index += 1;
                task_input = Some(required_option(args, index, "task input option")?.clone());
            }
            "--artifact-ref" | "--input-artifact" => {
                index += 1;
                task_artifact_ref =
                    Some(required_option(args, index, "task artifact reference option")?.clone());
            }
            "--artifact-digest" | "--input-digest" => {
                index += 1;
                task_artifact_digest =
                    Some(required_option(args, index, "task artifact digest option")?.clone());
            }
            "--idempotency-key" => {
                index += 1;
                task_idempotency_key =
                    Some(required_option(args, index, "task idempotency key option")?.clone());
            }
            "--max-attempts" | "--retry-attempts" => {
                index += 1;
                task_max_attempts = Some(
                    required_option(args, index, "task max attempts option")?
                        .parse::<u32>()
                        .map_err(|_| {
                            EvaError::invalid_argument(
                                "task max attempts must be an unsigned 32-bit integer",
                            )
                        })?,
                );
            }
            "--retry-backoff-ms" => {
                index += 1;
                task_retry_backoff_ms = Some(
                    required_option(args, index, "task retry backoff option")?
                        .parse::<u64>()
                        .map_err(|_| {
                            EvaError::invalid_argument(
                                "task retry backoff must be an unsigned integer",
                            )
                        })?,
                );
            }
            "--attempt-timeout-ms" => {
                index += 1;
                task_attempt_timeout_ms = Some(
                    required_option(args, index, "task attempt timeout option")?
                        .parse::<u64>()
                        .map_err(|_| {
                            EvaError::invalid_argument(
                                "task attempt timeout must be an unsigned integer",
                            )
                        })?,
                );
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
        task_kind,
        agent_id,
        task_input,
        task_artifact_ref,
        task_artifact_digest,
        task_idempotency_key,
        task_max_attempts,
        task_retry_backoff_ms,
        task_attempt_timeout_ms,
        reason,
        plan_id,
        generation_id,
        control_timeout_ms,
        foreground,
        dev_mode,
        shutdown_after_smoke,
    })
}

/// 将 CLI 覆盖合并进项目默认 DaemonStartOptions，并最终解析为项目绝对路径。
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

/// 为每类控制操作提供稳定默认请求 ID，便于 trace 和审计关联。
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

/// 构造 control mailbox 请求，并只附加与具体操作相关的可选 payload 字段。
fn daemon_control_request(
    operation: DaemonControlOperation,
    project: &ProjectConfig,
    options: &DaemonCliOptions,
    request_id: RequestId,
    trace: &TraceFields,
) -> Result<DaemonControlRequest, EvaError> {
    let mut request = DaemonControlRequest::new(request_id.clone(), trace, operation);
    if operation == DaemonControlOperation::SubmitTask {
        let task_id = options
            .task_id
            .clone()
            .unwrap_or_else(|| request_id.as_str().to_owned());
        RequestId::parse(&task_id)?;
        let envelope = task_envelope_from_cli(project, options, &task_id)?;
        request = request.with_task_id(task_id).with_task_envelope(envelope);
    } else if let Some(value) = &options.task_id {
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
    if operation != DaemonControlOperation::SubmitTask {
        if let Some(value) = &options.agent_id {
            request = request.with_agent_id(value.clone());
        }
    }
    if operation != DaemonControlOperation::SubmitTask
        && (options.task_kind.is_some()
            || options.task_input.is_some()
            || options.task_artifact_ref.is_some()
            || options.task_artifact_digest.is_some()
            || options.task_idempotency_key.is_some()
            || options.task_max_attempts.is_some()
            || options.task_retry_backoff_ms.is_some()
            || options.task_attempt_timeout_ms.is_some())
    {
        return Err(EvaError::invalid_argument(
            "task envelope options are only valid for daemon submit",
        ));
    }
    Ok(request)
}

fn task_envelope_from_cli(
    project: &ProjectConfig,
    options: &DaemonCliOptions,
    task_id: &str,
) -> Result<TaskEnvelope, EvaError> {
    let explicit = options.task_kind.is_some()
        || options.agent_id.is_some()
        || options.task_input.is_some()
        || options.task_artifact_ref.is_some()
        || options.task_artifact_digest.is_some()
        || options.task_idempotency_key.is_some()
        || options.task_max_attempts.is_some()
        || options.task_retry_backoff_ms.is_some()
        || options.task_attempt_timeout_ms.is_some();
    if !explicit {
        let agent_id = project
            .agents
            .iter()
            .find(|agent| agent.enabled)
            .map(|agent| agent.id.clone())
            .ok_or_else(|| EvaError::not_found("daemon submit requires an enabled agent"))?;
        return TaskEnvelope::new(
            TaskKind::parse("legacy.submit")?,
            agent_id,
            TaskInput::inline(Vec::new())?,
            IdempotencyKey::parse(task_id)?,
            TaskAttemptPolicy::new(1, 0, None)?,
        );
    }

    let kind = options
        .task_kind
        .as_deref()
        .ok_or_else(|| EvaError::invalid_argument("daemon submit requires --kind"))?;
    let agent = options
        .agent_id
        .as_deref()
        .ok_or_else(|| EvaError::invalid_argument("daemon submit requires --agent"))?;
    let agent_id = configured_submit_agent(project, agent)?;
    let input = match (
        options.task_input.as_ref(),
        options.task_artifact_ref.as_ref(),
        options.task_artifact_digest.as_ref(),
    ) {
        (Some(input), None, None) => TaskInput::inline(input.as_bytes().to_vec())?,
        (None, Some(artifact_ref), Some(digest)) => {
            TaskInput::artifact(TaskArtifactRef::new(artifact_ref.clone(), digest.clone())?)
        }
        (None, Some(_), None) => {
            return Err(EvaError::invalid_argument(
                "daemon submit artifact input requires --artifact-digest",
            ))
        }
        (None, None, Some(_)) => {
            return Err(EvaError::invalid_argument(
                "daemon submit --artifact-digest requires --artifact-ref",
            ))
        }
        (Some(_), Some(_), _) | (Some(_), None, Some(_)) => {
            return Err(EvaError::invalid_argument(
                "daemon submit inline input and artifact input are mutually exclusive",
            ))
        }
        (None, None, None) => {
            return Err(EvaError::invalid_argument(
                "daemon submit requires --input or --artifact-ref/--artifact-digest",
            ))
        }
    };
    let idempotency_key = options.task_idempotency_key.as_deref().unwrap_or(task_id);
    TaskEnvelope::new(
        TaskKind::parse(kind)?,
        agent_id,
        input,
        IdempotencyKey::parse(idempotency_key)?,
        TaskAttemptPolicy::new(
            options.task_max_attempts.unwrap_or(1),
            options.task_retry_backoff_ms.unwrap_or(0),
            options.task_attempt_timeout_ms,
        )?,
    )
}

fn configured_submit_agent(project: &ProjectConfig, value: &str) -> Result<AgentId, EvaError> {
    let agent_id = AgentId::parse(value)?;
    let agent = project
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .ok_or_else(|| {
            EvaError::not_found("daemon submit references an unknown agent")
                .with_context("agent_id", value)
        })?;
    if !agent.enabled {
        return Err(
            EvaError::conflict("daemon submit references a disabled agent")
                .with_context("agent_id", value),
        );
    }
    Ok(agent_id)
}

/// 输出 daemon 启动、恢复、provider、可观测性、热插拔和维护摘要。
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
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "hardware_hotplug: status={} watcher={} devices_seen={} events_published={} raw_handles_exposed={}",
                report.hardware_hotplug.status,
                report.hardware_hotplug.watcher_kind,
                report.hardware_hotplug.devices_seen,
                report.hardware_hotplug.events_published.len(),
                report.hardware_hotplug.raw_handles_exposed
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "memory_maintenance: status={} expired_removed={} knowledge_items_indexed={}",
                report.memory_maintenance.status,
                report.memory_maintenance.memory_gc.expired_removed,
                report.memory_maintenance.knowledge_rebuild.items_indexed
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

/// 输出 control 响应，明确区分请求接受、daemon 可用和真实变更事实。
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

/// 将完整 daemon 启动报告编码为稳定 JSON。
fn daemon_start_json(report: &DaemonStartReport) -> String {
    let shutdown = report
        .shutdown
        .as_ref()
        .map(shutdown_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"status\":{},\"mode\":{},\"pid\":{},\"generation_id\":{},\"project_root\":{},\"foreground\":{},\"dev_mode\":{},\"provider_processes_started\":{},\"paths\":{},\"durable_backend\":{},\"recovery\":{},\"policy\":{},\"observability\":{},\"hardware_hotplug\":{},\"memory_maintenance\":{},\"shutdown\":{},\"audit\":{}}}",
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
        hardware_hotplug_json(&report.hardware_hotplug),
        memory_maintenance_json(&report.memory_maintenance),
        shutdown,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将 durable runtime 恢复扫描、重放和跳过统计编码为 JSON。
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

/// 将一条恢复任务证据编码为 JSON。
fn recovered_task_json(task: &eva_runtime::RecoveredTask) -> String {
    format!(
        "{{\"task_id\":{},\"previous_status\":{},\"status\":{},\"redrive_candidate\":{}}}",
        json_string(&task.task_id),
        json_string(&task.previous_status),
        json_string(&task.status),
        task.redrive_candidate
    )
}

/// 将一条恢复事件证据编码为 JSON。
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

/// 将一条跳过重放的事件及原因编码为 JSON。
fn skipped_redrive_event_json(event: &eva_runtime::SkippedRedriveEvent) -> String {
    format!(
        "{{\"task_id\":{},\"event_id\":{},\"reason\":{}}}",
        json_string(&event.task_id),
        json_string(&event.event_id),
        json_string(&event.reason)
    )
}

/// 将一个恢复后的 provider 进程状态编码为 JSON。
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

/// 将 provider backoff 队列任务编码为 JSON。
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

/// 将恢复时跳过的 provider 任务及原因编码为 JSON。
fn skipped_provider_task_json(task: &eva_runtime::SkippedProviderTask) -> String {
    format!(
        "{{\"task_id\":{},\"session_id\":{},\"reason\":{}}}",
        json_string(&task.task_id),
        json_string(&task.session_id),
        json_string(&task.reason)
    )
}

/// 将 daemon control 响应及可选操作证据编码为 JSON。
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

/// 将 daemon 解析后的状态、锁、pid 和 control 路径编码为 JSON。
fn daemon_paths_json(paths: &DaemonPathReport) -> String {
    format!(
        "{{\"durable_backend_root\":{},\"observability_backend_root\":{},\"state_dir\":{},\"lock_dir\":{},\"pid_dir\":{},\"control_request_dir\":{},\"control_response_dir\":{},\"state_file\":{},\"hardware_hotplug_state_file\":{},\"lock_file\":{},\"pid_file\":{}}}",
        json_string(&paths.durable_backend_root),
        json_string(&paths.observability_backend_root),
        json_string(&paths.state_dir),
        json_string(&paths.lock_dir),
        json_string(&paths.pid_dir),
        json_string(&paths.control_request_dir),
        json_string(&paths.control_response_dir),
        json_string(&paths.state_file),
        json_string(&paths.hardware_hotplug_state_file),
        json_string(&paths.lock_file),
        json_string(&paths.pid_file)
    )
}

/// 将 daemon 硬件热插拔订阅报告编码为 JSON。
fn hardware_hotplug_json(report: &eva_hardware::HardwareHotplugSubscriberReport) -> String {
    format!(
        "{{\"status\":{},\"watcher_kind\":{},\"devices_seen\":{},\"events_published\":{},\"state\":{},\"raw_handles_exposed\":{},\"audit\":{}}}",
        json_string(&report.status),
        json_string(&report.watcher_kind),
        report.devices_seen,
        json_array(report.events_published.iter().map(hotplug_event_json)),
        json_array(report.state.iter().map(hotplug_state_json)),
        report.raw_handles_exposed,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将 daemon 记忆清理与知识重建报告编码为 JSON。
fn memory_maintenance_json(report: &eva_runtime::DaemonMemoryMaintenanceReport) -> String {
    format!(
        "{{\"status\":{},\"memory_gc\":{},\"knowledge_rebuild\":{},\"audit\":{}}}",
        json_string(&report.status),
        memory_gc_json(&report.memory_gc),
        knowledge_rebuild_json(&report.knowledge_rebuild),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将记忆压缩/过期清理统计编码为 JSON。
fn memory_gc_json(report: &eva_memory::MemoryCompactionReport) -> String {
    format!(
        "{{\"status\":{},\"lock_path\":{},\"checkpoint_path\":{},\"scanned_records\":{},\"records_kept\":{},\"expired_removed\":{},\"recovered_checkpoint\":{},\"audit\":{}}}",
        json_string(&report.status),
        json_string(&report.lock_path),
        json_string(&report.checkpoint_path),
        report.scanned_records,
        report.records_kept,
        report.expired_removed,
        report.recovered_checkpoint,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将知识索引重建 checkpoint 统计编码为 JSON。
fn knowledge_rebuild_json(report: &eva_memory::KnowledgeRebuildCheckpointReport) -> String {
    format!(
        "{{\"status\":{},\"lock_path\":{},\"checkpoint_path\":{},\"items_indexed\":{},\"recovered_checkpoint\":{},\"audit\":{}}}",
        json_string(&report.status),
        json_string(&report.lock_path),
        json_string(&report.checkpoint_path),
        report.items_indexed,
        report.recovered_checkpoint,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将单个热插拔发布回执编码为 JSON。
fn hotplug_event_json(report: &eva_hardware::HotplugPublishReport) -> String {
    format!(
        "{{\"event_id\":{},\"sequence\":{},\"topic\":{},\"device_id\":{},\"action\":{},\"previous\":{},\"next\":{},\"reason\":{}}}",
        json_string(report.receipt.event_id.as_str()),
        report.receipt.sequence,
        json_string(report.receipt.topic.as_str()),
        json_string(report.event.device_id.as_str()),
        json_string(report.event.action.as_str()),
        json_string(report.event.previous.as_str()),
        json_string(report.event.next.as_str()),
        json_string(&report.event.reason)
    )
}

/// 将热插拔设备状态快照编码为 JSON。
fn hotplug_state_json(state: &eva_hardware::HardwareHotplugDeviceState) -> String {
    format!(
        "{{\"device_id\":{},\"bus\":{},\"health\":{},\"source_path\":{}}}",
        json_string(state.device_id.as_str()),
        json_string(&state.bus),
        json_string(state.health.as_str()),
        json_string(&state.source_path)
    )
}

/// 将 daemon 启动策略门禁结果编码为 JSON。
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

/// 将 durable backend 布局和迁移状态编码为 JSON。
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

/// 将 daemon 可观测性 pipeline 摘要编码为 JSON。
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

/// 将 daemon 持久化状态记录编码为 JSON。
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

/// 将 daemon 关闭步骤、风险和审计证据编码为 JSON。
fn shutdown_json(report: &eva_runtime::ShutdownReport) -> String {
    format!(
        "{{\"already_shutdown\":{},\"request_count\":{},\"phase\":{}}}",
        report.already_shutdown,
        report.request_count,
        json_string(&report.phase)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn daemon_submit_uses_envelope_as_the_only_agent_identity() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let project = load_project_config(&project_root).unwrap();
        let args = [
            "--task",
            "req-daemon-cli-agent-identity",
            "--kind",
            "runtime.echo",
            "--agent",
            "root-agent",
            "--input",
            "identity-safe",
        ]
        .map(str::to_owned);
        let options = parse_daemon_options(&args).unwrap();
        let request_id = RequestId::parse("req-daemon-cli-agent-request").unwrap();
        let trace = TraceFields::default().with_request_id(request_id.clone());

        let request = daemon_control_request(
            DaemonControlOperation::SubmitTask,
            &project,
            &options,
            request_id,
            &trace,
        )
        .unwrap();

        assert!(request.agent_id.is_none());
        assert_eq!(
            request.task_envelope.unwrap().agent_id().as_str(),
            "root-agent"
        );
    }
}
