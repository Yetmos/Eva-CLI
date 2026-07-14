//! 中文：Agent 清单的 YAML 加载、核心契约校验和规范化。
//! Agent manifest loading and normalization.

use crate::{read_yaml_file, require_non_empty_path, with_field_context, EvaError};
use eva_core::{AgentId, CapabilityName, TopicPattern};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// 中文：本模块把 Agent 身份、脚本、订阅和权限声明转换为强类型清单。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent manifest loading and normalization";

/// 中文：写入清单错误上下文的稳定配置类型名称。
const CONFIG_TYPE: &str = "Agent manifest";

/// 中文：已校验、可由下游模块直接注册的 Agent 清单。
/// Validated Agent manifest ready for registration by downstream modules.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentManifest {
    /// 中文：源 `agent.yaml` 路径，用于错误定位和相对资源解析。
    /// Path to the source `agent.yaml`.
    pub path: PathBuf,
    /// 中文：经过核心契约校验的稳定 Agent 标识。
    /// Stable Agent id.
    pub id: AgentId,
    /// 中文：该 Agent 是否可被注册和启动。
    /// Whether this Agent can be registered.
    pub enabled: bool,
    /// 中文：可选管理父 Agent，不代表事件路由关系。
    /// Optional management parent.
    pub parent: Option<AgentId>,
    /// 中文：按配置顺序声明的子 Agent。
    /// Declared child Agents.
    pub children: Vec<AgentId>,
    /// 中文：相对于 Agent 目录的非空 Lua 入口脚本路径。
    /// Lua entry script relative to this Agent directory.
    pub script: PathBuf,
    /// 中文：可选脚本代际标记，用于热重载比较。
    /// Optional script generation marker.
    pub script_version: Option<String>,
    /// 中文：该 Agent 消费的已校验主题订阅模式。
    /// Topic subscriptions consumed by this Agent.
    pub subscriptions: Vec<TopicPattern>,
    /// 中文：涉及核心标识和主题契约时已经规范化的权限声明。
    /// Permission declarations normalized where they touch core contracts.
    pub permissions: AgentManifestPermissions,
    /// 中文：由运行时、调度器、策略或存储 crate 解释的扩展字段。
    /// Additional fields owned by runtime, scheduler, policy, or storage crates.
    pub extra: Mapping,
}

/// 中文：完成核心契约校验后的 Agent 权限声明。
/// Agent permission declarations parsed enough to validate core contracts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentManifestPermissions {
    /// 中文：Agent 被允许发出的主题模式。
    pub emit: Vec<TopicPattern>,
    /// 中文：Agent 声明使用的工具名称。
    pub tools: Vec<String>,
    /// 中文：Agent 被允许请求的 Adapter capability。
    pub adapter_capabilities: Vec<CapabilityName>,
    /// 中文：Agent 声明的 Provider 名称，由 Adapter 层继续解释。
    pub adapter_providers: Vec<String>,
}

/// 中文：读取并完整校验一份 Agent 清单，不返回半规范化结果。
/// Loads and validates one Agent manifest file.
pub fn load_agent_manifest(path: impl AsRef<Path>) -> Result<AgentManifest, EvaError> {
    let path = path.as_ref();
    let raw: RawAgentManifest = read_yaml_file(path, CONFIG_TYPE)?;
    AgentManifest::try_from_raw(path.to_path_buf(), raw)
}

impl AgentManifest {
    /// 中文：解析所有核心标识、路径和主题模式，并保留下游拥有的扩展字段。
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
    /// 中文：规范化发出主题和 Adapter capability，保留工具及 Provider 声明顺序。
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

/// 中文：反序列化阶段使用的 Agent 清单结构，字段尚未经过核心类型校验。
#[derive(Debug, Deserialize)]
struct RawAgentManifest {
    /// 中文：原始 Agent 标识文本。
    id: String,
    /// 中文：原始启用开关。
    enabled: bool,
    /// 中文：可选父 Agent 标识文本。
    parent: Option<String>,
    /// 中文：原始子 Agent 标识列表。
    #[serde(default)]
    children: Vec<String>,
    /// 中文：原始入口脚本路径。
    script: PathBuf,
    /// 中文：可选脚本版本文本。
    script_version: Option<String>,
    /// 中文：原始订阅主题模式列表。
    #[serde(default)]
    subscriptions: Vec<String>,
    /// 中文：尚未规范化的权限块。
    permissions: RawAgentPermissions,
    /// 中文：保留给下游模块的所有未知字段。
    #[serde(flatten)]
    extra: Mapping,
}

/// 中文：反序列化阶段使用的 Agent 权限结构。
#[derive(Debug, Deserialize)]
struct RawAgentPermissions {
    /// 中文：原始可发出主题模式列表。
    #[serde(default)]
    emit: Vec<String>,
    /// 中文：原始工具名称列表。
    #[serde(default)]
    tools: Vec<String>,
    /// 中文：可选 Adapter 权限子块。
    adapters: Option<RawAgentAdapterPermissions>,
    /// 中文：保留尚未由配置层解释的权限字段。
    #[serde(flatten)]
    _extra: Mapping,
}

/// 中文：反序列化阶段使用的 Adapter capability 与 Provider 权限声明。
#[derive(Debug, Deserialize)]
struct RawAgentAdapterPermissions {
    /// 中文：原始 capability 名称列表。
    #[serde(default)]
    capabilities: Vec<String>,
    /// 中文：原始 Provider 名称列表。
    #[serde(default)]
    providers: Vec<String>,
    /// 中文：保留给 Adapter 或策略层解释的扩展字段。
    #[serde(flatten)]
    _extra: Mapping,
}

impl TryFrom<Value> for AgentManifest {
    /// 中文：内存 YAML 转换使用的结构化错误类型。
    type Error = EvaError;

    /// 中文：先反序列化原始清单，再执行与文件加载相同的核心契约校验。
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

    /// 中文：返回 Agent 清单测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 中文：验证样例 Agent 的身份、脚本、订阅和 Adapter 权限正确加载。
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
    /// 中文：验证非法 Agent 标识在配置边界被拒绝。
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
    /// 中文：验证无效订阅主题模式不会进入调度器。
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
    /// 中文：验证无效发出主题模式会报告精确权限字段路径。
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
