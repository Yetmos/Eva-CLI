//! 实现 MCP 的 JSON-RPC 客户端以及 stdio/HTTP 传输边界。
//!
//! 每次调用先校验工具允许列表，再完成初始化握手并发送工具请求。响应必须命中预期 id、
//! 符合协议结构且不超过大小限制；stdio 的读取线程只传递有界行，超时、EOF、读取错误或
//! 协议错配都会关闭子进程并映射为稳定错误，避免遗留失控会话。
//! MCP JSON-RPC client transport.

use crate::http_transport::{
    map_http_read_error, map_http_write_error, McpHttpConnector, McpTlsMaterial,
};
use crate::policy::McpAllowlist;
use crate::session::{McpEndpoint, McpServerTransport, McpSessionConfig, McpStreamableHttpConfig};
use eva_core::{AdapterId, EvaError, InvokeOutput, RequestId};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP JSON-RPC client transport with stdio process boundaries";

/// 定义 `DEFAULT_PROTOCOL_VERSION` 常量。
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";
/// 定义 `DEFAULT_REQUEST_TIMEOUT_MS` 常量。
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
/// 定义 `DEFAULT_OUTPUT_LIMIT_BYTES` 常量。
const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// A process boundary supplied by an external supervisor.
///
/// The MCP crate deliberately owns only this small capability surface so it
/// does not depend on `eva-adapter`; the adapter can inject its OS-backed
/// handle after durable provider registration has succeeded.
pub trait McpStdioProcess: Debug {
    /// Return the OS PID captured at spawn time.
    fn process_id(&self) -> u32;
    /// Transfer the child's stdin pipe to the JSON-RPC transport.
    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>>;
    /// Transfer the child's stdout pipe to the JSON-RPC reader.
    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>>;
    /// Terminate and reap the complete process boundary.
    fn terminate(&mut self) -> Result<(), EvaError>;

    /// Ask the process boundary to exit cleanly, falling back to the
    /// implementation's forceful termination when graceful shutdown is not
    /// available.  The default keeps third-party process adapters source
    /// compatible while allowing the central provider handle to implement a
    /// real timeout/fallback sequence.
    fn terminate_gracefully(&mut self, _timeout: Duration) -> Result<(), EvaError> {
        self.terminate()
    }
}

impl McpStdioProcess for Child {
    fn process_id(&self) -> u32 {
        self.id()
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        self.stdin.take().map(|pipe| Box::new(pipe) as _)
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stdout.take().map(|pipe| Box::new(pipe) as _)
    }

    fn terminate(&mut self) -> Result<(), EvaError> {
        let _ = self.kill();
        match self.wait() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(
                EvaError::unavailable("failed to terminate MCP stdio process")
                    .with_context("process_id", self.id().to_string())
                    .with_context("io_error", error.to_string()),
            ),
        }
    }
}

/// 表示 `McpJsonRpcClientConfig` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcClientConfig {
    /// 记录 `protocol_version` 字段对应的值。
    pub protocol_version: String,
    /// 记录 `client_name` 字段对应的值。
    pub client_name: String,
    /// 记录 `client_version` 字段对应的值。
    pub client_version: String,
    /// 记录 `request_timeout_ms` 字段对应的值。
    pub request_timeout_ms: u64,
    /// 记录 `output_limit_bytes` 字段对应的值。
    pub output_limit_bytes: usize,
}

/// 表示 `McpJsonRpcClient` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcClient {
    /// 记录 `adapter_id` 字段对应的值。
    adapter_id: AdapterId,
    /// 记录 `allowlist` 字段对应的值。
    allowlist: McpAllowlist,
    /// 记录 `config` 字段对应的值。
    config: McpJsonRpcClientConfig,
}

/// 表示 `McpJsonRpcCallReport` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct McpJsonRpcCallReport {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `output` 字段对应的值。
    pub output: InvokeOutput,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

impl fmt::Debug for McpJsonRpcCallReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpJsonRpcCallReport")
            .field("request_id", &self.request_id)
            .field("adapter_id", &self.adapter_id)
            .field("tool", &self.tool)
            .field("output", &"[REDACTED_OUTPUT]")
            .field("audit_count", &self.audit.len())
            .finish()
    }
}

/// 表示 `McpJsonRpcTool` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcTool {
    /// 记录 `name` 字段对应的值。
    pub name: String,
}

/// 约定 `McpJsonRpcTransport` 实现需要满足的接口。
pub trait McpJsonRpcTransport {
    /// 执行 `exchange` 对应的处理逻辑。
    fn exchange(
        &mut self,
        expected_id: u64,
        request: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<String, EvaError>;

    /// 执行 `notify` 对应的处理逻辑。
    fn notify(
        &mut self,
        notification: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<(), EvaError>;

    /// 执行 `audit` 对应的处理逻辑。
    fn audit(&self) -> Vec<String> {
        Vec::new()
    }
}

/// 表示 `McpStdioJsonRpcTransport` 数据结构。
pub struct McpStdioJsonRpcTransport {
    /// Central-supervisor-owned process boundary.
    process: Option<Box<dyn McpStdioProcess>>,
    /// 记录 `stdin` 字段对应的值。
    stdin: Box<dyn Write + Send>,
    /// 记录 `receiver` 字段对应的值。
    receiver: mpsc::Receiver<ReaderMessage>,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
}

impl Debug for McpStdioJsonRpcTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpStdioJsonRpcTransport")
            .field(
                "process_id",
                &self.process.as_ref().map(|process| process.process_id()),
            )
            .field("audit", &self.audit)
            .finish_non_exhaustive()
    }
}

/// 表示 `McpHttpJsonRpcTransport` 数据结构。
pub struct McpHttpJsonRpcTransport {
    /// 记录 `endpoint` 字段对应的值。
    endpoint: String,
    /// 记录 `headers` 字段对应的值。
    headers: BTreeMap<String, String>,
    /// Validated plaintext or TLS connector.
    connector: McpHttpConnector,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
    /// 记录 `exchange_count` 字段对应的值。
    exchange_count: usize,
    /// 记录 `notification_count` 字段对应的值。
    notification_count: usize,
}

impl fmt::Debug for McpHttpJsonRpcTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpJsonRpcTransport")
            .field("endpoint_present", &!self.endpoint.is_empty())
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("header_count", &self.headers.len())
            .field("audit_count", &self.audit.len())
            .field("exchange_count", &self.exchange_count)
            .field("notification_count", &self.notification_count)
            .finish()
    }
}

/// 表示 `JsonRpcResponse` 数据结构。
#[derive(Clone, PartialEq, Eq)]
struct JsonRpcResponse {
    /// 记录 `result` 字段对应的值。
    result: String,
}

impl fmt::Debug for JsonRpcResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonRpcResponse")
            .field("result_len", &self.result.len())
            .finish()
    }
}

impl Default for McpJsonRpcClientConfig {
    /// 创建采用默认协议版本、超时和响应上限的客户端配置。
    fn default() -> Self {
        Self::new()
    }
}

impl McpJsonRpcClientConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self {
            protocol_version: DEFAULT_PROTOCOL_VERSION.to_owned(),
            client_name: "eva-mcp".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
        }
    }

    /// 设置 `protocol_version` 并返回更新后的实例。
    pub fn with_protocol_version(mut self, protocol_version: impl Into<String>) -> Self {
        self.protocol_version = protocol_version.into();
        self
    }

    /// 设置 `client_name` 并返回更新后的实例。
    pub fn with_client_name(mut self, client_name: impl Into<String>) -> Self {
        self.client_name = client_name.into();
        self
    }

    /// 设置 `client_version` 并返回更新后的实例。
    pub fn with_client_version(mut self, client_version: impl Into<String>) -> Self {
        self.client_version = client_version.into();
        self
    }

    /// 设置 `request_timeout_ms` 并返回更新后的实例。
    pub fn with_request_timeout_ms(mut self, request_timeout_ms: u64) -> Self {
        self.request_timeout_ms = request_timeout_ms;
        self
    }

    /// 设置 `output_limit_bytes` 并返回更新后的实例。
    pub fn with_output_limit_bytes(mut self, output_limit_bytes: usize) -> Self {
        self.output_limit_bytes = output_limit_bytes;
        self
    }
}

impl McpJsonRpcClient {
    /// 创建并初始化当前类型的实例。
    pub fn new(adapter_id: AdapterId, allowlist: McpAllowlist) -> Self {
        Self {
            adapter_id,
            allowlist,
            config: McpJsonRpcClientConfig::default(),
        }
    }

    /// 设置 `config` 并返回更新后的实例。
    pub fn with_config(mut self, config: McpJsonRpcClientConfig) -> Self {
        self.config = config;
        self
    }

    /// 执行 `config` 对应的处理逻辑。
    pub fn config(&self) -> &McpJsonRpcClientConfig {
        &self.config
    }

    /// 执行 `call_stdio` 对应的受控流程。
    pub fn call_stdio(
        &self,
        session_config: &McpSessionConfig,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        self.allowlist.require_tool(tool)?;
        match session_config.server_transport {
            McpServerTransport::Stdio => {
                validate_stdio_config(session_config)?;
                let child = Command::new(&session_config.process.command)
                    .args(&session_config.process.args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|error| {
                        EvaError::unavailable("failed to start MCP stdio server")
                            .with_context("adapter_id", session_config.adapter_id.as_str())
                            .with_context("command", &session_config.process.command)
                            .with_context("io_error", error.to_string())
                    })?;
                let mut transport =
                    McpStdioJsonRpcTransport::start_with_process(session_config, Box::new(child))?;
                let call = self.call_tool_with_transport(&mut transport, request_id, tool, input);
                let shutdown_audit = transport.shutdown();
                let mut report = call?;
                report.audit.extend(shutdown_audit);
                Ok(report)
            }
            McpServerTransport::Http | McpServerTransport::StreamableHttp => Err(
                EvaError::unsupported("use MCP HTTP JSON-RPC transport for HTTP server_transport")
                    .with_context("adapter_id", session_config.adapter_id.as_str())
                    .with_context("server_transport", session_config.server_transport.as_str()),
            ),
        }
    }

    /// Call an MCP stdio server around a process owned by an external
    /// supervisor. The transport always shuts the supplied process down before
    /// returning, including protocol and timeout failures.
    pub fn call_stdio_with_process(
        &self,
        session_config: &McpSessionConfig,
        mut process: Box<dyn McpStdioProcess>,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        if let Err(error) = self.allowlist.require_tool(tool) {
            let _ = process.terminate();
            return Err(error);
        }
        if session_config.server_transport != McpServerTransport::Stdio {
            let error = EvaError::unsupported("MCP process handle requires stdio server transport")
                .with_context("adapter_id", session_config.adapter_id.as_str())
                .with_context("server_transport", session_config.server_transport.as_str());
            let _ = process.terminate();
            return Err(error);
        }
        let mut transport = McpStdioJsonRpcTransport::start_with_process(session_config, process)?;
        let call = self.call_tool_with_transport(&mut transport, request_id, tool, input);
        let shutdown_audit = transport.shutdown();
        let mut report = call?;
        report.audit.extend(shutdown_audit);
        Ok(report)
    }

