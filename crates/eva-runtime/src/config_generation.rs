//! Immutable, all-or-nothing runtime configuration generations.

use eva_config::routes::RouteConfig;
use eva_config::{load_project_config, validate_project_config, ConfigGeneration, ProjectConfig};
use eva_core::EvaError;
use eva_discovery::{DiscoveryScanReport, DiscoveryService};
use eva_policy::{EffectivePolicy, PolicyDomainSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigReloadPreflight {
    pub old_digest: String,
    pub candidate_digest: Option<String>,
    pub changed_paths: Vec<String>,
    pub outcome: ConfigReloadPreflightOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigReloadPreflightOutcome {
    Ready,
    Rejected {
        error_kind: String,
        error_field: String,
        error_message: String,
        remediation: String,
    },
}

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

pub fn preflight_config_reload(
    active: &RuntimeConfigGeneration,
    project_root: &Path,
    changed_paths: Vec<String>,
) -> ConfigReloadPreflight {
    let next_generation = active.identity.generation.saturating_add(1);
    let loaded = load_project_config(project_root);
    let candidate_digest = loaded
        .as_ref()
        .ok()
        .and_then(|project| ConfigGeneration::from_project(project, next_generation).ok())
        .map(|identity| identity.digest);
    let result =
        loaded.and_then(|project| RuntimeConfigGeneration::build(project, next_generation));
    let outcome = match result {
        Ok(_) => ConfigReloadPreflightOutcome::Ready,
        Err(error) => ConfigReloadPreflightOutcome::Rejected {
            error_kind: error.kind().as_str().to_owned(),
            error_field: error
                .context()
                .entries()
                .first()
                .map(|(key, _)| key.clone())
                .unwrap_or_else(|| "config".to_owned()),
            error_message: error.message().to_owned(),
            remediation: "correct the reported configuration field and save the source again"
                .to_owned(),
        },
    };
    ConfigReloadPreflight {
        old_digest: active.identity.digest.clone(),
        candidate_digest,
        changed_paths,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn isolated_project() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "eva-config-preflight-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        copy_dir(&workspace_root().join("config"), &root.join("config"));
        root
    }

    fn copy_dir(source: &Path, target: &Path) {
        fs::create_dir_all(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
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

    #[test]
    fn rejected_reload_reports_field_and_keeps_active_generation() {
        let root = isolated_project();
        let active =
            RuntimeConfigGeneration::build(load_project_config(&root).unwrap(), 7).unwrap();
        let old_digest = active.identity.digest.clone();
        fs::write(root.join("config/routes/topics.yaml"), "routes: [").unwrap();

        let report =
            preflight_config_reload(&active, &root, vec!["config/routes/topics.yaml".to_owned()]);

        assert_eq!(active.identity.generation, 7);
        assert_eq!(active.identity.digest, old_digest);
        assert_eq!(report.old_digest, old_digest);
        assert!(report.candidate_digest.is_none());
        match report.outcome {
            ConfigReloadPreflightOutcome::Rejected {
                error_field,
                remediation,
                ..
            } => {
                assert!(!error_field.is_empty());
                assert!(remediation.contains("correct"));
            }
            ConfigReloadPreflightOutcome::Ready => panic!("invalid candidate passed preflight"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn valid_reload_exposes_candidate_digest_without_promoting_it() {
        let root = isolated_project();
        let active =
            RuntimeConfigGeneration::build(load_project_config(&root).unwrap(), 3).unwrap();
        let old_digest = active.identity.digest.clone();
        let eva_path = root.join("config/eva.yaml");
        let text = fs::read_to_string(&eva_path).unwrap();
        let changed = if text.contains("hot_reload: true") {
            text.replace("hot_reload: true", "hot_reload: false")
        } else {
            text.replace("hot_reload: false", "hot_reload: true")
        };
        fs::write(&eva_path, changed).unwrap();

        let report = preflight_config_reload(&active, &root, vec!["config/eva.yaml".to_owned()]);

        assert_eq!(report.outcome, ConfigReloadPreflightOutcome::Ready);
        assert_ne!(
            report.candidate_digest.as_deref(),
            Some(old_digest.as_str())
        );
        assert_eq!(active.identity.generation, 3);
        assert_eq!(active.identity.digest, old_digest);
        fs::remove_dir_all(root).unwrap();
    }
}
