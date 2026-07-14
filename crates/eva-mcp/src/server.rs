//! 本模块提供 `server` 相关实现。
//! Controlled Eva MCP server surface descriptors.

use eva_core::EvaError;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "controlled MCP server exposure";

/// 表示 `McpServerTool` 数据结构。
/// Tool exposed by the future Eva MCP server surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerTool {
    /// 记录 `name` 字段对应的值。
    pub name: String,
    /// 记录 `description` 字段对应的值。
    pub description: String,
    /// 记录 `side_effects` 字段对应的值。
    pub side_effects: bool,
}

/// 表示 `EvaMcpServerSurface` 数据结构。
/// V1.1 server surface descriptor; it does not open a socket or stdio server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaMcpServerSurface {
    /// 记录 `tools` 字段对应的值。
    tools: Vec<McpServerTool>,
}

/// 表示 `McpServerToolGateReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerToolGateReport {
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `allowed` 字段对应的值。
    pub allowed: bool,
    /// 记录 `reason` 字段对应的值。
    pub reason: Option<String>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

impl EvaMcpServerSurface {
    /// 执行 `v11_minimal` 对应的处理逻辑。
    pub fn v11_minimal() -> Self {
        Self {
            tools: vec![
                McpServerTool {
                    name: "adapter.list".to_owned(),
                    description: "List authorized Eva adapter handles".to_owned(),
                    side_effects: false,
                },
                McpServerTool {
                    name: "adapter.probe".to_owned(),
                    description: "Probe an authorized Eva adapter without invoking it".to_owned(),
                    side_effects: false,
                },
            ],
        }
    }

    /// 执行 `tools` 对应的处理逻辑。
    pub fn tools(&self) -> &[McpServerTool] {
        &self.tools
    }

    /// 执行 `gate_tool` 对应的处理逻辑。
    pub fn gate_tool(&self, tool: &str) -> McpServerToolGateReport {
        if self.tools.iter().any(|entry| entry.name == tool) {
            McpServerToolGateReport {
                tool: tool.to_owned(),
                allowed: true,
                reason: None,
                audit: vec![
                    "mcp.server.tool:allowed".to_owned(),
                    format!("tool:{tool}"),
                    "proxy_boundary:explicit_tools_only".to_owned(),
                ],
            }
        } else {
            McpServerToolGateReport {
                tool: tool.to_owned(),
                allowed: false,
                reason: Some("tool is not explicitly exposed".to_owned()),
                audit: vec![
                    "mcp.server.tool:blocked".to_owned(),
                    format!("tool:{tool}"),
                    "proxy_boundary:explicit_tools_only".to_owned(),
                ],
            }
        }
    }

    /// 校验 `require_tool` 对应的约束，不满足时返回明确错误。
    pub fn require_tool(&self, tool: &str) -> Result<McpServerToolGateReport, EvaError> {
        let report = self.gate_tool(tool);
        if report.allowed {
            Ok(report)
        } else {
            Err(
                EvaError::permission_denied("MCP server tool is not explicitly exposed")
                    .with_provider_code("mcp_server_tool_not_exposed")
                    .with_context("tool", tool)
                    .with_context("proxy_boundary", "explicit_tools_only"),
            )
        }
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `minimal_server_surface_exposes_only_side_effect_free_tools` 场景下的预期行为。
    #[test]
    fn minimal_server_surface_exposes_only_side_effect_free_tools() {
        let surface = EvaMcpServerSurface::v11_minimal();

        assert!(surface
            .tools()
            .iter()
            .any(|tool| tool.name == "adapter.list"));
        assert!(surface.tools().iter().all(|tool| !tool.side_effects));
    }

    /// 验证 `illegal_proxy_request_is_rejected_with_audit` 场景下的预期行为。
    #[test]
    fn illegal_proxy_request_is_rejected_with_audit() {
        let surface = EvaMcpServerSurface::v11_minimal();

        let gate = surface.gate_tool("topic.publish");
        let error = surface.require_tool("topic.publish").unwrap_err();

        assert!(!gate.allowed);
        assert!(gate
            .audit
            .contains(&"proxy_boundary:explicit_tools_only".to_owned()));
        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_server_tool_not_exposed")
        );
    }
}
