//! 守护进程启动与 control mailbox 子命令；集中解析路径、请求和生命周期操作。

use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{AgentId, EvaError, RequestId};
use eva_lifecycle::{
    run_service_entrypoint, ServiceHostPlatform, ServiceManagerEntryPoint, ServiceManagerKind,
};
use eva_observability::TraceFields;
use eva_runtime::{
    cleanup_failed_daemon_start, daemon_status, read_daemon_startup_frame,
    read_daemon_startup_report, request_daemon_startup_abort, send_daemon_control_request,
    start_daemon, start_daemon_background_child, start_daemon_service_with_stop_token_and_ready,
    write_daemon_startup_report, DaemonControlOperation, DaemonControlRequest,
    DaemonControlResponse, DaemonLeaseReport, DaemonPathReport, DaemonPolicyReport,
    DaemonStartOptions, DaemonStartReport, DaemonStartupFrame, DaemonStartupHandshake,
    DaemonStartupPhase, DaemonStateRecord, IdempotencyKey, TaskArtifactRef, TaskAttemptPolicy,
    TaskEnvelope, TaskInput, TaskKind, MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS,
};
use eva_storage::DurableBackendReport;
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BACKGROUND_CHILD_ENV: &str = "EVA_DAEMON_BACKGROUND_CHILD";
const DEFAULT_BACKGROUND_STARTUP_TIMEOUT_MS: u64 = 15_000;
const BACKGROUND_ABORT_GRACE: Duration = Duration::from_secs(2);
const BACKGROUND_POLL_INTERVAL: Duration = Duration::from_millis(20);
const STARTUP_HANDSHAKE_PROTOCOL: &str = "eva.daemon-startup.v1";
#[cfg(debug_assertions)]
const BACKGROUND_REPORT_DELAY_ENV: &str = "EVA_DAEMON_TEST_REPORT_DELAY_MS";
#[cfg(debug_assertions)]
const BACKGROUND_READY_VALIDATION_DELAY_ENV: &str = "EVA_DAEMON_TEST_READY_VALIDATION_DELAY_MS";
#[cfg(debug_assertions)]
const BACKGROUND_CHILD_START_DELAY_ENV: &str = "EVA_DAEMON_TEST_BACKGROUND_CHILD_START_DELAY_MS";
#[cfg(debug_assertions)]
const BACKGROUND_TERMINAL_POLL_DELAY_ENV: &str = "EVA_DAEMON_TEST_TERMINAL_POLL_DELAY_MS";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Daemon 顶层命令；控制类操作共享同一传输与输出路径。
pub(super) enum DaemonCommand {
    /// 启动并验证本地 daemon 边界。
    Start(
        /// 已解析的 daemon 路径、前台模式、烟测退出标志与公共选项。
        DaemonCliOptions,
    ),
    /// Internal child entrypoint used by `daemon start --background`; never shown in help.
    BackgroundChild {
        options: DaemonCliOptions,
        handshake: DaemonStartupHandshake,
    },
    /// Direct service-manager entrypoint; unlike BackgroundChild it never
    /// creates a launcher or secondary Eva process.
    ServiceEntry {
        options: DaemonCliOptions,
        service_name: String,
        service_kind: String,
        service_identity: String,
    },
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
    /// daemon shutdown 内部等待、取消和强制收口的总毫秒预算。
    drain_timeout_ms: Option<u64>,
    /// 等待 background child ready frame 的独立毫秒预算。
    startup_timeout_ms: u64,
    /// 是否以前台模式运行 daemon。
    foreground: bool,
    /// 是否启用开发模式边界。
    dev_mode: bool,
    /// 启动烟测后是否自动关闭。
    shutdown_after_smoke: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonBackgroundStartReport {
    start_json: String,
    status: String,
    mode: String,
    pid: u32,
    generation_id: String,
    project_root: String,
    dev_mode: bool,
    paths: DaemonPathReport,
    lease: DaemonLeaseReport,
    handshake: DaemonStartupFrame,
    startup_timeout_ms: u64,
    startup_elapsed_ms: u128,
}

enum DaemonStartResult {
    Foreground(Box<DaemonStartReport>),
    Background(Box<DaemonBackgroundStartReport>),
}

/// 解析 daemon start/status/stop/submit/cancel/drain/reload，并为控制操作绑定稳定协议元数据。
pub(super) fn parse_daemon_command(args: &[String]) -> Result<DaemonCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing daemon subcommand"))?;
    match subcommand.as_str() {
        "start" => Ok(DaemonCommand::Start(parse_daemon_options(rest)?)),
        "__background-child" => {
            if env::var_os(BACKGROUND_CHILD_ENV).is_none() {
                return Err(EvaError::unsupported(
                    "daemon background child entrypoint is internal",
                ));
            }
            parse_background_child_command(rest)
        }
        "__service-entry" => parse_service_entry_command(rest),
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

fn parse_service_entry_command(args: &[String]) -> Result<DaemonCommand, EvaError> {
    const CANONICAL_OPTIONS: [&str; 4] = [
        "--project",
        "--service-name",
        "--service-kind",
        "--service-identity",
    ];
    if args.len() != CANONICAL_OPTIONS.len() * 2
        || CANONICAL_OPTIONS
            .iter()
            .enumerate()
            .any(|(index, expected)| args[index * 2] != *expected)
    {
        return Err(EvaError::invalid_argument(
            "service entrypoint requires canonical --project, --service-name, --service-kind, and --service-identity argv",
        ));
    }
    Ok(DaemonCommand::ServiceEntry {
        options: parse_daemon_options(&args[..2])?,
        service_name: args[3].clone(),
        service_kind: args[5].clone(),
        service_identity: args[7].clone(),
    })
}

fn parse_background_child_command(args: &[String]) -> Result<DaemonCommand, EvaError> {
    let mut nonce = None;
    let mut launcher_pid = None;
    let mut child_start_token = None;
    let mut daemon_args = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--startup-nonce" => {
                if nonce.is_some() {
                    return Err(EvaError::invalid_argument(
                        "daemon startup nonce option must not be repeated",
                    ));
                }
                index += 1;
                nonce = Some(required_option(args, index, "startup nonce option")?.clone());
            }
            "--launcher-pid" => {
                if launcher_pid.is_some() {
                    return Err(EvaError::invalid_argument(
                        "daemon launcher pid option must not be repeated",
                    ));
                }
                index += 1;
                launcher_pid = Some(
                    required_option(args, index, "launcher pid option")?
                        .parse::<u32>()
                        .map_err(|_| {
                            EvaError::invalid_argument(
                                "daemon launcher pid must be an unsigned 32-bit integer",
                            )
                        })?,
                );
            }
            "--child-start-token" => {
                if child_start_token.is_some() {
                    return Err(EvaError::invalid_argument(
                        "daemon child start token option must not be repeated",
                    ));
                }
                index += 1;
                child_start_token =
                    Some(required_option(args, index, "child start token option")?.clone());
            }
            _ => daemon_args.push(args[index].clone()),
        }
        index += 1;
    }
    let handshake = DaemonStartupHandshake::new(
        nonce.ok_or_else(|| EvaError::invalid_argument("missing daemon startup nonce"))?,
        launcher_pid.ok_or_else(|| EvaError::invalid_argument("missing daemon launcher pid"))?,
        child_start_token
            .ok_or_else(|| EvaError::invalid_argument("missing daemon child start token"))?,
    )?;
    Ok(DaemonCommand::BackgroundChild {
        options: parse_daemon_options(&daemon_args)?,
        handshake,
    })
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
                daemon_options_from_cli(&project, &options).and_then(|daemon_options| {
                    if daemon_options.foreground {
                        start_daemon(&project, daemon_options, &trace)
                            .map(|report| DaemonStartResult::Foreground(Box::new(report)))
                    } else {
                        start_daemon_background(
                            &project,
                            &daemon_options,
                            options.startup_timeout_ms,
                        )
                        .map(|report| DaemonStartResult::Background(Box::new(report)))
                    }
                })
            }) {
                Ok(DaemonStartResult::Foreground(report)) => {
                    write_daemon_start(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Ok(DaemonStartResult::Background(report)) => {
                    write_daemon_background_start(stdout, options.common.output, &report, &trace)?;
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
        DaemonCommand::BackgroundChild { options, handshake } => {
            delay_background_child_start_for_test()?;
            let trace = trace_for("cli.daemon.background_child")
                .with_request_id(RequestId::parse("req-daemon-background-child")?);
            load_project_config(&options.common.project_root).and_then(|project| {
                daemon_options_from_cli(&project, &options).and_then(|mut daemon_options| {
                    daemon_options.foreground = true;
                    daemon_options.shutdown_after_smoke = false;
                    let report_options = daemon_options.clone();
                    let mut publish_report = |report: &DaemonStartReport| {
                        delay_background_report_for_test()?;
                        write_daemon_startup_report(
                            &report_options,
                            &handshake,
                            &daemon_start_json(report),
                        )
                    };
                    start_daemon_background_child(
                        &project,
                        daemon_options,
                        &trace,
                        &handshake,
                        &mut publish_report,
                    )
                    .map(|_| EXIT_OK)
                })
            })
        }
        DaemonCommand::ServiceEntry {
            options,
            service_name,
            service_kind,
            service_identity,
        } => {
            let trace = trace_for("cli.daemon.service_entry")
                .with_request_id(RequestId::parse("req-daemon-service-entry")?);
            let output = options.common.output;
            let kind = ServiceManagerKind::parse(&service_kind)?;
            let handler_service_name = service_name.clone();
            let handler_trace = trace.clone();
            let result = run_service_entrypoint(kind, &service_name, move |context| {
                let project = load_project_config(&options.common.project_root)?;
                validate_service_entrypoint(
                    &project,
                    &handler_service_name,
                    &service_kind,
                    &service_identity,
                    options.dev_mode,
                )?;
                env::set_current_dir(&project.project_root).map_err(|error| {
                    EvaError::unavailable(
                        "failed to enter the service entrypoint working directory",
                    )
                    .with_context("path", project.project_root.display().to_string())
                    .with_context("io_error", error.to_string())
                })?;
                let daemon_options = daemon_options_from_cli(&project, &options)?;
                let stop_token = context.stop_token().clone();
                let ready_context = context.clone();
                let mut report_ready = move || ready_context.report_ready();
                start_daemon_service_with_stop_token_and_ready(
                    &project,
                    daemon_options,
                    &handler_trace,
                    &stop_token,
                    &mut report_ready,
                )
                .map(|_| ())
            });
            match result {
                Ok(()) => Ok(EXIT_OK),
                Err(error) => {
                    write_command_error(stderr, output, "daemon.service_entry", &error, &trace)
                }
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

fn validate_service_entrypoint(
    project: &ProjectConfig,
    service_name: &str,
    service_kind: &str,
    service_identity: &str,
    dev_mode: bool,
) -> Result<(), EvaError> {
    if dev_mode {
        return Err(EvaError::permission_denied(
            "service daemon entrypoint cannot run in development mode",
        ));
    }
    let configured = project.eva.service_manager.as_ref().ok_or_else(|| {
        EvaError::not_found("service entrypoint requires service_manager configuration")
    })?;
    if !configured.enabled {
        return Err(EvaError::permission_denied(
            "service entrypoint requires an enabled service manager",
        ));
    }
    let kind = ServiceManagerKind::parse(service_kind)?;
    if !kind.production_adapter() || configured.kind != kind {
        return Err(EvaError::unsupported(
            "service entrypoint kind does not match the configured production adapter",
        )
        .with_context("configured_kind", configured.kind.as_str())
        .with_context("requested_kind", kind.as_str()));
    }
    if ServiceHostPlatform::current().service_manager_kind() != Some(kind) {
        return Err(EvaError::unsupported(
            "service entrypoint kind does not match the current host platform",
        )
        .with_context("host_platform", ServiceHostPlatform::current().as_str())
        .with_context("requested_kind", kind.as_str()));
    }
    if configured.service_name != service_name {
        return Err(EvaError::conflict(
            "service entrypoint service name does not match configuration",
        )
        .with_context("configured_service_name", &configured.service_name)
        .with_context("requested_service_name", service_name));
    }
    let executable = env::current_exe().map_err(|error| {
        EvaError::internal("failed to resolve service entrypoint executable")
            .with_context("io_error", error.to_string())
    })?;
    let executable = std::fs::canonicalize(&executable).map_err(|error| {
        EvaError::internal("failed to canonicalize service entrypoint executable")
            .with_context("path", executable.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    if let Some(configured_binary) = configured.runtime_binary.as_ref() {
        let configured_binary = if configured_binary.is_absolute() {
            configured_binary.clone()
        } else {
            project.project_root.join(configured_binary)
        };
        let configured_binary = std::fs::canonicalize(&configured_binary).map_err(|error| {
            EvaError::unavailable("failed to canonicalize configured service runtime binary")
                .with_context("path", configured_binary.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        if configured_binary != executable {
            return Err(EvaError::conflict(
                "service entrypoint executable does not match configured runtime binary",
            )
            .with_context("configured_binary", configured_binary.display().to_string())
            .with_context("running_executable", executable.display().to_string()));
        }
    }
    let entrypoint = ServiceManagerEntryPoint::for_daemon(
        executable,
        project.project_root.clone(),
        service_name,
        kind,
    )?;
    if service_identity != entrypoint.identity_digest() {
        return Err(
            EvaError::conflict("service entrypoint identity digest does not match argv")
                .with_context("expected_digest", entrypoint.identity_digest())
                .with_context("provided_digest", service_identity),
        );
    }
    entrypoint.validate()
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
    let mut drain_timeout_ms = None;
    let mut startup_timeout_ms = DEFAULT_BACKGROUND_STARTUP_TIMEOUT_MS;
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
            "--drain-timeout-ms" => {
                index += 1;
                let value = required_option(args, index, "shutdown drain timeout option")?
                    .parse::<u64>()
                    .map_err(|_| {
                        EvaError::invalid_argument("shutdown drain timeout must be an integer")
                    })?;
                if !(2..=MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS).contains(&value) {
                    return Err(EvaError::invalid_argument(
                        "shutdown drain timeout is outside the live lease budget",
                    )
                    .with_context("minimum_ms", "2")
                    .with_context(
                        "maximum_ms",
                        MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS.to_string(),
                    ));
                }
                drain_timeout_ms = Some(value);
            }
            "--startup-timeout-ms" => {
                index += 1;
                startup_timeout_ms = required_option(args, index, "startup timeout option")?
                    .parse::<u64>()
                    .map_err(|_| {
                        EvaError::invalid_argument("startup timeout must be an integer")
                    })?;
                if startup_timeout_ms == 0 {
                    return Err(EvaError::invalid_argument(
                        "startup timeout must be greater than zero",
                    ));
                }
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
        drain_timeout_ms,
        startup_timeout_ms,
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
    options.shutdown_after_smoke = cli.foreground && cli.shutdown_after_smoke;
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
    if let Some(timeout_ms) = options.drain_timeout_ms {
        if operation != DaemonControlOperation::Shutdown {
            return Err(EvaError::invalid_argument(
                "shutdown drain timeout is only valid for daemon shutdown or stop",
            ));
        }
        request = request.with_timeout_ms(timeout_ms);
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

fn start_daemon_background(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    startup_timeout_ms: u64,
) -> Result<DaemonBackgroundStartReport, EvaError> {
    let handshake = new_background_handshake()?;
    let started = Instant::now();
    let timeout = Duration::from_millis(startup_timeout_ms);
    let deadline = started.checked_add(timeout).ok_or_else(|| {
        EvaError::invalid_argument("background daemon startup timeout is too large")
    })?;
    let mut child = spawn_background_child(project, options, &handshake)?;
    let expected_pid = child.id();
    loop {
        let terminal = match read_background_terminal_frame(options, &handshake) {
            Ok(terminal) => terminal,
            Err(error) => {
                return Err(abort_background_start(
                    options, &handshake, &mut child, error,
                ));
            }
        };
        match terminal {
            BackgroundTerminalFrame::Pending => {}
            BackgroundTerminalFrame::Failed(frame) => {
                let error = daemon_startup_failure(&frame);
                return Err(abort_background_start(
                    options, &handshake, &mut child, error,
                ));
            }
            BackgroundTerminalFrame::Ready(frame) => {
                match complete_background_ready(
                    options,
                    &handshake,
                    &mut child,
                    frame,
                    startup_timeout_ms,
                    started.elapsed().as_millis(),
                ) {
                    Ok(report) => return Ok(report),
                    Err(error) => {
                        return Err(abort_background_start(
                            options, &handshake, &mut child, error,
                        ));
                    }
                }
            }
        }

        if let Err(error) = delay_terminal_poll_for_test() {
            return Err(abort_background_start(
                options, &handshake, &mut child, error,
            ));
        }
        if let Some(exit) = child.try_wait().map_err(|error| {
            EvaError::internal("failed to inspect background daemon child")
                .with_context("io_error", error.to_string())
        })? {
            let failure = match read_background_terminal_frame(options, &handshake) {
                Ok(BackgroundTerminalFrame::Failed(frame)) => daemon_startup_failure(&frame),
                Ok(BackgroundTerminalFrame::Ready(frame)) => {
                    match complete_background_ready(
                        options,
                        &handshake,
                        &mut child,
                        frame,
                        startup_timeout_ms,
                        started.elapsed().as_millis(),
                    ) {
                        Ok(_) => EvaError::conflict(
                            "background daemon exited after publishing its ready frame",
                        ),
                        Err(error) => error,
                    }
                }
                Ok(BackgroundTerminalFrame::Pending) => {
                    EvaError::internal("background daemon exited before ready handshake")
                }
                Err(error) => error,
            };
            return Err(finish_failed_background_start(
                options,
                &handshake,
                expected_pid,
                failure,
                exit,
            ));
        }
        if Instant::now() >= deadline {
            return Err(abort_background_start(
                options,
                &handshake,
                &mut child,
                EvaError::timeout("background daemon ready handshake timed out")
                    .with_context("startup_timeout_ms", startup_timeout_ms.to_string()),
            ));
        }
        std::thread::sleep(BACKGROUND_POLL_INTERVAL);
    }
}

enum BackgroundTerminalFrame {
    Pending,
    Failed(DaemonStartupFrame),
    Ready(DaemonStartupFrame),
}

fn read_background_terminal_frame(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
) -> Result<BackgroundTerminalFrame, EvaError> {
    let failed = read_daemon_startup_frame(options, handshake, DaemonStartupPhase::Failed)
        .map_err(|error| {
            EvaError::conflict("background daemon failure frame is invalid")
                .with_context("frame_error", error.to_string())
        })?;
    let ready = read_daemon_startup_frame(options, handshake, DaemonStartupPhase::Ready).map_err(
        |error| {
            EvaError::conflict("background daemon ready frame is invalid")
                .with_context("frame_error", error.to_string())
        },
    )?;
    match (failed, ready) {
        (Some(_), Some(_)) => Err(EvaError::conflict(
            "background daemon published both ready and failed startup frames",
        )),
        (Some(frame), None) => Ok(BackgroundTerminalFrame::Failed(frame)),
        (None, Some(frame)) => Ok(BackgroundTerminalFrame::Ready(frame)),
        (None, None) => Ok(BackgroundTerminalFrame::Pending),
    }
}

fn spawn_background_child(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
) -> Result<Child, EvaError> {
    let executable = env::current_exe().map_err(|error| {
        EvaError::internal("failed to resolve Eva executable for background daemon")
            .with_context("io_error", error.to_string())
    })?;
    let mut command = Command::new(executable);
    command
        .arg("daemon")
        .arg("__background-child")
        .arg("--startup-nonce")
        .arg(handshake.nonce())
        .arg("--launcher-pid")
        .arg(handshake.launcher_pid().to_string())
        .arg("--child-start-token")
        .arg(handshake.child_start_token())
        .arg("--foreground")
        .arg("--no-shutdown-after-smoke")
        .arg("--project")
        .arg(&project.project_root)
        .arg("--durable-backend")
        .arg(&options.durable_backend)
        .arg("--state-dir")
        .arg(&options.state_dir)
        .arg("--lock-dir")
        .arg(&options.lock_dir)
        .arg("--pid-dir")
        .arg(&options.pid_dir)
        .arg("--observability-backend")
        .arg(&options.observability_backend)
        .env(BACKGROUND_CHILD_ENV, "1")
        .current_dir(&project.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if options.dev_mode {
        command.arg("--dev");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    spawn_background_process(&mut command).map_err(|error| {
        EvaError::internal("failed to spawn background daemon child")
            .with_context("io_error", error.to_string())
    })
}

#[cfg(not(windows))]
fn spawn_background_process(command: &mut Command) -> io::Result<Child> {
    command.spawn()
}

#[cfg(windows)]
fn spawn_background_process(command: &mut Command) -> io::Result<Child> {
    use std::sync::{Mutex, OnceLock};

    static SPAWN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _spawn_lock = SPAWN_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inheritance_guard = WindowsStandardHandleInheritanceGuard::clear()?;
    command.spawn()
}

#[cfg(windows)]
struct WindowsStandardHandleInheritanceGuard {
    handles: Vec<std::os::windows::io::RawHandle>,
}

#[cfg(windows)]
impl WindowsStandardHandleInheritanceGuard {
    fn clear() -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;

        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        let candidates = [
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ];
        let mut guard = Self {
            handles: Vec::new(),
        };
        for handle in candidates {
            if handle.is_null()
                || handle == (-1_isize) as std::os::windows::io::RawHandle
                || guard.handles.contains(&handle)
            {
                continue;
            }
            let mut flags = 0;
            // The handle comes from this process's live stdout/stderr objects.
            if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if flags & HANDLE_FLAG_INHERIT == 0 {
                continue;
            }
            // The spawn mutex keeps the process-wide flag stable until CreateProcess returns.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            guard.handles.push(handle);
        }
        Ok(guard)
    }
}

#[cfg(windows)]
impl Drop for WindowsStandardHandleInheritanceGuard {
    fn drop(&mut self) {
        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        for handle in self.handles.drain(..) {
            // Every stored handle was successfully queried and cleared by `clear` above.
            let _ =
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetHandleInformation(handle: std::os::windows::io::RawHandle, flags: *mut u32) -> i32;
    fn SetHandleInformation(handle: std::os::windows::io::RawHandle, mask: u32, flags: u32) -> i32;
}

fn new_background_handshake() -> Result<DaemonStartupHandshake, EvaError> {
    static NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let launcher_pid = std::process::id();
    let counter = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    DaemonStartupHandshake::new(
        format!("{launcher_pid}-{timestamp}-{counter}"),
        launcher_pid,
        format!("child-{launcher_pid}-{timestamp}-{counter}"),
    )
}

fn complete_background_ready(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    child: &mut Child,
    ready: DaemonStartupFrame,
    startup_timeout_ms: u64,
    startup_elapsed_ms: u128,
) -> Result<DaemonBackgroundStartReport, EvaError> {
    let expected_pid = child.id();
    delay_ready_validation_for_test()?;
    let persisted_ready = read_daemon_startup_frame(options, handshake, DaemonStartupPhase::Ready)?
        .ok_or_else(|| EvaError::conflict("background daemon ready frame disappeared"))?;
    if persisted_ready != ready {
        return Err(EvaError::conflict(
            "background daemon ready frame changed during validation",
        ));
    }
    if ready.child_pid != expected_pid {
        return Err(EvaError::conflict(
            "background daemon ready frame returned a mismatched child pid",
        )
        .with_context("expected_pid", expected_pid.to_string())
        .with_context("actual_pid", ready.child_pid.to_string()));
    }
    let claimed = read_daemon_startup_frame(options, handshake, DaemonStartupPhase::Claimed)?
        .ok_or_else(|| {
            EvaError::conflict("background daemon ready frame has no claimed predecessor")
        })?;
    ensure_startup_frames_match(&claimed, &ready)?;
    let report_digest = ready.report_digest.as_deref().ok_or_else(|| {
        EvaError::conflict("background daemon ready frame is missing its report digest")
    })?;
    let start_json = read_daemon_startup_report(options, handshake, report_digest)?
        .ok_or_else(|| EvaError::conflict("background daemon ready report is missing"))?;
    validate_background_start_json(&start_json, &ready)?;
    if child
        .try_wait()
        .map_err(|error| {
            EvaError::internal("failed to inspect ready background daemon child")
                .with_context("io_error", error.to_string())
        })?
        .is_some()
    {
        return Err(EvaError::conflict(
            "background daemon exited while validating its ready frame",
        ));
    }

    // The daemon anchor is intentionally untouched until the nonce-bound ready frame is complete.
    let status = daemon_status(options)?;
    let lease = status
        .lease
        .clone()
        .ok_or_else(|| EvaError::conflict("ready daemon has no lease projection"))?;
    let state = status
        .state
        .as_ref()
        .ok_or_else(|| EvaError::conflict("ready daemon has no running state projection"))?;
    let expected_token = ready.process_start_token.as_deref().unwrap_or_default();
    let expected_generation = ready.generation.unwrap_or_default();
    if !status.available
        || !status.pid_matches_lease
        || state.status != "running"
        || state.mode != "background"
        || state.pid != expected_pid
        || lease.state != "active"
        || !lease.owner_live
        || lease.expired
        || lease.pid != expected_pid
        || lease.process_start_token != expected_token
        || lease.generation != expected_generation
    {
        return Err(EvaError::conflict(
            "background daemon ready frame does not match live daemon state",
        )
        .with_context("child_pid", expected_pid.to_string())
        .with_context("daemon_available", status.available.to_string())
        .with_context("pid_matches_lease", status.pid_matches_lease.to_string()));
    }
    if child
        .try_wait()
        .map_err(|error| {
            EvaError::internal("failed to recheck ready background daemon child")
                .with_context("io_error", error.to_string())
        })?
        .is_some()
    {
        return Err(EvaError::conflict(
            "background daemon exited before launcher success publication",
        ));
    }

    Ok(DaemonBackgroundStartReport {
        start_json,
        status: status.status,
        mode: state.mode.clone(),
        pid: expected_pid,
        generation_id: state.generation_id.clone(),
        project_root: state.project_root.clone(),
        dev_mode: options.dev_mode,
        paths: status.paths,
        lease,
        handshake: ready,
        startup_timeout_ms,
        startup_elapsed_ms,
    })
}

fn ensure_startup_frames_match(
    claimed: &DaemonStartupFrame,
    ready: &DaemonStartupFrame,
) -> Result<(), EvaError> {
    if claimed.phase != DaemonStartupPhase::Claimed
        || ready.phase != DaemonStartupPhase::Ready
        || claimed.nonce != ready.nonce
        || claimed.launcher_pid != ready.launcher_pid
        || claimed.child_pid != ready.child_pid
        || claimed.process_start_token != ready.process_start_token
        || claimed.generation != ready.generation
    {
        return Err(EvaError::conflict(
            "background daemon claimed and ready identities do not match",
        ));
    }
    Ok(())
}

fn validate_background_start_json(
    start_json: &str,
    ready: &DaemonStartupFrame,
) -> Result<(), EvaError> {
    let token = ready.process_start_token.as_deref().unwrap_or_default();
    let generation = ready.generation.unwrap_or_default();
    let required = [
        "\"status\":\"running\"".to_owned(),
        "\"mode\":\"background\"".to_owned(),
        format!("\"pid\":{}", ready.child_pid),
        "\"foreground\":false".to_owned(),
        format!(
            "\"lease\":{{\"state\":\"active\",\"pid\":{},\"process_start_token\":{},\"generation\":{}",
            ready.child_pid,
            json_string(token),
            generation
        ),
    ];
    if !start_json.starts_with('{')
        || !start_json.ends_with('}')
        || required
            .iter()
            .any(|fragment| !start_json.contains(fragment))
    {
        return Err(EvaError::conflict(
            "background daemon startup report is not bound to its ready identity",
        ));
    }
    Ok(())
}

fn daemon_startup_failure(frame: &DaemonStartupFrame) -> EvaError {
    let message = "background daemon reported startup failure";
    let error = match frame.error_kind.as_deref() {
        Some("invalid_argument") => EvaError::invalid_argument(message),
        Some("not_found") => EvaError::not_found(message),
        Some("conflict") => EvaError::conflict(message),
        Some("permission_denied") => EvaError::permission_denied(message),
        Some("timeout") => EvaError::timeout(message),
        Some("unavailable") => EvaError::unavailable(message),
        Some("unsupported") => EvaError::unsupported(message),
        _ => EvaError::internal(message),
    };
    error
        .with_context("child_pid", frame.child_pid.to_string())
        .with_context(
            "child_error_kind",
            frame.error_kind.as_deref().unwrap_or("unknown"),
        )
        .with_context("child_cleanup_complete", frame.cleanup_complete.to_string())
}

fn abort_background_start(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    child: &mut Child,
    failure: EvaError,
) -> EvaError {
    match terminate_background_child(options, handshake, child) {
        Ok(exit) => finish_failed_background_start(options, handshake, child.id(), failure, exit),
        Err(error) => failure
            .with_context("child_pid", child.id().to_string())
            .with_context("termination_error", error.to_string()),
    }
}

fn terminate_background_child(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    child: &mut Child,
) -> Result<ExitStatus, EvaError> {
    if let Some(exit) = child.try_wait().map_err(|error| {
        EvaError::internal("failed to inspect background daemon child during cleanup")
            .with_context("io_error", error.to_string())
    })? {
        return Ok(exit);
    }
    let _ = request_daemon_startup_abort(options, handshake);
    let deadline = Instant::now() + BACKGROUND_ABORT_GRACE;
    loop {
        if let Some(exit) = child.try_wait().map_err(|error| {
            EvaError::internal("failed to wait for background daemon startup abort")
                .with_context("io_error", error.to_string())
        })? {
            return Ok(exit);
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(BACKGROUND_POLL_INTERVAL);
    }
    let _ = child.kill();
    child.wait().map_err(|error| {
        EvaError::internal("failed to reap background daemon child")
            .with_context("io_error", error.to_string())
    })
}

fn finish_failed_background_start(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    child_pid: u32,
    failure: EvaError,
    exit: ExitStatus,
) -> EvaError {
    let (claimed, claimed_error) =
        match read_daemon_startup_frame(options, handshake, DaemonStartupPhase::Claimed) {
            Ok(frame) => (frame, None),
            Err(error) => (None, Some(error)),
        };
    let cleanup =
        cleanup_failed_daemon_start(options, handshake, child_pid, claimed.as_ref(), &failure);
    let mut result = failure
        .with_context("child_pid", child_pid.to_string())
        .with_context("exit_status", exit_status_text(exit));
    if let Some(error) = claimed_error {
        result = result.with_context("claimed_frame_error", error.to_string());
    }
    match cleanup {
        Ok(report) => result
            .with_context("cleanup_complete", report.cleanup_complete.to_string())
            .with_context("cleanup_identity_source", report.identity_source)
            .with_context("cleanup_pid_removed", report.pid_removed.to_string())
            .with_context("cleanup_state_stopped", report.state_stopped.to_string()),
        Err(error) => result.with_context("cleanup_error", error.to_string()),
    }
}

fn exit_status_text(exit: ExitStatus) -> String {
    exit.code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_owned())
}

#[cfg(debug_assertions)]
fn delay_background_report_for_test() -> Result<(), EvaError> {
    let Some(value) = env::var_os(BACKGROUND_REPORT_DELAY_ENV) else {
        return Ok(());
    };
    let delay_ms = value
        .to_str()
        .ok_or_else(|| EvaError::invalid_argument("daemon test report delay is not utf-8"))?
        .parse::<u64>()
        .map_err(|_| EvaError::invalid_argument("daemon test report delay is invalid"))?;
    std::thread::sleep(Duration::from_millis(delay_ms));
    Ok(())
}

#[cfg(debug_assertions)]
fn delay_ready_validation_for_test() -> Result<(), EvaError> {
    let Some(value) = env::var_os(BACKGROUND_READY_VALIDATION_DELAY_ENV) else {
        return Ok(());
    };
    let delay_ms = value
        .to_str()
        .ok_or_else(|| EvaError::invalid_argument("daemon test ready delay is not utf-8"))?
        .parse::<u64>()
        .map_err(|_| EvaError::invalid_argument("daemon test ready delay is invalid"))?;
    std::thread::sleep(Duration::from_millis(delay_ms));
    Ok(())
}

#[cfg(debug_assertions)]
fn delay_background_child_start_for_test() -> Result<(), EvaError> {
    delay_from_env_for_test(
        BACKGROUND_CHILD_START_DELAY_ENV,
        "daemon test background child start delay",
    )
}

#[cfg(debug_assertions)]
fn delay_terminal_poll_for_test() -> Result<(), EvaError> {
    delay_from_env_for_test(
        BACKGROUND_TERMINAL_POLL_DELAY_ENV,
        "daemon test terminal poll delay",
    )
}

#[cfg(debug_assertions)]
fn delay_from_env_for_test(name: &str, label: &str) -> Result<(), EvaError> {
    let Some(value) = env::var_os(name) else {
        return Ok(());
    };
    let delay_ms = value
        .to_str()
        .ok_or_else(|| EvaError::invalid_argument(format!("{label} is not utf-8")))?
        .parse::<u64>()
        .map_err(|_| EvaError::invalid_argument(format!("{label} is invalid")))?;
    std::thread::sleep(Duration::from_millis(delay_ms));
    Ok(())
}

#[cfg(not(debug_assertions))]
fn delay_background_report_for_test() -> Result<(), EvaError> {
    Ok(())
}

#[cfg(not(debug_assertions))]
fn delay_ready_validation_for_test() -> Result<(), EvaError> {
    Ok(())
}

#[cfg(not(debug_assertions))]
fn delay_background_child_start_for_test() -> Result<(), EvaError> {
    Ok(())
}

#[cfg(not(debug_assertions))]
fn delay_terminal_poll_for_test() -> Result<(), EvaError> {
    Ok(())
}

/// 输出 daemon 后台启动的 ready handshake 摘要。
fn write_daemon_background_start<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &DaemonBackgroundStartReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Daemon background start").map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "mode: {}", report.mode).map_err(write_error_kind)?;
            writeln!(writer, "pid: {}", report.pid).map_err(write_error_kind)?;
            writeln!(writer, "generation_id: {}", report.generation_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(writer, "foreground: false").map_err(write_error_kind)?;
            writeln!(writer, "dev_mode: {}", report.dev_mode).map_err(write_error_kind)?;
            writeln!(writer, "ready: true").map_err(write_error_kind)?;
            writeln!(
                writer,
                "lease: state={} pid={} token={} generation={} heartbeat_at_ms={} expires_at_ms={} owner_live={} expired={}",
                report.lease.state,
                report.lease.pid,
                report.lease.process_start_token,
                report.lease.generation,
                report.lease.heartbeat_at_ms,
                report.lease.expires_at_ms,
                report.lease.owner_live,
                report.lease.expired
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "state_file: {}", report.paths.state_file)
                .map_err(write_error_kind)?;
            writeln!(writer, "lock_file: {}", report.paths.lock_file).map_err(write_error_kind)?;
            writeln!(writer, "lease_file: {}", report.paths.lease_file)
                .map_err(write_error_kind)?;
            writeln!(writer, "pid_file: {}", report.paths.pid_file).map_err(write_error_kind)?;
            writeln!(writer, "startup_nonce: {}", report.handshake.nonce)
                .map_err(write_error_kind)?;
            writeln!(writer, "startup_elapsed_ms: {}", report.startup_elapsed_ms)
                .map_err(write_error_kind)
        }
        OutputFormat::Json => {
            let data = daemon_background_start_json(report)?;
            writeln!(
                writer,
                "{}",
                success_envelope("daemon.start", EXIT_OK, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}

fn daemon_background_start_json(report: &DaemonBackgroundStartReport) -> Result<String, EvaError> {
    let base = report
        .start_json
        .strip_suffix('}')
        .ok_or_else(|| EvaError::conflict("daemon startup report is not a JSON object"))?;
    let handshake_json = format!(
        "{{\"protocol\":{},\"nonce\":{},\"phase\":{},\"ready_at_ms\":{},\"report_digest\":{}}}",
        json_string(STARTUP_HANDSHAKE_PROTOCOL),
        json_string(&report.handshake.nonce),
        json_string(report.handshake.phase.as_str()),
        report.handshake.observed_at_ms,
        json_string(
            report
                .handshake
                .report_digest
                .as_deref()
                .unwrap_or_default()
        ),
    );
    let spawn_json = format!(
        "{{\"strategy\":{},\"launcher_pid\":{},\"child_pid\":{},\"startup_timeout_ms\":{},\"startup_elapsed_ms\":{},\"handshake\":{}}}",
        json_string(background_spawn_strategy()),
        report.handshake.launcher_pid,
        report.handshake.child_pid,
        report.startup_timeout_ms,
        report.startup_elapsed_ms,
        handshake_json,
    );
    Ok(format!("{base},\"spawn\":{spawn_json}}}"))
}

fn background_spawn_strategy() -> &'static str {
    if cfg!(windows) {
        "windows_detached_process_group"
    } else {
        "unix_process_group_stdio_child"
    }
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
                "lease: state={} generation={} heartbeat_at_ms={} expires_at_ms={}",
                report.lease.state,
                report.lease.generation,
                report.lease.heartbeat_at_ms,
                report.lease.expires_at_ms
            )
            .map_err(write_error_kind)?;
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
            if let Some(maintenance) = &report.memory_maintenance {
                writeln!(
                    writer,
                    "memory_maintenance: status={} expired_removed={} knowledge_items_indexed={}",
                    maintenance.status,
                    maintenance.memory_gc.expired_removed,
                    maintenance.knowledge_rebuild.items_indexed
                )
                .map_err(write_error_kind)
            } else {
                writeln!(writer, "memory_maintenance: not_run").map_err(write_error_kind)
            }
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
            if let Some(lease) = &report.lease {
                let observed_at_ms = daemon_observed_at_ms();
                writeln!(
                    writer,
                    "lease: state={} generation={} owner_live={} expired={} freshness={} heartbeat_age_ms={}",
                    lease.state,
                    lease.generation,
                    lease.owner_live,
                    lease.expired,
                    lease.freshness_at(observed_at_ms).as_str(),
                    lease.heartbeat_age_ms(observed_at_ms),
                )
                .map_err(write_error_kind)?;
            }
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
        "{{\"status\":{},\"mode\":{},\"pid\":{},\"generation_id\":{},\"project_root\":{},\"foreground\":{},\"dev_mode\":{},\"provider_processes_started\":{},\"paths\":{},\"lease\":{},\"durable_backend\":{},\"recovery\":{},\"policy\":{},\"observability\":{},\"hardware_hotplug\":{},\"memory_maintenance\":{},\"shutdown\":{},\"audit\":{}}}",
        json_string(&report.status),
        json_string(&report.mode),
        report.pid,
        json_string(&report.generation_id),
        json_string(&report.project_root),
        report.foreground,
        report.dev_mode,
        report.provider_processes_started,
        daemon_paths_json(&report.paths),
        daemon_lease_json(&report.lease),
        durable_backend_json(&report.durable_backend),
        recovery_json(&report.recovery),
        daemon_policy_json(&report.policy),
        observability_json(&report.observability),
        hardware_hotplug_json(&report.hardware_hotplug),
        report.memory_maintenance.as_ref().map(memory_maintenance_json).unwrap_or_else(||"null".to_owned()),
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
    let lease = report
        .lease
        .as_ref()
        .map(daemon_lease_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"request_id\":{},\"trace_id\":{},\"operation\":{},\"accepted\":{},\"daemon_available\":{},\"status\":{},\"mutation_executed\":{},\"request_file\":{},\"response_file\":{},\"state\":{},\"lease\":{},\"task_id\":{},\"plan_id\":{},\"generation_id\":{},\"message\":{},\"shutdown\":{},\"audit\":{}}}",
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
        lease,
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
        "{{\"durable_backend_root\":{},\"observability_backend_root\":{},\"state_dir\":{},\"lock_dir\":{},\"pid_dir\":{},\"control_request_dir\":{},\"control_response_dir\":{},\"state_file\":{},\"hardware_hotplug_state_file\":{},\"lock_file\":{},\"lease_file\":{},\"pid_file\":{}}}",
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
        json_string(&paths.lease_file),
        json_string(&paths.pid_file)
    )
}

/// 将 daemon lease 身份、续租时间和 owner 判活证据编码为稳定 JSON。
fn daemon_lease_json(lease: &DaemonLeaseReport) -> String {
    let observed_at_ms = daemon_observed_at_ms();
    format!(
        "{{\"state\":{},\"pid\":{},\"process_start_token\":{},\"generation\":{},\"heartbeat_at_ms\":{},\"heartbeat_age_ms\":{},\"freshness\":{},\"expires_at_ms\":{},\"owner_live\":{},\"expired\":{}}}",
        json_string(&lease.state),
        lease.pid,
        json_string(&lease.process_start_token),
        lease.generation,
        lease.heartbeat_at_ms,
        lease.heartbeat_age_ms(observed_at_ms),
        json_string(lease.freshness_at(observed_at_ms).as_str()),
        lease.expires_at_ms,
        lease.owner_live,
        lease.expired
    )
}

fn daemon_observed_at_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
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
    fn service_entry_parser_requires_each_identity_field_exactly_once() {
        let canonical = parse_daemon_command(
            &[
                "__service-entry",
                "--project",
                ".",
                "--service-name",
                "eva-test",
                "--service-kind",
                "systemd",
                "--service-identity",
                "deadbeef",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert!(matches!(canonical, DaemonCommand::ServiceEntry { .. }));

        let missing = parse_daemon_command(
            &[
                "__service-entry",
                "--service-name",
                "eva-test",
                "--service-kind",
                "systemd",
            ]
            .map(str::to_owned),
        )
        .unwrap_err();
        assert_eq!(missing.kind(), eva_core::ErrorKind::InvalidArgument);
        assert!(missing.message().contains("--service-identity"));

        let duplicate = parse_daemon_command(
            &[
                "__service-entry",
                "--service-name",
                "eva-test",
                "--service-name",
                "eva-other",
                "--service-kind",
                "systemd",
                "--service-identity",
                "deadbeef",
            ]
            .map(str::to_owned),
        )
        .unwrap_err();
        assert_eq!(duplicate.kind(), eva_core::ErrorKind::InvalidArgument);
        assert!(duplicate.message().contains("canonical"));

        let reordered = parse_daemon_command(
            &[
                "__service-entry",
                "--service-name",
                "eva-test",
                "--project",
                ".",
                "--service-kind",
                "systemd",
                "--service-identity",
                "deadbeef",
            ]
            .map(str::to_owned),
        )
        .unwrap_err();
        assert!(reordered.message().contains("canonical"));
    }

    #[test]
    fn service_entry_validation_binds_host_kind_name_project_and_digest() {
        let Some(kind) = ServiceHostPlatform::current().service_manager_kind() else {
            return;
        };
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let mut project = load_project_config(&project_root).unwrap();
        let manager = project.eva.service_manager.as_mut().unwrap();
        manager.enabled = true;
        manager.kind = kind;
        manager.service_name = "eva-test".to_owned();
        let executable = std::fs::canonicalize(env::current_exe().unwrap()).unwrap();
        manager.runtime_binary = Some(executable.clone());
        let entrypoint = ServiceManagerEntryPoint::for_daemon(
            executable.clone(),
            project.project_root.clone(),
            "eva-test",
            kind,
        )
        .unwrap();

        validate_service_entrypoint(
            &project,
            "eva-test",
            kind.as_str(),
            entrypoint.identity_digest(),
            false,
        )
        .unwrap();
        let mismatch =
            validate_service_entrypoint(&project, "eva-test", kind.as_str(), "deadbeef", false)
                .unwrap_err();
        assert_eq!(mismatch.kind(), eva_core::ErrorKind::Conflict);
        assert!(mismatch.message().contains("identity digest"));

        project.eva.service_manager.as_mut().unwrap().runtime_binary =
            Some(project.project_root.join("Cargo.toml"));
        let binary_drift = validate_service_entrypoint(
            &project,
            "eva-test",
            kind.as_str(),
            entrypoint.identity_digest(),
            false,
        )
        .unwrap_err();
        assert_eq!(binary_drift.kind(), eva_core::ErrorKind::Conflict);
        assert!(binary_drift.message().contains("runtime binary"));
    }

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

    #[test]
    fn daemon_shutdown_keeps_drain_and_client_timeouts_distinct() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let project = load_project_config(&project_root).unwrap();
        let args =
            ["--drain-timeout-ms", "1200", "--control-timeout-ms", "5000"].map(str::to_owned);
        let options = parse_daemon_options(&args).unwrap();
        let request_id = RequestId::parse("req-daemon-cli-drain-timeout").unwrap();
        let trace = TraceFields::default().with_request_id(request_id.clone());

        let request = daemon_control_request(
            DaemonControlOperation::Shutdown,
            &project,
            &options,
            request_id.clone(),
            &trace,
        )
        .unwrap();
        assert_eq!(request.timeout_ms, Some(1_200));
        assert_eq!(options.control_timeout_ms, 5_000);

        let error = daemon_control_request(
            DaemonControlOperation::Status,
            &project,
            &options,
            request_id,
            &trace,
        )
        .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        for valid in [2, MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS] {
            assert!(
                parse_daemon_options(&["--drain-timeout-ms".to_owned(), valid.to_string(),])
                    .is_ok()
            );
        }
        assert!(parse_daemon_options(&["--drain-timeout-ms".to_owned(), "1".to_owned()]).is_err());
        assert!(parse_daemon_options(&[
            "--drain-timeout-ms".to_owned(),
            (MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS + 1).to_string(),
        ])
        .is_err());
    }
}
