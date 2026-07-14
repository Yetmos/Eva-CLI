//! 本模块提供 `tool_mapping` 相关实现。
//! MCP tool/resource/prompt to Eva capability mapping.

use eva_core::{AdapterId, CapabilityName, EvaError};
use std::collections::BTreeMap;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP tool, resource, and prompt capability mapping";

/// 表示 `McpToolMapping` 数据结构。
/// Stable mapping from one allowlisted MCP tool to one Eva capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolMapping {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
}

/// 表示 `McpToolRegistry` 数据结构。
/// In-memory deterministic mapping table for MCP tool diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpToolRegistry {
    /// 记录 `by_tool` 字段对应的值。
    by_tool: BTreeMap<(AdapterId, String), McpToolMapping>,
}

impl McpToolMapping {
    /// 创建并初始化当前类型的实例。
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
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记 `register` 对应的数据或状态。
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

    /// 返回 `get` 对应的数据视图。
    pub fn get(&self, adapter_id: &AdapterId, tool: &str) -> Option<&McpToolMapping> {
        self.by_tool.get(&(adapter_id.clone(), tool.to_owned()))
    }

    /// 返回 `list` 对应的数据视图。
    pub fn list(&self) -> Vec<&McpToolMapping> {
        self.by_tool.values().collect()
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `registry_rejects_duplicate_tool_mapping` 场景下的预期行为。
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
