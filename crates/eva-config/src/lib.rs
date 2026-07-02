//! Configuration loading and normalization boundary.

pub mod eva_yaml;
pub mod manifest;
pub mod policy;
pub mod routes;
pub mod schema;

use crate::eva_yaml::{ConfigRoots, EvaConfig};
use crate::manifest::adapter::{load_adapter_manifest, AdapterManifest};
use crate::manifest::agent::{load_agent_manifest, AgentManifest};
use crate::manifest::capability::{load_capability_manifest, CapabilityManifest};
use crate::policy::{load_policy_document, PolicyDocument};
use crate::routes::{load_routes, RouteConfig};
use eva_core::EvaError;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub use eva_yaml::{load_eva_config, RuntimeConfig};
pub use manifest::adapter::{AdapterTransport, RawAdapterTransport};
pub use manifest::agent::AgentManifestPermissions;
pub use manifest::capability::{CapabilityKind, RawCapabilityKind};
pub use routes::{RawRouteDelivery, RouteDelivery, RouteRule};
pub use schema::{schema_paths, SchemaPaths};

/// Project-level configuration assembled from `eva.yaml` and split manifests.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectConfig {
    /// Canonical project root used for resolving split configuration roots.
    pub project_root: PathBuf,
    /// Path to the loaded `eva.yaml` file.
    pub eva_config_path: PathBuf,
    /// Parsed main configuration.
    pub eva: EvaConfig,
    /// Config roots resolved against `project_root`.
    pub roots: ConfigRoots,
    /// Loaded Agent manifests.
    pub agents: Vec<AgentManifest>,
    /// Loaded Adapter manifests.
    pub adapters: Vec<AdapterManifest>,
    /// Loaded capability manifests.
    pub capabilities: Vec<CapabilityManifest>,
    /// Loaded policy documents.
    pub policies: Vec<PolicyDocument>,
    /// Loaded route table.
    pub routes: RouteConfig,
}

/// Loads the minimum project configuration set from a project root directory.
pub fn load_project_config(project_root: impl AsRef<Path>) -> Result<ProjectConfig, EvaError> {
    let project_root = normalize_existing_dir(project_root.as_ref(), "project root")?;
    let eva_config_path = project_root.join("config").join("eva.yaml");
    let eva = load_eva_config(&eva_config_path)?;
    let roots = eva.config.resolve_against(&project_root);

    let agents = find_named_files(&roots.agent_dir, "agent.yaml")?
        .into_iter()
        .map(load_agent_manifest)
        .collect::<Result<Vec<_>, _>>()?;

    let adapters = find_yaml_files(&roots.adapter_dir)?
        .into_iter()
        .map(load_adapter_manifest)
        .collect::<Result<Vec<_>, _>>()?;

    let capabilities = find_yaml_files(&roots.capability_dir)?
        .into_iter()
        .map(load_capability_manifest)
        .collect::<Result<Vec<_>, _>>()?;
    let policies = find_yaml_files(&roots.policy_dir)?
        .into_iter()
        .map(load_policy_document)
        .collect::<Result<Vec<_>, _>>()?;
    let routes = load_routes(&roots.route_file)?;

    let project = ProjectConfig {
        project_root,
        eva_config_path,
        eva,
        roots,
        agents,
        adapters,
        capabilities,
        policies,
        routes,
    };
    validate_project_config(&project)?;
    Ok(project)
}

/// Checks cross-file consistency that is not visible while loading one file.
pub fn validate_project_config(project: &ProjectConfig) -> Result<(), EvaError> {
    validate_roots(project)?;
    validate_unique_agents(&project.agents)?;
    validate_unique_adapters(&project.adapters)?;
    validate_unique_capabilities(&project.capabilities)?;
    validate_agent_references(&project.agents)?;
    validate_agent_scripts(&project.agents)?;
    validate_capability_providers(project)?;
    validate_route_agents(project)?;
    Ok(())
}

pub(crate) fn read_yaml_file<T>(
    path: impl AsRef<Path>,
    config_type: &'static str,
) -> Result<T, EvaError>
where
    T: DeserializeOwned,
{
    let path = path.as_ref();
    let content = fs::read_to_string(path).map_err(|error| io_error(error, path, config_type))?;
    serde_yaml::from_str(&content).map_err(|error| yaml_error(error, path, config_type))
}

pub(crate) fn invalid_config(
    config_type: &'static str,
    path: &Path,
    field: impl Into<String>,
    message: impl Into<String>,
) -> EvaError {
    EvaError::invalid_argument(message)
        .with_context("config_type", config_type)
        .with_context("path", path.display().to_string())
        .with_context("field", field.into())
}

pub(crate) fn with_field_context(
    error: EvaError,
    config_type: &'static str,
    path: &Path,
    field: impl Into<String>,
) -> EvaError {
    error
        .with_context("config_type", config_type)
        .with_context("path", path.display().to_string())
        .with_context("field", field.into())
}

