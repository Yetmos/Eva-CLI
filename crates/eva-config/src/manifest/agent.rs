//! Agent manifest loading and normalization.

use crate::{read_yaml_file, require_non_empty_path, with_field_context, EvaError};
use eva_core::{AgentId, CapabilityName, TopicPattern};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent manifest loading and normalization";

const CONFIG_TYPE: &str = "Agent manifest";

/// Validated Agent manifest ready for registration by downstream modules.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentManifest {
    /// Path to the source `agent.yaml`.
    pub path: PathBuf,
    /// Stable Agent id.
    pub id: AgentId,
    /// Whether this Agent can be registered.
    pub enabled: bool,
    /// Optional management parent.
    pub parent: Option<AgentId>,
    /// Declared child Agents.
    pub children: Vec<AgentId>,
    /// Lua entry script relative to this Agent directory.
    pub script: PathBuf,
    /// Optional script generation marker.
    pub script_version: Option<String>,
    /// Topic subscriptions consumed by this Agent.
    pub subscriptions: Vec<TopicPattern>,
    /// Permission declarations normalized where they touch core contracts.
    pub permissions: AgentManifestPermissions,
    /// Additional fields owned by runtime, scheduler, policy, or storage crates.
    pub extra: Mapping,
}

/// Agent permission declarations parsed enough to validate core contracts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentManifestPermissions {
    pub emit: Vec<TopicPattern>,
    pub tools: Vec<String>,
    pub adapter_capabilities: Vec<CapabilityName>,
    pub adapter_providers: Vec<String>,
}

/// Loads and validates one Agent manifest file.
pub fn load_agent_manifest(path: impl AsRef<Path>) -> Result<AgentManifest, EvaError> {
    let path = path.as_ref();
    let raw: RawAgentManifest = read_yaml_file(path, CONFIG_TYPE)?;
    AgentManifest::try_from_raw(path.to_path_buf(), raw)
}

impl AgentManifest {
    fn try_from_raw(path: PathBuf, raw: RawAgentManifest) -> Result<Self, EvaError> {
        let id = AgentId::parse(&raw.id)
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "id"))?;
        let parent = raw
            .parent
            .as_deref()
            .map(AgentId::parse)
            .transpose()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "parent"))?;
        let children = raw
            .children
            .iter()
            .map(|child| AgentId::parse(child))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "children"))?;
        let script = require_non_empty_path(raw.script, CONFIG_TYPE, &path, "script")?;
        let subscriptions = raw
            .subscriptions
            .iter()
            .map(|subscription| TopicPattern::parse(subscription))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, &path, "subscriptions"))?;
        let permissions = AgentManifestPermissions::try_from_raw(&path, raw.permissions)?;

        Ok(Self {
            path,
            id,
            enabled: raw.enabled,
            parent,
            children,
            script,
            script_version: raw.script_version,
            subscriptions,
            permissions,
            extra: raw.extra,
        })
    }
}

impl AgentManifestPermissions {
    fn try_from_raw(path: &Path, raw: RawAgentPermissions) -> Result<Self, EvaError> {
        let emit = raw
            .emit
            .iter()
            .map(|topic| TopicPattern::parse(topic))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| with_field_context(error, CONFIG_TYPE, path, "permissions.emit"))?;

        let adapter_capabilities = raw
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
                    path,
                    "permissions.adapters.capabilities",
                )
            })?
            .unwrap_or_default();

        let adapter_providers = raw
            .adapters
            .as_ref()
            .map(|adapters| adapters.providers.clone())
            .unwrap_or_default();

        Ok(Self {
            emit,
            tools: raw.tools,
            adapter_capabilities,
            adapter_providers,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawAgentManifest {
    id: String,
    enabled: bool,
    parent: Option<String>,
    #[serde(default)]
    children: Vec<String>,
    script: PathBuf,
    script_version: Option<String>,
    #[serde(default)]
    subscriptions: Vec<String>,
    permissions: RawAgentPermissions,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Deserialize)]
struct RawAgentPermissions {
    #[serde(default)]
    emit: Vec<String>,
    #[serde(default)]
    tools: Vec<String>,
    adapters: Option<RawAgentAdapterPermissions>,
    #[serde(flatten)]
    _extra: Mapping,
}

#[derive(Debug, Deserialize)]
struct RawAgentAdapterPermissions {
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    providers: Vec<String>,
    #[serde(flatten)]
    _extra: Mapping,
}

impl TryFrom<Value> for AgentManifest {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawAgentManifest::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse Agent manifest")
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
    fn load_agent_manifest_accepts_sample_agent() {
        let manifest =
            load_agent_manifest(workspace_root().join("config/agents/root-agent/agent.yaml"))
                .unwrap();

        assert_eq!(manifest.id.as_str(), "root-agent");
        assert!(manifest.enabled);
        assert_eq!(manifest.script, PathBuf::from("main.lua"));
        assert_eq!(manifest.subscriptions[0].as_str(), "/sys");
        assert_eq!(manifest.permissions.emit[0].as_str(), "/sys/**");
        assert_eq!(
            manifest.permissions.adapter_capabilities[0].as_str(),
            "chat.reply"
        );
    }

    #[test]
    fn load_agent_manifest_rejects_invalid_agent_id() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: bad/id
enabled: true
script: main.lua
subscriptions:
  - /sys
permissions:
  emit:
    - /sys/**
"#,
        )
        .unwrap();

        let error = AgentManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn load_agent_manifest_rejects_invalid_subscription_pattern() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: agent-test
enabled: true
script: main.lua
subscriptions:
  - sys
permissions:
  emit:
    - /sys/**
"#,
        )
        .unwrap();

        let error = AgentManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error.message().contains("topic path"));
    }

    #[test]
    fn load_agent_manifest_rejects_invalid_emit_pattern() {
        let value = serde_yaml::from_str::<Value>(
            r#"
id: agent-test
enabled: true
script: main.lua
subscriptions:
  - /sys
permissions:
  emit:
    - /sys/**/bad
"#,
        )
        .unwrap();

        let error = AgentManifest::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "field")
                .unwrap()
                .1,
            "permissions.emit"
        );
    }
}
