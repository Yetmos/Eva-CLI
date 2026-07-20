//! Controlled loopback Streamable HTTP server transport.
//!
//! This boundary intentionally exposes only side-effect-free tools. It is a
//! local transport, not an authentication boundary: callers that need remote
//! access or mutating tools must add an independently reviewed authority.

use crate::json_rpc::{
    json_string, parse_json_object_fields, parse_json_string, DEFAULT_PROTOCOL_VERSION,
};
use crate::server::{
    EvaMcpServerSurface, McpServerToolCall, McpServerToolHandler, McpServerToolResult,
};
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "bounded loopback MCP Streamable HTTP server transport and session gate";

const DEFAULT_PATH: &str = "/mcp";
const DEFAULT_HEADER_LIMIT_BYTES: usize = 16 * 1024;
const DEFAULT_REQUEST_LIMIT_BYTES: usize = 1024 * 1024;
const DEFAULT_RESPONSE_LIMIT_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_SESSIONS: usize = 64;
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_IO_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_HEADER_COUNT: usize = 128;
const MAX_METHOD_BYTES: usize = 128;
const MAX_ID_STRING_BYTES: usize = 256;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_SURFACE_TOOLS: usize = 128;
const MIN_RESPONSE_LIMIT_BYTES: usize = 512;
const MAX_CONFIGURED_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Bounded configuration for one local HTTP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamableHttpServerConfig {
    path: String,
    header_limit_bytes: usize,
    request_limit_bytes: usize,
    response_limit_bytes: usize,
    max_sessions: usize,
    io_timeout: Duration,
    accept_poll_interval: Duration,
    allowed_origins: BTreeSet<String>,
}

impl Default for McpStreamableHttpServerConfig {
    fn default() -> Self {
        Self {
            path: DEFAULT_PATH.to_owned(),
            header_limit_bytes: DEFAULT_HEADER_LIMIT_BYTES,
            request_limit_bytes: DEFAULT_REQUEST_LIMIT_BYTES,
            response_limit_bytes: DEFAULT_RESPONSE_LIMIT_BYTES,
            max_sessions: DEFAULT_MAX_SESSIONS,
            io_timeout: DEFAULT_IO_TIMEOUT,
            accept_poll_interval: DEFAULT_ACCEPT_POLL_INTERVAL,
            allowed_origins: BTreeSet::new(),
        }
    }
}

impl McpStreamableHttpServerConfig {
    /// Create a server configuration for one exact origin-form path.
    pub fn new(path: impl Into<String>) -> Result<Self, EvaError> {
        let config = Self {
            path: path.into(),
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    /// Set independent bounded header, request-body, and response-body limits.
    pub fn with_limits(
        mut self,
        header_limit_bytes: usize,
        request_limit_bytes: usize,
        response_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        self.header_limit_bytes = header_limit_bytes;
        self.request_limit_bytes = request_limit_bytes;
        self.response_limit_bytes = response_limit_bytes;
        self.validate()?;
        Ok(self)
    }

    /// Set the maximum number of initialized sessions retained by the listener.
    pub fn with_max_sessions(mut self, max_sessions: usize) -> Result<Self, EvaError> {
        self.max_sessions = max_sessions;
        self.validate()?;
        Ok(self)
    }

    /// Set socket I/O and nonblocking accept poll bounds.
    pub fn with_timeouts(
        mut self,
        io_timeout: Duration,
        accept_poll_interval: Duration,
    ) -> Result<Self, EvaError> {
        self.io_timeout = io_timeout;
        self.accept_poll_interval = accept_poll_interval;
        self.validate()?;
        Ok(self)
    }

    /// Explicitly allow one exact HTTP Origin value. When this is not called,
    /// every request carrying `Origin` is rejected.
    pub fn with_allowed_origin(mut self, origin: impl Into<String>) -> Result<Self, EvaError> {
        let origin = origin.into();
        validate_origin_literal(&origin)?;
        self.allowed_origins.insert(origin);
        self.validate()?;
        Ok(self)
    }

    /// Return the exact endpoint path.
    pub fn path(&self) -> &str {
        &self.path
    }

    fn validate(&self) -> Result<(), EvaError> {
        if self.path.len() > 1024
            || !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.contains(['?', '#', '%', '\\'])
            || self.path.contains("//")
            || self
                .path
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
            || !self.path.is_ascii()
            || self.path.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
        {
            return Err(EvaError::invalid_argument(
                "MCP HTTP server path must be bounded exact origin-form ASCII",
            )
            .with_provider_code("mcp_server_path_invalid"));
        }
        if !(512..=MAX_CONFIGURED_LIMIT_BYTES).contains(&self.header_limit_bytes)
            || !(1..=MAX_CONFIGURED_LIMIT_BYTES).contains(&self.request_limit_bytes)
            || !(MIN_RESPONSE_LIMIT_BYTES..=MAX_CONFIGURED_LIMIT_BYTES)
                .contains(&self.response_limit_bytes)
            || self.max_sessions == 0
            || self.max_sessions > 4096
            || self.io_timeout.is_zero()
            || self.io_timeout > MAX_IO_TIMEOUT
            || self.accept_poll_interval.is_zero()
            || self.accept_poll_interval > Duration::from_secs(1)
        {
            return Err(
                EvaError::invalid_argument("MCP HTTP server resource bounds are invalid")
                    .with_provider_code("mcp_server_limits_invalid"),
            );
        }
        for origin in &self.allowed_origins {
            validate_origin_literal(origin)?;
        }
        Ok(())
    }
}

/// Cooperative stop signal for a running server loop.
#[derive(Clone)]
pub struct McpServerShutdownHandle {
    requested: Arc<AtomicBool>,
    active_stream: Arc<Mutex<Option<TcpStream>>>,
}

impl McpServerShutdownHandle {
    /// Request bounded listener shutdown. Active socket I/O remains bounded by
    /// the configured read/write timeout.
    pub fn shutdown(&self) {
        self.requested.store(true, Ordering::Release);
        let active_stream = lock_unpoisoned(&self.active_stream);
        if let Some(stream) = active_stream.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Return whether shutdown has been requested.
    pub fn is_shutdown_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

impl fmt::Debug for McpServerShutdownHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerShutdownHandle")
            .field("requested", &self.is_shutdown_requested())
            .finish()
    }
}

struct ActiveStreamGuard {
    slot: Arc<Mutex<Option<TcpStream>>>,
}

impl ActiveStreamGuard {
    fn register(handle: &McpServerShutdownHandle, stream: &TcpStream) -> io::Result<Self> {
        let abort_stream = stream.try_clone()?;
        let slot = handle.active_stream.clone();
        *lock_unpoisoned(&slot) = Some(abort_stream);
        Ok(Self { slot })
    }
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&self.slot).take();
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Redacted evidence returned after the listener loop exits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpServerServeReport {
    /// Connections accepted from the loopback interface.
    pub accepted_connections: usize,
    /// Complete bounded HTTP requests dispatched.
    pub requests: usize,
    /// Sessions created by valid initialize requests.
    pub sessions_created: usize,
    /// Sessions removed by valid DELETE requests.
    pub sessions_deleted: usize,
    /// Sessions closed by listener shutdown.
    pub sessions_closed_on_shutdown: usize,
    /// Calls that crossed the explicit tool gate and reached the handler.
    pub handler_calls: usize,
    /// Hidden tool calls rejected before handler execution.
    pub blocked_tool_calls: usize,
    /// Protocol, framing, or write failures observed without payload details.
    pub protocol_errors: usize,
    /// Must be zero after `serve` returns successfully.
    pub dangling_sessions: usize,
    /// Stable low-cardinality evidence with no request, session, or payload data.
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    AwaitingInitialized,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerSession {
    peer_ip: IpAddr,
    phase: SessionPhase,
}

/// A bounded synchronous Streamable HTTP server over one pre-bound loopback
/// listener. Session state spans connections because MCP clients may use a new
/// connection for each protocol step.
pub struct McpStreamableHttpServer<H> {
    listener: TcpListener,
    local_addr: SocketAddr,
    expected_host: String,
    config: McpStreamableHttpServerConfig,
    surface: EvaMcpServerSurface,
    handler: H,
    sessions: BTreeMap<String, ServerSession>,
    shutdown: McpServerShutdownHandle,
    report: McpServerServeReport,
}

impl<H> fmt::Debug for McpStreamableHttpServer<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpServer")
            .field("local_addr", &self.local_addr)
            .field("path", &self.config.path)
            .field("tool_count", &self.surface.tools().len())
            .field("active_session_count", &self.sessions.len())
            .field("handler", &"[REDACTED_HANDLER]")
            .finish()
    }
}

impl<H: McpServerToolHandler> McpStreamableHttpServer<H> {
    /// Bind a numeric loopback address and construct the controlled server.
    pub fn bind(
        address: SocketAddr,
        config: McpStreamableHttpServerConfig,
        surface: EvaMcpServerSurface,
        handler: H,
    ) -> Result<Self, EvaError> {
        if !address.ip().is_loopback() {
            return Err(non_loopback_error());
        }
        let listener = TcpListener::bind(address).map_err(|error| {
            EvaError::unavailable("failed to bind MCP HTTP server listener")
                .with_provider_code("mcp_server_bind_failed")
                .with_context("io_error", error.to_string())
        })?;
        Self::from_listener(listener, config, surface, handler)
    }

