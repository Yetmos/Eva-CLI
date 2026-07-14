//! 本模块提供 `manifest` 相关实现。
//! Adapter runtime handle representation.

use eva_config::manifest::adapter::AdapterManifest;
use eva_config::manifest::capability::CapabilityManifest;
use eva_config::{AdapterTransport, CapabilityKind};
use eva_core::{AdapterId, CapabilityId, CapabilityName, EvaError};
use eva_mcp::{McpProcessSpec, McpServerTransport, McpSessionConfig};
use std::collections::BTreeMap;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Adapter manifest runtime representation";

/// 定义 `AdapterHealth` 可取的状态。
/// Lightweight health state carried by a registered Adapter handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterHealth {
    /// 表示 `Ready` 枚举分支。
    Ready,
    /// 表示 `Disabled` 枚举分支。
    Disabled,
}

/// 表示 `AdapterCapabilityBinding` 数据结构。
/// Runtime binding between one Eva capability manifest and one Adapter handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCapabilityBinding {
    /// 记录 `capability_id` 字段对应的值。
    pub capability_id: Option<CapabilityId>,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `kind` 字段对应的值。
    pub kind: CapabilityKind,
    /// 记录 `provider` 字段对应的值。
    pub provider: AdapterId,
    /// 记录 `mcp_tool` 字段对应的值。
    pub mcp_tool: Option<String>,
}

/// 表示 `SkillInputSchema` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInputSchema {
    /// 记录 `schema_type` 字段对应的值。
    pub schema_type: Option<String>,
    /// 记录 `required` 字段对应的值。
    pub required: Vec<String>,
    /// 记录 `properties` 字段对应的值。
    pub properties: BTreeMap<String, SkillInputProperty>,
}

/// 表示 `SkillInputProperty` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInputProperty {
    /// 记录 `value_type` 字段对应的值。
    pub value_type: Option<String>,
    /// 记录 `enum_values` 字段对应的值。
    pub enum_values: Vec<String>,
}

/// 表示 `AdapterRateLimit` 数据结构。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterRateLimit {
    /// 记录 `max_requests` 字段对应的值。
    pub max_requests: u32,
    /// 记录 `window_ms` 字段对应的值。
    pub window_ms: u64,
}

/// 表示 `AdapterCircuitBreaker` 数据结构。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterCircuitBreaker {
    /// 记录 `failure_threshold` 字段对应的值。
    pub failure_threshold: u32,
    /// 记录 `recovery_window_ms` 字段对应的值。
    pub recovery_window_ms: u64,
}

/// 表示 `AdapterHandle` 数据结构。
/// Authorized runtime handle derived from configuration, not from discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterHandle {
    /// 记录 `id` 字段对应的值。
    pub id: AdapterId,
    /// 记录 `name` 字段对应的值。
    pub name: String,
    /// 记录 `version` 字段对应的值。
    pub version: String,
    /// 记录 `enabled` 字段对应的值。
    pub enabled: bool,
    /// 记录 `transport` 字段对应的值。
    pub transport: AdapterTransport,
    /// 记录 `capabilities` 字段对应的值。
    pub capabilities: Vec<CapabilityName>,
    /// 记录 `source_path` 字段对应的值。
    pub source_path: String,
    /// 记录 `command` 字段对应的值。
    pub command: Option<String>,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `endpoint` 字段对应的值。
    pub endpoint: Option<String>,
    /// 记录 `method` 字段对应的值。
    pub method: Option<String>,
    /// 记录 `credential_env` 字段对应的值。
    pub credential_env: Vec<String>,
    /// 记录 `timeout_ms` 字段对应的值。
    pub timeout_ms: Option<u64>,
    /// 记录 `max_concurrency` 字段对应的值。
    pub max_concurrency: Option<usize>,
    /// 记录 `output_limit_bytes` 字段对应的值。
    pub output_limit_bytes: Option<usize>,
    /// 记录 `max_prompt_bytes` 字段对应的值。
    pub max_prompt_bytes: Option<usize>,
    /// 记录 `rate_limit` 字段对应的值。
    pub rate_limit: Option<AdapterRateLimit>,
    /// 记录 `circuit_breaker` 字段对应的值。
    pub circuit_breaker: Option<AdapterCircuitBreaker>,
    /// 记录 `headers` 字段对应的值。
    pub headers: BTreeMap<String, String>,
    /// 记录 `mcp_server_transport` 字段对应的值。
    pub mcp_server_transport: Option<String>,
    /// 记录 `mcp_command` 字段对应的值。
    pub mcp_command: Option<String>,
    /// 记录 `mcp_args` 字段对应的值。
    pub mcp_args: Vec<String>,
    /// 记录 `mcp_tools` 字段对应的值。
    pub mcp_tools: Vec<String>,
    /// 记录 `skill_id` 字段对应的值。
    pub skill_id: Option<String>,
    /// 记录 `skill_kind` 字段对应的值。
    pub skill_kind: Option<String>,
    /// 记录 `skill_runtime_gate` 字段对应的值。
    pub skill_runtime_gate: Option<String>,
    /// 记录 `skill_path` 字段对应的值。
    pub skill_path: Option<String>,
    /// 记录 `skill_entry_type` 字段对应的值。
    pub skill_entry_type: Option<String>,
    /// 记录 `skill_runner_command` 字段对应的值。
    pub skill_runner_command: Option<String>,
    /// 记录 `skill_runner_args` 字段对应的值。
    pub skill_runner_args: Vec<String>,
    /// 记录 `skill_artifact_root` 字段对应的值。
    pub skill_artifact_root: Option<String>,
    /// 记录 `skill_input_schema` 字段对应的值。
    pub skill_input_schema: Option<SkillInputSchema>,
    /// 记录 `hardware_logical_name` 字段对应的值。
    pub hardware_logical_name: Option<String>,
    /// 记录 `hardware_device_class` 字段对应的值。
    pub hardware_device_class: Option<String>,
    /// 记录 `hardware_driver_id` 字段对应的值。
    pub hardware_driver_id: Option<String>,
    /// 记录 `hardware_driver_kind` 字段对应的值。
    pub hardware_driver_kind: Option<String>,
    /// 记录 `bindings` 字段对应的值。
    pub bindings: Vec<AdapterCapabilityBinding>,
}