    /// 执行 `call_http` 对应的受控流程。
    pub fn call_http(
        &self,
        endpoint: &str,
        headers: BTreeMap<String, String>,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        self.allowlist.require_tool(tool)?;
        let mut transport = McpHttpJsonRpcTransport::new(endpoint, headers)?;
        self.call_tool_with_transport(&mut transport, request_id, tool, input)
    }

    /// 使用已验证的 Streamable HTTP 配置执行一次 MCP 调用。
    pub fn call_http_with_config(
        &self,
        config: &McpStreamableHttpConfig,
        headers: BTreeMap<String, String>,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        self.call_http_with_config_and_tls(
            config,
            headers,
            McpTlsMaterial::new(),
            request_id,
            tool,
            input,
        )
    }

    /// Execute an MCP HTTP call with per-invocation TLS material.
    pub fn call_http_with_config_and_tls(
        &self,
        config: &McpStreamableHttpConfig,
        headers: BTreeMap<String, String>,
        tls_material: McpTlsMaterial,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        validate_client_config(&self.config)?;
        self.allowlist.require_tool(tool)?;
        let mut transport = McpHttpJsonRpcTransport::new_with_config_and_tls(
            config.clone(),
            headers,
            tls_material,
        )?;
        self.call_tool_with_transport(&mut transport, request_id, tool, input)
    }

    /// 执行 `call_tool_with_transport` 对应的受控流程。
    pub fn call_tool_with_transport(
        &self,
        transport: &mut impl McpJsonRpcTransport,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        validate_client_config(&self.config)?;
        self.allowlist.require_tool(tool)?;

        let timeout = Duration::from_millis(self.config.request_timeout_ms);
        let mut next_id = 1;

        let initialize_id = next_json_rpc_id(&mut next_id);
        let initialize = initialize_request(initialize_id, &self.config);
        let initialize_response = transport.exchange(
            initialize_id,
            &initialize,
            timeout,
            self.config.output_limit_bytes,
        )?;
        enforce_response_limit(&initialize_response, self.config.output_limit_bytes)?;
        parse_json_rpc_response(&initialize_response, initialize_id)?;

        transport.notify(
            &initialized_notification(),
            timeout,
            self.config.output_limit_bytes,
        )?;

        let list_id = next_json_rpc_id(&mut next_id);
        let list_request = tools_list_request(list_id);
        let list_response = transport.exchange(
            list_id,
            &list_request,
            timeout,
            self.config.output_limit_bytes,
        )?;
        enforce_response_limit(&list_response, self.config.output_limit_bytes)?;
        let listed_tools = parse_tools_list(&parse_json_rpc_response(&list_response, list_id)?)?;
        if !listed_tools.iter().any(|entry| entry.name == tool) {
            return Err(
                EvaError::not_found("MCP server did not advertise allowlisted tool")
                    .with_provider_code("mcp_tool_not_listed")
                    .with_context("adapter_id", self.adapter_id.as_str())
                    .with_context("tool", tool),
            );
        }

        let call_id = next_json_rpc_id(&mut next_id);
        let call_request = tools_call_request(call_id, tool, input);
        let call_response = transport.exchange(
            call_id,
            &call_request,
            timeout,
            self.config.output_limit_bytes,
        )?;
        enforce_response_limit(&call_response, self.config.output_limit_bytes)?;
        let call = parse_json_rpc_response(&call_response, call_id)?;

        let mut audit = vec![
            "transport:mcp".to_owned(),
            "mcp.client:json_rpc".to_owned(),
            "mcp.method:initialize".to_owned(),
            "mcp.notification:initialized".to_owned(),
            "mcp.method:tools/list".to_owned(),
            "mcp.method:tools/call".to_owned(),
            format!("mcp.tool:{tool}"),
            "tool_allowlist:passed".to_owned(),
        ];
        audit.extend(transport.audit());

        Ok(McpJsonRpcCallReport {
            request_id,
            adapter_id: self.adapter_id.clone(),
            tool: tool.to_owned(),
            output: InvokeOutput::text(format!(
                "{{\"transport\":\"mcp\",\"adapter_id\":{},\"tool\":{},\"mode\":\"json-rpc\",\"result\":{}}}",
                json_string(self.adapter_id.as_str()),
                json_string(tool),
                call.result
            )),
            audit,
        })
    }
}

impl McpStdioJsonRpcTransport {
    /// 执行 `start` 对应的受控流程。
    pub fn start(config: &McpSessionConfig) -> Result<Self, EvaError> {
        validate_stdio_config(config)?;
        let child = Command::new(&config.process.command)
            .args(&config.process.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                EvaError::unavailable("failed to start MCP stdio server")
                    .with_context("adapter_id", config.adapter_id.as_str())
                    .with_context("command", &config.process.command)
                    .with_context("io_error", error.to_string())
            })?;
        Self::start_with_process(config, Box::new(child))
    }

    /// Start a JSON-RPC transport around a process already spawned by the
    /// central supervisor. Any pipe transfer failure terminates the supplied
    /// process before returning the error.
    pub fn start_with_process(
        config: &McpSessionConfig,
        mut process: Box<dyn McpStdioProcess>,
    ) -> Result<Self, EvaError> {
        if let Err(error) = validate_stdio_config(config) {
            let _ = process.terminate();
            return Err(error);
        }
        let started_at = Instant::now();
        let process_id = process.process_id();

        let Some(stdin) = process.take_stdin() else {
            let _ = process.terminate();
            return Err(EvaError::internal("MCP stdio stdin was not available")
                .with_context("adapter_id", config.adapter_id.as_str()));
        };
        let Some(stdout) = process.take_stdout() else {
            let _ = process.terminate();
            return Err(EvaError::internal("MCP stdio stdout was not available")
                .with_context("adapter_id", config.adapter_id.as_str()));
        };
        let (sender, receiver) = mpsc::channel();
        spawn_stdout_reader(stdout, sender);

        let audit = vec![
            "mcp.stdio:started".to_owned(),
            "shell:false".to_owned(),
            "command_allowlist:passed".to_owned(),
            format!("startup_timeout_ms:{}", config.process.startup_timeout_ms),
            format!("startup_duration_ms:{}", started_at.elapsed().as_millis()),
            format!("process_id:{process_id}"),
        ];

        Ok(Self {
            process: Some(process),
            stdin,
            receiver,
            audit,
        })
    }

    /// 停止或释放 `shutdown` 管理的资源。
    pub fn shutdown(&mut self) -> Vec<String> {
        let mut audit = Vec::new();
        // Closing stdin is the portable cooperative shutdown signal for
        // stdio servers. Replace the field before waiting so a blocked writer
        // cannot keep the child alive while the process boundary is reaped.
        let stdin = std::mem::replace(&mut self.stdin, Box::new(io::sink()));
        drop(stdin);
        if let Some(mut process) = self.process.take() {
            match process.terminate_gracefully(Duration::from_millis(250)) {
                Ok(()) => audit.push("mcp.stdio:stopped".to_owned()),
                Err(_) => audit.push("mcp.stdio:termination_failed".to_owned()),
            }
        }
        audit
    }
}

impl McpJsonRpcTransport for McpStdioJsonRpcTransport {
    /// 执行 `exchange` 对应的处理逻辑。
    fn exchange(
        &mut self,
        expected_id: u64,
        request: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<String, EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP response output limit must be greater than zero",
            ));
        }
        self.stdin.write_all(request.as_bytes()).map_err(|error| {
            EvaError::unavailable("failed to write MCP JSON-RPC request")
                .with_context("io_error", error.to_string())
        })?;
        self.stdin.write_all(b"\n").map_err(|error| {
            EvaError::unavailable("failed to write MCP JSON-RPC newline")
                .with_context("io_error", error.to_string())
        })?;
        self.stdin.flush().map_err(|error| {
            EvaError::unavailable("failed to flush MCP JSON-RPC request")
                .with_context("io_error", error.to_string())
        })?;

        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                self.shutdown();
                return Err(EvaError::timeout("MCP JSON-RPC response timed out")
                    .with_context("json_rpc_id", expected_id.to_string()));
            }
            let remaining = deadline.saturating_duration_since(now);
            match self.receiver.recv_timeout(remaining) {
                Ok(ReaderMessage::Line(line)) => {
                    let text = String::from_utf8(line).map_err(|error| {
                        EvaError::unavailable("MCP JSON-RPC response was not UTF-8")
                            .with_provider_code("mcp_protocol_error")
                            .with_context("utf8_error", error.to_string())
                    })?;
                    enforce_response_limit(&text, output_limit_bytes)?;
                    match json_u64_field(&text, "id")? {
                        Some(id) if id == expected_id => return Ok(text),
                        Some(id) => {
                            return Err(protocol_error("MCP JSON-RPC response id mismatch")
                                .with_context("expected_id", expected_id.to_string())
                                .with_context("actual_id", id.to_string()));
                        }
                        None => continue,
                    }
                }
                Ok(ReaderMessage::LimitExceeded) => {
                    self.shutdown();
                    return Err(
                        EvaError::conflict("MCP JSON-RPC response exceeded output limit")
                            .with_provider_code("mcp_response_too_large")
                            .with_context("output_limit_bytes", output_limit_bytes.to_string()),
                    );
                }
                Ok(ReaderMessage::ReadError(error)) => {
                    self.shutdown();
                    return Err(
                        EvaError::unavailable("failed to read MCP JSON-RPC response")
                            .with_context("io_error", error),
                    );
                }
                Ok(ReaderMessage::Eof) => {
                    self.shutdown();
                    return Err(EvaError::unavailable("MCP stdio server closed stdout")
                        .with_provider_code("mcp_stdio_eof"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.shutdown();
                    return Err(EvaError::timeout("MCP JSON-RPC response timed out")
                        .with_context("json_rpc_id", expected_id.to_string()));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.shutdown();
                    return Err(EvaError::unavailable("MCP stdio reader stopped")
                        .with_provider_code("mcp_stdio_reader_stopped"));
                }
            }
        }
    }

    /// 执行 `notify` 对应的处理逻辑。
    fn notify(
        &mut self,
        notification: &str,
        _timeout: Duration,
        _output_limit_bytes: usize,
    ) -> Result<(), EvaError> {
        self.stdin
            .write_all(notification.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|error| {
                EvaError::unavailable("failed to write MCP JSON-RPC notification")
                    .with_context("io_error", error.to_string())
            })
    }

    /// 执行 `audit` 对应的处理逻辑。
    fn audit(&self) -> Vec<String> {
        self.audit.clone()
    }
}

