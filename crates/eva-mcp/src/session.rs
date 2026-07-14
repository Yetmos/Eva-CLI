//! 定义 MCP 会话配置及其进程监督边界。
//!
//! 会话管理器只负责启动 stdio 进程型会话，并验证监督器返回的句柄。关闭时先暂时取走句柄；
//! 若监督器关闭失败则把句柄放回，使调用方仍可重试且不会把仍运行的进程误报为已停止。
//! MCP process/session lifecycle boundary.

use eva_core::{AdapterId, EvaError};
use std::collections::BTreeSet;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP process startup and session shutdown boundary";

/// 定义 `DEFAULT_STARTUP_TIMEOUT_MS` 常量。
const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 10_000;
/// 定义 `DEFAULT_SHUTDOWN_TIMEOUT_MS` 常量。
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;

/// 定义 `McpServerTransport` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerTransport {
    /// 表示 `Stdio` 枚举分支。
    Stdio,
    /// 表示 `Http` 枚举分支。
    Http,
}

/// 表示 `McpProcessSpec` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessSpec {
    /// 记录 `command` 字段对应的值。
    pub command: String,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `allowed_commands` 字段对应的值。
    pub allowed_commands: BTreeSet<String>,
    /// 记录 `startup_timeout_ms` 字段对应的值。
    pub startup_timeout_ms: u64,
    /// 记录 `shutdown_timeout_ms` 字段对应的值。
    pub shutdown_timeout_ms: u64,
}

/// 表示 `McpSessionConfig` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionConfig {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `server_transport` 字段对应的值。
    pub server_transport: McpServerTransport,
    /// 记录 `process` 字段对应的值。
    pub process: McpProcessSpec,
}

/// 表示 `McpProcessStartRequest` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessStartRequest {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `server_transport` 字段对应的值。
    pub server_transport: McpServerTransport,
    /// 记录 `command` 字段对应的值。
    pub command: String,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `startup_timeout_ms` 字段对应的值。
    pub startup_timeout_ms: u64,
}

/// 表示 `McpProcessShutdownRequest` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessShutdownRequest {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `shutdown_timeout_ms` 字段对应的值。
    pub shutdown_timeout_ms: u64,
}

/// 表示 `McpProcessHandle` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessHandle {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `process_id` 字段对应的值。
    pub process_id: Option<u32>,
}

/// 表示 `McpSession` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSession {
    /// 记录 `config` 字段对应的值。
    config: McpSessionConfig,
    /// 记录 `handle` 字段对应的值。
    handle: Option<McpProcessHandle>,
}

/// 表示 `McpSessionStartReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionStartReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpSessionStatus,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `McpSessionShutdownReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionShutdownReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpSessionStatus,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 定义 `McpSessionStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSessionStatus {
    /// 表示 `Started` 枚举分支。
    Started,
    /// 表示 `Stopped` 枚举分支。
    Stopped,
}

/// 约定 `McpSessionSupervisor` 实现需要满足的接口。
pub trait McpSessionSupervisor {
    /// 执行 `start_process` 对应的受控流程。
    fn start_process(
        &mut self,
        request: &McpProcessStartRequest,
    ) -> Result<McpProcessHandle, EvaError>;

    /// 停止或释放 `shutdown_process` 管理的资源。
    fn shutdown_process(&mut self, request: &McpProcessShutdownRequest) -> Result<(), EvaError>;
}

/// 表示 `McpSessionManager` 数据结构。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct McpSessionManager;

impl McpServerTransport {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "stdio" => Ok(Self::Stdio),
            "http" => Ok(Self::Http),
            _ => Err(EvaError::unsupported("unsupported MCP server transport")
                .with_context("server_transport", value)),
        }
    }

    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

impl McpProcessSpec {
    /// 创建并初始化当前类型的实例。
    pub fn new(command: impl Into<String>) -> Self {
        let command = command.into();
        Self {
            allowed_commands: [command.clone()].into_iter().collect(),
            command,
            args: Vec::new(),
            startup_timeout_ms: DEFAULT_STARTUP_TIMEOUT_MS,
            shutdown_timeout_ms: DEFAULT_SHUTDOWN_TIMEOUT_MS,
        }
    }

