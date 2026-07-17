//! Canonical configuration generation and provenance digest.
use crate::{canonical_config_bytes, merge_config_layers, ConfigLayerKind, ProjectConfig};
use eva_core::{sha256_digest, EvaError};
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigGeneration {
    pub generation: u64,
    pub digest: String,
    pub environment: String,
}

impl ConfigGeneration {
    pub fn from_project(project: &ProjectConfig, generation: u64) -> Result<Self, EvaError> {
        if generation == 0 {
            return Err(EvaError::invalid_argument(
                "config generation must be positive",
            ));
        }
        let environment = project.eva.runtime.env.clone();
        let mut canonical = Mapping::new();
        canonical.insert(
            Value::String("environment".to_owned()),
            Value::String(environment.clone()),
        );
        canonical.insert(
            Value::String("eva".to_owned()),
            merged_main_config(project)?,
        );
        canonical.insert(
            Value::String("agents".to_owned()),
            source_values(
                project,
                project.agents.iter().map(|item| item.path.as_path()),
            )?,
        );
        canonical.insert(
            Value::String("adapters".to_owned()),
            source_values(
                project,
                project.adapters.iter().map(|item| item.path.as_path()),
            )?,
        );
        canonical.insert(
            Value::String("capabilities".to_owned()),
            source_values(
                project,
                project.capabilities.iter().map(|item| item.path.as_path()),
            )?,
        );
        canonical.insert(
            Value::String("policies".to_owned()),
            source_values(
                project,
                project.policies.iter().map(|item| item.path.as_path()),
            )?,
        );
        canonical.insert(
            Value::String("routes".to_owned()),
            read_value(&project.routes.path)?,
        );
        let digest = sha256_digest(&canonical_config_bytes(&Value::Mapping(canonical))?);
        Ok(Self {
            generation,
            digest,
            environment,
        })
    }
}

fn merged_main_config(project: &ProjectConfig) -> Result<Value, EvaError> {
    let config_dir = project.project_root.join("config");
    let environment = &project.eva.runtime.env;
    let mut layers = vec![(
        ConfigLayerKind::Base,
        project.eva_config_path.clone(),
        read_value(&project.eva_config_path)?,
    )];
    for (kind, path) in [
        (
            ConfigLayerKind::Profile,
            config_dir
                .join("profiles")
                .join(format!("{environment}.yaml")),
        ),
        (ConfigLayerKind::User, config_dir.join("eva.user.yaml")),
        (
            ConfigLayerKind::Environment,
            config_dir
                .join("environments")
                .join(format!("{environment}.yaml")),
        ),
    ] {
        if path.exists() {
            layers.push((kind, path.clone(), read_value(&path)?));
        }
    }
    Ok(merge_config_layers(layers)?.value)
}

fn source_values<'a>(
    project: &ProjectConfig,
    paths: impl Iterator<Item = &'a Path>,
) -> Result<Value, EvaError> {
    let mut paths = paths.map(Path::to_path_buf).collect::<Vec<_>>();
    paths.sort_by_key(|path| {
        path.strip_prefix(&project.project_root)
            .unwrap_or(path)
            .to_path_buf()
    });
    paths
        .into_iter()
        .map(|path| read_value(&path))
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Sequence)
}

fn read_value(path: &Path) -> Result<Value, EvaError> {
    let text = fs::read_to_string(path).map_err(|error| {
        EvaError::not_found("read canonical config source")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    serde_yaml::from_str(&text).map_err(|error| {
        EvaError::invalid_argument("parse canonical config source")
            .with_context("path", path.display().to_string())
            .with_context("yaml_error", error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_project_config;
    #[test]
    fn digest_is_stable_for_same_project() {
        let root = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let p = load_project_config(root).unwrap();
        let a = ConfigGeneration::from_project(&p, 1).unwrap();
        let b = ConfigGeneration::from_project(&p, 1).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.digest.len(), 71);
        assert!(a.digest.starts_with("sha256:"));
    }

    #[test]
    fn digest_changes_when_runtime_environment_changes() {
        let root = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let project = load_project_config(root).unwrap();
        let baseline = ConfigGeneration::from_project(&project, 1).unwrap();
        let mut changed = project.clone();
        changed.eva.runtime.env.push_str("-override");
        let overridden = ConfigGeneration::from_project(&changed, 1).unwrap();
        assert_ne!(baseline.environment, overridden.environment);
        assert_ne!(baseline.digest, overridden.digest);
    }
}
