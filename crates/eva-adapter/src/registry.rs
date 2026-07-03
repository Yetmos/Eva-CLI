//! Adapter registry and capability indexes.

use crate::manifest::{AdapterCapabilityBinding, AdapterHandle};
use eva_config::ProjectConfig;
use eva_core::{AdapterId, CapabilityName, EvaError};
use std::collections::{BTreeMap, BTreeSet};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "registered Adapter handles and capability indexes";

/// Registry of authorized Adapter handles derived from project manifests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterRegistry {
    by_id: BTreeMap<AdapterId, AdapterHandle>,
    by_capability: BTreeMap<CapabilityName, BTreeSet<AdapterId>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        let mut registry = Self::new();
        for manifest in &project.adapters {
            registry.register(AdapterHandle::from_manifest(manifest))?;
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

    pub fn get(&self, id: &AdapterId) -> Option<&AdapterHandle> {
        self.by_id.get(id)
    }

    pub fn list(&self) -> Vec<&AdapterHandle> {
        self.by_id.values().collect()
    }

    pub fn providers_for_capability(&self, capability: &CapabilityName) -> Vec<&AdapterHandle> {
        self.by_capability
            .get(capability)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.by_id.get(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn registry_indexes_adapter_capabilities() {
        let project = load_project_config(workspace_root()).unwrap();
        let registry = AdapterRegistry::from_project(&project).unwrap();
        let capability = CapabilityName::parse("mcp.tool.call").unwrap();

        assert!(registry
            .providers_for_capability(&capability)
            .iter()
            .any(|handle| handle.id.as_str() == "github-mcp"));
    }
}