pub(crate) fn require_non_empty(
    value: String,
    config_type: &'static str,
    path: &Path,
    field: &'static str,
) -> Result<String, EvaError> {
    if value.trim().is_empty() {
        Err(invalid_config(
            config_type,
            path,
            field,
            "required field cannot be empty",
        ))
    } else if value.trim() != value {
        Err(invalid_config(
            config_type,
            path,
            field,
            "field cannot contain leading or trailing whitespace",
        ))
    } else {
        Ok(value)
    }
}

pub(crate) fn require_non_empty_path(
    value: PathBuf,
    config_type: &'static str,
    path: &Path,
    field: &'static str,
) -> Result<PathBuf, EvaError> {
    if value.as_os_str().is_empty() {
        Err(invalid_config(
            config_type,
            path,
            field,
            "path field cannot be empty",
        ))
    } else {
        Ok(value)
    }
}

fn validate_roots(project: &ProjectConfig) -> Result<(), EvaError> {
    require_existing_dir(&project.roots.agent_dir, "agent_dir")?;
    require_existing_dir(&project.roots.adapter_dir, "adapter_dir")?;
    require_existing_dir(&project.roots.capability_dir, "capability_dir")?;
    require_existing_dir(&project.roots.policy_dir, "policy_dir")?;
    require_existing_dir(&project.roots.schema_dir, "schema_dir")?;
    require_existing_file(&project.roots.route_file, "route_file")?;
    Ok(())
}

fn validate_unique_agents(agents: &[AgentManifest]) -> Result<(), EvaError> {
    let mut seen = BTreeMap::new();
    for agent in agents {
        let id = agent.id.as_str().to_owned();
        if let Some(first_path) = seen.insert(id.clone(), agent.path.clone()) {
            return Err(EvaError::conflict("duplicate Agent manifest id")
                .with_context("agent_id", id)
                .with_context("first_path", first_path.display().to_string())
                .with_context("second_path", agent.path.display().to_string()));
        }
    }
    Ok(())
}

fn validate_unique_adapters(adapters: &[AdapterManifest]) -> Result<(), EvaError> {
    let mut seen = BTreeMap::new();
    for adapter in adapters {
        let id = adapter.id.as_str().to_owned();
        if let Some(first_path) = seen.insert(id.clone(), adapter.path.clone()) {
            return Err(EvaError::conflict("duplicate Adapter manifest id")
                .with_context("adapter_id", id)
                .with_context("first_path", first_path.display().to_string())
                .with_context("second_path", adapter.path.display().to_string()));
        }
    }
    Ok(())
}

fn validate_unique_capabilities(capabilities: &[CapabilityManifest]) -> Result<(), EvaError> {
    let mut seen = BTreeMap::new();
    for capability in capabilities {
        let id = capability.id.as_str().to_owned();
        if let Some(first_path) = seen.insert(id.clone(), capability.path.clone()) {
            return Err(EvaError::conflict("duplicate Capability manifest id")
                .with_context("capability_id", id)
                .with_context("first_path", first_path.display().to_string())
                .with_context("second_path", capability.path.display().to_string()));
        }
    }
    Ok(())
}

