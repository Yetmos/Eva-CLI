//! Discovery service coordination.

use crate::cache::DiscoveryCache;
use crate::health::{DiscoveryHealth, DiscoveryHealthStatus};
use crate::normalizer::DiscoveryCandidate;
use crate::scanner::{scan_sources, DiscoveryScanReport, DiscoverySource, ProjectDiscoverySource};
use eva_config::ProjectConfig;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "coordinate trusted source discovery without authorization";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryService {
    cache: DiscoveryCache,
}

impl DiscoveryService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan_project(&mut self, project: &ProjectConfig) -> DiscoveryScanReport {
        let source = ProjectDiscoverySource::new(project);
        let report = scan_sources(&[&source as &dyn DiscoverySource]);
        self.cache
            .replace(report.candidates.clone(), "project scan");
        report
    }

    pub fn cache(&self) -> &DiscoveryCache {
        &self.cache
    }

    pub fn candidates(&self) -> &[DiscoveryCandidate] {
        self.cache.snapshot()
    }

    pub fn health(&self) -> Vec<DiscoveryHealth> {
        self.cache
            .snapshot()
            .iter()
            .map(|candidate| DiscoveryHealth {
                candidate_id: candidate.id.clone(),
                status: if candidate.rejected_reason.is_some() {
                    DiscoveryHealthStatus::Rejected
                } else {
                    DiscoveryHealthStatus::Seen
                },
                message: candidate.rejected_reason.clone().unwrap_or_else(|| {
                    "candidate discovered; authorization remains external".to_owned()
                }),
            })
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
    fn service_caches_candidates_without_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        let report = service.scan_project(&project);

        assert_eq!(service.candidates().len(), report.candidates.len());
        assert!(service
            .candidates()
            .iter()
            .all(|candidate| !candidate.handle_granted));
    }
}
