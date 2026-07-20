//! 实现 MCP 的 JSON-RPC 客户端以及 stdio/HTTP 传输边界。
//!
//! 每次调用先校验工具允许列表，再完成初始化握手并发送工具请求。响应必须命中预期 id、
//! 符合协议结构且不超过大小限制；stdio 的读取线程只传递有界行，超时、EOF、读取错误或
//! 协议错配都会关闭子进程并映射为稳定错误，避免遗留失控会话。
//! MCP JSON-RPC client transport.

use crate::policy::McpAllowlist;
use crate::session::{McpServerTransport, McpSessionConfig};
use eva_core::{AdapterId, EvaError, InvokeOutput, RequestId};
use std::collections::BTreeMap;
use std::fmt::{self, Debug};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
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
#[derive(Clone, PartialEq, Eq)]
pub struct McpHttpJsonRpcTransport {
    /// 记录 `endpoint` 字段对应的值。
    endpoint: String,
    /// 记录 `headers` 字段对应的值。
    headers: BTreeMap<String, String>,
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
            McpServerTransport::Http => Err(EvaError::unsupported(
                "use MCP HTTP JSON-RPC transport for HTTP server_transport",
            )
            .with_context("adapter_id", session_config.adapter_id.as_str())
            .with_context("server_transport", session_config.server_transport.as_str())),
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
        let parsed = ParsedHttpUrl::parse(&endpoint)?;
        if parsed.scheme != "http" {
            return Err(EvaError::unsupported(
                "MCP HTTP transport requires an http:// endpoint in this runtime",
            )
            .with_context("endpoint", &endpoint)
            .with_context("scheme", parsed.scheme));
        }
        for (name, value) in &headers {
            validate_http_header(name, value)?;
        }
        let mut audit = vec![
            "mcp.http:client".to_owned(),
            format!("mcp.http.origin:{}", parsed.origin),
            "mcp.http.method:POST".to_owned(),
        ];
        for name in headers.keys() {
            audit.push(format!("mcp.http.header:{name}:redacted"));
        }
        Ok(Self {
            endpoint,
            headers,
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
            &self.endpoint,
            &self.headers,
            request,
            timeout,
            output_limit_bytes,
        )?;
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
        let text = String::from_utf8(response.body).map_err(|error| {
            EvaError::unavailable("MCP HTTP response was not UTF-8")
                .with_provider_code("mcp_protocol_error")
                .with_context("utf8_error", error.to_string())
        })?;
        enforce_response_limit(&text, output_limit_bytes)?;
        Ok(text)
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
            &self.endpoint,
            &self.headers,
            notification,
            timeout,
            output_limit_bytes,
        )?;
        if !(200..300).contains(&response.status_code) {
            return Err(
                EvaError::unavailable("MCP HTTP notification returned non-success status")
                    .with_provider_code("mcp_http_status")
                    .with_context("status_code", response.status_code.to_string()),
            );
        }
        Ok(())
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
    /// 记录 `body` 字段对应的值。
    body: Vec<u8>,
    /// 记录 `body_truncated` 字段对应的值。
    body_truncated: bool,
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
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| EvaError::invalid_argument("MCP HTTP URL must include a scheme"))?;
        if !matches!(scheme, "http" | "https") {
            return Err(
                EvaError::invalid_argument("MCP HTTP URL scheme is unsupported")
                    .with_context("url", url),
            );
        }
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
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) {
            let port = port.parse::<u16>().map_err(|error| {
                EvaError::invalid_argument("MCP HTTP URL port is invalid")
                    .with_context("port", port)
                    .with_context("parse_error", error.to_string())
            })?;
            return Ok((host.to_owned(), port));
        }
    }
    Ok((authority.to_owned(), default_port))
}

/// 执行 `send_http_json_rpc_request` 对应的处理逻辑。
fn send_http_json_rpc_request(
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    body: &str,
    timeout: Duration,
    output_limit_bytes: usize,
) -> Result<HttpJsonRpcResponse, EvaError> {
    let parsed = ParsedHttpUrl::parse(endpoint)?;
    if parsed.scheme != "http" {
        return Err(EvaError::unsupported(
            "MCP HTTP JSON-RPC execution requires a TLS client for https endpoints",
        )
        .with_context("endpoint", endpoint)
        .with_context("scheme", parsed.scheme));
    }
    let mut addrs = (parsed.host.as_str(), parsed.port)
        .to_socket_addrs()
        .map_err(|error| {
            EvaError::unavailable("failed to resolve MCP HTTP server")
                .with_context("host", &parsed.host)
                .with_context("io_error", error.to_string())
        })?;
    let addr = addrs.next().ok_or_else(|| {
        EvaError::unavailable("MCP HTTP server host did not resolve")
            .with_context("host", &parsed.host)
    })?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|error| {
        EvaError::unavailable("failed to connect MCP HTTP server")
            .with_context("origin", &parsed.origin)
            .with_context("io_error", error.to_string())
    })?;
    if !timeout.is_zero() {
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|_| stream.set_write_timeout(Some(timeout)))
            .map_err(|error| {
                EvaError::unavailable("failed to configure MCP HTTP timeout")
                    .with_context("origin", &parsed.origin)
                    .with_context("io_error", error.to_string())
            })?;
    }

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
    stream.write_all(request.as_bytes()).map_err(|error| {
        EvaError::unavailable("failed to write MCP HTTP request")
            .with_context("origin", &parsed.origin)
            .with_context("io_error", error.to_string())
    })?;
    stream.write_all(body.as_bytes()).map_err(|error| {
        EvaError::unavailable("failed to write MCP HTTP body")
            .with_context("origin", &parsed.origin)
            .with_context("io_error", error.to_string())
    })?;

    read_http_json_rpc_response(&mut stream, &parsed.origin, output_limit_bytes)
}

