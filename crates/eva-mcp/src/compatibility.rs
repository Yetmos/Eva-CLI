//! 本模块提供 `compatibility` 相关实现。
//! MCP compatibility matrix and stream lifecycle evidence.

use crate::http_transport::McpTlsMaterial;
use crate::json_rpc::{
    json_string, parse_json_object_fields, parse_json_string, DEFAULT_PROTOCOL_VERSION,
};
use crate::lifecycle::{McpHttpAbortReceipt, McpStreamableHttpSessionRegistry};
use crate::policy::McpAllowlist;
use crate::server::{
    EvaMcpServerSurface, McpServerTool, McpServerToolCall, McpServerToolHandler,
    McpServerToolResult,
};
use crate::server_transport::{
    McpServerServeReport, McpStreamableHttpServer, McpStreamableHttpServerConfig,
};
use crate::session::{McpRedirectPolicy, McpServerTransport, McpStreamableHttpConfig};
use crate::streamable_http::McpStreamableHttpSession;
use crate::{McpJsonRpcClient, McpJsonRpcClientConfig};
use eva_core::{sha256_digest, AdapterId, EvaError, RequestId};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP transport, schema, and stream lifecycle compatibility matrix";

/// MCP compatibility 数据来自受控 fixture 还是真实 server run。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCompatibilityEvidenceKind {
    /// 静态构造的兼容矩阵，只能支持 alpha 展示。
    Fixture,
    /// 由命名 MCP server 实际运行生成的兼容结果。
    Measurement,
}

impl McpCompatibilityEvidenceKind {
    /// 返回跨 crate 映射使用的稳定小写标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Measurement => "measurement",
        }
    }
}

/// 表示 `McpCompatibilityMatrix` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityMatrix {
    /// compatibility 数据的来源强度。
    evidence_kind: McpCompatibilityEvidenceKind,
    /// 记录 `protocol_version` 字段对应的值。
    pub protocol_version: String,
    /// 记录 `transports` 字段对应的值。
    pub transports: Vec<McpTransportCompatibility>,
    /// 记录 `tool_schemas` 字段对应的值。
    pub tool_schemas: Vec<McpToolSchemaCompatibility>,
    /// 记录 `stream_lifecycle` 字段对应的值。
    pub stream_lifecycle: McpStreamLifecycleCompatibility,
    /// 记录 `server_surface` 字段对应的值。
    pub server_surface: McpServerSurfaceCompatibility,
}

/// 表示 `McpTransportCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTransportCompatibility {
    /// 记录 `transport` 字段对应的值。
    pub transport: McpServerTransport,
    /// 记录 `json_rpc` 字段对应的值。
    pub json_rpc: bool,
    /// 记录 `auth_headers` 字段对应的值。
    pub auth_headers: bool,
    /// 记录 `timeout_and_output_limits` 字段对应的值。
    pub timeout_and_output_limits: bool,
    /// 记录 `stream_lifecycle` 字段对应的值。
    pub stream_lifecycle: bool,
}

/// 表示 `McpToolSchemaCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolSchemaCompatibility {
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `input_schema` 字段对应的值。
    pub input_schema: String,
    /// 记录 `output_content` 字段对应的值。
    pub output_content: String,
    /// 记录 `schema_checked` 字段对应的值。
    pub schema_checked: bool,
}

/// 表示 `McpStreamLifecycleCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamLifecycleCompatibility {
    /// 记录 `start` 字段对应的值。
    pub start: bool,
    /// 记录 `abort` 字段对应的值。
    pub abort: bool,
    /// 记录 `cleanup` 字段对应的值。
    pub cleanup: bool,
    /// 记录 `dangling_sessions_after_abort` 字段对应的值。
    pub dangling_sessions_after_abort: u32,
}

/// 表示 `McpServerSurfaceCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSurfaceCompatibility {
    /// 记录 `explicit_tool_gate` 字段对应的值。
    pub explicit_tool_gate: bool,
    /// 记录 `unlimited_proxy_exposed` 字段对应的值。
    pub unlimited_proxy_exposed: bool,
}

/// 表示 `McpCompatibilityReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityReport {
    /// verifier 保留的 fixture 或 measurement 来源分类。
    evidence_kind: McpCompatibilityEvidenceKind,
    /// 记录 `protocol_version` 字段对应的值。
    pub protocol_version: String,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `transport_count` 字段对应的值。
    pub transport_count: usize,
    /// 记录 `tool_schema_count` 字段对应的值。
    pub tool_schema_count: usize,
    /// 记录 `failures` 字段对应的值。
    pub failures: Vec<String>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// A canonical digest and byte count observed from one real protocol value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityDigestReceipt {
    sha256: String,
    bytes: usize,
}

impl McpCompatibilityDigestReceipt {
    /// Canonical lowercase SHA-256 with the `sha256:` prefix.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Number of bytes supplied to the digest function.
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

/// TLS facts returned by the rustls server after real loopback handshakes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityTlsReceipt {
    handshake_completed: bool,
    peer_name: String,
    protocol: String,
    handshake_count: usize,
}

impl McpCompatibilityTlsReceipt {
    /// Whether the controlled TLS server completed every expected handshake.
    pub const fn handshake_completed(&self) -> bool {
        self.handshake_completed
    }

    /// DNS name authenticated by the client connector.
    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    /// Protocol negotiated by rustls on the observed connections.
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// Number of completed request-connection handshakes in the TLS run.
    pub const fn handshake_count(&self) -> usize {
        self.handshake_count
    }
}

/// Direct JSON-RPC observations retained without retaining response payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityObservationReceipt {
    initialize_server_info: bool,
    tools_list_schema: bool,
    tools_call_result: bool,
}

impl McpCompatibilityObservationReceipt {
    /// Whether name and version came from the direct initialize `serverInfo`.
    pub const fn initialize_server_info(&self) -> bool {
        self.initialize_server_info
    }

    /// Whether the schema digest came from the named `tools/list` entry.
    pub const fn tools_list_schema(&self) -> bool {
        self.tools_list_schema
    }

