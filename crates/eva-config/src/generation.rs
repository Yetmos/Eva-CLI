//! 配置代次标识与规范化来源摘要。
//! Canonical configuration generation and provenance digest.
use crate::{canonical_config_bytes, merge_config_layers, ConfigLayerKind, ProjectConfig};
use eva_core::{sha256_digest, EvaError};
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::Path;

/// 一次完整项目配置快照的稳定身份信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigGeneration {
    /// 由调用方分配的单调递增正整数代次。
    pub generation: u64,
    /// 对去敏后的规范化配置计算得到的 SHA-256 摘要。
    pub digest: String,
    /// 生成快照时生效的运行环境名称。
    pub environment: String,
}

impl ConfigGeneration {
    /// 从已校验的项目配置生成指定代次的稳定摘要。
    ///
    /// 摘要按固定键纳入主配置、拆分清单与路由，且不依赖文件系统遍历顺序。
    pub fn from_project(project: &ProjectConfig, generation: u64) -> Result<Self, EvaError> {
        if generation == 0 {
            return Err(EvaError::invalid_argument(
                "config generation must be positive",
            ));
        }
        let environment = project.eva.runtime.env.clone();
        // 将所有配置来源投影到统一映射，再交给规范化编码器排序和去敏。
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

/// 按与项目加载器一致的优先级重新合并主配置层。
fn merged_main_config(project: &ProjectConfig) -> Result<Value, EvaError> {
    let config_dir = project.project_root.join("config");
    let environment = &project.eva.runtime.env;
    let mut layers = vec![(
        ConfigLayerKind::Base,
        project.eva_config_path.clone(),
        read_value(&project.eva_config_path)?,
    )];
    // 仅纳入实际存在的覆盖文件，保持可选层语义。
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

/// 按相对项目路径排序并读取一类拆分配置源。
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

/// 将单个 YAML 来源读取为未类型化值，并保留路径诊断上下文。
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
