//! Controlled Eva MCP server surface descriptors.

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
}