    /// Whether the output digest came from the direct `tools/call` result.
    pub const fn tools_call_result(&self) -> bool {
        self.tools_call_result
    }
}

/// Typed lifecycle evidence combining registry completion with server-side
/// observation that the persistent stream closed before DELETE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityAbortReceipt {
    socket_closed: bool,
    session_deleted: bool,
    reader_joined: bool,
    sessions_after: usize,
    readers_after: usize,
    cleanup_pending_after: usize,
}

impl McpCompatibilityAbortReceipt {
    pub const fn socket_closed(&self) -> bool {
        self.socket_closed
    }

    pub const fn session_deleted(&self) -> bool {
        self.session_deleted
    }

    pub const fn reader_joined(&self) -> bool {
        self.reader_joined
    }

    pub const fn sessions_after(&self) -> usize {
        self.sessions_after
    }

    pub const fn readers_after(&self) -> usize {
        self.readers_after
    }

    pub const fn cleanup_pending_after(&self) -> usize {
        self.cleanup_pending_after
    }

    pub const fn complete(&self) -> bool {
        self.socket_closed
            && self.session_deleted
            && self.reader_joined
            && self.sessions_after == 0
            && self.readers_after == 0
            && self.cleanup_pending_after == 0
    }
}

/// Sealed compatibility measurement produced only by executing the crate's
/// controlled server, rustls connector, persistent SSE reader, and registry
/// cleanup path. There is intentionally no public constructor or mutable
/// access to any evidence field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityMeasurement {
    server_name: String,
    server_version: String,
    protocol_version: String,
    transport: McpServerTransport,
    tool_name: String,
    tls: McpCompatibilityTlsReceipt,
    schema: McpCompatibilityDigestReceipt,
    output: McpCompatibilityDigestReceipt,
    observations: McpCompatibilityObservationReceipt,
    abort: McpCompatibilityAbortReceipt,
    matrix: McpCompatibilityMatrix,
    canonical_manifest: String,
    canonical_digest: String,
    _seal: measurement_seal::Seal,
}

mod measurement_seal {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Seal;
}

impl McpCompatibilityMeasurement {
    /// Execute the explicit local measurement. This performs network I/O on
    /// numeric loopback only and never contacts an external service.
    pub fn measure_loopback() -> Result<Self, EvaError> {
        measure_loopback_compatibility()
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub const fn transport(&self) -> McpServerTransport {
        self.transport
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub const fn tls(&self) -> &McpCompatibilityTlsReceipt {
        &self.tls
    }

    pub const fn schema(&self) -> &McpCompatibilityDigestReceipt {
        &self.schema
    }

    pub const fn output(&self) -> &McpCompatibilityDigestReceipt {
        &self.output
    }

    pub const fn observations(&self) -> &McpCompatibilityObservationReceipt {
        &self.observations
    }

    pub const fn abort(&self) -> &McpCompatibilityAbortReceipt {
        &self.abort
    }

    /// Canonical key/value subject. Lines use LF and the final line ends in LF.
    pub fn canonical_manifest(&self) -> &str {
        &self.canonical_manifest
    }

    /// Bytes that W0 evidence must bind. A JSON display wrapper is not a subject.
    pub fn subject_bytes(&self) -> &[u8] {
        self.canonical_manifest.as_bytes()
    }

    pub fn canonical_digest(&self) -> &str {
        &self.canonical_digest
    }

    pub fn verify(&self) -> Result<McpCompatibilityReport, EvaError> {
        if sha256_digest(self.subject_bytes()) != self.canonical_digest
            || !self.tls.handshake_completed
            || !self.observations.initialize_server_info
            || !self.observations.tools_list_schema
            || !self.observations.tools_call_result
            || !self.abort.complete()
            || self.matrix.evidence_kind != McpCompatibilityEvidenceKind::Measurement
        {
            return Err(EvaError::conflict(
                "MCP compatibility measurement receipt is inconsistent",
            )
            .with_provider_code("mcp_compatibility_measurement_invalid"));
        }
        let report = self.matrix.verify()?;
        if report.status != "compatible" || !report.failures.is_empty() {
            return Err(EvaError::conflict(
                "MCP compatibility measurement matrix is not compatible",
            )
            .with_provider_code("mcp_compatibility_measurement_blocked"));
        }
        Ok(report)
    }
}

impl McpCompatibilityReport {
    /// 返回 verifier 从受控 matrix 保留的 evidence 来源分类。
    pub const fn evidence_kind(&self) -> McpCompatibilityEvidenceKind {
        self.evidence_kind
    }
}

impl McpCompatibilityMatrix {
    /// 返回由受控构造路径固定的 evidence 来源分类。
    pub const fn evidence_kind(&self) -> McpCompatibilityEvidenceKind {
        self.evidence_kind
    }

    /// 执行 `v1137_fixture` 对应的处理逻辑。
    pub fn v1137_fixture() -> Self {
        Self {
            evidence_kind: McpCompatibilityEvidenceKind::Fixture,
            protocol_version: "2025-11-25".to_owned(),
            transports: vec![
                McpTransportCompatibility {
                    transport: McpServerTransport::Stdio,
                    json_rpc: true,
                    auth_headers: false,
                    timeout_and_output_limits: true,
                    stream_lifecycle: true,
                },
                McpTransportCompatibility {
                    transport: McpServerTransport::Http,
                    json_rpc: true,
                    auth_headers: true,
                    timeout_and_output_limits: true,
                    stream_lifecycle: true,
                },
            ],
            tool_schemas: vec![McpToolSchemaCompatibility {
                tool: "list_issues".to_owned(),
                input_schema: "object".to_owned(),
                output_content: "content[].text".to_owned(),
                schema_checked: true,
            }],
            stream_lifecycle: McpStreamLifecycleCompatibility {
                start: true,
                abort: true,
                cleanup: true,
                dangling_sessions_after_abort: 0,
            },
            server_surface: McpServerSurfaceCompatibility {
                explicit_tool_gate: true,
                unlimited_proxy_exposed: false,
            },
        }
    }

    /// 执行 `verify` 对应的处理逻辑。
    pub fn verify(&self) -> Result<McpCompatibilityReport, EvaError> {
        if self.protocol_version.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "MCP compatibility protocol version must be non-empty",
            ));
        }

