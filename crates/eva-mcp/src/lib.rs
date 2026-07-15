//! 本模块提供 `lib` 相关实现。
//! MCP client/server boundary.

/// 声明 `client` 子模块。
pub mod client;
/// 声明 `compatibility` 子模块。
pub mod compatibility;
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
/// 声明 `session` 子模块。
pub mod session;
/// 声明 `tool_mapping` 子模块。
pub mod tool_mapping;

pub use client::{InMemoryMcpClient, McpCallReport, McpProbeReport};
pub use compatibility::{
    McpCompatibilityEvidenceKind, McpCompatibilityMatrix, McpCompatibilityReport,
    McpServerSurfaceCompatibility, McpStreamLifecycleCompatibility, McpToolSchemaCompatibility,
    McpTransportCompatibility,
};
pub use json_rpc::{
    McpHttpJsonRpcTransport, McpJsonRpcCallReport, McpJsonRpcClient, McpJsonRpcClientConfig,
    McpJsonRpcTool, McpJsonRpcTransport, McpStdioJsonRpcTransport,
};
pub use lifecycle::{
    McpOrphanCleanupReport, McpProcessInspector, McpRegisteredSession, McpSessionHealthReport,
    McpSessionLifecycleReport, McpSessionLifecycleStatus, McpSessionRegistry, McpStreamReport,
    McpStreamStatus,
};
pub use policy::McpAllowlist;
pub use server::{EvaMcpServerSurface, McpServerTool, McpServerToolGateReport};
pub use session::{
    McpProcessHandle, McpProcessShutdownRequest, McpProcessSpec, McpProcessStartRequest,
    McpServerTransport, McpSession, McpSessionConfig, McpSessionManager, McpSessionShutdownReport,
    McpSessionStartReport, McpSessionStatus, McpSessionSupervisor,
};
pub use tool_mapping::{McpToolMapping, McpToolRegistry};
