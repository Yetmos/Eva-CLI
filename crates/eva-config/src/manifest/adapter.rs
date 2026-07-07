//! Adapter manifest loading and normalization.

use crate::{read_yaml_file, require_non_empty, with_field_context, EvaError};
use eva_core::{AdapterId, CapabilityName};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Adapter manifest loading and normalization";

const CONFIG_TYPE: &str = "Adapter manifest";

/// Validated Adapter manifest ready for downstream registration.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterManifest {
    /// Path to the source manifest file.
    pub path: PathBuf,
    /// Stable Adapter id.
    pub id: AdapterId,
    /// Human-readable adapter name.
    pub name: String,
    /// Manifest version.
    pub version: String,
    /// Whether this adapter can be registered.
    pub enabled: bool,
    /// Supported adapter transport.
    pub transport: AdapterTransport,
    /// Capabilities exposed through this adapter.
    pub capabilities: Vec<CapabilityName>,
    /// Additional fields owned by adapter/runtime/policy crates.
    pub extra: Mapping,
}

/// JSON-Schema-like object schema subset preserved from manifest extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestObjectSchema {
    pub schema_type: Option<String>,
    pub required: Vec<String>,
    pub properties: BTreeMap<String, ManifestSchemaProperty>,
}

/// JSON-Schema-like property subset used by current Adapter manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSchemaProperty {
    pub value_type: Option<String>,
    pub enum_values: Vec<String>,
}

/// Supported adapter transport implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdapterTransport {
    Builtin,
    Stdio,
    Http,
    Eventbus,
    Mcp,
    Skill,
    Hardware,
    LuaCapability,
}

/// Raw transport spelling retained for schema alignment tests.
pub type RawAdapterTransport = AdapterTransport;

/// Loads and validates one Adapter manifest file.
pub fn load_adapter_manifest(path: impl AsRef<Path>) -> Result<AdapterManifest, EvaError> {
    let path = path.as_ref();
    let raw: RawAdapterManifest = read_yaml_file(path, CONFIG_TYPE)?;
    AdapterManifest::try_from_raw(path.to_path_buf(), raw)
}

