//! 实现 MCP 的 JSON-RPC 客户端以及 stdio/HTTP 传输边界。
//!
//! 每次调用先校验工具允许列表，再完成初始化握手并发送工具请求。响应必须命中预期 id、
//! 符合协议结构且不超过大小限制；stdio 的读取线程只传递有界行，超时、EOF、读取错误或
//! 协议错配都会关闭子进程并映射为稳定错误，避免遗留失控会话。
//! MCP JSON-RPC client transport.

use crate::http_transport::{
    map_http_read_error, map_http_write_error, McpHttpConnector, McpHttpStream, McpTlsMaterial,
};
use crate::policy::McpAllowlist;
use crate::session::{McpEndpoint, McpServerTransport, McpSessionConfig, McpStreamableHttpConfig};
use crate::sse::{McpSseAbortHandle, McpSseEventStream, McpSseSource};
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
pub(crate) const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";
/// 定义 `DEFAULT_REQUEST_TIMEOUT_MS` 常量。
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
/// 定义 `DEFAULT_OUTPUT_LIMIT_BYTES` 常量。
const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
/// Bound recursive tokenization before untrusted JSON can exhaust the thread stack.
const MAX_JSON_NESTING_DEPTH: usize = 64;
/// Bound opaque MCP session identifiers retained from response headers.
const MCP_SESSION_ID_LIMIT_BYTES: usize = 4 * 1024;
/// Bound negotiated MCP protocol version values.
const MCP_PROTOCOL_VERSION_LIMIT_BYTES: usize = 128;

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

/// Direct JSON-RPC ID used to correlate messages carried by SSE.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpJsonRpcMessageId {
    /// A non-negative integer ID used by Eva's client requests.
    Number(u64),
    /// An opaque string ID used by a peer request.
    String(String),
}

impl fmt::Debug for McpJsonRpcMessageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Number(_) => "Number(<redacted>)",
            Self::String(_) => "String(<redacted>)",
        })
    }
}

impl From<u64> for McpJsonRpcMessageId {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

impl From<String> for McpJsonRpcMessageId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for McpJsonRpcMessageId {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

/// JSON-RPC envelope role carried by one SSE event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpJsonRpcMessageKind {
    /// A peer notification without an ID.
    Notification,
    /// A peer request that requires a response.
    Request,
    /// A result or error response correlated to a prior request.
    Response,
}

/// Direct fields extracted from one SSE JSON-RPC envelope.
pub(crate) struct ParsedSseJsonRpcEnvelope {
    pub(crate) request_id: Option<McpJsonRpcMessageId>,
    pub(crate) kind: McpJsonRpcMessageKind,
}

/// Classify one complete SSE payload from direct JSON-RPC envelope fields.
/// Nested `id`, `method`, `result`, or `error` fields never participate.
pub(crate) fn parse_sse_json_rpc_envelope(
    message: &str,
) -> Result<ParsedSseJsonRpcEnvelope, EvaError> {
    let fields = parse_json_object_fields(message)?;
    let version = fields
        .get("jsonrpc")
        .map(|value| parse_json_string(value))
        .transpose()?;
    if version.as_deref() != Some("2.0") {
        return Err(protocol_error(
            "MCP SSE JSON-RPC message has invalid version",
        ));
    }

    let request_id = fields
        .get("id")
        .map(|value| parse_sse_json_rpc_id(value))
        .transpose()?;
    let method = fields
        .get("method")
        .map(|value| parse_json_string(value))
        .transpose()?;
    if method.as_ref().is_some_and(String::is_empty) {
        return Err(protocol_error("MCP SSE JSON-RPC method is empty"));
    }
    let has_result = fields.contains_key("result");
    let has_error = fields.contains_key("error");

    let kind = match (
        request_id.is_some(),
        method.is_some(),
        has_result,
        has_error,
    ) {
        (false, true, false, false) => McpJsonRpcMessageKind::Notification,
        (true, true, false, false) => McpJsonRpcMessageKind::Request,
        (true, false, true, false) | (true, false, false, true) => McpJsonRpcMessageKind::Response,
        _ => {
            return Err(protocol_error(
                "MCP SSE JSON-RPC envelope has conflicting message fields",
            ));
        }
    };
    Ok(ParsedSseJsonRpcEnvelope { request_id, kind })
}

fn parse_sse_json_rpc_id(value: &str) -> Result<McpJsonRpcMessageId, EvaError> {
    if value.starts_with('"') {
        return parse_json_string(value).map(McpJsonRpcMessageId::String);
    }
    value
        .parse::<u64>()
        .map(McpJsonRpcMessageId::Number)
        .map_err(|_| protocol_error("MCP SSE JSON-RPC id must be a string or non-negative integer"))
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
    /// Session identifier returned by the initialize response.
    session_id: Option<String>,
    /// Protocol version selected by the initialize response.
    protocol_version: Option<String>,
    /// Whether application I/O has been closed locally by shutdown or poison.
    session_closed: bool,
    /// Whether `notifications/initialized` completed successfully.
    initialized_notification_sent: bool,
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
            .field("session_present", &self.session_id.is_some())
            .field("protocol_version_present", &self.protocol_version.is_some())
            .field("session_closed", &self.session_closed)
            .field(
                "initialized_notification_sent",
                &self.initialized_notification_sent,
            )
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
        let call = self.call_tool_with_transport(&mut transport, request_id, tool, input);
        self.finish_http_call(&mut transport, call)
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
        let call = self.call_tool_with_transport(&mut transport, request_id, tool, input);
        self.finish_http_call(&mut transport, call)
    }