        let mut failures = Vec::new();
        if self.evidence_kind == McpCompatibilityEvidenceKind::Fixture
            && !self
                .transports
                .iter()
                .any(|entry| entry.transport == McpServerTransport::Stdio && entry.json_rpc)
        {
            failures.push("transport:stdio_json_rpc_missing".to_owned());
        }
        if !self
            .transports
            .iter()
            .any(|entry| entry.transport.is_http() && entry.json_rpc)
        {
            failures.push("transport:http_json_rpc_missing".to_owned());
        }
        if self.evidence_kind == McpCompatibilityEvidenceKind::Fixture
            && !self
                .transports
                .iter()
                .any(|entry| entry.transport.is_http() && entry.auth_headers)
        {
            failures.push("transport:http_auth_headers_missing".to_owned());
        }
        failures.extend(
            self.transports
                .iter()
                .filter(|entry| !entry.timeout_and_output_limits)
                .map(|entry| format!("transport:{}_limits_missing", entry.transport.as_str())),
        );
        failures.extend(
            self.transports
                .iter()
                .filter(|entry| !entry.stream_lifecycle)
                .map(|entry| {
                    format!(
                        "transport:{}_stream_lifecycle_missing",
                        entry.transport.as_str()
                    )
                }),
        );
        if self.tool_schemas.is_empty() {
            failures.push("tool_schema:matrix_empty".to_owned());
        }
        failures.extend(
            self.tool_schemas
                .iter()
                .filter(|entry| !entry.schema_checked)
                .map(|entry| format!("tool_schema:{}_unchecked", entry.tool)),
        );
        if !self.stream_lifecycle.start {
            failures.push("stream_lifecycle:start_missing".to_owned());
        }
        if !self.stream_lifecycle.abort {
            failures.push("stream_lifecycle:abort_missing".to_owned());
        }
        if !self.stream_lifecycle.cleanup {
            failures.push("stream_lifecycle:cleanup_missing".to_owned());
        }
        if self.stream_lifecycle.dangling_sessions_after_abort > 0 {
            failures.push(format!(
                "stream_lifecycle:dangling_sessions:{}",
                self.stream_lifecycle.dangling_sessions_after_abort
            ));
        }
        if !self.server_surface.explicit_tool_gate {
            failures.push("server_surface:explicit_tool_gate_missing".to_owned());
        }
        if self.server_surface.unlimited_proxy_exposed {
            failures.push("server_surface:unlimited_proxy_exposed".to_owned());
        }

        let status = if failures.is_empty() {
            "compatible"
        } else {
            "blocked"
        }
        .to_owned();
        Ok(McpCompatibilityReport {
            evidence_kind: self.evidence_kind,
            protocol_version: self.protocol_version.clone(),
            status,
            transport_count: self.transports.len(),
            tool_schema_count: self.tool_schemas.len(),
            failures,
            audit: self.audit_entries(),
        })
    }

    /// 执行 `audit_entries` 对应的处理逻辑。
    fn audit_entries(&self) -> Vec<String> {
        let mut audit = vec![
            format!("mcp.protocol_version:{}", self.protocol_version),
            format!("mcp.transport_count:{}", self.transports.len()),
            format!("mcp.tool_schema_count:{}", self.tool_schemas.len()),
            format!(
                "mcp.stream_lifecycle:start={},abort={},cleanup={}",
                self.stream_lifecycle.start,
                self.stream_lifecycle.abort,
                self.stream_lifecycle.cleanup
            ),
            format!(
                "mcp.server_surface:explicit_tool_gate={}",
                self.server_surface.explicit_tool_gate
            ),
        ];
        audit.extend(self.transports.iter().map(|entry| {
            format!(
                "mcp.transport:{}:json_rpc={},auth_headers={},limits={},stream_lifecycle={}",
                entry.transport.as_str(),
                entry.json_rpc,
                entry.auth_headers,
                entry.timeout_and_output_limits,
                entry.stream_lifecycle
            )
        }));
        audit
    }
}

const COMPATIBILITY_FORMAT: &str = "eva.mcp-compatibility.v1";
const COMPATIBILITY_TOOL: &str = "compat.echo";
const COMPATIBILITY_SCHEMA: &str = r#"{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}"#;
const COMPATIBILITY_INPUT: &str = r#"{"value":"loopback-ok"}"#;
const COMPATIBILITY_TLS_CA_REF: &str = "pem:eva-mcp-loopback-compatibility-ca";
const COMPATIBILITY_TLS_CA: &[u8] = include_bytes!("../testdata/tls/ca.pem");
const COMPATIBILITY_TLS_CERT: &[u8] = include_bytes!("../testdata/tls/server.pem");
const COMPATIBILITY_TLS_KEY: &[u8] = include_bytes!("../testdata/tls/server.key");
const COMPATIBILITY_TIMEOUT: Duration = Duration::from_secs(3);
const COMPATIBILITY_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
struct ControlledServerRun {
    server_name: String,
    server_version: String,
    protocol_version: String,
    schema_bytes: Vec<u8>,
    output_bytes: Vec<u8>,
}

#[derive(Debug)]
struct TlsLifecycleRun {
    tls_protocol: String,
    handshake_count: usize,
    stream_closed_before_delete: bool,
    delete_observed: bool,
    lifecycle: McpHttpAbortReceipt,
}

#[derive(Debug)]
struct TlsFixtureRun {
    tls_protocol: String,
    handshake_count: usize,
    initialize_observed: bool,
    initialized_observed: bool,
    list_observed: bool,
    call_observed: bool,
    stream_closed_before_delete: bool,
    delete_observed: bool,
}

#[derive(Debug, Default)]
struct CompatibilityEchoHandler;

impl McpServerToolHandler for CompatibilityEchoHandler {
    fn call_tool(
        &mut self,
        request: McpServerToolCall<'_>,
    ) -> Result<McpServerToolResult, EvaError> {
        request.require_only_arguments(&["value"])?;
        Ok(McpServerToolResult::text(
            request.required_string_argument("value")?,
        ))
    }
}