impl AdapterManifest {
    fn try_from_raw(path: PathBuf, raw: RawAdapterManifest) -> Result<Self, EvaError> {
        let id = AdapterId::parse(&raw.id)
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "id"))?;
        let name = require_non_empty(raw.name, CONFIG_TYPE, &path, "name")?;
        let version = require_non_empty(raw.version, CONFIG_TYPE, &path, "version")?;
        let transport = AdapterTransport::parse(&raw.transport)
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "transport"))?;
        let capabilities = raw
            .capabilities
            .iter()
            .map(|capability| CapabilityName::parse(capability))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "capabilities"))?;

        Ok(Self {
            path,
            id,
            name,
            version,
            enabled: raw.enabled,
            transport,
            capabilities,
            extra: raw.extra,
        })
    }

    /// Returns a top-level string field preserved in the manifest extension map.
    pub fn extra_string(&self, key: &str) -> Option<&str> {
        self.extra
            .get(Value::String(key.to_owned()))
            .and_then(Value::as_str)
    }

    /// Returns a top-level string list preserved in the manifest extension map.
    pub fn extra_string_list(&self, key: &str) -> Vec<String> {
        self.extra
            .get(Value::String(key.to_owned()))
            .and_then(Value::as_sequence)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns a top-level string map preserved in the manifest extension map.
    pub fn extra_string_map(&self, key: &str) -> BTreeMap<String, String> {
        self.extra
            .get(Value::String(key.to_owned()))
            .and_then(Value::as_mapping)
            .map(string_map)
            .unwrap_or_default()
    }

    /// Returns a nested string field preserved in the manifest extension map.
    pub fn nested_extra_string(&self, section: &str, key: &str) -> Option<&str> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_str)
    }

    /// Returns a nested string list preserved in the manifest extension map.
    pub fn nested_extra_string_list(&self, section: &str, key: &str) -> Vec<String> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_sequence)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns a nested string map preserved in the manifest extension map.
    pub fn nested_extra_string_map(&self, section: &str, key: &str) -> BTreeMap<String, String> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_mapping)
            .map(string_map)
            .unwrap_or_default()
    }

    /// Returns a nested unsigned integer preserved in the manifest extension map.
    pub fn nested_extra_u64(&self, section: &str, key: &str) -> Option<u64> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_u64)
    }

    /// Returns a nested `usize` preserved in the manifest extension map.
    pub fn nested_extra_usize(&self, section: &str, key: &str) -> Option<usize> {
        self.nested_extra_u64(section, key)
            .and_then(|value| usize::try_from(value).ok())
    }

    /// Returns a string field from a nested manifest extension path.
    pub fn deep_extra_string(&self, path: &[&str]) -> Option<&str> {
        let (first, rest) = path.split_first()?;
        let mut value = self.extra.get(Value::String((*first).to_owned()))?;
        for key in rest {
            value = value
                .as_mapping()
                .and_then(|mapping| mapping.get(Value::String((*key).to_owned())))?;
        }
        value.as_str()
    }

    /// Returns a string list from a nested manifest extension path.
    pub fn deep_extra_string_list(&self, path: &[&str]) -> Vec<String> {
        self.deep_extra_value(path)
            .and_then(Value::as_sequence)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the object-schema subset used by workflow skill manifests.
    pub fn deep_extra_object_schema(&self, path: &[&str]) -> Option<ManifestObjectSchema> {
        let mapping = self.deep_extra_value(path)?.as_mapping()?;
        let schema_type = mapping
            .get(Value::String("type".to_owned()))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let required = mapping
            .get(Value::String("required".to_owned()))
            .and_then(Value::as_sequence)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let properties = mapping
            .get(Value::String("properties".to_owned()))
            .and_then(Value::as_mapping)
            .map(|properties| {
                properties
                    .iter()
                    .filter_map(|(key, value)| {
                        let key = key.as_str()?.to_owned();
                        let property = value.as_mapping()?;
                        let value_type = property
                            .get(Value::String("type".to_owned()))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        let enum_values = property
                            .get(Value::String("enum".to_owned()))
                            .and_then(Value::as_sequence)
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        Some((
                            key,
                            ManifestSchemaProperty {
                                value_type,
                                enum_values,
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(ManifestObjectSchema {
            schema_type,
            required,
            properties,
        })
    }

    fn deep_extra_value(&self, path: &[&str]) -> Option<&Value> {
        let (first, rest) = path.split_first()?;
        let mut value = self.extra.get(Value::String((*first).to_owned()))?;
        for key in rest {
            value = value
                .as_mapping()
                .and_then(|mapping| mapping.get(Value::String((*key).to_owned())))?;
        }
        Some(value)
    }
}

fn string_map(mapping: &Mapping) -> BTreeMap<String, String> {
    mapping
        .iter()
        .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), value.as_str()?.to_owned())))
        .collect()
}

impl AdapterTransport {
    /// Parses a supported transport string from manifest YAML.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "builtin" => Ok(Self::Builtin),
            "stdio" => Ok(Self::Stdio),
            "http" => Ok(Self::Http),
            "eventbus" => Ok(Self::Eventbus),
            "mcp" => Ok(Self::Mcp),
            "skill" => Ok(Self::Skill),
            "hardware" => Ok(Self::Hardware),
            "lua_capability" => Ok(Self::LuaCapability),
            _ => Err(EvaError::unsupported("unsupported Adapter transport")
                .with_context("transport", value)),
        }
    }

    /// Returns the stable manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Eventbus => "eventbus",
            Self::Mcp => "mcp",
            Self::Skill => "skill",
            Self::Hardware => "hardware",
            Self::LuaCapability => "lua_capability",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawAdapterManifest {
    id: String,
    name: String,
    version: String,
    enabled: bool,
    transport: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(flatten)]
    extra: Mapping,
}

impl TryFrom<Value> for AdapterManifest {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawAdapterManifest::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse Adapter manifest")
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
    fn load_adapter_manifest_accepts_sample_adapter() {
        let manifest =
            load_adapter_manifest(workspace_root().join("config/adapters/codex-cli.yaml")).unwrap();

        assert_eq!(manifest.id.as_str(), "codex-cli");
        assert_eq!(manifest.transport, AdapterTransport::Stdio);
        assert_eq!(manifest.capabilities[0].as_str(), "repo.analyze");
        assert!(manifest
            .extra
            .contains_key(Value::String("permissions".to_owned())));
    }

    #[test]
    fn load_adapter_manifest_rejects_unknown_transport() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: adapter-test
name: Adapter Test
version: 1.0.0
enabled: true
transport: mystery
capabilities:
  - repo.analyze
"#,
        )
        .unwrap();

        let error = AdapterManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn load_adapter_manifest_rejects_invalid_capability_name() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: adapter-test
name: Adapter Test
version: 1.0.0
enabled: true
transport: stdio
capabilities:
  - repo/analyze
"#,
        )
        .unwrap();

        let error = AdapterManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "field")
                .unwrap()
                .1,
            "capabilities"
        );
    }

    #[test]
    fn adapter_manifest_exposes_nested_extension_lists() {
        let manifest =
            load_adapter_manifest(workspace_root().join("config/adapters/github-mcp.yaml"))
                .unwrap();

        assert_eq!(
            manifest.nested_extra_string("mcp", "command"),
            Some("github-mcp-server")
        );
        assert!(manifest
            .nested_extra_string_list("mcp", "tool_allowlist")
            .contains(&"list_issues".to_owned()));
    }

    #[test]
    fn adapter_manifest_exposes_deep_extension_strings() {
        let manifest = load_adapter_manifest(
            workspace_root().join("config/adapters/hardware/scale-main.yaml"),
        )
        .unwrap();

        assert_eq!(
            manifest.deep_extra_string(&["hardware", "bus"]),
            Some("usb")
        );
        assert_eq!(
            manifest.deep_extra_string(&["hardware", "identity", "logical_name"]),
            Some("main-scale")
        );
    }

    #[test]
    fn adapter_manifest_exposes_runtime_extension_values() {
        let manifest =
            load_adapter_manifest(workspace_root().join("config/adapters/codex-cli.yaml")).unwrap();

        assert_eq!(manifest.extra_string("command"), Some("codex"));
        assert!(manifest
            .extra_string_list("args")
            .contains(&"exec".to_owned()));
        assert_eq!(
            manifest.nested_extra_string_list("permissions", "env"),
            Vec::<String>::new()
        );
        assert_eq!(
            manifest.nested_extra_u64("limits", "timeout_ms"),
            Some(300000)
        );
        assert_eq!(
            manifest.nested_extra_usize("limits", "max_prompt_bytes"),
            Some(200000)
        );
    }

    #[test]
    fn adapter_manifest_exposes_skill_schema_subset() {
        let manifest =
            load_adapter_manifest(workspace_root().join("config/adapters/code-review-skill.yaml"))
                .unwrap();

        assert_eq!(
            manifest.deep_extra_string(&["skill", "entry", "type"]),
            Some("codex_skill")
        );
        assert_eq!(
            manifest.deep_extra_string_list(&["skill", "input_schema", "required"]),
            vec!["scope".to_owned()]
        );

        let schema = manifest
            .deep_extra_object_schema(&["skill", "input_schema"])
            .unwrap();

        assert_eq!(schema.schema_type.as_deref(), Some("object"));
        assert!(schema.required.contains(&"scope".to_owned()));
        assert_eq!(
            schema.properties["scope"].enum_values,
            vec!["current_diff".to_owned(), "workspace".to_owned()]
        );
    }
}
