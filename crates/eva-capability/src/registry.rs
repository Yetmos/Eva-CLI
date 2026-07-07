//! Capability registration and lookup.

use crate::selection::CapabilityProviderSelection;
use eva_config::manifest::capability::CapabilityManifest;
use eva_core::{AdapterId, CapabilityId, CapabilityName, EvaError};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability registration and lookup";

/// Runtime descriptor for a capability entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub name: CapabilityName,
    pub enabled: bool,
    pub provider: String,
    pub provider_selection: CapabilityProviderSelection,
}

/// In-memory descriptor registry.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapabilityRegistry {
    by_name: BTreeMap<CapabilityName, CapabilityDescriptor>,
}

impl CapabilityDescriptor {
    pub fn builtin(id: CapabilityId, name: CapabilityName) -> Self {
        Self {
            id,
            name,
            enabled: true,
            provider: "builtin".to_owned(),
            provider_selection: CapabilityProviderSelection::default(),
        }
    }

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

    pub fn provider_plan(
        &self,
        explicit_provider: Option<AdapterId>,
    ) -> crate::selection::CapabilityProviderPlan {
        self.provider_selection
            .plan_for(self.name.clone(), explicit_provider)
    }
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

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

    pub fn register(&mut self, descriptor: CapabilityDescriptor) -> Result<(), EvaError> {
        if self.by_name.contains_key(&descriptor.name) {
            return Err(EvaError::conflict("capability already registered")
                .with_context("capability", descriptor.name.as_str()));
        }
        self.by_name.insert(descriptor.name.clone(), descriptor);
        Ok(())
    }

    pub fn get(&self, name: &CapabilityName) -> Option<&CapabilityDescriptor> {
        self.by_name.get(name)
    }

    pub fn list(&self) -> Vec<&CapabilityDescriptor> {
        self.by_name.values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v04_builtins_include_config_lint() {
        let registry = CapabilityRegistry::with_v04_builtins();

        assert!(registry
            .get(&CapabilityName::parse("config.lint").unwrap())
            .is_some());
    }

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