fn measure_loopback_compatibility() -> Result<McpCompatibilityMeasurement, EvaError> {
    let controlled = run_controlled_server_measurement()?;
    let tls_run = run_tls_lifecycle_measurement()?;
    if !tls_run.stream_closed_before_delete
        || !tls_run.delete_observed
        || !tls_run.lifecycle.complete()
    {
        return Err(EvaError::unavailable(
            "MCP compatibility lifecycle measurement did not complete",
        )
        .with_provider_code("mcp_compatibility_lifecycle_incomplete"));
    }

    let schema = digest_receipt(&controlled.schema_bytes);
    let output = digest_receipt(&controlled.output_bytes);
    let tls = McpCompatibilityTlsReceipt {
        handshake_completed: tls_run.handshake_count > 0,
        peer_name: "127.0.0.1".to_owned(),
        protocol: tls_run.tls_protocol,
        handshake_count: tls_run.handshake_count,
    };
    if !tls.handshake_completed {
        return Err(
            EvaError::unavailable("MCP compatibility TLS handshakes were incomplete")
                .with_provider_code("mcp_compatibility_tls_incomplete"),
        );
    }
    let observations = McpCompatibilityObservationReceipt {
        initialize_server_info: true,
        tools_list_schema: true,
        tools_call_result: true,
    };
    let abort = McpCompatibilityAbortReceipt {
        socket_closed: tls_run.stream_closed_before_delete && tls_run.lifecycle.socket_closed(),
        session_deleted: tls_run.delete_observed && tls_run.lifecycle.session_deleted(),
        reader_joined: tls_run.lifecycle.reader_joined(),
        sessions_after: tls_run.lifecycle.sessions_after(),
        readers_after: tls_run.lifecycle.readers_after(),
        cleanup_pending_after: tls_run.lifecycle.cleanup_pending_after(),
    };
    let matrix = McpCompatibilityMatrix {
        evidence_kind: McpCompatibilityEvidenceKind::Measurement,
        protocol_version: controlled.protocol_version.clone(),
        transports: vec![McpTransportCompatibility {
            transport: McpServerTransport::StreamableHttp,
            json_rpc: true,
            auth_headers: false,
            timeout_and_output_limits: true,
            stream_lifecycle: true,
        }],
        tool_schemas: vec![McpToolSchemaCompatibility {
            tool: COMPATIBILITY_TOOL.to_owned(),
            input_schema: schema.sha256.clone(),
            output_content: output.sha256.clone(),
            schema_checked: true,
        }],
        stream_lifecycle: McpStreamLifecycleCompatibility {
            start: true,
            abort: abort.socket_closed,
            cleanup: abort.session_deleted && abort.reader_joined,
            dangling_sessions_after_abort: u32::try_from(abort.sessions_after).unwrap_or(u32::MAX),
        },
        server_surface: McpServerSurfaceCompatibility {
            explicit_tool_gate: true,
            unlimited_proxy_exposed: false,
        },
    };
    let canonical_manifest =
        compatibility_manifest(&controlled, &tls, &schema, &output, &observations, &abort)?;
    let canonical_digest = sha256_digest(canonical_manifest.as_bytes());
    let measurement = McpCompatibilityMeasurement {
        server_name: controlled.server_name,
        server_version: controlled.server_version,
        protocol_version: controlled.protocol_version,
        transport: McpServerTransport::StreamableHttp,
        tool_name: COMPATIBILITY_TOOL.to_owned(),
        tls,
        schema,
        output,
        observations,
        abort,
        matrix,
        canonical_manifest,
        canonical_digest,
        _seal: measurement_seal::Seal,
    };
    measurement.verify()?;
    Ok(measurement)
}

fn digest_receipt(bytes: &[u8]) -> McpCompatibilityDigestReceipt {
    McpCompatibilityDigestReceipt {
        sha256: sha256_digest(bytes),
        bytes: bytes.len(),
    }
}

fn compatibility_surface() -> Result<EvaMcpServerSurface, EvaError> {
    EvaMcpServerSurface::new(vec![McpServerTool::new(
        COMPATIBILITY_TOOL,
        "Echo one controlled compatibility value",
        COMPATIBILITY_SCHEMA,
        false,
    )?])
}

fn run_controlled_server_measurement() -> Result<ControlledServerRun, EvaError> {
    let server = McpStreamableHttpServer::bind(
        "127.0.0.1:0".parse::<SocketAddr>().map_err(|error| {
            EvaError::internal("MCP compatibility loopback address is invalid")
                .with_context("parse_error", error.to_string())
        })?,
        McpStreamableHttpServerConfig::default(),
        compatibility_surface()?,
        CompatibilityEchoHandler,
    )?;
    let address = server.local_addr();
    let shutdown = server.shutdown_handle();
    let join = thread::spawn(move || server.serve());
    let run = run_controlled_server_client(address);
    shutdown.shutdown();
    let report = join_server(join)?;
    let run = run?;
    validate_controlled_server_report(&report)?;
    Ok(run)
}