impl McpHttpJsonRpcTransport {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        endpoint: impl Into<String>,
        headers: BTreeMap<String, String>,
    ) -> Result<Self, EvaError> {
        let endpoint = endpoint.into();
        let config = McpStreamableHttpConfig::legacy_http(endpoint)?;
        Self::new_with_config(config, headers)
    }

    /// 使用已解析并校验的配置创建 HTTP transport。
    pub fn new_with_config(
        config: McpStreamableHttpConfig,
        headers: BTreeMap<String, String>,
    ) -> Result<Self, EvaError> {
        Self::new_with_config_and_tls(config, headers, McpTlsMaterial::new())
    }

    /// Create an HTTP transport with explicit per-invocation TLS material.
    pub fn new_with_config_and_tls(
        config: McpStreamableHttpConfig,
        headers: BTreeMap<String, String>,
        tls_material: McpTlsMaterial,
    ) -> Result<Self, EvaError> {
        config.validate_for_environment("dev")?;
        let endpoint = config.endpoint.clone();
        let parsed = ParsedHttpUrl::parse(&endpoint)?;
        let mut normalized_headers = BTreeSet::new();
        for (name, value) in &headers {
            validate_http_header(name, value)?;
            if !normalized_headers.insert(name.to_ascii_lowercase()) {
                return Err(EvaError::invalid_argument(
                    "MCP HTTP header names must be unique ignoring ASCII case",
                )
                .with_context("header", name));
            }
        }
        let connector = McpHttpConnector::from_config(&config, tls_material)?;
        let mut audit = vec![
            "mcp.http:client".to_owned(),
            format!("mcp.http.origin:{}", parsed.origin),
            "mcp.http.method:POST".to_owned(),
        ];
        if parsed.scheme == "https" {
            audit.push("mcp.tls:certificate_verification_enabled".to_owned());
            audit.push("mcp.tls:sni_enabled".to_owned());
            audit.push(format!(
                "mcp.tls:client_auth_configured:{}",
                config.client_auth.is_some()
            ));
        }
        for name in headers.keys() {
            audit.push(format!("mcp.http.header:{name}:redacted"));
        }
        Ok(Self {
            endpoint,
            headers,
            connector,
            audit,
            exchange_count: 0,
            notification_count: 0,
        })
    }
}

impl McpJsonRpcTransport for McpHttpJsonRpcTransport {
    /// 执行 `exchange` 对应的处理逻辑。
    fn exchange(
        &mut self,
        expected_id: u64,
        request: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<String, EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP HTTP response output limit must be greater than zero",
            ));
        }
        self.exchange_count = self.exchange_count.saturating_add(1);
        let response = send_http_json_rpc_request(
            &self.connector,
            &self.endpoint,
            &self.headers,
            request,
            timeout,
            output_limit_bytes,
            false,
        )?;
        decode_http_exchange_response(response, expected_id, output_limit_bytes)
    }

    /// 执行 `notify` 对应的处理逻辑。
    fn notify(
        &mut self,
        notification: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<(), EvaError> {
        self.notification_count = self.notification_count.saturating_add(1);
        let response = send_http_json_rpc_request(
            &self.connector,
            &self.endpoint,
            &self.headers,
            notification,
            timeout,
            output_limit_bytes,
            true,
        )?;
        validate_http_notification_response(response, output_limit_bytes)
    }

    /// 执行 `audit` 对应的处理逻辑。
    fn audit(&self) -> Vec<String> {
        let mut audit = self.audit.clone();
        audit.push(format!("mcp.http.exchange_count:{}", self.exchange_count));
        audit.push(format!(
            "mcp.http.notification_count:{}",
            self.notification_count
        ));
        audit
    }
}

impl Drop for McpStdioJsonRpcTransport {
    /// 停止或释放 `drop` 管理的资源。
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 定义 `ReaderMessage` 可取的状态。
#[derive(Debug)]
enum ReaderMessage {
    /// 表示 `Line` 枚举分支。
    Line(
        /// 保存一行尚未执行 UTF-8 与 JSON-RPC 校验的原始字节。
        Vec<u8>,
    ),
    /// 表示 `LimitExceeded` 枚举分支。
    LimitExceeded,
    /// 表示 `ReadError` 枚举分支。
    ReadError(
        /// 保存读取线程产生且可跨通道传递的错误文本。
        String,
    ),
    /// 表示 `Eof` 枚举分支。
    Eof,
}

/// 执行 `spawn_stdout_reader` 对应的受控流程。
fn spawn_stdout_reader(
    stdout: impl std::io::Read + Send + 'static,
    sender: mpsc::Sender<ReaderMessage>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            let buffer = match reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(error) => {
                    let _ = sender.send(ReaderMessage::ReadError(error.to_string()));
                    return;
                }
            };
            if buffer.is_empty() {
                let _ = sender.send(ReaderMessage::Eof);
                return;
            }
            let take = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|position| position + 1)
                .unwrap_or(buffer.len());
            if line.len().saturating_add(take) > DEFAULT_OUTPUT_LIMIT_BYTES * 16 {
                let _ = sender.send(ReaderMessage::LimitExceeded);
                return;
            }
            line.extend_from_slice(&buffer[..take]);
            reader.consume(take);
            if line.ends_with(b"\n") {
                let _ = sender.send(ReaderMessage::Line(std::mem::take(&mut line)));
            }
        }
    });
}

/// 表示 `HttpJsonRpcResponse` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpJsonRpcResponse {
    /// 记录 `status_code` 字段对应的值。
    status_code: u16,
    /// Parsed response media type, without parameters.
    content_type: Option<String>,
    /// 记录 `body` 字段对应的值。
    body: Vec<u8>,
    /// 记录 `body_truncated` 字段对应的值。
    body_truncated: bool,
}

/// HTTP response body framing selected from the response status and headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpBodyFraming {
    /// The status code forbids a response body.
    None,
    /// A fixed number of octets follows the header block.
    ContentLength(usize),
    /// The body is encoded as HTTP/1.1 chunks.
    Chunked,
    /// The server closes the connection to delimit the body.
    CloseDelimited,
}

/// Parsed response headers needed by the framing state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpResponseHead {
    status_code: u16,
    http_11: bool,
    content_length: Option<usize>,
    transfer_encoding_chunked: bool,
    content_type: Option<String>,
    connection_close: bool,
    connection_keep_alive: bool,
}

/// 表示 `ParsedHttpUrl` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedHttpUrl {
    /// 记录 `scheme` 字段对应的值。
    scheme: String,
    /// 记录 `host` 字段对应的值。
    host: String,
    /// 记录 `port` 字段对应的值。
    port: u16,
    /// 记录 `path` 字段对应的值。
    path: String,
    /// 记录 `authority` 字段对应的值。
    authority: String,
    /// 记录 `origin` 字段对应的值。
    origin: String,
}

impl ParsedHttpUrl {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    fn parse(url: &str) -> Result<Self, EvaError> {
        let canonical = McpEndpoint::canonicalize(url)?;
        let (scheme, rest) = canonical
            .split_once("://")
            .ok_or_else(|| EvaError::invalid_argument("MCP HTTP URL must include a scheme"))?;
        let authority = rest
            .split(['/', '?', '#'])
            .next()
            .filter(|authority| !authority.trim().is_empty())
            .ok_or_else(|| EvaError::invalid_argument("MCP HTTP URL must include a host"))?;
        if authority.contains('@') {
            return Err(
                EvaError::invalid_argument("MCP HTTP URL must not include userinfo")
                    .with_context("url", url),
            );
        }
        let (host, port) = parse_http_authority(scheme, authority)?;
        let path_start = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let path = if path_start < rest.len() {
            let value = &rest[path_start..];
            if value.starts_with('?') || value.starts_with('#') {
                format!("/{value}")
            } else {
                value.to_owned()
            }
        } else {
            "/".to_owned()
        };
        Ok(Self {
            scheme: scheme.to_owned(),
            host,
            port,
            path,
            authority: authority.to_owned(),
            origin: format!("{scheme}://{authority}"),
        })
    }
}

/// 读取或解析 `parse_http_authority` 所需的数据，失败时保留错误语义。
fn parse_http_authority(scheme: &str, authority: &str) -> Result<(String, u16), EvaError> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if authority.starts_with('[') {
        let end = authority.find(']').ok_or_else(|| {
            EvaError::invalid_argument("MCP HTTP URL IPv6 authority is malformed")
        })?;
        let host = authority[1..end]
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| EvaError::invalid_argument("MCP HTTP URL IPv6 address is malformed"))?;
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else {
            let value = suffix.strip_prefix(':').ok_or_else(|| {
                EvaError::invalid_argument("MCP HTTP URL IPv6 authority is malformed")
            })?;
            parse_http_port(value)?
        };
        return Ok((host.to_string(), port));
    }
    if authority.bytes().filter(|byte| *byte == b':').count() > 1 {
        return Err(EvaError::invalid_argument(
            "MCP HTTP URL IPv6 addresses must use brackets",
        ));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() || port.is_empty() {
            return Err(EvaError::invalid_argument(
                "MCP HTTP URL authority is malformed",
            ));
        }
        return Ok((host.to_owned(), parse_http_port(port)?));
    }
    Ok((authority.to_owned(), default_port))
}

fn parse_http_port(value: &str) -> Result<u16, EvaError> {
    let port = value.parse::<u16>().map_err(|error| {
        EvaError::invalid_argument("MCP HTTP URL port is invalid")
            .with_context("port", value)
            .with_context("parse_error", error.to_string())
    })?;
    if port == 0 {
        return Err(EvaError::invalid_argument(
            "MCP HTTP URL port must be non-zero",
        ));
    }
    Ok(port)
}

/// 执行 `send_http_json_rpc_request` 对应的处理逻辑。
fn send_http_json_rpc_request(
    connector: &McpHttpConnector,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    body: &str,
    timeout: Duration,
    output_limit_bytes: usize,
    allow_bodyless_accepted: bool,
) -> Result<HttpJsonRpcResponse, EvaError> {
    let parsed = ParsedHttpUrl::parse(endpoint)?;
    let mut stream = connector.connect(
        &parsed.scheme,
        &parsed.host,
        parsed.port,
        &parsed.origin,
        timeout,
    )?;

    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\n",
        parsed.path,
        parsed.authority,
        body.len()
    );
    for (name, value) in headers {
        validate_http_header(name, value)?;
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|error| map_http_write_error(error, &parsed.origin))?;

    if allow_bodyless_accepted {
        read_http_json_rpc_response_with_options(
            &mut stream,
            &parsed.origin,
            output_limit_bytes,
            true,
        )
    } else {
        read_http_json_rpc_response(&mut stream, &parsed.origin, output_limit_bytes)
    }
}

/// 定义 `HTTP_HEADER_LIMIT_BYTES` 常量。
const HTTP_HEADER_LIMIT_BYTES: usize = 64 * 1024;

