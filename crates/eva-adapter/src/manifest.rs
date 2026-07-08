//! Adapter runtime handle representation.

use eva_config::manifest::adapter::AdapterManifest;
use eva_config::manifest::capability::CapabilityManifest;
use eva_config::{AdapterTransport, CapabilityKind};
use eva_core::{AdapterId, CapabilityId, CapabilityName, EvaError};
use eva_mcp::{McpProcessSpec, McpServerTransport, McpSessionConfig};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Adapter manifest runtime representation";

/// Lightweight health state carried by a registered Adapter handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterHealth {
    Ready,
    Disabled,
}

/// Runtime binding between one Eva capability manifest and one Adapter handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCapabilityBinding {
    pub capability_id: Option<CapabilityId>,
    pub capability: CapabilityName,
    pub kind: CapabilityKind,
    pub provider: AdapterId,
    pub mcp_tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInputSchema {
    pub schema_type: Option<String>,
    pub required: Vec<String>,
    pub properties: BTreeMap<String, SkillInputProperty>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInputProperty {
    pub value_type: Option<String>,
    pub enum_values: Vec<String>,
}

/// Authorized runtime handle derived from configuration, not from discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterHandle {
    pub id: AdapterId,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub transport: AdapterTransport,
    pub capabilities: Vec<CapabilityName>,
    pub source_path: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub endpoint: Option<String>,
    pub method: Option<String>,
    pub credential_env: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub output_limit_bytes: Option<usize>,
    pub max_prompt_bytes: Option<usize>,
    pub headers: BTreeMap<String, String>,
    pub mcp_server_transport: Option<String>,
    pub mcp_command: Option<String>,
    pub mcp_args: Vec<String>,
    pub mcp_tools: Vec<String>,
    pub skill_id: Option<String>,
    pub skill_kind: Option<String>,
    pub skill_runtime_gate: Option<String>,
    pub skill_path: Option<String>,
    pub skill_entry_type: Option<String>,
    pub skill_runner_command: Option<String>,
    pub skill_runner_args: Vec<String>,
    pub skill_artifact_root: Option<String>,
    pub skill_input_schema: Option<SkillInputSchema>,
    pub hardware_logical_name: Option<String>,
    pub hardware_device_class: Option<String>,
    pub hardware_driver_id: Option<String>,
    pub hardware_driver_kind: Option<String>,
    pub bindings: Vec<AdapterCapabilityBinding>,
}

impl AdapterHealth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disabled => "disabled",
        }
    }
}

impl AdapterHandle {
    pub fn from_manifest(manifest: &AdapterManifest) -> Self {
        let hardware_config = manifest.hardware_config().ok().flatten();
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            enabled: manifest.enabled,
            transport: manifest.transport,
            capabilities: manifest.capabilities.clone(),
            source_path: manifest.path.display().to_string(),
            command: manifest.extra_string("command").map(str::to_owned),
            args: manifest.extra_string_list("args"),
            endpoint: manifest.extra_string("endpoint").map(str::to_owned),
            method: manifest.extra_string("method").map(str::to_owned),
            credential_env: manifest.nested_extra_string_list("permissions", "env"),
            timeout_ms: manifest.nested_extra_u64("limits", "timeout_ms"),
            output_limit_bytes: manifest
                .nested_extra_usize("limits", "output_limit_bytes")
                .or_else(|| manifest.nested_extra_usize("limits", "max_output_bytes")),
            max_prompt_bytes: manifest.nested_extra_usize("limits", "max_prompt_bytes"),
            headers: {
                let mut headers = manifest.extra_string_map("headers");
                headers.extend(manifest.nested_extra_string_map("http", "headers"));
                headers
            },
            mcp_server_transport: manifest
                .nested_extra_string("mcp", "server_transport")
                .map(str::to_owned),
            mcp_command: manifest
                .nested_extra_string("mcp", "command")
                .map(str::to_owned),
            mcp_args: manifest.nested_extra_string_list("mcp", "args"),
            mcp_tools: manifest.nested_extra_string_list("mcp", "tool_allowlist"),
            skill_id: manifest
                .nested_extra_string("skill", "id")
                .map(str::to_owned),
            skill_kind: manifest
                .nested_extra_string("skill", "kind")
                .map(str::to_owned),
            skill_runtime_gate: manifest
                .nested_extra_string("skill", "runtime_gate")
                .map(str::to_owned),
            skill_path: manifest
                .nested_extra_string("skill", "path")
                .map(str::to_owned),
            skill_entry_type: manifest
                .deep_extra_string(&["skill", "entry", "type"])
                .map(str::to_owned),
            skill_runner_command: manifest
                .deep_extra_string(&["skill", "runner", "command"])
                .map(str::to_owned),
            skill_runner_args: manifest.deep_extra_string_list(&["skill", "runner", "args"]),
            skill_artifact_root: manifest
                .deep_extra_string(&["skill", "artifacts", "root"])
                .or_else(|| manifest.deep_extra_string(&["skill", "artifact_root"]))
                .map(str::to_owned),
            skill_input_schema: manifest
                .deep_extra_object_schema(&["skill", "input_schema"])
                .map(|schema| SkillInputSchema {
                    schema_type: schema.schema_type,
                    required: schema.required,
                    properties: schema
                        .properties
                        .into_iter()
                        .map(|(name, property)| {
                            (
                                name,
                                SkillInputProperty {
                                    value_type: property.value_type,
                                    enum_values: property.enum_values,
                                },
                            )
                        })
                        .collect(),
                }),
            hardware_logical_name: hardware_config
                .as_ref()
                .map(|hardware| hardware.identity.logical_name.clone()),
            hardware_device_class: hardware_config
                .as_ref()
                .map(|hardware| hardware.identity.device_class.clone()),
            hardware_driver_id: hardware_config
                .as_ref()
                .map(|hardware| hardware.driver.driver_id.clone()),
            hardware_driver_kind: hardware_config
                .as_ref()
                .map(|hardware| hardware.driver.kind.as_str().to_owned()),
            bindings: Vec::new(),
        }
    }

    pub fn health(&self) -> AdapterHealth {
        if self.enabled {
            AdapterHealth::Ready
        } else {
            AdapterHealth::Disabled
        }
    }

    pub fn supports(&self, capability: &CapabilityName) -> bool {
        self.capabilities.iter().any(|entry| entry == capability)
            || self
                .bindings
                .iter()
                .any(|entry| &entry.capability == capability)
    }

    pub fn add_binding(&mut self, binding: AdapterCapabilityBinding) {
        if !self.bindings.iter().any(|existing| {
            existing.capability == binding.capability && existing.provider == binding.provider
        }) {
            self.bindings.push(binding);
            self.bindings.sort_by(|left, right| {
                left.capability
                    .cmp(&right.capability)
                    .then(left.provider.cmp(&right.provider))
            });
        }
    }

    pub fn binding_for(&self, capability: &CapabilityName) -> Option<&AdapterCapabilityBinding> {
        self.bindings
            .iter()
            .find(|binding| &binding.capability == capability)
    }

    pub fn mcp_tool_for(&self, capability: &CapabilityName) -> Option<&str> {
        self.binding_for(capability)
            .and_then(|binding| binding.mcp_tool.as_deref())
            .or_else(|| self.mcp_tools.first().map(String::as_str))
    }

    pub fn mcp_session_config(&self) -> Result<McpSessionConfig, EvaError> {
        let command = self.mcp_command.as_deref().ok_or_else(|| {
            EvaError::invalid_argument("MCP adapter is missing a process command")
                .with_context("adapter_id", self.id.as_str())
        })?;
        let server_transport =
            McpServerTransport::parse(self.mcp_server_transport.as_deref().unwrap_or("stdio"))?;
        let process = McpProcessSpec::new(command.to_owned()).with_args(self.mcp_args.clone());
        McpSessionConfig::new(self.id.clone(), server_transport, process)
    }

    pub fn skill_name(&self) -> Option<&str> {
        self.skill_id.as_deref()
    }
}

