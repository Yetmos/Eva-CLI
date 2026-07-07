//! MCP JSON-RPC client transport.

use crate::policy::McpAllowlist;
use crate::session::{McpServerTransport, McpSessionConfig};
use eva_core::{AdapterId, EvaError, InvokeOutput, RequestId};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP JSON-RPC client transport with stdio process boundaries";

const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcClientConfig {
    pub protocol_version: String,
    pub client_name: String,
    pub client_version: String,
    pub request_timeout_ms: u64,
    pub output_limit_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcClient {
    adapter_id: AdapterId,
    allowlist: McpAllowlist,
    config: McpJsonRpcClientConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcCallReport {
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub tool: String,
    pub output: InvokeOutput,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonRpcTool {
    pub name: String,
}

pub trait McpJsonRpcTransport {
    fn exchange(
        &mut self,
        expected_id: u64,
        request: &str,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<String, EvaError>;

    fn notify(&mut self, notification: &str) -> Result<(), EvaError>;

    fn audit(&self) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Debug)]
pub struct McpStdioJsonRpcTransport {
    child: Option<Child>,
    stdin: ChildStdin,
    receiver: mpsc::Receiver<ReaderMessage>,
    audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonRpcResponse {
    result: String,
}

impl Default for McpJsonRpcClientConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl McpJsonRpcClientConfig {
    pub fn new() -> Self {
        Self {
            protocol_version: DEFAULT_PROTOCOL_VERSION.to_owned(),
            client_name: "eva-mcp".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
        }
    }

    pub fn with_protocol_version(mut self, protocol_version: impl Into<String>) -> Self {
        self.protocol_version = protocol_version.into();
        self
    }

    pub fn with_client_name(mut self, client_name: impl Into<String>) -> Self {
        self.client_name = client_name.into();
        self
    }

    pub fn with_client_version(mut self, client_version: impl Into<String>) -> Self {
        self.client_version = client_version.into();
        self
    }

    pub fn with_request_timeout_ms(mut self, request_timeout_ms: u64) -> Self {
        self.request_timeout_ms = request_timeout_ms;
        self
    }

    pub fn with_output_limit_bytes(mut self, output_limit_bytes: usize) -> Self {
        self.output_limit_bytes = output_limit_bytes;
        self
    }
}

impl McpJsonRpcClient {
    pub fn new(adapter_id: AdapterId, allowlist: McpAllowlist) -> Self {
        Self {
            adapter_id,
            allowlist,
            config: McpJsonRpcClientConfig::default(),
        }
    }

    pub fn with_config(mut self, config: McpJsonRpcClientConfig) -> Self {
        self.config = config;
        self
    }

    pub fn config(&self) -> &McpJsonRpcClientConfig {
        &self.config
    }

    pub fn call_stdio(
        &self,
        session_config: &McpSessionConfig,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        match session_config.server_transport {
            McpServerTransport::Stdio => {
                let mut transport = McpStdioJsonRpcTransport::start(session_config)?;
                let mut report =
                    self.call_tool_with_transport(&mut transport, request_id, tool, input)?;
                report.audit.extend(transport.shutdown());
                Ok(report)
            }
        }
    }

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

        transport.notify(&initialized_notification())?;

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
    pub fn start(config: &McpSessionConfig) -> Result<Self, EvaError> {
        validate_stdio_config(config)?;
        let started_at = Instant::now();
        let mut child = Command::new(&config.process.command)
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

        let stdin = child.stdin.take().ok_or_else(|| {
            EvaError::internal("MCP stdio stdin was not available")
                .with_context("adapter_id", config.adapter_id.as_str())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            EvaError::internal("MCP stdio stdout was not available")
                .with_context("adapter_id", config.adapter_id.as_str())
        })?;
        let (sender, receiver) = mpsc::channel();
        spawn_stdout_reader(stdout, sender);

        let mut audit = vec![
            "mcp.stdio:started".to_owned(),
            "shell:false".to_owned(),
            "command_allowlist:passed".to_owned(),
            format!("startup_timeout_ms:{}", config.process.startup_timeout_ms),
            format!("startup_duration_ms:{}", started_at.elapsed().as_millis()),
        ];
        audit.push(format!("process_id:{}", child.id()));

        Ok(Self {
            child: Some(child),
            stdin,
            receiver,
            audit,
        })
    }

    pub fn shutdown(&mut self) -> Vec<String> {
        let mut audit = Vec::new();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            audit.push("mcp.stdio:stopped".to_owned());
        }
        audit
    }
}

impl McpJsonRpcTransport for McpStdioJsonRpcTransport {
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

    fn notify(&mut self, notification: &str) -> Result<(), EvaError> {
        self.stdin
            .write_all(notification.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|error| {
                EvaError::unavailable("failed to write MCP JSON-RPC notification")
                    .with_context("io_error", error.to_string())
            })
    }

    fn audit(&self) -> Vec<String> {
        self.audit.clone()
    }
}

impl Drop for McpStdioJsonRpcTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
enum ReaderMessage {
    Line(Vec<u8>),
    LimitExceeded,
    ReadError(String),
    Eof,
}

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

fn next_json_rpc_id(next: &mut u64) -> u64 {
    let id = *next;
    *next += 1;
    id
}

fn initialize_request(id: u64, config: &McpJsonRpcClientConfig) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"initialize\",\"params\":{{\"protocolVersion\":{},\"capabilities\":{{}},\"clientInfo\":{{\"name\":{},\"version\":{}}}}}}}",
        id,
        json_string(&config.protocol_version),
        json_string(&config.client_name),
        json_string(&config.client_version)
    )
}

fn initialized_notification() -> String {
    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}".to_owned()
}

