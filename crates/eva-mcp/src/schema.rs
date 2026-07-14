//! 本模块提供 `schema` 相关实现。
//! MCP schema boundary descriptors.

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP input and output schema boundaries";

/// 定义 `McpSchemaFamily` 可取的状态。
/// Stable schema family names used by CLI and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSchemaFamily {
    /// 表示 `ToolCall` 枚举分支。
    ToolCall,
    /// 表示 `ToolResult` 枚举分支。
    ToolResult,
    /// 表示 `ErrorEnvelope` 枚举分支。
    ErrorEnvelope,
}

impl McpSchemaFamily {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "mcp.tool_call",
            Self::ToolResult => "mcp.tool_result",
            Self::ErrorEnvelope => "mcp.error_envelope",
        }
    }
}