impl AdapterCapabilityBinding {
    pub fn from_manifest(provider: AdapterId, manifest: &CapabilityManifest) -> Self {
        Self {
            capability_id: Some(manifest.id.clone()),
            capability: manifest.capability.clone(),
            kind: manifest.kind,
            provider,
            mcp_tool: manifest.extra_string("tool").map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn handle_reads_mcp_and_skill_extensions() {
        let project = load_project_config(workspace_root()).unwrap();
        let mcp = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "github-mcp")
            .unwrap();
        let skill = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "code-review-skill")
            .unwrap();

        let mcp_handle = AdapterHandle::from_manifest(mcp);
        let skill_handle = AdapterHandle::from_manifest(skill);

        assert!(mcp_handle.mcp_tools.contains(&"list_issues".to_owned()));
        let session_config = mcp_handle.mcp_session_config().unwrap();
        assert_eq!(session_config.server_transport, McpServerTransport::Stdio);
        assert_eq!(session_config.process.command, "github-mcp-server");
        assert_eq!(mcp_handle.timeout_ms, Some(60000));
        assert!(mcp_handle
            .credential_env
            .contains(&"GITHUB_TOKEN".to_owned()));
        assert_eq!(skill_handle.skill_name(), Some("code-review"));
        assert_eq!(
            skill_handle.skill_path.as_deref(),
            Some("~/.codex/skills/code-review/SKILL.md")
        );
        assert_eq!(
            skill_handle.skill_entry_type.as_deref(),
            Some("codex_skill")
        );
        assert_eq!(
            skill_handle.skill_input_schema.as_ref().unwrap().required,
            vec!["scope".to_owned()]
        );
    }

    #[test]
    fn handle_reads_hardware_identity_extensions() {
        let project = load_project_config(workspace_root()).unwrap();
        let hardware = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "scale-main")
            .unwrap();

        let handle = AdapterHandle::from_manifest(hardware);

        assert_eq!(handle.hardware_logical_name.as_deref(), Some("main-scale"));
        assert_eq!(handle.hardware_device_class.as_deref(), Some("scale"));
        assert_eq!(
            handle.hardware_driver_id.as_deref(),
            Some("scale-main-simulated-driver")
        );
        assert_eq!(handle.hardware_driver_kind.as_deref(), Some("simulated"));
    }

    #[test]
    fn handle_reads_stdio_and_http_runtime_fields() {
        let project = load_project_config(workspace_root()).unwrap();
        let stdio = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "codex-cli")
            .unwrap();
        let http = project
            .adapters
            .iter()
            .find(|adapter| adapter.id.as_str() == "claude-api")
            .unwrap();

        let stdio_handle = AdapterHandle::from_manifest(stdio);
        let http_handle = AdapterHandle::from_manifest(http);

        assert_eq!(stdio_handle.command.as_deref(), Some("codex"));
        assert!(stdio_handle.args.contains(&"exec".to_owned()));
        assert_eq!(stdio_handle.timeout_ms, Some(300000));
        assert_eq!(stdio_handle.max_prompt_bytes, Some(200000));
        assert_eq!(
            http_handle.endpoint.as_deref(),
            Some("https://api.anthropic.com/v1/messages")
        );
        assert!(http_handle
            .credential_env
            .contains(&"ANTHROPIC_API_KEY".to_owned()));
    }
}