/// 定义 `HTTP_HEADER_LIMIT_BYTES` 常量。
const HTTP_HEADER_LIMIT_BYTES: usize = 64 * 1024;

/// 读取或解析 `read_http_json_rpc_response` 所需的数据，失败时保留错误语义。
fn read_http_json_rpc_response(
    stream: &mut TcpStream,
    origin: &str,
    output_limit_bytes: usize,
) -> Result<HttpJsonRpcResponse, EvaError> {
    if output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "MCP HTTP response output limit must be greater than zero",
        ));
    }
    let mut header_bytes = Vec::new();
    let mut status_code = None;
    let mut body = Vec::new();
    let mut body_truncated = false;
    let mut buffer = [0_u8; 8192];

    loop {
        let read = stream.read(&mut buffer).map_err(|error| {
            EvaError::unavailable("failed to read MCP HTTP response")
                .with_context("origin", origin)
                .with_context("io_error", error.to_string())
        })?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        if status_code.is_none() {
            header_bytes.extend_from_slice(chunk);
            if header_bytes.len() > HTTP_HEADER_LIMIT_BYTES {
                return Err(
                    EvaError::conflict("MCP HTTP response headers exceeded limit")
                        .with_context("origin", origin)
                        .with_context("header_limit_bytes", HTTP_HEADER_LIMIT_BYTES.to_string()),
                );
            }
            if let Some(header_end) = http_header_end(&header_bytes) {
                let head = String::from_utf8_lossy(&header_bytes[..header_end]).into_owned();
                status_code = Some(parse_http_status_code(&head)?);
                let body_start = header_end + 4;
                append_http_body(
                    &header_bytes[body_start..],
                    output_limit_bytes,
                    &mut body,
                    &mut body_truncated,
                );
                header_bytes.clear();
            }
        } else {
            append_http_body(chunk, output_limit_bytes, &mut body, &mut body_truncated);
        }
        if body_truncated {
            break;
        }
    }

    let status_code = status_code.ok_or_else(|| {
        EvaError::unavailable("MCP HTTP server returned malformed response")
            .with_context("response", "missing header terminator")
    })?;
    Ok(HttpJsonRpcResponse {
        status_code,
        body,
        body_truncated,
    })
}

/// 执行 `append_http_body` 对应的处理逻辑。
fn append_http_body(
    chunk: &[u8],
    output_limit_bytes: usize,
    body: &mut Vec<u8>,
    body_truncated: &mut bool,
) {
    if chunk.is_empty() || *body_truncated {
        return;
    }
    let remaining = output_limit_bytes.saturating_sub(body.len());
    if chunk.len() > remaining {
        body.extend_from_slice(&chunk[..remaining]);
        *body_truncated = true;
    } else {
        body.extend_from_slice(chunk);
    }
}

/// 执行 `http_header_end` 对应的处理逻辑。
fn http_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// 读取或解析 `parse_http_status_code` 所需的数据，失败时保留错误语义。
fn parse_http_status_code(head: &str) -> Result<u16, EvaError> {
    let status_line = head.lines().next().ok_or_else(|| {
        EvaError::unavailable("MCP HTTP server returned malformed response")
            .with_context("response", "missing status line")
    })?;
    status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| {
            EvaError::unavailable("MCP HTTP server returned malformed response")
                .with_context("response", "missing status code")
        })?
        .parse::<u16>()
        .map_err(|error| {
            EvaError::unavailable("MCP HTTP server returned invalid status code")
                .with_context("parse_error", error.to_string())
        })
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
    if value.contains('\r') || value.contains('\n') {
        return Err(
            EvaError::invalid_argument("MCP HTTP header value must not contain newlines")
                .with_context("header", name),
        );
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
    use eva_core::ErrorKind;
    use std::collections::VecDeque;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::channel;
    use std::thread;

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

    /// 读取或解析 `read_test_http_request` 所需的数据，失败时保留错误语义。
    fn read_test_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
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
}