fn validate_agent_references(agents: &[AgentManifest]) -> Result<(), EvaError> {
    let known = agents
        .iter()
        .map(|agent| agent.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for agent in agents {
        if let Some(parent) = &agent.parent {
            if !known.contains(parent.as_str()) {
                return Err(EvaError::not_found("Agent parent reference does not exist")
                    .with_context("agent_id", agent.id.as_str())
                    .with_context("parent", parent.as_str())
                    .with_context("path", agent.path.display().to_string()));
            }
        }

        for child in &agent.children {
            if !known.contains(child.as_str()) {
                return Err(EvaError::not_found("Agent child reference does not exist")
                    .with_context("agent_id", agent.id.as_str())
                    .with_context("child", child.as_str())
                    .with_context("path", agent.path.display().to_string()));
            }
        }
    }

    Ok(())
}

fn validate_agent_scripts(agents: &[AgentManifest]) -> Result<(), EvaError> {
    for agent in agents {
        let manifest_dir = agent.path.parent().unwrap_or_else(|| Path::new(""));
        let script_path = if agent.script.is_absolute() {
            agent.script.clone()
        } else {
            manifest_dir.join(&agent.script)
        };
        if !script_path.is_file() {
            return Err(EvaError::not_found("Agent script does not exist")
                .with_context("agent_id", agent.id.as_str())
                .with_context("script", agent.script.display().to_string())
                .with_context("resolved_path", script_path.display().to_string())
                .with_context("path", agent.path.display().to_string()));
        }
    }
    Ok(())
}

fn validate_capability_providers(project: &ProjectConfig) -> Result<(), EvaError> {
    let adapters = project
        .adapters
        .iter()
        .map(|adapter| adapter.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for capability in &project.capabilities {
        for provider in capability.adapter_providers() {
            if !adapters.contains(provider.as_str()) {
                return Err(
                    EvaError::not_found("Capability provider Adapter does not exist")
                        .with_context("capability_id", capability.id.as_str())
                        .with_context("provider", provider.as_str())
                        .with_context("path", capability.path.display().to_string()),
                );
            }
        }
    }

    Ok(())
}

fn validate_route_agents(project: &ProjectConfig) -> Result<(), EvaError> {
    let agents = project
        .agents
        .iter()
        .map(|agent| agent.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for route in &project.routes.routes {
        for agent in &route.agents {
            if !agents.contains(agent.as_str()) {
                return Err(EvaError::not_found("Route target Agent does not exist")
                    .with_context("agent_id", agent.as_str())
                    .with_context("pattern", route.pattern.as_str())
                    .with_context("path", project.routes.path.display().to_string()));
            }
        }
    }

    Ok(())
}

fn find_named_files(root: &Path, filename: &str) -> Result<Vec<PathBuf>, EvaError> {
    let mut files = Vec::new();
    collect_files(root, &mut files, &|path| {
        path.file_name().and_then(|name| name.to_str()) == Some(filename)
    })?;
    files.sort();
    Ok(files)
}

fn find_yaml_files(root: &Path) -> Result<Vec<PathBuf>, EvaError> {
    let mut files = Vec::new();
    collect_files(root, &mut files, &is_yaml_file)?;
    files.sort();
    Ok(files)
}

fn collect_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    include: &dyn Fn(&Path) -> bool,
) -> Result<(), EvaError> {
    let entries = fs::read_dir(root).map_err(|error| io_error(error, root, "config root"))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(error, root, "config root"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| io_error(error, &path, "config root"))?;
        if file_type.is_dir() {
            collect_files(&path, files, include)?;
        } else if file_type.is_file() && include(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_yaml_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("yaml") | Some("yml")
    )
}

fn normalize_existing_dir(path: &Path, field: &'static str) -> Result<PathBuf, EvaError> {
    let path = fs::canonicalize(path).map_err(|error| io_error(error, path, field))?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(EvaError::invalid_argument("path is not a directory")
            .with_context("field", field)
            .with_context("path", path.display().to_string()))
    }
}

fn require_existing_dir(path: &Path, field: &'static str) -> Result<(), EvaError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(
            EvaError::not_found("configuration directory does not exist")
                .with_context("field", field)
                .with_context("path", path.display().to_string()),
        )
    }
}

fn require_existing_file(path: &Path, field: &'static str) -> Result<(), EvaError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(EvaError::not_found("configuration file does not exist")
            .with_context("field", field)
            .with_context("path", path.display().to_string()))
    }
}

fn io_error(error: std::io::Error, path: &Path, config_type: &'static str) -> EvaError {
    let message = format!("failed to read {config_type}");
    let base = if error.kind() == std::io::ErrorKind::NotFound {
        EvaError::not_found(message)
    } else {
        EvaError::internal(message)
    };
    base.with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

fn yaml_error(error: serde_yaml::Error, path: &Path, config_type: &'static str) -> EvaError {
    let mut eva_error = EvaError::invalid_argument("failed to parse YAML")
        .with_context("config_type", config_type)
        .with_context("path", path.display().to_string())
        .with_context("yaml_error", error.to_string());
    if let Some(location) = error.location() {
        eva_error = eva_error
            .with_context("line", location.line().to_string())
            .with_context("column", location.column().to_string());
    }
    eva_error
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn project_config_loads_all_config_roots() {
        let project = load_project_config(workspace_root()).unwrap();

        assert!(!project.agents.is_empty());
        assert!(!project.adapters.is_empty());
        assert!(!project.capabilities.is_empty());
        assert!(!project.policies.is_empty());
        assert!(!project.routes.routes.is_empty());
        assert!(project.roots.route_file.is_file());
        assert!(project.roots.schema_dir.is_dir());
    }

    #[test]
    fn validate_project_config_rejects_duplicate_agent_id() {
        let mut project = load_project_config(workspace_root()).unwrap();
        let duplicate = project.agents[0].clone();
        project.agents.push(duplicate);

        let error = validate_project_config(&project).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert!(error.message().contains("duplicate Agent"));
    }

    #[test]
    fn validate_project_config_rejects_unknown_route_agent() {
        let mut project = load_project_config(workspace_root()).unwrap();
        project.routes.routes[0]
            .agents
            .push("missing-agent".try_into().unwrap());

        let error = validate_project_config(&project).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(error.message().contains("Route target"));
    }
}