/// 读取或解析 `read_http_json_rpc_response` 所需的数据，失败时保留错误语义。
fn read_http_json_rpc_response(
    stream: &mut impl Read,
    origin: &str,
    output_limit_bytes: usize,
) -> Result<HttpJsonRpcResponse, EvaError> {
    read_http_json_rpc_response_with_options(stream, origin, output_limit_bytes, false)
}

/// Read a response with the caller's notification-specific bodyless status policy.
fn read_http_json_rpc_response_with_options(
    stream: &mut impl Read,
    origin: &str,
    output_limit_bytes: usize,
    allow_bodyless_accepted: bool,
) -> Result<HttpJsonRpcResponse, EvaError> {
    if output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "MCP HTTP response output limit must be greater than zero",
        ));
    }
    let mut reader = BufReader::new(stream);
    let mut informational_responses = 0_u8;
    let head = loop {
        let head = read_http_response_head(&mut reader, origin)?;
        if (100..200).contains(&head.status_code) && head.status_code != 101 {
            if select_http_body_framing(&head, origin, false)? != HttpBodyFraming::None {
                return Err(http_framing_error(
                    origin,
                    "informational response carried a body",
                ));
            }
            informational_responses = informational_responses.saturating_add(1);
            if informational_responses > 8 {
                return Err(http_framing_error(
                    origin,
                    "too many informational responses",
                ));
            }
            continue;
        }
        break head;
    };
    let framing = select_http_body_framing(&head, origin, allow_bodyless_accepted)?;
    let (body, body_truncated) = match framing {
        HttpBodyFraming::None => (Vec::new(), false),
        HttpBodyFraming::ContentLength(length) => {
            if length > output_limit_bytes {
                (Vec::new(), true)
            } else {
                let mut body = vec![0_u8; length];
                read_http_exact(&mut reader, &mut body, origin)?;
                (body, false)
            }
        }
        HttpBodyFraming::Chunked => {
            read_http_chunked_body(&mut reader, output_limit_bytes, origin)?
        }
        HttpBodyFraming::CloseDelimited => {
            read_http_close_delimited_body(&mut reader, output_limit_bytes, origin)?
        }
    };
    Ok(HttpJsonRpcResponse {
        status_code: head.status_code,
        content_type: head.content_type,
        body,
        body_truncated,
    })
}

/// Read one CRLF-terminated HTTP line without allowing unbounded buffering.
fn read_http_line(
    reader: &mut impl BufRead,
    consumed: &mut usize,
    origin: &str,
    limit: usize,
) -> Result<Vec<u8>, EvaError> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        let read = reader
            .read(&mut byte)
            .map_err(|error| map_http_read_error(error, origin))?;
        if read == 0 {
            return Err(http_body_incomplete_error(
                origin,
                "missing CRLF-terminated line",
            ));
        }
        *consumed = consumed.saturating_add(1);
        if *consumed > limit {
            return Err(
                EvaError::conflict("MCP HTTP response headers exceeded limit")
                    .with_provider_code("mcp_http_headers_too_large")
                    .with_context("origin", origin)
                    .with_context("header_limit_bytes", limit.to_string()),
            );
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    if line.len() < 2 || !line.ends_with(b"\r\n") {
        return Err(http_framing_error(origin, "line is not CRLF terminated"));
    }
    line.truncate(line.len() - 2);
    Ok(line)
}

/// Parse the status line and response headers used for body framing.
fn read_http_response_head(
    reader: &mut impl BufRead,
    origin: &str,
) -> Result<HttpResponseHead, EvaError> {
    let mut consumed = 0_usize;
    let status_line = read_http_line(reader, &mut consumed, origin, HTTP_HEADER_LIMIT_BYTES)?;
    let (status_code, http_11) = parse_http_status_line(&status_line, origin)?;
    let mut head = HttpResponseHead {
        status_code,
        http_11,
        content_length: None,
        transfer_encoding_chunked: false,
        content_type: None,
        connection_close: false,
        connection_keep_alive: false,
    };

    loop {
        let line = read_http_line(reader, &mut consumed, origin, HTTP_HEADER_LIMIT_BYTES)?;
        if line.is_empty() {
            break;
        }
        let (name, value) = parse_http_header_line(&line, origin)?;
        match name.as_str() {
            "content-length" => {
                if head.content_length.is_some() {
                    return Err(http_framing_error(origin, "duplicate content-length"));
                }
                head.content_length = Some(parse_http_content_length(&value, origin)?);
            }
            "transfer-encoding" => {
                if head.transfer_encoding_chunked {
                    return Err(http_framing_error(origin, "duplicate transfer-encoding"));
                }
                let tokens = value.split(',').map(str::trim).collect::<Vec<_>>();
                if tokens.len() != 1
                    || tokens[0].is_empty()
                    || !tokens[0].eq_ignore_ascii_case("chunked")
                {
                    return Err(http_framing_error(origin, "unsupported transfer-encoding"));
                }
                head.transfer_encoding_chunked = true;
            }
            "content-type" => {
                if head.content_type.is_some() {
                    return Err(http_framing_error(origin, "duplicate content-type"));
                }
                head.content_type = Some(parse_http_media_type(&value, origin)?);
            }
            "connection" => {
                for token in value.split(',').map(str::trim) {
                    if token.eq_ignore_ascii_case("close") {
                        head.connection_close = true;
                    } else if token.eq_ignore_ascii_case("keep-alive") {
                        head.connection_keep_alive = true;
                    }
                }
                if head.connection_close && head.connection_keep_alive {
                    return Err(http_framing_error(
                        origin,
                        "conflicting connection directives",
                    ));
                }
            }
            "content-encoding" if !value.trim().eq_ignore_ascii_case("identity") => {
                return Err(http_framing_error(origin, "unsupported content-encoding"));
            }
            _ => {}
        }
    }
    Ok(head)
}

/// Parse a strict HTTP status line and return whether it is HTTP/1.1.
fn parse_http_status_line(line: &[u8], origin: &str) -> Result<(u16, bool), EvaError> {
    let text = std::str::from_utf8(line)
        .map_err(|_| http_framing_error(origin, "status line is not valid UTF-8"))?;
    let mut fields = text.splitn(3, ' ');
    let version = fields
        .next()
        .ok_or_else(|| http_framing_error(origin, "missing HTTP version"))?;
    let code = fields
        .next()
        .ok_or_else(|| http_framing_error(origin, "missing status code"))?;
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(http_framing_error(origin, "unsupported HTTP version"));
    }
    if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(http_framing_error(origin, "invalid status code"));
    }
    let status_code = code
        .parse::<u16>()
        .map_err(|_| http_framing_error(origin, "invalid status code"))?;
    if !(100..=599).contains(&status_code) {
        return Err(http_framing_error(
            origin,
            "status code is outside HTTP range",
        ));
    }
    if let Some(reason) = fields.next() {
        if reason
            .bytes()
            .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
        {
            return Err(http_framing_error(origin, "invalid reason phrase"));
        }
    }
    Ok((status_code, version == "HTTP/1.1"))
}

/// Parse a response header line without exposing its value in errors.
fn parse_http_header_line(line: &[u8], origin: &str) -> Result<(String, String), EvaError> {
    if line
        .first()
        .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
    {
        return Err(http_framing_error(origin, "obsolete folded header"));
    }
    let colon = line
        .iter()
        .position(|byte| *byte == b':')
        .ok_or_else(|| http_framing_error(origin, "header is missing colon"))?;
    if colon == 0 || !line[..colon].iter().copied().all(is_http_token_byte) {
        return Err(http_framing_error(origin, "invalid header name"));
    }
    if line[colon + 1..]
        .iter()
        .any(|byte| *byte < 0x20 && *byte != b'\t')
        || line[colon + 1..].contains(&0x7f)
    {
        return Err(http_framing_error(origin, "invalid header value"));
    }
    let name = std::str::from_utf8(&line[..colon])
        .map_err(|_| http_framing_error(origin, "header name is not ASCII"))?
        .to_ascii_lowercase();
    let value = String::from_utf8_lossy(&line[colon + 1..])
        .trim_matches(|character| character == ' ' || character == '\t')
        .to_owned();
    Ok((name, value))
}

/// Parse and bound a response Content-Length value.
fn parse_http_content_length(value: &str, origin: &str) -> Result<usize, EvaError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(http_framing_error(origin, "invalid content-length"));
    }
    value
        .parse::<usize>()
        .map_err(|_| http_framing_error(origin, "content-length overflows host size"))
}

/// Parse a media type and discard parameters that do not affect framing.
fn parse_http_media_type(value: &str, origin: &str) -> Result<String, EvaError> {
    let media_type = value
        .split(';')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| http_framing_error(origin, "empty content-type"))?;
    if !media_type.contains('/') || media_type.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(http_framing_error(origin, "invalid content-type"));
    }
    Ok(media_type.to_ascii_lowercase())
}

/// Select a body framing mode from a parsed response head.
fn select_http_body_framing(
    head: &HttpResponseHead,
    origin: &str,
    allow_bodyless_accepted: bool,
) -> Result<HttpBodyFraming, EvaError> {
    let body_forbidden =
        (100..200).contains(&head.status_code) || matches!(head.status_code, 204 | 205 | 304);
    if body_forbidden {
        if head.transfer_encoding_chunked
            || ((100..200).contains(&head.status_code) || matches!(head.status_code, 204 | 205))
                && head.content_length.unwrap_or(0) > 0
        {
            return Err(http_framing_error(origin, "body is forbidden for status"));
        }
        return Ok(HttpBodyFraming::None);
    }
    if allow_bodyless_accepted
        && head.status_code == 202
        && head.content_length.is_none()
        && !head.transfer_encoding_chunked
    {
        return Ok(HttpBodyFraming::None);
    }
    if head.transfer_encoding_chunked {
        if head.content_length.is_some() {
            return Err(http_framing_error(
                origin,
                "content-length conflicts with transfer-encoding",
            ));
        }
        return Ok(HttpBodyFraming::Chunked);
    }
    if let Some(length) = head.content_length {
        return Ok(HttpBodyFraming::ContentLength(length));
    }
    if head.connection_close || (!head.http_11 && !head.connection_keep_alive) {
        return Ok(HttpBodyFraming::CloseDelimited);
    }
    Err(http_framing_error(
        origin,
        "response body framing is missing",
    ))
}

/// Read a fixed number of octets, distinguishing clean EOF from truncation.
fn read_http_exact(
    reader: &mut impl Read,
    target: &mut [u8],
    origin: &str,
) -> Result<(), EvaError> {
    let mut offset = 0_usize;
    while offset < target.len() {
        let read = reader
            .read(&mut target[offset..])
            .map_err(|error| map_http_read_error(error, origin))?;
        if read == 0 {
            return Err(http_body_incomplete_error(
                origin,
                "content-length body ended early",
            ));
        }
        offset += read;
    }
    Ok(())
}