    /// Construct a server around a pre-bound loopback listener.
    pub fn from_listener(
        listener: TcpListener,
        config: McpStreamableHttpServerConfig,
        surface: EvaMcpServerSurface,
        handler: H,
    ) -> Result<Self, EvaError> {
        config.validate()?;
        let local_addr = listener.local_addr().map_err(|error| {
            EvaError::unavailable("failed to inspect MCP HTTP server listener")
                .with_provider_code("mcp_server_listener_invalid")
                .with_context("io_error", error.to_string())
        })?;
        if !local_addr.ip().is_loopback() {
            return Err(non_loopback_error());
        }
        if surface.tools().is_empty()
            || surface.tools().len() > MAX_SURFACE_TOOLS
            || surface.tools().iter().any(|tool| tool.side_effects)
        {
            return Err(EvaError::permission_denied(
                "local MCP HTTP server may expose only bounded side-effect-free tools",
            )
            .with_provider_code("mcp_server_surface_not_read_only"));
        }
        let expected_host = local_addr.to_string();
        let expected_origin = format!("http://{expected_host}");
        if config
            .allowed_origins
            .iter()
            .any(|origin| origin != &expected_origin)
        {
            return Err(EvaError::permission_denied(
                "MCP HTTP server allowed origins must exactly match its bound origin",
            )
            .with_provider_code("mcp_server_origin_not_bound"));
        }
        listener.set_nonblocking(true).map_err(|error| {
            EvaError::unavailable("failed to configure MCP HTTP server listener")
                .with_provider_code("mcp_server_listener_invalid")
                .with_context("io_error", error.to_string())
        })?;
        let shutdown = McpServerShutdownHandle {
            requested: Arc::new(AtomicBool::new(false)),
            active_stream: Arc::new(Mutex::new(None)),
        };
        Ok(Self {
            listener,
            local_addr,
            expected_host,
            config,
            surface,
            handler,
            sessions: BTreeMap::new(),
            shutdown,
            report: McpServerServeReport::default(),
        })
    }

    /// Return the actual bound loopback address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Return a cooperative stop signal for a server running on another thread.
    pub fn shutdown_handle(&self) -> McpServerShutdownHandle {
        self.shutdown.clone()
    }

    /// Serve one request per connection until shutdown is requested.
    pub fn serve(mut self) -> Result<McpServerServeReport, EvaError> {
        self.report.audit.extend([
            "mcp.server.transport:streamable_http".to_owned(),
            "mcp.server.bind:loopback".to_owned(),
            "mcp.server.tools:read_only".to_owned(),
            "mcp.server.origin:explicit_or_absent".to_owned(),
        ]);
        while !self.shutdown.is_shutdown_requested() {
            match self.listener.accept() {
                Ok((stream, peer)) => {
                    self.report.accepted_connections =
                        self.report.accepted_connections.saturating_add(1);
                    self.handle_connection(stream, peer);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(self.config.accept_poll_interval);
                }
                Err(error) => {
                    self.close_all_sessions();
                    return Err(EvaError::unavailable("MCP HTTP server accept failed")
                        .with_provider_code("mcp_server_accept_failed")
                        .with_context("io_error", error.to_string()));
                }
            }
        }
        self.close_all_sessions();
        self.report.dangling_sessions = self.sessions.len();
        self.report
            .audit
            .push("mcp.server.shutdown:complete".to_owned());
        Ok(self.report)
    }

    fn close_all_sessions(&mut self) {
        self.report.sessions_closed_on_shutdown = self
            .report
            .sessions_closed_on_shutdown
            .saturating_add(self.sessions.len());
        self.sessions.clear();
    }
}

fn non_loopback_error() -> EvaError {
    EvaError::permission_denied("MCP HTTP server must bind a numeric loopback address")
        .with_provider_code("mcp_server_bind_not_loopback")
}

fn validate_origin_literal(origin: &str) -> Result<(), EvaError> {
    let authority = origin.strip_prefix("http://").ok_or_else(|| {
        EvaError::invalid_argument("MCP HTTP server origin must use explicit http scheme")
            .with_provider_code("mcp_server_origin_invalid")
    })?;
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@'])
        || !authority.is_ascii()
        || authority.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
    {
        return Err(EvaError::invalid_argument(
            "MCP HTTP server origin must be an exact bounded authority",
        )
        .with_provider_code("mcp_server_origin_invalid"));
    }
    Ok(())
}

struct HttpRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: String,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

#[derive(Debug)]
struct HttpReadFailure {
    status: u16,
    message: &'static str,
    allow: Option<&'static str>,
}

impl HttpReadFailure {
    fn bad_request(message: &'static str) -> Self {
        Self {
            status: 400,
            message,
            allow: None,
        }
    }

    fn timeout() -> Self {
        Self {
            status: 408,
            message: "request did not complete within the server deadline",
            allow: None,
        }
    }

    fn too_large(status: u16, message: &'static str) -> Self {
        Self {
            status,
            message,
            allow: None,
        }
    }

    fn rejected(status: u16, message: &'static str) -> Self {
        Self {
            status,
            message,
            allow: None,
        }
    }

    fn method_not_allowed() -> Self {
        Self {
            status: 405,
            message: "HTTP method is not supported",
            allow: Some("POST, DELETE"),
        }
    }

    fn into_response(self) -> HttpResponse {
        let response = HttpResponse::http_error(self.status, self.message);
        match self.allow {
            Some(allow) => response.with_allow(allow),
            None => response,
        }
    }
}

struct HttpResponse {
    status: u16,
    body: String,
    content_type: Option<&'static str>,
    session_id: Option<String>,
    allow: Option<&'static str>,
    error: bool,
}

impl HttpResponse {
    fn json(status: u16, body: String) -> Self {
        Self {
            status,
            body,
            content_type: Some("application/json"),
            session_id: None,
            allow: None,
            error: false,
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
            content_type: None,
            session_id: None,
            allow: None,
            error: false,
        }
    }

    fn http_error(status: u16, message: &'static str) -> Self {
        let mut response = Self::json(status, json_rpc_error_body(None, -32600, message));
        response.error = true;
        response
    }

    fn rpc_error(id: Option<&ValidatedRequestId>, code: i64, message: &'static str) -> Self {
        let mut response = Self::json(200, json_rpc_error_body(id, code, message));
        response.error = true;
        response
    }

    fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    fn with_allow(mut self, methods: &'static str) -> Self {
        self.allow = Some(methods);
        self
    }
}

enum ServerAction {
    None,
    Create {
        session_id: String,
        session: ServerSession,
    },
    MarkReady {
        session_id: String,
    },
    Delete {
        session_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseWriteOutcome {
    Original,
    LimitFallback,
}

struct DispatchResult {
    response: HttpResponse,
    action: ServerAction,
}

impl DispatchResult {
    fn respond(response: HttpResponse) -> Self {
        Self {
            response,
            action: ServerAction::None,
        }
    }
}

impl<H: McpServerToolHandler> McpStreamableHttpServer<H> {
    fn handle_connection(&mut self, mut stream: TcpStream, peer: SocketAddr) {
        let Ok(_active_stream) = ActiveStreamGuard::register(&self.shutdown, &stream) else {
            self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
            return;
        };
        if self.shutdown.is_shutdown_requested() {
            return;
        }
        let Some(deadline) = Instant::now().checked_add(self.config.io_timeout) else {
            self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
            return;
        };
        if !peer.ip().is_loopback() {
            self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
            let _ = write_http_response(
                &mut stream,
                HttpResponse::http_error(403, "loopback client required"),
                self.config.response_limit_bytes,
                deadline,
            );
            return;
        }
        let request =
            match read_http_request(&mut stream, &self.config, &self.expected_host, deadline) {
                Ok(Some(request)) => request,
                Ok(None) => return,
                Err(failure) => {
                    self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
                    let _ = write_http_response(
                        &mut stream,
                        failure.into_response(),
                        self.config.response_limit_bytes,
                        deadline,
                    );
                    return;
                }
            };
        if self.shutdown.is_shutdown_requested() {
            return;
        }
        if remaining_io_budget(deadline, "request deadline elapsed").is_err() {
            self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
            return;
        }
        self.report.requests = self.report.requests.saturating_add(1);
        let dispatched = self.dispatch(request, peer.ip(), deadline);
        if dispatched.response.error {
            self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
        }
        if self.shutdown.is_shutdown_requested() {
            return;
        }
        let write_result = write_http_response(
            &mut stream,
            dispatched.response,
            self.config.response_limit_bytes,
            deadline,
        );
        self.commit_action_after_response(dispatched.action, write_result);
    }

    fn commit_action_after_response(
        &mut self,
        action: ServerAction,
        write_result: io::Result<ResponseWriteOutcome>,
    ) {
        match write_result {
            Ok(ResponseWriteOutcome::Original) if !self.shutdown.is_shutdown_requested() => {
                self.apply_action(action);
            }
            Ok(ResponseWriteOutcome::Original) => {}
            Ok(ResponseWriteOutcome::LimitFallback) | Err(_) => {
                self.report.protocol_errors = self.report.protocol_errors.saturating_add(1);
            }
        }
    }

    fn apply_action(&mut self, action: ServerAction) {
        match action {
            ServerAction::None => {}
            ServerAction::Create {
                session_id,
                session,
            } => {
                self.sessions.insert(session_id, session);
                self.report.sessions_created = self.report.sessions_created.saturating_add(1);
            }
            ServerAction::MarkReady { session_id } => {
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    session.phase = SessionPhase::Ready;
                }
            }
            ServerAction::Delete { session_id } => {
                if self.sessions.remove(&session_id).is_some() {
                    self.report.sessions_deleted = self.report.sessions_deleted.saturating_add(1);
                }
            }
        }
    }
}

fn read_http_request(
    stream: &mut TcpStream,
    config: &McpStreamableHttpServerConfig,
    expected_host: &str,
    deadline: Instant,
) -> Result<Option<HttpRequest>, HttpReadFailure> {
    let mut reader = BufReader::new(DeadlineReader { stream, deadline });
    let mut consumed = 0_usize;
    let Some(request_line) =
        read_crlf_line(&mut reader, &mut consumed, config.header_limit_bytes, 431)?
    else {
        return Ok(None);
    };
    let request_line = std::str::from_utf8(&request_line)
        .map_err(|_| HttpReadFailure::bad_request("request line must be ASCII"))?;
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || method.is_empty()
        || target.is_empty()
        || version != "HTTP/1.1"
        || !method.bytes().all(is_http_token_byte)
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        || !target.is_ascii()
        || target.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
    {
        return Err(HttpReadFailure::bad_request("request line is invalid"));
    }

    let mut headers = BTreeMap::new();
    for _ in 0..=MAX_HEADER_COUNT {
        let line = read_crlf_line(&mut reader, &mut consumed, config.header_limit_bytes, 431)?
            .ok_or_else(|| HttpReadFailure::bad_request("request headers ended early"))?;
        if line.is_empty() {
            break;
        }
        if headers.len() == MAX_HEADER_COUNT {
            return Err(HttpReadFailure::too_large(431, "too many request headers"));
        }
        if line.first().is_some_and(u8::is_ascii_whitespace) {
            return Err(HttpReadFailure::bad_request(
                "folded headers are not supported",
            ));
        }
        let colon = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or_else(|| HttpReadFailure::bad_request("request header is malformed"))?;
        let name = &line[..colon];
        let value = trim_ascii_spaces(&line[colon + 1..]);
        if name.is_empty()
            || !name.iter().copied().all(is_http_token_byte)
            || value.iter().any(|byte| !(0x20..=0x7e).contains(byte))
        {
            return Err(HttpReadFailure::bad_request("request header is invalid"));
        }
        let name = std::str::from_utf8(name)
            .map_err(|_| HttpReadFailure::bad_request("request header name is invalid"))?
            .to_ascii_lowercase();
        let value = std::str::from_utf8(value)
            .map_err(|_| HttpReadFailure::bad_request("request header value is invalid"))?
            .to_owned();
        if headers.insert(name, value).is_some() {
            return Err(HttpReadFailure::bad_request("duplicate request header"));
        }
    }
    if !headers.contains_key("host") {
        return Err(HttpReadFailure::bad_request("Host header is required"));
    }
    if headers.contains_key("transfer-encoding") {
        return Err(HttpReadFailure::bad_request(
            "request transfer encoding is not supported",
        ));
    }
    if [
        "content-encoding",
        "expect",
        "proxy-connection",
        "te",
        "trailer",
        "upgrade",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return Err(HttpReadFailure::bad_request(
            "request uses an unsupported framing control header",
        ));
    }
    let content_length = headers
        .get("content-length")
        .map(|value| parse_request_content_length(value))
        .transpose()?
        .unwrap_or(0);
    if method == "POST" && !headers.contains_key("content-length") {
        return Err(HttpReadFailure {
            status: 411,
            message: "Content-Length is required for POST",
            allow: None,
        });
    }
    validate_request_head(
        method,
        target,
        &headers,
        content_length,
        config,
        expected_host,
    )?;
    if content_length > config.request_limit_bytes {
        return Err(HttpReadFailure::too_large(
            413,
            "request body exceeded the server limit",
        ));
    }
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).map_err(map_request_io_error)?;
    let body = String::from_utf8(body)
        .map_err(|_| HttpReadFailure::bad_request("request body must be UTF-8 JSON"))?;
    Ok(Some(HttpRequest {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body,
    }))
}

fn validate_request_head(
    method: &str,
    target: &str,
    headers: &BTreeMap<String, String>,
    content_length: usize,
    config: &McpStreamableHttpServerConfig,
    expected_host: &str,
) -> Result<(), HttpReadFailure> {
    if target != config.path {
        return Err(HttpReadFailure::rejected(404, "endpoint not found"));
    }
    if headers.get("host").map(String::as_str) != Some(expected_host) {
        return Err(HttpReadFailure::rejected(
            421,
            "Host does not match the bound listener",
        ));
    }
    if headers
        .get("origin")
        .is_some_and(|origin| !config.allowed_origins.contains(origin))
    {
        return Err(HttpReadFailure::rejected(
            403,
            "Origin is not explicitly allowed",
        ));
    }
    match method {
        "POST" => {
            if !headers
                .get("content-type")
                .is_some_and(|value| is_json_content_type(value))
            {
                return Err(HttpReadFailure::rejected(
                    415,
                    "Content-Type must be application/json",
                ));
            }
            if !headers
                .get("accept")
                .is_some_and(|value| accepts_json(value))
            {
                return Err(HttpReadFailure::rejected(
                    406,
                    "Accept must allow application/json",
                ));
            }
        }
        "DELETE" => {
            if content_length != 0 {
                return Err(HttpReadFailure::bad_request(
                    "DELETE request body must be empty",
                ));
            }
        }
        _ => return Err(HttpReadFailure::method_not_allowed()),
    }
    Ok(())
}

fn read_crlf_line(
    reader: &mut impl BufRead,
    consumed: &mut usize,
    limit: usize,
    too_large_status: u16,
) -> Result<Option<Vec<u8>>, HttpReadFailure> {
    if *consumed >= limit {
        return Err(HttpReadFailure::too_large(
            too_large_status,
            "request headers exceeded the server limit",
        ));
    }
    let remaining = limit - *consumed;
    let mut line = Vec::new();
    let read = reader
        .take((remaining + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(map_request_io_error)?;
    if read == 0 {
        return Ok(None);
    }
    if read > remaining {
        return Err(HttpReadFailure::too_large(
            too_large_status,
            "request headers exceeded the server limit",
        ));
    }
    *consumed += read;
    if !line.ends_with(b"\r\n") {
        return Err(HttpReadFailure::bad_request(
            "request lines must use CRLF framing",
        ));
    }
    line.truncate(line.len() - 2);
    Ok(Some(line))
}

fn map_request_io_error(error: io::Error) -> HttpReadFailure {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        HttpReadFailure::timeout()
    } else {
        HttpReadFailure::bad_request("request body ended before Content-Length")
    }
}

fn parse_request_content_length(value: &str) -> Result<usize, HttpReadFailure> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HttpReadFailure::bad_request("Content-Length is invalid"));
    }
    value
        .parse::<usize>()
        .map_err(|_| HttpReadFailure::bad_request("Content-Length is invalid"))
}

fn trim_ascii_spaces(mut value: &[u8]) -> &[u8] {
    while value.first() == Some(&b' ') {
        value = &value[1..];
    }
    while value.last() == Some(&b' ') {
        value = &value[..value.len() - 1];
    }
    value
}

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

struct DeadlineReader<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "read deadline elapsed"))?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ValidatedRequestId {
    raw: String,
}

impl fmt::Debug for ValidatedRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValidatedRequestId([REDACTED])")
    }
}

struct ParsedRequestEnvelope<'a> {
    id: Option<ValidatedRequestId>,
    method: String,
    params: Option<&'a str>,
}

#[derive(Debug)]
struct RpcFailure {
    id: Option<ValidatedRequestId>,
    code: i64,
    message: &'static str,
}

impl RpcFailure {
    fn parse(message: &'static str) -> Self {
        Self {
            id: None,
            code: -32700,
            message,
        }
    }

    fn invalid(id: Option<ValidatedRequestId>, message: &'static str) -> Self {
        Self {
            id,
            code: -32600,
            message,
        }
    }
}

impl<H: McpServerToolHandler> McpStreamableHttpServer<H> {
    fn dispatch(
        &mut self,
        request: HttpRequest,
        peer_ip: IpAddr,
        deadline: Instant,
    ) -> DispatchResult {
        if request.target != self.config.path {
            return DispatchResult::respond(HttpResponse::http_error(404, "endpoint not found"));
        }
        if request.header("host") != Some(self.expected_host.as_str()) {
            return DispatchResult::respond(HttpResponse::http_error(
                421,
                "Host does not match the bound listener",
            ));
        }
        if let Some(origin) = request.header("origin") {
            if !self.config.allowed_origins.contains(origin) {
                return DispatchResult::respond(HttpResponse::http_error(
                    403,
                    "Origin is not explicitly allowed",
                ));
            }
        }
        match request.method.as_str() {
            "POST" => self.dispatch_post(request, peer_ip, deadline),
            "DELETE" => self.dispatch_delete(request, peer_ip),
            _ => DispatchResult::respond(
                HttpResponse::http_error(405, "HTTP method is not supported")
                    .with_allow("POST, DELETE"),
            ),
        }
    }