fn run_controlled_server_client(address: SocketAddr) -> Result<ControlledServerRun, EvaError> {
    let endpoint = format!("http://{address}/mcp");
    let config = McpStreamableHttpConfig::legacy_http(endpoint)?;
    let mut session = McpStreamableHttpSession::new(
        config,
        BTreeMap::new(),
        COMPATIBILITY_TIMEOUT,
        COMPATIBILITY_OUTPUT_LIMIT,
    )?;
    let initialize = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":{},\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"eva-compatibility\",\"version\":{}}}}}}}",
        json_string(DEFAULT_PROTOCOL_VERSION),
        json_string(env!("CARGO_PKG_VERSION")),
    );
    let initialize_response = session.initialize(1, &initialize)?;
    session
        .notify("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}")?;
    let list_response = session.post(
        2,
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}",
    )?;
    let call_response = session.post(
        3,
        &format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{{\"name\":{},\"arguments\":{COMPATIBILITY_INPUT}}}}}",
            json_string(COMPATIBILITY_TOOL),
        ),
    )?;
    session.shutdown()?;

    let initialize_result = direct_json_rpc_result(&initialize_response, "1")?;
    let initialize_fields = parse_json_object_fields(initialize_result)?;
    let protocol_version = direct_json_string_field(&initialize_fields, "protocolVersion")?;
    let server_info = initialize_fields.get("serverInfo").ok_or_else(|| {
        compatibility_protocol_error("initialize result is missing direct serverInfo")
    })?;
    let server_fields = parse_json_object_fields(server_info)?;
    let server_name = direct_json_string_field(&server_fields, "name")?;
    let server_version = direct_json_string_field(&server_fields, "version")?;

    let list_result = direct_json_rpc_result(&list_response, "2")?;
    let list_fields = parse_json_object_fields(list_result)?;
    let tool_value = single_array_value(list_fields.get("tools").ok_or_else(|| {
        compatibility_protocol_error("tools/list result is missing direct tools")
    })?)?;
    let tool_fields = parse_json_object_fields(tool_value)?;
    if direct_json_string_field(&tool_fields, "name")? != COMPATIBILITY_TOOL {
        return Err(compatibility_protocol_error(
            "tools/list did not return the measured tool",
        ));
    }
    let schema = tool_fields.get("inputSchema").ok_or_else(|| {
        compatibility_protocol_error("tools/list tool is missing direct inputSchema")
    })?;

    let call_result = direct_json_rpc_result(&call_response, "3")?;
    Ok(ControlledServerRun {
        server_name,
        server_version,
        protocol_version,
        schema_bytes: schema.as_bytes().to_vec(),
        output_bytes: call_result.as_bytes().to_vec(),
    })
}

fn join_server(
    join: JoinHandle<Result<McpServerServeReport, EvaError>>,
) -> Result<McpServerServeReport, EvaError> {
    join.join().map_err(|_| {
        EvaError::internal("MCP compatibility controlled server thread panicked")
            .with_provider_code("mcp_compatibility_server_panicked")
    })?
}

fn validate_controlled_server_report(report: &McpServerServeReport) -> Result<(), EvaError> {
    if report.sessions_created != 1
        || report.sessions_deleted != 1
        || report.sessions_closed_on_shutdown != 0
        || report.handler_calls != 1
        || report.blocked_tool_calls != 0
        || report.protocol_errors != 0
        || report.dangling_sessions != 0
    {
        return Err(EvaError::unavailable(
            "MCP compatibility controlled server run was incomplete",
        )
        .with_provider_code("mcp_compatibility_server_incomplete"));
    }
    Ok(())
}

fn direct_json_rpc_result<'a>(response: &'a str, expected_id: &str) -> Result<&'a str, EvaError> {
    let fields = parse_json_object_fields(response)?;
    if fields.get("jsonrpc").copied() != Some("\"2.0\"")
        || fields.get("id").copied() != Some(expected_id)
        || fields.contains_key("error")
    {
        return Err(compatibility_protocol_error(
            "MCP compatibility JSON-RPC response envelope is invalid",
        ));
    }
    fields.get("result").copied().ok_or_else(|| {
        compatibility_protocol_error("MCP compatibility JSON-RPC response is missing result")
    })
}

fn direct_json_string_field(
    fields: &BTreeMap<String, &str>,
    name: &'static str,
) -> Result<String, EvaError> {
    fields
        .get(name)
        .ok_or_else(|| compatibility_protocol_error("MCP compatibility field is missing"))
        .and_then(|value| parse_json_string(value))
}

fn single_array_value(value: &str) -> Result<&str, EvaError> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
        .ok_or_else(|| compatibility_protocol_error("MCP compatibility array is invalid"))?;
    if inner.is_empty() {
        return Err(compatibility_protocol_error(
            "MCP compatibility array is empty",
        ));
    }
    parse_json_object_fields(inner)?;
    Ok(inner)
}

fn compatibility_protocol_error(message: &'static str) -> EvaError {
    EvaError::unavailable(message).with_provider_code("mcp_compatibility_protocol_invalid")
}

