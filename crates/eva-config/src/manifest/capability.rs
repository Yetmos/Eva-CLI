//! Capability manifest loading and normalization.

use crate::{read_yaml_file, require_non_empty, with_field_context, EvaError};
use eva_core::{AdapterId, CapabilityId, CapabilityName};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability manifest loading and normalization";

const CONFIG_TYPE: &str = "Capability manifest";

/// Validated capability manifest ready for downstream registration.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityManifest {
    /// Path to the source manifest file.
    pub path: PathBuf,
    /// Stable capability manifest id.
    pub id: CapabilityId,
    /// Human-readable capability name.
    pub name: String,
    /// Manifest version.
    pub version: String,
    /// Whether this capability can be registered.
    pub enabled: bool,
    /// Capability implementation category.
    pub kind: CapabilityKind,
    /// Runtime capability exposed to callers.
    pub capability: CapabilityName,
    /// Default adapter provider, when the manifest declares one.
    pub default_provider: Option<AdapterId>,
    /// Explicit provider adapter, used by MCP-backed manifests.
    pub provider: Option<AdapterId>,
    /// Adapter capabilities required by the permission declaration.
    pub required_adapter_capabilities: Vec<CapabilityName>,
    /// Provider adapters allowed by the permission declaration.
    pub allowed_adapter_providers: Vec<AdapterId>,
    /// Additional fields owned by capability, Lua, MCP, skill, or policy crates.
    pub extra: Mapping,
}

/// Supported capability implementation categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    AdapterCapability,
    LuaCapability,
    McpTool,
    Skill,
}

/// Raw capability kind spelling retained for schema alignment tests.
pub type RawCapabilityKind = CapabilityKind;

/// Loads and validates one capability manifest file.
pub fn load_capability_manifest(path: impl AsRef<Path>) -> Result<CapabilityManifest, EvaError> {
    let path = path.as_ref();
    let raw: RawCapabilityManifest = read_yaml_file(path, CONFIG_TYPE)?;
    CapabilityManifest::try_from_raw(path.to_path_buf(), raw)
}

impl CapabilityManifest {
    fn try_from_raw(path: PathBuf, raw: RawCapabilityManifest) -> Result<Self, EvaError> {
        let id = CapabilityId::parse(&raw.id)
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "id"))?;
        let name = require_non_empty(raw.name, CONFIG_TYPE, &path, "name")?;
        let version = require_non_empty(raw.version, CONFIG_TYPE, &path, "version")?;
        let kind = CapabilityKind::parse(&raw.kind)
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "kind"))?;
        let capability = CapabilityName::parse(&raw.capability)
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "capability"))?;
        let default_provider = raw
            .default_provider
            .as_deref()
            .map(AdapterId::parse)
            .transpose()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "default_provider"))?;
        let provider = raw
            .provider
            .as_deref()
            .map(AdapterId::parse)
            .transpose()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "provider"))?;
        let permissions = raw.permissions.unwrap_or_default();
        let required_adapter_capabilities = permissions
            .adapters
            .as_ref()
            .map(|adapters| {
                adapters
                    .capabilities
                    .iter()
                    .map(|capability| CapabilityName::parse(capability))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| {
                with_field_context(
                    error,
                    CONFIG_TYPE,
                    &path,
                    "permissions.adapters.capabilities",
                )
            })?
            .unwrap_or_default();
        let allowed_adapter_providers = permissions
            .adapters
            .as_ref()
            .map(|adapters| {
                adapters
                    .providers
                    .iter()
                    .map(|provider| AdapterId::parse(provider))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| {
                with_field_context(error, CONFIG_TYPE, &path, "permissions.adapters.providers")
            })?
            .unwrap_or_default();

        Ok(Self {
            path,
            id,
            name,
            version,
            enabled: raw.enabled,
            kind,
            capability,
            default_provider,
            provider,
            required_adapter_capabilities,
            allowed_adapter_providers,
            extra: raw.extra,
        })
    }

    /// Returns all adapter ids referenced by provider fields.
    pub fn adapter_providers(&self) -> impl Iterator<Item = &AdapterId> {
        self.default_provider
            .iter()
            .chain(self.provider.iter())
            .chain(self.allowed_adapter_providers.iter())
    }
}

impl CapabilityKind {
    /// Parses a supported capability kind from manifest YAML.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "adapter_capability" => Ok(Self::AdapterCapability),
            "lua_capability" => Ok(Self::LuaCapability),
            "mcp_tool" => Ok(Self::McpTool),
            "skill" => Ok(Self::Skill),
            _ => {
                Err(EvaError::unsupported("unsupported capability kind")
                    .with_context("kind", value))
            }
        }
    }

    /// Returns the stable manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdapterCapability => "adapter_capability",
            Self::LuaCapability => "lua_capability",
            Self::McpTool => "mcp_tool",
            Self::Skill => "skill",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawCapabilityManifest {
    id: String,
    name: String,
    version: String,
    enabled: bool,
    kind: String,
    capability: String,
    default_provider: Option<String>,
    provider: Option<String>,
    permissions: Option<RawCapabilityPermissions>,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Default, Deserialize)]
struct RawCapabilityPermissions {
    adapters: Option<RawCapabilityAdapterPermissions>,
    #[serde(flatten)]
    _extra: Mapping,
}

#[derive(Debug, Deserialize)]
struct RawCapabilityAdapterPermissions {
    #[serde(default)]
    providers: Vec<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(flatten)]
    _extra: Mapping,
}

impl TryFrom<Value> for CapabilityManifest {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawCapabilityManifest::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse Capability manifest")
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
    fn load_capability_manifest_accepts_sample_capability() {
        let manifest = load_capability_manifest(
            workspace_root().join("config/capabilities/repo-summary.yaml"),
        )
        .unwrap();

        assert_eq!(manifest.id.as_str(), "repo-summary");
        assert_eq!(manifest.kind, CapabilityKind::AdapterCapability);
        assert_eq!(manifest.capability.as_str(), "repo.analyze");
        assert_eq!(
            manifest.default_provider.as_ref().unwrap().as_str(),
            "codex-cli"
        );
        assert_eq!(
            manifest.required_adapter_capabilities[0].as_str(),
            "repo.analyze"
        );
    }

    #[test]
    fn load_capability_manifest_rejects_invalid_runtime_capability() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: capability-test
name: Capability Test
version: 1.0.0
enabled: true
kind: adapter_capability
capability: repo/analyze
"#,
        )
        .unwrap();

        let error = CapabilityManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "field")
                .unwrap()
                .1,
            "capability"
        );
    }

    #[test]
    fn load_capability_manifest_rejects_unknown_kind() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: capability-test
name: Capability Test
version: 1.0.0
enabled: true
kind: mystery
capability: repo.analyze
"#,
        )
        .unwrap();

        let error = CapabilityManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn load_capability_manifest_rejects_invalid_provider_id() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: capability-test
name: Capability Test
version: 1.0.0
enabled: true
kind: mcp_tool
capability: mcp.tool.call
provider: github/mcp
"#,
        )
        .unwrap();

        let error = CapabilityManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "field")
                .unwrap()
                .1,
            "provider"
        );
    }
}