    fn dispatch_post(
        &mut self,
        request: HttpRequest,
        peer_ip: IpAddr,
        deadline: Instant,
    ) -> DispatchResult {
        if !request
            .header("content-type")
            .is_some_and(is_json_content_type)
        {
            return DispatchResult::respond(HttpResponse::http_error(
                415,
                "Content-Type must be application/json",
            ));
        }
        if !request.header("accept").is_some_and(accepts_json) {
            return DispatchResult::respond(HttpResponse::http_error(
                406,
                "Accept must allow application/json",
            ));
        }
        let envelope = match parse_request_envelope(&request.body) {
            Ok(envelope) => envelope,
            Err(failure) => {
                return DispatchResult::respond(HttpResponse::rpc_error(
                    failure.id.as_ref(),
                    failure.code,
                    failure.message,
                ));
            }
        };
        if envelope.id.is_none() && envelope.method == "initialize" {
            return DispatchResult::respond(HttpResponse::empty(202));
        }
        if envelope.method == "initialize" {
            return self.dispatch_initialize(&request, envelope, peer_ip);
        }
        let (session_id, phase) = match self.controlled_session(&request, peer_ip) {
            Ok(session) => session,
            Err(response) => return DispatchResult::respond(response),
        };
        if envelope.id.is_none() && envelope.method != "notifications/initialized" {
            return DispatchResult::respond(HttpResponse::empty(202).with_session(session_id));
        }
        match envelope.method.as_str() {
            "notifications/initialized" => self.dispatch_initialized(envelope, session_id, phase),
            "tools/list" => self.dispatch_tools_list(envelope, session_id, phase),
            "tools/call" => self.dispatch_tools_call(envelope, session_id, phase, deadline),
            _ => DispatchResult::respond(
                HttpResponse::rpc_error(envelope.id.as_ref(), -32601, "method is not supported")
                    .with_session(session_id),
            ),
        }
    }

    fn dispatch_initialize(
        &mut self,
        request: &HttpRequest,
        envelope: ParsedRequestEnvelope<'_>,
        peer_ip: IpAddr,
    ) -> DispatchResult {
        let Some(id) = envelope.id.as_ref() else {
            return DispatchResult::respond(HttpResponse::rpc_error(
                None,
                -32600,
                "initialize requires a request id",
            ));
        };
        if request.header("mcp-session-id").is_some()
            || request.header("mcp-protocol-version").is_some()
        {
            return DispatchResult::respond(HttpResponse::rpc_error(
                Some(id),
                -32600,
                "initialize must not carry session control headers",
            ));
        }
        if let Err(message) = validate_initialize_params(envelope.params) {
            return DispatchResult::respond(HttpResponse::rpc_error(Some(id), -32602, message));
        }
        if self.sessions.len() >= self.config.max_sessions {
            return DispatchResult::respond(HttpResponse::http_error(
                503,
                "server session capacity is exhausted",
            ));
        }
        let session_id = match new_session_id() {
            Ok(session_id) => session_id,
            Err(_) => {
                return DispatchResult::respond(HttpResponse::rpc_error(
                    Some(id),
                    -32603,
                    "server could not create a session",
                ));
            }
        };
        let body = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":{},\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"eva\",\"version\":{}}}}}}}",
            id.raw,
            json_string(DEFAULT_PROTOCOL_VERSION),
            json_string(env!("CARGO_PKG_VERSION"))
        );
        DispatchResult {
            response: HttpResponse::json(200, body).with_session(session_id.clone()),
            action: ServerAction::Create {
                session_id,
                session: ServerSession {
                    peer_ip,
                    phase: SessionPhase::AwaitingInitialized,
                },
            },
        }
    }

    fn dispatch_initialized(
        &self,
        envelope: ParsedRequestEnvelope<'_>,
        session_id: String,
        phase: SessionPhase,
    ) -> DispatchResult {
        if envelope.id.is_some()
            || !params_are_empty(envelope.params)
            || phase != SessionPhase::AwaitingInitialized
        {
            return DispatchResult::respond(
                HttpResponse::http_error(400, "initialized notification is out of phase")
                    .with_session(session_id),
            );
        }
        DispatchResult {
            response: HttpResponse::empty(202).with_session(session_id.clone()),
            action: ServerAction::MarkReady { session_id },
        }
    }

    fn dispatch_tools_list(
        &self,
        envelope: ParsedRequestEnvelope<'_>,
        session_id: String,
        phase: SessionPhase,
    ) -> DispatchResult {
        let Some(id) = envelope.id.as_ref() else {
            return DispatchResult::respond(
                HttpResponse::rpc_error(None, -32600, "tools/list requires a request id")
                    .with_session(session_id),
            );
        };
        if phase != SessionPhase::Ready {
            return DispatchResult::respond(
                HttpResponse::rpc_error(
                    Some(id),
                    -32002,
                    "session is awaiting notifications/initialized",
                )
                .with_session(session_id),
            );
        }
        if !params_are_empty(envelope.params) {
            return DispatchResult::respond(
                HttpResponse::rpc_error(Some(id), -32602, "tools/list params are invalid")
                    .with_session(session_id),
            );
        }
        let tools = self
            .surface
            .tools()
            .iter()
            .map(|tool| {
                format!(
                    "{{\"name\":{},\"description\":{},\"inputSchema\":{},\"annotations\":{{\"readOnlyHint\":true}}}}",
                    json_string(&tool.name),
                    json_string(&tool.description),
                    tool.input_schema
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{}]}}}}",
            id.raw, tools
        );
        DispatchResult::respond(HttpResponse::json(200, body).with_session(session_id))
    }

    fn dispatch_tools_call(
        &mut self,
        envelope: ParsedRequestEnvelope<'_>,
        session_id: String,
        phase: SessionPhase,
        deadline: Instant,
    ) -> DispatchResult {
        let Some(id) = envelope.id.as_ref() else {
            return DispatchResult::respond(
                HttpResponse::rpc_error(None, -32600, "tools/call requires a request id")
                    .with_session(session_id),
            );
        };
        if phase != SessionPhase::Ready {
            return DispatchResult::respond(
                HttpResponse::rpc_error(
                    Some(id),
                    -32002,
                    "session is awaiting notifications/initialized",
                )
                .with_session(session_id),
            );
        }
        let params = match envelope
            .params
            .ok_or(())
            .and_then(|params| parse_json_object_fields(params).map_err(|_| ()))
        {
            Ok(params) => params,
            Err(()) => {
                return DispatchResult::respond(
                    HttpResponse::rpc_error(Some(id), -32602, "tools/call params are invalid")
                        .with_session(session_id),
                );
            }
        };
        let tool_name = match params
            .get("name")
            .ok_or(())
            .and_then(|value| parse_json_string(value).map_err(|_| ()))
            .and_then(|name| {
                (!name.is_empty()
                    && name.len() <= MAX_METHOD_BYTES
                    && !name.chars().any(char::is_control))
                .then_some(name)
                .ok_or(())
            }) {
            Ok(name) => name,
            Err(()) => {
                return DispatchResult::respond(
                    HttpResponse::rpc_error(Some(id), -32602, "tool name is invalid")
                        .with_session(session_id),
                );
            }
        };

        // This is the critical proxy boundary. Do not inspect `arguments` or
        // call the handler until the direct tool name passes the surface gate.
        if self.surface.require_tool(&tool_name).is_err() {
            self.report.blocked_tool_calls = self.report.blocked_tool_calls.saturating_add(1);
            return DispatchResult::respond(
                HttpResponse::rpc_error(Some(id), -32602, "tool is not explicitly exposed")
                    .with_session(session_id),
            );
        }
        if params
            .keys()
            .any(|name| !matches!(name.as_str(), "name" | "arguments"))
        {
            return DispatchResult::respond(
                HttpResponse::rpc_error(Some(id), -32602, "tools/call params are invalid")
                    .with_session(session_id),
            );
        }
        let arguments = params.get("arguments").copied().unwrap_or("{}");
        if self
            .surface
            .validate_tool_arguments(&tool_name, arguments)
            .is_err()
        {
            return DispatchResult::respond(
                HttpResponse::rpc_error(Some(id), -32602, "tool arguments are invalid")
                    .with_session(session_id),
            );
        }
        let call = match McpServerToolCall::admitted(&tool_name, arguments) {
            Ok(call) => call,
            Err(_) => {
                return DispatchResult::respond(
                    HttpResponse::rpc_error(Some(id), -32602, "tool arguments are invalid")
                        .with_session(session_id),
                );
            }
        };
        if self.shutdown.is_shutdown_requested()
            || remaining_io_budget(deadline, "request deadline elapsed").is_err()
        {
            return DispatchResult::respond(
                HttpResponse::http_error(503, "server is shutting down").with_session(session_id),
            );
        }
        self.report.handler_calls = self.report.handler_calls.saturating_add(1);
        let result = match self.handler.call_tool(call) {
            Ok(result) => result,
            Err(_) => {
                return DispatchResult::respond(
                    HttpResponse::rpc_error(Some(id), -32603, "tool handler failed")
                        .with_session(session_id),
                );
            }
        };
        if !tool_result_fits(id, &result, self.config.response_limit_bytes) {
            return DispatchResult::respond(
                HttpResponse::rpc_error(Some(id), -32603, "tool result exceeded server limit")
                    .with_session(session_id),
            );
        }
        let body = tool_result_body(id, &result);
        DispatchResult::respond(HttpResponse::json(200, body).with_session(session_id))
    }

    fn dispatch_delete(&self, request: HttpRequest, peer_ip: IpAddr) -> DispatchResult {
        if !request.body.is_empty() {
            return DispatchResult::respond(HttpResponse::http_error(
                400,
                "DELETE request body must be empty",
            ));
        }
        let (session_id, _) = match self.controlled_session(&request, peer_ip) {
            Ok(session) => session,
            Err(response) => return DispatchResult::respond(response),
        };
        DispatchResult {
            response: HttpResponse::empty(204).with_session(session_id.clone()),
            action: ServerAction::Delete { session_id },
        }
    }

    fn controlled_session(
        &self,
        request: &HttpRequest,
        peer_ip: IpAddr,
    ) -> Result<(String, SessionPhase), HttpResponse> {
        if request.header("mcp-protocol-version") != Some(DEFAULT_PROTOCOL_VERSION) {
            return Err(HttpResponse::http_error(
                400,
                "MCP-Protocol-Version is missing or unsupported",
            ));
        }
        let session_id = request
            .header("mcp-session-id")
            .filter(|value| valid_session_id(value))
            .ok_or_else(|| HttpResponse::http_error(400, "Mcp-Session-Id is missing or invalid"))?;
        let Some(session) = self.sessions.get(session_id) else {
            return Err(HttpResponse::http_error(404, "MCP session was not found"));
        };
        if session.peer_ip != peer_ip {
            return Err(HttpResponse::http_error(404, "MCP session was not found"));
        }
        Ok((session_id.to_owned(), session.phase))
    }
}