fn run_tls_lifecycle_measurement() -> Result<TlsLifecycleRun, EvaError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| {
        EvaError::unavailable("failed to bind MCP compatibility TLS listener")
            .with_provider_code("mcp_compatibility_tls_bind_failed")
            .with_context("io_error", error.to_string())
    })?;
    let address = listener.local_addr().map_err(|error| {
        EvaError::unavailable("failed to inspect MCP compatibility TLS listener")
            .with_provider_code("mcp_compatibility_tls_bind_failed")
            .with_context("io_error", error.to_string())
    })?;
    listener.set_nonblocking(true).map_err(|error| {
        EvaError::unavailable("failed to configure MCP compatibility TLS listener")
            .with_provider_code("mcp_compatibility_tls_bind_failed")
            .with_context("io_error", error.to_string())
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_config = compatibility_tls_server_config()?;
    let join = thread::spawn(move || serve_tls_fixture(listener, server_config, server_stop));

    let client_result = run_tls_lifecycle_client(address);
    if client_result.is_err() {
        stop.store(true, Ordering::Release);
    }
    let fixture = join.join().map_err(|_| {
        EvaError::internal("MCP compatibility TLS server thread panicked")
            .with_provider_code("mcp_compatibility_tls_server_panicked")
    })??;
    let lifecycle = client_result?;
    if !fixture.initialize_observed
        || !fixture.initialized_observed
        || !fixture.list_observed
        || !fixture.call_observed
        || !fixture.stream_closed_before_delete
        || !fixture.delete_observed
        || fixture.handshake_count == 0
        || fixture.tls_protocol.is_empty()
    {
        return Err(
            EvaError::unavailable("MCP compatibility TLS fixture run was incomplete")
                .with_provider_code("mcp_compatibility_tls_incomplete"),
        );
    }
    Ok(TlsLifecycleRun {
        tls_protocol: fixture.tls_protocol,
        handshake_count: fixture.handshake_count,
        stream_closed_before_delete: fixture.stream_closed_before_delete,
        delete_observed: fixture.delete_observed,
        lifecycle,
    })
}

fn run_tls_lifecycle_client(address: SocketAddr) -> Result<McpHttpAbortReceipt, EvaError> {
    let origin = format!("https://{address}");
    let config = McpStreamableHttpConfig::from_parts(
        format!("{origin}/mcp"),
        [COMPATIBILITY_TLS_CA_REF],
        None,
        McpRedirectPolicy::Deny,
        [origin],
    )?;
    config.validate_for_environment("production")?;
    let tls_material = McpTlsMaterial::new()
        .with_indirect_trust_root(COMPATIBILITY_TLS_CA_REF, COMPATIBILITY_TLS_CA.to_vec())?;
    let session = McpStreamableHttpSession::new_with_tls(
        config,
        BTreeMap::new(),
        tls_material,
        COMPATIBILITY_TIMEOUT,
        COMPATIBILITY_OUTPUT_LIMIT,
    )?;
    let adapter_id = AdapterId::parse("mcp-compatibility-loopback")?;
    let mut registry = McpStreamableHttpSessionRegistry::new();
    let registered = registry.register_starting_session(adapter_id.clone(), session)?;
    let client = McpJsonRpcClient::new(adapter_id, McpAllowlist::from_tools([COMPATIBILITY_TOOL])?)
        .with_config(
            McpJsonRpcClientConfig::new()
                .with_request_timeout_ms(COMPATIBILITY_TIMEOUT.as_millis() as u64)
                .with_output_limit_bytes(COMPATIBILITY_OUTPUT_LIMIT),
        );
    registry.call_tool(
        &registered.session_id,
        &client,
        RequestId::parse("mcp-compatibility-loopback-call")?,
        COMPATIBILITY_TOOL,
        COMPATIBILITY_INPUT,
    )?;
    registry.open_event_stream(&registered.session_id, "compatibility-events")?;
    let (_, receipt) =
        registry.abort_stream_with_receipt(&registered.session_id, "compatibility-events")?;
    if !receipt.complete() {
        return Err(
            EvaError::unavailable("MCP compatibility registry abort left residual state")
                .with_provider_code("mcp_compatibility_registry_residual"),
        );
    }
    Ok(receipt)
}

fn compatibility_tls_server_config() -> Result<Arc<ServerConfig>, EvaError> {
    let certificates = rustls_pemfile::certs(&mut BufReader::new(COMPATIBILITY_TLS_CERT))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            EvaError::internal("MCP compatibility TLS certificate is invalid")
                .with_provider_code("mcp_compatibility_tls_fixture_invalid")
                .with_context("tls_error", error.to_string())
        })?;
    let private_key = rustls_pemfile::private_key(&mut BufReader::new(COMPATIBILITY_TLS_KEY))
        .map_err(|error| {
            EvaError::internal("MCP compatibility TLS private key is invalid")
                .with_provider_code("mcp_compatibility_tls_fixture_invalid")
                .with_context("tls_error", error.to_string())
        })?
        .ok_or_else(|| {
            EvaError::internal("MCP compatibility TLS private key is missing")
                .with_provider_code("mcp_compatibility_tls_fixture_invalid")
        })?;
    let provider = rustls::crypto::ring::default_provider();
    let config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            EvaError::internal("MCP compatibility TLS protocol configuration failed")
                .with_provider_code("mcp_compatibility_tls_fixture_invalid")
                .with_context("tls_error", error.to_string())
        })?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| {
            EvaError::internal("MCP compatibility TLS identity configuration failed")
                .with_provider_code("mcp_compatibility_tls_fixture_invalid")
                .with_context("tls_error", error.to_string())
        })?;
    Ok(Arc::new(config))
}

