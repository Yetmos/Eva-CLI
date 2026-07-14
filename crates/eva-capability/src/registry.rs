//! 本模块提供 `registry` 相关实现。
//! Capability registration and lookup.

use crate::selection::CapabilityProviderSelection;
use eva_config::manifest::capability::CapabilityManifest;
use eva_core::{AdapterId, CapabilityId, CapabilityName, EvaError};
use std::collections::BTreeMap;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability registration and lookup";

/// 表示 `CapabilityDescriptor` 数据结构。
/// Runtime descriptor for a capability entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    /// 记录 `id` 字段对应的值。
    pub id: CapabilityId,
    /// 记录 `name` 字段对应的值。
    pub name: CapabilityName,
    /// 记录 `enabled` 字段对应的值。
    pub enabled: bool,
    /// 记录 `provider` 字段对应的值。
    pub provider: String,
    /// 记录 `provider_selection` 字段对应的值。
    pub provider_selection: CapabilityProviderSelection,
}

/// 表示 `CapabilityRegistry` 数据结构。
/// In-memory descriptor registry.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapabilityRegistry {
    /// 记录 `by_name` 字段对应的值。
    by_name: BTreeMap<CapabilityName, CapabilityDescriptor>,
}

impl CapabilityDescriptor {
    /// 执行 `builtin` 对应的处理逻辑。
    pub fn builtin(id: CapabilityId, name: CapabilityName) -> Self {
        Self {
            id,
            name,
            enabled: true,
            provider: "builtin".to_owned(),
            provider_selection: CapabilityProviderSelection::default(),
        }
    }

    /// 根据输入构造当前类型，作为 `from_manifest` 的标准入口。
    pub fn from_manifest(manifest: &CapabilityManifest) -> Self {
        let provider_selection = CapabilityProviderSelection::new(
            manifest.provider.clone(),
            manifest.default_provider.clone(),
            manifest.allowed_adapter_providers.clone(),
            manifest.required_adapter_capabilities.clone(),
        );
        Self {
            id: manifest.id.clone(),
            name: manifest.capability.clone(),
            enabled: manifest.enabled,
            provider: manifest
                .default_provider
                .as_ref()
                .or(manifest.provider.as_ref())
                .map(ToString::to_string)
                .unwrap_or_else(|| manifest.kind.as_str().to_owned()),
            provider_selection,
        }
    }

    /// 执行 `provider_plan` 对应的处理逻辑。
    pub fn provider_plan(
        &self,
        explicit_provider: Option<AdapterId>,
    ) -> crate::selection::CapabilityProviderPlan {
        self.provider_selection
            .plan_for(self.name.clone(), explicit_provider)
    }
}

impl CapabilityRegistry {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 `v04_builtins` 并返回更新后的实例。
    pub fn with_v04_builtins() -> Self {
        let mut registry = Self::new();
        registry
            .register(CapabilityDescriptor::builtin(
                CapabilityId::parse("config-lint-builtin").unwrap(),
                CapabilityName::parse("config.lint").unwrap(),
            ))
            .unwrap();
        registry
            .register(CapabilityDescriptor::builtin(
                CapabilityId::parse("runtime-echo-builtin").unwrap(),
                CapabilityName::parse("runtime.echo").unwrap(),
            ))
            .unwrap();
        registry
    }

    /// 登记 `register` 对应的数据或状态。
    pub fn register(&mut self, descriptor: CapabilityDescriptor) -> Result<(), EvaError> {
        if self.by_name.contains_key(&descriptor.name) {
            return Err(EvaError::conflict("capability already registered")
                .with_context("capability", descriptor.name.as_str()));
        }
        self.by_name.insert(descriptor.name.clone(), descriptor);
        Ok(())
    }

    /// 返回 `get` 对应的数据视图。
    pub fn get(&self, name: &CapabilityName) -> Option<&CapabilityDescriptor> {
        self.by_name.get(name)
    }

    /// 返回 `list` 对应的数据视图。
    pub fn list(&self) -> Vec<&CapabilityDescriptor> {
        self.by_name.values().collect()
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `v04_builtins_include_config_lint` 场景下的预期行为。
    #[test]
    fn v04_builtins_include_config_lint() {
        let registry = CapabilityRegistry::with_v04_builtins();

        assert!(registry
            .get(&CapabilityName::parse("config.lint").unwrap())
            .is_some());
    }

    /// 验证 `manifest_descriptor_preserves_provider_selection_metadata` 场景下的预期行为。
    #[test]
    fn manifest_descriptor_preserves_provider_selection_metadata() {
        let manifest = eva_config::manifest::capability::load_capability_manifest(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("config/capabilities/repo-summary.yaml"),
        )
        .unwrap();

        let descriptor = CapabilityDescriptor::from_manifest(&manifest);
        let plan = descriptor.provider_plan(Some(AdapterId::parse("explicit-provider").unwrap()));

        assert_eq!(descriptor.provider, "codex-cli");
        assert_eq!(
            plan.provider_ids()
                .map(AdapterId::as_str)
                .collect::<Vec<_>>(),
            ["explicit-provider", "codex-cli"]
        );
        assert_eq!(
            plan.required_adapter_capabilities[0].as_str(),
            "repo.analyze"
        );
    }
}
