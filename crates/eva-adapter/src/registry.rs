//! 本模块提供 `registry` 相关实现。
//! Adapter registry and capability indexes.

use crate::manifest::{AdapterCapabilityBinding, AdapterHandle};
use eva_config::ProjectConfig;
use eva_core::{AdapterId, CapabilityName, EvaError};
use std::collections::{BTreeMap, BTreeSet};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "registered Adapter handles and capability indexes";

/// 表示 `AdapterRegistry` 数据结构。
/// Registry of authorized Adapter handles derived from project manifests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterRegistry {
    /// 记录 `by_id` 字段对应的值。
    by_id: BTreeMap<AdapterId, AdapterHandle>,
    /// 记录 `by_capability` 字段对应的值。
    by_capability: BTreeMap<CapabilityName, BTreeSet<AdapterId>>,
}

impl AdapterRegistry {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 根据输入构造当前类型，作为 `from_project` 的标准入口。
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        let mut registry = Self::new();
        for manifest in &project.adapters {
            registry.register(AdapterHandle::from_manifest_in_project(
                manifest,
                &project.project_root,
            ))?;
        }
        for capability in &project.capabilities {
            if !capability.enabled {
                continue;
            }
            let providers = capability.adapter_providers().cloned().collect::<Vec<_>>();
            for provider in providers {
                if let Some(handle) = registry.by_id.get_mut(&provider) {
                    handle.add_binding(AdapterCapabilityBinding::from_manifest(
                        provider.clone(),
                        capability,
                    ));
                    registry
                        .by_capability
                        .entry(capability.capability.clone())
                        .or_default()
                        .insert(provider);
                }
            }
        }
        Ok(registry)
    }

    /// 登记 `register` 对应的数据或状态。
    pub fn register(&mut self, handle: AdapterHandle) -> Result<(), EvaError> {
        if self.by_id.contains_key(&handle.id) {
            return Err(EvaError::conflict("Adapter handle already registered")
                .with_context("adapter_id", handle.id.as_str()));
        }
        for capability in &handle.capabilities {
            self.by_capability
                .entry(capability.clone())
                .or_default()
                .insert(handle.id.clone());
        }
        self.by_id.insert(handle.id.clone(), handle);
        Ok(())
    }

    /// 返回 `get` 对应的数据视图。
    pub fn get(&self, id: &AdapterId) -> Option<&AdapterHandle> {
        self.by_id.get(id)
    }

    /// 返回 `list` 对应的数据视图。
    pub fn list(&self) -> Vec<&AdapterHandle> {
        self.by_id.values().collect()
    }

    /// 执行 `providers_for_capability` 对应的处理逻辑。
    pub fn providers_for_capability(&self, capability: &CapabilityName) -> Vec<&AdapterHandle> {
        self.by_capability
            .get(capability)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.by_id.get(id))
            .collect()
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 验证 `registry_indexes_adapter_capabilities` 场景下的预期行为。
    #[test]
    fn registry_indexes_adapter_capabilities() {
        let project = load_project_config(workspace_root()).unwrap();
        let registry = AdapterRegistry::from_project(&project).unwrap();
        let capability = CapabilityName::parse("mcp.tool.call").unwrap();

        assert!(registry
            .providers_for_capability(&capability)
            .iter()
            .any(|handle| handle.id.as_str() == "github-mcp"));
        assert!(registry
            .list()
            .iter()
            .all(|handle| handle.project_root.as_deref() == Some(project.project_root.as_path())));
    }
}
