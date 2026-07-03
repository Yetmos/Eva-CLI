//! MCP client/server boundary.

pub mod client;
pub mod policy;
pub mod schema;
pub mod server;
pub mod tool_mapping;

pub use client::{InMemoryMcpClient, McpCallReport, McpProbeReport};
pub use policy::McpAllowlist;
pub use server::{EvaMcpServerSurface, McpServerTool};
pub use tool_mapping::{McpToolMapping, McpToolRegistry};