    /// 设置 `args` 并返回更新后的实例。
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// 设置 `allowed_commands` 并返回更新后的实例。
    pub fn with_allowed_commands(
        mut self,
        allowed_commands: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_commands = allowed_commands.into_iter().map(Into::into).collect();
        self
    }

    /// 设置 `startup_timeout_ms` 并返回更新后的实例。
    pub fn with_startup_timeout_ms(mut self, startup_timeout_ms: u64) -> Self {
        self.startup_timeout_ms = startup_timeout_ms;
        self
    }

    /// 设置 `shutdown_timeout_ms` 并返回更新后的实例。
    pub fn with_shutdown_timeout_ms(mut self, shutdown_timeout_ms: u64) -> Self {
        self.shutdown_timeout_ms = shutdown_timeout_ms;
        self
    }
}

impl McpSessionConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        adapter_id: AdapterId,
        server_transport: McpServerTransport,
        process: McpProcessSpec,
    ) -> Result<Self, EvaError> {
        let config = Self {
            adapter_id,
            server_transport,
            process,
        };
        validate_config(&config)?;
        Ok(config)
    }

    /// 执行 `stdio` 对应的处理逻辑。
    pub fn stdio(adapter_id: AdapterId, command: impl Into<String>) -> Result<Self, EvaError> {
        Self::new(
            adapter_id,
            McpServerTransport::Stdio,
            McpProcessSpec::new(command),
        )
    }
}

impl McpProcessHandle {
    /// 创建并初始化当前类型的实例。
    pub fn new(session_id: impl Into<String>, process_id: Option<u32>) -> Self {
        Self {
            session_id: session_id.into(),
            process_id,
        }
    }
}

impl McpSession {
    /// 返回 `adapter_id` 对应的数据视图。
    pub fn adapter_id(&self) -> &AdapterId {
        &self.config.adapter_id
    }

    /// 执行 `session_id` 对应的处理逻辑。
    pub fn session_id(&self) -> Option<&str> {
        self.handle
            .as_ref()
            .map(|handle| handle.session_id.as_str())
    }

    /// 判断 `is_running` 对应的条件是否成立。
    pub fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    /// 执行 `process_id` 对应的处理逻辑。
    pub fn process_id(&self) -> Option<u32> {
        self.handle.as_ref().and_then(|handle| handle.process_id)
    }

    /// 执行 `server_transport` 对应的处理逻辑。
    pub fn server_transport(&self) -> McpServerTransport {
        self.config.server_transport
    }

    /// 执行 `start_report` 对应的受控流程。
    pub fn start_report(&self) -> Result<McpSessionStartReport, EvaError> {
        let handle = self.handle.as_ref().ok_or_else(|| {
            EvaError::conflict("MCP session is not running")
                .with_context("adapter_id", self.config.adapter_id.as_str())
        })?;
        Ok(McpSessionStartReport {
            adapter_id: self.config.adapter_id.clone(),
            session_id: handle.session_id.clone(),
            status: McpSessionStatus::Started,
            audit: start_audit(&self.config, handle),
        })
    }
}

impl McpSessionStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
        }
    }
}

impl McpSessionManager {
    /// 执行 `start` 对应的受控流程。
    pub fn start(
        &self,
        supervisor: &mut impl McpSessionSupervisor,
        config: McpSessionConfig,
    ) -> Result<McpSession, EvaError> {
        validate_config(&config)?;
        if config.server_transport != McpServerTransport::Stdio {
            return Err(EvaError::unsupported(
                "MCP session manager only starts stdio process sessions",
            )
            .with_context("adapter_id", config.adapter_id.as_str())
            .with_context("server_transport", config.server_transport.as_str()));
        }
        let request = McpProcessStartRequest {
            adapter_id: config.adapter_id.clone(),
            server_transport: config.server_transport,
            command: config.process.command.clone(),
            args: config.process.args.clone(),
            startup_timeout_ms: config.process.startup_timeout_ms,
        };
        let handle = supervisor.start_process(&request)?;
        if handle.session_id.trim().is_empty() {
            return Err(
                EvaError::internal("MCP supervisor returned empty session id")
                    .with_context("adapter_id", config.adapter_id.as_str()),
            );
        }
        Ok(McpSession {
            config,
            handle: Some(handle),
        })
    }