fn parse_request_envelope(body: &str) -> Result<ParsedRequestEnvelope<'_>, RpcFailure> {
    let fields = parse_json_object_fields(body)
        .map_err(|_| RpcFailure::parse("request body is not one complete JSON object"))?;
    let id = fields
        .get("id")
        .map(|value| validate_request_id(value))
        .transpose()
        .map_err(|_| RpcFailure::invalid(None, "request id is invalid"))?;
    if fields
        .keys()
        .any(|name| !matches!(name.as_str(), "jsonrpc" | "id" | "method" | "params"))
    {
        return Err(RpcFailure::invalid(
            id,
            "request contains an unknown envelope field",
        ));
    }
    let version = fields
        .get("jsonrpc")
        .ok_or_else(|| RpcFailure::invalid(id.clone(), "jsonrpc version is missing"))
        .and_then(|value| {
            parse_json_string(value)
                .map_err(|_| RpcFailure::invalid(id.clone(), "jsonrpc version is invalid"))
        })?;
    if version != "2.0" {
        return Err(RpcFailure::invalid(id, "jsonrpc version is unsupported"));
    }
    let method = fields
        .get("method")
        .ok_or_else(|| RpcFailure::invalid(id.clone(), "request method is missing"))
        .and_then(|value| {
            parse_json_string(value)
                .map_err(|_| RpcFailure::invalid(id.clone(), "request method is invalid"))
        })?;
    if method.is_empty() || method.len() > MAX_METHOD_BYTES || method.chars().any(char::is_control)
    {
        return Err(RpcFailure::invalid(id, "request method is invalid"));
    }
    Ok(ParsedRequestEnvelope {
        id,
        method,
        params: fields.get("params").copied(),
    })
}

fn validate_request_id(value: &str) -> Result<ValidatedRequestId, ()> {
    if value.starts_with('"') {
        let decoded = parse_json_string(value).map_err(|_| ())?;
        if decoded.is_empty()
            || decoded.len() > MAX_ID_STRING_BYTES
            || decoded.chars().any(char::is_control)
        {
            return Err(());
        }
    } else {
        value.parse::<u64>().map_err(|_| ())?;
    }
    Ok(ValidatedRequestId {
        raw: value.to_owned(),
    })
}

fn validate_initialize_params(params: Option<&str>) -> Result<(), &'static str> {
    let params = params.ok_or("initialize params are required")?;
    let fields = parse_json_object_fields(params).map_err(|_| "initialize params are invalid")?;
    if fields.keys().any(|name| {
        !matches!(
            name.as_str(),
            "protocolVersion" | "capabilities" | "clientInfo"
        )
    }) {
        return Err("initialize params contain an unknown field");
    }
    let version = fields
        .get("protocolVersion")
        .ok_or("initialize protocolVersion is required")
        .and_then(|value| {
            parse_json_string(value).map_err(|_| "initialize protocolVersion is invalid")
        })?;
    if version != DEFAULT_PROTOCOL_VERSION {
        return Err("initialize protocolVersion is unsupported");
    }
    let capabilities = fields
        .get("capabilities")
        .ok_or("initialize capabilities are required")?;
    parse_json_object_fields(capabilities).map_err(|_| "initialize capabilities are invalid")?;
    let client_info = fields
        .get("clientInfo")
        .ok_or("initialize clientInfo is required")?;
    let client_fields =
        parse_json_object_fields(client_info).map_err(|_| "initialize clientInfo is invalid")?;
    if client_fields
        .keys()
        .any(|name| !matches!(name.as_str(), "name" | "version"))
    {
        return Err("initialize clientInfo contains an unknown field");
    }
    for name in ["name", "version"] {
        let value = client_fields
            .get(name)
            .ok_or("initialize clientInfo is incomplete")
            .and_then(|value| {
                parse_json_string(value).map_err(|_| "initialize clientInfo is invalid")
            })?;
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err("initialize clientInfo is invalid");
        }
    }
    Ok(())
}

fn params_are_empty(params: Option<&str>) -> bool {
    params
        .map(|params| {
            parse_json_object_fields(params)
                .map(|fields| fields.is_empty())
                .unwrap_or(false)
        })
        .unwrap_or(true)
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_ID_BYTES
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn new_session_id() -> Result<String, EvaError> {
    let mut random = [0_u8; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut random)
        .map_err(|_| {
            EvaError::unavailable("secure MCP session randomness was unavailable")
                .with_provider_code("mcp_server_session_random_failed")
        })?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(random.len() * 2);
    for byte in random {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(encoded)
}

fn tool_result_body(id: &ValidatedRequestId, result: &McpServerToolResult) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":{}}}],\"isError\":{}}}}}",
        id.raw,
        json_string(result.text_content()),
        result.is_error_result()
    )
}

fn tool_result_fits(
    id: &ValidatedRequestId,
    result: &McpServerToolResult,
    response_limit_bytes: usize,
) -> bool {
    const FIXED_BYTES: usize =
        b"{\"jsonrpc\":\"2.0\",\"id\":,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"\"}],\"isError\":false}}".len();
    let escaped_bytes = result
        .text_content()
        .chars()
        .try_fold(0_usize, |total, character| {
            let bytes = match character {
                '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
                value if value.is_control() => 6,
                value => value.len_utf8(),
            };
            total.checked_add(bytes)
        });
    escaped_bytes
        .and_then(|escaped| FIXED_BYTES.checked_add(id.raw.len())?.checked_add(escaped))
        .is_some_and(|size| size <= response_limit_bytes)
}

fn json_rpc_error_body(
    id: Option<&ValidatedRequestId>,
    code: i64,
    message: &'static str,
) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":{},\"message\":{}}}}}",
        id.map(|id| id.raw.as_str()).unwrap_or("null"),
        code,
        json_string(message)
    )
}

fn is_json_content_type(value: &str) -> bool {
    let mut parts = value.split(';');
    if parts
        .next()
        .map(str::trim)
        .is_none_or(|media_type| !media_type.eq_ignore_ascii_case("application/json"))
    {
        return false;
    }
    parts.all(|parameter| {
        let parameter = parameter.trim();
        parameter.eq_ignore_ascii_case("charset=utf-8")
            || parameter.eq_ignore_ascii_case("charset=\"utf-8\"")
    })
}

fn accepts_json(value: &str) -> bool {
    let mut selected = None;
    'entries: for entry in value.split(',') {
        let mut parts = entry.split(';');
        let media_type = parts.next().unwrap_or_default().trim();
        let specificity = if media_type.eq_ignore_ascii_case("application/json") {
            2_u8
        } else if media_type.eq_ignore_ascii_case("application/*") {
            1
        } else if media_type == "*/*" {
            0
        } else {
            continue;
        };
        let mut quality = 1_000_u16;
        let mut quality_seen = false;
        for parameter in parts {
            let Some((name, value)) = parameter.trim().split_once('=') else {
                continue;
            };
            if !name.trim().eq_ignore_ascii_case("q") {
                continue;
            }
            if quality_seen {
                continue 'entries;
            }
            quality_seen = true;
            let Some(parsed) = parse_http_quality(value.trim()) else {
                continue 'entries;
            };
            quality = parsed;
        }
        match selected {
            Some((selected_specificity, selected_quality))
                if selected_specificity > specificity
                    || (selected_specificity == specificity && selected_quality >= quality) => {}
            _ => selected = Some((specificity, quality)),
        }
    }
    selected.is_some_and(|(_, quality)| quality > 0)
}