    fn finish_http_call(
        &self,
        transport: &mut McpHttpJsonRpcTransport,
        call: Result<McpJsonRpcCallReport, EvaError>,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        let had_session = transport.session_id().is_some();
        let shutdown = transport.shutdown_session(
            Duration::from_millis(self.config.request_timeout_ms),
            self.config.output_limit_bytes,
        );
        match call {
            Ok(mut report) => {
                shutdown?;
                if had_session {
                    report.audit.push(transport.session_shutdown_audit());
                }
                Ok(report)
            }
            Err(error) => match shutdown {
                Ok(()) => Err(error),
                Err(shutdown_error) => Err(error
                    .with_context("mcp_session_cleanup", "failed")
                    .with_context(
                        "mcp_session_cleanup_code",
                        shutdown_error
                            .provider_code()
                            .map(|code| code.as_str())
                            .unwrap_or_else(|| shutdown_error.kind().as_str()),
                    )),
            },
        }
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
                    let envelope = text.trim();
                    if !envelope.starts_with('{') {
                        continue;
                    }
                    match json_u64_field(envelope, "id")? {
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
            session_id: None,
            protocol_version: None,
            session_closed: false,
            initialized_notification_sent: false,
        })
    }

    /// Return the opaque server session identifier without exposing its value
    /// in debug or audit output.
    pub fn session_id(&self) -> Option<&str> {
        (!self.session_closed && self.exchange_count > 0)
            .then_some(self.session_id.as_deref())
            .flatten()
    }

    /// Return the protocol version selected during initialization.
    pub fn negotiated_protocol_version(&self) -> Option<&str> {
        (!self.session_closed && self.exchange_count > 0)
            .then_some(self.protocol_version.as_deref())
            .flatten()
    }

    /// Return whether the initialize notification completed and requests may
    /// enter the operating phase.
    pub fn is_ready(&self) -> bool {
        !self.session_closed && self.initialized_notification_sent
    }

    /// Return whether this local session can no longer issue application I/O.
    pub fn is_closed(&self) -> bool {
        self.session_closed
    }

    /// Perform a session-bound GET request. SSE decoding remains owned by
    /// W4-L05; this method enforces the ready phase, status/media type, bounded
    /// control body, and session/protocol headers.
    pub fn get(
        &mut self,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<Vec<u8>, EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP HTTP response output limit must be greater than zero",
            ));
        }
        self.ensure_session_ready()?;
        let extra_headers = self.session_headers()?;
        let response = send_http_request(
            &self.connector,
            &self.endpoint,
            &self.headers,
            HttpRequestSpec {
                extra_headers: &extra_headers,
                method: "GET",
                accept: "text/event-stream",
                body: "",
                timeout,
                output_limit_bytes,
                allow_bodyless_accepted: false,
            },
        )?;
        self.reject_unknown_session(&response)?;
        self.validate_response_session(&response)?;
        let body = decode_http_event_stream_response(response, output_limit_bytes)?;
        self.audit.push("mcp.http:session_get".to_owned());
        Ok(body)
    }

    /// Open the session-bound SSE data plane without buffering the response
    /// body or waiting for the server to close the connection.
    pub fn open_event_stream(
        &mut self,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<McpSseEventStream, EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP HTTP response output limit must be greater than zero",
            ));
        }
        self.ensure_session_ready()?;
        let extra_headers = self.session_headers()?;
        let response = open_http_response(
            &self.connector,
            &self.endpoint,
            &self.headers,
            HttpRequestSpec {
                extra_headers: &extra_headers,
                method: "GET",
                accept: "text/event-stream",
                body: "",
                timeout,
                output_limit_bytes,
                allow_bodyless_accepted: false,
            },
        )?;
        let metadata = response.metadata();
        self.reject_unknown_session(&metadata)?;
        self.validate_response_session(&metadata)?;
        validate_http_event_stream_metadata(&metadata)?;
        let stream = response.into_persistent_event_stream(output_limit_bytes)?;
        self.audit.push("mcp.http:session_stream_opened".to_owned());
        Ok(stream)
    }

    /// Send one request whose response is explicitly consumed as SSE. The
    /// caller retains the stream so interleaved peer requests and
    /// notifications remain observable after the matching response.
    pub fn post_event_stream(
        &mut self,
        expected_id: u64,
        request: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<McpSseEventStream, EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP HTTP response output limit must be greater than zero",
            ));
        }
        self.ensure_session_ready()?;
        let envelope = parse_sse_json_rpc_envelope(request)?;
        if envelope.kind != McpJsonRpcMessageKind::Request
            || envelope.request_id != Some(McpJsonRpcMessageId::Number(expected_id))
        {
            return Err(protocol_error(
                "MCP HTTP streaming POST request id does not match its envelope",
            ));
        }
        if json_string_field(request, "method")?.as_deref() == Some("initialize") {
            return Err(
                EvaError::conflict("MCP HTTP session is already initialized")
                    .with_provider_code("mcp_http_session_already_initialized"),
            );
        }
        let extra_headers = self.session_headers()?;
        let response = open_http_response(
            &self.connector,
            &self.endpoint,
            &self.headers,
            HttpRequestSpec {
                extra_headers: &extra_headers,
                method: "POST",
                accept: "application/json, text/event-stream",
                body: request,
                timeout,
                output_limit_bytes,
                allow_bodyless_accepted: false,
            },
        )?;
        let metadata = response.metadata();
        self.reject_unknown_session(&metadata)?;
        self.validate_response_session(&metadata)?;
        if !(200..300).contains(&metadata.status_code) {
            return Err(
                EvaError::unavailable("MCP HTTP server returned non-success status")
                    .with_provider_code("mcp_http_status")
                    .with_context("json_rpc_id", expected_id.to_string())
                    .with_context("status_code", metadata.status_code.to_string()),
            );
        }
        require_event_stream_content_type(metadata.content_type.as_deref())?;
        let stream = response.into_persistent_event_stream(output_limit_bytes)?;
        self.exchange_count = self.exchange_count.saturating_add(1);
        self.audit
            .push("mcp.http:session_post_stream_opened".to_owned());
        Ok(stream)
    }

    /// Gracefully close a server-owned MCP session with DELETE.
    ///
    /// Stateless servers do not return `Mcp-Session-Id`; in that case no
    /// DELETE is sent, but the local transport still closes. A session that
    /// was established is never silently discarded. A 405 means the server
    /// does not support client-initiated termination and closes local state;
    /// a 404 invalidates local state and returns a stable unknown-session
    /// error. Other failures retain cleanup material for an explicit retry.
    pub fn shutdown_session(
        &mut self,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<(), EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP HTTP response output limit must be greater than zero",
            ));
        }
        if self.session_id.is_none() {
            self.close_and_clear_session();
            return Ok(());
        }
        let extra_headers = self.cleanup_headers()?;
        let response = send_http_request(
            &self.connector,
            &self.endpoint,
            &self.headers,
            HttpRequestSpec {
                extra_headers: &extra_headers,
                method: "DELETE",
                accept: "application/json",
                body: "",
                timeout,
                output_limit_bytes,
                allow_bodyless_accepted: true,
            },
        )?;
        self.reject_unknown_session(&response)?;
        self.validate_response_session(&response)?;
        if response.status_code == 405 {
            self.close_and_clear_session();
            self.audit
                .push("mcp.http:session_delete_unsupported".to_owned());
            return Ok(());
        }
        if !(200..300).contains(&response.status_code) {
            return Err(EvaError::unavailable(
                "MCP HTTP session DELETE returned non-success status",
            )
            .with_provider_code("mcp_http_session_delete_failed")
            .with_context("status_code", response.status_code.to_string()));
        }
        if response.body_truncated {
            return Err(EvaError::conflict(
                "MCP HTTP session DELETE response exceeded output limit",
            )
            .with_provider_code("mcp_response_too_large")
            .with_context("output_limit_bytes", output_limit_bytes.to_string()));
        }
        self.close_and_clear_session();
        self.audit.push("mcp.http:session_deleted".to_owned());
        Ok(())
    }

    fn ensure_session_open(&self) -> Result<(), EvaError> {
        if self.session_closed {
            return Err(EvaError::conflict("MCP HTTP session is already closed")
                .with_provider_code("mcp_http_session_closed"));
        }
        if self.exchange_count == 0 || self.protocol_version.is_none() {
            return Err(EvaError::conflict(
                "MCP HTTP session requires a successful initialize exchange",
            )
            .with_provider_code("mcp_http_session_not_initialized"));
        }
        Ok(())
    }

    fn ensure_session_ready(&self) -> Result<(), EvaError> {
        self.ensure_session_open()?;
        if !self.initialized_notification_sent {
            return Err(EvaError::conflict(
                "MCP HTTP session requires notifications/initialized before application I/O",
            )
            .with_provider_code("mcp_http_session_not_ready"));
        }
        Ok(())
    }

    fn ensure_not_closed(&self) -> Result<(), EvaError> {
        if self.session_closed {
            return Err(EvaError::conflict("MCP HTTP session is already closed")
                .with_provider_code("mcp_http_session_closed"));
        }
        Ok(())
    }

    fn session_shutdown_audit(&self) -> String {
        self.audit
            .iter()
            .rev()
            .find(|entry| {
                matches!(
                    entry.as_str(),
                    "mcp.http:session_deleted" | "mcp.http:session_delete_unsupported"
                )
            })
            .cloned()
            .unwrap_or_else(|| "mcp.http:session_closed".to_owned())
    }

    fn session_headers(&self) -> Result<Vec<(String, String)>, EvaError> {
        self.ensure_session_open()?;
        self.cleanup_headers()
    }

    fn cleanup_headers(&self) -> Result<Vec<(String, String)>, EvaError> {
        let protocol_version = self.protocol_version.as_ref().ok_or_else(|| {
            EvaError::conflict("MCP HTTP protocol version was not negotiated")
                .with_provider_code("mcp_protocol_version_missing")
        })?;
        let mut headers = vec![("MCP-Protocol-Version".to_owned(), protocol_version.clone())];
        if let Some(session_id) = &self.session_id {
            headers.push(("Mcp-Session-Id".to_owned(), session_id.clone()));
        }
        Ok(headers)
    }

    fn validate_response_session(
        &mut self,
        response: &HttpJsonRpcResponse,
    ) -> Result<(), EvaError> {
        let error = match (&self.session_id, response.session_id.as_deref()) {
            (Some(expected), Some(actual)) if expected != actual => Some(
                EvaError::permission_denied("MCP HTTP response carried an unexpected session")
                    .with_provider_code("mcp_http_session_id_mismatch"),
            ),
            (None, Some(_)) => Some(
                EvaError::permission_denied("MCP HTTP response introduced an unknown session")
                    .with_provider_code("mcp_http_session_id_unexpected"),
            ),
            _ => None,
        };
        if let Some(error) = error {
            // Keep the trusted original ID/version for an explicit DELETE,
            // while preventing any further application I/O.
            self.session_closed = true;
            return Err(error);
        }
        Ok(())
    }

    fn reject_unknown_session(&mut self, response: &HttpJsonRpcResponse) -> Result<(), EvaError> {
        if response.status_code != 404 {
            return Ok(());
        }
        self.close_and_clear_session();
        Err(EvaError::not_found("MCP HTTP session is no longer known")
            .with_provider_code("mcp_http_session_not_found"))
    }

    fn close_and_clear_session(&mut self) {
        self.session_closed = true;
        self.session_id = None;
        self.protocol_version = None;
        self.initialized_notification_sent = false;
    }

    fn remember_initialize(
        &mut self,
        response: HttpExchangeResponse,
        expected_id: u64,
        output_limit_bytes: usize,
        requested_protocol: &str,
    ) -> Result<String, EvaError> {
        // Preserve provisional cleanup material before validating the body.
        // It remains hidden from public accessors until initialization commits.
        self.session_id = response.metadata().session_id.clone();
        if self.session_id.is_some() {
            self.protocol_version = Some(requested_protocol.to_owned());
        }
        let initialized = (|| {
            let text = decode_http_exchange_payload(response, expected_id, output_limit_bytes)?;
            let parsed = parse_json_rpc_response(&text, expected_id)?;
            let negotiated = json_string_field(&parsed.result, "protocolVersion")?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    protocol_error("MCP initialize response is missing protocolVersion")
                        .with_provider_code("mcp_protocol_version_missing")
                })?;
            validate_mcp_protocol_version(&negotiated).map_err(|_| {
                protocol_error("MCP initialize response has an invalid protocolVersion")
                    .with_provider_code("mcp_protocol_version_invalid")
            })?;
            if negotiated != requested_protocol {
                return Err(EvaError::unsupported(
                    "MCP server negotiated an unsupported protocol version",
                )
                .with_provider_code("mcp_protocol_version_mismatch"));
            }
            Ok((text, negotiated))
        })();
        let (text, negotiated) = match initialized {
            Ok(initialized) => initialized,
            Err(error) => {
                self.session_closed = true;
                return Err(error);
            }
        };
        self.protocol_version = Some(negotiated);
        self.audit.push("mcp.http:session_initialized".to_owned());
        if self.session_id.is_some() {
            self.audit.push("mcp.http:session_id_received".to_owned());
        }
        Ok(text)
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
        self.ensure_not_closed()?;
        let initialize = self.exchange_count == 0;
        let method = json_string_field(request, "method")?.ok_or_else(|| {
            protocol_error("MCP HTTP JSON-RPC request is missing a top-level method")
        })?;
        let requested_protocol = if initialize {
            if method != "initialize" {
                return Err(
                    protocol_error("MCP HTTP session must begin with initialize")
                        .with_provider_code("mcp_http_session_not_initialized"),
                );
            }
            let params = json_field_value(request, "params")?.ok_or_else(|| {
                protocol_error("MCP initialize request is missing params")
                    .with_provider_code("mcp_protocol_version_missing")
            })?;
            let value = json_string_field(params, "protocolVersion")?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    protocol_error("MCP initialize request is missing protocolVersion")
                        .with_provider_code("mcp_protocol_version_missing")
                })?;
            validate_mcp_protocol_version(&value).map_err(|_| {
                protocol_error("MCP initialize request has an invalid protocolVersion")
                    .with_provider_code("mcp_protocol_version_invalid")
            })?;
            value
        } else {
            if method == "initialize" {
                return Err(
                    EvaError::conflict("MCP HTTP session is already initialized")
                        .with_provider_code("mcp_http_session_already_initialized"),
                );
            }
            self.ensure_session_ready()?;
            String::new()
        };
        let extra_headers = if initialize {
            Vec::new()
        } else {
            self.session_headers()?
        };
        let response = send_http_exchange_request(
            &self.connector,
            &self.endpoint,
            &self.headers,
            HttpRequestSpec {
                extra_headers: &extra_headers,
                method: "POST",
                accept: "application/json, text/event-stream",
                body: request,
                timeout,
                output_limit_bytes,
                allow_bodyless_accepted: false,
            },
        )?;
        let text = if initialize {
            match self.remember_initialize(
                response,
                expected_id,
                output_limit_bytes,
                &requested_protocol,
            ) {
                Ok(text) => text,
                Err(error) => {
                    let cleanup = self.shutdown_session(timeout, output_limit_bytes);
                    return Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup_error) => error
                            .with_context("mcp_session_cleanup", "failed")
                            .with_context(
                                "mcp_session_cleanup_code",
                                cleanup_error
                                    .provider_code()
                                    .map(|code| code.as_str())
                                    .unwrap_or_else(|| cleanup_error.kind().as_str()),
                            ),
                    });
                }
            }
        } else {
            self.reject_unknown_session(response.metadata())?;
            self.validate_response_session(response.metadata())?;
            decode_http_exchange_payload(response, expected_id, output_limit_bytes)?
        };
        self.exchange_count = self.exchange_count.saturating_add(1);
        Ok(text)
    }

    /// 执行 `notify` 对应的处理逻辑。
    fn notify(
        &mut self,
        notification: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<(), EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP HTTP response output limit must be greater than zero",
            ));
        }
        self.ensure_not_closed()?;
        let method = json_string_field(notification, "method")?.ok_or_else(|| {
            protocol_error("MCP HTTP JSON-RPC notification is missing a top-level method")
        })?;
        if self.exchange_count == 0 {
            return Err(EvaError::conflict(
                "MCP initialized notification requires a successful initialize exchange",
            )
            .with_provider_code("mcp_http_session_not_initialized"));
        }
        let initialized = method == "notifications/initialized";
        if !self.initialized_notification_sent && !initialized {
            return Err(EvaError::conflict(
                "MCP HTTP session requires notifications/initialized as its first notification",
            )
            .with_provider_code("mcp_http_initialized_notification_required"));
        }
        if self.initialized_notification_sent && initialized {
            return Err(EvaError::conflict(
                "MCP HTTP notifications/initialized was already accepted",
            )
            .with_provider_code("mcp_http_initialized_notification_duplicate"));
        }
        let extra_headers = self.session_headers()?;
        let response = send_http_request(
            &self.connector,
            &self.endpoint,
            &self.headers,
            HttpRequestSpec {
                extra_headers: &extra_headers,
                method: "POST",
                accept: "application/json, text/event-stream",
                body: notification,
                timeout,
                output_limit_bytes,
                allow_bodyless_accepted: true,
            },
        )?;
        self.reject_unknown_session(&response)?;
        self.validate_response_session(&response)?;
        validate_http_notification_response(response, output_limit_bytes)?;
        self.notification_count = self.notification_count.saturating_add(1);
        if initialized {
            self.initialized_notification_sent = true;
            self.audit
                .push("mcp.http:initialized_notification_accepted".to_owned());
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
        audit.push(format!(
            "mcp.http.session_present:{}",
            self.session_id().is_some()
        ));
        audit.push(format!(
            "mcp.http.protocol_version_present:{}",
            self.negotiated_protocol_version().is_some()
        ));
        audit.push(format!("mcp.http.session_ready:{}", self.is_ready()));
        audit.push(format!("mcp.http.session_closed:{}", self.is_closed()));
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
    /// Opaque MCP application-session identifier selected by the server.
    session_id: Option<String>,
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
    session_id: Option<String>,
}