fn serve_tls_fixture(
    listener: TcpListener,
    server_config: Arc<ServerConfig>,
    stop: Arc<AtomicBool>,
) -> Result<TlsFixtureRun, EvaError> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(15);
    let mut receipt = TlsFixtureRun {
        tls_protocol: String::new(),
        handshake_count: 0,
        initialize_observed: false,
        initialized_observed: false,
        list_observed: false,
        call_observed: false,
        stream_closed_before_delete: false,
        delete_observed: false,
    };
    let mut closed_receiver = None;
    let mut stream_monitor = None;
    while !receipt.delete_observed && !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        let socket = match listener.accept() {
            Ok((socket, _)) => socket,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(error) => {
                return Err(
                    EvaError::unavailable("MCP compatibility TLS listener accept failed")
                        .with_provider_code("mcp_compatibility_tls_accept_failed")
                        .with_context("io_error", error.to_string()),
                );
            }
        };
        socket.set_nonblocking(false).map_err(|error| {
            EvaError::unavailable("failed to configure MCP compatibility TLS socket")
                .with_provider_code("mcp_compatibility_tls_io_failed")
                .with_context("io_error", error.to_string())
        })?;
        socket
            .set_read_timeout(Some(COMPATIBILITY_TIMEOUT))
            .map_err(compatibility_tls_io_error)?;
        socket
            .set_write_timeout(Some(COMPATIBILITY_TIMEOUT))
            .map_err(compatibility_tls_io_error)?;
        let connection = ServerConnection::new(server_config.clone()).map_err(|error| {
            EvaError::unavailable("MCP compatibility TLS server connection failed")
                .with_provider_code("mcp_compatibility_tls_handshake_failed")
                .with_context("tls_error", error.to_string())
        })?;
        let mut stream = StreamOwned::new(connection, socket);
        let request = read_bounded_http_request(&mut stream)?;
        if stream.conn.is_handshaking() {
            return Err(
                EvaError::unavailable("MCP compatibility TLS handshake did not complete")
                    .with_provider_code("mcp_compatibility_tls_handshake_failed"),
            );
        }
        let protocol = stream
            .conn
            .protocol_version()
            .map(|version| format!("{version:?}"))
            .ok_or_else(|| {
                EvaError::unavailable("MCP compatibility TLS protocol was not negotiated")
                    .with_provider_code("mcp_compatibility_tls_handshake_failed")
            })?;
        if receipt.tls_protocol.is_empty() {
            receipt.tls_protocol = protocol;
        } else if receipt.tls_protocol != protocol {
            return Err(EvaError::conflict(
                "MCP compatibility TLS protocol changed during one run",
            )
            .with_provider_code("mcp_compatibility_tls_protocol_changed"));
        }
        receipt.handshake_count = receipt.handshake_count.saturating_add(1);

        if request.starts_with("GET /mcp HTTP/1.1\r\n") {
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Mcp-Session-Id: compatibility-tls-session\r\n",
                "Content-Type: text/event-stream\r\n",
                "Transfer-Encoding: chunked\r\n",
                "Connection: keep-alive\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .map_err(compatibility_tls_io_error)?;
            stream.flush().map_err(compatibility_tls_io_error)?;
            let (sender, receiver) = mpsc::sync_channel(1);
            closed_receiver = Some(receiver);
            stream_monitor = Some(thread::spawn(move || observe_stream_close(stream, sender)));
            continue;
        }

        let (status, body) = if request.starts_with("DELETE /mcp HTTP/1.1\r\n") {
            receipt.stream_closed_before_delete = closed_receiver
                .as_ref()
                .and_then(|receiver| receiver.recv_timeout(COMPATIBILITY_TIMEOUT).ok())
                .unwrap_or(false);
            receipt.delete_observed = true;
            (204, String::new())
        } else if request.contains("\"method\":\"initialize\"") {
            receipt.initialize_observed = true;
            (
                200,
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":{},\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"eva\",\"version\":{}}}}}}}",
                    json_string(DEFAULT_PROTOCOL_VERSION),
                    json_string(env!("CARGO_PKG_VERSION")),
                ),
            )
        } else if request.contains("\"method\":\"notifications/initialized\"") {
            receipt.initialized_observed = true;
            (202, String::new())
        } else if request.contains("\"method\":\"tools/list\"") {
            receipt.list_observed = true;
            (
                200,
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[{{\"name\":{},\"description\":\"Compatibility echo\",\"inputSchema\":{COMPATIBILITY_SCHEMA}}}]}}}}",
                    json_string(COMPATIBILITY_TOOL),
                ),
            )
        } else if request.contains("\"method\":\"tools/call\"") {
            receipt.call_observed = true;
            (
                200,
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"tls-loopback-ok\"}],\"isError\":false}}".to_owned(),
            )
        } else {
            return Err(compatibility_protocol_error(
                "MCP compatibility TLS fixture received an unexpected request",
            ));
        };
        let response = compatibility_http_response(status, &body);
        stream
            .write_all(response.as_bytes())
            .map_err(compatibility_tls_io_error)?;
        stream.flush().map_err(compatibility_tls_io_error)?;
    }
    if let Some(join) = stream_monitor {
        let monitor_closed = join.join().map_err(|_| {
            EvaError::internal("MCP compatibility stream monitor panicked")
                .with_provider_code("mcp_compatibility_stream_monitor_panicked")
        })?;
        receipt.stream_closed_before_delete &= monitor_closed;
    }
    if !receipt.delete_observed {
        return Err(
            EvaError::timeout("MCP compatibility TLS fixture did not observe DELETE")
                .with_provider_code("mcp_compatibility_tls_fixture_timeout"),
        );
    }
    Ok(receipt)
}

fn observe_stream_close(
    mut stream: StreamOwned<ServerConnection, TcpStream>,
    sender: mpsc::SyncSender<bool>,
) -> bool {
    let mut byte = [0_u8; 1];
    let closed = match stream.read(&mut byte) {
        Ok(0) => true,
        Err(error) => matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::NotConnected
                | io::ErrorKind::UnexpectedEof
        ),
        Ok(_) => false,
    };
    let _ = sender.send(closed);
    closed
}

fn read_bounded_http_request(stream: &mut impl Read) -> Result<String, EvaError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .map_err(compatibility_tls_io_error)?;
        if read == 0 {
            return Err(compatibility_protocol_error(
                "MCP compatibility TLS request ended before framing completed",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > COMPATIBILITY_OUTPUT_LIMIT {
            return Err(compatibility_protocol_error(
                "MCP compatibility TLS request exceeded its bound",
            ));
        }
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = header_end + 4;
        let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| {
            compatibility_protocol_error("MCP compatibility TLS request headers are not UTF-8")
        })?;
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if content_length > COMPATIBILITY_OUTPUT_LIMIT {
            return Err(compatibility_protocol_error(
                "MCP compatibility TLS request body exceeded its bound",
            ));
        }
        if bytes.len() >= body_start.saturating_add(content_length) {
            return String::from_utf8(bytes).map_err(|_| {
                compatibility_protocol_error("MCP compatibility TLS request is not UTF-8")
            });
        }
    }
}

fn compatibility_http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        _ => "Compatibility",
    };
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nMcp-Session-Id: compatibility-tls-session\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn compatibility_tls_io_error(error: io::Error) -> EvaError {
    EvaError::unavailable("MCP compatibility TLS fixture I/O failed")
        .with_provider_code("mcp_compatibility_tls_io_failed")
        .with_context("io_error", error.to_string())
}