/// Decode a chunked response body with bounded chunk metadata and trailers.
fn read_http_chunked_body(
    reader: &mut impl BufRead,
    output_limit_bytes: usize,
    origin: &str,
) -> Result<(Vec<u8>, bool), EvaError> {
    let mut metadata_bytes = 0_usize;
    let mut body = Vec::new();
    loop {
        let line = read_http_line(reader, &mut metadata_bytes, origin, HTTP_HEADER_LIMIT_BYTES)?;
        if line.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
            return Err(http_framing_error(origin, "invalid chunk extension"));
        }
        let size_text = line.split(|byte| *byte == b';').next().unwrap_or_default();
        let size_text = std::str::from_utf8(size_text)
            .map_err(|_| http_framing_error(origin, "chunk size is not ASCII"))?;
        if size_text.is_empty() || !size_text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(http_framing_error(origin, "invalid chunk size"));
        }
        let size = u64::from_str_radix(size_text, 16)
            .ok()
            .and_then(|size| usize::try_from(size).ok())
            .ok_or_else(|| http_framing_error(origin, "chunk size overflows host size"))?;
        if size == 0 {
            loop {
                let trailer =
                    read_http_line(reader, &mut metadata_bytes, origin, HTTP_HEADER_LIMIT_BYTES)?;
                if trailer.is_empty() {
                    return Ok((body, false));
                }
                let (name, _) = parse_http_header_line(&trailer, origin)?;
                if matches!(
                    name.as_str(),
                    "host"
                        | "content-length"
                        | "transfer-encoding"
                        | "content-type"
                        | "content-encoding"
                        | "connection"
                        | "trailer"
                        | "te"
                        | "upgrade"
                        | "proxy-authenticate"
                        | "proxy-authorization"
                ) {
                    return Err(http_framing_error(origin, "forbidden trailer field"));
                }
            }
        }
        let remaining = output_limit_bytes.saturating_sub(body.len());
        if size > remaining {
            return Ok((body, true));
        }
        let mut chunk = vec![0_u8; size];
        read_http_exact(reader, &mut chunk, origin)?;
        body.extend_from_slice(&chunk);
        let mut delimiter = [0_u8; 2];
        read_http_exact(reader, &mut delimiter, origin)?;
        if delimiter != *b"\r\n" {
            return Err(http_framing_error(
                origin,
                "chunk data is not CRLF terminated",
            ));
        }
    }
}

/// Read a close-delimited body while retaining the response output bound.
fn read_http_close_delimited_body(
    reader: &mut impl Read,
    output_limit_bytes: usize,
    origin: &str,
) -> Result<(Vec<u8>, bool), EvaError> {
    let mut body = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| map_http_read_error(error, origin))?;
        if read == 0 {
            return Ok((body, false));
        }
        let remaining = output_limit_bytes.saturating_sub(body.len());
        if read > remaining {
            body.extend_from_slice(&buffer[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&buffer[..read]);
        if body.len() == output_limit_bytes {
            let mut extra = [0_u8; 1];
            let read = reader
                .read(&mut extra)
                .map_err(|error| map_http_read_error(error, origin))?;
            if read == 0 {
                return Ok((body, false));
            }
            return Ok((body, true));
        }
    }
}

/// Decode a response that must carry one JSON-RPC object.
fn decode_http_exchange_response(
    response: HttpJsonRpcResponse,
    expected_id: u64,
    output_limit_bytes: usize,
) -> Result<String, EvaError> {
    if !(200..300).contains(&response.status_code) {
        return Err(
            EvaError::unavailable("MCP HTTP server returned non-success status")
                .with_provider_code("mcp_http_status")
                .with_context("json_rpc_id", expected_id.to_string())
                .with_context("status_code", response.status_code.to_string()),
        );
    }
    if response.body_truncated {
        return Err(
            EvaError::conflict("MCP HTTP response exceeded output limit")
                .with_provider_code("mcp_response_too_large")
                .with_context("json_rpc_id", expected_id.to_string())
                .with_context("output_limit_bytes", output_limit_bytes.to_string()),
        );
    }
    if response.body.is_empty() {
        return Err(
            EvaError::unavailable("MCP HTTP response did not contain a JSON-RPC body")
                .with_provider_code("mcp_http_body_missing")
                .with_context("json_rpc_id", expected_id.to_string())
                .with_context("status_code", response.status_code.to_string()),
        );
    }
    require_json_content_type(response.content_type.as_deref())?;
    let text = String::from_utf8(response.body).map_err(|error| {
        EvaError::unavailable("MCP HTTP response was not UTF-8")
            .with_provider_code("mcp_protocol_error")
            .with_context("utf8_error", error.to_string())
    })?;
    enforce_response_limit(&text, output_limit_bytes)?;
    Ok(text)
}

/// Validate a notification response, which may legitimately be bodyless.
fn validate_http_notification_response(
    response: HttpJsonRpcResponse,
    output_limit_bytes: usize,
) -> Result<(), EvaError> {
    if !(200..300).contains(&response.status_code) {
        return Err(
            EvaError::unavailable("MCP HTTP notification returned non-success status")
                .with_provider_code("mcp_http_status")
                .with_context("status_code", response.status_code.to_string()),
        );
    }
    if response.body_truncated {
        return Err(
            EvaError::conflict("MCP HTTP notification response exceeded output limit")
                .with_provider_code("mcp_response_too_large")
                .with_context("output_limit_bytes", output_limit_bytes.to_string()),
        );
    }
    if !response.body.is_empty() {
        require_json_content_type(response.content_type.as_deref())?;
    }
    Ok(())
}

/// Require the media type supported by the JSON-RPC HTTP path.
fn require_json_content_type(content_type: Option<&str>) -> Result<(), EvaError> {
    match content_type {
        Some("application/json") => Ok(()),
        Some(_) => Err(
            EvaError::unsupported("MCP HTTP response content type is unsupported")
                .with_provider_code("mcp_http_content_type_unsupported"),
        ),
        None => Err(
            EvaError::unavailable("MCP HTTP response content type is missing")
                .with_provider_code("mcp_http_content_type_missing"),
        ),
    }
}

/// Return a stable framing error without echoing untrusted response data.
fn http_framing_error(origin: &str, reason: &'static str) -> EvaError {
    EvaError::unavailable("MCP HTTP response framing is invalid")
        .with_provider_code("mcp_http_framing_invalid")
        .with_context("origin", origin)
        .with_context("reason", reason)
}

/// Return a stable incomplete-body error.
fn http_body_incomplete_error(origin: &str, reason: &'static str) -> EvaError {
    EvaError::unavailable("MCP HTTP response body ended before its framing completed")
        .with_provider_code("mcp_http_body_incomplete")
        .with_context("origin", origin)
        .with_context("reason", reason)
}

/// Match the RFC token grammar for response header names.
fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Find a CRLFCRLF terminator for the test request reader.
#[cfg(test)]
fn http_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// 校验 `validate_http_header` 对应的约束，不满足时返回明确错误。
fn validate_http_header(name: &str, value: &str) -> Result<(), EvaError> {
    if name.trim().is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(
            EvaError::invalid_argument("MCP HTTP header name is unsupported")
                .with_context("header", name),
        );
    }
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "content-type"
            | "accept"
            | "content-length"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-connection"
    ) {
        return Err(
            EvaError::permission_denied("MCP HTTP header is controlled by the transport")
                .with_context("header", name),
        );
    }
    if value
        .chars()
        .any(|character| character != '\t' && character.is_control())
    {
        return Err(EvaError::invalid_argument(
            "MCP HTTP header value contains a control character",
        )
        .with_context("header", name));
    }
    Ok(())
}

/// 校验 `validate_client_config` 对应的约束，不满足时返回明确错误。
fn validate_client_config(config: &McpJsonRpcClientConfig) -> Result<(), EvaError> {
    if config.protocol_version.trim().is_empty() {
        return Err(EvaError::invalid_argument(
            "MCP protocol version must be non-empty",
        ));
    }
    if config.client_name.trim().is_empty() {
        return Err(EvaError::invalid_argument(
            "MCP client name must be non-empty",
        ));
    }
    if config.request_timeout_ms == 0 {
        return Err(EvaError::invalid_argument(
            "MCP JSON-RPC request timeout must be greater than zero",
        ));
    }
    if config.output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "MCP JSON-RPC output limit must be greater than zero",
        ));
    }
    Ok(())
}

/// 校验 `validate_stdio_config` 对应的约束，不满足时返回明确错误。
fn validate_stdio_config(config: &McpSessionConfig) -> Result<(), EvaError> {
    if config.server_transport != McpServerTransport::Stdio {
        return Err(EvaError::unsupported("unsupported MCP JSON-RPC transport")
            .with_context("server_transport", config.server_transport.as_str()));
    }
    if config.process.command.trim().is_empty() {
        return Err(
            EvaError::invalid_argument("MCP stdio command cannot be empty")
                .with_context("adapter_id", config.adapter_id.as_str()),
        );
    }
    if !config
        .process
        .allowed_commands
        .contains(&config.process.command)
    {
        return Err(
            EvaError::permission_denied("MCP stdio command is not allowlisted")
                .with_context("adapter_id", config.adapter_id.as_str())
                .with_context("command", &config.process.command),
        );
    }
    if config.process.startup_timeout_ms == 0 {
        return Err(EvaError::invalid_argument(
            "MCP stdio startup timeout must be greater than zero",
        ));
    }
    Ok(())
}

/// 执行 `next_json_rpc_id` 对应的处理逻辑。
fn next_json_rpc_id(next: &mut u64) -> u64 {
    let id = *next;
    *next += 1;
    id
}

/// 执行 `initialize_request` 对应的处理逻辑。
fn initialize_request(id: u64, config: &McpJsonRpcClientConfig) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"initialize\",\"params\":{{\"protocolVersion\":{},\"capabilities\":{{}},\"clientInfo\":{{\"name\":{},\"version\":{}}}}}}}",
        id,
        json_string(&config.protocol_version),
        json_string(&config.client_name),
        json_string(&config.client_version)
    )
}

/// 执行 `initialized_notification` 对应的处理逻辑。
fn initialized_notification() -> String {
    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}".to_owned()
}

/// 执行 `tools_list_request` 对应的处理逻辑。
fn tools_list_request(id: u64) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/list\",\"params\":{{}}}}",
        id
    )
}

/// 执行 `tools_call_request` 对应的处理逻辑。
fn tools_call_request(id: u64, tool: &str, input: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/call\",\"params\":{{\"name\":{},\"arguments\":{}}}}}",
        id,
        json_string(tool),
        tool_arguments_json(input)
    )
}

/// 执行 `tool_arguments_json` 对应的处理逻辑。
fn tool_arguments_json(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        "{}".to_owned()
    } else if trimmed.starts_with('{') && trimmed.ends_with('}') {
        trimmed.to_owned()
    } else {
        format!("{{\"input\":{}}}", json_string(input))
    }
}