fn tools_list_request(id: u64) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/list\",\"params\":{{}}}}",
        id
    )
}

fn tools_call_request(id: u64, tool: &str, input: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/call\",\"params\":{{\"name\":{},\"arguments\":{}}}}}",
        id,
        json_string(tool),
        tool_arguments_json(input)
    )
}

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

fn protocol_error(message: impl Into<String>) -> EvaError {
    EvaError::unavailable(message)
        .with_provider_code("mcp_protocol_error")
        .with_retryable(false)
}

fn truncate_context(value: &str) -> String {
    const MAX_CONTEXT_CHARS: usize = 120;
    value.chars().take(MAX_CONTEXT_CHARS).collect()
}

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

fn json_string_field(text: &str, key: &str) -> Result<Option<String>, EvaError> {
    json_field_value(text, key)
        .map(parse_json_string)
        .transpose()
}

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

fn json_string(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use std::collections::VecDeque;

    #[derive(Debug, Default)]
    struct FakeTransport {
        responses: VecDeque<Result<String, EvaError>>,
        requests: Vec<String>,
        notifications: Vec<String>,
    }

    impl FakeTransport {
        fn with_responses(responses: impl IntoIterator<Item = String>) -> Self {
            Self {
                responses: responses.into_iter().map(Ok).collect(),
                requests: Vec::new(),
                notifications: Vec::new(),
            }
        }

        fn with_error(error: EvaError) -> Self {
            Self {
                responses: [Err(error)].into_iter().collect(),
                requests: Vec::new(),
                notifications: Vec::new(),
            }
        }
    }

    impl McpJsonRpcTransport for FakeTransport {
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

        fn notify(&mut self, notification: &str) -> Result<(), EvaError> {
            self.notifications.push(notification.to_owned());
            Ok(())
        }
    }

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

    fn client(tools: impl IntoIterator<Item = &'static str>) -> McpJsonRpcClient {
        McpJsonRpcClient::new(
            AdapterId::parse("github-mcp").unwrap(),
            McpAllowlist::from_tools(tools).unwrap(),
        )
    }

    fn response(id: u64, result: &str) -> String {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}")
    }
}
