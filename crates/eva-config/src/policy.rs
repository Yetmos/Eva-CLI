//! Policy document loading and normalization.

use crate::{read_yaml_file, EvaError};
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CONFIG_TYPE: &str = "Policy document";

/// Extensible policy document loaded from `config/policies/*.yaml`.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDocument {
    /// Path to the source policy file.
    pub path: PathBuf,
    /// Top-level policy domains keyed by YAML field name.
    pub domains: BTreeMap<String, Value>,
}

/// Loads one policy document. Domain-specific interpretation belongs to
/// `eva-policy`; `eva-config` only guarantees that the document is a non-empty
/// YAML mapping with stable string keys.
pub fn load_policy_document(path: impl AsRef<Path>) -> Result<PolicyDocument, EvaError> {
    let path = path.as_ref();
    let value: Value = read_yaml_file(path, CONFIG_TYPE)?;
    PolicyDocument::try_from_value(path.to_path_buf(), value)
}

impl PolicyDocument {
    fn try_from_value(path: PathBuf, value: Value) -> Result<Self, EvaError> {
        let mapping = match value {
            Value::Mapping(mapping) => mapping,
            _ => {
                return Err(
                    EvaError::invalid_argument("policy document must be a mapping")
                        .with_context("config_type", CONFIG_TYPE)
                        .with_context("path", path.display().to_string()),
                );
            }
        };

        if mapping.is_empty() {
            return Err(
                EvaError::invalid_argument("policy document cannot be empty")
                    .with_context("config_type", CONFIG_TYPE)
                    .with_context("path", path.display().to_string()),
            );
        }

        let mut domains = BTreeMap::new();
        for (key, value) in mapping {
            let key = match key {
                Value::String(key) if !key.trim().is_empty() && key.trim() == key => key,
                _ => {
                    return Err(EvaError::invalid_argument(
                        "policy domain keys must be non-empty strings",
                    )
                    .with_context("config_type", CONFIG_TYPE)
                    .with_context("path", path.display().to_string()));
                }
            };
            domains.insert(key, value);
        }

        Ok(Self { path, domains })
    }
}

impl TryFrom<Value> for PolicyDocument {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::try_from_value(PathBuf::new(), value)
    }
}

impl From<Mapping> for PolicyDocument {
    fn from(mapping: Mapping) -> Self {
        let domains = mapping
            .into_iter()
            .filter_map(|(key, value)| match key {
                Value::String(key) => Some((key, value)),
                _ => None,
            })
            .collect();
        Self {
            path: PathBuf::new(),
            domains,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn load_policy_document_accepts_sample_policy() {
        let policy =
            load_policy_document(workspace_root().join("config/policies/sandbox.yaml")).unwrap();

        assert!(policy.domains.contains_key("lua_sandbox"));
    }

    #[test]
    fn policy_document_rejects_empty_mapping() {
        let value = serde_yaml::from_str::<Value>("{}").unwrap();

        let error = PolicyDocument::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn policy_document_rejects_non_string_domain_key() {
        let value = serde_yaml::from_str::<Value>("1: value").unwrap();

        let error = PolicyDocument::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }
}
