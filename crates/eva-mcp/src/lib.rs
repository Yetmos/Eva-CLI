//! MCP client/server boundary.

pub mod client;
pub mod json_rpc;
pub mod lifecycle;
pub mod policy;
pub mod schema;
pub mod server;
pub mod session;
pub mod tool_mapping;

pub use client::{InMemoryMcpClient, McpCallReport, McpProbeReport};
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
