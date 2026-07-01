//! Main `eva.yaml` loading and normalization.

use crate::{read_yaml_file, require_non_empty, require_non_empty_path, EvaError};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load and normalize the main Eva YAML configuration";

const CONFIG_TYPE: &str = "eva.yaml";

/// Project-level Eva runtime configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaConfig {
    /// Path to the source file.
    pub path: PathBuf,
    /// Runtime path and hot-reload settings.
    pub runtime: RuntimeConfig,
    /// Split configuration roots.
    pub config: ConfigRoots,
    /// Additional top-level objects owned by downstream crates.
    pub extra: Mapping,
}

/// Stable subset of the `runtime` object used during configuration loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub env: String,
    pub workspace: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub script_dir: Option<PathBuf>,
    pub adapter_dir: Option<PathBuf>,
    pub hot_reload: bool,
}

/// Paths to split configuration files and directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRoots {
    pub agent_dir: PathBuf,
    pub adapter_dir: PathBuf,
    pub capability_dir: PathBuf,
    pub policy_dir: PathBuf,
    pub route_file: PathBuf,
    pub schema_dir: PathBuf,
}

/// Loads and validates the main `eva.yaml` file.
pub fn load_eva_config(path: impl AsRef<Path>) -> Result<EvaConfig, EvaError> {
    let path = path.as_ref();
    let raw: RawEvaConfig = read_yaml_file(path, CONFIG_TYPE)?;
    EvaConfig::try_from_raw(path.to_path_buf(), raw)
}

impl EvaConfig {
    fn try_from_raw(path: PathBuf, raw: RawEvaConfig) -> Result<Self, EvaError> {
        let runtime = RuntimeConfig::try_from_raw(&path, raw.runtime)?;
        let config = ConfigRoots::try_from_raw(&path, raw.config)?;
        Ok(Self {
            path,
            runtime,
            config,
            extra: raw.extra,
        })
    }
}

impl RuntimeConfig {
    fn try_from_raw(path: &Path, raw: RawRuntimeConfig) -> Result<Self, EvaError> {
        let env = require_non_empty(raw.env, CONFIG_TYPE, path, "runtime.env")?;
        let workspace =
            require_non_empty_path(raw.workspace, CONFIG_TYPE, path, "runtime.workspace")?;

        Ok(Self {
            env,
            workspace,
            data_dir: raw.data_dir,
            script_dir: raw.script_dir,
            adapter_dir: raw.adapter_dir,
            hot_reload: raw.hot_reload,
        })
    }
}

impl ConfigRoots {
    fn try_from_raw(path: &Path, raw: RawConfigRoots) -> Result<Self, EvaError> {
        Ok(Self {
            agent_dir: require_non_empty_path(
                raw.agent_dir,
                CONFIG_TYPE,
                path,
                "config.agent_dir",
            )?,
            adapter_dir: require_non_empty_path(
                raw.adapter_dir,
                CONFIG_TYPE,
                path,
                "config.adapter_dir",
            )?,
            capability_dir: require_non_empty_path(
                raw.capability_dir,
                CONFIG_TYPE,
                path,
                "config.capability_dir",
            )?,
            policy_dir: require_non_empty_path(
                raw.policy_dir,
                CONFIG_TYPE,
                path,
                "config.policy_dir",
            )?,
            route_file: require_non_empty_path(
                raw.route_file,
                CONFIG_TYPE,
                path,
                "config.route_file",
            )?,
            schema_dir: require_non_empty_path(
                raw.schema_dir,
                CONFIG_TYPE,
                path,
                "config.schema_dir",
            )?,
        })
    }

    /// Resolves relative config roots against a project root.
    pub fn resolve_against(&self, project_root: &Path) -> Self {
        Self {
            agent_dir: resolve_path(project_root, &self.agent_dir),
            adapter_dir: resolve_path(project_root, &self.adapter_dir),
            capability_dir: resolve_path(project_root, &self.capability_dir),
            policy_dir: resolve_path(project_root, &self.policy_dir),
            route_file: resolve_path(project_root, &self.route_file),
            schema_dir: resolve_path(project_root, &self.schema_dir),
        }
    }
}

fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[derive(Debug, Deserialize)]
struct RawEvaConfig {
    runtime: RawRuntimeConfig,
    config: RawConfigRoots,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Deserialize)]
struct RawRuntimeConfig {
    env: String,
    workspace: PathBuf,
    data_dir: Option<PathBuf>,
    script_dir: Option<PathBuf>,
    adapter_dir: Option<PathBuf>,
    hot_reload: bool,
}

#[derive(Debug, Deserialize)]
struct RawConfigRoots {
    agent_dir: PathBuf,
    adapter_dir: PathBuf,
    capability_dir: PathBuf,
    policy_dir: PathBuf,
    route_file: PathBuf,
    schema_dir: PathBuf,
}

impl TryFrom<Value> for EvaConfig {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawEvaConfig::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse eva.yaml")
                .with_context("yaml_error", error.to_string())
        })?;
        Self::try_from_raw(PathBuf::new(), raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use serde_yaml::Value;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn load_eva_config_accepts_sample_config() {
        let config = load_eva_config(workspace_root().join("config").join("eva.yaml")).unwrap();

        assert_eq!(config.runtime.env, "dev");
        assert_eq!(config.config.agent_dir, PathBuf::from("config/agents"));
        assert!(config
            .extra
            .contains_key(Value::String("eventbus".to_owned())));
    }

    #[test]
    fn load_eva_config_rejects_missing_required_runtime() {
        let value = serde_yaml::from_str::<Value>(
            r#"
config:
  agent_dir: config/agents
  adapter_dir: config/adapters
  capability_dir: config/capabilities
  policy_dir: config/policies
  route_file: config/routes/topics.yaml
  schema_dir: config/schemas
"#,
        )
        .unwrap();

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn config_roots_resolve_relative_paths() {
        let config = load_eva_config(workspace_root().join("config").join("eva.yaml")).unwrap();
        let roots = config.config.resolve_against(Path::new("C:/workspace"));

        assert_eq!(roots.agent_dir, PathBuf::from("C:/workspace/config/agents"));
        assert_eq!(
            roots.route_file,
            PathBuf::from("C:/workspace/config/routes/topics.yaml")
        );
    }

    #[test]
    fn load_eva_config_rejects_blank_env() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: ""
  workspace: .
  hot_reload: true
config:
  agent_dir: config/agents
  adapter_dir: config/adapters
  capability_dir: config/capabilities
  policy_dir: config/policies
  route_file: config/routes/topics.yaml
  schema_dir: config/schemas
"#,
        )
        .unwrap();

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "field")
                .unwrap()
                .1,
            "runtime.env"
        );
    }
}
