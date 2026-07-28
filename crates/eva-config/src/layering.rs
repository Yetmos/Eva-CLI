//! 可确定重现并显式记录来源的主配置分层合并。
//! Deterministic, explicitly sourced main-configuration layering.

use eva_core::EvaError;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 配置层类别；枚举顺序同时定义从低到高的覆盖优先级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigLayerKind {
    /// 必须且仅能出现一次的基础配置。
    Base,
    /// 与当前运行环境同名的配置档案。
    Profile,
    /// 本机或用户级覆盖配置。
    User,
    /// 具有最高优先级的环境覆盖配置。
    Environment,
}

/// 已参与合并的配置层来源描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLayer {
    /// 配置层类别和优先级。
    pub kind: ConfigLayerKind,
    /// 该层的源文件路径。
    pub path: PathBuf,
}

/// 合并后的配置值、来源列表以及字段级来源索引。
#[derive(Debug, Clone, PartialEq)]
pub struct LayeredConfig {
    /// 应用全部覆盖后的 YAML 值。
    pub value: Value,
    /// 按实际应用顺序排列的配置层。
    pub layers: Vec<ConfigLayer>,
    /// YAML 字段路径到最后写入该字段的配置层类别的映射。
    pub field_sources: BTreeMap<String, ConfigLayerKind>,
}

/// 将 YAML 值编码为跨平台稳定且会遮蔽敏感字段的字节序列。
pub fn canonical_config_bytes(value: &Value) -> Result<Vec<u8>, EvaError> {
    let mut output = Vec::new();
    encode_value(value, None, &mut output)?;
    Ok(output)
}

/// 递归编码 YAML 节点，并根据父映射键判断是否需要遮蔽值。
fn encode_value(value: &Value, field: Option<&str>, output: &mut Vec<u8>) -> Result<(), EvaError> {
    if field.is_some_and(is_sensitive_field) {
        encode_bytes(b'R', b"<redacted>", output);
        return Ok(());
    }
    match value {
        Value::Null => output.push(b'N'),
        Value::Bool(value) => output.extend_from_slice(if *value { b"B1" } else { b"B0" }),
        Value::Number(value) => encode_bytes(b'#', value.to_string().as_bytes(), output),
        Value::String(value) => encode_bytes(b'S', value.as_bytes(), output),
        Value::Sequence(values) => {
            output.push(b'[');
            output.extend_from_slice(&(values.len() as u64).to_be_bytes());
            for value in values {
                encode_value(value, None, output)?;
            }
        }
        Value::Mapping(mapping) => {
            // 按键的原始字节排序，以消除 YAML 映射遍历顺序造成的差异。
            let mut entries = mapping
                .iter()
                .map(|(key, value)| {
                    key.as_str().map(|key| (key, value)).ok_or_else(|| {
                        EvaError::invalid_argument("canonical config keys must be strings")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            output.push(b'{');
            output.extend_from_slice(&(entries.len() as u64).to_be_bytes());
            for (key, value) in entries {
                encode_bytes(b'K', key.as_bytes(), output);
                encode_value(value, Some(key), output)?;
            }
        }
        Value::Tagged(_) => {
            return Err(EvaError::invalid_argument(
                "tagged YAML is not canonical configuration",
            ))
        }
    }
    Ok(())
}

/// 写入类型标签、八字节长度和载荷，避免不同节点组合产生边界歧义。
fn encode_bytes(tag: u8, bytes: &[u8], output: &mut Vec<u8>) {
    output.push(tag);
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}
/// 判断字段名是否表示不应进入摘要明文的敏感信息。
fn is_sensitive_field(field: &str) -> bool {
    let field = field.to_ascii_lowercase();
    ["token", "password", "secret", "authorization", "credential"]
        .iter()
        .any(|part| field.contains(part))
}

/// 按类别和路径的稳定顺序合并多个配置层。
///
/// 映射递归合并，标量和序列整体替换；输入必须恰好包含一个基础层。
pub fn merge_config_layers(
    layers: impl IntoIterator<Item = (ConfigLayerKind, PathBuf, Value)>,
) -> Result<LayeredConfig, EvaError> {
    let mut layers = layers.into_iter().collect::<Vec<_>>();
    // 同类别再按路径排序，使调用方给出的迭代顺序不会影响结果。
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

/// 将单层值递归并入目标值，并同步更新叶子字段的来源信息。
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
            // 新键直接落入目标；已有键则继续递归，以保留未覆盖的兄弟字段。
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

/// 为新插入的映射树递归登记所有叶子字段来源。
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

/// 返回用于类型冲突诊断的稳定 YAML 节点类别名称。
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
/// 构造附带配置层路径和字段位置的参数错误。
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
    #[test]
    fn canonical_bytes_sort_keys_and_redact_sensitive_values() {
        let left = yaml("z: 1\na:\n  token: alpha\n  normal: visible\n");
        let right = yaml("a:\n  normal: visible\n  token: beta\nz: 1\n");
        let first = canonical_config_bytes(&left).unwrap();
        assert_eq!(first, canonical_config_bytes(&right).unwrap());
        assert!(!String::from_utf8_lossy(&first).contains("alpha"));
        assert!(!String::from_utf8_lossy(&first).contains("beta"));
        assert!(String::from_utf8_lossy(&first).contains("visible"));
    }
}
