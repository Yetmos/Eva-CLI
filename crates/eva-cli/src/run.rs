//! CLI 命令解析、输出信封和进程退出码映射，是所有子命令共享的协议边界。
//! CLI command parsing, output envelopes, and process exit mapping.

// Adapter 子命令的参数、执行与序列化实现。
mod adapter_cmd;
// Agent 生命周期子命令实现。
mod agent_cmd;
// 备份创建子命令实现。
mod backup_cmd;
// Capability 列表、探测和调用实现。
mod capability_cmd;
// 配置校验子命令实现。
mod config_cmd;
// 守护进程启动与控制子命令实现。
mod daemon_cmd;
// 可信来源发现子命令实现。
mod discovery_cmd;
// Doctor 自检子命令的输出适配。
mod doctor_cmd;
// 类型化事件发布子命令实现。
mod emit_cmd;
// 硬件枚举、探测和绑定计划实现。
mod hardware_cmd;
// 项目与持久化状态检查子命令实现。
mod inspect_cmd;
// MCP 列表与探测子命令实现。
mod mcp_cmd;
// 请求级记忆上下文子命令实现。
mod memory_cmd;
// 可观测性烟测子命令实现。
mod observability_cmd;
// 发布门禁与证据检查子命令实现。
mod release_cmd;
// 恢复计划、应用和回滚子命令实现。
mod restore_cmd;
// 基础事件循环运行子命令实现。
mod run_cmd;
// Skill 列表与受控执行子命令实现。
mod skill_cmd;
// 发布快照创建与晋升计划实现。
mod snapshot_cmd;
// 持久化任务查询和取消子命令实现。
mod task_cmd;
// 代际升级检查与应用子命令实现。
mod upgrade_cmd;
// 版本与发布契约输出实现。
mod version_cmd;

use adapter_cmd::AdapterCommand;
use agent_cmd::AgentCommand;
use backup_cmd::BackupCommand;
use capability_cmd::CapabilityCommand;
use daemon_cmd::DaemonCommand;
use discovery_cmd::DiscoveryCommand;
use emit_cmd::EmitCommand;
use eva_core::{CapabilityName, ErrorKind, EvaError};
use eva_lifecycle::RollbackPlan;
use eva_observability::{SpanId, TraceFields};
use hardware_cmd::HardwareCommand;
use inspect_cmd::InspectOptions;
use mcp_cmd::McpCommand;
use memory_cmd::MemoryCommand;
use observability_cmd::ObservabilityCommand;
use release_cmd::ReleaseCommand;
use restore_cmd::RestoreCommand;
use run_cmd::RunOptions;
use skill_cmd::SkillCommand;
use snapshot_cmd::SnapshotCommand;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use task_cmd::TaskCommand;
use upgrade_cmd::UpgradeCommand;

/// 本模块的架构职责：统一解析命令，并把内部结果映射为稳定输出和退出码。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "parse Eva CLI commands and map results to stable output and exit codes";

/// 命令成功的稳定进程退出码。
const EXIT_OK: i32 = 0;
/// 未归类内部故障的稳定退出码。
const EXIT_INTERNAL: i32 = 1;
/// 配置、参数值或状态冲突错误的稳定退出码。
const EXIT_CONFIG: i32 = 2;
/// 策略拒绝操作的稳定退出码。
const EXIT_POLICY: i32 = 3;
/// Production release evidence was evaluated or rejected by the consumer policy.
const EXIT_PRODUCTION_BLOCKED: i32 = EXIT_POLICY;
/// 当前运行时版本不支持所请求能力的稳定退出码。
const EXIT_RUNTIME_UNAVAILABLE: i32 = 4;
/// 外部 provider 超时或不可用的稳定退出码。
const EXIT_EXTERNAL_UNAVAILABLE: i32 = 5;
/// CLI 语法或命令解析失败的标准 usage 退出码。
const EXIT_USAGE: i32 = 64;
/// 构建时注入的 crate 版本，是 `version` 输出的权威来源。
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 当前发布成熟度标签。
const RELEASE_STATUS: &str = "alpha";
/// 面向用户的历史发布标签；与 Cargo 版本分别维护以保留既有输出契约。
const RELEASE_LABEL: &str = "V1.11.5-alpha";
/// 汇总当前二进制包含的运行时能力代际，用于版本诊断而不参与功能开关。
const RELEASE_RUNTIME_MODE: &str =
    "in_memory_v1.0 + external_capability_v1.1 + context_v1.2 + hardware_v1.3 + lifecycle_v1.4 + release_v1.5 + durable_backend_v1.6.1 + durable_eventbus_v1.6.2 + durable_task_audit_artifact_v1.6.3 + durable_runtime_recovery_v1.6.4 + durable_diagnostics_v1.6.5 + lua_vm_execution_v1.7.1 + lua_host_bindings_v1.7.2 + lua_resource_limits_v1.7.3 + lua_hot_reload_lifecycle_v1.7.4 + adapter_mcp_skill_runtime_v1.8 + policy_discovery_memory_observability_v1.9 + hardware_apply_paths_v1.10 + release_distribution_cli_split_v1.11.4 + cli_runtime_commands_v1.11.5 + daemon_process_boundary_v1.12.1 + daemon_control_mailbox_v1.12.2 + durable_task_lifecycle_v1.12.3 + scheduler_retry_dispatch_v1.12.4 + agent_daemon_drain_reload_v1.12.5 + daemon_release_gate_v1.12.6 + provider_supervisor_v1.13.1 + provider_credential_session_v1.13.2 + provider_limits_circuit_breaker_v1.13.3 + provider_stream_artifact_v1.13.4 + provider_execution_recovery_v1.13.5 + mcp_http_auth_v1.13.6 + mcp_compat_matrix_v1.13.7 + provider_supervision_release_gate_v1.13.8 + restore_staged_mutation_planner_v1.14.1 + restore_file_mutation_engine_v1.14.2 + restore_rollback_apply_v1.14.3 + restore_operator_confirmation_v1.14.4 + service_manager_abstraction_v1.14.5 + hardware_os_permission_provider_v1.15.1 + hardware_hotplug_subscriber_v1.15.4 + hardware_safety_release_gate_v1.15.5 + memory_knowledge_maintenance_v1.15.6 + knowledge_retrieval_provider_v1.15.7 + memory_redaction_audit_v1.15.8 + runtime_audit_sink_wiring_v1.16.1 + tracing_subscriber_bridge_v1.16.2 + opentelemetry_sdk_exporter_v1.16.3 + observability_retention_policy_v1.16.4 + run_command_module_split_v1.17.1 + operator_execution_fields_v1.17.2 + operator_apply_text_v1.17.3 + json_contract_diff_suite_v1.17.4 + v1x_closure_gate_v1.17.6";
/// 当前发布声明支持的 CLI 契约清单，版本命令按此顺序稳定输出。
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
    "restore rollback",
    "upgrade check",
    "upgrade apply",
    "release check",
    "release security",
    "release perf",
    "release migration",
    "public JSON contract diff",
    "V1.x closure report",
    "cli command module split",
    "emit",
    "daemon start/status/stop/shutdown/submit/cancel/drain/reload",
    "agent status/drain/reload",
    "capability list/probe/call",
];

/// 根二进制使用的进程入口；执行后以命令映射出的稳定退出码结束进程。
/// Process entry point for the root binary shim.
pub fn run() {
    let exit_code = run_with_args(env::args_os().skip(1), &mut io::stdout(), &mut io::stderr());
    std::process::exit(exit_code);
}

/// 可测试的 CLI 入口，通过注入参数和输出 writer 避免测试直接终止进程。
///
/// 参数解析错误固定使用 usage 退出码；命令执行错误则按 `ErrorKind` 映射。
/// 输出失败不会覆盖原始业务退出语义，因为入口已尽力写出诊断后返回确定的码。
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

/// 将已解析命令分发到唯一子命令实现，保持解析与执行阶段的错误边界清晰。
/// `Help` 已在入口短路，若到达这里说明内部控制流违反约束，因此显式不可达。
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
        Command::Run(options) => run_cmd::execute_run(options, stdout, stderr),
        Command::Emit(command) => emit_cmd::execute_emit(command, stdout, stderr),
        Command::Daemon(command) => daemon_cmd::execute_daemon(*command, stdout, stderr),
        Command::Agent(command) => agent_cmd::execute_agent(command, stdout, stderr),
        Command::Capability(command) => capability_cmd::execute_capability(command, stdout, stderr),
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
/// 已解析的顶层命令；各变体携带完成语法校验后的专用选项。
enum Command {
    /// 打印静态帮助文本，不进入命令执行层。
    Help,
    /// 输出版本与发布契约。
    Version(
        /// 版本命令共享的项目根目录与输出格式。
        CommonOptions,
    ),
    /// 运行环境和配置自检。
    Doctor(
        /// 自检命令共享的项目根目录与输出格式。
        CommonOptions,
    ),
    /// 加载并验证拆分配置。
    ConfigValidate(
        /// 配置校验命令共享的项目根目录与输出格式。
        CommonOptions,
    ),
    /// 检查项目或持久化运行时状态。
    Inspect(
        /// 已解析的检查对象、项目根目录与输出格式。
        InspectOptions,
    ),
    /// 执行基础事件循环示例。
    Run(
        /// 已解析的运行步数、间隔、项目根目录与输出格式。
        RunOptions,
    ),
    /// 发布类型化入口事件。
    Emit(
        /// 已解析的事件类型、投递目标与载荷配置。
        EmitCommand,
    ),
    /// 启动或控制守护进程。
    Daemon(
        /// 已解析的 daemon 启动或控制子命令。
        Box<DaemonCommand>,
    ),
    /// 查询或变更 Agent 生命周期。
    Agent(
        /// 已解析的 Agent 状态、排空或重载子命令。
        AgentCommand,
    ),
    /// 列表、探测或调用 capability。
    Capability(
        /// 已解析的 capability 列表、探测或调用子命令。
        CapabilityCommand,
    ),
    /// 查询日志、状态或取消持久化任务。
    Task(
        /// 已解析的任务状态、日志或取消子命令。
        TaskCommand,
    ),
    /// 列表或探测 Adapter。
    Adapter(
        /// 已解析的 Adapter 列表或探测子命令。
        AdapterCommand,
    ),
    /// 列表或探测 MCP 工具。
    Mcp(
        /// 已解析的 MCP 工具列表或探测子命令。
        McpCommand,
    ),
    /// 列表或运行受控 Skill。
    Skill(
        /// 已解析的 Skill 列表或运行子命令。
        SkillCommand,
    ),
    /// 扫描可信发现来源。
    Discovery(
        /// 已解析的可信来源扫描子命令。
        DiscoveryCommand,
    ),
    /// 构建请求级记忆上下文。
    Memory(
        /// 已解析的记忆上下文构建子命令。
        MemoryCommand,
    ),
    /// 执行可观测性烟测。
    Observability(
        /// 已解析的可观测性烟测子命令。
        ObservabilityCommand,
    ),
    /// 列表、探测或规划硬件绑定。
    Hardware(
        /// 已解析的硬件列表、探测或绑定子命令。
        HardwareCommand,
    ),
    /// 创建并验证备份产物。
    Backup(
        /// 已解析的备份创建子命令。
        BackupCommand,
    ),
    /// 创建或规划晋升发布快照。
    Snapshot(
        /// 已解析的快照创建或晋升子命令。
        SnapshotCommand,
    ),
    /// 规划、应用或回滚恢复操作。
    Restore(
        /// 已解析的恢复规划、应用或回滚子命令。
        RestoreCommand,
    ),
    /// 检查或应用代际升级。
    Upgrade(
        /// 已解析的升级检查或应用子命令。
        UpgradeCommand,
    ),
    /// 执行发布门禁与兼容性检查。
    Release(
        /// 已解析的发布检查、安全评审、性能或迁移子命令。
        ReleaseCommand,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 所有子命令共享的项目定位和输出格式选项。
struct CommonOptions {
    /// 命令读取配置和相对资源的项目根目录。
    project_root: PathBuf,
    /// 文本或机器可读 JSON 输出模式。
    output: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// CLI 支持的稳定输出格式集合。
enum OutputFormat {
    /// 面向操作者的逐行文本。
    Text,
    /// 面向自动化的单行 JSON 信封。
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 对产物存储后端的可序列化引用，避免把具体 store 实现泄漏到 CLI 契约。
struct ArtifactStoreRef {
    /// 后端种类，例如 `filesystem` 或 `in_memory`。
    kind: String,
    /// 文件系统后端的可选路径；内存后端固定为空。
    path: Option<String>,
}

/// 将原始 OS 参数转换为 UTF-8 并解析为顶层命令。
///
/// 非 UTF-8 参数在协议边界直接失败；帮助标志优先于其他参数，以保持常见 CLI 体验。
/// 子命令的细节校验委托给对应模块，未知顶层命令保留原值作为错误上下文。
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
        "run" => Ok(Command::Run(run_cmd::parse_run_options(&args[1..])?)),
        "emit" => Ok(Command::Emit(emit_cmd::parse_emit_command(&args[1..])?)),
        "daemon" => Ok(Command::Daemon(Box::new(daemon_cmd::parse_daemon_command(
            &args[1..],
        )?))),
        "agent" => Ok(Command::Agent(agent_cmd::parse_agent_command(&args[1..])?)),
        "capability" => Ok(Command::Capability(
            capability_cmd::parse_capability_command(&args[1..])?,
        )),
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

/// 读取必须跟随选项名的值；缺失时返回带选项名的统一参数错误。
fn required_option<'a>(
    args: &'a [String],
    index: usize,
    name: &'static str,
) -> Result<&'a String, EvaError> {
    args.get(index)
        .ok_or_else(|| EvaError::invalid_argument(format!("missing value for {name}")))
}

/// 解析 u64 数值选项，并在失败时保留选项名与原始值上下文。
fn parse_u64_option(name: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::invalid_argument("option must be an unsigned integer")
            .with_context("option", name)
            .with_context("value", value)
    })
}