    /// 停止或释放 `shutdown` 管理的资源。
    pub fn shutdown(
        &self,
        supervisor: &mut impl McpSessionSupervisor,
        session: &mut McpSession,
    ) -> Result<McpSessionShutdownReport, EvaError> {
        let handle = session.handle.take().ok_or_else(|| {
            EvaError::conflict("MCP session is already stopped")
                .with_context("adapter_id", session.config.adapter_id.as_str())
        })?;
        let request = McpProcessShutdownRequest {
            adapter_id: session.config.adapter_id.clone(),
            session_id: handle.session_id.clone(),
            shutdown_timeout_ms: session.config.process.shutdown_timeout_ms,
        };

        match supervisor.shutdown_process(&request) {
            Ok(()) => Ok(McpSessionShutdownReport {
                adapter_id: session.config.adapter_id.clone(),
                session_id: handle.session_id,
                status: McpSessionStatus::Stopped,
                audit: vec![
                    "transport:mcp".to_owned(),
                    "mcp.session:shutdown_requested".to_owned(),
                    "mcp.session:stopped".to_owned(),
                ],
            }),
            Err(error) => {
                session.handle = Some(handle);
                Err(error)
            }
        }
    }
}

/// 校验 `validate_config` 对应的约束，不满足时返回明确错误。
fn validate_config(config: &McpSessionConfig) -> Result<(), EvaError> {
    if config.process.command.trim().is_empty() {
        return Err(
            EvaError::invalid_argument("MCP process command cannot be empty")
                .with_context("adapter_id", config.adapter_id.as_str()),
        );
    }
    if config.process.command.trim() != config.process.command {
        return Err(
            EvaError::invalid_argument("MCP process command must be trimmed")
                .with_context("adapter_id", config.adapter_id.as_str())
                .with_context("command", &config.process.command),
        );
    }
    if !config
        .process
        .allowed_commands
        .contains(&config.process.command)
    {
        return Err(
            EvaError::permission_denied("MCP process command is not allowlisted")
                .with_context("adapter_id", config.adapter_id.as_str())
                .with_context("command", &config.process.command),
        );
    }
    if config.process.startup_timeout_ms == 0 {
        return Err(EvaError::invalid_argument(
            "MCP process startup timeout must be greater than zero",
        )
        .with_context("adapter_id", config.adapter_id.as_str()));
    }
    if config.process.shutdown_timeout_ms == 0 {
        return Err(EvaError::invalid_argument(
            "MCP process shutdown timeout must be greater than zero",
        )
        .with_context("adapter_id", config.adapter_id.as_str()));
    }
    for arg in &config.process.args {
        if arg.contains('\0') {
            return Err(
                EvaError::invalid_argument("MCP process argument cannot contain NUL")
                    .with_context("adapter_id", config.adapter_id.as_str()),
            );
        }
    }
    Ok(())
}