fn parse_http_quality(value: &str) -> Option<u16> {
    if value == "0" {
        return Some(0);
    }
    if value == "1" {
        return Some(1_000);
    }
    let (whole, fraction) = value.split_once('.')?;
    if fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match whole {
        "0" => {
            let padded = format!("{fraction:0<3}");
            padded.parse().ok()
        }
        "1" if fraction.bytes().all(|byte| byte == b'0') => Some(1_000),
        _ => None,
    }
}

fn write_http_response(
    stream: &mut TcpStream,
    mut response: HttpResponse,
    response_limit_bytes: usize,
    deadline: Instant,
) -> io::Result<ResponseWriteOutcome> {
    let outcome = if response.body.len() > response_limit_bytes {
        response = HttpResponse::http_error(500, "response exceeded the server limit");
        ResponseWriteOutcome::LimitFallback
    } else {
        ResponseWriteOutcome::Original
    };
    let reason = http_reason(response.status);
    let mut wire = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        response.status, reason
    );
    if let Some(session_id) = response
        .session_id
        .as_deref()
        .filter(|id| valid_session_id(id))
    {
        wire.push_str("Mcp-Session-Id: ");
        wire.push_str(session_id);
        wire.push_str("\r\n");
    }
    if let Some(allow) = response.allow {
        wire.push_str("Allow: ");
        wire.push_str(allow);
        wire.push_str("\r\n");
    }
    if let Some(content_type) = response.content_type {
        wire.push_str("Content-Type: ");
        wire.push_str(content_type);
        wire.push_str("\r\n");
    }
    if response.status != 204 {
        wire.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    wire.push_str("\r\n");
    wire.push_str(&response.body);
    write_all_with_deadline(stream, wire.as_bytes(), deadline).map(|()| outcome)
}

fn write_all_with_deadline(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = remaining_io_budget(deadline, "write deadline elapsed")?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write MCP HTTP response",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let remaining = remaining_io_budget(deadline, "write deadline elapsed")?;
    stream.set_write_timeout(Some(remaining))?;
    stream.flush()
}

fn remaining_io_budget(deadline: Instant, message: &'static str) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, message))
}

