//! Discovery candidate normalization.

use eva_core::{AdapterId, CapabilityName};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "normalize discovered candidates and rejected reasons";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiscoveryCandidateKind {
    Agent,
    Adapter,
    Capability,
    PathCommand,
    McpTool,
    Skill,
    Workflow,
    RegistryEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiscoveryTrust {
    ProjectManifest,
    ConfiguredAllowlist,
    DisplayOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryCandidate {
    pub id: String,
    pub kind: DiscoveryCandidateKind,
    pub source: String,
    pub trust: DiscoveryTrust,
    pub adapter_id: Option<AdapterId>,
    pub capability: Option<CapabilityName>,
    pub handle_granted: bool,
    pub rejected_reason: Option<String>,
}

impl DiscoveryCandidateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Adapter => "adapter",
            Self::Capability => "capability",
            Self::PathCommand => "path_command",
            Self::McpTool => "mcp_tool",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
            Self::RegistryEntry => "registry_entry",
        }
    }
}

impl DiscoveryTrust {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectManifest => "project_manifest",
            Self::ConfiguredAllowlist => "configured_allowlist",
            Self::DisplayOnly => "display_only",
        }
    }
}

impl DiscoveryCandidate {
    pub fn adapter(source: impl Into<String>, adapter_id: AdapterId) -> Self {
        Self {
            id: format!("adapter:{}", adapter_id.as_str()),
            kind: DiscoveryCandidateKind::Adapter,
            source: source.into(),
            trust: DiscoveryTrust::ProjectManifest,
            adapter_id: Some(adapter_id),
            capability: None,
            handle_granted: false,
            rejected_reason: None,
        }
    }

    pub fn capability(
        source: impl Into<String>,
        capability: CapabilityName,
        adapter_id: Option<AdapterId>,
        kind: DiscoveryCandidateKind,
        trust: DiscoveryTrust,
    ) -> Self {
        let id = match &adapter_id {
            Some(adapter_id) => format!(
                "{}:{}:{}",
                kind.as_str(),
                adapter_id.as_str(),
                capability.as_str()
            ),
            None => format!("{}:{}", kind.as_str(), capability.as_str()),
        };
        Self {
            id,
            kind,
            source: source.into(),
            trust,
            adapter_id,
            capability: Some(capability),
            handle_granted: false,
            rejected_reason: None,
        }
    }

    pub fn named(
        source: impl Into<String>,
        kind: DiscoveryCandidateKind,
        name: impl Into<String>,
        adapter_id: Option<AdapterId>,
        trust: DiscoveryTrust,
    ) -> Self {
        let source = source.into();
        let name = name.into();
        let id = match &adapter_id {
            Some(adapter_id) => format!("{}:{}:{}", kind.as_str(), adapter_id.as_str(), name),
            None => format!("{}:{}", kind.as_str(), name),
        };
        Self {
            id,
            kind,
            source,
            trust,
            adapter_id,
            capability: None,
            handle_granted: false,
            rejected_reason: None,
        }
    }

    pub fn rejected(mut self, reason: impl Into<String>) -> Self {
        self.rejected_reason = Some(reason.into());
        self
    }
}

pub fn dedupe(mut candidates: Vec<DiscoveryCandidate>) -> Vec<DiscoveryCandidate> {
    candidates.sort_by(|left, right| left.id.cmp(&right.id).then(left.source.cmp(&right.source)));
    candidates.dedup_by(|left, right| left.id == right.id);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_never_grant_runtime_handles() {
        let candidate = DiscoveryCandidate::adapter(
            "project_adapters",
            AdapterId::parse("github-mcp").unwrap(),
        );

        assert!(!candidate.handle_granted);
    }
}
