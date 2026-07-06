//! MCP client/server boundary.

pub mod client;
pub mod policy;
pub mod schema;
pub mod server;
pub mod session;
pub mod tool_mapping;

pub use client::{InMemoryMcpClient, McpCallReport, McpProbeReport};
pub use policy::McpAllowlist;
pub use server::{EvaMcpServerSurface, McpServerTool};
pub use session::{
    McpProcessHandle, McpProcessShutdownRequest, McpProcessSpec, McpProcessStartRequest,
    McpServerTransport, McpSession, McpSessionConfig, McpSessionManager, McpSessionShutdownReport,
    McpSessionStartReport, McpSessionStatus, McpSessionSupervisor,
};
pub use tool_mapping::{McpToolMapping, McpToolRegistry};
