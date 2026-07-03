//! MCP tool/resource/prompt to Eva capability mapping.

use eva_core::{AdapterId, CapabilityName, EvaError};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP tool, resource, and prompt capability mapping";

/// Stable mapping from one allowlisted MCP tool to one Eva capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolMapping {
    pub adapter_id: AdapterId,
    pub tool: String,
    pub capability: CapabilityName,
}

/// In-memory deterministic mapping table for MCP tool diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpToolRegistry {
    by_tool: BTreeMap<(AdapterId, String), McpToolMapping>,
}

impl McpToolMapping {
    pub fn new(
        adapter_id: AdapterId,
        tool: impl Into<String>,
        capability: CapabilityName,
    ) -> Result<Self, EvaError> {
        let tool = tool.into();
        if tool.is_empty() || tool.trim() != tool || tool.chars().any(char::is_whitespace) {
            return Err(EvaError::invalid_argument("MCP tool name must be stable")
                .with_context("tool", tool));
        }
        Ok(Self {
            adapter_id,
            tool,
            capability,
        })
    }
}

impl McpToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, mapping: McpToolMapping) -> Result<(), EvaError> {
        let key = (mapping.adapter_id.clone(), mapping.tool.clone());
        if self.by_tool.contains_key(&key) {
            return Err(EvaError::conflict("MCP tool mapping already exists")
                .with_context("adapter_id", mapping.adapter_id.as_str())
                .with_context("tool", mapping.tool.as_str()));
        }
        self.by_tool.insert(key, mapping);
        Ok(())
    }

    pub fn get(&self, adapter_id: &AdapterId, tool: &str) -> Option<&McpToolMapping> {
        self.by_tool.get(&(adapter_id.clone(), tool.to_owned()))
    }

    pub fn list(&self) -> Vec<&McpToolMapping> {
        self.by_tool.values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rejects_duplicate_tool_mapping() {
        let adapter = AdapterId::parse("github-mcp").unwrap();
        let mapping = McpToolMapping::new(
            adapter.clone(),
            "list_issues",
            CapabilityName::parse("mcp.tool.call").unwrap(),
        )
        .unwrap();
        let mut registry = McpToolRegistry::new();

        registry.register(mapping.clone()).unwrap();

        assert_eq!(
            registry.register(mapping).unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
    }
}
