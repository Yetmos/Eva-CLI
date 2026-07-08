//! Codex workflow surface discovery from trusted local Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::ProjectConfig;
use eva_core::EvaError;

pub const RESPONSIBILITY: &str = "discover trusted Codex capabilities";

pub struct CodexDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> CodexDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for CodexDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "codex"
    }

    fn timeout_ms(&self) -> u64 {
        250
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            let command = adapter.extra_string("command");
            let is_codex_adapter =
                adapter.id.as_str().contains("codex") || command == Some("codex");
            if !is_codex_adapter {
                continue;
            }
            let mut candidate = DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::Workflow,
                "codex-cli",
                Some(adapter.id.clone()),
                DiscoveryTrust::ConfiguredAllowlist,
            );
            if !adapter.enabled {
                candidate = candidate.rejected("Codex adapter manifest is disabled");
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}
