//! Capability 清单的加载与规范化。
//! Capability manifest loading and normalization.

use crate::{read_yaml_file, require_non_empty, with_field_context, EvaError};
use eva_core::{AdapterId, CapabilityId, CapabilityName};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// 本模块的架构职责：加载 Capability 清单并将字符串字段规范化为强类型标识。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability manifest loading and normalization";

/// 错误上下文中使用的配置类型名称。
const CONFIG_TYPE: &str = "Capability manifest";

/// 已验证、可供下游注册的 Capability 清单。
/// Validated capability manifest ready for downstream registration.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityManifest {
    /// 源清单文件路径。
    /// Path to the source manifest file.
    pub path: PathBuf,
    /// 稳定的 Capability 清单标识。
    /// Stable capability manifest id.
    pub id: CapabilityId,
    /// 面向用户的 Capability 名称。
    /// Human-readable capability name.
    pub name: String,
    /// 清单版本。
    /// Manifest version.
    pub version: String,
    /// 是否允许注册该 Capability。
    /// Whether this capability can be registered.
    pub enabled: bool,
    /// Capability 的实现类别。
    /// Capability implementation category.
    pub kind: CapabilityKind,
    /// 向调用方暴露的运行时能力名。
    /// Runtime capability exposed to callers.
    pub capability: CapabilityName,
    /// 清单声明的默认适配器 Provider。
    /// Default adapter provider, when the manifest declares one.
    pub default_provider: Option<AdapterId>,
    /// MCP 等清单使用的显式 Provider 适配器。
    /// Explicit provider adapter, used by MCP-backed manifests.
    pub provider: Option<AdapterId>,
    /// 权限声明要求的适配器能力集合。
    /// Adapter capabilities required by the permission declaration.
    pub required_adapter_capabilities: Vec<CapabilityName>,
    /// 权限声明允许的 Provider 适配器集合。
    /// Provider adapters allowed by the permission declaration.
    pub allowed_adapter_providers: Vec<AdapterId>,
    /// 由 Capability、Lua、MCP、Skill 或策略 crate 解释的扩展字段。
    /// Additional fields owned by capability, Lua, MCP, skill, or policy crates.
    pub extra: Mapping,
}

/// 支持的 Capability 实现类别。
/// Supported capability implementation categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    /// 由 Adapter 提供的普通能力。
    AdapterCapability,
    /// 由 Lua 脚本实现的能力。
    LuaCapability,
    /// MCP 服务暴露的工具。
    McpTool,
    /// 可加载的 Skill。
    Skill,
}

/// 为 Schema 对齐测试保留的原始 Capability 类别别名。
/// Raw capability kind spelling retained for schema alignment tests.
pub type RawCapabilityKind = CapabilityKind;

/// 从 YAML 文件加载并验证一份 Capability 清单。
/// Loads and validates one capability manifest file.
pub fn load_capability_manifest(path: impl AsRef<Path>) -> Result<CapabilityManifest, EvaError> {
    let path = path.as_ref();
    let raw: RawCapabilityManifest = read_yaml_file(path, CONFIG_TYPE)?;
    CapabilityManifest::try_from_raw(path.to_path_buf(), raw)
}

impl CapabilityManifest {
    /// 将反序列化结构逐字段转换为强类型清单，并为错误附加文件与字段上下文。
    ///
    /// Provider 和权限引用在此只做语法规范化；是否引用项目中实际存在的 Adapter 由
    /// 项目级交叉校验负责。未知扩展字段完整保留，避免核心加载器吞掉下游配置。
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

    /// 按默认、显式、权限允许列表的顺序遍历所有 Adapter Provider 引用。
    /// Returns all adapter ids referenced by provider fields.
    pub fn adapter_providers(&self) -> impl Iterator<Item = &AdapterId> {
        self.default_provider
            .iter()
            .chain(self.provider.iter())
            .chain(self.allowed_adapter_providers.iter())
    }

    /// 读取扩展映射中的顶层字符串字段；不存在或类型不符时返回 `None`。
    /// Returns a top-level string field preserved in the manifest extension map.
    pub fn extra_string(&self, key: &str) -> Option<&str> {
        self.extra
            .get(Value::String(key.to_owned()))
            .and_then(Value::as_str)
    }

    /// 读取扩展映射中一层嵌套的字符串字段。
    /// Returns a nested string field preserved in the manifest extension map.
    pub fn nested_extra_string(&self, section: &str, key: &str) -> Option<&str> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_str)
    }
}

impl CapabilityKind {
    /// 从 YAML 中的稳定拼写解析受支持类别，未知值失败关闭。
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

    /// 返回 Schema 和序列化使用的稳定清单拼写。
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

/// 仅负责 YAML 形状反序列化、尚未进行语义验证的 Capability 清单。
#[derive(Debug, Deserialize)]
struct RawCapabilityManifest {
    /// 原始清单标识字符串。
    id: String,
    /// 原始显示名称。
    name: String,
    /// 原始清单版本。
    version: String,
    /// 原始启用标志。
    enabled: bool,
    /// 原始实现类别拼写。
    kind: String,
    /// 原始运行时能力名。
    capability: String,
    /// 可选默认 Provider 标识。
    default_provider: Option<String>,
    /// 可选显式 Provider 标识。
    provider: Option<String>,
    /// 可选权限对象。
    permissions: Option<RawCapabilityPermissions>,
    /// 未由核心模型占用的顶层扩展字段。
    #[serde(flatten)]
    extra: Mapping,
}

/// Capability 清单中原始权限对象。
#[derive(Debug, Default, Deserialize)]
struct RawCapabilityPermissions {
    /// 可选 Adapter 权限分区。
    adapters: Option<RawCapabilityAdapterPermissions>,
    /// 为前向兼容保留但不由本模块解释的权限字段。
    #[serde(flatten)]
    _extra: Mapping,
}

/// Adapter Provider 和能力的原始权限列表。
#[derive(Debug, Deserialize)]
struct RawCapabilityAdapterPermissions {
    /// 允许的原始 Provider 标识列表。
    #[serde(default)]
    providers: Vec<String>,
    /// 要求的原始能力名列表。
    #[serde(default)]
    capabilities: Vec<String>,
    /// 为前向兼容保留的 Adapter 权限扩展字段。
    #[serde(flatten)]
    _extra: Mapping,
}

impl TryFrom<Value> for CapabilityManifest {
    /// 值转换失败时使用的统一配置错误类型。
    type Error = EvaError;

    /// 从内存 YAML 值解析并验证清单，供测试和组合加载复用。
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawCapabilityManifest::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse Capability manifest")
                .with_context("yaml_error", error.to_string())
        })?;
        Self::try_from_raw(PathBuf::new(), raw)
    }
}

#[cfg(test)]
/// Capability 清单加载、字段错误定位和扩展字段测试。
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use serde_yaml::Value;

    /// 返回包含示例配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证示例清单可规范化为预期类型和权限引用。
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
    /// 验证非法运行时能力名附带稳定字段上下文。
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
    /// 验证未知实现类别不会被宽松接受。
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
    /// 验证非法 Provider 标识附带 provider 字段上下文。
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

    #[test]
    /// 验证下游扩展字符串在核心规范化后仍可读取。
    fn capability_manifest_exposes_extension_strings() {
        let manifest = load_capability_manifest(
            workspace_root().join("config/capabilities/project-summary-mcp.yaml"),
        )
        .unwrap();

        assert_eq!(manifest.extra_string("tool"), Some("list_issues"));
    }
}
