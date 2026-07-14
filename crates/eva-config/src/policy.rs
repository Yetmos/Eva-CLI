//! 中文：策略文档的 YAML 加载和顶层域规范化。
//! Policy document loading and normalization.

use crate::{read_yaml_file, EvaError};
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 中文：写入配置错误上下文的稳定文档类型名称。
const CONFIG_TYPE: &str = "Policy document";

/// 中文：从 `config/policies/*.yaml` 加载的可扩展策略文档。
/// Extensible policy document loaded from `config/policies/*.yaml`.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDocument {
    /// 中文：源策略文件路径，用于错误定位和策略层命名。
    /// Path to the source policy file.
    pub path: PathBuf,
    /// 中文：按 YAML 顶层字段名称索引的策略域；具体语义由 `eva-policy` 解释。
    /// Top-level policy domains keyed by YAML field name.
    pub domains: BTreeMap<String, Value>,
}

/// 中文：加载一份策略文档，只校验它是非空且具有稳定字符串键的 YAML 映射。
///
/// 域内字段由 `eva-policy` 负责解释，使配置层能够保留尚未认识的新策略域而不越过
/// crate 边界。读取或结构校验失败时附加源文件路径，不返回部分文档。
/// Loads one policy document. Domain-specific interpretation belongs to
/// `eva-policy`; `eva-config` only guarantees that the document is a non-empty
/// YAML mapping with stable string keys.
pub fn load_policy_document(path: impl AsRef<Path>) -> Result<PolicyDocument, EvaError> {
    let path = path.as_ref();
    let value: Value = read_yaml_file(path, CONFIG_TYPE)?;
    PolicyDocument::try_from_value(path.to_path_buf(), value)
}

impl PolicyDocument {
    /// 中文：从已解析 YAML 值构建策略文档，并规范化、校验所有顶层域名。
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
    /// 中文：内存 YAML 值转换失败时使用的结构化配置错误类型。
    type Error = EvaError;

    /// 中文：把无源路径的 YAML 值转换为策略文档，主要供测试和嵌入调用使用。
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::try_from_value(PathBuf::new(), value)
    }
}

impl From<Mapping> for PolicyDocument {
    /// 中文：宽松地从已有映射创建文档，只保留字符串键；调用方已选择跳过严格校验。
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

    /// 中文：返回策略配置测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 中文：验证仓库样例策略文档可加载并保留 Lua 沙箱域。
    fn load_policy_document_accepts_sample_policy() {
        let policy =
            load_policy_document(workspace_root().join("config/policies/sandbox.yaml")).unwrap();

        assert!(policy.domains.contains_key("lua_sandbox"));
    }

    #[test]
    /// 中文：验证空策略映射不会被误认为有效默认策略。
    fn policy_document_rejects_empty_mapping() {
        let value = serde_yaml::from_str::<Value>("{}").unwrap();

        let error = PolicyDocument::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    /// 中文：验证非字符串顶层域名被拒绝。
    fn policy_document_rejects_non_string_domain_key() {
        let value = serde_yaml::from_str::<Value>("1: value").unwrap();

        let error = PolicyDocument::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }
}