fn http_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{McpStreamableHttpConfig, McpStreamableHttpSession};
    use std::net::Shutdown;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    const TEST_CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

    #[derive(Clone)]
    struct RecordingHandler {
        calls: Arc<AtomicUsize>,
    }

    impl McpServerToolHandler for RecordingHandler {
        fn call_tool(
            &mut self,
            request: McpServerToolCall<'_>,
        ) -> Result<McpServerToolResult, EvaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match request.tool() {
                "adapter.list" => {
                    request.require_only_arguments(&[])?;
                    Ok(McpServerToolResult::text("adapter-count:0"))
                }
                "adapter.probe" => {
                    request.require_only_arguments(&["adapter_id"])?;
                    let adapter_id = request.required_string_argument("adapter_id")?;
                    match adapter_id.as_str() {
                        "fail-secret" => Err(EvaError::internal(
                            "private handler path C:\\secrets\\credential.txt",
                        )),
                        "huge" => Ok(McpServerToolResult::text("x".repeat(4096))),
                        "slow" => {
                            thread::sleep(Duration::from_millis(120));
                            Ok(McpServerToolResult::text("slow-result"))
                        }
                        _ => Ok(McpServerToolResult::text(format!(
                            "probe-ready:{adapter_id}"
                        ))),
                    }
                }
                _ => Err(EvaError::internal("unreachable hidden handler")),
            }
        }
    }

    fn spawn_server(
        config: McpStreamableHttpServerConfig,
    ) -> (
        SocketAddr,
        McpServerShutdownHandle,
        Arc<AtomicUsize>,
        JoinHandle<McpServerServeReport>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        spawn_server_from_listener(listener, config)
    }

    fn spawn_server_from_listener(
        listener: TcpListener,
        config: McpStreamableHttpServerConfig,
    ) -> (
        SocketAddr,
        McpServerShutdownHandle,
        Arc<AtomicUsize>,
        JoinHandle<McpServerServeReport>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let server = McpStreamableHttpServer::from_listener(
            listener,
            config,
            EvaMcpServerSurface::v11_minimal(),
            RecordingHandler {
                calls: calls.clone(),
            },
        )
        .unwrap();
        let address = server.local_addr();
        let shutdown = server.shutdown_handle();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        let join = thread::spawn(move || {
            ready_sender.send(()).unwrap();
            server.serve().unwrap()
        });
        ready_receiver
            .recv_timeout(TEST_CLIENT_TIMEOUT)
            .expect("MCP test server thread did not become ready");
        (address, shutdown, calls, join)
    }

    fn connect_client(address: SocketAddr, output_limit_bytes: usize) -> McpStreamableHttpSession {
        let config =
            McpStreamableHttpConfig::legacy_http(format!("http://{address}{}", DEFAULT_PATH))
                .unwrap();
        McpStreamableHttpSession::new(
            config,
            BTreeMap::new(),
            TEST_CLIENT_TIMEOUT,
            output_limit_bytes,
        )
        .unwrap()
    }

    fn initialize_request(id: u64) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"initialize\",\"params\":{{\"protocolVersion\":{},\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"external-test-client\",\"version\":\"1\"}}}}}}",
            json_string(DEFAULT_PROTOCOL_VERSION)
        )
    }

    fn initialized_notification() -> &'static str {
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}"
    }

    fn ready_client(address: SocketAddr) -> McpStreamableHttpSession {
        let mut client = connect_client(address, 64 * 1024);
        client.initialize(1, &initialize_request(1)).unwrap();
        client.notify(initialized_notification()).unwrap();
        client
    }

    fn raw_request(address: SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request).unwrap();
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = Vec::new();
        match stream.read_to_end(&mut response) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::NotConnected
                ) => {}
            Err(error) => panic!("failed to read MCP test response: {error}"),
        }
        response
    }

    fn raw_headers_without_body(address: SocketAddr, request_head: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        stream.write_all(request_head.as_bytes()).unwrap();
        let mut response = String::new();
        match stream.read_to_string(&mut response) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::NotConnected
                ) => {}
            Err(error) => panic!("failed to read MCP test response head: {error}"),
        }
        response
    }

    fn raw_post(
        address: SocketAddr,
        body: &str,
        session_id: Option<&str>,
        host: Option<&str>,
        origin: Option<&str>,
    ) -> String {
        let default_host = address.to_string();
        let mut request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            DEFAULT_PATH,
            host.unwrap_or(&default_host),
            body.len()
        );
        if let Some(session_id) = session_id {
            request.push_str(&format!(
                "MCP-Protocol-Version: {}\r\nMcp-Session-Id: {}\r\n",
                DEFAULT_PROTOCOL_VERSION, session_id
            ));
        }
        if let Some(origin) = origin {
            request.push_str("Origin: ");
            request.push_str(origin);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        request.push_str(body);
        String::from_utf8(raw_request(address, request.as_bytes())).unwrap()
    }

    #[test]
    fn real_external_http_client_runs_full_server_lifecycle() {
        let (address, shutdown, calls, join) =
            spawn_server(McpStreamableHttpServerConfig::default());
        let mut client = connect_client(address, 64 * 1024);

        let initialized = client.initialize(1, &initialize_request(1)).unwrap();
        assert!(initialized.contains("\"protocolVersion\":\"2025-11-25\""));
        assert!(client.session_id().is_some());
        client.notify(initialized_notification()).unwrap();
        let listed = client
            .post(
                2,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}",
            )
            .unwrap();
        assert!(listed.contains("\"name\":\"adapter.list\""));
        assert!(listed.contains("\"inputSchema\":{\"type\":\"object\""));
        let called = client
            .post(
                3,
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"github-mcp\"}}}",
            )
            .unwrap();
        assert!(called.contains("probe-ready:github-mcp"));
        assert!(called.contains("\"isError\":false"));
        client.shutdown().unwrap();

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.requests, 5);
        assert_eq!(report.sessions_created, 1);
        assert_eq!(report.sessions_deleted, 1);
        assert_eq!(report.handler_calls, 1);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn hidden_tool_is_rejected_before_arguments_and_handler() {
        let (address, shutdown, calls, join) =
            spawn_server(McpStreamableHttpServerConfig::default());
        let mut client = ready_client(address);
        let denied = client
            .post(
                2,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"topic.publish\",\"arguments\":{\"secret\":\"opaque-value\"}}}",
            )
            .unwrap();

        assert!(denied.contains("tool is not explicitly exposed"));
        assert!(!denied.contains("topic.publish"));
        assert!(!denied.contains("opaque-value"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.blocked_tool_calls, 1);
        assert_eq!(report.handler_calls, 0);
        assert!(!format!("{report:?}").contains("topic.publish"));
    }

    #[test]
    fn schema_rejects_invalid_arguments_before_handler_and_errors_are_redacted() {
        let (address, shutdown, calls, join) =
            spawn_server(McpStreamableHttpServerConfig::default());
        let mut client = ready_client(address);
        for (id, params) in [
            (
                2,
                "{\"name\":\"adapter.list\",\"arguments\":{\"extra\":\"x\"}}",
            ),
            (3, "{\"name\":\"adapter.probe\",\"arguments\":{}}"),
            (
                4,
                "{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":1}}",
            ),
            (
                5,
                "{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"x\",\"extra\":\"y\"}}",
            ),
        ] {
            let response = client
                .post(
                    id,
                    &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{params}}}"
                    ),
                )
                .unwrap();
            assert!(response.contains("tool arguments are invalid"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let failed = client
            .post(
                6,
                "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"fail-secret\"}}}",
            )
            .unwrap();
        assert!(failed.contains("tool handler failed"));
        assert!(!failed.contains("credential.txt"));
        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.handler_calls, 1);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn string_id_is_echoed_as_validated_raw_json_token() {
        let (address, shutdown, _, join) = spawn_server(McpStreamableHttpServerConfig::default());
        let mut client = ready_client(address);
        let session_id = client.session_id().unwrap().to_owned();
        let body = "{\"jsonrpc\":\"2.0\",\"id\":\"opaque\\u002did\",\"method\":\"tools/list\",\"params\":{}}";
        let response = raw_post(address, body, Some(&session_id), None, None);

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("\"id\":\"opaque\\u002did\""));
        client.shutdown().unwrap();
        shutdown.shutdown();
        assert_eq!(join.join().unwrap().dangling_sessions, 0);
    }

    #[test]
    fn envelope_parser_rejects_batch_duplicates_nested_spoofs_and_bad_ids() {
        for body in [
            "[]",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"method\":\"tools/call\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"params\":{\"method\":\"tools/list\"}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}{}",
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"tools/list\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"tools/list\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":1.5,\"method\":\"tools/list\"}",
        ] {
            assert!(parse_request_envelope(body).is_err(), "{body}");
        }
        let secret_id = parse_request_envelope(
            "{\"jsonrpc\":\"2.0\",\"id\":\"private-request-id\",\"extra\":true,\"method\":\"tools/list\"}",
        )
        .err()
        .unwrap();
        assert!(!format!("{secret_id:?}").contains("private-request-id"));
    }

    #[test]
    fn notifications_receive_empty_202_without_session_or_handler_side_effects() {
        fn assert_empty_202(response: &str) {
            let (head, body) = response.split_once("\r\n\r\n").unwrap();
            assert!(head.starts_with("HTTP/1.1 202 Accepted\r\n"), "{response}");
            assert!(body.is_empty(), "{response}");
        }

        let (address, shutdown, calls, join) =
            spawn_server(McpStreamableHttpServerConfig::default());
        let initialize_notification = initialize_request(1).replacen("\"id\":1,", "", 1);
        let response = raw_post(address, &initialize_notification, None, None, None);
        assert_empty_202(&response);
        assert!(!response.contains("Mcp-Session-Id:"));

        let mut client = ready_client(address);
        let session_id = client.session_id().unwrap().to_owned();
        for notification in [
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/unknown\",\"params\":{\"opaque\":\"secret\"}}",
            "{\"jsonrpc\":\"2.0\",\"method\":\"tools/list\",\"params\":{}}",
            "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"must-not-run\"}}}",
        ] {
            let response = raw_post(address, notification, Some(&session_id), None, None);
            assert_empty_202(&response);
            assert!(!response.contains("secret"));
            assert!(!response.contains("must-not-run"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let listed = client
            .post(
                2,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}",
            )
            .unwrap();
        assert!(listed.contains("adapter.list"));
        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.sessions_created, 1);
        assert_eq!(report.sessions_deleted, 1);
        assert_eq!(report.handler_calls, 0);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn excessive_json_nesting_is_rejected_without_stopping_the_server() {
        let (address, shutdown, calls, join) =
            spawn_server(McpStreamableHttpServerConfig::default());
        let nested = format!("{}null{}", "[".repeat(70), "]".repeat(70));
        let initialize = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":{},\"capabilities\":{nested},\"clientInfo\":{{\"name\":\"deep-client\",\"version\":\"1\"}}}}}}",
            json_string(DEFAULT_PROTOCOL_VERSION)
        );
        let rejected_initialize = raw_post(address, &initialize, None, None, None);
        assert!(
            rejected_initialize.contains("request body is not one complete JSON object"),
            "{rejected_initialize}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let mut client = ready_client(address);
        let session_id = client.session_id().unwrap().to_owned();
        let hidden = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"topic.publish\",\"arguments\":{nested}}}}}"
        );
        let rejected_hidden = raw_post(address, &hidden, Some(&session_id), None, None);
        assert!(rejected_hidden.contains("request body is not one complete JSON object"));
        assert!(!rejected_hidden.contains("topic.publish"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let listed = client
            .post(
                3,
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\",\"params\":{}}",
            )
            .unwrap();
        assert!(listed.contains("adapter.list"));
        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.handler_calls, 0);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn session_phase_duplicate_initialized_and_stale_session_fail_closed() {
        let (address, shutdown, _, join) = spawn_server(McpStreamableHttpServerConfig::default());
        let mut client = connect_client(address, 64 * 1024);
        client.initialize(1, &initialize_request(1)).unwrap();
        let session_id = client.session_id().unwrap().to_owned();
        let list = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}";

        let before_ready = raw_post(address, list, Some(&session_id), None, None);
        assert!(before_ready.contains("awaiting notifications/initialized"));
        client.notify(initialized_notification()).unwrap();
        let duplicate = raw_post(
            address,
            initialized_notification(),
            Some(&session_id),
            None,
            None,
        );
        assert!(duplicate.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        client.shutdown().unwrap();
        let stale = raw_post(address, list, Some(&session_id), None, None);
        assert!(stale.starts_with("HTTP/1.1 404 Not Found\r\n"));

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.sessions_created, 1);
        assert_eq!(report.sessions_deleted, 1);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn failed_response_write_never_commits_session_actions() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut server = McpStreamableHttpServer::from_listener(
            listener,
            McpStreamableHttpServerConfig::default(),
            EvaMcpServerSurface::v11_minimal(),
            RecordingHandler { calls },
        )
        .unwrap();
        let session_id = "test-session".to_owned();
        let failed_write = || {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected response write failure",
            ))
        };

        server.commit_action_after_response(
            ServerAction::Create {
                session_id: session_id.clone(),
                session: ServerSession {
                    peer_ip: "127.0.0.1".parse().unwrap(),
                    phase: SessionPhase::AwaitingInitialized,
                },
            },
            failed_write(),
        );
        assert!(!server.sessions.contains_key(&session_id));

        server.apply_action(ServerAction::Create {
            session_id: session_id.clone(),
            session: ServerSession {
                peer_ip: "127.0.0.1".parse().unwrap(),
                phase: SessionPhase::AwaitingInitialized,
            },
        });
        server.commit_action_after_response(
            ServerAction::MarkReady {
                session_id: session_id.clone(),
            },
            failed_write(),
        );
        assert_eq!(
            server
                .sessions
                .get(&session_id)
                .map(|session| session.phase),
            Some(SessionPhase::AwaitingInitialized)
        );
        server.commit_action_after_response(
            ServerAction::Delete {
                session_id: session_id.clone(),
            },
            failed_write(),
        );
        assert!(server.sessions.contains_key(&session_id));
        assert_eq!(server.report.protocol_errors, 3);

        server.commit_action_after_response(
            ServerAction::Delete { session_id },
            Ok(ResponseWriteOutcome::Original),
        );
        assert!(server.sessions.is_empty());
    }

    #[test]
    fn oversized_initialize_fallback_does_not_create_an_unreachable_session() {
        let config = McpStreamableHttpServerConfig::default()
            .with_limits(16 * 1024, 64 * 1024, 512)
            .unwrap()
            .with_max_sessions(1)
            .unwrap();
        let (address, shutdown, calls, join) = spawn_server(config);
        let escaped_id = format!("\"{}\"", "\\u0061".repeat(MAX_ID_STRING_BYTES));
        let oversized_initialize = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{escaped_id},\"method\":\"initialize\",\"params\":{{\"protocolVersion\":{},\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"external-test-client\",\"version\":\"1\"}}}}}}",
            json_string(DEFAULT_PROTOCOL_VERSION)
        );

        let fallback = raw_post(address, &oversized_initialize, None, None, None);
        assert!(fallback.starts_with("HTTP/1.1 500 Internal Server Error\r\n"));
        assert!(!fallback.contains("Mcp-Session-Id:"));

        let mut client = ready_client(address);
        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.sessions_created, 1);
        assert_eq!(report.sessions_deleted, 1);
        assert_eq!(report.sessions_closed_on_shutdown, 0);
        assert_eq!(report.dangling_sessions, 0);
        assert!(report.protocol_errors >= 1);
    }

    #[test]
    fn request_smuggling_compression_and_invalid_utf8_are_rejected() {
        let (address, shutdown, _, join) = spawn_server(McpStreamableHttpServerConfig::default());
        let body = initialize_request(1);
        let prefix = format!(
            "POST {DEFAULT_PATH} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\n"
        );
        for headers in [
            format!(
                "Content-Length: {}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
                body.len()
            ),
            format!(
                "Content-Length: {}\r\nTransfer-Encoding: chunked\r\n\r\n{body}",
                body.len()
            ),
            format!(
                "Content-Length: {}\r\nContent-Encoding: gzip\r\n\r\n{body}",
                body.len()
            ),
        ] {
            let response = String::from_utf8(raw_request(
                address,
                format!("{prefix}{headers}").as_bytes(),
            ))
            .unwrap();
            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        }
        let mut invalid_utf8 = format!("{prefix}Content-Length: 1\r\n\r\n").into_bytes();
        invalid_utf8.push(0xff);
        let response = String::from_utf8(raw_request(address, &invalid_utf8)).unwrap();
        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.sessions_created, 0);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn host_origin_path_and_non_loopback_bind_fail_closed() {
        assert!(McpStreamableHttpServerConfig::default()
            .with_timeouts(Duration::from_secs(61), Duration::from_millis(5))
            .is_err());
        let calls = Arc::new(AtomicUsize::new(0));
        let error = McpStreamableHttpServer::bind(
            "0.0.0.0:0".parse().unwrap(),
            McpStreamableHttpServerConfig::default(),
            EvaMcpServerSurface::v11_minimal(),
            RecordingHandler { calls },
        )
        .unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_server_bind_not_loopback")
        );

        let (address, shutdown, _, join) = spawn_server(McpStreamableHttpServerConfig::default());
        let body = initialize_request(1);
        let wrong_host = raw_post(address, &body, None, Some("localhost:1"), None);
        assert!(wrong_host.starts_with("HTTP/1.1 421 Misdirected Request\r\n"));
        let origin = format!("http://{address}");
        let blocked_origin = raw_post(address, &body, None, None, Some(&origin));
        assert!(blocked_origin.starts_with("HTTP/1.1 403 Forbidden\r\n"));

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.sessions_created, 0);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn request_head_gates_reject_before_waiting_for_a_body() {
        let (address, shutdown, calls, join) =
            spawn_server(McpStreamableHttpServerConfig::default());
        let valid_headers = format!(
            "Host: {address}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: 128\r\n"
        );
        let cases = [
            (
                format!("POST /wrong HTTP/1.1\r\n{valid_headers}\r\n"),
                "HTTP/1.1 404 Not Found\r\n",
            ),
            (
                format!("POST /mcp?query HTTP/1.1\r\n{valid_headers}\r\n"),
                "HTTP/1.1 404 Not Found\r\n",
            ),
            (
                "POST /mcp HTTP/1.1\r\nHost: localhost:1\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: 128\r\n\r\n".to_owned(),
                "HTTP/1.1 421 Misdirected Request\r\n",
            ),
            (
                format!(
                    "POST /mcp HTTP/1.1\r\n{valid_headers}Origin: http://evil.invalid\r\n\r\n"
                ),
                "HTTP/1.1 403 Forbidden\r\n",
            ),
            (
                format!(
                    "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAccept: application/json\r\nContent-Type: text/plain\r\nContent-Length: 128\r\n\r\n"
                ),
                "HTTP/1.1 415 Unsupported Media Type\r\n",
            ),
            (
                format!(
                    "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAccept: application/json;q=0\r\nContent-Type: application/json\r\nContent-Length: 128\r\n\r\n"
                ),
                "HTTP/1.1 406 Not Acceptable\r\n",
            ),
            (
                format!(
                    "GET /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: 128\r\n\r\n"
                ),
                "HTTP/1.1 405 Method Not Allowed\r\n",
            ),
            (
                format!(
                    "DELETE /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: 1\r\n\r\n"
                ),
                "HTTP/1.1 400 Bad Request\r\n",
            ),
        ];

        for (request, expected_status) in cases {
            let response = raw_headers_without_body(address, &request);
            assert!(response.starts_with(expected_status), "{response}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.sessions_created, 0);
        assert_eq!(report.handler_calls, 0);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn accept_quality_excludes_unacceptable_json_ranges() {
        assert!(!accepts_json("application/json;q=0"));
        assert!(!accepts_json("application/json;q=0, */*;q=0.5"));
        assert!(accepts_json("application/*;q=0.5, */*;q=0"));
        assert!(accepts_json("text/plain, */*;q=0.5"));
    }

    #[test]
    fn exact_bound_origin_is_accepted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let config = McpStreamableHttpServerConfig::default()
            .with_allowed_origin(origin.clone())
            .unwrap();
        let (address, shutdown, calls, join) = spawn_server_from_listener(listener, config);

        let response = raw_post(address, &initialize_request(1), None, None, Some(&origin));
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.sessions_created, 1);
        assert_eq!(report.sessions_closed_on_shutdown, 1);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn total_read_deadline_and_request_limit_stop_slow_or_large_clients() {
        let config = McpStreamableHttpServerConfig::default()
            .with_limits(512, 64, 1024)
            .unwrap()
            .with_timeouts(Duration::from_millis(60), Duration::from_millis(5))
            .unwrap();
        let (address, shutdown, _, join) = spawn_server(config);

        let oversized_body = "x".repeat(65);
        let oversized = raw_post(address, &oversized_body, None, None, None);
        assert!(oversized.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));

        let mut slow = TcpStream::connect(address).unwrap();
        slow.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        for byte in b"POST" {
            let _ = slow.write_all(&[*byte]);
            thread::sleep(Duration::from_millis(25));
        }
        let _ = slow.shutdown(Shutdown::Write);
        let mut response = String::new();
        match slow.read_to_string(&mut response) {
            Ok(_) => assert!(
                response.is_empty() || response.starts_with("HTTP/1.1 408 Request Timeout\r\n"),
                "{response}"
            ),
            Err(error) => assert!(matches!(
                error.kind(),
                io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::NotConnected
            )),
        }

        shutdown.shutdown();
        let report = join.join().unwrap();
        assert!(report.protocol_errors >= 2);
        assert_eq!(report.sessions_created, 0);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn handler_time_does_not_reset_the_response_write_deadline() {
        let config = McpStreamableHttpServerConfig::default()
            .with_timeouts(Duration::from_millis(60), Duration::from_millis(5))
            .unwrap();
        let (address, shutdown, calls, join) = spawn_server(config);
        let mut client = ready_client(address);
        let session_id = client.session_id().unwrap().to_owned();
        let started = Instant::now();
        let response = raw_post(
            address,
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"slow\"}}}",
            Some(&session_id),
            None,
            None,
        );

        assert!(
            response.is_empty(),
            "deadline-expired response leaked: {response}"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let listed = client
            .post(
                3,
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\",\"params\":{}}",
            )
            .unwrap();
        assert!(listed.contains("adapter.list"));

        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(report.handler_calls, 1);
        assert!(report.protocol_errors >= 1);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn shutdown_aborts_an_accepted_partial_request_before_handler() {
        let config = McpStreamableHttpServerConfig::default()
            .with_timeouts(Duration::from_secs(5), Duration::from_millis(5))
            .unwrap();
        let (address, shutdown, calls, join) = spawn_server(config);
        let client = ready_client(address);
        let session_id = client.session_id().unwrap().to_owned();
        let body = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"must-not-run\"}}}";
        let request = format!(
            "POST {DEFAULT_PATH} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nMCP-Protocol-Version: {DEFAULT_PROTOCOL_VERSION}\r\nMcp-Session-Id: {session_id}\r\n\r\n{body}",
            body.len()
        );
        let mut partial = TcpStream::connect(address).unwrap();
        partial
            .write_all(&request.as_bytes()[..request.len() - 1])
            .unwrap();
        thread::sleep(Duration::from_millis(50));

        let started = Instant::now();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.handler_calls, 0);
        assert_eq!(report.sessions_closed_on_shutdown, 1);
        assert_eq!(report.dangling_sessions, 0);

        drop(partial);
        drop(client);
        TcpListener::bind(address).unwrap();
    }

    #[test]
    fn oversized_handler_result_is_replaced_without_partial_payload() {
        let config = McpStreamableHttpServerConfig::default()
            .with_limits(16 * 1024, 64 * 1024, 512)
            .unwrap();
        let (address, shutdown, calls, join) = spawn_server(config);
        let mut client = ready_client(address);
        let response = client
            .post(
                2,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"adapter.probe\",\"arguments\":{\"adapter_id\":\"huge\"}}}",
            )
            .unwrap();
        assert!(response.contains("tool result exceeded server limit"));
        assert!(!response.contains(&"x".repeat(32)));
        client.shutdown().unwrap();
        shutdown.shutdown();
        let report = join.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.dangling_sessions, 0);
    }

    #[test]
    fn tool_result_limit_estimate_matches_encoded_body() {
        let id = ValidatedRequestId {
            raw: "1".to_owned(),
        };
        let result = McpServerToolResult::text("quote=\" control=\u{0001} utf8=中");
        let body = tool_result_body(&id, &result);

        assert!(tool_result_fits(&id, &result, body.len()));
        assert!(!tool_result_fits(&id, &result, body.len() - 1));
    }
}