/// 解析平台大小的非负计数选项，并在失败时返回结构化参数错误。
fn parse_usize_option(name: &'static str, value: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::invalid_argument("option must be an unsigned integer")
            .with_context("option", name)
            .with_context("value", value)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 对互斥锁存储后端的可序列化引用，用于向操作者说明实际锁边界。
struct LockStoreRef {
    /// 后端种类，例如 `filesystem` 或 `in_memory`。
    kind: String,
    /// 文件系统锁目录；内存锁实现没有路径。
    path: Option<String>,
}

/// 根据可选路径描述实际产物存储实现；缺省路径明确表示进程内存后端。
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

/// 根据可选路径描述实际锁存储实现；该信息进入操作审计输出。
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

/// 按错误分类计算退出码并写出子命令错误，供各命令保持一致失败语义。
fn write_command_error<W: Write>(
    stderr: &mut W,
    output: OutputFormat,
    command: &str,
    error: &EvaError,
    trace: &TraceFields,
) -> Result<i32, EvaError> {
    let exit_code = exit_code_for_error(error);
    write_command_error_with_exit_code(stderr, output, command, exit_code, error, trace)
}

/// Write a command error with an explicitly selected public exit-code contract.
fn write_command_error_with_exit_code<W: Write>(
    stderr: &mut W,
    output: OutputFormat,
    command: &str,
    exit_code: i32,
    error: &EvaError,
    trace: &TraceFields,
) -> Result<i32, EvaError> {
    write_error(stderr, output, command, exit_code, error, trace)?;
    Ok(exit_code)
}

/// 解析项目根与输出格式两个公共选项，拒绝任何未消费参数。
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

/// 以当前工作目录构造公共选项；无法读取 cwd 属于内部环境故障而非用户语法错误。
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
    /// 解析稳定输出格式别名；`human` 仅作为 `text` 的兼容别名。
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

/// 将产物存储引用写为文本字段；只有文件系统后端才输出路径。
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

/// 将锁存储引用写为文本字段；保持与 JSON 引用相同的缺省语义。
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

/// 将产物存储引用编码为稳定 JSON 对象。
fn artifact_store_ref_json(artifact_store: &ArtifactStoreRef) -> String {
    format!(
        "{{\"kind\":{},\"path\":{}}}",
        json_string(&artifact_store.kind),
        option_json(artifact_store.path.as_deref())
    )
}

/// 将锁存储引用编码为稳定 JSON 对象。
fn lock_store_ref_json(lock_store: &LockStoreRef) -> String {
    format!(
        "{{\"kind\":{},\"path\":{}}}",
        json_string(&lock_store.kind),
        option_json(lock_store.path.as_deref())
    )
}

/// 将跨命令共享的回滚计划编码为稳定 JSON，完整保留步骤、风险和审计证据。
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

/// 将 capability 名称按输入顺序连接为文本列表，不重新排序路由优先级。
fn join_capabilities(capabilities: &[CapabilityName]) -> String {
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// 将可选字符串编码为 JSON 字符串或字面量 `null`。
fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

/// 按所选格式写出统一错误契约。
///
/// 文本模式逐项展示上下文和建议；JSON 模式使用稳定信封。任何 writer 故障都会转换为
/// `EvaError::Internal`，从而不会把底层 `io::Error` 泄漏出 CLI 边界。
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

/// 包装成功数据为所有 JSON 子命令共用的顶层信封；`data_json` 必须已是合法 JSON。
fn success_envelope(command: &str, exit_code: i32, data_json: &str, trace: &TraceFields) -> String {
    format!(
        "{{\"ok\":true,\"command\":{},\"exit_code\":{},\"data\":{},\"trace\":{}}}",
        json_string(command),
        exit_code,
        data_json,
        trace_json(trace)
    )
}

/// 将结构化 Eva 错误、退出码、建议和 trace 编码为稳定失败信封。
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

/// 为 CLI 操作创建最小 trace 字段；调用点只传静态、已知合法的 span ID。
fn trace_for(span_id: &str) -> TraceFields {
    TraceFields::default().with_span_id(
        SpanId::parse(span_id)
            .expect("static CLI span identifiers use the eva-observability character set"),
    )
}

/// 将 trace 键值编码为 JSON 对象，供成功和失败信封共享。
fn trace_json(trace: &TraceFields) -> String {
    let fields = trace
        .entries()
        .into_iter()
        .map(|(key, value)| format!("{}:{}", json_string(key), json_string(&value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", fields.join(","))
}

/// 对字符串执行完整 JSON 转义并加引号；所有手写 JSON 构造器必须经过此入口。
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

/// 将一组已经编码为 JSON 的值连接成数组，不对元素进行二次加引号。
pub(crate) fn json_array<I>(values: I) -> String
where
    I: IntoIterator<Item = String>,
{
    format!("[{}]", values.into_iter().collect::<Vec<_>>().join(","))
}

/// 使用平台原生展示规则生成路径文本，集中保持 CLI 路径输出一致。
pub(crate) fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// 将跨 crate 的错误分类映射为 CLI 稳定退出码。
///
/// 此映射是外部自动化契约：策略拒绝、运行时不支持和 provider 不可用必须保持可区分；
/// 输入、缺失资源和状态冲突共同归入配置类错误。
fn exit_code_for_error(error: &EvaError) -> i32 {
    match error.kind() {
        ErrorKind::PermissionDenied => EXIT_POLICY,
        ErrorKind::Timeout | ErrorKind::Unavailable => EXIT_EXTERNAL_UNAVAILABLE,
        ErrorKind::Unsupported => EXIT_RUNTIME_UNAVAILABLE,
        ErrorKind::InvalidArgument | ErrorKind::NotFound | ErrorKind::Conflict => EXIT_CONFIG,
        ErrorKind::Internal => EXIT_INTERNAL,
    }
}

/// 选择面向操作者的恢复建议；错误上下文中的显式建议优先于按分类生成的默认文本。
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

/// 将输出 writer 的 I/O 故障映射为带底层消息上下文的内部错误。
fn write_error_kind(error: io::Error) -> EvaError {
    EvaError::internal("failed to write CLI output").with_context("io_error", error.to_string())
}

/// 以稳定索引格式写出风险列表，便于人工阅读和脚本按行定位。
fn write_risk_lines_text<W: Write>(writer: &mut W, risks: &[String]) -> Result<(), EvaError> {
    writeln!(writer, "risk_count: {}", risks.len()).map_err(write_error_kind)?;
    for (index, risk) in risks.iter().enumerate() {
        writeln!(writer, "risk[{index}]: {risk}").map_err(write_error_kind)?;
    }
    Ok(())
}

/// 返回编译期静态帮助文本；与解析器支持的命令和退出码契约保持同步。
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
        "  eva emit <topic> [--event-id <id>] [--payload <text>|--payload-empty|--payload-bytes-hex <hex>] [--target-agent <id>|--target-capability <name>|--target-adapter <id>] [--request-id <id>] [--generation <id>] [--correlation-id <event_id>] [--causation-id <event_id>] [--durable-backend <path>] [--output text|json]\n",
        "  eva daemon start [--foreground|--background] [--startup-timeout-ms <ms>] [--dev] [--no-shutdown-after-smoke] [--durable-backend <path>] [--state-dir <path>] [--lock-dir <path>] [--pid-dir <path>] [--observability-backend <path>] [--project <path>] [--output text|json]\n",
        "  eva daemon status [--state-dir <path>] [--lock-dir <path>] [--pid-dir <path>] [--request-id <id>] [--control-timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva daemon shutdown|stop [--state-dir <path>] [--lock-dir <path>] [--pid-dir <path>] [--request-id <id>] [--control-timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva daemon submit [--task <id>] [--kind <task.kind> --agent <id> (--input <text> | --artifact-ref <key> --artifact-digest <sha256>) [--idempotency-key <key>] [--max-attempts <n>] [--retry-backoff-ms <ms>] [--attempt-timeout-ms <ms>]] [--durable-backend <path>] [--state-dir <path>] [--lock-dir <path>] [--pid-dir <path>] [--request-id <id>] [--control-timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva daemon cancel --task <id> [--reason <text>] [--durable-backend <path>] [--state-dir <path>] [--lock-dir <path>] [--pid-dir <path>] [--request-id <id>] [--control-timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva daemon drain|reload [--plan <id>] [--generation <id>] [--state-dir <path>] [--lock-dir <path>] [--pid-dir <path>] [--request-id <id>] [--control-timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva agent status [--agent <id>] [--project <path>] [--output text|json]\n",
        "  eva agent drain --agent <id> [--generation <id>] [--inflight <n>] [--timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva agent reload --agent <id> [--from-generation <id>] [--to-generation <id>] [--from-release <ref>] [--to-release <ref>] [--inflight <n>] [--timeout-ms <ms>] [--project <path>] [--output text|json]\n",
        "  eva capability list [--project <path>] [--output text|json]\n",
        "  eva capability probe [<capability>|--capability <name>] [--provider <id>] [--project <path>] [--output text|json]\n",
        "  eva capability call [<capability>|--capability <name>] [--provider <id>] [--input <text>] [--request-id <id>] [--dry-run|--confirm <request-id>] [--project <path>] [--output text|json]\n",
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
        "  eva observability smoke [--backend <path>] [--tracing-sink jsonl|dev-console] [--otel-endpoint <url>] [--otel-auth-header <value>] [--otel-batch-size <n>] [--otel-timeout-ms <ms>] [--otel-drop-policy drop-new|drop-oldest] [--otel-max-metric-labels <n>] [--project <path>] [--output text|json]\n",
        "  eva hardware list [--project <path>] [--output text|json]\n",
        "  eva hardware probe [--adapter <id>] [--project <path>] [--output text|json]\n",
        "  eva hardware bind [--adapter <id>] [--request-id <id>] [--apply] [--project <path>] [--output text|json]\n",
        "  eva backup create [--artifact-id <id>] [--request-id <id>] [--reason <text>] [--artifact-store <path>] [--dry-run] [--encrypt] [--project <path>] [--output text|json]\n",
        "  eva snapshot create [--snapshot-id <id>] [--release <ref>] [--role pre_release|post_release] [--artifact-store <path>] [--project <path>] [--output text|json]\n",
        "  eva snapshot promote --snapshot-id <id> --confirm <snapshot_id> [--release <ref>] [--artifact-store <path>] [--project <path>] [--output text|json]\n",
        "  eva restore plan [--snapshot-id <id>] [--release <ref>] [--artifact-store <path>] [--project <path>] [--output text|json]\n",
        "  eva restore apply --plan <path> --confirm <plan_id> --artifact-store <path> --lock-store <path> [--dry-run] [--owner <id>] [--health healthy|failed] [--project <path>] [--output text|json]\n",
        "  eva restore rollback --plan <path> --confirm <plan_id> --artifact-store <path> --lock-store <path> [--transaction-log <path>] [--owner <id>] [--health healthy|failed] [--project <path>] [--output text|json]\n",
        "  eva upgrade check [--from-generation <id>] [--to-generation <id>] [--from-release <ref>] [--to-release <ref>] [--project <path>] [--output text|json]\n",
        "  eva upgrade apply --plan <path> --confirm <plan_id> --lock-store <path> [--state-store <path>] [--runtime-binary <path>] [--health healthy|failed|unavailable] [--owner <id>] [--project <path>] [--output text|json]\n",
        "  eva release check [--target all|windows|linux|macos] [--scope alpha|production] [--evidence-manifest <path>] [--expected-source-commit <sha>] [--expected-run-id <id>] [--expected-run-attempt <n>] [--expected-manifest-digest <sha256>] [--artifact-evidence <path>] [--distribution-evidence <path>] [--security-scan-evidence <path>] [--benchmark-evidence <path>] [--project <path>] [--output text|json]\n",
        "  eva release security [--project <path>] [--output text|json]\n",
        "  eva release perf [--benchmark-evidence <path>] [--project <path>] [--output text|json]\n",
        "  eva release migration [--from-version <semver>] [--to-version <semver>] [--project <path>] [--output text|json]\n\n",
        "Commands:\n",
        "  version          Print the current alpha release version and supported contracts.\n",
        "  doctor           Check workspace, configuration roots, schema files, and runtime boundaries.\n",
        "  config validate  Load eva.yaml plus split manifests and report stable diagnostics.\n",
        "  inspect          Show project surfaces or durable backend diagnostics without mutating runtime state.\n",
        "  run              Execute the V1.0-compatible in-memory basic event loop and persist the latest task report under .eva/tasks or a durable backend task store.\n",
        "  emit             Publish a typed Event to the in-memory or durable EventBus boundary.\n",
        "  daemon           Verify V1.12.1 local daemon config, lock, state, pid, durable backend, policy, observability, and shutdown boundaries.\n",
        "  agent            Report Agent lifecycle status, drain plans, and daemon-backed reload/drain mutation when available.\n",
        "  capability       List, probe, and dry-run or confirmed-call capability provider routes.\n",
        "  task             Inspect or cancel the latest persisted basic task report from .eva/tasks or a durable backend task store.\n",
        "  adapter          List and probe authorized Adapter handles derived from manifests.\n",
        "  mcp              List and probe allowlisted MCP tools without starting external servers.\n",
        "  skill            List and run controlled workflow skill runners.\n",
        "  discovery        Scan trusted configuration sources and return candidates without granting runtime handles.\n",
        "  memory           Build request-scoped private/global memory plus knowledge context for one Agent.\n",
        "  hardware         List, probe, and plan hardware bindings without opening raw I/O.\n",
        "  backup           Create and verify a V1.4 backup artifact, optionally in a filesystem ArtifactStore.\n",
        "  snapshot         Capture or plan promotion for a release snapshot without moving release pointer.\n",
        "  restore          Produce restore plans, apply staged mutations, or rollback failed staged mutations under explicit gates.\n",
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
/// 跨子命令的 CLI 输出、退出码、持久化与高风险门禁集成回归测试。
mod tests {
    use super::*;
    use eva_storage::{ArtifactRecord, ArtifactStore, FileSystemArtifactStore};
    use std::fs;
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// 返回仓库根目录，供 CLI 集成测试读取真实示例配置和 schema。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 以内存 stdout/stderr 运行 CLI，返回退出码和 UTF-8 输出以便断言完整协议。
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

    /// 为并行测试创建包含进程和时间后缀的独立临时目录。
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

    /// 从测试生成的稳定 JSON 文本中提取已知字符串字段；仅用于受控 fixture 输出。
    fn extract_json_value(data: &str, prefix: &str) -> String {
        let start = data.find(prefix).expect("json value prefix should exist") + prefix.len();
        let end = data[start..]
            .find('"')
            .expect("json string value should terminate");
        data[start..start + end].to_owned()
    }

    /// 等待 daemon 状态、锁和 pid 边界全部就绪，限制轮询次数以防测试无限挂起。
    fn wait_for_daemon_files(state: &Path, locks: &Path, pids: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state_running = fs::read_to_string(state.join("daemon.state"))
                .map(|data| data.contains("status=running"))
                .unwrap_or(false);
            if state_running
                && locks.join("daemon.lock").is_file()
                && pids.join("daemon.pid").is_file()
            {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "daemon files did not become available: state_running={state_running}, lock_present={}, pid_present={}",
                    locks.join("daemon.lock").is_file(),
                    pids.join("daemon.pid").is_file()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 递归复制测试项目 fixture，保留文件层级但不共享可变状态。
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

    /// 创建 restore apply 计划、目标备份和前置备份的最小一致 fixture。
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

    /// 复制项目并只放开 restore apply 策略，用于隔离门禁之后的事务路径。
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

    /// 复制项目并只放开 upgrade/pointer 策略，用于测试锁和 handoff 行为。
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

    /// 创建包含确认 ID 和代际信息的 upgrade apply 计划文件。
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

    /// 创建可切换签名有效性的发布产物证据 fixture。
    fn release_artifact_evidence_fixture(name: &str, signed: bool) -> (PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        let evidence_path = root.join("release-artifact.evidence");
        fs::create_dir_all(&root).unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let key = eva_release::ReleaseArtifactSigningKey::local_development();
        let artifact = eva_release::ReleaseArtifactSubject::new(
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
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
            "1.11.5-alpha",
            "v1.11.5-alpha",
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

    /// 创建指定 package 状态的跨平台分发与安装烟测证据。
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
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.11.5-alpha",
            package_status,
        )
        .unwrap();
        let evidence = eva_release::ReleaseDistributionEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            commit,
            install_doc,
            install_doc,
            install_doc,
            vec![
                smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.11.5-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                ),
                smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                ),
                smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.11.5-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                ),
            ],
            vec![dry_run],
        )
        .unwrap();
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        (root, evidence_path)
    }

    /// 创建指定扫描状态和最高严重度的安全扫描证据。
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
            "1.11.5-alpha",
            "v1.11.5-alpha",
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

    /// 创建指定状态和观测延迟的 benchmark 证据，用于预算边界测试。
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
            "release check wall time",
            5_000,
            observed_ms,
            3,
            "target/release/eva release check --output json",
            "github-actions-ubuntu-latest",
        )
        .unwrap();
        let evidence = eva_release::ReleaseBenchmarkEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            commit,
            status,
            vec![measurement],
        )
        .unwrap();
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        (root, evidence_path)
    }

    /// 创建统一 benchmark manifest、canonical evidence 和匹配 envelope。
    fn release_benchmark_manifest_fixture(
        name: &str,
        scope: eva_release::ReleaseEvidenceScope,
    ) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let (root, evidence_path) = release_benchmark_evidence_fixture(name, "passed", 120);
        let evidence_data = fs::read_to_string(&evidence_path).unwrap();
        let evidence =
            eva_release::ReleaseBenchmarkEvidence::parse_manifest(&evidence_data).unwrap();
        let envelope = evidence
            .to_envelope(
                eva_release::EvidenceKind::Measurement,
                "test:release-benchmark",
                "test-runner",
                "eva-cli-tests",
                1_784_073_600_000,
            )
            .unwrap();
        let envelope_path = root.join("release-benchmark.envelope");
        fs::write(&envelope_path, envelope.to_manifest()).unwrap();
        let manifest = eva_release::ReleaseEvidenceManifest::new(
            scope,
            &evidence.source_commit,
            vec![eva_release::ReleaseEvidenceManifestEntry::new(
                eva_release::ReleaseEvidenceType::Benchmark,
                "release-benchmark.evidence",
                "release-benchmark.envelope",
                None,
            )
            .unwrap()],
        )
        .unwrap();
        let manifest_path = root.join("release-evidence.manifest");
        fs::write(&manifest_path, manifest.to_manifest()).unwrap();
        (root, manifest_path, evidence_path, envelope_path)
    }

    /// 创建使用真实 subject bytes 和一致 artifact digest/size 的统一 manifest。
    fn release_artifact_manifest_fixture(
        name: &str,
        scope: eva_release::ReleaseEvidenceScope,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let root = test_temp_dir(name);
        fs::create_dir_all(&root).unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let subject_bytes = b"real release artifact bytes".to_vec();
        let record = ArtifactRecord::new("release/test-artifact", subject_bytes.clone());
        let subject_path = root.join("eva-test.tar.gz");
        fs::write(&subject_path, &subject_bytes).unwrap();

        let key = eva_release::ReleaseArtifactSigningKey::local_development();
        let artifact = eva_release::ReleaseArtifactSubject::new(
            "eva-test.tar.gz",
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "eva",
            &record.digest,
            subject_bytes.len() as u64,
            true,
        )
        .unwrap();
        let provenance = eva_release::ReleaseProvenanceEvidence::new(
            "github-actions",
            commit,
            "cargo build --release --locked",
            "release",
            "spdx:eva-test.spdx.json",
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
            "1.11.5-alpha",
            "v1.11.5-alpha",
            commit,
            artifact,
            provenance,
            signature,
        )
        .unwrap();
        evidence.signature = evidence.sign(&key);
        let evidence_path = root.join("release-artifact.evidence");
        fs::write(&evidence_path, evidence.to_manifest()).unwrap();
        let envelope = evidence
            .to_envelope(
                eva_release::EvidenceKind::Measurement,
                "test:release-artifact",
                "test-runner",
                "eva-cli-tests",
                1_784_073_600_000,
            )
            .unwrap();
        fs::write(
            root.join("release-artifact.envelope"),
            envelope.to_manifest(),
        )
        .unwrap();
        let manifest = eva_release::ReleaseEvidenceManifest::new(
            scope,
            commit,
            vec![eva_release::ReleaseEvidenceManifestEntry::new(
                eva_release::ReleaseEvidenceType::Artifact,
                "release-artifact.evidence",
                "release-artifact.envelope",
                Some("eva-test.tar.gz".to_owned()),
            )
            .unwrap()],
        )
        .unwrap();
        let manifest_path = root.join("release-evidence.manifest");
        fs::write(&manifest_path, manifest.to_manifest()).unwrap();
        (root, manifest_path, subject_path)
    }

    /// 创建四类 evidence、逐类型可信执行器和新鲜时间戳的完整 production manifest。
    fn release_complete_production_manifest_fixture(
        name: &str,
    ) -> (PathBuf, PathBuf, PathBuf, PathBuf, String) {
        let (root, manifest_path, subject_path) = release_artifact_manifest_fixture(
            &format!("{name}-artifact"),
            eva_release::ReleaseEvidenceScope::Production,
        );
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_millis()
            .saturating_sub(1_000);

        let artifact_path = root.join("release-artifact.evidence");
        let artifact = eva_release::ReleaseArtifactEvidence::parse_manifest(
            &fs::read_to_string(&artifact_path).unwrap(),
        )
        .unwrap();
        let artifact_envelope = artifact
            .to_envelope(
                eva_release::EvidenceKind::Measurement,
                "cli-test:release-artifact",
                "ubuntu-x86_64",
                "github-actions:release-artifact/123/1/artifact",
                timestamp,
            )
            .unwrap();
        let artifact_envelope_path = root.join("release-artifact.envelope");
        fs::write(&artifact_envelope_path, artifact_envelope.to_manifest()).unwrap();

        let (distribution_root, distribution_source) =
            release_distribution_evidence_fixture(&format!("{name}-distribution"), "passed");
        let distribution_path = root.join("release-distribution.evidence");
        fs::copy(&distribution_source, &distribution_path).unwrap();
        let distribution = eva_release::ReleaseDistributionEvidence::parse_manifest(
            &fs::read_to_string(&distribution_path).unwrap(),
        )
        .unwrap();
        let distribution_envelope = distribution
            .to_envelope(
                eva_release::EvidenceKind::Measurement,
                "cli-test:release-distribution",
                "release-matrix",
                "github-actions:release-distribution/123/1/distribution",
                timestamp,
            )
            .unwrap();
        let distribution_envelope_path = root.join("release-distribution.envelope");
        fs::write(
            &distribution_envelope_path,
            distribution_envelope.to_manifest(),
        )
        .unwrap();

        let (security_root, security_source) =
            release_security_scan_evidence_fixture(&format!("{name}-security"), "passed", None);
        let security_path = root.join("release-security-scan.evidence");
        fs::copy(&security_source, &security_path).unwrap();
        let security = eva_release::ReleaseSecurityScanEvidence::parse_manifest(
            &fs::read_to_string(&security_path).unwrap(),
        )
        .unwrap();
        let security_envelope = security
            .to_envelope(
                eva_release::EvidenceKind::Measurement,
                "cli-test:release-security-scan",
                "ubuntu-x86_64",
                "github-actions:release-security-scan/123/1/security",
                timestamp,
            )
            .unwrap();
        let security_envelope_path = root.join("release-security-scan.envelope");
        fs::write(&security_envelope_path, security_envelope.to_manifest()).unwrap();

        let (benchmark_root, benchmark_source) =
            release_benchmark_evidence_fixture(&format!("{name}-benchmark"), "passed", 120);
        let benchmark_path = root.join("release-benchmark.evidence");
        fs::copy(&benchmark_source, &benchmark_path).unwrap();
        let benchmark = eva_release::ReleaseBenchmarkEvidence::parse_manifest(
            &fs::read_to_string(&benchmark_path).unwrap(),
        )
        .unwrap();
        let benchmark_envelope = benchmark
            .to_envelope(
                eva_release::EvidenceKind::Measurement,
                "cli-test:release-benchmark",
                "ubuntu-x86_64",
                "github-actions:release-benchmark/123/1/benchmark",
                timestamp,
            )
            .unwrap();
        let benchmark_envelope_path = root.join("release-benchmark.envelope");
        fs::write(&benchmark_envelope_path, benchmark_envelope.to_manifest()).unwrap();

        let manifest = eva_release::ReleaseEvidenceManifest::new(
            eva_release::ReleaseEvidenceScope::Production,
            commit,
            vec![
                eva_release::ReleaseEvidenceManifestEntry::new(
                    eva_release::ReleaseEvidenceType::Artifact,
                    "release-artifact.evidence",
                    "release-artifact.envelope",
                    Some("eva-test.tar.gz".to_owned()),
                )
                .unwrap(),
                eva_release::ReleaseEvidenceManifestEntry::new(
                    eva_release::ReleaseEvidenceType::Distribution,
                    "release-distribution.evidence",
                    "release-distribution.envelope",
                    None,
                )
                .unwrap(),
                eva_release::ReleaseEvidenceManifestEntry::new(
                    eva_release::ReleaseEvidenceType::SecurityScan,
                    "release-security-scan.evidence",
                    "release-security-scan.envelope",
                    None,
                )
                .unwrap(),
                eva_release::ReleaseEvidenceManifestEntry::new(
                    eva_release::ReleaseEvidenceType::Benchmark,
                    "release-benchmark.evidence",
                    "release-benchmark.envelope",
                    None,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        fs::write(&manifest_path, manifest.to_manifest()).unwrap();
        let manifest_digest = bind_release_manifest_envelopes(&root, &manifest_path);

        fs::remove_dir_all(distribution_root).unwrap();
        fs::remove_dir_all(security_root).unwrap();
        fs::remove_dir_all(benchmark_root).unwrap();
        (
            root,
            manifest_path,
            subject_path,
            benchmark_envelope_path,
            manifest_digest,
        )
    }

    /// Bind canonical envelope bytes into the manifest and return its canonical digest.
    fn bind_release_manifest_envelopes(root: &Path, manifest_path: &Path) -> String {
        let mut manifest = eva_release::ReleaseEvidenceManifest::parse_manifest(
            &fs::read_to_string(manifest_path).unwrap(),
        )
        .unwrap();
        for entry in &mut manifest.entries {
            let envelope = eva_release::EvidenceEnvelope::parse_manifest(
                &fs::read_to_string(root.join(&entry.envelope_path)).unwrap(),
            )
            .unwrap();
            entry.envelope_digest = Some(envelope.canonical_digest());
        }
        fs::write(manifest_path, manifest.to_manifest()).unwrap();
        manifest.canonical_digest()
    }

    #[cfg(unix)]
    /// 创建可执行的 runtime `--version` 替身，验证 upgrade 二进制烟测协议。
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
    /// 验证示例项目配置校验的 JSON 成功信封和计数字段。
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
    /// 验证 schema 违规会返回 usage/config 语义并保留具体规则上下文。
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
    /// 验证 inspect 文本报告包含只读 no-op runtime 摘要。
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
    /// 验证未知顶层命令使用稳定 usage 退出码且错误写入 stderr。
    fn unknown_command_is_usage_error() {
        let (exit_code, _stdout, stderr) = run_cli(&["missing"]);

        assert_eq!(exit_code, EXIT_USAGE);
        assert!(stderr.contains("unknown command"));
    }

    #[test]
    /// 验证文本和 JSON emit 均发布同一强类型事件及 metadata 契约。
    fn emit_text_and_json_publish_typed_event() {
        let (exit_code, stdout, stderr) = run_cli(&[
            "emit",
            "/input/user",
            "--event-id",
            "evt-cli-text",
            "--payload",
            "hello",
            "--target-agent",
            "root-agent",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("Event emitted"));
        assert!(stdout.contains("event: evt-cli-text"));
        assert!(stdout.contains("topic: /input/user"));
        assert!(stdout.contains("target: agent:root-agent"));
        assert!(stderr.is_empty());

        let (exit_code, stdout, stderr) = run_cli(&[
            "emit",
            "--topic",
            "/input/user",
            "--event-id",
            "evt-cli-json",
            "--payload",
            r#"{"kind":"demo"}"#,
            "--target-capability",
            "repo.summary",
            "--request-id",
            "req-emit-1",
            "--generation",
            "gen-emit-1",
            "--correlation-id",
            "evt-root",
            "--causation-id",
            "evt-parent",
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"emit\""));
        assert!(stdout.contains("\"status\":\"published\""));
        assert!(stdout.contains("\"event_id\":\"evt-cli-json\""));
        assert!(stdout.contains("\"topic\":\"/input/user\""));
        assert!(stdout.contains("\"kind\":\"capability\""));
        assert!(stdout.contains("\"value\":\"repo.summary\""));
        assert!(stdout.contains("\"payload\":{\"kind\":\"text\""));
        assert!(stdout.contains("\"request_id\":\"req-emit-1\""));
        assert!(stderr.is_empty());
    }

    #[test]
    /// 验证指定 durable backend 时 emit 真实持久化事件并报告后端路径。
    fn emit_can_publish_to_durable_backend() {
        let root = test_temp_dir("emit-durable");
        let (exit_code, stdout, stderr) = run_cli(&[
            "emit",
            "/input/user",
            "--event-id",
            "evt-durable",
            "--payload-bytes-hex",
            "68656c6c6f",
            "--target-adapter",
            "adapter-cli",
            "--durable-backend",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"backend\":{\"kind\":\"durable\""));
        assert!(stdout.contains("\"event_id\":\"evt-durable\""));
        assert!(stdout.contains("\"payload\":{\"kind\":\"bytes\",\"size\":5}"));
        assert!(root.join("backend.manifest").is_file());
        let event_path = root.join("events/log/00000000000000000001.event");
        assert!(event_path.is_file());
        let event_data = fs::read_to_string(&event_path).unwrap();
        assert!(event_data.contains("sequence=1"));
        assert!(event_data.contains("topic=2f696e7075742f75736572"));
        assert!(event_data.contains("target_kind=adapter"));
        assert!(event_data.contains("payload_kind=bytes"));
        assert!(event_data.contains("payload_value=68656c6c6f"));
        assert!(stderr.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证 daemon 前台烟测报告锁、状态、pid、恢复、策略和关闭边界。
    fn daemon_start_foreground_smoke_reports_verified_boundaries() {
        let project = workspace_root();
        let root = test_temp_dir("daemon-start");
        let durable = root.join("durable");
        let state = root.join("state");
        let locks = root.join("locks");
        let pids = root.join("pids");
        let observability = root.join("observability");

        let (exit_code, stdout, stderr) = run_cli(&[
            "daemon",
            "start",
            "--foreground",
            "--dev",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--observability-backend",
            observability.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"daemon.start\""));
        assert!(stdout.contains("\"status\":\"stopped\""));
        assert!(stdout.contains("\"mode\":\"foreground_dev\""));
        assert!(stdout.contains("\"provider_processes_started\":false"));
        assert!(stdout.contains("\"durable_backend\":{"));
        assert!(stdout.contains("\"recovery\":{"));
        assert!(stdout.contains("\"scanned_provider_processes\":0"));
        assert!(stdout.contains("\"policy\":{\"status\":\"verified\""));
        assert!(stdout.contains("\"observability\":{"));
        assert!(stdout.contains("\"hardware_hotplug\":{"));
        assert!(stdout.contains("\"watcher_kind\":\"manifest_snapshot\""));
        assert!(stdout.contains("\"raw_handles_exposed\":false"));
        assert!(stdout.contains("\"hardware_hotplug_state_file\":"));
        assert!(stdout.contains("\"lease_file\":"));
        assert!(stdout.contains("\"lease\":{\"state\":\"released\""));
        assert!(stdout.contains("\"generation\":1"));
        assert!(stdout.contains("\"memory_maintenance\":{"));
        assert!(stdout.contains("\"memory_gc\":{"));
        assert!(stdout.contains("\"knowledge_rebuild\":{"));
        assert!(stdout.contains("\"memory.maintenance:ttl_gc_completed\""));
        assert!(stdout.contains("\"shutdown\":{\"already_shutdown\":false"));
        assert!(state.join("daemon.state").is_file());
        assert!(state.join("hardware-hotplug.state").is_file());
        assert!(durable
            .join("state")
            .join("memory")
            .join("memory-gc.checkpoint")
            .is_file());
        assert!(durable
            .join("state")
            .join("knowledge")
            .join("knowledge-rebuild.checkpoint")
            .is_file());
        assert!(locks.join("daemon.lock").is_file());
        assert!(locks.join("daemon.lease").is_file());
        assert!(!pids.join("daemon.pid").exists());
        assert!(observability.join("audit.jsonl").is_file());
        assert!(stderr.is_empty());

        let (status_exit, status_stdout, status_stderr) = run_cli(&[
            "daemon",
            "status",
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(status_exit, EXIT_EXTERNAL_UNAVAILABLE);
        assert!(status_stdout.is_empty());
        assert!(status_stderr.contains("\"command\":\"daemon.status\""));
        assert!(status_stderr.contains("\"kind\":\"unavailable\""));
        assert!(status_stderr.contains("\"key\":\"lease_freshness\",\"value\":\"stale\""));
        assert!(status_stderr.contains("\"key\":\"lease_heartbeat_age_ms\""));
        assert!(status_stderr.contains("\"trace_id\",\"value\":\"request_id:req-daemon-status\""));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证 CLI 可通过 control mailbox 完成 daemon 状态查询和关闭往返。
    fn daemon_control_status_and_shutdown_round_trip_via_cli() {
        let project = workspace_root();
        let root = test_temp_dir("daemon-control-cli");
        let durable = root.join("durable");
        let state = root.join("state");
        let locks = root.join("locks");
        let pids = root.join("pids");
        let observability = root.join("observability");
        let daemon_project = eva_config::load_project_config(&project).unwrap();
        let daemon_options = eva_runtime::DaemonStartOptions {
            durable_backend: durable.clone(),
            state_dir: state.clone(),
            lock_dir: locks.clone(),
            pid_dir: pids.clone(),
            observability_backend: observability.clone(),
            foreground: true,
            dev_mode: true,
            shutdown_after_smoke: false,
        };
        let daemon = std::thread::spawn(move || {
            eva_runtime::start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(eva_core::RequestId::parse("req-daemon-cli-loop").unwrap()),
            )
        });

        wait_for_daemon_files(&state, &locks, &pids);

        let (status_exit, status_stdout, status_stderr) = run_cli(&[
            "daemon",
            "status",
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(status_exit, EXIT_OK, "{status_stderr}");
        assert!(status_stdout.contains("\"command\":\"daemon.status\""));
        assert!(status_stdout.contains("\"operation\":\"status\""));
        assert!(status_stdout.contains("\"trace_id\":\"request_id:req-daemon-status\""));
        assert!(status_stdout.contains("\"daemon_available\":true"));
        assert!(status_stdout.contains("\"status\":\"running\""));
        assert!(status_stdout.contains("\"lease\":{\"state\":\"active\""));
        assert!(status_stdout.contains("\"owner_live\":true"));
        assert!(status_stdout.contains("\"expired\":false"));
        assert!(status_stdout.contains("\"freshness\":\"live\""));
        assert!(status_stdout.contains("\"heartbeat_age_ms\":"));
        assert!(status_stdout.contains("\"response_file\""));
        assert!(status_stderr.is_empty());

        let (text_status_exit, text_status_stdout, text_status_stderr) = run_cli(&[
            "daemon",
            "status",
            "--request-id",
            "req-daemon-status-text",
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "text",
        ]);
        assert_eq!(text_status_exit, EXIT_OK, "{text_status_stderr}");
        assert!(text_status_stdout.contains("freshness=live"));
        assert!(text_status_stdout.contains("heartbeat_age_ms="));
        assert!(text_status_stderr.is_empty());

        let (submit_exit, submit_stdout, submit_stderr) = run_cli(&[
            "daemon",
            "submit",
            "--task",
            "req-daemon-cli-task",
            "--kind",
            "runtime.echo",
            "--agent",
            "root-agent",
            "--input",
            "line one\n%|line two",
            "--idempotency-key",
            "idem-daemon-cli-task",
            "--max-attempts",
            "3",
            "--retry-backoff-ms",
            "250",
            "--attempt-timeout-ms",
            "5000",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(submit_exit, EXIT_OK, "{submit_stderr}");
        assert!(submit_stdout.contains("\"command\":\"daemon.submit\""));
        assert!(submit_stdout.contains("\"operation\":\"submit_task\""));
        assert!(submit_stdout.contains("\"task_id\":\"req-daemon-cli-task\""));
        assert!(submit_stderr.is_empty());

        let (task_exit, task_stdout, task_stderr) = run_cli(&[
            "task",
            "status",
            "--task",
            "req-daemon-cli-task",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(task_exit, EXIT_OK, "{task_stderr}");
        assert!(task_stdout.contains("\"kind\":\"runtime.echo\""));
        assert!(task_stdout.contains("\"agent_id\":\"root-agent\""));
        assert!(task_stdout.contains("\"input_kind\":\"inline\""));
        assert!(task_stdout.contains("\"idempotency_key\":\"idem-daemon-cli-task\""));
        assert!(task_stdout.contains("\"retry_backoff_ms\":250"));
        assert!(task_stdout.contains("\"attempt_timeout_ms\":5000"));
        assert!(!task_stdout.contains("line one"));
        assert!(task_stderr.is_empty());

        let mut completed_task_stdout = task_stdout;
        for _ in 0..100 {
            if completed_task_stdout.contains("\"status\":\"completed\"") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            let (task_exit, task_stdout, task_stderr) = run_cli(&[
                "task",
                "status",
                "--task",
                "req-daemon-cli-task",
                "--durable-backend",
                durable.to_str().unwrap(),
                "--project",
                project.to_str().unwrap(),
                "--output",
                "json",
            ]);
            assert_eq!(task_exit, EXIT_OK, "{task_stderr}");
            completed_task_stdout = task_stdout;
        }
        assert!(completed_task_stdout.contains("\"status\":\"completed\""));
        assert!(completed_task_stdout.contains("\"execution\":{\"owner\":\"daemon:"));
        assert!(completed_task_stdout.contains("\"result_digest\":\"sha256:"));
        assert!(completed_task_stdout.contains("\"result_size_bytes\":19"));
        assert!(completed_task_stdout.contains("\"freshness\":\"not_applicable\""));
        assert!(completed_task_stdout.contains("\"heartbeat_age_ms\":"));
        assert!(!completed_task_stdout.contains("cancel_token"));

        let (legacy_exit, legacy_stdout, legacy_stderr) = run_cli(&[
            "daemon",
            "submit",
            "--task",
            "req-daemon-cli-default-task",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(legacy_exit, EXIT_OK, "{legacy_stderr}");
        assert!(legacy_stdout.contains("\"task_id\":\"req-daemon-cli-default-task\""));
        assert!(legacy_stderr.is_empty());

        let (cancel_exit, cancel_stdout, cancel_stderr) = run_cli(&[
            "daemon",
            "cancel",
            "--task",
            "req-daemon-cli-task",
            "--reason",
            "operator stop",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(cancel_exit, EXIT_OK, "{cancel_stderr}");
        assert!(cancel_stdout.contains("\"command\":\"daemon.cancel\""));
        assert!(cancel_stdout.contains("\"operation\":\"cancel_task\""));
        assert!(cancel_stdout.contains("\"task_id\":\"req-daemon-cli-task\""));
        assert!(cancel_stderr.is_empty());
        let task_state =
            fs::read_to_string(durable.join("tasks").join("req-daemon-cli-task.task")).unwrap();
        assert!(task_state.starts_with("format=eva.task-state.v4\n"));
        assert!(task_state.contains("envelope_kind=runtime.echo"));
        assert!(task_state.contains("envelope_agent_id=root-agent"));
        assert!(task_state.contains("envelope_input_kind=inline"));
        assert!(task_state.contains("envelope_idempotency_key=idem-daemon-cli-task"));
        assert!(task_state.contains("envelope_max_attempts=3"));
        assert!(task_state.contains("envelope_retry_backoff_ms=250"));
        assert!(task_state.contains("envelope_attempt_timeout_ms=5000"));
        assert!(task_state.contains("status=completed"));
        assert!(task_state.contains("execution_owner="));
        assert!(task_state.contains("result_digest=sha256:"));
        assert!(task_state.contains("result_size_bytes=19"));
        assert!(task_state.contains("cancel_requested=true"));
        assert!(task_state.contains("cancel_accepted=false"));
        let legacy_state = fs::read_to_string(
            durable
                .join("tasks")
                .join("req-daemon-cli-default-task.task"),
        )
        .unwrap();
        assert!(legacy_state.contains("envelope_kind=legacy.submit"));
        assert!(legacy_state.contains("envelope_input_kind=inline"));
        assert!(legacy_state.contains("envelope_max_attempts=1"));

        let (shutdown_exit, shutdown_stdout, shutdown_stderr) = run_cli(&[
            "daemon",
            "shutdown",
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(shutdown_exit, EXIT_OK, "{shutdown_stderr}");
        assert!(shutdown_stdout.contains("\"command\":\"daemon.shutdown\""));
        assert!(shutdown_stdout.contains("\"operation\":\"shutdown\""));
        assert!(shutdown_stdout.contains("\"mutation_executed\":true"));
        assert!(shutdown_stdout.contains("\"shutdown\":{\"already_shutdown\":false"));
        assert!(shutdown_stderr.is_empty());

        let report = daemon.join().unwrap().unwrap();
        assert_eq!(report.status, "stopped");
        assert!(locks.join("daemon.lock").is_file());
        assert!(locks.join("daemon.lease").is_file());
        assert!(!pids.join("daemon.pid").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 非 canonical artifact digest 在创建 mailbox 请求前返回参数错误。
    fn daemon_submit_rejects_invalid_task_envelope_before_mailbox_write() {
        let project = workspace_root();
        let root = test_temp_dir("daemon-submit-invalid-envelope");
        let state = root.join("state");

        let (exit, stdout, stderr) = run_cli(&[
            "daemon",
            "submit",
            "--task",
            "req-daemon-cli-invalid-envelope",
            "--kind",
            "runtime.echo",
            "--agent",
            "root-agent",
            "--artifact-ref",
            "tasks/input-1",
            "--artifact-digest",
            "sha256:BAD",
            "--state-dir",
            state.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"kind\":\"invalid_argument\""));
        assert!(!state.join("control").join("requests").exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 验证 daemon 锁冲突在状态写入前失败，不留下伪启动记录。
    fn daemon_start_lock_conflict_does_not_write_state() {
        let project = workspace_root();
        let root = test_temp_dir("daemon-lock");
        let durable = root.join("durable");
        let state = root.join("state");
        let locks = root.join("locks");
        let pids = root.join("pids");
        let observability = root.join("observability");
        fs::create_dir_all(&locks).unwrap();
        fs::write(locks.join("daemon.lock"), "pid=1\n").unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "daemon",
            "start",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--observability-backend",
            observability.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"daemon.start\""));
        assert!(stderr.contains("\"kind\":\"conflict\""));
        assert!(stderr.contains("daemon lock anchor format is corrupt or unsupported"));
        assert!(!state.join("daemon.state").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证无效 durable backend 在 daemon 状态持久化前阻断启动。
    fn daemon_start_bad_durable_backend_does_not_write_state() {
        let project = workspace_root();
        let root = test_temp_dir("daemon-bad-durable");
        let durable = root.join("durable");
        let state = root.join("state");
        let locks = root.join("locks");
        let pids = root.join("pids");
        let observability = root.join("observability");
        fs::create_dir_all(&durable).unwrap();
        fs::write(
            durable.join("backend.manifest"),
            "schema_version=999\nlayout_version=eva.durable.v1\nevent_dir=events\nstate_dir=state\ntask_dir=tasks\naudit_dir=audit\nartifact_dir=artifacts\n",
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "daemon",
            "start",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--observability-backend",
            observability.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"daemon.start\""));
        assert!(stderr.contains("durable backend schema version mismatch"));
        assert!(!state.join("daemon.state").exists());
        assert!(!locks.join("daemon.lock").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证 Agent 状态 JSON 合并 manifest、层级、订阅与生命周期字段。
    fn agent_status_reports_manifest_and_lifecycle_json() {
        let root = workspace_root();
        let (text_exit, text_stdout, text_stderr) = run_cli(&[
            "agent",
            "status",
            "--agent",
            "root-agent",
            "--project",
            root.to_str().unwrap(),
        ]);

        assert_eq!(text_exit, EXIT_OK, "{text_stderr}");
        assert!(text_stdout.contains("Agent status"));
        assert!(text_stdout.contains("root-agent enabled=true lifecycle=running"));
        assert!(text_stderr.is_empty());

        let (exit_code, stdout, stderr) = run_cli(&[
            "agent",
            "status",
            "--agent",
            "root-agent",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"agent.status\""));
        assert!(stdout.contains("\"status\":\"ready\""));
        assert!(stdout.contains("\"agent_id\":\"root-agent\""));
        assert!(stdout.contains("\"enabled\":true"));
        assert!(stdout.contains("\"lifecycle\":\"running\""));
        assert!(stdout.contains("\"queued_events\":0"));
        assert!(stdout.contains("\"subscriptions\":[\"/sys\"]"));
        assert!(stderr.is_empty());
    }

    #[test]
    /// 验证 daemon 不可用时 Agent drain 只输出计划且 mutation 标记为 false。
    fn agent_drain_outputs_drain_plan_without_runtime_mutation() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "agent",
            "drain",
            "--agent",
            "root-agent",
            "--generation",
            "gen-agent-old",
            "--inflight",
            "0",
            "--timeout-ms",
            "30000",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"agent.drain\""));
        assert!(stdout.contains("\"agent_id\":\"root-agent\""));
        assert!(stdout.contains("\"status\":\"draining\""));
        assert!(stdout.contains("\"lifecycle\":\"draining\""));
        assert!(stdout.contains("\"generation_id\":\"gen-agent-old\""));
        assert!(stdout.contains("\"accepts_new_work\":false"));
        assert!(stdout.contains("\"status\":\"completed\""));
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stderr.is_empty());
    }

    #[test]
    /// 验证本地 Agent reload 报告源/目标代际和前代最终状态。
    fn agent_reload_outputs_generation_swap_evidence() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "agent",
            "reload",
            "--agent",
            "root-agent",
            "--from-generation",
            "gen-old",
            "--to-generation",
            "gen-new",
            "--from-release",
            "1.11.4-alpha",
            "--to-release",
            "1.11.5-alpha",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"agent.reload\""));
        assert!(stdout.contains("\"agent_id\":\"root-agent\""));
        assert!(stdout.contains("\"from_generation\":\"gen-old\""));
        assert!(stdout.contains("\"to_generation\":\"gen-new\""));
        assert!(stdout.contains("\"active_generation\":\"gen-new\""));
        assert!(stdout.contains("\"previous_generation\":\"gen-old\""));
        assert!(stdout.contains("\"previous_generation_state\":\"draining\""));
        assert!(stdout.contains("generation:gen-old:draining_after_swap_to:gen-new"));
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stderr.is_empty());
    }

    #[test]
    /// 验证 daemon 可用时 Agent drain/reload 使用真实 control mutation 路径。
    fn agent_drain_and_reload_use_daemon_mutation_when_available() {
        let project = workspace_root();
        let root = test_temp_dir("agent-daemon-mutation");
        let durable = root.join("durable");
        let state = root.join("state");
        let locks = root.join("locks");
        let pids = root.join("pids");
        let observability = root.join("observability");
        let daemon_project = eva_config::load_project_config(&project).unwrap();
        let daemon_options = eva_runtime::DaemonStartOptions {
            durable_backend: durable.clone(),
            state_dir: state.clone(),
            lock_dir: locks.clone(),
            pid_dir: pids.clone(),
            observability_backend: observability,
            foreground: true,
            dev_mode: true,
            shutdown_after_smoke: false,
        };
        let daemon = std::thread::spawn(move || {
            eva_runtime::start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(eva_core::RequestId::parse("req-agent-daemon-loop").unwrap()),
            )
        });

        wait_for_daemon_files(&state, &locks, &pids);

        let (drain_exit, drain_stdout, drain_stderr) = run_cli(&[
            "agent",
            "drain",
            "--agent",
            "root-agent",
            "--generation",
            "gen-agent-old",
            "--inflight",
            "1",
            "--timeout-ms",
            "30000",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(drain_exit, EXIT_OK, "{drain_stderr}");
        assert!(drain_stdout.contains("\"command\":\"agent.drain\""));
        assert!(drain_stdout.contains("\"agent_id\":\"root-agent\""));
        assert!(drain_stdout.contains("\"generation_id\":\"gen-agent-old\""));
        assert!(drain_stdout.contains("\"accepts_new_work\":false"));
        assert!(drain_stdout.contains("\"mutation_executed\":true"));
        assert!(drain_stdout.contains("agent drain mutation recorded"));
        assert!(drain_stderr.is_empty());

        let (reload_exit, reload_stdout, reload_stderr) = run_cli(&[
            "agent",
            "reload",
            "--agent",
            "root-agent",
            "--from-generation",
            "gen-old",
            "--to-generation",
            "gen-new",
            "--from-release",
            "1.11.4-alpha",
            "--to-release",
            "1.11.5-alpha",
            "--durable-backend",
            durable.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(reload_exit, EXIT_OK, "{reload_stderr}");
        assert!(reload_stdout.contains("\"command\":\"agent.reload\""));
        assert!(reload_stdout.contains("\"status\":\"reloaded\""));
        assert!(reload_stdout.contains("\"active_generation\":\"gen-new\""));
        assert!(reload_stdout.contains("\"previous_generation\":\"gen-old\""));
        assert!(reload_stdout.contains("\"previous_generation_state\":\"draining\""));
        assert!(reload_stdout.contains("\"mutation_executed\":true"));
        assert!(reload_stdout.contains("scheduler:new_work_generation:gen-new"));
        assert!(!reload_stdout.contains("planned_without_daemon_mutation"));
        assert!(reload_stderr.is_empty());

        let control_state = fs::read_to_string(state.join("agent-control.state")).unwrap();
        assert!(control_state.contains("mutation_executed=true"));
        assert!(control_state.contains("drain_accepts_new_work=false"));

        let (shutdown_exit, shutdown_stdout, shutdown_stderr) = run_cli(&[
            "daemon",
            "shutdown",
            "--state-dir",
            state.to_str().unwrap(),
            "--lock-dir",
            locks.to_str().unwrap(),
            "--pid-dir",
            pids.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(shutdown_exit, EXIT_OK, "{shutdown_stderr}");
        assert!(shutdown_stdout.contains("\"operation\":\"shutdown\""));
        let report = daemon.join().unwrap().unwrap();
        assert_eq!(report.status, "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证 capability list/probe JSON 包含 provider 计划、来源和权限门禁。
    fn capability_list_and_probe_report_provider_plan_json() {
        let root = workspace_root();
        let (list_exit, list_stdout, list_stderr) = run_cli(&[
            "capability",
            "list",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(list_exit, EXIT_OK, "{list_stderr}");
        assert!(list_stdout.contains("\"command\":\"capability.list\""));
        assert!(list_stdout.contains("\"capability\":\"repo.analyze\""));
        assert!(list_stdout.contains("\"provider\":\"codex-cli\""));
        assert!(list_stdout.contains("\"source\":\"default_provider\""));
        assert!(list_stderr.is_empty());

        let (probe_exit, probe_stdout, probe_stderr) = run_cli(&[
            "capability",
            "probe",
            "repo.analyze",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(probe_exit, EXIT_OK, "{probe_stderr}");
        assert!(probe_stdout.contains("\"command\":\"capability.probe\""));
        assert!(probe_stdout.contains("\"status\":\"ready\""));
        assert!(probe_stdout.contains("\"capability\":\"repo.analyze\""));
        assert!(probe_stdout.contains("\"manifest_allowed_providers\":[\"codex-cli\"]"));
        assert!(probe_stdout.contains("\"permission_gate\":{\"allowed\":true"));
        assert!(probe_stderr.is_empty());
    }

    #[test]
    /// 验证 capability call 默认 dry-run，只有匹配确认后才执行内置调用。
    fn capability_call_dry_run_and_confirmed_builtin_paths() {
        let root = workspace_root();
        let (dry_exit, dry_stdout, dry_stderr) = run_cli(&[
            "capability",
            "call",
            "config.lint",
            "--input",
            "config",
            "--request-id",
            "req-cap-dry",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(dry_exit, EXIT_OK, "{dry_stderr}");
        assert!(dry_stdout.contains("\"command\":\"capability.call\""));
        assert!(dry_stdout.contains("\"status\":\"dry_run\""));
        assert!(dry_stdout.contains("\"confirmed\":false"));
        assert!(dry_stdout.contains("\"invocation_executed\":false"));
        assert!(dry_stdout.contains("\"mutation_executed\":false"));
        assert!(dry_stdout.contains("\"response\":null"));
        assert!(dry_stderr.is_empty());

        let (run_exit, run_stdout, run_stderr) = run_cli(&[
            "capability",
            "call",
            "config.lint",
            "--input",
            "config",
            "--request-id",
            "req-cap-run",
            "--confirm",
            "req-cap-run",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(run_exit, EXIT_OK, "{run_stderr}");
        assert!(run_stdout.contains("\"status\":\"executed\""));
        assert!(run_stdout.contains("\"confirmed\":true"));
        assert!(run_stdout.contains("\"invocation_executed\":true"));
        assert!(run_stdout.contains("\"mutation_executed\":false"));
        assert!(run_stdout.contains("\"response\":{\"request_id\":\"req-cap-run\""));
        assert!(run_stdout.contains("\"status\":\"completed\""));
        assert!(run_stdout.contains("valid"));
        assert!(run_stderr.is_empty());
    }

    #[test]
    /// 验证显式 provider 不在 manifest allowlist 时在调用前被拒绝。
    fn capability_call_rejects_provider_outside_manifest_allowlist() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "capability",
            "call",
            "repo.analyze",
            "--provider",
            "claude-api",
            "--request-id",
            "req-cap-deny",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_POLICY);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"command\":\"capability.call\""));
        assert!(stderr.contains("\"kind\":\"permission_denied\""));
        assert!(stderr.contains("adapter provider is not explicitly allowed"));
        assert!(stderr.contains("\"key\":\"gate\",\"value\":\"adapter\""));
    }

    #[test]
    /// 验证 basic 示例完整运行并输出可查询的 JSON 任务报告。
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
    /// 验证项目本地任务快照可被 status/logs 查询并接受取消更新。
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
        assert!(status_stdout.contains("\"freshness\":\"not_applicable\""));

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
    fn task_status_reports_stale_fenced_heartbeat_in_json_and_text() {
        use eva_storage::{
            DurableBackendOptions, FileSystemDurableBackend, FileSystemTaskStateStore,
            TaskAttemptPolicySnapshot, TaskEnvelopeSnapshot, TaskStateSnapshot,
        };

        let project = workspace_root();
        let durable = test_temp_dir("stale-task-status");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&durable)).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"stale".to_vec(),
            "idem-stale-task-status",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope("req-stale-task-status", envelope)
                    .unwrap(),
            )
            .unwrap();
        store
            .try_claim_queued(
                "req-stale-task-status",
                "daemon:stale-task-status",
                "cancel.stale-task-status",
                1,
            )
            .unwrap()
            .unwrap();
        drop(store);
        drop(backend);

        for output in ["json", "text"] {
            let (exit, stdout, stderr) = run_cli(&[
                "task",
                "status",
                "--task",
                "req-stale-task-status",
                "--durable-backend",
                durable.to_str().unwrap(),
                "--project",
                project.to_str().unwrap(),
                "--output",
                output,
            ]);
            assert_eq!(exit, EXIT_OK, "{stderr}");
            match output {
                "json" => assert!(stdout.contains("\"freshness\":\"stale\"")),
                "text" => assert!(stdout.contains("freshness: stale")),
                _ => unreachable!(),
            }
            assert!(stdout.contains("heartbeat_age_ms"));
            assert!(!stdout.contains("cancel.stale-task-status"));
            assert!(stderr.is_empty());
        }

        fs::remove_dir_all(durable).unwrap();
    }

    #[test]
    /// 验证任务命令在 durable backend 上保持与本地 store 相同的读写契约。
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
    /// 验证 durable inspect 报告布局、迁移、事件日志和 dead-letter 诊断。
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
    /// 验证启动即取消的 basic 运行持久化 cancelled 终态而非成功状态。
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
    /// 验证公共 JSON 字符串编码器正确转义引号、反斜线和控制字符。
    fn json_string_escapes_control_characters() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    /// 验证版本文本与 JSON 同时报告既有 alpha 标签和 CLI runtime 契约。
    fn version_text_and_json_report_v1115_cli_runtime_commands_alpha() {
        let (text_exit, text_stdout, text_stderr) = run_cli(&["--version"]);
        assert_eq!(text_exit, EXIT_OK, "{text_stderr}");
        assert!(text_stdout.contains("eva 1.11.5-alpha"));
        assert!(text_stdout.contains("V1.11.5-alpha"));
        assert!(text_stdout.contains("status: alpha"));

        let (json_exit, json_stdout, json_stderr) = run_cli(&["version", "--output", "json"]);
        assert_eq!(json_exit, EXIT_OK, "{json_stderr}");
        assert!(json_stdout.contains("\"command\":\"version\""));
        assert!(json_stdout.contains("\"version\":\"1.11.5-alpha\""));
        assert!(json_stdout.contains("\"release\":\"V1.11.5-alpha\""));
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
        assert!(json_stdout.contains("release_distribution_cli_split_v1.11.4"));
        assert!(json_stdout.contains("cli_runtime_commands_v1.11.5"));
        assert!(json_stdout.contains("durable_task_lifecycle_v1.12.3"));
        assert!(json_stdout.contains("scheduler_retry_dispatch_v1.12.4"));
        assert!(json_stdout.contains("agent_daemon_drain_reload_v1.12.5"));
        assert!(json_stdout.contains("daemon_release_gate_v1.12.6"));
        assert!(json_stdout.contains("provider_supervisor_v1.13.1"));
        assert!(json_stdout.contains("provider_credential_session_v1.13.2"));
        assert!(json_stdout.contains("provider_limits_circuit_breaker_v1.13.3"));
        assert!(json_stdout.contains("provider_stream_artifact_v1.13.4"));
        assert!(json_stdout.contains("provider_execution_recovery_v1.13.5"));
        assert!(json_stdout.contains("mcp_http_auth_v1.13.6"));
        assert!(json_stdout.contains("mcp_compat_matrix_v1.13.7"));
        assert!(json_stdout.contains("provider_supervision_release_gate_v1.13.8"));
        assert!(json_stdout.contains("restore_staged_mutation_planner_v1.14.1"));
        assert!(json_stdout.contains("restore_file_mutation_engine_v1.14.2"));
        assert!(json_stdout.contains("restore_rollback_apply_v1.14.3"));
        assert!(json_stdout.contains("restore_operator_confirmation_v1.14.4"));
        assert!(json_stdout.contains("service_manager_abstraction_v1.14.5"));
        assert!(json_stdout.contains("hardware_os_permission_provider_v1.15.1"));
        assert!(json_stdout.contains("hardware_hotplug_subscriber_v1.15.4"));
        assert!(json_stdout.contains("hardware_safety_release_gate_v1.15.5"));
        assert!(json_stdout.contains("memory_knowledge_maintenance_v1.15.6"));
        assert!(json_stdout.contains("knowledge_retrieval_provider_v1.15.7"));
        assert!(json_stdout.contains("memory_redaction_audit_v1.15.8"));
        assert!(json_stdout.contains("runtime_audit_sink_wiring_v1.16.1"));
        assert!(json_stdout.contains("tracing_subscriber_bridge_v1.16.2"));
        assert!(json_stdout.contains("opentelemetry_sdk_exporter_v1.16.3"));
        assert!(json_stdout.contains("observability_retention_policy_v1.16.4"));
        assert!(json_stdout.contains("run_command_module_split_v1.17.1"));
        assert!(json_stdout.contains("operator_execution_fields_v1.17.2"));
        assert!(json_stdout.contains("operator_apply_text_v1.17.3"));
        assert!(json_stdout.contains("json_contract_diff_suite_v1.17.4"));
        assert!(json_stdout.contains("v1x_closure_gate_v1.17.6"));
        assert!(json_stdout.contains("cli command module split"));
        assert!(json_stdout.contains("public JSON contract diff"));
        assert!(json_stdout.contains("V1.x closure report"));
        assert!(json_stdout.contains("emit"));
        assert!(json_stdout.contains("agent status/drain/reload"));
        assert!(json_stdout.contains("capability list/probe/call"));
        assert!(json_stdout.contains("restore apply"));
        assert!(json_stdout.contains("restore rollback"));
        assert!(json_stdout.contains("release check"));
    }

    #[test]
    /// 验证发布 readiness JSON 显式包含 durable recovery gate 和 blocker 统计。
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
        assert!(stdout.contains("\"id\":\"REL-DAEMON-RUNTIME-001\""));
        assert!(stdout.contains("\"domain\":\"daemon_runtime\""));
        assert!(stdout.contains("run_control_loop"));
        assert!(stdout.contains("daemon_control_loop_ticks_scheduler_retry_once"));
        assert!(stdout.contains("agent_drain_and_reload_use_daemon_mutation_when_available"));
        assert!(stdout.contains("daemon_runtime_readiness_gate_ready"));
        assert!(stdout.contains("\"id\":\"REL-HARDWARE-SAFETY-001\""));
        assert!(stdout.contains("\"domain\":\"hardware_safety\""));
        assert!(stdout.contains("hardware.safety.release_mode:alpha_simulator_only"));
        assert!(stdout.contains("real_hardware_fixture:not_required_for_alpha"));
        assert!(stdout.contains("hardware_safety_release_gate_ready"));
        assert!(stdout.contains("\"id\":\"REL-JSON-CONTRACT-001\""));
        assert!(stdout.contains("\"domain\":\"cli_json_contract\""));
        assert!(stdout.contains("scripts/validate-cli-json-contracts.ps1"));
        assert!(stdout.contains("contracts/cli-json/version.json"));
        assert!(stdout.contains("public_json_contract_diff_ready"));
        assert!(stdout.contains("\"id\":\"REL-OBSERVABILITY-POLICY-001\""));
        assert!(stdout.contains("\"domain\":\"observability_policy\""));
        assert!(stdout.contains("observability_retention_policy_v1.16.4"));
        assert!(stdout.contains("observability_policy_release_gate_ready"));
        assert!(stdout.contains("\"id\":\"REL-V1X-CLOSURE-001\""));
        assert!(stdout.contains("\"domain\":\"v1x_closure\""));
        assert!(stdout.contains("\"closure\""));
        assert!(stdout.contains("\"status\":\"ready_with_external_blockers\""));
        assert!(stdout.contains("closure.required_gate:REL-JSON-CONTRACT-001"));
        assert!(stdout.contains("production_signing_attestation_credentials"));
        assert!(stdout.contains("v1x_closure_report_ready"));
        assert!(stdout.contains("\"evidence_scope\":\"alpha\""));
        assert!(stdout.contains("\"source\":\"none\""));
        assert!(stdout.contains("\"integrity_status\":\"not_applicable\""));
        assert!(stdout.contains("\"manifest_digest\":null"));
        assert!(stdout.contains("\"manifest_digest_source\":\"none\""));
        assert!(stdout.contains(
            "\"provenance\":{\"evidence_type\":null,\"source\":null,\"source_commit\":null,\"environment\":null,\"executor\":null,\"timestamp_ms\":null,\"subject_digest\":null,\"envelope_digest\":null}"
        ));
        assert!(stdout.contains("\"remediation\":["));
    }

    #[test]
    /// 验证 production 缺统一 manifest 时走 release.check JSON 错误信封并非零退出。
    fn production_release_check_requires_evidence_manifest() {
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("\"ok\":false"));
        assert!(stderr.contains("\"command\":\"release.check\""));
        assert!(stderr.contains("\"exit_code\":3"));
        assert!(stderr.contains("production release check requires an evidence manifest"));
        assert!(stderr.contains("production_evidence_manifest_required"));
    }

    #[test]
    /// 验证 alpha 与 production manifest 均不能被另一 CLI scope 冒充消费。
    fn release_check_rejects_bidirectional_scope_mismatch() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        for (name, manifest_scope, cli_scope) in [
            (
                "release-manifest-production-as-alpha",
                eva_release::ReleaseEvidenceScope::Production,
                "alpha",
            ),
            (
                "release-manifest-alpha-as-production",
                eva_release::ReleaseEvidenceScope::Alpha,
                "production",
            ),
        ] {
            let (evidence_root, manifest_path, _, _) =
                release_benchmark_manifest_fixture(name, manifest_scope);
            let (exit_code, stdout, stderr) = run_cli(&[
                "release",
                "check",
                "--scope",
                cli_scope,
                "--evidence-manifest",
                manifest_path.to_str().unwrap(),
                "--expected-source-commit",
                commit,
                "--output",
                "json",
            ]);

            let expected_exit = if cli_scope == "production" {
                EXIT_PRODUCTION_BLOCKED
            } else {
                EXIT_CONFIG
            };
            assert_eq!(exit_code, expected_exit);
            assert!(stdout.is_empty());
            assert!(stderr.contains("manifest scope does not match CLI scope"));
            fs::remove_dir_all(evidence_root).unwrap();
        }
    }

    #[test]
    /// 验证 production manifest 不能用自身 source_commit 代替外部可信提交。
    fn production_release_check_requires_external_expected_commit() {
        let (evidence_root, manifest_path, _, _) = release_benchmark_manifest_fixture(
            "release-manifest-missing-expected-commit",
            eva_release::ReleaseEvidenceScope::Production,
        );
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("requires --expected-source-commit"));
        assert!(stderr.contains("production_evidence_trusted_commit_required"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证统一 production benchmark manifest 通过 scope、提交和 digest 校验后进入 gate。
    fn production_release_check_consumes_verified_manifest() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let (evidence_root, manifest_path, _, _, manifest_digest) =
            release_complete_production_manifest_fixture("release-manifest-production-verified");
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            commit,
            "--expected-run-id",
            "123",
            "--expected-run-attempt",
            "1",
            "--expected-manifest-digest",
            manifest_digest.as_str(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED, "{stderr}");
        assert!(stdout.contains("\"evidence_scope\":\"production\""));
        assert!(stdout.contains("\"source\":\"manifest\""));
        assert!(stdout.contains("\"integrity_status\":\"verified\""));
        assert!(stdout.contains("\"expected_commit_source\":\"external_option\""));
        assert!(stdout.contains(&format!("\"manifest_digest\":\"{manifest_digest}\"")));
        assert!(stdout.contains("\"manifest_digest_source\":\"external_option\""));
        assert!(stdout.contains("\"id\":\"REL-BENCHMARK-001\""));
        assert!(stdout
            .contains("\"domain\":\"production_benchmark\",\"evidence_kind\":\"measurement\""));
        assert!(stdout.contains("\"exit_code\":3"));
        assert!(stdout.contains("\"status\":\"blocked\""));
        for (evidence_type, source, executor) in [
            (
                "artifact",
                "cli-test:release-artifact",
                "github-actions:release-artifact/123/1/artifact",
            ),
            (
                "distribution",
                "cli-test:release-distribution",
                "github-actions:release-distribution/123/1/distribution",
            ),
            (
                "security_scan",
                "cli-test:release-security-scan",
                "github-actions:release-security-scan/123/1/security",
            ),
            (
                "benchmark",
                "cli-test:release-benchmark",
                "github-actions:release-benchmark/123/1/benchmark",
            ),
        ] {
            assert!(stdout.contains(&format!(
                "\"provenance\":{{\"evidence_type\":\"{evidence_type}\",\"source\":\"{source}\",\"source_commit\":\"{commit}\""
            )));
            assert!(stdout.contains(&format!("\"executor\":\"{executor}\"")));
        }
        assert!(stdout.contains("evidence_kind_not_measured:REL-MCP-COMPAT-001:fixture"));
        assert!(!stdout.contains("REL-PRODUCTION-EVIDENCE-POLICY-001"));
        assert!(!stdout.contains(manifest_path.to_str().unwrap()));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 production 不从 envelope 自报 executor 推导可信 run identity。
    fn production_release_check_requires_external_run_identity() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let (evidence_root, manifest_path, _, _, _) =
            release_complete_production_manifest_fixture("release-manifest-missing-trusted-run");
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            commit,
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("production_evidence_trusted_run_required"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证缺失、非法或不匹配的外部 manifest/run trust context 使用稳定 blocker。
    fn production_release_check_rejects_invalid_external_trust_context() {
        enum DigestInput {
            Missing,
            Invalid,
            Mismatch,
            Valid,
        }
        let cases = [
            (
                "missing-manifest-digest",
                "123",
                DigestInput::Missing,
                "production_evidence_manifest_digest_required",
            ),
            (
                "invalid-manifest-digest",
                "123",
                DigestInput::Invalid,
                "production_evidence_manifest_digest_invalid",
            ),
            (
                "mismatched-manifest-digest",
                "123",
                DigestInput::Mismatch,
                "production_evidence_manifest_digest_mismatch",
            ),
            (
                "invalid-run-id",
                "run-123",
                DigestInput::Valid,
                "production_evidence_trusted_run_required",
            ),
        ];
        let commit = "0123456789abcdef0123456789abcdef01234567";

        for (name, run_id, digest_input, expected_blocker) in cases {
            let (evidence_root, manifest_path, _, _, manifest_digest) =
                release_complete_production_manifest_fixture(&format!(
                    "release-external-trust-{name}"
                ));
            let digest = match digest_input {
                DigestInput::Missing => None,
                DigestInput::Invalid => Some("sha256:bad"),
                DigestInput::Mismatch => {
                    Some("sha256:0000000000000000000000000000000000000000000000000000000000000000")
                }
                DigestInput::Valid => Some(manifest_digest.as_str()),
            };
            let mut args = vec![
                "release",
                "check",
                "--scope",
                "production",
                "--evidence-manifest",
                manifest_path.to_str().unwrap(),
                "--expected-source-commit",
                commit,
                "--expected-run-id",
                run_id,
                "--expected-run-attempt",
                "1",
            ];
            if let Some(digest) = digest {
                args.extend(["--expected-manifest-digest", digest]);
            }
            args.extend(["--output", "json"]);
            let (exit_code, stdout, stderr) = run_cli(&args);

            assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED, "{name}: {stderr}");
            assert!(stdout.is_empty(), "{name}: {stdout}");
            assert!(
                stderr.contains(expected_blocker),
                "{name}: missing {expected_blocker}: {stderr}"
            );
            fs::remove_dir_all(evidence_root).unwrap();
        }
    }

    #[test]
    /// 验证 production manifest 必须覆盖当前 consumer 要求的全部 evidence 类型。
    fn production_release_check_rejects_incomplete_manifest() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let (evidence_root, manifest_path, _, _, _) =
            release_complete_production_manifest_fixture("release-manifest-production-incomplete");
        let mut manifest = eva_release::ReleaseEvidenceManifest::parse_manifest(
            &fs::read_to_string(&manifest_path).unwrap(),
        )
        .unwrap();
        manifest
            .entries
            .retain(|entry| entry.evidence_type != eva_release::ReleaseEvidenceType::Benchmark);
        fs::write(&manifest_path, manifest.to_manifest()).unwrap();
        let manifest_digest = bind_release_manifest_envelopes(&evidence_root, &manifest_path);
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            commit,
            "--expected-run-id",
            "123",
            "--expected-run-attempt",
            "1",
            "--expected-manifest-digest",
            manifest_digest.as_str(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("production_evidence_coverage_missing"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 CLI production loader 使用可信系统时间、measurement 和执行器 policy。
    fn production_release_check_enforces_freshness_kind_and_executor_policy() {
        enum Mutation {
            Stale,
            Future,
            NonMeasurement,
            Untrusted,
            WrongRun,
            WrongAttempt,
        }
        let cases = [
            ("stale", Mutation::Stale, "production_evidence_stale"),
            (
                "future",
                Mutation::Future,
                "production_evidence_future_timestamp",
            ),
            (
                "non-measurement",
                Mutation::NonMeasurement,
                "production_evidence_kind_not_measurement",
            ),
            (
                "untrusted",
                Mutation::Untrusted,
                "production_evidence_executor_untrusted",
            ),
            (
                "wrong-run",
                Mutation::WrongRun,
                "production_evidence_executor_untrusted",
            ),
            (
                "wrong-attempt",
                Mutation::WrongAttempt,
                "production_evidence_executor_untrusted",
            ),
        ];
        let commit = "0123456789abcdef0123456789abcdef01234567";

        for (name, mutation, expected_blocker) in cases {
            let (evidence_root, manifest_path, _, benchmark_envelope_path, _) =
                release_complete_production_manifest_fixture(&format!(
                    "release-production-policy-{name}"
                ));
            let mut envelope = eva_release::EvidenceEnvelope::parse_manifest(
                &fs::read_to_string(&benchmark_envelope_path).unwrap(),
            )
            .unwrap();
            match mutation {
                Mutation::Stale => envelope.timestamp = 1,
                Mutation::Future => {
                    envelope.timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                        + eva_release::PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS
                        + 60_000;
                }
                Mutation::NonMeasurement => {
                    envelope.kind = eva_release::EvidenceKind::Fixture;
                }
                Mutation::Untrusted => {
                    envelope.executor = "local:release-benchmark/run-123".to_owned();
                }
                Mutation::WrongRun => {
                    envelope.executor =
                        "github-actions:release-benchmark/124/1/benchmark".to_owned();
                }
                Mutation::WrongAttempt => {
                    envelope.executor =
                        "github-actions:release-benchmark/123/2/benchmark".to_owned();
                }
            }
            fs::write(&benchmark_envelope_path, envelope.to_manifest()).unwrap();
            let manifest_digest = bind_release_manifest_envelopes(&evidence_root, &manifest_path);

            let (exit_code, stdout, stderr) = run_cli(&[
                "release",
                "check",
                "--scope",
                "production",
                "--evidence-manifest",
                manifest_path.to_str().unwrap(),
                "--expected-source-commit",
                commit,
                "--expected-run-id",
                "123",
                "--expected-run-attempt",
                "1",
                "--expected-manifest-digest",
                manifest_digest.as_str(),
                "--output",
                "json",
            ]);

            assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED, "{name}: {stderr}");
            assert!(stdout.is_empty(), "{name}: {stdout}");
            assert!(
                stderr.contains(expected_blocker),
                "{name}: missing {expected_blocker}: {stderr}"
            );
            fs::remove_dir_all(evidence_root).unwrap();
        }
    }

    #[test]
    /// 验证把已绑定的 invalid envelope 改写成合规字段仍会撞上 envelope digest。
    fn production_release_check_rejects_invalid_to_valid_envelope_rewrite() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let (evidence_root, manifest_path, _, benchmark_envelope_path, _) =
            release_complete_production_manifest_fixture("release-envelope-rewrite");
        let mut envelope = eva_release::EvidenceEnvelope::parse_manifest(
            &fs::read_to_string(&benchmark_envelope_path).unwrap(),
        )
        .unwrap();
        envelope.kind = eva_release::EvidenceKind::Fixture;
        envelope.timestamp = 1;
        envelope.executor = "local:release-benchmark".to_owned();
        fs::write(&benchmark_envelope_path, envelope.to_manifest()).unwrap();
        let trusted_manifest_digest =
            bind_release_manifest_envelopes(&evidence_root, &manifest_path);

        envelope.kind = eva_release::EvidenceKind::Measurement;
        envelope.timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .saturating_sub(1_000);
        envelope.executor = "github-actions:release-benchmark/123/1/benchmark".to_owned();
        fs::write(&benchmark_envelope_path, envelope.to_manifest()).unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            commit,
            "--expected-run-id",
            "123",
            "--expected-run-attempt",
            "1",
            "--expected-manifest-digest",
            trusted_manifest_digest.as_str(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("production_evidence_envelope_digest_mismatch"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 entry envelope digest 的缺失、非法格式和内容不匹配均稳定失败。
    fn production_release_check_rejects_invalid_entry_envelope_digest() {
        enum Mutation {
            Missing,
            Invalid,
            Mismatch,
        }
        let cases = [
            (
                "missing",
                Mutation::Missing,
                "production_evidence_envelope_digest_missing",
            ),
            (
                "invalid",
                Mutation::Invalid,
                "production_evidence_envelope_digest_invalid",
            ),
            (
                "mismatch",
                Mutation::Mismatch,
                "production_evidence_envelope_digest_mismatch",
            ),
        ];
        let commit = "0123456789abcdef0123456789abcdef01234567";

        for (name, mutation, expected_blocker) in cases {
            let (evidence_root, manifest_path, _, _, _) =
                release_complete_production_manifest_fixture(&format!(
                    "release-envelope-digest-{name}"
                ));
            let mut manifest = eva_release::ReleaseEvidenceManifest::parse_manifest(
                &fs::read_to_string(&manifest_path).unwrap(),
            )
            .unwrap();
            let entry = manifest
                .entries
                .iter_mut()
                .find(|entry| entry.evidence_type == eva_release::ReleaseEvidenceType::Benchmark)
                .unwrap();
            match mutation {
                Mutation::Missing => entry.envelope_digest = None,
                Mutation::Invalid => entry.envelope_digest = Some("sha256:bad".to_owned()),
                Mutation::Mismatch => {
                    entry.envelope_digest = Some(
                        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                            .to_owned(),
                    );
                }
            }
            let trusted_manifest_digest = manifest.canonical_digest();
            fs::write(&manifest_path, manifest.to_manifest()).unwrap();

            let (exit_code, stdout, stderr) = run_cli(&[
                "release",
                "check",
                "--scope",
                "production",
                "--evidence-manifest",
                manifest_path.to_str().unwrap(),
                "--expected-source-commit",
                commit,
                "--expected-run-id",
                "123",
                "--expected-run-attempt",
                "1",
                "--expected-manifest-digest",
                trusted_manifest_digest.as_str(),
                "--output",
                "json",
            ]);

            assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED, "{name}: {stderr}");
            assert!(stdout.is_empty(), "{name}: {stdout}");
            assert!(
                stderr.contains(expected_blocker),
                "{name}: missing {expected_blocker}: {stderr}"
            );
            fs::remove_dir_all(evidence_root).unwrap();
        }
    }

    #[test]
    /// 验证外部可信提交错误时使用稳定 provenance blocker 拒绝 manifest。
    fn production_release_check_rejects_wrong_expected_commit() {
        let (evidence_root, manifest_path, _, _) = release_benchmark_manifest_fixture(
            "release-manifest-wrong-expected-commit",
            eva_release::ReleaseEvidenceScope::Production,
        );
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            "abcdef0123456789abcdef0123456789abcdef01",
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("evidence_source_commit_mismatch"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 canonical typed evidence 被改写后 envelope 摘要不再通过。
    fn release_check_rejects_tampered_manifest_evidence_subject() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let (evidence_root, manifest_path, _, _, manifest_digest) =
            release_complete_production_manifest_fixture("release-manifest-tampered-benchmark");
        let evidence_path = evidence_root.join("release-benchmark.evidence");
        let tampered = fs::read_to_string(&evidence_path).unwrap().replace(
            "measurement.0.observed_ms=120",
            "measurement.0.observed_ms=121",
        );
        fs::write(&evidence_path, tampered).unwrap();
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            commit,
            "--expected-run-id",
            "123",
            "--expected-run-attempt",
            "1",
            "--expected-manifest-digest",
            manifest_digest.as_str(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED);
        assert!(stdout.is_empty());
        assert!(stderr.contains("evidence_subject_digest_mismatch"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 artifact manifest 校验真实包字节，篡改后不会只凭 evidence 文本通过。
    fn release_check_verifies_real_artifact_subject_bytes() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let (evidence_root, manifest_path, subject_path, _, manifest_digest) =
            release_complete_production_manifest_fixture("release-manifest-real-artifact");
        let args = [
            "release",
            "check",
            "--scope",
            "production",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--expected-source-commit",
            commit,
            "--expected-run-id",
            "123",
            "--expected-run-attempt",
            "1",
            "--expected-manifest-digest",
            manifest_digest.as_str(),
            "--output",
            "json",
        ];
        let (exit_code, stdout, stderr) = run_cli(&args);
        assert_eq!(exit_code, EXIT_PRODUCTION_BLOCKED, "{stderr}");
        assert!(stdout.contains("\"id\":\"REL-ARTIFACT-PROVENANCE-001\""));
        assert!(stdout.contains(
            "\"domain\":\"release_artifact_provenance\",\"evidence_kind\":\"measurement\""
        ));
        assert!(stdout.contains("evidence_kind_not_measured:REL-MCP-COMPAT-001:fixture"));
        assert!(!stdout.contains("REL-PRODUCTION-EVIDENCE-POLICY-001"));

        fs::write(&subject_path, b"tampered release artifact bytes").unwrap();
        let (tampered_exit, tampered_stdout, tampered_stderr) = run_cli(&args);
        assert_eq!(tampered_exit, EXIT_PRODUCTION_BLOCKED);
        assert!(tampered_stdout.is_empty());
        assert!(tampered_stderr.contains("evidence_subject_digest_mismatch"));
        assert!(tampered_stderr.contains("evidence_subject_size_mismatch"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证统一 manifest 与旧 evidence 参数不存在隐式优先级。
    fn release_check_rejects_manifest_and_legacy_evidence_mix() {
        let (evidence_root, manifest_path, evidence_path, _) = release_benchmark_manifest_fixture(
            "release-manifest-legacy-mix",
            eva_release::ReleaseEvidenceScope::Alpha,
        );
        let (exit_code, stdout, stderr) = run_cli(&[
            "release",
            "check",
            "--evidence-manifest",
            manifest_path.to_str().unwrap(),
            "--benchmark-evidence",
            evidence_path.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_CONFIG);
        assert!(stdout.is_empty());
        assert!(stderr.contains("cannot be combined with legacy evidence options"));
        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证有效签名产物证据使对应发布 gate 通过。
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
        assert!(stdout.contains(
            "\"domain\":\"release_artifact_provenance\",\"evidence_kind\":\"declaration\""
        ));
        assert!(stdout.contains("\"status\":\"pass\""));
        assert!(stdout.contains("signed_artifact_provenance_verified"));
        assert!(stdout.contains("\"evidence_scope\":\"alpha\""));
        assert!(stdout.contains("\"source\":\"legacy_alpha\""));
        assert!(stdout.contains("\"normalized_envelope_count\":1"));
        assert!(stdout.contains(
            "\"provenance\":{\"evidence_type\":\"artifact\",\"source\":\"legacy:artifact-evidence-manifest\""
        ));
        assert!(stdout.contains("\"environment\":\"legacy-unbound\""));
        assert!(stdout.contains("\"executor\":\"legacy-cli-input\",\"timestamp_ms\":1"));
        assert!(!stdout.contains(evidence_path.to_str().unwrap()));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证未签名产物证据形成 blocker 并返回配置类退出码。
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
    /// 验证成功分发/安装烟测证据使跨平台分发 gate 通过。
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
    /// 验证失败分发证据阻断发布而不被内置基线掩盖。
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
    /// 验证无阻塞发现的安全扫描证据使扫描 gate 通过。
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
    /// 验证高严重度安全发现形成发布 blocker。
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
    /// 验证预算内 benchmark 实测证据使性能 gate 通过。
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
    /// 验证 benchmark 回归在综合 readiness 中形成 blocker。
    fn release_check_with_benchmark_regression_blocks_gate() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-benchmark-regression", "passed", 6_000);
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
        assert!(stdout.contains("benchmark release.check observed 6000ms over 5000ms budget"));
        assert!(stdout.contains("production_benchmark_blocked"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 release perf 优先使用外部观测值而非内置基线。
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
        assert!(stdout.contains("\"exit_code\":0"));
        assert!(stdout.contains("\"status\":\"within_budget\""));
        assert!(stdout.contains("\"measured\":1"));
        assert!(stdout.contains("\"unmeasured\":0"));
        assert!(stdout.contains("\"component\":\"release.check\""));
        assert!(stdout.contains("\"observed_ms\":120"));
        assert!(stdout.contains("\"observation_kind\":\"measurement\""));
        assert!(stdout.contains("performance:benchmark_evidence:v1.11.3"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证超预算 benchmark 返回 runtime-unavailable 退出码。
    fn release_perf_with_benchmark_regression_returns_runtime_exit() {
        let root = workspace_root();
        let (evidence_root, evidence_path) = release_benchmark_evidence_fixture(
            "release-perf-benchmark-regression",
            "passed",
            6_000,
        );
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
        assert!(stdout.contains("\"exit_code\":4"));
        assert!(stdout.contains("\"status\":\"over_budget\""));
        assert!(stdout.contains("\"over_budget\":1"));
        assert!(stdout.contains("\"observed_ms\":6000"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 benchmark producer 不能通过抬高 claimed budget 绕过 consumer policy。
    fn release_perf_rejects_claimed_budget_override() {
        let root = workspace_root();
        let (evidence_root, evidence_path) =
            release_benchmark_evidence_fixture("release-perf-budget-override", "passed", 6_000);
        let forged = fs::read_to_string(&evidence_path).unwrap().replace(
            "measurement.0.budget_ms=5000",
            "measurement.0.budget_ms=7000",
        );
        fs::write(&evidence_path, forged).unwrap();
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
        assert!(stdout.contains("\"exit_code\":4"));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("\"budget_ms\":5000"));
        assert!(stdout.contains("claimed_budget_ms=7000"));
        assert!(stdout.contains("benchmark_budget_policy_matches:false"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 benchmark 自身失败状态即使数值存在也阻断性能发布。
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
        assert!(stdout.contains("\"exit_code\":4"));
        assert!(stdout.contains("\"status\":\"blocked\""));
        assert!(stdout.contains("benchmark_status:failed"));

        fs::remove_dir_all(evidence_root).unwrap();
    }

    #[test]
    /// 验证 Adapter、MCP、Skill 和 Discovery 外部能力命令均提供稳定 JSON。
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
    /// 验证 MCP allowlist 外工具在探测阶段被策略边界拒绝。
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
    /// 验证 discovery JSON 区分来源状态、候选和拒绝原因。
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
    /// 验证 Skill 调用输出将 Adapter 审计与 Invoke trace 连续关联。
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
    /// 验证 memory context 同时返回私有、全局和知识三类受限内容。
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
    /// 验证 durable memory 后端执行脱敏和过期过滤后再构建上下文。
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
    /// 验证 memory context 写入包含请求与 Agent 关联的可观测性记录。
    fn memory_context_writes_request_agent_observability() {
        let root = workspace_root();
        let request_id = format!(
            "req-memory-audit-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let (exit_code, stdout, stderr) = run_cli(&[
            "memory",
            "context",
            "--project",
            root.to_str().unwrap(),
            "--agent",
            "root-agent",
            "--request-id",
            &request_id,
            "--query",
            "memory",
            "--private-limit",
            "8",
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"memory.context\""));
        assert!(stdout.contains("\"redactions\":"), "{stdout}");
        let observability_root = root.join(".eva/data/observability");
        let audit = fs::read_to_string(observability_root.join("audit.jsonl")).unwrap();
        let metrics = fs::read_to_string(observability_root.join("metrics.jsonl")).unwrap();

        assert!(audit.contains("\"action\":\"memory.read\""), "{audit}");
        assert!(audit.contains("\"action\":\"memory.search\""), "{audit}");
        assert!(audit.contains("\"action\":\"memory.context\""), "{audit}");
        assert!(audit.contains(&format!("\"request_id\":\"{request_id}\"")));
        assert!(audit.contains("\"agent_id\":\"root-agent\""));
        assert!(metrics.contains("\"name\":\"memory.operation.count\""));
        assert!(metrics.contains("\"name\":\"memory.redaction.count\""));
    }

    #[test]
    /// 验证 observability smoke 持久化信号并显式报告 best-effort 降级状态。
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
        assert!(stdout.contains("\"audit_events\":2"), "{stdout}");
        assert!(stdout.contains("\"metric_points\":3"), "{stdout}");
        assert!(stdout.contains("\"otel_spans\":3"), "{stdout}");
        assert!(stdout.contains("\"tracing_bridge\":{"), "{stdout}");
        assert!(stdout.contains("\"otel_exporter\":null"), "{stdout}");
        assert!(stdout.contains("\"spans\":1"), "{stdout}");
        assert!(stdout.contains("\"events\":1"), "{stdout}");
        assert!(stdout.contains("\"duplicate_span_ids\":0"), "{stdout}");
        assert!(backend.join("audit.jsonl").is_file());
        assert!(backend.join("metrics.jsonl").is_file());
        assert!(backend.join("otel-spans.jsonl").is_file());
        fs::remove_dir_all(&backend).ok();

        let otel_option_only_backend = test_temp_dir("observability-otel-option-only");
        let (option_only_exit, option_only_stdout, option_only_stderr) = run_cli(&[
            "observability",
            "smoke",
            "--backend",
            otel_option_only_backend.to_str().unwrap(),
            "--otel-timeout-ms",
            "100",
            "--output",
            "json",
        ]);

        assert_eq!(option_only_exit, EXIT_OK, "{option_only_stderr}");
        assert!(
            option_only_stdout.contains("\"otel_exporter\":null"),
            "{option_only_stdout}"
        );
        fs::remove_dir_all(otel_option_only_backend).ok();

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

        let dev_console_backend = test_temp_dir("observability-dev-console");
        let (dev_exit, dev_stdout, dev_stderr) = run_cli(&[
            "observability",
            "smoke",
            "--backend",
            dev_console_backend.to_str().unwrap(),
            "--tracing-sink",
            "dev-console",
            "--output",
            "json",
        ]);

        assert_eq!(dev_exit, EXIT_OK, "{dev_stderr}");
        assert!(
            dev_stdout.contains("\"sink\":\"dev-console\""),
            "{dev_stdout}"
        );
        assert!(
            dev_stdout.contains("\"dev_console_lines\":2"),
            "{dev_stdout}"
        );
        assert!(!dev_stdout.contains("sk-"), "{dev_stdout}");
        fs::remove_dir_all(dev_console_backend).ok();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let unavailable_endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let otel_backend = test_temp_dir("observability-otel-unavailable");
        let (otel_exit, otel_stdout, otel_stderr) = run_cli(&[
            "observability",
            "smoke",
            "--backend",
            otel_backend.to_str().unwrap(),
            "--otel-endpoint",
            &unavailable_endpoint,
            "--otel-timeout-ms",
            "100",
            "--output",
            "json",
        ]);

        assert_eq!(otel_exit, EXIT_OK, "{otel_stderr}");
        assert!(otel_stdout.contains("\"otel_exporter\":{"), "{otel_stdout}");
        assert!(otel_stdout.contains("\"degraded\":true"), "{otel_stdout}");
        assert!(
            otel_stdout.contains("\"metric_points_attempted\":3"),
            "{otel_stdout}"
        );
        fs::remove_dir_all(otel_backend).ok();
    }

    #[test]
    /// 验证硬件 list/probe/bind 输出候选与 plan-first、无物理 mutation 事实。
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
        assert!(bind_stdout.contains("\"permission\":"));
        assert!(bind_stdout.contains("\"mutation_executed\":false"));
        assert!(bind_stdout.contains("\"raw_device_path_exposed\":false"));
        assert!(bind_stdout.contains("raw I/O"));

        let (bind_text_exit, bind_text_stdout, bind_text_stderr) = run_cli(&[
            "hardware",
            "bind",
            "--adapter",
            "scale-main",
            "--project",
            root,
            "--output",
            "text",
        ]);
        assert_eq!(bind_text_exit, EXIT_OK, "{bind_text_stderr}");
        assert!(bind_text_stdout.contains("operator_summary: hardware.bind"));
        assert!(bind_text_stdout.contains("plan_id: req-hardware-1"));
        assert!(bind_text_stdout.contains("confirm_token: not_required_plan_only"));
        assert!(bind_text_stdout.contains("final_state: blocked"));
        assert!(bind_text_stdout.contains("rollback_path: none; no raw I/O handle granted"));
        assert!(bind_text_stdout.contains("risk_count:"));
    }

    #[test]
    /// 验证 V1.4 backup/snapshot/restore 计划命令输出完整 JSON 证据链。
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
        assert!(restore_stdout.contains("\"mutation_executed\":false"));

        let (_exit_code, upgrade_stdout, _stderr) =
            run_cli(&["upgrade", "check", "--project", root, "--output", "json"]);
        assert!(upgrade_stdout.contains("\"status\":\"ready\""));
        assert!(upgrade_stdout.contains("\"mutation_executed\":false"));
        assert!(upgrade_stdout.contains("rollback"));
    }

    #[test]
    /// 验证 snapshot promote 只生成 release pointer 计划，不执行 pointer mutation。
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
            "1.11.5-alpha",
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
    /// 验证 snapshot 确认值不匹配时在晋升计划前返回冲突。
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
    /// 验证 snapshot promote 缺少显式确认时拒绝执行。
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
    /// 验证非 dry-run restore apply 在策略检查前先要求 lock store。
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
    /// 验证默认策略拒绝 restore apply 且不会创建锁或 mutation 状态。
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
    /// 验证策略、锁和健康门禁全部通过时 restore apply 被允许。
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
        let observability_root = project.join(".eva/data/observability");
        let audit = fs::read_to_string(observability_root.join("audit.jsonl")).unwrap();
        let metrics = fs::read_to_string(observability_root.join("metrics.jsonl")).unwrap();
        let spans = fs::read_to_string(observability_root.join("otel-spans.jsonl")).unwrap();
        assert!(audit.contains("\"action\":\"restore.apply\""));
        assert!(audit.contains("\"command\":\"restore.apply\""));
        assert!(audit.contains("\"plan_id\":\"plan-allowed\""));
        assert!(metrics.contains("\"name\":\"runtime.restore.apply\""));
        assert!(spans.contains("\"name\":\"runtime.restore.apply\""));

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    /// 验证声明 staged steps 的 restore plan 真实执行文件事务并记录日志。
    fn restore_apply_executes_staged_mutation_when_plan_declares_steps() {
        let project = project_with_restore_apply_allowed("restore-apply-mutation-project");
        let artifact_root = test_temp_dir("restore-apply-mutation-artifacts");
        let target_root = test_temp_dir("restore-apply-mutation-target");
        let lock_root = test_temp_dir("restore-apply-mutation-lock");
        let plan_path = artifact_root.join("restore.plan");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        fs::create_dir_all(target_root.join("logs")).unwrap();
        fs::write(target_root.join("bin/eva"), b"old-binary").unwrap();
        fs::write(target_root.join("logs/old.log"), b"old-log").unwrap();
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/apply-mutation", b"ok".as_slice())
            .unwrap();
        let pre_restore = store
            .put_bytes("backup/pre-apply-mutation", b"before".as_slice())
            .unwrap();
        let config = store
            .put_bytes("backup/mutation-config", b"config".as_slice())
            .unwrap();
        let binary = store
            .put_bytes("backup/mutation-binary", b"binary".as_slice())
            .unwrap();
        let old_binary_digest = eva_backup::archive::digest_bytes(b"old-binary");
        let old_log_digest = eva_backup::archive::digest_bytes(b"old-log");
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-mutation\nbackup_artifact_id=apply-mutation\nbackup_digest={}\npre_restore_backup_artifact_id=pre-apply-mutation\npre_restore_backup_digest={}\nrestore_target_root={}\nmutation_step=copy|config/eva.yaml|backup/mutation-config|{}|none|file\nmutation_step=replace|bin/eva|backup/mutation-binary|{}|{}|file\nmutation_step=delete|logs/old.log|none|none|{}|file\n",
                artifact.digest,
                pre_restore.digest,
                target_root.display(),
                config.digest,
                binary.digest,
                old_binary_digest,
                old_log_digest
            ),
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-mutation",
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
        assert!(stdout.contains("\"status\":\"applied\""));
        assert!(stdout.contains("\"apply_allowed\":true"));
        assert!(stdout.contains("\"mutation_executed\":true"));
        assert!(stdout.contains("\"mutation_apply\":{"));
        assert!(stdout.contains("\"rollback_required\":false"));
        assert!(stdout.contains("\"completed_steps\":3"));
        assert!(stdout.contains("\"operator_confirmation\":{"));
        assert!(stdout.contains("\"confirm_token\":\"plan-mutation\""));
        assert!(stdout.contains("\"affected_count\":3"));
        assert!(stdout.contains("\"mutation_planned\":true"));
        assert!(stdout.contains("\"irreversible_warning\":"));
        assert_eq!(
            fs::read(target_root.join("config/eva.yaml")).unwrap(),
            b"config"
        );
        assert_eq!(fs::read(target_root.join("bin/eva")).unwrap(), b"binary");
        assert!(!target_root.join("logs/old.log").exists());
        assert!(lock_root.join("plan-mutation.restore.lock").exists());
        let transaction_log = fs::read_to_string(lock_root.join("plan-mutation.restore.txn"))
            .expect("transaction log should be written");
        assert!(transaction_log.contains("status=applied"));

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(target_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    /// 验证失败 staged mutation 可从 transaction log 和前置备份恢复原文件。
    fn restore_rollback_restores_failed_staged_mutation_from_transaction_log() {
        let project = project_with_restore_apply_allowed("restore-rollback-project");
        let artifact_root = test_temp_dir("restore-rollback-artifacts");
        let target_root = test_temp_dir("restore-rollback-target");
        let lock_root = test_temp_dir("restore-rollback-lock");
        let plan_path = artifact_root.join("restore.plan");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/rollback-apply", b"ok".as_slice())
            .unwrap();
        let pre_restore = store
            .put_bytes(
                "backup/pre-rollback-apply",
                b"eva-backup-archive:v1\nentry.path=bin/eva\nentry.size=10\nentry.redacted=false\nentry.bytes.hex=6f6c642d62696e617279\n"
                    .as_slice(),
            )
            .unwrap();
        let binary = store
            .put_bytes("backup/rollback-binary", b"binary".as_slice())
            .unwrap();
        let old_binary_digest = eva_backup::archive::digest_bytes(b"old-binary");
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-rollback\nbackup_artifact_id=rollback-apply\nbackup_digest={}\npre_restore_backup_artifact_id=pre-rollback-apply\npre_restore_backup_digest={}\nrestore_target_root={}\nmutation_step=replace|bin/eva|backup/rollback-binary|{}|{}|file\n",
                artifact.digest,
                pre_restore.digest,
                target_root.display(),
                binary.digest,
                old_binary_digest
            ),
        )
        .unwrap();
        fs::write(target_root.join("bin/eva"), b"binary").unwrap();
        let plan = restore_cmd::parse_restore_apply_plan(&fs::read_to_string(&plan_path).unwrap())
            .unwrap();
        let staged = eva_backup::RestoreStagedMutationPlanner
            .plan(&plan)
            .unwrap();
        fs::create_dir_all(&lock_root).unwrap();
        fs::write(
            lock_root.join("plan-rollback.restore.txn"),
            format!(
                "restore-mutation-transaction:v1\nplan_id=plan-rollback\ntarget_root={}\npreflight_hash={}\nstep=0|replace|bin/eva|committed|{}|none\nstep=0|replace|bin/eva|failed|none|post-commit health failed\nstatus=rollback_required\nmutation_executed=true\n",
                target_root.display(),
                staged.preflight_hash,
                binary.digest
            ),
        )
        .unwrap();

        let (exit_code, stdout, stderr) = run_cli(&[
            "restore",
            "rollback",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-rollback",
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
        assert!(stdout.contains("\"command\":\"restore.rollback\""));
        assert!(stdout.contains("\"status\":\"rolled_back\""));
        assert!(stdout.contains("\"mutation_executed\":true"));
        assert!(stdout.contains("\"rollback_executed\":true"));
        assert!(stdout.contains("\"transaction_status\":\"rollback_required\""));
        assert!(stdout.contains("\"operator_confirmation\":{"));
        assert!(stdout.contains("\"confirm_token\":\"plan-rollback\""));
        assert!(stdout.contains("\"target_root\":"));
        assert!(stdout.contains("\"affected_count\":1"));
        assert!(stdout.contains("\"irreversible_warning\":"));
        assert!(lock_root
            .join("plan-rollback.restore.rollback.lock")
            .exists());
        assert!(lock_root
            .join("plan-rollback.restore.rollback.txn")
            .exists());
        assert_eq!(
            fs::read(target_root.join("bin/eva")).unwrap(),
            b"old-binary"
        );
        let observability_root = project.join(".eva/data/observability");
        let audit = fs::read_to_string(observability_root.join("audit.jsonl")).unwrap();
        let metrics = fs::read_to_string(observability_root.join("metrics.jsonl")).unwrap();
        let spans = fs::read_to_string(observability_root.join("otel-spans.jsonl")).unwrap();
        assert!(audit.contains("\"action\":\"restore.rollback\""));
        assert!(audit.contains("\"command\":\"restore.rollback\""));
        assert!(audit.contains("\"plan_id\":\"plan-rollback\""));
        assert!(metrics.contains("\"name\":\"runtime.restore.rollback\""));
        assert!(spans.contains("\"name\":\"runtime.restore.rollback\""));

        fs::remove_dir_all(project).unwrap();
        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(target_root).unwrap();
        fs::remove_dir_all(lock_root).unwrap();
    }

    #[test]
    /// 验证 restore apply 健康失败产生代际 rollback plan 而非报告成功。
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
    /// 验证 restore apply 独占锁冲突被报告且第二次操作不执行 mutation。
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
    /// 验证 upgrade apply 获取并报告文件系统独占锁。
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
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stdout.contains("\"lock_id\":\"upgrade-apply-plan-upgrade\""));
        assert!(lock_root.join("plan-upgrade.lock").exists());

        let text_lock_root = test_temp_dir("upgrade-apply-lock-text");
        let (text_exit, text_stdout, text_stderr) = run_cli(&[
            "upgrade",
            "apply",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-upgrade",
            "--lock-store",
            text_lock_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "text",
        ]);

        assert_eq!(text_exit, EXIT_OK, "{text_stderr}");
        assert!(text_stdout.contains("operator_summary: upgrade.apply"));
        assert!(text_stdout.contains("plan_id: plan-upgrade"));
        assert!(text_stdout.contains("confirm_token: plan-upgrade"));
        assert!(text_stdout.contains("target: gen-v14 -> gen-v15"));
        assert!(text_stdout.contains("final_state: locked"));
        assert!(
            text_stdout.contains("rollback_path: none; no supervisor handoff mutation executed")
        );
        assert!(text_stdout.contains("risk_count:"));

        fs::remove_dir_all(lock_root).unwrap();
        fs::remove_dir_all(text_lock_root).unwrap();
    }

    #[test]
    /// 验证策略、健康和 state store 通过时 handoff 提交 release pointer。
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
    /// 验证 upgrade handoff 在提交 pointer 前运行目标 runtime `--version` 烟测。
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
    /// 验证目标 runtime 二进制缺失时在 release pointer mutation 前阻断 handoff。
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
    /// 验证 handoff 健康失败生成回滚证据且不提交 release pointer。
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
    /// 验证默认策略在 state store 和 pointer 变更前拒绝 upgrade apply。
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
    /// 验证 upgrade 独占锁冲突阻止第二个 apply 协调器进入 handoff。
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
    /// 验证 upgrade 确认值必须与 plan ID 精确匹配。
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
    /// 验证 upgrade 计划解析器兼容带 UTF-8 BOM 的文件。
    fn upgrade_apply_plan_allows_utf8_bom() {
        let plan = upgrade_cmd::parse_upgrade_apply_plan(
            "\u{feff}plan_id=plan-bom\nfrom_generation=gen-v14\nto_generation=gen-v15\nfrom_release=1.4.0\nto_release=1.5.1\n",
        )
        .unwrap();

        assert_eq!(plan.plan_id, "plan-bom");
        assert_eq!(plan.lock_id(), "upgrade-apply-plan-bom");
    }

    #[test]
    /// 验证 restore dry-run 校验 durable 备份 digest 和 manifest 证据。
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
        assert!(stdout.contains("\"mutation_executed\":false"));
        assert!(stdout.contains("\"backup_artifact_key\":\"backup/apply-ok\""));
        assert!(stdout.contains("\"pre_restore_backup_artifact_key\":\"backup/pre-apply-ok\""));

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    /// 验证 restore dry-run 输出 staged mutation 目标、步骤和回滚计划。
    fn restore_apply_dry_run_reports_staged_mutation_plan() {
        let root = workspace_root();
        let artifact_root = test_temp_dir("restore-apply-staged");
        let plan_path = artifact_root.join("restore.plan");
        let mut store = FileSystemArtifactStore::new(&artifact_root);
        let artifact = store
            .put_bytes("backup/apply-staged", b"ok".as_slice())
            .unwrap();
        let pre_restore = store
            .put_bytes("backup/pre-apply-staged", b"before".as_slice())
            .unwrap();
        let copy = store
            .put_bytes("backup/staged-config", b"config".as_slice())
            .unwrap();
        let replace = store
            .put_bytes("backup/staged-binary", b"binary".as_slice())
            .unwrap();
        let old_binary = store
            .put_bytes("backup/pre-binary", b"old-binary".as_slice())
            .unwrap();
        let old_log = store
            .put_bytes("backup/pre-log", b"old-log".as_slice())
            .unwrap();
        fs::write(
            &plan_path,
            format!(
                "plan_id=plan-staged\nbackup_artifact_id=apply-staged\nbackup_digest={}\npre_restore_backup_artifact_id=pre-apply-staged\npre_restore_backup_digest={}\nrestore_target_root=workspace\nmutation_step=copy|config/eva.yaml|backup/staged-config|{}|none|file\nmutation_step=replace|bin/eva|backup/staged-binary|{}|{}|file\nmutation_step=delete|logs/old.log|none|none|{}|file\n",
                artifact.digest,
                pre_restore.digest,
                copy.digest,
                replace.digest,
                old_binary.digest,
                old_log.digest
            ),
        )
        .unwrap();

        let (first_exit, first_stdout, first_stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-staged",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        let (second_exit, second_stdout, second_stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-staged",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);
        let (text_exit, text_stdout, text_stderr) = run_cli(&[
            "restore",
            "apply",
            "--dry-run",
            "--plan",
            plan_path.to_str().unwrap(),
            "--confirm",
            "plan-staged",
            "--artifact-store",
            artifact_root.to_str().unwrap(),
            "--project",
            root.to_str().unwrap(),
            "--output",
            "text",
        ]);

        assert_eq!(first_exit, EXIT_OK, "{first_stderr}");
        assert_eq!(second_exit, EXIT_OK, "{second_stderr}");
        assert_eq!(text_exit, EXIT_OK, "{text_stderr}");
        assert!(
            first_stdout.contains("\"mutation_plan\":{"),
            "{first_stdout}"
        );
        assert!(
            first_stdout.contains("\"mutation_planned\":true"),
            "{first_stdout}"
        );
        assert!(
            first_stdout.contains("\"mutation_executed\":false"),
            "{first_stdout}"
        );
        assert!(first_stdout.contains("\"target_root\":\"workspace\""));
        assert!(first_stdout.contains("\"operation\":\"copy\""));
        assert!(first_stdout.contains("\"operation\":\"replace\""));
        assert!(first_stdout.contains("\"operation\":\"delete\""));
        assert!(first_stdout
            .contains("\"affected_paths\":[\"bin/eva\",\"config/eva.yaml\",\"logs/old.log\"]"));
        assert!(first_stdout.contains("\"rollback_manifest\":["));
        assert!(first_stdout.contains("\"action\":\"restore_pre_restore_digest\""));
        assert!(first_stdout.contains("\"preflight_hash\":\"sha256:"));
        assert!(first_stdout.contains("\"operator_confirmation\":{"));
        assert!(first_stdout.contains("\"confirm_token\":\"plan-staged\""));
        assert!(first_stdout.contains("\"affected_count\":3"));
        assert!(first_stdout.contains("\"irreversible_warning\":"));
        assert!(text_stdout.contains("operator_confirmation: restore.apply.dry_run"));
        assert!(text_stdout.contains("plan_id: plan-staged"));
        assert!(text_stdout.contains("confirm_token: plan-staged"));
        assert!(text_stdout.contains("target_root: workspace"));
        assert!(text_stdout.contains("affected_count: 3"));
        assert!(text_stdout.contains("apply_allowed: false"));
        assert!(text_stdout.contains("mutation_planned: true"));
        assert!(text_stdout.contains("mutation_executed: false"));
        assert!(text_stdout.contains("rollback_required: false"));
        assert!(text_stdout.contains("rollback_executed: false"));
        assert!(text_stdout.contains("final_state: dry_run_validated"));
        assert!(text_stdout.contains("rollback_path: pre_restore_backup=backup/pre-apply-staged"));
        assert!(text_stdout.contains("rollback_manifest_entries=3"));
        assert!(text_stdout.contains("risk_count: 2"));
        assert!(
            text_stdout.contains("risk[0]: 3 affected paths require review before confirmation")
        );
        assert!(text_stdout.contains("irreversible_warning:"));
        assert!(text_stdout.contains("next_action:"));
        assert_eq!(
            extract_json_value(&first_stdout, "\"preflight_hash\":\""),
            extract_json_value(&second_stdout, "\"preflight_hash\":\"")
        );

        fs::remove_dir_all(artifact_root).unwrap();
    }

    #[test]
    /// 验证 restore apply 计划解析器兼容 UTF-8 BOM。
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
    /// 验证 restore dry-run 缺少前置备份字段时在校验阶段失败。
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
    /// 验证目标备份产物缺失时 dry-run 返回 NotFound 而不获取锁。
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
    /// 验证前置备份产物缺失时 dry-run 明确报告对应 artifact key。
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
    /// 验证备份 digest 不匹配时 dry-run 拒绝进入 apply 协调器。
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
    /// 验证 V1.4 backup/snapshot/restore 可使用真实文件系统 ArtifactStore。
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
    /// 验证 V1.5 release check/security/perf/migration 均输出稳定 JSON 契约。
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
        assert!(perf_stdout.contains("\"status\":\"unmeasured\""));
        assert!(perf_stdout.contains("\"measured\":0"));
        assert!(perf_stdout.contains("\"unmeasured\":6"));
        assert!(perf_stdout.contains("\"observed_ms\":null"));
        assert!(perf_stdout.contains("\"observation_kind\":\"unmeasured\""));
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
        assert!(migration_stdout.contains("\"to_version\":\"1.11.5-alpha\""));
        assert!(migration_stdout.contains("\"breaking_changes\":[]"));
    }
}