impl AdapterHealth {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disabled => "disabled",
        }
    }
}

impl AdapterHandle {
    /// 根据输入构造当前类型，作为 `from_manifest` 的标准入口。
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
            endpoint: manifest
                .extra_string("endpoint")
                .or_else(|| manifest.nested_extra_string("mcp", "endpoint"))
                .map(str::to_owned),
            method: manifest.extra_string("method").map(str::to_owned),
            credential_env: manifest.nested_extra_string_list("permissions", "env"),
            timeout_ms: manifest.nested_extra_u64("limits", "timeout_ms"),
            max_concurrency: nonzero_usize(
                manifest.nested_extra_usize("limits", "max_concurrency"),
            ),
            output_limit_bytes: manifest
                .nested_extra_usize("limits", "output_limit_bytes")
                .or_else(|| manifest.nested_extra_usize("limits", "max_output_bytes")),
            max_prompt_bytes: manifest.nested_extra_usize("limits", "max_prompt_bytes"),
            rate_limit: AdapterRateLimit::from_manifest(manifest),
            circuit_breaker: AdapterCircuitBreaker::from_manifest(manifest),
            headers: {
                let mut headers = manifest.extra_string_map("headers");
                headers.extend(manifest.nested_extra_string_map("http", "headers"));
                headers.extend(manifest.nested_extra_string_map("mcp", "headers"));
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

    /// 执行 `health` 对应的处理逻辑。
    pub fn health(&self) -> AdapterHealth {
        if self.enabled {
            AdapterHealth::Ready
        } else {
            AdapterHealth::Disabled
        }
    }

    /// 执行 `supports` 对应的处理逻辑。
    pub fn supports(&self, capability: &CapabilityName) -> bool {
        self.capabilities.iter().any(|entry| entry == capability)
            || self
                .bindings
                .iter()
                .any(|entry| &entry.capability == capability)
    }

    /// 登记 `add_binding` 对应的数据或状态。
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

    /// 执行 `binding_for` 对应的处理逻辑。
    pub fn binding_for(&self, capability: &CapabilityName) -> Option<&AdapterCapabilityBinding> {
        self.bindings
            .iter()
            .find(|binding| &binding.capability == capability)
    }

    /// 执行 `mcp_tool_for` 对应的处理逻辑。
    pub fn mcp_tool_for(&self, capability: &CapabilityName) -> Option<&str> {
        self.binding_for(capability)
            .and_then(|binding| binding.mcp_tool.as_deref())
            .or_else(|| self.mcp_tools.first().map(String::as_str))
    }

    /// 执行 `mcp_session_config` 对应的处理逻辑。
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

    /// 执行 `skill_name` 对应的处理逻辑。
    pub fn skill_name(&self) -> Option<&str> {
        self.skill_id.as_deref()
    }
}

impl AdapterRateLimit {
    /// 根据输入构造当前类型，作为 `from_manifest` 的标准入口。
    fn from_manifest(manifest: &AdapterManifest) -> Option<Self> {
        let max_requests = manifest
            .deep_extra_u64(&["limits", "rate_limit", "max_requests"])
            .or_else(|| manifest.nested_extra_u64("limits", "rate_limit_max_requests"))?;
        let max_requests = u32::try_from(max_requests)
            .ok()
            .filter(|value| *value > 0)?;
        let window_ms = manifest
            .deep_extra_u64(&["limits", "rate_limit", "window_ms"])
            .or_else(|| manifest.nested_extra_u64("limits", "rate_limit_window_ms"))
            .unwrap_or(60_000);
        if window_ms == 0 {
            return None;
        }
        Some(Self {
            max_requests,
            window_ms,
        })
    }
}

impl AdapterCircuitBreaker {
    /// 根据输入构造当前类型，作为 `from_manifest` 的标准入口。
    fn from_manifest(manifest: &AdapterManifest) -> Option<Self> {
        let failure_threshold = manifest
            .deep_extra_u64(&["limits", "circuit_breaker", "failure_threshold"])
            .or_else(|| manifest.nested_extra_u64("limits", "circuit_breaker_failure_threshold"))?;
        let failure_threshold = u32::try_from(failure_threshold)
            .ok()
            .filter(|value| *value > 0)?;
        let recovery_window_ms = manifest
            .deep_extra_u64(&["limits", "circuit_breaker", "recovery_window_ms"])
            .or_else(|| manifest.nested_extra_u64("limits", "circuit_breaker_recovery_ms"))
            .unwrap_or(60_000);
        Some(Self {
            failure_threshold,
            recovery_window_ms,
        })
    }
}

/// 执行 `nonzero_usize` 对应的处理逻辑。
fn nonzero_usize(value: Option<usize>) -> Option<usize> {
    value.filter(|value| *value > 0)
}

impl AdapterCapabilityBinding {
    /// 根据输入构造当前类型，作为 `from_manifest` 的标准入口。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use eva_config::manifest::adapter::load_adapter_manifest;
    use std::path::{Path, PathBuf};

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 执行 `temp_root` 对应的处理逻辑。
    fn temp_root(name: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "eva-adapter-manifest-{name}-{}-{now}",
            std::process::id()
        ))
    }

    /// 验证 `handle_reads_mcp_and_skill_extensions` 场景下的预期行为。
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

    /// 验证 `handle_reads_mcp_http_endpoint_and_headers` 场景下的预期行为。
    #[test]
    fn handle_reads_mcp_http_endpoint_and_headers() {
        let root = temp_root("mcp-http");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("mcp-http.yaml");
        std::fs::write(
            &path,
            r#"
id: github-mcp-http
name: GitHub MCP HTTP Adapter
version: 1.0.0
enabled: true
transport: mcp
mcp:
  server_transport: http
  endpoint: http://127.0.0.1:8765/mcp
  headers:
    Authorization: env:GITHUB_TOKEN
  tool_allowlist:
    - list_issues
capabilities:
  - github.issue.list
permissions: {}
limits: {}
routing: {}
"#,
        )
        .unwrap();

        let manifest = load_adapter_manifest(&path).unwrap();
        let handle = AdapterHandle::from_manifest(&manifest);

        assert_eq!(handle.mcp_server_transport.as_deref(), Some("http"));
        assert_eq!(
            handle.endpoint.as_deref(),
            Some("http://127.0.0.1:8765/mcp")
        );
        assert_eq!(
            handle.headers.get("Authorization").map(String::as_str),
            Some("env:GITHUB_TOKEN")
        );
        assert_eq!(handle.mcp_tools, vec!["list_issues".to_owned()]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 验证 `handle_reads_hardware_identity_extensions` 场景下的预期行为。
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

    /// 验证 `handle_reads_stdio_and_http_runtime_fields` 场景下的预期行为。
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
        assert_eq!(stdio_handle.max_concurrency, Some(1));
        assert_eq!(stdio_handle.max_prompt_bytes, Some(200000));
        assert_eq!(
            http_handle.endpoint.as_deref(),
            Some("https://api.anthropic.com/v1/messages")
        );
        assert_eq!(http_handle.max_concurrency, Some(4));
        assert_eq!(
            http_handle.rate_limit,
            Some(AdapterRateLimit {
                max_requests: 60,
                window_ms: 60000
            })
        );
        assert_eq!(
            http_handle.circuit_breaker,
            Some(AdapterCircuitBreaker {
                failure_threshold: 3,
                recovery_window_ms: 30000
            })
        );
        assert!(http_handle
            .credential_env
            .contains(&"ANTHROPIC_API_KEY".to_owned()));
    }
}
