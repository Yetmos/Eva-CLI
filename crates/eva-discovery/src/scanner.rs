//! Discovery source orchestration.

use crate::normalizer::{dedupe, DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use eva_config::{AdapterTransport, CapabilityKind, ProjectConfig};
use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "scan trusted sources for candidate capabilities";

pub trait DiscoverySource {
    fn source_id(&self) -> &str;
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverySourceReport {
    pub source_id: String,
    pub candidates: Vec<DiscoveryCandidate>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryScanReport {
    pub candidates: Vec<DiscoveryCandidate>,
    pub source_reports: Vec<DiscoverySourceReport>,
}

pub struct ProjectDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> ProjectDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for ProjectDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "project_config"
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            candidates.push(DiscoveryCandidate::adapter(
                "project_adapters",
                adapter.id.clone(),
            ));
            for capability in &adapter.capabilities {
                let kind = match adapter.transport {
                    AdapterTransport::Mcp => DiscoveryCandidateKind::McpTool,
                    AdapterTransport::Skill => DiscoveryCandidateKind::Skill,
                    _ => DiscoveryCandidateKind::Capability,
                };
                let trust = if adapter.enabled {
                    DiscoveryTrust::ProjectManifest
                } else {
                    DiscoveryTrust::DisplayOnly
                };
                let candidate = DiscoveryCandidate::capability(
                    "project_adapters",
                    capability.clone(),
                    Some(adapter.id.clone()),
                    kind,
                    trust,
                );
                candidates.push(if adapter.enabled {
                    candidate
                } else {
                    candidate.rejected("adapter manifest is disabled")
                });
            }
        }
        for capability in &self.project.capabilities {
            let kind = match capability.kind {
                CapabilityKind::McpTool => DiscoveryCandidateKind::McpTool,
                CapabilityKind::Skill => DiscoveryCandidateKind::Skill,
                _ => DiscoveryCandidateKind::Capability,
            };
            candidates.push(DiscoveryCandidate::capability(
                "project_capabilities",
                capability.capability.clone(),
                capability
                    .default_provider
                    .clone()
                    .or_else(|| capability.provider.clone()),
                kind,
                DiscoveryTrust::ConfiguredAllowlist,
            ));
        }
        Ok(candidates)
    }
}

pub fn scan_sources(sources: &[&dyn DiscoverySource]) -> DiscoveryScanReport {
    let mut all = Vec::new();
    let mut source_reports = Vec::new();
    for source in sources {
        match source.scan() {
            Ok(candidates) => {
                all.extend(candidates.clone());
                source_reports.push(DiscoverySourceReport {
                    source_id: source.source_id().to_owned(),
                    candidates,
                    error: None,
                });
            }
            Err(error) => source_reports.push(DiscoverySourceReport {
                source_id: source.source_id().to_owned(),
                candidates: Vec::new(),
                error: Some(error.message().to_owned()),
            }),
        }
    }
    DiscoveryScanReport {
        candidates: dedupe(all),
        source_reports,
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
    fn project_source_finds_mcp_and_skill_candidates() {
        let project = load_project_config(workspace_root()).unwrap();
        let source = ProjectDiscoverySource::new(&project);
        let candidates = source.scan().unwrap();

        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == DiscoveryCandidateKind::McpTool));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == DiscoveryCandidateKind::Skill));
        assert!(candidates.iter().all(|candidate| !candidate.handle_granted));
    }
}
