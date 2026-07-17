//! Immutable, all-or-nothing runtime configuration generations.

use eva_config::routes::RouteConfig;
use eva_config::{validate_project_config, ConfigGeneration, ProjectConfig};
use eva_core::EvaError;
use eva_discovery::{DiscoveryScanReport, DiscoveryService};
use eva_policy::{EffectivePolicy, PolicyDomainSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfigGeneration {
    pub identity: ConfigGeneration,
    pub project: Arc<ProjectConfig>,
    pub routes: RouteConfig,
    pub policy_domains: PolicyDomainSet,
    pub effective_policy: EffectivePolicy,
    pub discovery: DiscoveryScanReport,
}

impl RuntimeConfigGeneration {
    pub fn build(project: ProjectConfig, generation: u64) -> Result<Self, EvaError> {
        validate_project_config(&project)?;
        let identity = ConfigGeneration::from_project(&project, generation)?;
        let policy_domains = PolicyDomainSet::from_project(&project)?;
        let effective_policy = policy_domains.effective_policy()?;
        let mut discovery_service = DiscoveryService::new();
        let discovery = discovery_service.scan_project(&project);
        if let Some(failed) = discovery
            .source_reports
            .iter()
            .find(|report| matches!(report.status.as_str(), "error" | "timeout"))
        {
            return Err(
                EvaError::unavailable("runtime config discovery preflight failed")
                    .with_context("source_id", &failed.source_id)
                    .with_context("status", &failed.status)
                    .with_context(
                        "reason",
                        failed.error.as_deref().unwrap_or("discovery source failed"),
                    ),
            );
        }
        let routes = project.routes.clone();
        Ok(Self {
            identity,
            project: Arc::new(project),
            routes,
            policy_domains,
            effective_policy,
            discovery,
        })
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
    fn generation_contains_one_consistent_config_policy_route_and_discovery_snapshot() {
        let project = load_project_config(workspace_root()).unwrap();
        let snapshot = RuntimeConfigGeneration::build(project.clone(), 1).unwrap();
        assert_eq!(snapshot.identity.environment, project.eva.runtime.env);
        assert_eq!(snapshot.routes, project.routes);
        assert!(!snapshot.effective_policy.layer_names.is_empty());
        assert!(!snapshot.discovery.candidates.is_empty());
        assert_eq!(snapshot.project.as_ref(), &project);
    }

    #[test]
    fn generation_zero_and_invalid_route_never_return_partial_candidate() {
        let project = load_project_config(workspace_root()).unwrap();
        assert!(RuntimeConfigGeneration::build(project.clone(), 0).is_err());
        let mut invalid = project;
        invalid.routes.routes[0]
            .agents
            .push("missing-generation-agent".try_into().unwrap());
        assert!(RuntimeConfigGeneration::build(invalid, 1).is_err());
    }
}