/// 读取或解析 `parse_json_rpc_response` 所需的数据，失败时保留错误语义。
fn parse_json_rpc_response(response: &str, expected_id: u64) -> Result<JsonRpcResponse, EvaError> {
    if json_string_field(response, "jsonrpc")?.as_deref() != Some("2.0") {
        return Err(protocol_error("MCP JSON-RPC response has invalid version")
            .with_context("json_rpc_id", expected_id.to_string()));
    }
    match json_u64_field(response, "id")? {
        Some(id) if id == expected_id => {}
        Some(id) => {
            return Err(protocol_error("MCP JSON-RPC response id mismatch")
                .with_context("expected_id", expected_id.to_string())
                .with_context("actual_id", id.to_string()));
        }
        None => {
            return Err(protocol_error("MCP JSON-RPC response is missing id")
                .with_context("expected_id", expected_id.to_string()));
        }
    }
    if let Some(error) = json_field_value(response, "error") {
        return Err(map_json_rpc_error(error, expected_id));
    }
    let result = json_field_value(response, "result")
        .ok_or_else(|| {
            protocol_error("MCP JSON-RPC response is missing result")
                .with_context("json_rpc_id", expected_id.to_string())
        })?
        .to_owned();
    Ok(JsonRpcResponse { result })
}

/// 读取或解析 `parse_tools_list` 所需的数据，失败时保留错误语义。
fn parse_tools_list(response: &JsonRpcResponse) -> Result<Vec<McpJsonRpcTool>, EvaError> {
    let tools = json_field_value(&response.result, "tools").ok_or_else(|| {
        protocol_error("MCP tools/list response is missing tools")
            .with_provider_code("mcp_tools_list_missing_tools")
    })?;
    let mut names = Vec::new();
    let mut offset = 0;
    while let Some(position) = tools[offset..].find("\"name\"") {
        let start = offset + position;
        let Some(value) = json_field_value(&tools[start..], "name") else {
            break;
        };
        let name = parse_json_string(value)?;
        names.push(McpJsonRpcTool { name });
        offset = start + 6;
        if offset >= tools.len() {
            break;
        }
    }
    Ok(names)
}

/// 执行 `map_json_rpc_error` 对应的处理逻辑。
fn map_json_rpc_error(error: &str, expected_id: u64) -> EvaError {
    let code = json_i64_field(error, "code").ok().flatten().unwrap_or(0);
    let message = json_string_field(error, "message")
        .ok()
        .flatten()
        .unwrap_or_else(|| "MCP JSON-RPC error".to_owned());
    EvaError::unavailable("MCP JSON-RPC error object returned")
        .with_provider_code(format!("json_rpc_{code}"))
        .with_context("json_rpc_id", expected_id.to_string())
        .with_context("json_rpc_code", code.to_string())
        .with_context("json_rpc_message", truncate_context(&message))
}

/// 执行 `enforce_response_limit` 对应的处理逻辑。
fn enforce_response_limit(response: &str, output_limit_bytes: usize) -> Result<(), EvaError> {
    if response.len() > output_limit_bytes {
        Err(
            EvaError::conflict("MCP JSON-RPC response exceeded output limit")
                .with_provider_code("mcp_response_too_large")
                .with_context("output_limit_bytes", output_limit_bytes.to_string())
                .with_context("actual_bytes", response.len().to_string()),
        )
    } else {
        Ok(())
    }
}

/// 执行 `protocol_error` 对应的处理逻辑。
fn protocol_error(message: impl Into<String>) -> EvaError {
    EvaError::unavailable(message)
        .with_provider_code("mcp_protocol_error")
        .with_retryable(false)
}

/// 执行 `truncate_context` 对应的处理逻辑。
fn truncate_context(value: &str) -> String {
    /// 定义 `MAX_CONTEXT_CHARS` 常量。
    const MAX_CONTEXT_CHARS: usize = 120;
    value.chars().take(MAX_CONTEXT_CHARS).collect()
}

/// 执行 `json_field_value` 对应的处理逻辑。
fn json_field_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let position = text.find(&pattern)?;
    let after_key = position + pattern.len();
    let colon = text[after_key..].find(':')?;
    let mut value_start = after_key + colon + 1;
    value_start += text[value_start..]
        .chars()
        .take_while(|character| character.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    let value_end = json_value_end(text, value_start)?;
    Some(&text[value_start..value_end])
}

/// 执行 `json_value_end` 对应的处理逻辑。
fn json_value_end(text: &str, start: usize) -> Option<usize> {
    match text[start..].chars().next()? {
        '"' => json_string_end(text, start),
        '{' => balanced_json_end(text, start, '{', '}'),
        '[' => balanced_json_end(text, start, '[', ']'),
        _ => {
            let mut end = text.len();
            for (offset, character) in text[start..].char_indices() {
                if character == ','
                    || character == '}'
                    || character == ']'
                    || character.is_whitespace()
                {
                    end = start + offset;
                    break;
                }
            }
            Some(end)
        }
    }
}

/// 执行 `json_string_end` 对应的处理逻辑。
fn json_string_end(text: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (offset, character) in text[start + 1..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => return Some(start + 1 + offset + character.len_utf8()),
            _ => {}
        }
    }
    None
}

/// 执行 `balanced_json_end` 对应的处理逻辑。
fn balanced_json_end(text: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character == '"' {
            in_string = true;
        } else if character == open {
            depth += 1;
        } else if character == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + offset + character.len_utf8());
            }
        }
    }
    None
}

/// 执行 `json_string_field` 对应的处理逻辑。
fn json_string_field(text: &str, key: &str) -> Result<Option<String>, EvaError> {
    json_field_value(text, key)
        .map(parse_json_string)
        .transpose()
}

/// 执行 `json_u64_field` 对应的处理逻辑。
fn json_u64_field(text: &str, key: &str) -> Result<Option<u64>, EvaError> {
    json_field_value(text, key)
        .map(|value| {
            value.trim().parse::<u64>().map_err(|error| {
                protocol_error("MCP JSON-RPC numeric field is invalid")
                    .with_context("field", key)
                    .with_context("parse_error", error.to_string())
            })
        })
        .transpose()
}

/// 执行 `json_i64_field` 对应的处理逻辑。
fn json_i64_field(text: &str, key: &str) -> Result<Option<i64>, EvaError> {
    json_field_value(text, key)
        .map(|value| {
            value.trim().parse::<i64>().map_err(|error| {
                protocol_error("MCP JSON-RPC numeric field is invalid")
                    .with_context("field", key)
                    .with_context("parse_error", error.to_string())
            })
        })
        .transpose()
}

/// 读取或解析 `parse_json_string` 所需的数据，失败时保留错误语义。
fn parse_json_string(value: &str) -> Result<String, EvaError> {
    if !value.starts_with('"') {
        return Err(protocol_error("MCP JSON-RPC string field is invalid"));
    }
    let mut output = String::new();
    let mut chars = value[1..].chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Ok(output),
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| protocol_error("MCP JSON-RPC string escape is incomplete"))?;
                match escaped {
                    '"' => output.push('"'),
                    '\\' => output.push('\\'),
                    '/' => output.push('/'),
                    'b' => output.push('\u{0008}'),
                    'f' => output.push('\u{000c}'),
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    'u' => {
                        let mut hex = String::new();
                        for _ in 0..4 {
                            hex.push(chars.next().ok_or_else(|| {
                                protocol_error("MCP JSON-RPC unicode escape is incomplete")
                            })?);
                        }
                        let code = u16::from_str_radix(&hex, 16).map_err(|error| {
                            protocol_error("MCP JSON-RPC unicode escape is invalid")
                                .with_context("parse_error", error.to_string())
                        })?;
                        if let Some(character) = char::from_u32(code as u32) {
                            output.push(character);
                        }
                    }
                    _ => {
                        return Err(protocol_error("MCP JSON-RPC string escape is unsupported"));
                    }
                }
            }
            value => output.push(value),
        }
    }
    Err(protocol_error("MCP JSON-RPC string field is unterminated"))
}

