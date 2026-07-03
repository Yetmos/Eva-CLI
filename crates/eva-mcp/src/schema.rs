//! MCP schema boundary descriptors.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP input and output schema boundaries";

/// Stable schema family names used by CLI and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSchemaFamily {
    ToolCall,
    ToolResult,
    ErrorEnvelope,
}

impl McpSchemaFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "mcp.tool_call",
            Self::ToolResult => "mcp.tool_result",
            Self::ErrorEnvelope => "mcp.error_envelope",
        }
    }
}
