//! MCP process/session lifecycle boundary.

use eva_core::{AdapterId, EvaError};
use std::collections::BTreeSet;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP process startup and session shutdown boundary";

const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerTransport {
    Stdio,
    Http,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessSpec {
    pub command: String,
    pub args: Vec<String>,
    pub allowed_commands: BTreeSet<String>,
    pub startup_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionConfig {
    pub adapter_id: AdapterId,
    pub server_transport: McpServerTransport,
    pub process: McpProcessSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessStartRequest {
    pub adapter_id: AdapterId,
    pub server_transport: McpServerTransport,
    pub command: String,
    pub args: Vec<String>,
    pub startup_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessShutdownRequest {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub shutdown_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessHandle {
    pub session_id: String,
    pub process_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSession {
    config: McpSessionConfig,
    handle: Option<McpProcessHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionStartReport {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub status: McpSessionStatus,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionShutdownReport {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub status: McpSessionStatus,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSessionStatus {
    Started,
    Stopped,
}

pub trait McpSessionSupervisor {
    fn start_process(
        &mut self,
        request: &McpProcessStartRequest,
    ) -> Result<McpProcessHandle, EvaError>;

    fn shutdown_process(&mut self, request: &McpProcessShutdownRequest) -> Result<(), EvaError>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct McpSessionManager;

impl McpServerTransport {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "stdio" => Ok(Self::Stdio),
            "http" => Ok(Self::Http),
            _ => Err(EvaError::unsupported("unsupported MCP server transport")
                .with_context("server_transport", value)),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

impl McpProcessSpec {
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

    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_allowed_commands(
        mut self,
        allowed_commands: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_commands = allowed_commands.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_startup_timeout_ms(mut self, startup_timeout_ms: u64) -> Self {
        self.startup_timeout_ms = startup_timeout_ms;
        self
    }

    pub fn with_shutdown_timeout_ms(mut self, shutdown_timeout_ms: u64) -> Self {
        self.shutdown_timeout_ms = shutdown_timeout_ms;
        self
    }
}

impl McpSessionConfig {
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

    pub fn stdio(adapter_id: AdapterId, command: impl Into<String>) -> Result<Self, EvaError> {
        Self::new(
            adapter_id,
            McpServerTransport::Stdio,
            McpProcessSpec::new(command),
        )
    }
}

impl McpProcessHandle {
    pub fn new(session_id: impl Into<String>, process_id: Option<u32>) -> Self {
        Self {
            session_id: session_id.into(),
            process_id,
        }
    }
}

impl McpSession {
    pub fn adapter_id(&self) -> &AdapterId {
        &self.config.adapter_id
    }

    pub fn session_id(&self) -> Option<&str> {
        self.handle
            .as_ref()
            .map(|handle| handle.session_id.as_str())
    }

    pub fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    pub fn process_id(&self) -> Option<u32> {
        self.handle.as_ref().and_then(|handle| handle.process_id)
    }

    pub fn server_transport(&self) -> McpServerTransport {
        self.config.server_transport
    }

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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
        }
    }
}

impl McpSessionManager {
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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    #[derive(Debug, Clone)]
    struct FakeSupervisor {
        start_response: Result<McpProcessHandle, EvaError>,
        shutdown_response: Result<(), EvaError>,
        start_calls: usize,
        shutdown_calls: usize,
        last_shutdown_session_id: Option<String>,
    }

    impl FakeSupervisor {
        fn started() -> Self {
            Self {
                start_response: Ok(McpProcessHandle::new("session-1", Some(42))),
                shutdown_response: Ok(()),
                start_calls: 0,
                shutdown_calls: 0,
                last_shutdown_session_id: None,
            }
        }

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
        fn start_process(
            &mut self,
            request: &McpProcessStartRequest,
        ) -> Result<McpProcessHandle, EvaError> {
            self.start_calls += 1;
            assert_eq!(request.command, "github-mcp-server");
            self.start_response.clone()
        }

        fn shutdown_process(
            &mut self,
            request: &McpProcessShutdownRequest,
        ) -> Result<(), EvaError> {
            self.shutdown_calls += 1;
            self.last_shutdown_session_id = Some(request.session_id.clone());
            self.shutdown_response.clone()
        }
    }

    #[test]
    fn session_start_reports_startup_failure() {
        let mut supervisor = FakeSupervisor::startup_failure();
        let manager = McpSessionManager;

        let error = manager.start(&mut supervisor, config()).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(supervisor.start_calls, 1);
        assert_eq!(supervisor.shutdown_calls, 0);
    }

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

    fn config() -> McpSessionConfig {
        McpSessionConfig::stdio(AdapterId::parse("github-mcp").unwrap(), "github-mcp-server")
            .unwrap()
    }
}
