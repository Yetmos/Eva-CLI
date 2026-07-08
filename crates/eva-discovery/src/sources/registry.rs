//! External registry discovery boundary.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::ProjectConfig;
use eva_core::EvaError;

pub const RESPONSIBILITY: &str = "discover configured external registry candidates";

pub struct ExternalRegistryDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> ExternalRegistryDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for ExternalRegistryDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "external_registry"
    }

    fn timeout_ms(&self) -> u64 {
        100
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let registry_marker = self.project.project_root.join("config/registries");
        let mut candidate = DiscoveryCandidate::named(
            self.source_id(),
            DiscoveryCandidateKind::RegistryEntry,
            "configured-registry",
            None,
            DiscoveryTrust::DisplayOnly,
        );
        if !registry_marker.is_dir() {
            candidate = candidate.rejected("external registry source is not configured");
        }
        Ok(vec![candidate])
    }
}