/// 执行 `json_string` 对应的处理逻辑。
fn json_string(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

/// 按 `escape_json` 的协议约定生成输出。
fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value => escaped.push(value),
        }
    }
    escaped
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpRedirectPolicy;
    use eva_core::ErrorKind;
    use rustls::{ServerConfig, ServerConnection, StreamOwned};
    use std::collections::VecDeque;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;

    const TEST_CA_PEM: &[u8] = include_bytes!("../testdata/tls/ca.pem");
    const TEST_SERVER_PEM: &[u8] = include_bytes!("../testdata/tls/server.pem");
    const TEST_SERVER_KEY: &[u8] = include_bytes!("../testdata/tls/server.key");

    /// 表示 `FakeTransport` 数据结构。
    #[derive(Debug, Default)]
    struct FakeTransport {
        /// 记录 `responses` 字段对应的值。
        responses: VecDeque<Result<String, EvaError>>,
        /// 记录 `requests` 字段对应的值。
        requests: Vec<String>,
        /// 记录 `notifications` 字段对应的值。
        notifications: Vec<String>,
    }

    /// A reader that exposes prescribed fragments and can fail instead of EOF.
    #[derive(Debug)]
    struct SegmentedReader {
        chunks: VecDeque<Vec<u8>>,
        terminal_error: Option<io::ErrorKind>,
        terminal_reads: usize,
    }

    impl SegmentedReader {
        fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                chunks: chunks.into_iter().collect(),
                terminal_error: None,
                terminal_reads: 0,
            }
        }

        fn keep_alive(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                chunks: chunks.into_iter().collect(),
                terminal_error: Some(io::ErrorKind::TimedOut),
                terminal_reads: 0,
            }
        }

        fn fragmented(bytes: &[u8]) -> Self {
            Self::keep_alive(bytes.iter().map(|byte| vec![*byte]))
        }
    }

    impl Read for SegmentedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            while self.chunks.front().is_some_and(Vec::is_empty) {
                self.chunks.pop_front();
            }
            let Some(chunk) = self.chunks.front_mut() else {
                self.terminal_reads = self.terminal_reads.saturating_add(1);
                return match self.terminal_error {
                    Some(kind) => Err(io::Error::new(kind, "scripted keep-alive reader")),
                    None => Ok(0),
                };
            };
            let read = buffer.len().min(chunk.len());
            buffer[..read].copy_from_slice(&chunk[..read]);
            chunk.drain(..read);
            Ok(read)
        }
    }

    impl FakeTransport {
        /// 设置 `responses` 并返回更新后的实例。
        fn with_responses(responses: impl IntoIterator<Item = String>) -> Self {
            Self {
                responses: responses.into_iter().map(Ok).collect(),
                requests: Vec::new(),
                notifications: Vec::new(),
            }
        }

        /// 设置 `error` 并返回更新后的实例。
        fn with_error(error: EvaError) -> Self {
            Self {
                responses: [Err(error)].into_iter().collect(),
                requests: Vec::new(),
                notifications: Vec::new(),
            }
        }
    }

    impl McpJsonRpcTransport for FakeTransport {
        /// 执行 `exchange` 对应的处理逻辑。
        fn exchange(
            &mut self,
            _expected_id: u64,
            request: &str,
            _timeout: Duration,
            _output_limit_bytes: usize,
        ) -> Result<String, EvaError> {
            self.requests.push(request.to_owned());
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(EvaError::unavailable("fake response missing")))
        }

        /// 执行 `notify` 对应的处理逻辑。
        fn notify(
            &mut self,
            notification: &str,
            _timeout: Duration,
            _output_limit_bytes: usize,
        ) -> Result<(), EvaError> {
            self.notifications.push(notification.to_owned());
            Ok(())
        }
    }

    /// 验证 `json_rpc_client_calls_fake_mcp_server_tool` 场景下的预期行为。
    #[test]
    fn json_rpc_client_calls_fake_mcp_server_tool() {
        let client = client(["list_issues"]);
        let mut transport = FakeTransport::with_responses([
            response(1, "{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fake\",\"version\":\"1\"}}"),
            response(2, "{\"tools\":[{\"name\":\"list_issues\",\"inputSchema\":{\"type\":\"object\"}}]}"),
            response(3, "{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}],\"isError\":false}"),
        ]);

        let report = client
            .call_tool_with_transport(
                &mut transport,
                RequestId::parse("req-mcp-json-rpc").unwrap(),
                "list_issues",
                "{\"owner\":\"eva\"}",
            )
            .unwrap();

        assert_eq!(report.tool, "list_issues");
        assert!(report
            .output
            .as_text()
            .unwrap()
            .contains("\"mode\":\"json-rpc\""));
        assert_eq!(transport.requests.len(), 3);
        assert!(transport.requests[0].contains("\"method\":\"initialize\""));
        assert!(transport.requests[1].contains("\"method\":\"tools/list\""));
        assert!(transport.requests[2].contains("\"method\":\"tools/call\""));
        assert!(transport.notifications[0].contains("notifications/initialized"));
    }

    /// 验证 `json_rpc_client_calls_fake_http_mcp_server_with_auth_header` 场景下的预期行为。
    #[test]
    fn json_rpc_client_calls_fake_http_mcp_server_with_auth_header() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (sender, receiver) = channel();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default();
                let response = if body.contains("\"method\":\"initialize\"") {
                    http_response(
                        200,
                        &response(1, "{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fake-http\",\"version\":\"1\"}}"),
                    )
                } else if body.contains("notifications/initialized") {
                    http_response(202, "")
                } else if body.contains("\"method\":\"tools/list\"") {
                    http_response(
                        200,
                        &response(2, "{\"tools\":[{\"name\":\"list_issues\",\"inputSchema\":{\"type\":\"object\"}}]}"),
                    )
                } else if body.contains("\"method\":\"tools/call\"") {
                    http_response(
                        200,
                        &response(
                            3,
                            "{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}],\"isError\":false}",
                        ),
                    )
                } else {
                    http_response(400, "")
                };
                stream.write_all(response.as_bytes()).unwrap();
                sender.send(request).unwrap();
            }
        });
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_owned(), "Bearer test-token".to_owned());

        let report = client(["list_issues"])
            .call_http(
                &endpoint,
                headers,
                RequestId::parse("req-mcp-http").unwrap(),
                "list_issues",
                "{\"owner\":\"eva\"}",
            )
            .unwrap();

        assert_eq!(report.tool, "list_issues");
        assert!(report
            .audit
            .contains(&"mcp.http.exchange_count:3".to_owned()));
        assert!(report
            .audit
            .contains(&"mcp.http.notification_count:1".to_owned()));
        let requests = (0..4)
            .map(|_| receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect::<Vec<_>>();
        server.join().unwrap();
        assert!(requests
            .iter()
            .all(|request| request.contains("Authorization: Bearer test-token")));
        assert!(requests
            .iter()
            .any(|request| request.contains("\"method\":\"tools/call\"")));
    }

    #[test]
    fn json_rpc_client_calls_fake_https_mcp_server_through_tls_stream() {
        let listener = TcpListener::bind(("localhost", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = format!("https://localhost:{port}/mcp");
        let origin = format!("https://localhost:{port}");
        let server_config = test_https_server_config();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let connection = ServerConnection::new(server_config.clone()).unwrap();
                let mut stream = StreamOwned::new(connection, socket);
                let request = read_test_http_request_from(&mut stream);
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default();
                let response = if body.contains("\"method\":\"initialize\"") {
                    http_response(
                        200,
                        &response(1, "{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fake-https\",\"version\":\"1\"}}"),
                    )
                } else if body.contains("notifications/initialized") {
                    http_response(202, "")
                } else if body.contains("\"method\":\"tools/list\"") {
                    http_response(
                        200,
                        &response(2, "{\"tools\":[{\"name\":\"list_issues\",\"inputSchema\":{\"type\":\"object\"}}]}"),
                    )
                } else if body.contains("\"method\":\"tools/call\"") {
                    http_response(
                        200,
                        &response(
                            3,
                            "{\"content\":[{\"type\":\"text\",\"text\":\"tls-ok\"}],\"isError\":false}",
                        ),
                    )
                } else {
                    http_response(400, "")
                };
                stream.write_all(response.as_bytes()).unwrap();
                stream.conn.send_close_notify();
                stream.flush().unwrap();
            }
        });
        let config = McpStreamableHttpConfig::from_parts(
            endpoint,
            ["pem:test-ca"],
            None,
            McpRedirectPolicy::Deny,
            [origin],
        )
        .unwrap();
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();

        let report = client(["list_issues"])
            .call_http_with_config_and_tls(
                &config,
                BTreeMap::new(),
                material,
                RequestId::parse("req-mcp-https").unwrap(),
                "list_issues",
                "{\"owner\":\"eva\"}",
            )
            .unwrap();

        server.join().unwrap();
        assert!(report
            .audit
            .contains(&"mcp.tls:certificate_verification_enabled".to_owned()));
        assert!(report.audit.contains(&"mcp.tls:sni_enabled".to_owned()));
        assert_eq!(report.tool, "list_issues");
        assert!(report.output.as_text().unwrap().contains("tls-ok"));
    }

    #[test]
    fn json_rpc_client_handles_fragmented_chunked_keep_alive_http_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default();
                let response = if body.contains("\"method\":\"initialize\"") {
                    keep_alive_http_response(
                        200,
                        &response(1, "{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"framed\",\"version\":\"1\"}}"),
                    )
                } else if body.contains("notifications/initialized") {
                    "HTTP/1.1 202 Accepted\r\nConnection: keep-alive\r\n\r\n".to_owned()
                } else if body.contains("\"method\":\"tools/list\"") {
                    chunked_keep_alive_http_response(&response(
                        2,
                        "{\"tools\":[{\"name\":\"list_issues\",\"inputSchema\":{\"type\":\"object\"}}]}",
                    ))
                } else if body.contains("\"method\":\"tools/call\"") {
                    keep_alive_http_response(
                        200,
                        &response(
                            3,
                            "{\"content\":[{\"type\":\"text\",\"text\":\"framed-ok\"}],\"isError\":false}",
                        ),
                    )
                } else {
                    keep_alive_http_response(400, "")
                };
                for fragment in response.as_bytes().chunks(3) {
                    stream.write_all(fragment).unwrap();
                }
                stream.flush().unwrap();
                let mut after_response = [0_u8; 1];
                assert_eq!(stream.read(&mut after_response).unwrap(), 0);
            }
        });

        let report = client(["list_issues"])
            .call_http(
                &endpoint,
                BTreeMap::new(),
                RequestId::parse("req-mcp-framed-http").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap();

        server.join().unwrap();
        assert!(report.output.as_text().unwrap().contains("framed-ok"));
    }

    #[test]
    fn http_framing_reads_fragmented_content_length_without_waiting_for_eof() {
        let body = br#"{"ok":true}"#;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        let bytes = [head.as_bytes(), body].concat();
        let mut reader = SegmentedReader::fragmented(&bytes);

        let response =
            read_http_json_rpc_response(&mut reader, "http://framing.test", 1024).unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(response.content_type.as_deref(), Some("application/json"));
        assert_eq!(response.body, body);
        assert!(!response.body_truncated);
        assert_eq!(reader.terminal_reads, 0);
    }

    #[test]
    fn http_framing_skips_bounded_informational_responses() {
        let body = br#"{"ok":true}"#;
        let head = format!(
            "HTTP/1.1 100 Continue\r\nContent-Length: 0\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        let bytes = [head.as_bytes(), body].concat();
        let mut reader = SegmentedReader::fragmented(&bytes);
        let response =
            read_http_json_rpc_response(&mut reader, "http://framing.test", 1024).unwrap();
        assert_eq!(response.status_code, 200);
        assert_eq!(response.body, body);
        assert_eq!(reader.terminal_reads, 0);

        let mut invalid =
            SegmentedReader::new([b"HTTP/1.1 100 Continue\r\nContent-Length: 1\r\n\r\nx".to_vec()]);
        let error =
            read_http_json_rpc_response(&mut invalid, "http://framing.test", 1024).unwrap_err();
        assert_provider_code(&error, "mcp_http_framing_invalid");
    }

    #[test]
    fn http_framing_decodes_fragmented_chunks_extensions_and_trailers() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n6;fixture=yes\r\n{\"ok\":\r\n5\r\ntrue}\r\n0\r\nX-Trace: complete\r\n\r\n";
        let mut reader = SegmentedReader::fragmented(raw);

        let response =
            read_http_json_rpc_response(&mut reader, "http://framing.test", 1024).unwrap();

        assert_eq!(response.body, br#"{"ok":true}"#);
        assert!(!response.body_truncated);
        assert_eq!(reader.terminal_reads, 0);
    }

    #[test]
    fn http_framing_handles_bodyless_202_and_204_without_waiting_for_eof() {
        for (status, raw) in [
            (
                202,
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
                    .as_slice(),
            ),
            (
                204,
                b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n".as_slice(),
            ),
        ] {
            let mut reader = SegmentedReader::fragmented(raw);
            let response =
                read_http_json_rpc_response(&mut reader, "http://framing.test", 1024).unwrap();
            assert_eq!(response.status_code, status);
            assert!(response.body.is_empty());
            validate_http_notification_response(response.clone(), 1024).unwrap();
            let error = decode_http_exchange_response(response, 7, 1024).unwrap_err();
            assert_provider_code(&error, "mcp_http_body_missing");
            assert_eq!(reader.terminal_reads, 0);
        }

        let raw = b"HTTP/1.1 202 Accepted\r\nConnection: keep-alive\r\n\r\n";
        let mut strict = SegmentedReader::fragmented(raw);
        let error =
            read_http_json_rpc_response(&mut strict, "http://framing.test", 1024).unwrap_err();
        assert_provider_code(&error, "mcp_http_framing_invalid");

        let mut notification = SegmentedReader::fragmented(raw);
        let response = read_http_json_rpc_response_with_options(
            &mut notification,
            "http://framing.test",
            1024,
            true,
        )
        .unwrap();
        validate_http_notification_response(response, 1024).unwrap();
        assert_eq!(notification.terminal_reads, 0);
    }

    #[test]
    fn http_framing_rejects_ambiguous_or_invalid_response_headers() {
        for raw in [
            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nx",
            "HTTP/1.1 200 OK\r\n Folded: value\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\nContent-Length: 0\n\n",
            "HTTP/2 200 OK\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 099 Invalid\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 600 Invalid\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nConnection: keep-alive\r\n\r\n",
            "HTTP/1.1 204 No Content\r\nContent-Length: 1\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 1\r\n\r\nx",
        ] {
            let mut reader = SegmentedReader::new([raw.as_bytes().to_vec()]);
            let error =
                read_http_json_rpc_response(&mut reader, "http://framing.test", 1024).unwrap_err();
            assert_provider_code(&error, "mcp_http_framing_invalid");
        }
    }

    #[test]
    fn http_framing_rejects_incomplete_or_malformed_bodies() {
        for (raw, expected_code) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nxx",
                "mcp_http_body_incomplete",
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nnot-hex\r\n",
                "mcp_http_framing_invalid",
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nxXX",
                "mcp_http_framing_invalid",
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nContent-Length: 0\r\n\r\n",
                "mcp_http_framing_invalid",
            ),
        ] {
            let mut reader = SegmentedReader::new([raw.as_bytes().to_vec()]);
            let error = read_http_json_rpc_response(
                &mut reader,
                "http://framing.test",
                1024,
            )
            .unwrap_err();
            assert_provider_code(&error, expected_code);
        }
    }

    #[test]
    fn http_framing_enforces_header_and_decoded_body_limits_before_waiting() {
        let mut fixed = SegmentedReader::keep_alive([
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 99\r\nConnection: keep-alive\r\n\r\n"
                .to_vec(),
        ]);
        let response = read_http_json_rpc_response(&mut fixed, "http://framing.test", 8).unwrap();
        assert!(response.body_truncated);
        assert_eq!(fixed.terminal_reads, 0);

        let mut chunked = SegmentedReader::keep_alive([
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n"
                .to_vec(),
            b"9\r\n".to_vec(),
        ]);
        let response = read_http_json_rpc_response(&mut chunked, "http://framing.test", 8).unwrap();
        assert!(response.body_truncated);
        assert_eq!(chunked.terminal_reads, 0);

        let oversized_header = format!(
            "HTTP/1.1 200 OK\r\nX-Oversized: {}\r\nContent-Length: 0\r\n\r\n",
            "x".repeat(HTTP_HEADER_LIMIT_BYTES)
        );
        let mut header_reader = SegmentedReader::new([oversized_header.into_bytes()]);
        let error =
            read_http_json_rpc_response(&mut header_reader, "http://framing.test", 8).unwrap_err();
        assert_provider_code(&error, "mcp_http_headers_too_large");
    }

    #[test]
    fn http_response_semantics_require_json_for_nonempty_bodies() {
        let valid = HttpJsonRpcResponse {
            status_code: 200,
            content_type: Some("application/json".to_owned()),
            body: br#"{"jsonrpc":"2.0","id":1,"result":{}}"#.to_vec(),
            body_truncated: false,
        };
        assert!(decode_http_exchange_response(valid.clone(), 1, 1024).is_ok());

        let missing = HttpJsonRpcResponse {
            content_type: None,
            ..valid.clone()
        };
        assert_provider_code(
            &decode_http_exchange_response(missing, 1, 1024).unwrap_err(),
            "mcp_http_content_type_missing",
        );

        let unsupported = HttpJsonRpcResponse {
            content_type: Some("text/plain".to_owned()),
            ..valid
        };
        assert_provider_code(
            &decode_http_exchange_response(unsupported, 1, 1024).unwrap_err(),
            "mcp_http_content_type_unsupported",
        );

        let truncated_notification = HttpJsonRpcResponse {
            status_code: 202,
            content_type: None,
            body: Vec::new(),
            body_truncated: true,
        };
        assert_provider_code(
            &validate_http_notification_response(truncated_notification, 8).unwrap_err(),
            "mcp_response_too_large",
        );
    }

    #[test]
    fn http_transport_rejects_reserved_and_injected_headers() {
        for header in [
            "Host",
            "content-length",
            "Transfer-Encoding",
            "Connection",
            "Content-Type",
            "Accept",
        ] {
            assert!(
                validate_http_header(header, "controlled").is_err(),
                "{header}"
            );
        }
        assert!(validate_http_header("X-Custom", "ok").is_ok());
        assert!(validate_http_header("X-Custom", "one\ttwo").is_ok());
        assert!(validate_http_header("X-Custom", "ok\r\nInjected: true").is_err());
        assert!(validate_http_header("X-Custom", "bad\0value").is_err());
        assert!(validate_http_header("X-Custom", "bad\u{7f}value").is_err());

        let mut duplicate_headers = BTreeMap::new();
        duplicate_headers.insert("X-Token".to_owned(), "first".to_owned());
        duplicate_headers.insert("x-token".to_owned(), "second".to_owned());
        let config = McpStreamableHttpConfig::legacy_http("http://127.0.0.1/mcp").unwrap();
        assert!(McpHttpJsonRpcTransport::new_with_config(config, duplicate_headers).is_err());

        let config = McpStreamableHttpConfig::from_parts(
            "https://example.test/mcp",
            ["file:certs/root.pem"],
            None,
            McpRedirectPolicy::Deny,
            ["https://example.test"],
        )
        .unwrap();
        let error = McpHttpJsonRpcTransport::new_with_config_and_tls(
            config.clone(),
            BTreeMap::from([("Host".to_owned(), "attacker.test".to_owned())]),
            McpTlsMaterial::new(),
        )
        .unwrap_err();
        assert!(error.message().contains("controlled by the transport"));
        assert_ne!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_tls_project_root_required")
        );

        let client = client(["list_issues"])
            .with_config(McpJsonRpcClientConfig::new().with_request_timeout_ms(0));
        let error = client
            .call_http_with_config_and_tls(
                &config,
                BTreeMap::new(),
                McpTlsMaterial::new(),
                RequestId::parse("req-mcp-invalid-client-preflight").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap_err();
        assert!(error
            .message()
            .contains("timeout must be greater than zero"));
        assert_ne!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_tls_project_root_required")
        );
    }

    /// 验证 `blocked_tool_does_not_send_rpc` 场景下的预期行为。
    #[test]
    fn blocked_tool_does_not_send_rpc() {
        let client = client(["list_issues"]);
        let mut transport = FakeTransport::default();

        let error = client
            .call_tool_with_transport(
                &mut transport,
                RequestId::parse("req-mcp-blocked").unwrap(),
                "delete_repo",
                "{}",
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(transport.requests.is_empty());
        assert!(transport.notifications.is_empty());
    }

    /// 验证 `blocked_http_tool_is_rejected_before_endpoint_validation` 场景下的预期行为。
    #[test]
    fn blocked_http_tool_is_rejected_before_endpoint_validation() {
        let error = client(["list_issues"])
            .call_http(
                "not-a-url",
                BTreeMap::new(),
                RequestId::parse("req-mcp-http-blocked").unwrap(),
                "delete_repo",
                "{}",
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    /// 验证 `json_rpc_error_object_maps_to_stable_provider_error` 场景下的预期行为。
    #[test]
    fn json_rpc_error_object_maps_to_stable_provider_error() {
        let client = client(["list_issues"]);
        let mut transport = FakeTransport::with_responses([
            response(1, "{\"protocolVersion\":\"2025-11-25\"}"),
            response(2, "{\"tools\":[{\"name\":\"list_issues\"}]}"),
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":-32602,\"message\":\"bad params\"}}"
                .to_owned(),
        ]);

        let error = client
            .call_tool_with_transport(
                &mut transport,
                RequestId::parse("req-mcp-error").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("json_rpc_-32602")
        );
    }

    /// 验证 `oversized_response_has_stable_error` 场景下的预期行为。
    #[test]
    fn oversized_response_has_stable_error() {
        let client = client(["list_issues"]).with_config(
            McpJsonRpcClientConfig::new()
                .with_request_timeout_ms(1_000)
                .with_output_limit_bytes(32),
        );
        let mut transport = FakeTransport::with_responses([response(
            1,
            "{\"protocolVersion\":\"2025-11-25\",\"padding\":\"this response is too large\"}",
        )]);

        let error = client
            .call_tool_with_transport(
                &mut transport,
                RequestId::parse("req-mcp-large").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_response_too_large")
        );
    }

    /// 验证 `transport_timeout_is_preserved` 场景下的预期行为。
    #[test]
    fn transport_timeout_is_preserved() {
        let client = client(["list_issues"]);
        let mut transport = FakeTransport::with_error(EvaError::timeout("fake timeout"));

        let error = client
            .call_tool_with_transport(
                &mut transport,
                RequestId::parse("req-mcp-timeout").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(transport.requests.len(), 1);
    }

    /// 执行 `client` 对应的处理逻辑。
    fn client(tools: impl IntoIterator<Item = &'static str>) -> McpJsonRpcClient {
        McpJsonRpcClient::new(
            AdapterId::parse("github-mcp").unwrap(),
            McpAllowlist::from_tools(tools).unwrap(),
        )
    }

    /// 执行 `response` 对应的处理逻辑。
    fn response(id: u64, result: &str) -> String {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}")
    }

    /// 执行 `http_response` 对应的处理逻辑。
    fn http_response(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn keep_alive_http_response(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
            body.len()
        )
    }

    fn chunked_keep_alive_http_response(body: &str) -> String {
        let split = body.len() / 2;
        let (first, second) = body.split_at(split);
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\nX-Test: complete\r\n\r\n",
            first.len(),
            second.len()
        )
    }

    fn assert_provider_code(error: &EvaError, expected: &str) {
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some(expected),
            "{error:?}"
        );
    }

    /// 读取或解析 `read_test_http_request` 所需的数据，失败时保留错误语义。
    fn read_test_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        read_test_http_request_from(stream)
    }

    fn read_test_http_request_from(stream: &mut impl Read) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 512];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(header_end) = http_header_end(&bytes) {
                let header = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
                let content_length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let body_start = header_end + 4;
                while bytes.len().saturating_sub(body_start) < content_length {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn test_https_server_config() -> Arc<ServerConfig> {
        let certificates = rustls_pemfile::certs(&mut BufReader::new(TEST_SERVER_PEM))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let private_key = rustls_pemfile::private_key(&mut BufReader::new(TEST_SERVER_KEY))
            .unwrap()
            .unwrap();
        let provider = rustls::crypto::ring::default_provider();
        Arc::new(
            ServerConfig::builder_with_provider(Arc::new(provider))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certificates, private_key)
                .unwrap(),
        )
    }
}