/// 执行 `start_audit` 对应的受控流程。
fn start_audit(config: &McpSessionConfig, handle: &McpProcessHandle) -> Vec<String> {
    let mut audit = vec![
        "transport:mcp".to_owned(),
        format!("mcp.server_transport:{}", config.server_transport.as_str()),
        "mcp.session:start_requested".to_owned(),
        "mcp.session:started".to_owned(),
        "shell:false".to_owned(),
        "command_allowlist:passed".to_owned(),
    ];
    if let Some(process_id) = handle.process_id {
        audit.push(format!("process_id:{process_id}"));
    }
    audit
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    /// 表示 `FakeSupervisor` 数据结构。
    #[derive(Debug, Clone)]
    struct FakeSupervisor {
        /// 记录 `start_response` 字段对应的值。
        start_response: Result<McpProcessHandle, EvaError>,
        /// 记录 `shutdown_response` 字段对应的值。
        shutdown_response: Result<(), EvaError>,
        /// 记录 `start_calls` 字段对应的值。
        start_calls: usize,
        /// 记录 `shutdown_calls` 字段对应的值。
        shutdown_calls: usize,
        /// 记录 `last_shutdown_session_id` 字段对应的值。
        last_shutdown_session_id: Option<String>,
    }

    impl FakeSupervisor {
        /// 执行 `started` 对应的受控流程。
        fn started() -> Self {
            Self {
                start_response: Ok(McpProcessHandle::new("session-1", Some(42))),
                shutdown_response: Ok(()),
                start_calls: 0,
                shutdown_calls: 0,
                last_shutdown_session_id: None,
            }
        }

        /// 执行 `startup_failure` 对应的受控流程。
        fn startup_failure() -> Self {
            Self {
                start_response: Err(EvaError::unavailable("MCP process failed to start")
                    .with_context("command", "github-mcp-server")),
                shutdown_response: Ok(()),
                start_calls: 0,
                shutdown_calls: 0,
                last_shutdown_session_id: None,
            }
        }
    }

    impl McpSessionSupervisor for FakeSupervisor {
        /// 执行 `start_process` 对应的受控流程。
        fn start_process(
            &mut self,
            request: &McpProcessStartRequest,
        ) -> Result<McpProcessHandle, EvaError> {
            self.start_calls += 1;
            assert_eq!(request.command, "github-mcp-server");
            self.start_response.clone()
        }

        /// 停止或释放 `shutdown_process` 管理的资源。
        fn shutdown_process(
            &mut self,
            request: &McpProcessShutdownRequest,
        ) -> Result<(), EvaError> {
            self.shutdown_calls += 1;
            self.last_shutdown_session_id = Some(request.session_id.clone());
            self.shutdown_response.clone()
        }
    }

    /// 验证 `session_start_reports_startup_failure` 场景下的预期行为。
    #[test]
    fn session_start_reports_startup_failure() {
        let mut supervisor = FakeSupervisor::startup_failure();
        let manager = McpSessionManager;

        let error = manager.start(&mut supervisor, config()).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(supervisor.start_calls, 1);
        assert_eq!(supervisor.shutdown_calls, 0);
    }

    /// 验证 `session_shutdown_stops_running_session` 场景下的预期行为。
    #[test]
    fn session_shutdown_stops_running_session() {
        let mut supervisor = FakeSupervisor::started();
        let manager = McpSessionManager;
        let mut session = manager.start(&mut supervisor, config()).unwrap();

        let start_report = session.start_report().unwrap();
        assert_eq!(start_report.status, McpSessionStatus::Started);
        assert!(start_report.audit.contains(&"shell:false".to_owned()));
        assert!(session.is_running());

        let shutdown_report = manager.shutdown(&mut supervisor, &mut session).unwrap();

        assert_eq!(shutdown_report.status, McpSessionStatus::Stopped);
        assert_eq!(
            supervisor.last_shutdown_session_id.as_deref(),
            Some("session-1")
        );
        assert!(!session.is_running());
    }

    /// 验证 `session_rejects_non_allowlisted_command` 场景下的预期行为。
    #[test]
    fn session_rejects_non_allowlisted_command() {
        let process = McpProcessSpec::new("github-mcp-server")
            .with_allowed_commands(["different-mcp-server"]);
        let error = McpSessionConfig::new(
            AdapterId::parse("github-mcp").unwrap(),
            McpServerTransport::Stdio,
            process,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    /// 执行 `config` 对应的处理逻辑。
    fn config() -> McpSessionConfig {
        McpSessionConfig::stdio(AdapterId::parse("github-mcp").unwrap(), "github-mcp-server")
            .unwrap()
    }
}