fn compatibility_manifest(
    controlled: &ControlledServerRun,
    tls: &McpCompatibilityTlsReceipt,
    schema: &McpCompatibilityDigestReceipt,
    output: &McpCompatibilityDigestReceipt,
    observations: &McpCompatibilityObservationReceipt,
    abort: &McpCompatibilityAbortReceipt,
) -> Result<String, EvaError> {
    let fields = [
        ("format", COMPATIBILITY_FORMAT.to_owned()),
        ("evidence_kind", "measurement".to_owned()),
        ("server_name", controlled.server_name.clone()),
        ("server_version", controlled.server_version.clone()),
        ("protocol_version", controlled.protocol_version.clone()),
        (
            "transport",
            McpServerTransport::StreamableHttp
                .canonical_str()
                .to_owned(),
        ),
        (
            "tls_handshake_completed",
            tls.handshake_completed.to_string(),
        ),
        ("tls_peer_name", tls.peer_name.clone()),
        ("tls_protocol", tls.protocol.clone()),
        ("tls_handshake_count", tls.handshake_count.to_string()),
        ("tool_name", COMPATIBILITY_TOOL.to_owned()),
        ("schema_sha256", schema.sha256.clone()),
        ("schema_bytes", schema.bytes.to_string()),
        ("output_sha256", output.sha256.clone()),
        ("output_bytes", output.bytes.to_string()),
        (
            "initialize_server_info_observed",
            observations.initialize_server_info.to_string(),
        ),
        (
            "tools_list_schema_observed",
            observations.tools_list_schema.to_string(),
        ),
        (
            "tools_call_result_observed",
            observations.tools_call_result.to_string(),
        ),
        ("abort_socket_closed", abort.socket_closed.to_string()),
        ("abort_session_deleted", abort.session_deleted.to_string()),
        ("abort_reader_joined", abort.reader_joined.to_string()),
        ("abort_sessions_after", abort.sessions_after.to_string()),
        ("abort_readers_after", abort.readers_after.to_string()),
        (
            "abort_cleanup_pending_after",
            abort.cleanup_pending_after.to_string(),
        ),
    ];
    let mut manifest = String::new();
    for (key, value) in fields {
        if value.is_empty()
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b'=')
        {
            return Err(EvaError::conflict(
                "MCP compatibility canonical manifest contains an invalid scalar",
            )
            .with_provider_code("mcp_compatibility_manifest_invalid")
            .with_context("field", key));
        }
        manifest.push_str(key);
        manifest.push('=');
        manifest.push_str(&value);
        manifest.push('\n');
    }
    Ok(manifest)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `v1137_fixture_verifies_mcp_compatibility_matrix` 场景下的预期行为。
    #[test]
    fn v1137_fixture_verifies_mcp_compatibility_matrix() {
        let matrix = McpCompatibilityMatrix::v1137_fixture();
        assert_eq!(
            matrix.evidence_kind(),
            McpCompatibilityEvidenceKind::Fixture
        );
        let report = matrix.verify().unwrap();

        assert_eq!(
            report.evidence_kind(),
            McpCompatibilityEvidenceKind::Fixture
        );
        assert_eq!(report.status, "compatible");
        assert_eq!(report.transport_count, 2);
        assert_eq!(report.tool_schema_count, 1);
        assert!(report.failures.is_empty());
        assert!(report.audit.contains(
            &"mcp.transport:http:json_rpc=true,auth_headers=true,limits=true,stream_lifecycle=true"
                .to_owned()
        ));
    }

    /// 验证 `compatibility_matrix_blocks_missing_stream_cleanup` 场景下的预期行为。
    #[test]
    fn compatibility_matrix_blocks_missing_stream_cleanup() {
        let mut matrix = McpCompatibilityMatrix::v1137_fixture();
        matrix.stream_lifecycle.cleanup = false;

        let report = matrix.verify().unwrap();

        assert_eq!(report.status, "blocked");
        assert!(report
            .failures
            .contains(&"stream_lifecycle:cleanup_missing".to_owned()));
    }

    /// 验证 `compatibility_matrix_blocks_missing_transport_stream_lifecycle` 场景下的预期行为。
    #[test]
    fn compatibility_matrix_blocks_missing_transport_stream_lifecycle() {
        let mut matrix = McpCompatibilityMatrix::v1137_fixture();
        matrix.transports[1].stream_lifecycle = false;

        let report = matrix.verify().unwrap();

        assert_eq!(report.status, "blocked");
        assert!(report
            .failures
            .contains(&"transport:http_stream_lifecycle_missing".to_owned()));
    }

    /// 验证 `compatibility_matrix_blocks_unlimited_server_proxy` 场景下的预期行为。
    #[test]
    fn compatibility_matrix_blocks_unlimited_server_proxy() {
        let mut matrix = McpCompatibilityMatrix::v1137_fixture();
        matrix.server_surface.unlimited_proxy_exposed = true;

        let report = matrix.verify().unwrap();

        assert_eq!(report.status, "blocked");
        assert!(report
            .failures
            .contains(&"server_surface:unlimited_proxy_exposed".to_owned()));
    }

    #[test]
    fn loopback_measurement_runs_server_tls_and_real_abort_before_sealing_subject() {
        let measurement = McpCompatibilityMeasurement::measure_loopback().unwrap();

        assert_eq!(measurement.server_name(), "eva");
        assert_eq!(measurement.server_version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(measurement.protocol_version(), DEFAULT_PROTOCOL_VERSION);
        assert_eq!(measurement.transport(), McpServerTransport::StreamableHttp);
        assert_eq!(measurement.tool_name(), COMPATIBILITY_TOOL);
        assert!(measurement.tls().handshake_completed());
        assert_eq!(measurement.tls().peer_name(), "127.0.0.1");
        assert!(matches!(
            measurement.tls().protocol(),
            "TLSv1_2" | "TLSv1_3"
        ));
        assert!(measurement.tls().handshake_count() > 0);
        assert_eq!(
            measurement.schema().sha256(),
            sha256_digest(COMPATIBILITY_SCHEMA.as_bytes())
        );
        assert_eq!(measurement.schema().bytes(), COMPATIBILITY_SCHEMA.len());
        assert!(measurement.output().sha256().starts_with("sha256:"));
        assert!(measurement.output().bytes() > 0);
        assert!(measurement.observations().initialize_server_info());
        assert!(measurement.observations().tools_list_schema());
        assert!(measurement.observations().tools_call_result());
        assert!(measurement.abort().complete());
        assert_eq!(measurement.abort().sessions_after(), 0);
        assert_eq!(measurement.abort().readers_after(), 0);
        assert_eq!(measurement.abort().cleanup_pending_after(), 0);

        let manifest = measurement.canonical_manifest();
        assert!(manifest.starts_with("format=eva.mcp-compatibility.v1\n"));
        assert!(manifest.ends_with("abort_cleanup_pending_after=0\n"));
        assert!(!manifest.contains(COMPATIBILITY_SCHEMA));
        assert!(!manifest.contains("loopback-ok"));
        assert_eq!(
            measurement.canonical_digest(),
            sha256_digest(measurement.subject_bytes())
        );
        let report = measurement.verify().unwrap();
        assert_eq!(
            report.evidence_kind(),
            McpCompatibilityEvidenceKind::Measurement
        );
        assert_eq!(report.status, "compatible");
        assert!(report.failures.is_empty());
    }
}
