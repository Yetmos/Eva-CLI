//! Agent 状态、排空与热重载子命令；守护进程可用时执行真实变更，否则只生成本地计划。

use super::{
    json_array, json_string, option_json, parse_common_options, parse_u64_option,
    parse_usize_option, required_option, success_envelope, trace_for, write_command_error,
    write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_agent::{AgentLifecycle, AgentRuntime, AgentStateSnapshot};
use eva_config::{load_project_config, manifest::agent::AgentManifest, ProjectConfig};
use eva_core::{AgentId, ErrorKind, EvaError, GenerationId, RequestId};
use eva_lifecycle::{
    DrainCoordinator, DrainPlan, GenerationController, GenerationDrainEvidence, GenerationState,
    RuntimeGeneration,
};
use eva_observability::TraceFields;
use eva_runtime::{
    send_daemon_control_request, DaemonControlOperation, DaemonControlRequest,
    DaemonControlResponse, DaemonStartOptions,
};
use std::io::Write;
use std::path::PathBuf;

/// 构建只读 AgentRuntime 时使用的默认队列容量。
const DEFAULT_QUEUE_CAPACITY: usize = 256;
/// 未指定时用于排空计划的兼容代际 ID。
const DEFAULT_DRAIN_GENERATION: &str = "gen-v1115-agent";
/// 热重载计划的默认源代际。
const DEFAULT_FROM_GENERATION: &str = "gen-current";
/// 热重载计划的默认目标代际。
const DEFAULT_TO_GENERATION: &str = "gen-next";
/// 热重载计划的默认源发布引用。
const DEFAULT_FROM_RELEASE: &str = "current";
/// 热重载计划的默认目标发布引用。
const DEFAULT_TO_RELEASE: &str = "next";
/// 排空和 daemon control 的默认超时预算。
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 子命令及其已解析选项。
pub(super) enum AgentCommand {
    /// 查询一个或全部 Agent 的 manifest 与生命周期状态。
    Status(
        /// 已解析的可选 Agent 标识与公共选项。
        AgentStatusOptions,
    ),
    /// 排空指定 Agent 的在途任务。
    Drain(
        /// 已解析的 Agent 标识、排空时限、操作者与公共选项。
        AgentDrainOptions,
    ),
    /// 将指定 Agent 切换到新运行时代际。
    Reload(
        /// 已解析的 Agent 标识、目标代际、操作者与公共选项。
        AgentReloadOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 状态查询选项。
pub(super) struct AgentStatusOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 可选 Agent ID；缺省查询全部 Agent。
    agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 排空选项，包括 daemon control 路径和本地计划参数。
pub(super) struct AgentDrainOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 必须存在于项目清单的 Agent ID。
    agent_id: String,
    /// 要排空的运行时代际。
    generation: String,
    /// 规划时视为仍在执行的任务数。
    inflight_tasks: usize,
    /// 排空等待预算。
    timeout_ms: u64,
    /// daemon 使用的可选 durable backend。
    durable_backend: Option<PathBuf>,
    /// daemon 状态目录。
    state_dir: Option<PathBuf>,
    /// daemon 锁目录。
    lock_dir: Option<PathBuf>,
    /// daemon pid 目录。
    pid_dir: Option<PathBuf>,
    /// control mailbox 响应超时。
    control_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 热重载选项，包含代际/发布切换和 daemon control 路径。
pub(super) struct AgentReloadOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 要重载的 Agent ID。
    agent_id: String,
    /// 当前运行时代际。
    from_generation: String,
    /// 目标运行时代际。
    to_generation: String,
    /// 当前发布引用。
    from_release: String,
    /// 目标发布引用。
    to_release: String,
    /// 切换前需要排空的在途任务数。
    inflight_tasks: usize,
    /// 排空等待预算。
    timeout_ms: u64,
    /// daemon 使用的可选 durable backend。
    durable_backend: Option<PathBuf>,
    /// daemon 状态目录。
    state_dir: Option<PathBuf>,
    /// daemon 锁目录。
    lock_dir: Option<PathBuf>,
    /// daemon pid 目录。
    pid_dir: Option<PathBuf>,
    /// control mailbox 响应超时。
    control_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 状态命令的组合报告。
struct AgentStatusReport {
    /// 聚合状态文本。
    status: String,
    /// 选中 Agent 的逐项状态。
    agents: Vec<AgentStatusEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 单个 Agent 的 manifest 与生命周期状态投影。
struct AgentStatusEntry {
    /// 稳定 Agent ID。
    agent_id: String,
    /// 清单启用状态。
    enabled: bool,
    /// 当前生命周期状态。
    lifecycle: String,
    /// AgentRuntime 中的排队事件数。
    queued_events: usize,
    /// 脚本路径。
    script: String,
    /// 可选脚本版本。
    script_version: Option<String>,
    /// 可选父 Agent ID。
    parent: Option<String>,
    /// 子 Agent ID 列表。
    children: Vec<String>,
    /// 订阅 topic pattern 列表。
    subscriptions: Vec<String>,
    /// 清单来源路径。
    manifest_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 排空计划或 daemon 变更的结果。
struct AgentDrainReport {
    /// 目标 Agent ID。
    agent_id: String,
    /// 操作结果状态。
    status: String,
    /// 操作后的生命周期文本。
    lifecycle: String,
    /// 清单启用状态。
    enabled: bool,
    /// 详细排空计划。
    drain: DrainPlan,
    /// 是否由 daemon 执行了真实状态变更。
    mutation_executed: bool,
    /// daemon 或本地 fallback 的说明。
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Agent 热重载的代际切换证据。
struct AgentReloadReport {
    /// 目标 Agent ID。
    agent_id: String,
    /// 操作状态。
    status: String,
    /// 操作后的生命周期文本。
    lifecycle: String,
    /// 清单启用状态。
    enabled: bool,
    /// 源代际 ID。
    from_generation: String,
    /// 目标代际 ID。
    to_generation: String,
    /// 源发布引用。
    from_release: String,
    /// 目标发布引用。
    to_release: String,
    /// 切换后的活动代际。
    active_generation: String,
    /// 被替换的前一代际。
    previous_generation: String,
    /// 前一代际最终状态。
    previous_generation_state: String,
    /// 是否由 daemon 执行了真实状态变更。
    mutation_executed: bool,
    /// 切换前排空证据。
    drain: GenerationDrainEvidence,
    /// 代际控制器与 daemon 的审计记录。
    audit: Vec<String>,
}

/// 解析 `agent status|drain|reload` 子命令。
pub(super) fn parse_agent_command(args: &[String]) -> Result<AgentCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing agent subcommand"))?;
    match subcommand.as_str() {
        "status" => Ok(AgentCommand::Status(parse_agent_status_options(rest)?)),
        "drain" => Ok(AgentCommand::Drain(parse_agent_drain_options(rest)?)),
        "reload" => Ok(AgentCommand::Reload(parse_agent_reload_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown agent subcommand").with_context("subcommand", value))
        }
    }
}

/// 执行 Agent 查询或生命周期操作，并保持各分支的 trace 和错误输出一致。
pub(super) fn execute_agent<W, E>(
    command: AgentCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        AgentCommand::Status(options) => {
            let trace = trace_for("cli.agent.status");
            match load_project_config(&options.common.project_root)
                .and_then(|project| create_agent_status(&project, options.agent_id.as_deref()))
            {
                Ok(report) => {
                    write_agent_status(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "agent.status",
                    &error,
                    &trace,
                ),
            }
        }
        AgentCommand::Drain(options) => {
            let trace = trace_for("cli.agent.drain");
            match load_project_config(&options.common.project_root).and_then(|project| {
                create_agent_drain_with_daemon_fallback(&project, &options, &trace)
            }) {
                Ok(report) => {
                    write_agent_drain(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "agent.drain",
                    &error,
                    &trace,
                ),
            }
        }
        AgentCommand::Reload(options) => {
            let trace = trace_for("cli.agent.reload");
            match load_project_config(&options.common.project_root).and_then(|project| {
                create_agent_reload_with_daemon_fallback(&project, &options, &trace)
            }) {
                Ok(report) => {
                    write_agent_reload(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "agent.reload",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// 解析可选 Agent ID 和公共选项，并在 I/O 前校验 ID。
fn parse_agent_status_options(args: &[String]) -> Result<AgentStatusOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut agent_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--agent" | "--agent-id" => {
                index += 1;
                agent_id = Some(required_option(args, index, "agent option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    if let Some(value) = &agent_id {
        AgentId::parse(value)?;
    }

    Ok(AgentStatusOptions {
        common: parse_common_options(&passthrough)?,
        agent_id,
    })
}

/// 解析排空参数、daemon 路径和两类超时预算。
fn parse_agent_drain_options(args: &[String]) -> Result<AgentDrainOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut agent_id = None;
    let mut generation = DEFAULT_DRAIN_GENERATION.to_owned();
    let mut inflight_tasks = 0;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut durable_backend = None;
    let mut state_dir = None;
    let mut lock_dir = None;
    let mut pid_dir = None;
    let mut control_timeout_ms = 5_000;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--agent" | "--agent-id" => {
                index += 1;
                agent_id = Some(required_option(args, index, "agent option")?.clone());
            }
            "--generation" | "--generation-id" => {
                index += 1;
                generation = required_option(args, index, "generation option")?.clone();
            }
            "--inflight" | "--inflight-tasks" => {
                index += 1;
                inflight_tasks = parse_usize_option(
                    "inflight_tasks",
                    required_option(args, index, "inflight option")?,
                )?;
            }
            "--timeout-ms" | "--timeout" => {
                index += 1;
                timeout_ms = parse_u64_option(
                    "timeout_ms",
                    required_option(args, index, "timeout option")?,
                )?;
            }
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
            "--control-timeout-ms" => {
                index += 1;
                control_timeout_ms = parse_u64_option(
                    "control_timeout_ms",
                    required_option(args, index, "control timeout option")?,
                )?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    let agent_id = agent_id.ok_or_else(|| {
        EvaError::invalid_argument("agent drain requires --agent")
            .with_context("suggestion", "use `eva agent drain --agent root-agent`")
    })?;
    AgentId::parse(&agent_id)?;
    GenerationId::parse(&generation)?;

    Ok(AgentDrainOptions {
        common: parse_common_options(&passthrough)?,
        agent_id,
        generation,
        inflight_tasks,
        timeout_ms,
        durable_backend,
        state_dir,
        lock_dir,
        pid_dir,
        control_timeout_ms,
    })
}

/// 解析热重载的源/目标代际与发布引用，并验证全部强类型 ID。
fn parse_agent_reload_options(args: &[String]) -> Result<AgentReloadOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut agent_id = None;
    let mut from_generation = DEFAULT_FROM_GENERATION.to_owned();
    let mut to_generation = DEFAULT_TO_GENERATION.to_owned();
    let mut from_release = DEFAULT_FROM_RELEASE.to_owned();
    let mut to_release = DEFAULT_TO_RELEASE.to_owned();
    let mut inflight_tasks = 0;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut durable_backend = None;
    let mut state_dir = None;
    let mut lock_dir = None;
    let mut pid_dir = None;
    let mut control_timeout_ms = 5_000;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--agent" | "--agent-id" => {
                index += 1;
                agent_id = Some(required_option(args, index, "agent option")?.clone());
            }
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
            "--inflight" | "--inflight-tasks" => {
                index += 1;
                inflight_tasks = parse_usize_option(
                    "inflight_tasks",
                    required_option(args, index, "inflight option")?,
                )?;
            }
            "--timeout-ms" | "--timeout" => {
                index += 1;
                timeout_ms = parse_u64_option(
                    "timeout_ms",
                    required_option(args, index, "timeout option")?,
                )?;
            }
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
            "--control-timeout-ms" => {
                index += 1;
                control_timeout_ms = parse_u64_option(
                    "control_timeout_ms",
                    required_option(args, index, "control timeout option")?,
                )?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    let agent_id = agent_id.ok_or_else(|| {
        EvaError::invalid_argument("agent reload requires --agent")
            .with_context("suggestion", "use `eva agent reload --agent root-agent`")
    })?;
    AgentId::parse(&agent_id)?;
    GenerationId::parse(&from_generation)?;
    GenerationId::parse(&to_generation)?;

    Ok(AgentReloadOptions {
        common: parse_common_options(&passthrough)?,
        agent_id,
        from_generation,
        to_generation,
        from_release,
        to_release,
        inflight_tasks,
        timeout_ms,
        durable_backend,
        state_dir,
        lock_dir,
        pid_dir,
        control_timeout_ms,
    })
}

/// 为选中 Agent 构建只读 manifest 与生命周期状态报告。
fn create_agent_status(
    project: &ProjectConfig,
    agent_id: Option<&str>,
) -> Result<AgentStatusReport, EvaError> {
    let agents = selected_agents(project, agent_id)?
        .into_iter()
        .map(agent_status_entry)
        .collect::<Result<Vec<_>, _>>()?;
    let status = if agents.iter().any(|agent| agent.enabled) {
        "ready"
    } else {
        "disabled"
    }
    .to_owned();
    Ok(AgentStatusReport { status, agents })
}

/// 优先通过运行中的 daemon 执行排空；daemon 不可用时退化为本地无变更计划。
/// 只有 `Unavailable`/`NotFound` 等可解释的控制边界失败才应形成 fallback，真实协议错误继续返回。
fn create_agent_drain_with_daemon_fallback(
    project: &ProjectConfig,
    options: &AgentDrainOptions,
    trace: &TraceFields,
) -> Result<AgentDrainReport, EvaError> {
    match create_agent_drain_via_daemon(project, options, trace) {
        Ok(report) => Ok(report),
        Err(error) if error.kind() == ErrorKind::Unavailable => {
            create_agent_drain(project, options)
        }
        Err(error) => Err(error),
    }
}

/// 优先通过 daemon 执行热重载，并在控制端不可用时生成本地代际切换证据。
fn create_agent_reload_with_daemon_fallback(
    project: &ProjectConfig,
    options: &AgentReloadOptions,
    trace: &TraceFields,
) -> Result<AgentReloadReport, EvaError> {
    match create_agent_reload_via_daemon(project, options, trace) {
        Ok(report) => Ok(report),
        Err(error) if error.kind() == ErrorKind::Unavailable => {
            create_agent_reload(project, options)
        }
        Err(error) => Err(error),
    }
}

/// 使用生命周期模型生成本地排空计划；该路径明确不修改 daemon 状态。
fn create_agent_drain(
    project: &ProjectConfig,
    options: &AgentDrainOptions,
) -> Result<AgentDrainReport, EvaError> {
    let agent = find_agent(project, &options.agent_id)?;
    let snapshot = lifecycle_snapshot_for_drain(agent)?;
    let drain = DrainCoordinator.plan(
        GenerationId::parse(&options.generation)?,
        options.inflight_tasks,
        options.timeout_ms,
    )?;
    Ok(AgentDrainReport {
        agent_id: agent.id.as_str().to_owned(),
        status: if agent.enabled { "draining" } else { "blocked" }.to_owned(),
        lifecycle: snapshot.lifecycle,
        enabled: agent.enabled,
        drain,
        mutation_executed: false,
        detail: "planned locally; no daemon process or scheduler state was mutated".to_owned(),
    })
}

/// 将排空请求发送到 daemon control mailbox，并要求响应证明已执行变更。
fn create_agent_drain_via_daemon(
    project: &ProjectConfig,
    options: &AgentDrainOptions,
    trace: &TraceFields,
) -> Result<AgentDrainReport, EvaError> {
    let agent = find_agent(project, &options.agent_id)?;
    let request_id = agent_control_request_id("drain", &options.agent_id)?;
    let request = DaemonControlRequest::new(request_id, trace, DaemonControlOperation::Drain)
        .with_agent_id(options.agent_id.clone())
        .with_generation_id(options.generation.clone())
        .with_inflight_tasks(options.inflight_tasks)
        .with_timeout_ms(options.timeout_ms);
    let response = send_daemon_control_request(
        &daemon_options_from_agent_paths(
            project,
            options.durable_backend.as_ref(),
            options.state_dir.as_ref(),
            options.lock_dir.as_ref(),
            options.pid_dir.as_ref(),
        ),
        request,
        options.control_timeout_ms,
    )?;
    daemon_response_require_mutation(&response, DaemonControlOperation::Drain)?;

    let snapshot = lifecycle_snapshot_for_drain(agent)?;
    let drain = DrainCoordinator.plan(
        GenerationId::parse(&options.generation)?,
        options.inflight_tasks,
        options.timeout_ms,
    )?;
    Ok(AgentDrainReport {
        agent_id: agent.id.as_str().to_owned(),
        status: if agent.enabled { "draining" } else { "blocked" }.to_owned(),
        lifecycle: snapshot.lifecycle,
        enabled: agent.enabled,
        drain,
        mutation_executed: true,
        detail: response.message,
    })
}

/// 使用 GenerationController 本地模拟排空和代际切换，生成可审计但无外部变更的报告。
fn create_agent_reload(
    project: &ProjectConfig,
    options: &AgentReloadOptions,
) -> Result<AgentReloadReport, EvaError> {
    let agent = find_agent(project, &options.agent_id)?;
    let snapshot = lifecycle_snapshot_for_reload(agent)?;
    let from_generation = GenerationId::parse(&options.from_generation)?;
    let to_generation = GenerationId::parse(&options.to_generation)?;
    let active = RuntimeGeneration::new(
        from_generation.clone(),
        options.from_release.clone(),
        GenerationState::Active,
    )?;
    let candidate = RuntimeGeneration::new(
        to_generation.clone(),
        options.to_release.clone(),
        GenerationState::Pending,
    )?;
    let mut controller = GenerationController::new(active)?;
    controller.start_candidate(candidate)?;
    controller.promote_candidate()?;
    let drain = DrainCoordinator.plan_generation_swap_drain(
        from_generation,
        to_generation,
        options.inflight_tasks,
        options.timeout_ms,
    )?;
    let previous = controller
        .retired
        .first()
        .ok_or_else(|| EvaError::internal("generation promotion did not retain previous active"))?;
    let mut audit = controller.audit.clone();
    audit.extend(drain.audit.iter().cloned());
    audit.push("agent:reload:planned_without_daemon_mutation".to_owned());

    Ok(AgentReloadReport {
        agent_id: agent.id.as_str().to_owned(),
        status: if agent.enabled { "planned" } else { "blocked" }.to_owned(),
        lifecycle: snapshot.lifecycle,
        enabled: agent.enabled,
        from_generation: options.from_generation.clone(),
        to_generation: options.to_generation.clone(),
        from_release: options.from_release.clone(),
        to_release: options.to_release.clone(),
        active_generation: controller.active.id.as_str().to_owned(),
        previous_generation: previous.id.as_str().to_owned(),
        previous_generation_state: previous.state.as_str().to_owned(),
        mutation_executed: false,
        drain,
        audit,
    })
}

/// 通过 daemon control mailbox 执行热重载并解析代际证据。
fn create_agent_reload_via_daemon(
    project: &ProjectConfig,
    options: &AgentReloadOptions,
    trace: &TraceFields,
) -> Result<AgentReloadReport, EvaError> {
    let agent = find_agent(project, &options.agent_id)?;
    let request_id = agent_control_request_id("reload", &options.agent_id)?;
    let request = DaemonControlRequest::new(request_id, trace, DaemonControlOperation::ReloadPlan)
        .with_agent_id(options.agent_id.clone())
        .with_from_generation_id(options.from_generation.clone())
        .with_to_generation_id(options.to_generation.clone())
        .with_from_release(options.from_release.clone())
        .with_to_release(options.to_release.clone())
        .with_inflight_tasks(options.inflight_tasks)
        .with_timeout_ms(options.timeout_ms);
    let response = send_daemon_control_request(
        &daemon_options_from_agent_paths(
            project,
            options.durable_backend.as_ref(),
            options.state_dir.as_ref(),
            options.lock_dir.as_ref(),
            options.pid_dir.as_ref(),
        ),
        request,
        options.control_timeout_ms,
    )?;
    daemon_response_require_mutation(&response, DaemonControlOperation::ReloadPlan)?;
    let mut report = create_agent_reload(project, options)?;
    report.status = if agent.enabled { "reloaded" } else { "blocked" }.to_owned();
    report.lifecycle = lifecycle_snapshot_for_reload(agent)?.lifecycle;
    report.mutation_executed = true;
    report
        .audit
        .retain(|entry| entry != "agent:reload:planned_without_daemon_mutation");
    report.audit.extend(response.audit);
    report
        .audit
        .push("agent:reload:daemon_mutation_executed".to_owned());
    Ok(report)
}

/// 将 Agent 命令提供的路径合成为 daemon control 连接选项。
fn daemon_options_from_agent_paths(
    project: &ProjectConfig,
    durable_backend: Option<&PathBuf>,
    state_dir: Option<&PathBuf>,
    lock_dir: Option<&PathBuf>,
    pid_dir: Option<&PathBuf>,
) -> DaemonStartOptions {
    let mut options = DaemonStartOptions::defaults(project);
    if let Some(path) = durable_backend {
        options.durable_backend = path.clone();
    }
    if let Some(path) = state_dir {
        options.state_dir = path.clone();
    }
    if let Some(path) = lock_dir {
        options.lock_dir = path.clone();
    }
    if let Some(path) = pid_dir {
        options.pid_dir = path.clone();
    }
    options.resolve_against_project(&project.project_root)
}

/// 验证 daemon 响应确实执行了请求变更；只返回计划不能冒充成功 apply。
fn daemon_response_require_mutation(
    response: &DaemonControlResponse,
    operation: DaemonControlOperation,
) -> Result<(), EvaError> {
    if response.operation != operation || !response.accepted || !response.mutation_executed {
        return Err(
            EvaError::conflict("daemon did not execute requested agent mutation")
                .with_context("operation", operation.as_str())
                .with_context("response_operation", response.operation.as_str())
                .with_context("accepted", response.accepted.to_string())
                .with_context("mutation_executed", response.mutation_executed.to_string()),
        );
    }
    Ok(())
}

/// 为 daemon Agent 操作生成稳定、可校验的控制请求 ID。
fn agent_control_request_id(operation: &str, agent_id: &str) -> Result<RequestId, EvaError> {
    let mut suffix = String::new();
    for ch in agent_id.chars().take(80) {
        suffix.push(ch);
    }
    RequestId::parse(&format!("req-agent-{operation}-{suffix}"))
}

/// 根据可选 ID 选择一个或全部 Agent；显式 ID 不存在时返回 NotFound。
fn selected_agents<'a>(
    project: &'a ProjectConfig,
    agent_id: Option<&str>,
) -> Result<Vec<&'a AgentManifest>, EvaError> {
    if let Some(agent_id) = agent_id {
        return Ok(vec![find_agent(project, agent_id)?]);
    }
    Ok(project.agents.iter().collect())
}

/// 在已验证项目清单中精确查找 Agent。
fn find_agent<'a>(
    project: &'a ProjectConfig,
    agent_id: &str,
) -> Result<&'a AgentManifest, EvaError> {
    project
        .agents
        .iter()
        .find(|agent| agent.id.as_str() == agent_id)
        .ok_or_else(|| {
            EvaError::not_found("agent manifest was not found").with_context("agent_id", agent_id)
        })
}

/// 将 Agent manifest 与模拟 lifecycle snapshot 合并为状态输出项。
fn agent_status_entry(agent: &AgentManifest) -> Result<AgentStatusEntry, EvaError> {
    let snapshot = lifecycle_snapshot_for_status(agent)?;
    Ok(AgentStatusEntry {
        agent_id: agent.id.as_str().to_owned(),
        enabled: agent.enabled,
        lifecycle: snapshot.lifecycle,
        queued_events: snapshot.queued_events,
        script: agent.script.display().to_string(),
        script_version: agent.script_version.clone(),
        parent: agent
            .parent
            .as_ref()
            .map(|parent| parent.as_str().to_owned()),
        children: agent
            .children
            .iter()
            .map(|child| child.as_str().to_owned())
            .collect(),
        subscriptions: agent
            .subscriptions
            .iter()
            .map(|subscription| subscription.as_str().to_owned())
            .collect(),
        manifest_path: agent.path.display().to_string(),
    })
}

/// 构建状态查询使用的只读 AgentRuntime 快照。
fn lifecycle_snapshot_for_status(agent: &AgentManifest) -> Result<AgentStateSnapshot, EvaError> {
    if !agent.enabled {
        return Ok(AgentStateSnapshot::new(agent.id.clone(), 0, "disabled"));
    }
    let mut runtime = AgentRuntime::new(agent.id.clone(), DEFAULT_QUEUE_CAPACITY)?;
    runtime.start()?;
    Ok(AgentStateSnapshot::new(
        runtime.agent_id().clone(),
        runtime.queued_len(),
        runtime.state().as_str(),
    ))
}

/// 通过生命周期转换构建排空后的快照，用于本地 plan 报告。
fn lifecycle_snapshot_for_drain(agent: &AgentManifest) -> Result<AgentStateSnapshot, EvaError> {
    if !agent.enabled {
        return Ok(AgentStateSnapshot::new(agent.id.clone(), 0, "disabled"));
    }
    let mut lifecycle = AgentLifecycle::new();
    lifecycle.start()?;
    lifecycle.drain()?;
    Ok(AgentStateSnapshot::new(
        agent.id.clone(),
        0,
        lifecycle.state().as_str(),
    ))
}

/// 构建重载完成后的 Agent 生命周期快照。
fn lifecycle_snapshot_for_reload(agent: &AgentManifest) -> Result<AgentStateSnapshot, EvaError> {
    if !agent.enabled {
        return Ok(AgentStateSnapshot::new(agent.id.clone(), 0, "disabled"));
    }
    lifecycle_snapshot_for_status(agent)
}

/// 输出一个或多个 Agent 的状态、层级、订阅和脚本信息。
fn write_agent_status<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &AgentStatusReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Agent status").map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            for agent in &report.agents {
                writeln!(
                    writer,
                    "  - {} enabled={} lifecycle={} queued_events={} subscriptions={}",
                    agent.agent_id,
                    agent.enabled,
                    agent.lifecycle,
                    agent.queued_events,
                    agent.subscriptions.join(",")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("agent.status", EXIT_OK, &agent_status_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 输出排空计划，并明确标识是否发生 daemon 变更。
fn write_agent_drain<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &AgentDrainReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Agent drain plan").map_err(write_error_kind)?;
            writeln!(writer, "agent: {}", report.agent_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "lifecycle: {}", report.lifecycle).map_err(write_error_kind)?;
            writeln!(
                writer,
                "generation: {}",
                report.drain.generation_id.as_str()
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "inflight_tasks: {}", report.drain.inflight_tasks)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "accepts_new_work: {}",
                report.drain.accepts_new_work
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", report.mutation_executed)
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("agent.drain", EXIT_OK, &agent_drain_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 输出热重载的源/目标代际、排空证据和变更事实。
fn write_agent_reload<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &AgentReloadReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Agent reload plan").map_err(write_error_kind)?;
            writeln!(writer, "agent: {}", report.agent_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "lifecycle: {}", report.lifecycle).map_err(write_error_kind)?;
            writeln!(writer, "from_generation: {}", report.from_generation)
                .map_err(write_error_kind)?;
            writeln!(writer, "to_generation: {}", report.to_generation)
                .map_err(write_error_kind)?;
            writeln!(writer, "active_generation: {}", report.active_generation)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "previous_generation: {}",
                report.previous_generation
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", report.mutation_executed)
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("agent.reload", EXIT_OK, &agent_reload_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 将 Agent 状态报告编码为 JSON。
fn agent_status_json(report: &AgentStatusReport) -> String {
    let agents = report.agents.iter().map(agent_status_entry_json);
    format!(
        "{{\"status\":{},\"agents\":{}}}",
        json_string(&report.status),
        json_array(agents)
    )
}

/// 将单个 Agent 状态项编码为 JSON。
fn agent_status_entry_json(agent: &AgentStatusEntry) -> String {
    format!(
        "{{\"agent_id\":{},\"enabled\":{},\"lifecycle\":{},\"queued_events\":{},\"script\":{},\"script_version\":{},\"parent\":{},\"children\":{},\"subscriptions\":{},\"manifest_path\":{}}}",
        json_string(&agent.agent_id),
        agent.enabled,
        json_string(&agent.lifecycle),
        agent.queued_events,
        json_string(&agent.script),
        option_json(agent.script_version.as_deref()),
        option_json(agent.parent.as_deref()),
        json_array(agent.children.iter().map(|child| json_string(child))),
        json_array(agent.subscriptions.iter().map(|subscription| json_string(subscription))),
        json_string(&agent.manifest_path)
    )
}

/// 将 Agent 排空结果编码为 JSON。
fn agent_drain_json(report: &AgentDrainReport) -> String {
    format!(
        "{{\"agent_id\":{},\"status\":{},\"lifecycle\":{},\"enabled\":{},\"drain\":{},\"mutation_executed\":{},\"detail\":{}}}",
        json_string(&report.agent_id),
        json_string(&report.status),
        json_string(&report.lifecycle),
        report.enabled,
        drain_plan_json(&report.drain),
        report.mutation_executed,
        json_string(&report.detail)
    )
}

/// 将 Agent 热重载证据编码为 JSON。
fn agent_reload_json(report: &AgentReloadReport) -> String {
    format!(
        "{{\"agent_id\":{},\"status\":{},\"lifecycle\":{},\"enabled\":{},\"from_generation\":{},\"to_generation\":{},\"from_release\":{},\"to_release\":{},\"active_generation\":{},\"previous_generation\":{},\"previous_generation_state\":{},\"mutation_executed\":{},\"drain\":{},\"audit\":{}}}",
        json_string(&report.agent_id),
        json_string(&report.status),
        json_string(&report.lifecycle),
        report.enabled,
        json_string(&report.from_generation),
        json_string(&report.to_generation),
        json_string(&report.from_release),
        json_string(&report.to_release),
        json_string(&report.active_generation),
        json_string(&report.previous_generation),
        json_string(&report.previous_generation_state),
        report.mutation_executed,
        generation_drain_json(&report.drain),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将代际排空证据编码为 JSON。
fn generation_drain_json(evidence: &GenerationDrainEvidence) -> String {
    format!(
        "{{\"from_generation\":{},\"to_generation\":{},\"plan\":{},\"audit\":{}}}",
        json_string(evidence.from_generation.as_str()),
        json_string(evidence.to_generation.as_str()),
        drain_plan_json(&evidence.plan),
        json_array(evidence.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将排空计划的状态、步骤、风险和审计编码为 JSON。
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