/// An HTTP response whose body remains attached to the live connection.
struct HttpOpenResponse {
    head: HttpResponseHead,
    framing: HttpBodyFraming,
    reader: BufReader<McpHttpStream>,
    origin: String,
    deadline: Instant,
    idle_timeout: Duration,
}

impl HttpOpenResponse {
    fn metadata(&self) -> HttpJsonRpcResponse {
        HttpJsonRpcResponse {
            status_code: self.head.status_code,
            content_type: self.head.content_type.clone(),
            body: Vec::new(),
            body_truncated: false,
            session_id: self.head.session_id.clone(),
        }
    }

    fn into_buffered(mut self, output_limit_bytes: usize) -> Result<HttpJsonRpcResponse, EvaError> {
        self.ensure_deadline()?;
        let (body, body_truncated) = read_http_body(
            &mut self.reader,
            self.framing,
            output_limit_bytes,
            &self.origin,
        )?;
        self.ensure_deadline()?;
        Ok(HttpJsonRpcResponse {
            status_code: self.head.status_code,
            content_type: self.head.content_type,
            body,
            body_truncated,
            session_id: self.head.session_id,
        })
    }

    fn into_event_stream(self, output_limit_bytes: usize) -> Result<McpSseEventStream, EvaError> {
        let deadline = self.deadline;
        self.into_event_stream_inner(output_limit_bytes, Some(deadline))
    }

    fn into_persistent_event_stream(
        mut self,
        output_limit_bytes: usize,
    ) -> Result<McpSseEventStream, EvaError> {
        self.ensure_deadline()?;
        self.reader
            .get_mut()
            .use_idle_timeout(self.idle_timeout)
            .map_err(|error| {
                EvaError::unavailable("failed to configure MCP HTTP stream idle timeout")
                    .with_provider_code("mcp_http_timeout_config_failed")
                    .with_context("origin", self.origin.clone())
                    .with_context("io_error_kind", format!("{:?}", error.kind()))
            })?;
        self.into_event_stream_inner(output_limit_bytes, None)
    }

    fn into_event_stream_inner(
        self,
        output_limit_bytes: usize,
        deadline: Option<Instant>,
    ) -> Result<McpSseEventStream, EvaError> {
        let abort = self.reader.get_ref().abort_handle()?;
        McpSseEventStream::from_abortable_source(
            Box::new(HttpSseSource::new_with_deadline(
                self.reader,
                self.framing,
                self.origin,
                deadline,
            )),
            McpSseAbortHandle::new(move || abort.abort()),
            output_limit_bytes,
        )
    }

    fn ensure_deadline(&self) -> Result<(), EvaError> {
        self.reader
            .get_ref()
            .ensure_deadline()
            .map_err(|error| map_http_read_error(error, &self.origin))
    }
}

/// A JSON-RPC HTTP response may be a bounded JSON body or a live SSE stream.
enum HttpExchangeResponse {
    Buffered(HttpJsonRpcResponse),
    EventStream {
        metadata: HttpJsonRpcResponse,
        stream: Box<McpSseEventStream>,
    },
}

impl HttpExchangeResponse {
    fn metadata(&self) -> &HttpJsonRpcResponse {
        match self {
            Self::Buffered(response) => response,
            Self::EventStream { metadata, .. } => metadata,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpChunkReadState {
    Size,
    Data(usize),
    Delimiter,
    Trailers(usize),
    Done,
}

/// Framing-aware pull source for a long-lived HTTP event stream.
struct HttpSseSource<R> {
    reader: BufReader<R>,
    framing: HttpBodyFraming,
    origin: String,
    chunk_state: HttpChunkReadState,
    deadline: Option<Instant>,
}

impl<R> HttpSseSource<R> {
    #[cfg(test)]
    fn new(reader: BufReader<R>, framing: HttpBodyFraming, origin: String) -> Self {
        Self::new_with_deadline(reader, framing, origin, None)
    }

    fn new_with_deadline(
        reader: BufReader<R>,
        framing: HttpBodyFraming,
        origin: String,
        deadline: Option<Instant>,
    ) -> Self {
        Self {
            reader,
            framing,
            origin,
            chunk_state: HttpChunkReadState::Size,
            deadline,
        }
    }
}

impl<R: Read + Send> McpSseSource for HttpSseSource<R> {
    fn read_sse(&mut self, buffer: &mut [u8]) -> Result<usize, EvaError> {
        if buffer.is_empty() {
            return Ok(0);
        }
        ensure_http_read_deadline(self.deadline, &self.origin)?;
        let result = match &mut self.framing {
            HttpBodyFraming::None => Ok(0),
            HttpBodyFraming::ContentLength(remaining) => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let target = buffer.len().min(*remaining);
                let read = self
                    .reader
                    .read(&mut buffer[..target])
                    .map_err(|error| map_http_read_error(error, &self.origin))?;
                if read == 0 {
                    return Err(http_body_incomplete_error(
                        &self.origin,
                        "content-length SSE body ended early",
                    ));
                }
                *remaining -= read;
                Ok(read)
            }
            HttpBodyFraming::Chunked => self.read_chunked(buffer),
            HttpBodyFraming::CloseDelimited => self
                .reader
                .read(buffer)
                .map_err(|error| map_http_read_error(error, &self.origin)),
        }?;
        ensure_http_read_deadline(self.deadline, &self.origin)?;
        Ok(result)
    }
}

fn ensure_http_read_deadline(deadline: Option<Instant>, origin: &str) -> Result<(), EvaError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(EvaError::timeout("MCP HTTP response read timed out")
            .with_provider_code("mcp_http_read_timeout")
            .with_context("origin", origin))
    } else {
        Ok(())
    }
}

impl<R: Read> HttpSseSource<R> {
    fn read_chunked(&mut self, buffer: &mut [u8]) -> Result<usize, EvaError> {
        loop {
            match self.chunk_state {
                HttpChunkReadState::Size => {
                    // Size-line bounds reset per chunk so a healthy long-lived
                    // stream is not rejected for cumulative metadata volume.
                    let mut metadata_bytes = 0_usize;
                    let line = read_http_line(
                        &mut self.reader,
                        &mut metadata_bytes,
                        &self.origin,
                        HTTP_HEADER_LIMIT_BYTES,
                    )?;
                    let size = parse_http_chunk_size(&line, &self.origin)?;
                    self.chunk_state = if size == 0 {
                        HttpChunkReadState::Trailers(0)
                    } else {
                        HttpChunkReadState::Data(size)
                    };
                }
                HttpChunkReadState::Data(remaining) => {
                    let target = buffer.len().min(remaining);
                    let read = self
                        .reader
                        .read(&mut buffer[..target])
                        .map_err(|error| map_http_read_error(error, &self.origin))?;
                    if read == 0 {
                        return Err(http_body_incomplete_error(
                            &self.origin,
                            "chunked SSE body ended early",
                        ));
                    }
                    self.chunk_state = if read == remaining {
                        HttpChunkReadState::Delimiter
                    } else {
                        HttpChunkReadState::Data(remaining - read)
                    };
                    return Ok(read);
                }
                HttpChunkReadState::Delimiter => {
                    let mut delimiter = [0_u8; 2];
                    read_http_exact(&mut self.reader, &mut delimiter, &self.origin)?;
                    if delimiter != *b"\r\n" {
                        return Err(http_framing_error(
                            &self.origin,
                            "chunk data is not CRLF terminated",
                        ));
                    }
                    self.chunk_state = HttpChunkReadState::Size;
                }
                HttpChunkReadState::Trailers(mut metadata_bytes) => {
                    let trailer = read_http_line(
                        &mut self.reader,
                        &mut metadata_bytes,
                        &self.origin,
                        HTTP_HEADER_LIMIT_BYTES,
                    )?;
                    if trailer.is_empty() {
                        self.chunk_state = HttpChunkReadState::Done;
                        return Ok(0);
                    }
                    validate_http_trailer(&trailer, &self.origin)?;
                    self.chunk_state = HttpChunkReadState::Trailers(metadata_bytes);
                }
                HttpChunkReadState::Done => return Ok(0),
            }
        }
    }
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

/// Parameters for one bounded HTTP request.
struct HttpRequestSpec<'a> {
    extra_headers: &'a [(String, String)],
    method: &'a str,
    accept: &'a str,
    body: &'a str,
    timeout: Duration,
    output_limit_bytes: usize,
    allow_bodyless_accepted: bool,
}

/// Send one bounded HTTP request with transport-controlled method and headers.
fn send_http_request(
    connector: &McpHttpConnector,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    spec: HttpRequestSpec<'_>,
) -> Result<HttpJsonRpcResponse, EvaError> {
    let output_limit_bytes = spec.output_limit_bytes;
    open_http_response(connector, endpoint, headers, spec)?.into_buffered(output_limit_bytes)
}

/// Send a JSON-RPC request and preserve a successful SSE body as a pull
/// stream instead of waiting for EOF.
fn send_http_exchange_request(
    connector: &McpHttpConnector,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    spec: HttpRequestSpec<'_>,
) -> Result<HttpExchangeResponse, EvaError> {
    let output_limit_bytes = spec.output_limit_bytes;
    let response = open_http_response(connector, endpoint, headers, spec)?;
    let metadata = response.metadata();
    if metadata.content_type.as_deref() == Some("text/event-stream") {
        let stream = response.into_event_stream(output_limit_bytes)?;
        Ok(HttpExchangeResponse::EventStream {
            metadata,
            stream: Box::new(stream),
        })
    } else {
        response
            .into_buffered(output_limit_bytes)
            .map(HttpExchangeResponse::Buffered)
    }
}

/// Write one request and return immediately after strict response-head
/// parsing, retaining any already-buffered body bytes with the connection.
fn open_http_response(
    connector: &McpHttpConnector,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    spec: HttpRequestSpec<'_>,
) -> Result<HttpOpenResponse, EvaError> {
    let HttpRequestSpec {
        extra_headers,
        method,
        accept,
        body,
        timeout,
        output_limit_bytes,
        allow_bodyless_accepted,
    } = spec;
    let request_started = Instant::now();
    if output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "MCP HTTP response output limit must be greater than zero",
        ));
    }
    let parsed = ParsedHttpUrl::parse(endpoint)?;
    let deadline = request_started.checked_add(timeout).ok_or_else(|| {
        EvaError::invalid_argument("MCP HTTP request timeout is out of range")
            .with_provider_code("mcp_http_timeout_invalid")
            .with_context("origin", parsed.origin.clone())
    })?;

    if method.is_empty()
        || !method
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(EvaError::internal("MCP HTTP method is invalid")
            .with_provider_code("mcp_http_method_invalid"));
    }
    if accept.is_empty() || accept.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(EvaError::internal("MCP HTTP accept value is invalid")
            .with_provider_code("mcp_http_accept_invalid"));
    }

    let mut header_names = BTreeSet::from([
        "host".to_owned(),
        "connection".to_owned(),
        "accept".to_owned(),
    ]);
    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: {accept}\r\n",
        parsed.path, parsed.authority
    );
    if !body.is_empty() {
        header_names.insert("content-type".to_owned());
        header_names.insert("content-length".to_owned());
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        validate_http_header(name, value)?;
        if !header_names.insert(name.to_ascii_lowercase()) {
            return Err(EvaError::invalid_argument(
                "MCP HTTP request header collides with a transport header",
            )
            .with_context("header", name));
        }
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    for (name, value) in extra_headers {
        validate_internal_http_header(name, value)?;
        if !header_names.insert(name.to_ascii_lowercase()) {
            return Err(EvaError::internal(
                "MCP HTTP internal header collides with a transport header",
            )
            .with_provider_code("mcp_http_internal_header_collision"));
        }
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    let mut stream = connector.connect_until(
        &parsed.scheme,
        &parsed.host,
        parsed.port,
        &parsed.origin,
        deadline,
    )?;
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|error| map_http_write_error(error, &parsed.origin))?;
    let mut reader = BufReader::new(stream);
    let (head, framing) =
        read_final_http_response_head(&mut reader, &parsed.origin, allow_bodyless_accepted)?;
    reader
        .get_ref()
        .ensure_deadline()
        .map_err(|error| map_http_read_error(error, &parsed.origin))?;
    Ok(HttpOpenResponse {
        head,
        framing,
        reader,
        origin: parsed.origin,
        deadline,
        idle_timeout: timeout,
    })
}

