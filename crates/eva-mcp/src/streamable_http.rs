//! Stateful MCP Streamable HTTP client boundary.
//!
//! The underlying transport owns bounded HTTP framing, TLS, protocol-version
//! negotiation, and the opaque application session. This wrapper gives later
//! SSE and abort work one explicit lifecycle without changing the generic
//! JSON-RPC transport trait or the stdio-only process session registry.

use crate::http_transport::McpTlsMaterial;
use crate::json_rpc::{McpHttpJsonRpcTransport, McpJsonRpcTransport};
use crate::session::McpStreamableHttpConfig;
use eva_core::EvaError;
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP Streamable HTTP application-session and shutdown lifecycle";

/// One initialized-or-initializing Streamable HTTP application session.
pub struct McpStreamableHttpSession {
    transport: McpHttpJsonRpcTransport,
    timeout: Duration,
    output_limit_bytes: usize,
}

impl fmt::Debug for McpStreamableHttpSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpSession")
            .field("transport", &self.transport)
            .field("timeout", &self.timeout)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .finish()
    }
}

impl McpStreamableHttpSession {
    /// Create a plaintext or platform-trust session boundary.
    pub fn new(
        config: McpStreamableHttpConfig,
        headers: BTreeMap<String, String>,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        Self::new_with_tls(
            config,
            headers,
            McpTlsMaterial::new(),
            timeout,
            output_limit_bytes,
        )
    }

    /// Create a session boundary with per-invocation TLS material.
    pub fn new_with_tls(
        config: McpStreamableHttpConfig,
        headers: BTreeMap<String, String>,
        tls_material: McpTlsMaterial,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        if timeout.is_zero() {
            return Err(EvaError::invalid_argument(
                "MCP Streamable HTTP timeout must be greater than zero",
            ));
        }
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP Streamable HTTP output limit must be greater than zero",
            ));
        }
        Ok(Self {
            transport: McpHttpJsonRpcTransport::new_with_config_and_tls(
                config,
                headers,
                tls_material,
            )?,
            timeout,
            output_limit_bytes,
        })
    }

    /// Send the initialize request and commit negotiated session state only
    /// after a valid initialize response.
    pub fn initialize(&mut self, request_id: u64, request: &str) -> Result<String, EvaError> {
        if self.transport.negotiated_protocol_version().is_some() {
            return Err(
                EvaError::conflict("MCP Streamable HTTP session is already initialized")
                    .with_provider_code("mcp_http_session_already_initialized"),
            );
        }
        self.transport
            .exchange(request_id, request, self.timeout, self.output_limit_bytes)
    }

    /// Send a JSON-RPC POST using the negotiated version and optional session
    /// identifier established by `initialize`.
    pub fn post(&mut self, request_id: u64, request: &str) -> Result<String, EvaError> {
        self.transport
            .exchange(request_id, request, self.timeout, self.output_limit_bytes)
    }

    /// Send a JSON-RPC notification using the established session headers.
    pub fn notify(&mut self, notification: &str) -> Result<(), EvaError> {
        self.transport
            .notify(notification, self.timeout, self.output_limit_bytes)
    }

    /// Issue the session-bound GET used to establish the SSE data plane.
    /// Incremental event parsing is intentionally deferred to W4-L05.
    pub fn get(&mut self) -> Result<Vec<u8>, EvaError> {
        self.transport.get(self.timeout, self.output_limit_bytes)
    }

    /// Close the application session with DELETE when the server issued an ID.
    pub fn shutdown(&mut self) -> Result<(), EvaError> {
        self.transport
            .shutdown_session(self.timeout, self.output_limit_bytes)
    }

    /// Return the opaque server session ID while the session is active.
    pub fn session_id(&self) -> Option<&str> {
        self.transport.session_id()
    }

    /// Return the protocol version accepted during initialize.
    pub fn negotiated_protocol_version(&self) -> Option<&str> {
        self.transport.negotiated_protocol_version()
    }

    /// Return whether initialize and notifications/initialized both completed.
    pub fn is_ready(&self) -> bool {
        self.transport.is_ready()
    }

    /// Return whether application I/O has been permanently closed locally.
    pub fn is_closed(&self) -> bool {
        self.transport.is_closed()
    }

    /// Return redacted lifecycle evidence.
    pub fn audit(&self) -> Vec<String> {
        self.transport.audit()
    }
}
