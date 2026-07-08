//! MCP tool discovery from configured Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::EvaError;

pub const RESPONSIBILITY: &str = "discover configured MCP server capabilities";

pub struct McpDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> McpDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for McpDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "mcp"
    }

    fn timeout_ms(&self) -> u64 {
        250
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            if adapter.transport != AdapterTransport::Mcp {
                continue;
            }
            let tools = adapter.nested_extra_string_list("mcp", "tool_allowlist");
            if tools.is_empty() {
                candidates.push(
                    DiscoveryCandidate::named(
                        self.source_id(),
                        DiscoveryCandidateKind::McpTool,
                        "tool_allowlist",
                        Some(adapter.id.clone()),
                        DiscoveryTrust::DisplayOnly,
                    )
                    .rejected("MCP adapter has no tool allowlist"),
                );
                continue;
            }
            for tool in tools {
                let mut candidate = DiscoveryCandidate::named(
                    self.source_id(),
                    DiscoveryCandidateKind::McpTool,
                    tool,
                    Some(adapter.id.clone()),
                    DiscoveryTrust::ConfiguredAllowlist,
                );
                if !adapter.enabled {
                    candidate = candidate.rejected("adapter manifest is disabled");
                }
                candidates.push(candidate);
            }
        }
        Ok(candidates)
    }
}
