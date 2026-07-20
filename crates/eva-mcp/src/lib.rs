//! 本模块提供 `lib` 相关实现。
//! MCP client/server boundary.

/// 声明 `client` 子模块。
pub mod client;
/// 声明 `compatibility` 子模块。
pub mod compatibility;
/// Synchronous HTTP/TLS connection boundary.
pub mod http_transport;
/// 声明 `json_rpc` 子模块。
pub mod json_rpc;
/// 声明 `lifecycle` 子模块。
pub mod lifecycle;
/// 声明 `policy` 子模块。
pub mod policy;
/// 声明 `schema` 子模块。
pub mod schema;
/// 声明 `server` 子模块。
pub mod server;
/// Controlled loopback Streamable HTTP server transport.
pub mod server_transport;
/// 声明 `session` 子模块。
pub mod session;
/// Incremental Streamable HTTP event decoding.
pub mod sse;
/// Stateful Streamable HTTP application-session boundary.
pub mod streamable_http;
/// 声明 `tool_mapping` 子模块。
pub mod tool_mapping;

pub use client::{InMemoryMcpClient, McpCallReport, McpProbeReport};
pub use compatibility::{
    McpCompatibilityEvidenceKind, McpCompatibilityMatrix, McpCompatibilityReport,
    McpServerSurfaceCompatibility, McpStreamLifecycleCompatibility, McpToolSchemaCompatibility,
    McpTransportCompatibility,
};
pub use http_transport::McpTlsMaterial;
pub use json_rpc::{
    McpHttpJsonRpcTransport, McpJsonRpcCallReport, McpJsonRpcClient, McpJsonRpcClientConfig,
    McpJsonRpcMessageId, McpJsonRpcMessageKind, McpJsonRpcTool, McpJsonRpcTransport,
    McpStdioJsonRpcTransport, McpStdioProcess,
};
pub use lifecycle::{
    McpOrphanCleanupReport, McpProcessInspector, McpRegisteredSession, McpSessionHealthReport,
    McpSessionLifecycleReport, McpSessionLifecycleStatus, McpSessionRegistry, McpStreamReport,
    McpStreamStatus, McpStreamableHttpSessionRegistry,
};
pub use policy::McpAllowlist;
pub use server::{
    EvaMcpServerSurface, McpServerTool, McpServerToolCall, McpServerToolGateReport,
    McpServerToolHandler, McpServerToolResult,
};
pub use server_transport::{
    McpServerServeReport, McpServerShutdownHandle, McpStreamableHttpServer,
    McpStreamableHttpServerConfig,
};
pub use session::{
    McpClientAuthConfig, McpEndpoint, McpHttpTransportConfig, McpProcessHandle,
    McpProcessShutdownRequest, McpProcessSpec, McpProcessStartRequest, McpRedirectPolicy,
    McpServerTransport, McpSession, McpSessionConfig, McpSessionManager, McpSessionShutdownReport,
    McpSessionStartReport, McpSessionStatus, McpSessionSupervisor, McpStreamableHttpConfig,
    McpTransportConfig,
};
pub use sse::{McpSseEventStream, McpSseItem, McpSseMessage, McpSseParser};
pub use streamable_http::McpStreamableHttpSession;
pub use tool_mapping::{McpToolMapping, McpToolRegistry};
