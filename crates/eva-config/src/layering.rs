//! Deterministic, explicitly sourced main-configuration layering.

use eva_core::EvaError;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigLayerKind {
    Base,
    Profile,
    User,
    Environment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLayer {
    pub kind: ConfigLayerKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayeredConfig {
    pub value: Value,
    pub layers: Vec<ConfigLayer>,
    pub field_sources: BTreeMap<String, ConfigLayerKind>,
}

pub fn merge_config_layers(
    layers: impl IntoIterator<Item = (ConfigLayerKind, PathBuf, Value)>,
) -> Result<LayeredConfig, EvaError> {
    let mut layers = layers.into_iter().collect::<Vec<_>>();
    layers.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    if layers
        .iter()
        .filter(|layer| layer.0 == ConfigLayerKind::Base)
        .count()
        != 1
    {
        return Err(EvaError::invalid_argument(
            "configuration layers require exactly one base layer",
        ));
    }
    let mut value = Value::Mapping(Mapping::new());
    let mut field_sources = BTreeMap::new();
    let mut provenance = Vec::new();
    for (kind, path, layer) in layers {
        if !layer.is_mapping() {
            return Err(layer_error(
                &path,
                "$",
                "configuration layer root must be a mapping",
            ));
        }
        merge_value(&mut value, layer, kind, &path, "$", &mut field_sources)?;
        provenance.push(ConfigLayer { kind, path });
    }
    Ok(LayeredConfig {
        value,
        layers: provenance,
        field_sources,
    })
}

fn merge_value(
    target: &mut Value,
    incoming: Value,
    kind: ConfigLayerKind,
    path: &Path,
    field: &str,
    sources: &mut BTreeMap<String, ConfigLayerKind>,
) -> Result<(), EvaError> {
    match (target, incoming) {
        (Value::Mapping(target), Value::Mapping(incoming)) => {
            for (key, value) in incoming {
                let name = key.as_str().ok_or_else(|| {
                    layer_error(path, field, "configuration mapping keys must be strings")
                })?;
                let child = if field == "$" {
                    name.to_owned()
                } else {
                    format!("{field}.{name}")
                };
                if let Some(existing) = target.get_mut(&key) {
                    merge_value(existing, value, kind, path, &child, sources)?;
                } else {
                    record_leaf_sources(&value, kind, &child, sources);
                    target.insert(key, value);
                }
            }
            Ok(())
        }
        (target @ Value::Sequence(_), incoming @ Value::Sequence(_))
        | (target @ Value::String(_), incoming @ Value::String(_))
        | (target @ Value::Bool(_), incoming @ Value::Bool(_))
        | (target @ Value::Number(_), incoming @ Value::Number(_))
        | (target @ Value::Null, incoming @ Value::Null) => {
            *target = incoming;
            sources.insert(field.to_owned(), kind);
            Ok(())
        }
        (target, incoming) => Err(layer_error(
            path,
            field,
            &format!(
                "configuration override changes field type from {} to {}",
                value_kind(target),
                value_kind(&incoming)
            ),
        )),
    }
}

fn record_leaf_sources(
    value: &Value,
    kind: ConfigLayerKind,
    field: &str,
    sources: &mut BTreeMap<String, ConfigLayerKind>,
) {
    if let Value::Mapping(mapping) = value {
        for (key, value) in mapping {
            if let Some(name) = key.as_str() {
                record_leaf_sources(value, kind, &format!("{field}.{name}"), sources);
            }
        }
    } else {
        sources.insert(field.to_owned(), kind);
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "sequence",
        Value::Mapping(_) => "mapping",
        Value::Tagged(_) => "tagged",
    }
}
fn layer_error(path: &Path, field: &str, message: &str) -> EvaError {
    EvaError::invalid_argument(message)
        .with_context("path", path.display().to_string())
        .with_context("field", field)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn yaml(value: &str) -> Value {
        serde_yaml::from_str(value).unwrap()
    }
    #[test]
    fn merge_is_deterministic_and_uses_fixed_precedence() {
        let layers = vec![
            (ConfigLayerKind::Environment, PathBuf::from("env"), yaml("runtime:\n  env: production\n")),
            (ConfigLayerKind::Base, PathBuf::from("base"), yaml("runtime:\n  env: dev\n  hot_reload: false\nobservability:\n  log_level: info\n")),
            (ConfigLayerKind::User, PathBuf::from("user"), yaml("observability:\n  log_level: debug\n")),
            (ConfigLayerKind::Profile, PathBuf::from("profile"), yaml("runtime:\n  hot_reload: true\n")),
        ];
        let merged = merge_config_layers(layers.clone()).unwrap();
        assert_eq!(
            merged,
            merge_config_layers(layers.into_iter().rev()).unwrap()
        );
        assert_eq!(merged.value["runtime"]["env"], "production");
        assert_eq!(merged.value["runtime"]["hot_reload"], true);
        assert_eq!(
            merged.field_sources["runtime.env"],
            ConfigLayerKind::Environment
        );
    }
    #[test]
    fn invalid_base_and_type_change_fail_closed() {
        assert!(merge_config_layers(vec![(
            ConfigLayerKind::Profile,
            PathBuf::from("p"),
            yaml("{}")
        )])
        .is_err());
        let error = merge_config_layers(vec![
            (
                ConfigLayerKind::Base,
                PathBuf::from("base"),
                yaml("runtime:\n  env: dev\n"),
            ),
            (
                ConfigLayerKind::Environment,
                PathBuf::from("env"),
                yaml("runtime:\n  env:\n    value: prod\n"),
            ),
        ])
        .unwrap_err();
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "field" && value == "runtime.env"));
    }
}
