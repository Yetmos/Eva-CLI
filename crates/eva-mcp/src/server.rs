//! Controlled Eva MCP server surface descriptors.

use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "controlled MCP server exposure";

/// Tool exposed by the future Eva MCP server surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerTool {
    pub name: String,
    pub description: String,
    pub side_effects: bool,
}

/// V1.1 server surface descriptor; it does not open a socket or stdio server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaMcpServerSurface {
    tools: Vec<McpServerTool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerToolGateReport {
    pub tool: String,
    pub allowed: bool,
    pub reason: Option<String>,
    pub audit: Vec<String>,
}

impl EvaMcpServerSurface {
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

    pub fn tools(&self) -> &[McpServerTool] {
        &self.tools
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_server_surface_exposes_only_side_effect_free_tools() {
        let surface = EvaMcpServerSurface::v11_minimal();

        assert!(surface
            .tools()
            .iter()
            .any(|tool| tool.name == "adapter.list"));
        assert!(surface.tools().iter().all(|tool| !tool.side_effects));
    }

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