/// 定义 `HTTP_HEADER_LIMIT_BYTES` 常量。
const HTTP_HEADER_LIMIT_BYTES: usize = 64 * 1024;

/// 读取或解析 `read_http_json_rpc_response` 所需的数据，失败时保留错误语义。
#[cfg(test)]
fn read_http_json_rpc_response(
    stream: &mut impl Read,
    origin: &str,
    output_limit_bytes: usize,
) -> Result<HttpJsonRpcResponse, EvaError> {
    read_http_json_rpc_response_with_options(stream, origin, output_limit_bytes, false)
}

/// Read a response with the caller's notification-specific bodyless status policy.
#[cfg(test)]
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
    let (head, framing) =
        read_final_http_response_head(&mut reader, origin, allow_bodyless_accepted)?;
    let (body, body_truncated) = read_http_body(&mut reader, framing, output_limit_bytes, origin)?;
    Ok(HttpJsonRpcResponse {
        status_code: head.status_code,
        content_type: head.content_type,
        body,
        body_truncated,
        session_id: head.session_id,
    })
}

fn read_final_http_response_head(
    reader: &mut impl BufRead,
    origin: &str,
    allow_bodyless_accepted: bool,
) -> Result<(HttpResponseHead, HttpBodyFraming), EvaError> {
    let mut informational_responses = 0_u8;
    let head = loop {
        let head = read_http_response_head(reader, origin)?;
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
    Ok((head, framing))
}

fn read_http_body(
    reader: &mut impl BufRead,
    framing: HttpBodyFraming,
    output_limit_bytes: usize,
    origin: &str,
) -> Result<(Vec<u8>, bool), EvaError> {
    let body = match framing {
        HttpBodyFraming::None => (Vec::new(), false),
        HttpBodyFraming::ContentLength(length) => {
            if length > output_limit_bytes {
                (Vec::new(), true)
            } else {
                let mut body = vec![0_u8; length];
                read_http_exact(reader, &mut body, origin)?;
                (body, false)
            }
        }
        HttpBodyFraming::Chunked => read_http_chunked_body(reader, output_limit_bytes, origin)?,
        HttpBodyFraming::CloseDelimited => {
            read_http_close_delimited_body(reader, output_limit_bytes, origin)?
        }
    };
    Ok(body)
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
        session_id: None,
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
            "mcp-session-id" => {
                if head.session_id.is_some() {
                    return Err(http_framing_error(origin, "duplicate MCP session id"));
                }
                head.session_id = Some(parse_mcp_session_id(&value, origin)?);
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

/// Parse the bounded opaque value carried by `Mcp-Session-Id`.
fn parse_mcp_session_id(value: &str, origin: &str) -> Result<String, EvaError> {
    if value.is_empty()
        || value.len() > MCP_SESSION_ID_LIMIT_BYTES
        || value.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
    {
        return Err(http_framing_error(origin, "invalid MCP session id"));
    }
    Ok(value.to_owned())
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
        let size = parse_http_chunk_size(&line, origin)?;
        if size == 0 {
            loop {
                let trailer =
                    read_http_line(reader, &mut metadata_bytes, origin, HTTP_HEADER_LIMIT_BYTES)?;
                if trailer.is_empty() {
                    return Ok((body, false));
                }
                validate_http_trailer(&trailer, origin)?;
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

fn parse_http_chunk_size(line: &[u8], origin: &str) -> Result<usize, EvaError> {
    if line.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
        return Err(http_framing_error(origin, "invalid chunk extension"));
    }
    let size_text = line.split(|byte| *byte == b';').next().unwrap_or_default();
    let size_text = std::str::from_utf8(size_text)
        .map_err(|_| http_framing_error(origin, "chunk size is not ASCII"))?;
    if size_text.is_empty() || !size_text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(http_framing_error(origin, "invalid chunk size"));
    }
    u64::from_str_radix(size_text, 16)
        .ok()
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| http_framing_error(origin, "chunk size overflows host size"))
}

fn validate_http_trailer(line: &[u8], origin: &str) -> Result<(), EvaError> {
    let (name, _) = parse_http_header_line(line, origin)?;
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
    Ok(())
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
fn decode_http_exchange_payload(
    response: HttpExchangeResponse,
    expected_id: u64,
    output_limit_bytes: usize,
) -> Result<String, EvaError> {
    match response {
        HttpExchangeResponse::Buffered(response) => {
            decode_http_exchange_response(response, expected_id, output_limit_bytes)
        }
        HttpExchangeResponse::EventStream {
            metadata,
            mut stream,
        } => {
            if !(200..300).contains(&metadata.status_code) {
                return Err(
                    EvaError::unavailable("MCP HTTP server returned non-success status")
                        .with_provider_code("mcp_http_status")
                        .with_context("json_rpc_id", expected_id.to_string())
                        .with_context("status_code", metadata.status_code.to_string()),
                );
            }
            require_event_stream_content_type(metadata.content_type.as_deref())?;
            let message = stream.next_response(expected_id)?;
            enforce_response_limit(&message.data, output_limit_bytes)?;
            Ok(message.data)
        }
    }
}

/// Decode a buffered response that must carry one JSON-RPC object.
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

/// Validate the bounded control-path response used to open an SSE stream.
/// Incremental event decoding replaces this buffered body in W4-L05.
fn decode_http_event_stream_response(
    response: HttpJsonRpcResponse,
    output_limit_bytes: usize,
) -> Result<Vec<u8>, EvaError> {
    validate_http_event_stream_metadata(&response)?;
    if response.body_truncated {
        return Err(
            EvaError::conflict("MCP HTTP GET response exceeded output limit")
                .with_provider_code("mcp_response_too_large")
                .with_context("output_limit_bytes", output_limit_bytes.to_string()),
        );
    }
    Ok(response.body)
}

fn validate_http_event_stream_metadata(response: &HttpJsonRpcResponse) -> Result<(), EvaError> {
    if response.status_code == 405 {
        return Err(EvaError::unsupported(
            "MCP HTTP server does not provide a server event stream",
        )
        .with_provider_code("mcp_http_sse_unsupported"));
    }
    if response.status_code != 200 {
        return Err(
            EvaError::unavailable("MCP HTTP GET did not return an event stream")
                .with_provider_code("mcp_http_status")
                .with_context("status_code", response.status_code.to_string()),
        );
    }
    require_event_stream_content_type(response.content_type.as_deref())?;
    Ok(())
}

/// Validate the bodyless `202 Accepted` required for a notification POST.
fn validate_http_notification_response(
    response: HttpJsonRpcResponse,
    output_limit_bytes: usize,
) -> Result<(), EvaError> {
    if response.status_code != 202 {
        return Err(EvaError::unavailable(
            "MCP HTTP notification was not accepted with status 202",
        )
        .with_provider_code("mcp_http_status")
        .with_context("status_code", response.status_code.to_string()));
    }
    if response.body_truncated {
        return Err(
            EvaError::conflict("MCP HTTP notification response exceeded output limit")
                .with_provider_code("mcp_response_too_large")
                .with_context("output_limit_bytes", output_limit_bytes.to_string()),
        );
    }
    if !response.body.is_empty() {
        return Err(
            protocol_error("MCP HTTP notification response must not contain a body")
                .with_provider_code("mcp_http_notification_body_unexpected"),
        );
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

/// Require the media type used by the Streamable HTTP server event stream.
fn require_event_stream_content_type(content_type: Option<&str>) -> Result<(), EvaError> {
    match content_type {
        Some("text/event-stream") => Ok(()),
        Some(_) => Err(
            EvaError::unsupported("MCP HTTP GET response is not an event stream")
                .with_provider_code("mcp_http_content_type_unsupported"),
        ),
        None => Err(
            EvaError::unavailable("MCP HTTP GET response content type is missing")
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
            | "mcp-session-id"
            | "mcp-protocol-version"
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

/// Validate headers generated by the session state machine itself. These are
/// deliberately kept separate from user-supplied headers, which cannot spoof
/// the two MCP control fields.
fn validate_internal_http_header(name: &str, value: &str) -> Result<(), EvaError> {
    if name.trim().is_empty() || !name.bytes().all(is_http_token_byte) {
        return Err(
            EvaError::internal("MCP HTTP internal header name is invalid")
                .with_provider_code("mcp_http_internal_header_invalid"),
        );
    }
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte >= 0x80)
    {
        return Err(
            EvaError::internal("MCP HTTP internal header value is invalid")
                .with_provider_code("mcp_http_internal_header_invalid"),
        );
    }
    Ok(())
}

fn validate_mcp_protocol_version(value: &str) -> Result<(), EvaError> {
    if value.is_empty()
        || value.len() > MCP_PROTOCOL_VERSION_LIMIT_BYTES
        || value.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
    {
        return Err(EvaError::invalid_argument(
            "MCP protocol version must be bounded visible ASCII",
        )
        .with_provider_code("mcp_protocol_version_invalid"));
    }
    Ok(())
}

/// 校验 `validate_client_config` 对应的约束，不满足时返回明确错误。
fn validate_client_config(config: &McpJsonRpcClientConfig) -> Result<(), EvaError> {
    validate_mcp_protocol_version(&config.protocol_version)?;
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
    if let Some(error) = json_field_value(response, "error")? {
        return Err(map_json_rpc_error(error, expected_id));
    }
    let result = json_field_value(response, "result")?
        .ok_or_else(|| {
            protocol_error("MCP JSON-RPC response is missing result")
                .with_context("json_rpc_id", expected_id.to_string())
        })?
        .to_owned();
    Ok(JsonRpcResponse { result })
}

/// 读取或解析 `parse_tools_list` 所需的数据，失败时保留错误语义。
fn parse_tools_list(response: &JsonRpcResponse) -> Result<Vec<McpJsonRpcTool>, EvaError> {
    let tools = json_field_value(&response.result, "tools")?.ok_or_else(|| {
        protocol_error("MCP tools/list response is missing tools")
            .with_provider_code("mcp_tools_list_missing_tools")
    })?;
    let mut names = Vec::new();
    let mut offset = 0;
    while let Some(position) = tools[offset..].find("\"name\"") {
        let start = offset + position;
        let Some(value) = find_json_field_value(&tools[start..], "name") else {
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

/// Return one direct field from a complete JSON object.
fn json_field_value<'a>(text: &'a str, key: &str) -> Result<Option<&'a str>, EvaError> {
    Ok(parse_json_object_fields(text)?.get(key).copied())
}

/// Parse only the direct members of one complete JSON object. Nested values
/// remain borrowed slices and are parsed explicitly by their caller.
pub(crate) fn parse_json_object_fields(text: &str) -> Result<BTreeMap<String, &str>, EvaError> {
    let bytes = text.as_bytes();
    let mut offset = 0usize;
    skip_json_whitespace(bytes, &mut offset);
    if bytes.get(offset).copied() != Some(b'{') {
        return Err(protocol_error("MCP JSON-RPC value must be an object"));
    }
    offset += 1;
    let mut fields = BTreeMap::new();
    loop {
        skip_json_whitespace(bytes, &mut offset);
        if bytes.get(offset).copied() == Some(b'}') {
            offset += 1;
            skip_json_whitespace(bytes, &mut offset);
            if offset != bytes.len() {
                return Err(protocol_error(
                    "MCP JSON-RPC object has trailing non-whitespace data",
                ));
            }
            return Ok(fields);
        }
        if bytes.get(offset).copied() != Some(b'"') {
            return Err(protocol_error("MCP JSON-RPC object key is invalid"));
        }
        let key_end = json_string_token_end(text, offset)?;
        let key = parse_json_string(&text[offset..key_end])?;
        offset = key_end;
        skip_json_whitespace(bytes, &mut offset);
        if bytes.get(offset).copied() != Some(b':') {
            return Err(protocol_error("MCP JSON-RPC object key is missing a colon"));
        }
        offset += 1;
        skip_json_whitespace(bytes, &mut offset);
        let value_start = offset;
        let value_end = json_value_token_end_at_depth(text, value_start, 1)?;
        if fields.insert(key, &text[value_start..value_end]).is_some() {
            return Err(protocol_error(
                "MCP JSON-RPC object contains a duplicate field",
            ));
        }
        offset = value_end;
        skip_json_whitespace(bytes, &mut offset);
        match bytes.get(offset).copied() {
            Some(b',') => {
                offset += 1;
                skip_json_whitespace(bytes, &mut offset);
                if bytes.get(offset).copied() == Some(b'}') {
                    return Err(protocol_error(
                        "MCP JSON-RPC object contains a trailing comma",
                    ));
                }
            }
            Some(b'}') => {
                offset += 1;
                skip_json_whitespace(bytes, &mut offset);
                if offset != bytes.len() {
                    return Err(protocol_error(
                        "MCP JSON-RPC object has trailing non-whitespace data",
                    ));
                }
                return Ok(fields);
            }
            _ => {
                return Err(protocol_error(
                    "MCP JSON-RPC object fields are not comma-separated",
                ));
            }
        }
    }
}

/// Parse one complete JSON array containing only strings. This stays crate
/// private so protocol surfaces can reuse the same strict tokenization without
/// introducing a second JSON parser.
pub(crate) fn parse_json_string_array(text: &str) -> Result<Vec<String>, EvaError> {
    let bytes = text.as_bytes();
    let mut offset = 0_usize;
    skip_json_whitespace(bytes, &mut offset);
    if bytes.get(offset).copied() != Some(b'[') {
        return Err(protocol_error("MCP JSON string array is invalid"));
    }
    offset += 1;
    skip_json_whitespace(bytes, &mut offset);
    let mut values = Vec::new();
    if bytes.get(offset).copied() == Some(b']') {
        offset += 1;
        skip_json_whitespace(bytes, &mut offset);
        if offset == bytes.len() {
            return Ok(values);
        }
        return Err(protocol_error("MCP JSON string array has trailing data"));
    }
    loop {
        let end = json_string_token_end(text, offset)?;
        values.push(parse_json_string(&text[offset..end])?);
        offset = end;
        skip_json_whitespace(bytes, &mut offset);
        match bytes.get(offset).copied() {
            Some(b',') => {
                offset += 1;
                skip_json_whitespace(bytes, &mut offset);
                if bytes.get(offset).copied() == Some(b']') {
                    return Err(protocol_error(
                        "MCP JSON string array contains a trailing comma",
                    ));
                }
            }
            Some(b']') => {
                offset += 1;
                skip_json_whitespace(bytes, &mut offset);
                if offset == bytes.len() {
                    return Ok(values);
                }
                return Err(protocol_error("MCP JSON string array has trailing data"));
            }
            _ => {
                return Err(protocol_error(
                    "MCP JSON string array values are not comma-separated",
                ));
            }
        }
    }
}

fn skip_json_whitespace(bytes: &[u8], offset: &mut usize) {
    while bytes
        .get(*offset)
        .is_some_and(|byte| matches!(*byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        *offset += 1;
    }
}

fn json_value_token_end(text: &str, start: usize) -> Result<usize, EvaError> {
    json_value_token_end_at_depth(text, start, 0)
}

fn json_value_token_end_at_depth(
    text: &str,
    start: usize,
    depth: usize,
) -> Result<usize, EvaError> {
    let bytes = text.as_bytes();
    match bytes.get(start).copied() {
        Some(b'"') => json_string_token_end(text, start),
        Some(b'{') => {
            let nested_depth = next_json_nesting_depth(depth)?;
            json_object_token_end(text, start, nested_depth)
        }
        Some(b'[') => {
            let nested_depth = next_json_nesting_depth(depth)?;
            json_array_token_end(text, start, nested_depth)
        }
        Some(_) => {
            let mut end = start;
            while bytes.get(end).is_some_and(|byte| {
                !matches!(*byte, b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']')
            }) {
                end += 1;
            }
            let token = &text[start..end];
            if !matches!(token, "true" | "false" | "null") && !is_json_number(token) {
                return Err(protocol_error("MCP JSON-RPC primitive value is invalid"));
            }
            Ok(end)
        }
        None => Err(protocol_error("MCP JSON-RPC object value is missing")),
    }
}

fn next_json_nesting_depth(depth: usize) -> Result<usize, EvaError> {
    let nested_depth = depth
        .checked_add(1)
        .ok_or_else(|| protocol_error("MCP JSON-RPC nesting limit exceeded"))?;
    if nested_depth > MAX_JSON_NESTING_DEPTH {
        return Err(protocol_error("MCP JSON-RPC nesting limit exceeded"));
    }
    Ok(nested_depth)
}

fn json_string_token_end(text: &str, start: usize) -> Result<usize, EvaError> {
    let bytes = text.as_bytes();
    if bytes.get(start).copied() != Some(b'"') {
        return Err(protocol_error("MCP JSON-RPC string field is invalid"));
    }
    let mut offset = start + 1;
    while let Some(byte) = bytes.get(offset).copied() {
        match byte {
            b'"' => return Ok(offset + 1),
            b'\\' => {
                let escaped = bytes
                    .get(offset + 1)
                    .copied()
                    .ok_or_else(|| protocol_error("MCP JSON-RPC string escape is incomplete"))?;
                match escaped {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                        offset += 2;
                    }
                    b'u' => {
                        let hex_end = offset.saturating_add(6);
                        let hex = bytes.get(offset + 2..hex_end).ok_or_else(|| {
                            protocol_error("MCP JSON-RPC unicode escape is incomplete")
                        })?;
                        if !hex.iter().all(u8::is_ascii_hexdigit) {
                            return Err(protocol_error("MCP JSON-RPC unicode escape is invalid"));
                        }
                        offset = hex_end;
                    }
                    _ => {
                        return Err(protocol_error("MCP JSON-RPC string escape is unsupported"));
                    }
                }
            }
            0x00..=0x1f => {
                return Err(protocol_error(
                    "MCP JSON-RPC string contains an unescaped control character",
                ));
            }
            _ => offset += 1,
        }
    }
    Err(protocol_error("MCP JSON-RPC string field is unterminated"))
}

fn json_object_token_end(text: &str, start: usize, depth: usize) -> Result<usize, EvaError> {
    let bytes = text.as_bytes();
    if bytes.get(start).copied() != Some(b'{') {
        return Err(protocol_error("MCP JSON-RPC object value is invalid"));
    }
    let mut offset = start + 1;
    skip_json_whitespace(bytes, &mut offset);
    if bytes.get(offset).copied() == Some(b'}') {
        return Ok(offset + 1);
    }
    loop {
        if bytes.get(offset).copied() != Some(b'"') {
            return Err(protocol_error("MCP JSON-RPC nested object key is invalid"));
        }
        offset = json_string_token_end(text, offset)?;
        skip_json_whitespace(bytes, &mut offset);
        if bytes.get(offset).copied() != Some(b':') {
            return Err(protocol_error(
                "MCP JSON-RPC nested object key is missing a colon",
            ));
        }
        offset += 1;
        skip_json_whitespace(bytes, &mut offset);
        offset = json_value_token_end_at_depth(text, offset, depth)?;
        skip_json_whitespace(bytes, &mut offset);
        match bytes.get(offset).copied() {
            Some(b',') => {
                offset += 1;
                skip_json_whitespace(bytes, &mut offset);
                if bytes.get(offset).copied() == Some(b'}') {
                    return Err(protocol_error(
                        "MCP JSON-RPC nested object contains a trailing comma",
                    ));
                }
            }
            Some(b'}') => return Ok(offset + 1),
            _ => {
                return Err(protocol_error(
                    "MCP JSON-RPC nested object fields are not comma-separated",
                ));
            }
        }
    }
}

fn json_array_token_end(text: &str, start: usize, depth: usize) -> Result<usize, EvaError> {
    let bytes = text.as_bytes();
    if bytes.get(start).copied() != Some(b'[') {
        return Err(protocol_error("MCP JSON-RPC array value is invalid"));
    }
    let mut offset = start + 1;
    skip_json_whitespace(bytes, &mut offset);
    if bytes.get(offset).copied() == Some(b']') {
        return Ok(offset + 1);
    }
    loop {
        offset = json_value_token_end_at_depth(text, offset, depth)?;
        skip_json_whitespace(bytes, &mut offset);
        match bytes.get(offset).copied() {
            Some(b',') => {
                offset += 1;
                skip_json_whitespace(bytes, &mut offset);
                if bytes.get(offset).copied() == Some(b']') {
                    return Err(protocol_error(
                        "MCP JSON-RPC array contains a trailing comma",
                    ));
                }
            }
            Some(b']') => return Ok(offset + 1),
            _ => {
                return Err(protocol_error(
                    "MCP JSON-RPC array values are not comma-separated",
                ));
            }
        }
    }
}

fn is_json_number(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut offset = 0usize;
    if bytes.get(offset).copied() == Some(b'-') {
        offset += 1;
    }
    match bytes.get(offset).copied() {
        Some(b'0') => offset += 1,
        Some(b'1'..=b'9') => {
            offset += 1;
            while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                offset += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(offset).copied() == Some(b'.') {
        offset += 1;
        let fraction_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == fraction_start {
            return false;
        }
    }
    if matches!(bytes.get(offset).copied(), Some(b'e' | b'E')) {
        offset += 1;
        if matches!(bytes.get(offset).copied(), Some(b'+' | b'-')) {
            offset += 1;
        }
        let exponent_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == exponent_start {
            return false;
        }
    }
    offset == bytes.len()
}

/// Find a nested field in the already isolated tools array. This legacy
/// helper is intentionally not used for protocol envelope or phase fields.
fn find_json_field_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let position = text.find(&pattern)?;
    let after_key = position + pattern.len();
    let colon = text[after_key..].find(':')?;
    let mut value_start = after_key + colon + 1;
    while text
        .as_bytes()
        .get(value_start)
        .is_some_and(|byte| matches!(*byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        value_start += 1;
    }
    let value_end = json_value_token_end(text, value_start).ok()?;
    Some(&text[value_start..value_end])
}

/// 执行 `json_string_field` 对应的处理逻辑。
fn json_string_field(text: &str, key: &str) -> Result<Option<String>, EvaError> {
    json_field_value(text, key)?
        .map(parse_json_string)
        .transpose()
}

/// 执行 `json_u64_field` 对应的处理逻辑。
fn json_u64_field(text: &str, key: &str) -> Result<Option<u64>, EvaError> {
    json_field_value(text, key)?
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
    json_field_value(text, key)?
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
pub(crate) fn parse_json_string(value: &str) -> Result<String, EvaError> {
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
                        let scalar = if (0xd800..=0xdbff).contains(&code) {
                            if chars.next() != Some('\\') || chars.next() != Some('u') {
                                return Err(protocol_error(
                                    "MCP JSON-RPC unicode surrogate pair is incomplete",
                                ));
                            }
                            let mut low_hex = String::new();
                            for _ in 0..4 {
                                low_hex.push(chars.next().ok_or_else(|| {
                                    protocol_error(
                                        "MCP JSON-RPC unicode surrogate pair is incomplete",
                                    )
                                })?);
                            }
                            let low = u16::from_str_radix(&low_hex, 16).map_err(|error| {
                                protocol_error("MCP JSON-RPC unicode escape is invalid")
                                    .with_context("parse_error", error.to_string())
                            })?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return Err(protocol_error(
                                    "MCP JSON-RPC unicode surrogate pair is invalid",
                                ));
                            }
                            0x1_0000 + (((code as u32) - 0xd800) << 10) + ((low as u32) - 0xdc00)
                        } else if (0xdc00..=0xdfff).contains(&code) {
                            return Err(protocol_error(
                                "MCP JSON-RPC unicode surrogate pair is invalid",
                            ));
                        } else {
                            code as u32
                        };
                        if let Some(character) = char::from_u32(scalar) {
                            output.push(character);
                        } else {
                            return Err(protocol_error("MCP JSON-RPC unicode scalar is invalid"));
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
pub(crate) fn json_string(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

/// 按 `escape_json` 的协议约定生成输出。
fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value if value.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", value as u32));
            }
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

    #[test]
    fn direct_object_parser_enforces_json_nesting_boundary() {
        fn nested_arrays(depth: usize) -> String {
            format!("{}null{}", "[".repeat(depth), "]".repeat(depth))
        }

        let accepted = format!(
            "{{\"value\":{}}}",
            nested_arrays(MAX_JSON_NESTING_DEPTH - 1)
        );
        let rejected = format!("{{\"value\":{}}}", nested_arrays(MAX_JSON_NESTING_DEPTH));

        assert!(parse_json_object_fields(&accepted).is_ok());
        let error = parse_json_object_fields(&rejected).unwrap_err();
        assert!(error.message().contains("nesting limit exceeded"));
    }

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

    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("response body must not be read")
        }
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
        assert!(!requests[0].contains("MCP-Protocol-Version:"));
        assert!(requests[1..]
            .iter()
            .all(|request| request.contains("MCP-Protocol-Version: 2025-11-25\r\n")));
        assert!(requests
            .iter()
            .all(|request| !request.contains("Mcp-Session-Id:")));
    }

    #[test]
    fn streamable_http_session_propagates_headers_and_deletes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (sender, receiver) = channel();
        let server = thread::spawn(move || {
            for request_index in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default();
                let response = match request_index {
                    0 => http_response_with_session(
                        200,
                        &response(1, "{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"session-fake\",\"version\":\"1\"}}"),
                        "session-opaque-1",
                    ),
                    1 if body.contains("notifications/initialized") => {
                        http_response_with_session(202, "", "session-opaque-1")
                    }
                    2 if body.contains("\"method\":\"tools/list\"") => http_response_with_session(
                        200,
                        &response(2, "{\"tools\":[{\"name\":\"list_issues\",\"inputSchema\":{\"type\":\"object\"}}]}"),
                        "session-opaque-1",
                    ),
                    3 if body.contains("\"method\":\"tools/call\"") => http_response_with_session(
                        200,
                        &response(3, "{\"content\":[{\"type\":\"text\",\"text\":\"session-ok\"}],\"isError\":false}"),
                        "session-opaque-1",
                    ),
                    4 if request.starts_with("DELETE /mcp HTTP/1.1\r\n") => {
                        "HTTP/1.1 204 No Content\r\nMcp-Session-Id: session-opaque-1\r\nConnection: close\r\n\r\n".to_owned()
                    }
                    _ => http_response(400, ""),
                };
                stream.write_all(response.as_bytes()).unwrap();
                sender.send(request).unwrap();
            }
        });

        let report = client(["list_issues"])
            .call_http(
                &endpoint,
                BTreeMap::new(),
                RequestId::parse("req-mcp-session").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap();

        let requests = (0..5)
            .map(|_| receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect::<Vec<_>>();
        server.join().unwrap();
        assert!(requests[0].starts_with("POST /mcp HTTP/1.1\r\n"));
        assert!(!requests[0].contains("Mcp-Session-Id:"));
        assert!(!requests[0].contains("MCP-Protocol-Version:"));
        for request in &requests[1..] {
            assert!(request.contains("Mcp-Session-Id: session-opaque-1\r\n"));
            assert!(request.contains("MCP-Protocol-Version: 2025-11-25\r\n"));
        }
        assert!(requests[4].starts_with("DELETE /mcp HTTP/1.1\r\n"));
        assert!(!requests[4].contains("Content-Type:"));
        assert!(!requests[4].contains("Content-Length:"));
        assert!(report
            .audit
            .contains(&"mcp.http:session_deleted".to_owned()));
        assert!(!format!("{report:?}").contains("session-opaque-1"));
        assert!(!report
            .audit
            .iter()
            .any(|entry| entry.contains("session-opaque-1")));
    }

    #[test]
    fn streamable_http_protocol_error_still_deletes_original_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (sender, receiver) = channel();
        let server = thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let response = match request_index {
                    0 => http_response_with_session(
                        200,
                        &response(1, "{\"protocolVersion\":\"2025-11-25\"}"),
                        "original-session",
                    ),
                    1 => http_response_with_session(202, "", "rotated-session"),
                    2 => {
                        "HTTP/1.1 204 No Content\r\nMcp-Session-Id: original-session\r\nConnection: close\r\n\r\n".to_owned()
                    }
                    _ => unreachable!(),
                };
                stream.write_all(response.as_bytes()).unwrap();
                sender.send(request).unwrap();
            }
        });

        let error = client(["list_issues"])
            .call_http(
                &endpoint,
                BTreeMap::new(),
                RequestId::parse("req-mcp-session-error").unwrap(),
                "list_issues",
                "{}",
            )
            .unwrap_err();

        assert_provider_code(&error, "mcp_http_session_id_mismatch");
        let requests = (0..3)
            .map(|_| receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect::<Vec<_>>();
        server.join().unwrap();
        assert!(requests[2].starts_with("DELETE /mcp HTTP/1.1\r\n"));
        assert!(requests[2].contains("Mcp-Session-Id: original-session\r\n"));
        assert!(!requests[2].contains("rotated-session"));
    }

    #[test]
    fn streamable_http_get_uses_negotiated_session_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (sender, receiver) = channel();
        let server = thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let response = match request_index {
                    0 => http_response_with_session(
                        200,
                        &response(1, "{\"protocolVersion\":\"2025-11-25\"}"),
                        "get-session",
                    ),
                    1 => http_response_with_session(202, "", "get-session"),
                    2 => "HTTP/1.1 200 OK\r\nMcp-Session-Id: get-session\r\nContent-Type: text/event-stream; charset=utf-8\r\nContent-Length: 8\r\nConnection: close\r\n\r\n: ping\n\n".to_owned(),
                    _ => unreachable!(),
                };
                stream.write_all(response.as_bytes()).unwrap();
                sender.send(request).unwrap();
            }
        });
        let mut transport = McpHttpJsonRpcTransport::new(&endpoint, BTreeMap::new()).unwrap();
        let config = McpJsonRpcClientConfig::new();
        transport
            .exchange(
                1,
                &initialize_request(1, &config),
                Duration::from_secs(1),
                1024,
            )
            .unwrap();
        transport
            .notify(&initialized_notification(), Duration::from_secs(1), 1024)
            .unwrap();
        assert_eq!(
            transport.get(Duration::from_secs(1), 1024).unwrap(),
            b": ping\n\n"
        );

        let initialize = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let initialized = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let get = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        server.join().unwrap();
        assert!(!initialize.contains("Mcp-Session-Id:"));
        assert!(initialized.contains("notifications/initialized"));
        assert!(get.starts_with("GET /mcp HTTP/1.1\r\n"));
        assert!(get.contains("Accept: text/event-stream\r\n"));
        assert!(get.contains("Mcp-Session-Id: get-session\r\n"));
        assert!(get.contains("MCP-Protocol-Version: 2025-11-25\r\n"));
        assert!(!get.contains("Content-Type:"));
        assert!(!get.contains("Content-Length:"));
    }

    #[test]
    fn sse_http_body_framings_return_matching_events_without_terminal_eof() {
        let payload = format!("data: {}\n\n", response(7, "true"));

        let fixed_source = HttpSseSource::new(
            BufReader::new(SegmentedReader::keep_alive(
                payload.as_bytes().iter().map(|byte| vec![*byte]),
            )),
            HttpBodyFraming::ContentLength(payload.len()),
            "http://sse.test".to_owned(),
        );
        let mut fixed = McpSseEventStream::from_source(Box::new(fixed_source), 1024).unwrap();
        assert_eq!(
            fixed.next_response(7_u64).unwrap().kind,
            McpJsonRpcMessageKind::Response
        );

        let close_source = HttpSseSource::new(
            BufReader::new(SegmentedReader::keep_alive(
                payload.as_bytes().iter().map(|byte| vec![*byte]),
            )),
            HttpBodyFraming::CloseDelimited,
            "http://sse.test".to_owned(),
        );
        let mut close = McpSseEventStream::from_source(Box::new(close_source), 1024).unwrap();
        assert_eq!(
            close.next_response(7_u64).unwrap().kind,
            McpJsonRpcMessageKind::Response
        );

        let first = &payload[..9];
        let second = &payload[9..payload.len() - 3];
        let third = &payload[payload.len() - 3..];
        let chunked = format!(
            "{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n{:x}\r\n{third}\r\n",
            first.len(),
            second.len(),
            third.len()
        );
        let chunked_source = HttpSseSource::new(
            BufReader::new(SegmentedReader::keep_alive(
                chunked.as_bytes().iter().map(|byte| vec![*byte]),
            )),
            HttpBodyFraming::Chunked,
            "http://sse.test".to_owned(),
        );
        let mut chunked_stream =
            McpSseEventStream::from_source(Box::new(chunked_source), 1024).unwrap();
        assert_eq!(
            chunked_stream.next_response(7_u64).unwrap().kind,
            McpJsonRpcMessageKind::Response
        );
    }

    #[test]
    fn http_exchange_payload_correlates_sse_and_rejects_status_before_body_read() {
        let payload = [
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notice\"}\n\n".to_owned(),
            format!("data: {}\n\n", response(8, "true")),
        ]
        .concat();
        let stream = McpSseEventStream::from_reader(
            SegmentedReader::keep_alive(payload.as_bytes().chunks(5).map(<[u8]>::to_vec)),
            1024,
        )
        .unwrap();
        let text = decode_http_exchange_payload(
            HttpExchangeResponse::EventStream {
                metadata: HttpJsonRpcResponse {
                    status_code: 200,
                    content_type: Some("text/event-stream".to_owned()),
                    body: Vec::new(),
                    body_truncated: false,
                    session_id: None,
                },
                stream: Box::new(stream),
            },
            8,
            1024,
        )
        .unwrap();
        assert_eq!(text, response(8, "true"));

        let unread = McpSseEventStream::from_reader(PanicReader, 1024).unwrap();
        let error = decode_http_exchange_payload(
            HttpExchangeResponse::EventStream {
                metadata: HttpJsonRpcResponse {
                    status_code: 503,
                    content_type: Some("text/event-stream".to_owned()),
                    body: Vec::new(),
                    body_truncated: false,
                    session_id: None,
                },
                stream: Box::new(unread),
            },
            9,
            1024,
        )
        .unwrap_err();
        assert_provider_code(&error, "mcp_http_status");
    }

    #[test]
    fn streamable_http_open_event_stream_is_chunked_and_session_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (request_sender, request_receiver) = channel();
        let (release_sender, release_receiver) = channel();
        let server = thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                match request_index {
                    0 => stream
                        .write_all(
                            http_response_with_session(
                                200,
                                &response(1, "{\"protocolVersion\":\"2025-11-25\"}"),
                                "stream-session-secret",
                            )
                            .as_bytes(),
                        )
                        .unwrap(),
                    1 => stream
                        .write_all(
                            http_response_with_session(202, "", "stream-session-secret").as_bytes(),
                        )
                        .unwrap(),
                    2 => {
                        let payload = format!("data: {}\n\n", response(7, "true"));
                        let first = &payload[..11];
                        let second = &payload[11..];
                        let head = "HTTP/1.1 200 OK\r\nMcp-Session-Id: stream-session-secret\r\nContent-Type: text/event-stream; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
                        stream.write_all(head.as_bytes()).unwrap();
                        for _ in 0..4 {
                            stream.write_all(b"8\r\n: ping\n\n\r\n").unwrap();
                            stream.flush().unwrap();
                            thread::sleep(Duration::from_millis(80));
                        }
                        stream
                            .write_all(format!("{:x}\r\n{first}\r\n", first.len()).as_bytes())
                            .unwrap();
                        stream
                            .write_all(format!("{:x}\r\n{second}\r\n", second.len()).as_bytes())
                            .unwrap();
                        stream.flush().unwrap();
                        request_sender.send(request).unwrap();
                        release_receiver
                            .recv_timeout(Duration::from_secs(2))
                            .unwrap();
                        continue;
                    }
                    _ => unreachable!(),
                }
                stream.flush().unwrap();
                request_sender.send(request).unwrap();
            }
        });

        let mut transport = McpHttpJsonRpcTransport::new(&endpoint, BTreeMap::new()).unwrap();
        let config = McpJsonRpcClientConfig::new();
        transport
            .exchange(
                1,
                &initialize_request(1, &config),
                Duration::from_secs(1),
                1024,
            )
            .unwrap();
        transport
            .notify(&initialized_notification(), Duration::from_secs(1), 1024)
            .unwrap();
        let mut events = transport
            .open_event_stream(Duration::from_millis(250), 1024)
            .unwrap();
        assert_eq!(
            events.next_response(7_u64).unwrap().request_id,
            Some(McpJsonRpcMessageId::Number(7))
        );
        release_sender.send(()).unwrap();

        let requests = (0..3)
            .map(|_| {
                request_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        server.join().unwrap();
        let get = &requests[2];
        assert!(get.starts_with("GET /mcp HTTP/1.1\r\n"));
        assert!(get.contains("Accept: text/event-stream\r\n"));
        assert!(get.contains("Mcp-Session-Id: stream-session-secret\r\n"));
        assert!(get.contains("MCP-Protocol-Version: 2025-11-25\r\n"));
        assert!(!format!("{transport:?}").contains("stream-session-secret"));
        assert!(!transport
            .audit()
            .iter()
            .any(|entry| entry.contains("stream-session-secret")));
    }

    #[test]
    fn streamable_http_post_event_stream_retains_interleaved_peer_messages() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (request_sender, request_receiver) = channel();
        let (release_sender, release_receiver) = channel();
        let server = thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                match request_index {
                    0 => stream
                        .write_all(
                            http_response_with_session(
                                200,
                                &response(1, "{\"protocolVersion\":\"2025-11-25\"}"),
                                "post-stream-session",
                            )
                            .as_bytes(),
                        )
                        .unwrap(),
                    1 => stream
                        .write_all(
                            http_response_with_session(202, "", "post-stream-session").as_bytes(),
                        )
                        .unwrap(),
                    2 => {
                        let body = [
                            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notice\"}\n\n".to_owned(),
                            "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n\n"
                                .to_owned(),
                            format!("data: {}\n\n", response(2, "true")),
                        ]
                        .concat();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nMcp-Session-Id: post-stream-session\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(head.as_bytes()).unwrap();
                        for fragment in body.as_bytes().chunks(7) {
                            stream.write_all(fragment).unwrap();
                            stream.flush().unwrap();
                        }
                        request_sender.send(request).unwrap();
                        release_receiver
                            .recv_timeout(Duration::from_secs(2))
                            .unwrap();
                        continue;
                    }
                    _ => unreachable!(),
                }
                stream.flush().unwrap();
                request_sender.send(request).unwrap();
            }
        });

        let mut transport = McpHttpJsonRpcTransport::new(&endpoint, BTreeMap::new()).unwrap();
        let config = McpJsonRpcClientConfig::new();
        transport
            .exchange(
                1,
                &initialize_request(1, &config),
                Duration::from_secs(1),
                2048,
            )
            .unwrap();
        transport
            .notify(&initialized_notification(), Duration::from_secs(1), 2048)
            .unwrap();
        let request = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}";
        let mut events = transport
            .post_event_stream(2, request, Duration::from_secs(1), 2048)
            .unwrap();
        assert_eq!(
            events.next_response(2_u64).unwrap().kind,
            McpJsonRpcMessageKind::Response
        );
        assert_eq!(
            events.take_peer_message().unwrap().kind,
            McpJsonRpcMessageKind::Notification
        );
        assert_eq!(
            events.take_peer_message().unwrap().kind,
            McpJsonRpcMessageKind::Request
        );
        release_sender.send(()).unwrap();

        let requests = (0..3)
            .map(|_| {
                request_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        server.join().unwrap();
        let post = &requests[2];
        assert!(post.starts_with("POST /mcp HTTP/1.1\r\n"));
        assert!(post.contains("Accept: application/json, text/event-stream\r\n"));
        assert!(post.contains("Mcp-Session-Id: post-stream-session\r\n"));
        assert!(post.contains("MCP-Protocol-Version: 2025-11-25\r\n"));
    }

    #[test]
    fn streamable_http_protocol_and_session_changes_fail_closed() {
        for (result, expected_code) in [
            ("{}", "mcp_protocol_version_missing"),
            (
                "{\"protocolVersion\":\"2024-11-05\"}",
                "mcp_protocol_version_mismatch",
            ),
            (
                "{\"protocolVersion\":\"bad version\"}",
                "mcp_protocol_version_invalid",
            ),
        ] {
            let response = HttpJsonRpcResponse {
                status_code: 200,
                content_type: Some("application/json".to_owned()),
                body: response(1, result).into_bytes(),
                body_truncated: false,
                session_id: Some("must-not-be-retained".to_owned()),
            };
            let mut transport =
                McpHttpJsonRpcTransport::new("http://127.0.0.1/mcp", BTreeMap::new()).unwrap();
            let error = transport
                .remember_initialize(
                    HttpExchangeResponse::Buffered(response.clone()),
                    1,
                    1024,
                    DEFAULT_PROTOCOL_VERSION,
                )
                .unwrap_err();
            assert_provider_code(&error, expected_code);
            assert!(transport.session_id().is_none());
            assert!(transport.negotiated_protocol_version().is_none());
            assert!(transport.is_closed());
            assert_eq!(
                transport.session_id.as_deref(),
                Some("must-not-be-retained")
            );
        }

        let mut transport =
            McpHttpJsonRpcTransport::new("http://127.0.0.1/mcp", BTreeMap::new()).unwrap();
        transport.exchange_count = 1;
        transport.protocol_version = Some(DEFAULT_PROTOCOL_VERSION.to_owned());
        transport.session_id = Some("expected-session".to_owned());
        let rotated = HttpJsonRpcResponse {
            status_code: 202,
            content_type: None,
            body: Vec::new(),
            body_truncated: false,
            session_id: Some("rotated-session".to_owned()),
        };
        let error = transport.validate_response_session(&rotated).unwrap_err();
        assert_provider_code(&error, "mcp_http_session_id_mismatch");
        assert!(transport.is_closed());
        assert_eq!(transport.session_id.as_deref(), Some("expected-session"));
        let debug = format!("{transport:?}");
        assert!(!debug.contains("expected-session"));
        assert!(!debug.contains("rotated-session"));
    }

    #[test]
    fn streamable_http_session_bound_404_invalidates_post_and_notification() {
        for (request_kind, response_session) in [
            ("post", None),
            ("post", Some("different-session")),
            ("notification", None),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let session_header = response_session
                    .map(|value| format!("Mcp-Session-Id: {value}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 404 Not Found\r\n{session_header}Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).unwrap();
                request
            });
            let mut transport = ready_http_transport(&endpoint, Some("known-session"));
            let error = if request_kind == "post" {
                transport
                    .exchange(
                        2,
                        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}",
                        Duration::from_secs(1),
                        1024,
                    )
                    .unwrap_err()
            } else {
                transport
                    .notify(
                        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}",
                        Duration::from_secs(1),
                        1024,
                    )
                    .unwrap_err()
            };

            assert_provider_code(&error, "mcp_http_session_not_found");
            assert!(transport.is_closed());
            assert!(transport.session_id.is_none());
            assert!(transport.protocol_version.is_none());
            let request = server.join().unwrap();
            assert!(request.contains("Mcp-Session-Id: known-session\r\n"));
        }
    }

    #[test]
    fn streamable_http_session_mismatch_blocks_followup_before_network_io() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let mut transport = ready_http_transport(&endpoint, Some("trusted-session"));
        let mismatch = HttpJsonRpcResponse {
            status_code: 202,
            content_type: None,
            body: Vec::new(),
            body_truncated: false,
            session_id: Some("rotated-session".to_owned()),
        };
        assert_provider_code(
            &transport.validate_response_session(&mismatch).unwrap_err(),
            "mcp_http_session_id_mismatch",
        );

        let followup = transport
            .exchange(
                2,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}",
                Duration::from_secs(1),
                1024,
            )
            .unwrap_err();
        assert_provider_code(&followup, "mcp_http_session_closed");
        assert_eq!(transport.session_id.as_deref(), Some("trusted-session"));
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn streamable_http_initialize_fields_must_be_direct_object_members() {
        let mut missing_method =
            McpHttpJsonRpcTransport::new("http://127.0.0.1:1/mcp", BTreeMap::new()).unwrap();
        let error = missing_method
            .exchange(
                1,
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"params\":{\"method\":\"initialize\",\"protocolVersion\":\"2025-11-25\"}}",
                Duration::from_secs(1),
                1024,
            )
            .unwrap_err();
        assert_provider_code(&error, "mcp_protocol_error");

        let mut nested_version =
            McpHttpJsonRpcTransport::new("http://127.0.0.1:1/mcp", BTreeMap::new()).unwrap();
        let error = nested_version
            .exchange(
                1,
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"nested\":{\"protocolVersion\":\"2025-11-25\"}}}",
                Duration::from_secs(1),
                1024,
            )
            .unwrap_err();
        assert_provider_code(&error, "mcp_protocol_version_missing");

        let nested_result = HttpJsonRpcResponse {
            status_code: 200,
            content_type: Some("application/json".to_owned()),
            body: response(1, "{\"serverInfo\":{\"protocolVersion\":\"2025-11-25\"}}").into_bytes(),
            body_truncated: false,
            session_id: None,
        };
        let mut transport =
            McpHttpJsonRpcTransport::new("http://127.0.0.1:1/mcp", BTreeMap::new()).unwrap();
        assert_provider_code(
            &transport
                .remember_initialize(
                    HttpExchangeResponse::Buffered(nested_result.clone()),
                    1,
                    1024,
                    DEFAULT_PROTOCOL_VERSION,
                )
                .unwrap_err(),
            "mcp_protocol_version_missing",
        );

        for malformed in [
            "{\"result\":{\"jsonrpc\":\"2.0\",\"id\":1,\"protocolVersion\":\"2025-11-25\"}}",
            "{\"jsonrpc\":\"2.0\",\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{},\"unknown\":{\"missing_colon\" 1}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{},\"unknown\":[1,]}",
        ] {
            assert_provider_code(
                &parse_json_rpc_response(malformed, 1).unwrap_err(),
                "mcp_protocol_error",
            );
        }
        assert!(parse_json_string("\"\\uD800\"").is_err());
        assert_eq!(
            parse_json_string("\"\\uD83D\\uDE00\"").unwrap(),
            "\u{1f600}"
        );
    }

    #[test]
    fn streamable_http_response_shapes_are_protocol_exact() {
        for status in [200, 204] {
            let response = HttpJsonRpcResponse {
                status_code: status,
                content_type: None,
                body: Vec::new(),
                body_truncated: false,
                session_id: None,
            };
            assert_provider_code(
                &validate_http_notification_response(response, 1024).unwrap_err(),
                "mcp_http_status",
            );
        }
        let notification_body = HttpJsonRpcResponse {
            status_code: 202,
            content_type: Some("application/json".to_owned()),
            body: b"{}".to_vec(),
            body_truncated: false,
            session_id: None,
        };
        assert_provider_code(
            &validate_http_notification_response(notification_body, 1024).unwrap_err(),
            "mcp_http_notification_body_unexpected",
        );

        for (status, content_type, expected_code) in [
            (204, Some("text/event-stream"), "mcp_http_status"),
            (
                200,
                Some("application/json"),
                "mcp_http_content_type_unsupported",
            ),
            (200, None, "mcp_http_content_type_missing"),
            (405, None, "mcp_http_sse_unsupported"),
        ] {
            let response = HttpJsonRpcResponse {
                status_code: status,
                content_type: content_type.map(str::to_owned),
                body: Vec::new(),
                body_truncated: false,
                session_id: None,
            };
            assert_provider_code(
                &decode_http_event_stream_response(response, 1024).unwrap_err(),
                expected_code,
            );
        }
        let event_stream = HttpJsonRpcResponse {
            status_code: 200,
            content_type: Some("text/event-stream".to_owned()),
            body: b": ping\n\n".to_vec(),
            body_truncated: false,
            session_id: None,
        };
        assert_eq!(
            decode_http_event_stream_response(event_stream, 1024).unwrap(),
            b": ping\n\n"
        );
    }

    #[test]
    fn streamable_http_wrapper_enforces_phase_and_closes_stateless_session() {
        use crate::streamable_http::McpStreamableHttpSession;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let response = if request_index == 0 {
                    http_response(200, &response(1, "{\"protocolVersion\":\"2025-11-25\"}"))
                } else {
                    http_response(202, "")
                };
                stream.write_all(response.as_bytes()).unwrap();
                requests.push(request);
            }
            requests
        });
        let config = McpStreamableHttpConfig::legacy_http(endpoint).unwrap();
        let mut session =
            McpStreamableHttpSession::new(config, BTreeMap::new(), Duration::from_secs(1), 1024)
                .unwrap();
        let normal_request =
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}";
        assert_provider_code(
            &session.post(2, normal_request).unwrap_err(),
            "mcp_http_session_not_initialized",
        );
        session
            .initialize(1, &initialize_request(1, &McpJsonRpcClientConfig::new()))
            .unwrap();
        assert!(!session.is_ready());
        assert_provider_code(
            &session.post(2, normal_request).unwrap_err(),
            "mcp_http_session_not_ready",
        );
        assert_provider_code(&session.get().unwrap_err(), "mcp_http_session_not_ready");
        assert_provider_code(
            &session
                .initialize(2, &initialize_request(2, &McpJsonRpcClientConfig::new()))
                .unwrap_err(),
            "mcp_http_session_already_initialized",
        );
        assert_provider_code(
            &session
                .notify("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}")
                .unwrap_err(),
            "mcp_http_initialized_notification_required",
        );
        session.notify(&initialized_notification()).unwrap();
        assert!(session.is_ready());
        assert_provider_code(
            &session.notify(&initialized_notification()).unwrap_err(),
            "mcp_http_initialized_notification_duplicate",
        );
        assert_provider_code(
            &session
                .post(2, &initialize_request(2, &McpJsonRpcClientConfig::new()))
                .unwrap_err(),
            "mcp_http_session_already_initialized",
        );
        session.shutdown().unwrap();
        assert!(session.is_closed());
        assert!(!session.is_ready());
        assert!(session.negotiated_protocol_version().is_none());
        assert_provider_code(
            &session.post(2, normal_request).unwrap_err(),
            "mcp_http_session_closed",
        );

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("\"method\":\"initialize\""));
        assert!(requests[1].contains("notifications/initialized"));
        assert!(requests
            .iter()
            .all(|request| !request.starts_with("DELETE ")));
    }

    #[test]
    fn streamable_http_invalid_initialize_deletes_provisional_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                let response = if request_index == 0 {
                    http_response_with_session(
                        200,
                        &response(1, "{\"serverInfo\":{\"protocolVersion\":\"2025-11-25\"}}"),
                        "provisional-session",
                    )
                } else {
                    "HTTP/1.1 204 No Content\r\nMcp-Session-Id: provisional-session\r\nConnection: close\r\n\r\n".to_owned()
                };
                stream.write_all(response.as_bytes()).unwrap();
                requests.push(request);
            }
            requests
        });
        let mut transport = McpHttpJsonRpcTransport::new(&endpoint, BTreeMap::new()).unwrap();
        let error = transport
            .exchange(
                1,
                &initialize_request(1, &McpJsonRpcClientConfig::new()),
                Duration::from_secs(1),
                1024,
            )
            .unwrap_err();
        assert_provider_code(&error, "mcp_protocol_version_missing");
        assert!(transport.is_closed());
        assert!(transport.session_id.is_none());
        assert!(transport.protocol_version.is_none());

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("DELETE /mcp HTTP/1.1\r\n"));
        assert!(requests[1].contains("Mcp-Session-Id: provisional-session\r\n"));
        assert!(requests[1].contains("MCP-Protocol-Version: 2025-11-25\r\n"));
    }

    #[test]
    fn streamable_http_session_header_is_bounded_visible_ascii() {
        let valid = b"HTTP/1.1 204 No Content\r\nMcp-Session-Id: opaque.123_-~\r\nConnection: keep-alive\r\n\r\n";
        let mut valid_reader = SegmentedReader::fragmented(valid);
        let response =
            read_http_json_rpc_response(&mut valid_reader, "http://session-header.test", 1024)
                .unwrap();
        assert_eq!(response.session_id.as_deref(), Some("opaque.123_-~"));

        let oversized = "x".repeat(MCP_SESSION_ID_LIMIT_BYTES + 1);
        for raw in [
            "HTTP/1.1 204 No Content\r\nMcp-Session-Id:\r\n\r\n".to_owned(),
            "HTTP/1.1 204 No Content\r\nMcp-Session-Id: has space\r\n\r\n".to_owned(),
            "HTTP/1.1 204 No Content\r\nMcp-Session-Id: first\r\nMcp-Session-Id: second\r\n\r\n"
                .to_owned(),
            format!("HTTP/1.1 204 No Content\r\nMcp-Session-Id: {oversized}\r\n\r\n"),
        ] {
            let mut reader = SegmentedReader::new([raw.into_bytes()]);
            let error =
                read_http_json_rpc_response(&mut reader, "http://session-header.test", 1024)
                    .unwrap_err();
            assert_provider_code(&error, "mcp_http_framing_invalid");
        }
    }

    #[test]
    fn streamable_http_delete_handles_bodyless_and_terminal_statuses() {
        for (status, expected_error) in [
            (202, None),
            (204, None),
            (405, None),
            (404, Some("mcp_http_session_not_found")),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                for request_index in 0..2 {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = read_test_http_request(&mut stream);
                    let response = if request_index == 0 {
                        http_response_with_session(
                            200,
                            &response(1, "{\"protocolVersion\":\"2025-11-25\"}"),
                            "delete-session",
                        )
                    } else if status == 202 {
                        "HTTP/1.1 202 Accepted\r\nMcp-Session-Id: delete-session\r\nConnection: keep-alive\r\n\r\n".to_owned()
                    } else {
                        format!(
                            "HTTP/1.1 {status} Status\r\nMcp-Session-Id: delete-session\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
                        )
                    };
                    stream.write_all(response.as_bytes()).unwrap();
                    if request_index == 1 {
                        assert!(request.starts_with("DELETE /mcp HTTP/1.1\r\n"));
                        assert!(request.contains("Mcp-Session-Id: delete-session\r\n"));
                    }
                }
            });
            let mut transport = McpHttpJsonRpcTransport::new(&endpoint, BTreeMap::new()).unwrap();
            let config = McpJsonRpcClientConfig::new();
            transport
                .exchange(
                    1,
                    &initialize_request(1, &config),
                    Duration::from_secs(1),
                    1024,
                )
                .unwrap();
            let shutdown = transport.shutdown_session(Duration::from_secs(1), 1024);
            match expected_error {
                Some(code) => assert_provider_code(&shutdown.unwrap_err(), code),
                None => shutdown.unwrap(),
            }
            assert!(transport.session_id().is_none());
            assert!(transport.negotiated_protocol_version().is_none());
            transport
                .shutdown_session(Duration::from_secs(1), 1024)
                .unwrap();
            server.join().unwrap();
        }
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
    fn http_framing_handles_bodyless_statuses_without_waiting_for_eof() {
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
            if status == 202 {
                validate_http_notification_response(response.clone(), 1024).unwrap();
            } else {
                assert_provider_code(
                    &validate_http_notification_response(response.clone(), 1024).unwrap_err(),
                    "mcp_http_status",
                );
            }
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
            session_id: None,
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
            session_id: None,
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
            "Mcp-Session-Id",
            "MCP-Protocol-Version",
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

    #[test]
    fn real_slow_drip_body_cannot_reset_the_request_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _request = read_test_http_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.flush().unwrap();
            for byte in b"null" {
                thread::sleep(Duration::from_millis(35));
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
        });
        let started = Instant::now();
        let error = send_http_request(
            &McpHttpConnector::Plaintext,
            &endpoint,
            &BTreeMap::new(),
            HttpRequestSpec {
                extra_headers: &[],
                method: "GET",
                accept: "application/json",
                body: "",
                timeout: Duration::from_millis(80),
                output_limit_bytes: 1024,
                allow_bodyless_accepted: false,
            },
        )
        .unwrap_err();
        assert_provider_code(&error, "mcp_http_read_timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
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

    fn http_response_with_session(status: u16, body: &str, session_id: &str) -> String {
        format!(
            "HTTP/1.1 {status} OK\r\nMcp-Session-Id: {session_id}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn ready_http_transport(endpoint: &str, session_id: Option<&str>) -> McpHttpJsonRpcTransport {
        let mut transport = McpHttpJsonRpcTransport::new(endpoint, BTreeMap::new()).unwrap();
        transport.exchange_count = 1;
        transport.protocol_version = Some(DEFAULT_PROTOCOL_VERSION.to_owned());
        transport.session_id = session_id.map(str::to_owned);
        transport.initialized_notification_sent = true;
        transport
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
