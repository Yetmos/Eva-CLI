//! Adapter runtime handle representation.

use eva_config::manifest::adapter::AdapterManifest;
use eva_config::manifest::capability::CapabilityManifest;
use eva_config::{AdapterTransport, CapabilityKind};
use eva_core::{AdapterId, CapabilityId, CapabilityName};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Adapter manifest runtime representation";

/// Lightweight health state carried by a registered Adapter handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterHealth {
    Ready,
    Disabled,
}

/// Runtime binding between one Eva capability manifest and one Adapter handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCapabilityBinding {
    pub capability_id: Option<CapabilityId>,
    pub capability: CapabilityName,
    pub kind: CapabilityKind,
    pub provider: AdapterId,
    pub mcp_tool: Option<String>,
}

/// Authorized runtime handle derived from configuration, not from discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterHandle {
    pub id: AdapterId,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub transport: AdapterTransport,
    pub capabilities: Vec<CapabilityName>,
    pub source_path: String,
    pub mcp_tools: Vec<String>,
    pub skill_id: Option<String>,
    pub skill_kind: Option<String>,
    pub skill_runtime_gate: Option<String>,
    pub bindings: Vec<AdapterCapabilityBinding>,
}

impl AdapterHealth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disabled => "disabled",
        }
    }
}

impl AdapterHandle {
    pub fn from_manifest(manifest: &AdapterManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            enabled: manifest.enabled,
            transport: manifest.transport,
            capabilities: manifest.capabilities.clone(),
            source_path: manifest.path.display().to_string(),
            mcp_tools: manifest.nested_extra_string_list("mcp", "tool_allowlist"),
            skill_id: manifest
                .nested_extra_string("skill", "id")
                .map(str::to_owned),
            skill_kind: manifest
                .nested_extra_string("skill", "kind")
                .map(str::to_owned),
            skill_runtime_gate: manifest
                .nested_extra_string("skill", "runtime_gate")
                .map(str::to_owned),
            bindings: Vec::new(),
        }
    }

    pub fn health(&self) -> AdapterHealth {
        if self.enabled {
            AdapterHealth::Ready
        } else {
            AdapterHealth::Disabled
        }
    }

    pub fn supports(&self, capability: &CapabilityName) -> bool {
        self.capabilities.iter().any(|entry| entry == capability)
            || self
                .bindings
                .iter()
                .any(|entry| &entry.capability == capability)
    }

    pub fn add_binding(&mut self, binding: AdapterCapabilityBinding) {
        if !self.bindings.iter().any(|existing| {
            existing.capability == binding.capability && existing.provider == binding.provider
        }) {
            self.bindings.push(binding);
            self.bindings.sort_by(|left, right| {
                left.capability
                    .cmp(&right.capability)
                    .then(left.provider.cmp(&right.provider))
            });
        }
    }

    pub fn binding_for(&self, capability: &CapabilityName) -> Option<&AdapterCapabilityBinding> {
        self.bindings
            .iter()
            .find(|binding| &binding.capability == capability)
    }

    pub fn mcp_tool_for(&self, capability: &CapabilityName) -> Option<&str> {
        self.binding_for(capability)
            .and_then(|binding| binding.mcp_tool.as_deref())
            .or_else(|| self.mcp_tools.first().map(String::as_str))
    }

    pub fn skill_name(&self) -> Option<&str> {
        self.skill_id.as_deref()
    }
}

impl AdapterCapabilityBinding {
    pub fn from_manifest(provider: AdapterId, manifest: &CapabilityManifest) -> Self {
        Self {
            capability_id: Some(manifest.id.clone()),
            capability: manifest.capability.clone(),
            kind: manifest.kind,
            provider,
            mcp_tool: manifest.extra_string("tool").map(str::to_owned),
        }
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
    fn handle_reads_mcp_and_skill_extensions() {
        let project = load_project_config(workspace_root()).unwrap();
        let mcp = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "github-mcp")
            .unwrap();
        let skill = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "code-review-skill")
            .unwrap();

        let mcp_handle = AdapterHandle::from_manifest(mcp);
        let skill_handle = AdapterHandle::from_manifest(skill);

        assert!(mcp_handle.mcp_tools.contains(&"list_issues".to_owned()));
        assert_eq!(skill_handle.skill_name(), Some("code-review"));
    }
}
