//! Adapter 清单的加载、扩展字段读取与硬件配置规范化。
//! Adapter manifest loading and normalization.

use crate::{invalid_config, read_yaml_file, require_non_empty, with_field_context, EvaError};
use eva_core::{AdapterId, CapabilityName};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：加载 Adapter 清单并把传输、能力及硬件扩展规范化为强类型。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Adapter manifest loading and normalization";

/// 错误上下文中使用的配置类型名称。
const CONFIG_TYPE: &str = "Adapter manifest";

/// 已验证、可供下游注册的 Adapter 清单。
/// Validated Adapter manifest ready for downstream registration.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterManifest {
    /// 源清单文件路径。
    /// Path to the source manifest file.
    pub path: PathBuf,
    /// 稳定的 Adapter 标识。
    /// Stable Adapter id.
    pub id: AdapterId,
    /// 面向用户的 Adapter 名称。
    /// Human-readable adapter name.
    pub name: String,
    /// 清单版本。
    /// Manifest version.
    pub version: String,
    /// 是否允许注册该 Adapter。
    /// Whether this adapter can be registered.
    pub enabled: bool,
    /// 已规范化的 Adapter 传输方式。
    /// Supported adapter transport.
    pub transport: AdapterTransport,
    /// 该 Adapter 暴露的能力集合。
    /// Capabilities exposed through this adapter.
    pub capabilities: Vec<CapabilityName>,
    /// Provider process restart, identity, and credential-reference configuration.
    pub provider: ProviderConfig,
    /// 由 Adapter、运行时或策略 crate 解释的扩展字段。
    /// Additional fields owned by adapter/runtime/policy crates.
    pub extra: Mapping,
}

/// Provider process configuration normalized before runtime registration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderConfig {
    /// Restart behavior reserved for the durable restart controller.
    pub restart: ProviderRestartConfig,
    /// Requested process identity; enforcement belongs to the OS process backend.
    pub run_as: ProviderRunAsIdentity,
    /// Vault references mapped to environment-variable injection targets.
    pub vault_secrets: Vec<ProviderVaultSecretRef>,
}

/// Stable provider restart modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ProviderRestartMode {
    /// Never restart automatically; legacy manifests use this default.
    #[default]
    None,
    /// Restart only after an unsuccessful provider exit.
    OnFailure,
    /// Restart after every provider exit while budget remains.
    Always,
}

/// Bounded restart declaration consumed later by the durable restart controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderRestartConfig {
    /// Restart decision mode.
    pub mode: ProviderRestartMode,
    /// Maximum automatic restart attempts after the initial process start.
    pub max_attempts: u32,
    /// Base delay between restart attempts in milliseconds.
    pub backoff_ms: u64,
}

/// Explicit process identity requested by an Adapter manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProviderRunAsIdentity {
    /// Inherit the daemon identity; legacy manifests use this default.
    #[default]
    Current,
    /// Run under one Unix numeric user/group pair.
    Unix {
        /// Numeric Unix user id.
        uid: u32,
        /// Numeric Unix primary group id.
        gid: u32,
    },
    /// Run with a Windows account token resolved by the process backend.
    Windows {
        /// Stable Windows account name; no credential material is stored here.
        account: String,
    },
}

/// One secret reference whose value may later be injected into an allowed env target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderVaultSecretRef {
    /// Environment variable receiving the fetched secret for one provider session.
    pub env: String,
    /// Opaque vault location; this is a reference, never secret bytes.
    pub secret_ref: String,
}

/// MCP Streamable HTTP manifest fields normalized without exposing YAML values
/// to runtime crates.
#[derive(Clone, PartialEq, Eq)]
pub struct McpHttpManifestConfig {
    /// Absolute HTTP(S) endpoint.
    pub endpoint: String,
    /// Trust-root references (never certificate bytes).
    pub trust_roots: Vec<String>,
    /// Optional client certificate reference.
    pub client_certificate_ref: Option<String>,
    /// Optional client private-key reference.
    pub client_private_key_ref: Option<String>,
    /// Redirect mode spelling.
    pub redirect_mode: String,
    /// Redirect hop limit, when declared.
    pub redirect_max_hops: Option<u64>,
    /// Explicit allowed origins.
    pub allowed_origins: Vec<String>,
}

impl std::fmt::Debug for McpHttpManifestConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpManifestConfig")
            .field("endpoint_present", &!self.endpoint.is_empty())
            .field("trust_root_count", &self.trust_roots.len())
            .field(
                "client_auth_configured",
                &(self.client_certificate_ref.is_some() || self.client_private_key_ref.is_some()),
            )
            .field("redirect_mode", &self.redirect_mode)
            .field("redirect_max_hops", &self.redirect_max_hops)
            .field("allowed_origin_count", &self.allowed_origins.len())
            .finish()
    }
}

impl ProviderRestartMode {
    /// Parses the stable manifest spelling.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "none" => Ok(Self::None),
            "on_failure" => Ok(Self::OnFailure),
            "always" => Ok(Self::Always),
            _ => Err(EvaError::unsupported("unsupported provider restart mode")
                .with_context("restart_mode", value)),
        }
    }

    /// Returns the stable manifest/storage spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OnFailure => "on_failure",
            Self::Always => "always",
        }
    }
}

impl ProviderRunAsIdentity {
    /// Returns the stable identity kind without exposing account details.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Unix { .. } => "unix",
            Self::Windows { .. } => "windows",
        }
    }
}

/// 从扩展字段保留的类 JSON Schema 对象子集。
/// JSON-Schema-like object schema subset preserved from manifest extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestObjectSchema {
    /// 可选对象类型字符串。
    pub schema_type: Option<String>,
    /// 必填属性名列表。
    pub required: Vec<String>,
    /// 按属性名索引的属性约束。
    pub properties: BTreeMap<String, ManifestSchemaProperty>,
}

/// 当前 Adapter 清单使用的类 JSON Schema 属性子集。
/// JSON-Schema-like property subset used by current Adapter manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSchemaProperty {
    /// 可选属性值类型。
    pub value_type: Option<String>,
    /// 允许的字符串枚举值。
    pub enum_values: Vec<String>,
}

/// 从 `hardware` 扩展区解析出的完整硬件 Adapter 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareAdapterConfig {
    /// 设备发现使用的总线类别。
    pub bus: HardwareBusKind,
    /// 用于匹配物理或模拟设备的条件。
    pub match_rule: HardwareMatchConfig,
    /// 暴露给运行时的逻辑身份。
    pub identity: HardwareIdentityConfig,
    /// 与设备通信的协议设置。
    pub protocol: HardwareProtocolConfig,
    /// 热插拔声明、重连退避和尝试次数。
    pub hotplug: HardwareHotplugConfig,
    /// 实际驱动实现声明。
    pub driver: HardwareDriverConfig,
}

/// 硬件设备所在的总线类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareBusKind {
    /// USB 总线。
    Usb,
    /// 串行端口。
    Serial,
    /// 低功耗蓝牙（Bluetooth Low Energy）总线。
    Ble,
    /// 网络 Socket。
    Socket,
    /// 厂商 SDK 管理的专有总线。
    VendorSdk,
}

/// 可选硬件发现匹配条件。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareMatchConfig {
    /// USB 等总线使用的厂商标识。
    pub vendor_id: Option<String>,
    /// 产品标识。
    pub product_id: Option<String>,
    /// 设备序列号。
    pub serial: Option<String>,
    /// BLE 或网络设备地址。
    pub address: Option<String>,
    /// 受控设备路径。
    pub path: Option<String>,
    /// BLE 服务 UUID。
    pub service_uuid: Option<String>,
}

/// 硬件设备在 Eva 中的稳定逻辑身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareIdentityConfig {
    /// 用于路由和展示的逻辑名称。
    pub logical_name: String,
    /// 设备能力类别。
    pub device_class: String,
}

/// 硬件传输协议及其可选参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProtocolConfig {
    /// 协议实现类别。
    pub kind: HardwareProtocolKind,
    /// 串行协议使用的可选波特率。
    pub baud_rate: Option<u32>,
    /// 网络协议使用的可选端点。
    pub endpoint: Option<String>,
    /// BLE GATT 使用的可选服务 UUID。
    pub service_uuid: Option<String>,
}

/// 支持声明的硬件协议类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareProtocolKind {
    /// 无真实 I/O 的模拟协议。
    Simulated,
    /// 每行一个 JSON 帧的文本协议。
    LineJson,
    /// 通用二进制帧协议。
    Binary,
    /// 使用串行链路的 Modbus RTU 协议。
    ModbusRtu,
    /// 低功耗蓝牙 GATT 协议。
    BleGatt,
    /// 面向连接的 TCP Socket 协议。
    TcpSocket,
    /// 无连接的 UDP Socket 协议。
    UdpSocket,
    /// 厂商 SDK 私有协议。
    VendorSdk,
}

/// 硬件热插拔和租用策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareHotplugConfig {
    /// 设备句柄的独占或共享读取模式。
    pub claim: HardwareClaimMode,
    /// 相邻重连尝试之间的等待毫秒数。
    pub reconnect_backoff_ms: u64,
    /// 最大重连尝试次数，零表示不自动重试。
    pub max_reconnect_attempts: u32,
}

/// 硬件设备句柄的租用模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareClaimMode {
    /// 单个驱动独占读写设备。
    Exclusive,
    /// 多个消费者只读共享。
    SharedRead,
}

/// 硬件驱动的稳定标识和实现类别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareDriverConfig {
    /// 驱动实例标识。
    pub driver_id: String,
    /// 驱动实现类别。
    pub kind: HardwareDriverKind,
}

/// 支持声明的硬件驱动类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareDriverKind {
    /// 无真实设备访问的模拟驱动。
    Simulated,
    /// USB 驱动。
    Usb,
    /// 串行驱动。
    Serial,
    /// BLE 驱动。
    Ble,
    /// 网络 Socket 驱动。
    Socket,
    /// 厂商 SDK 驱动。
    VendorSdk,
}

/// 支持的 Adapter 传输实现。
/// Supported adapter transport implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdapterTransport {
    /// 进程内置实现。
    Builtin,
    /// 标准输入输出子进程协议。
    Stdio,
    /// HTTP 传输。
    Http,
    /// EventBus 传输。
    Eventbus,
    /// Model Context Protocol（MCP）传输。
    Mcp,
    /// Skill 调用边界。
    Skill,
    /// 受硬件权限和租约约束的传输。
    Hardware,
    /// Lua Capability 宿主边界。
    LuaCapability,
}

/// 为 Schema 对齐测试保留的原始传输类别别名。
/// Raw transport spelling retained for schema alignment tests.
pub type RawAdapterTransport = AdapterTransport;

/// 从 YAML 文件加载并验证一份 Adapter 清单。
/// Loads and validates one Adapter manifest file.
pub fn load_adapter_manifest(path: impl AsRef<Path>) -> Result<AdapterManifest, EvaError> {
    let path = path.as_ref();
    let raw: RawAdapterManifest = read_yaml_file(path, CONFIG_TYPE)?;
    AdapterManifest::try_from_raw(path.to_path_buf(), raw)
}

impl AdapterManifest {
    /// 将原始 YAML 字段规范化为强类型标识，并保留未知扩展字段。
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
        let credential_env = validated_credential_env(&raw.extra, &path)?;
        let provider = parse_provider_config(
            &path,
            transport,
            raw.supervision,
            raw.credentials,
            &credential_env,
            &raw.extra,
        )?;

        let manifest = Self {
            path,
            id,
            name,
            version,
            enabled: raw.enabled,
            transport,
            capabilities,
            provider,
            extra: raw.extra,
        };
        // MCP transport contracts are validated even when callers load a single
        // manifest directly instead of going through ProjectConfig/schema loading.
        manifest.mcp_http_manifest_config()?;
        Ok(manifest)
    }

    /// 读取扩展映射中的顶层字符串字段。
    /// Returns a top-level string field preserved in the manifest extension map.
    pub fn extra_string(&self, key: &str) -> Option<&str> {
        self.extra
            .get(Value::String(key.to_owned()))
            .and_then(Value::as_str)
    }

    /// 读取顶层字符串列表，非字符串元素会被忽略。
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

    /// 读取顶层字符串键值映射，非字符串条目会被忽略。
    /// Returns a top-level string map preserved in the manifest extension map.
    pub fn extra_string_map(&self, key: &str) -> BTreeMap<String, String> {
        self.extra
            .get(Value::String(key.to_owned()))
            .and_then(Value::as_mapping)
            .map(string_map)
            .unwrap_or_default()
    }

    /// 读取一层嵌套扩展字符串字段。
    /// Returns a nested string field preserved in the manifest extension map.
    pub fn nested_extra_string(&self, section: &str, key: &str) -> Option<&str> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_str)
    }

    /// 读取一层嵌套字符串列表。
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

    /// 读取一层嵌套字符串映射。
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

    /// 读取一层嵌套无符号整数。
    /// Returns a nested unsigned integer preserved in the manifest extension map.
    pub fn nested_extra_u64(&self, section: &str, key: &str) -> Option<u64> {
        self.extra
            .get(Value::String(section.to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mapping| mapping.get(Value::String(key.to_owned())))
            .and_then(Value::as_u64)
    }

    /// 读取一层嵌套整数，并在超出当前平台 `usize` 时返回 `None`。
    /// Returns a nested `usize` preserved in the manifest extension map.
    pub fn nested_extra_usize(&self, section: &str, key: &str) -> Option<usize> {
        self.nested_extra_u64(section, key)
            .and_then(|value| usize::try_from(value).ok())
    }

    /// 沿任意深度扩展路径读取字符串；任一层缺失或类型不符即返回 `None`。
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

    /// 沿任意深度扩展路径读取无符号整数。
    /// Returns an unsigned integer from a nested manifest extension path.
    pub fn deep_extra_u64(&self, path: &[&str]) -> Option<u64> {
        self.deep_extra_value(path).and_then(Value::as_u64)
    }

    /// 沿任意深度扩展路径读取字符串列表。
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

    /// 沿任意深度扩展路径读取映射，供拥有该扩展的下游 crate 做强类型投影。
    /// Returns a nested mapping without silently coercing malformed values.
    pub fn deep_extra_mapping(&self, path: &[&str]) -> Option<&Mapping> {
        self.deep_extra_value(path).and_then(Value::as_mapping)
    }

    /// 读取并类型检查 MCP Streamable HTTP 扩展字段。
    ///
    /// 该方法只负责 YAML 形状和引用字段的规范化；endpoint/origin 的协议级
    /// canonical 校验由 eva-mcp 的配置合同执行，production 环境门禁在
    /// `validate_for_environment` 中完成。
    pub fn mcp_http_manifest_config(&self) -> Result<Option<McpHttpManifestConfig>, EvaError> {
        if self.transport != AdapterTransport::Mcp {
            return Ok(None);
        }
        let mcp_value = self.extra.get(Value::String("mcp".to_owned()));
        let mcp = match mcp_value {
            None => None,
            Some(Value::Mapping(mapping)) => Some(mapping),
            Some(_) => {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp",
                    "MCP configuration must be a mapping",
                ))
            }
        };
        if let Some(mcp) = mcp {
            reject_unknown_mcp_fields(mcp, &self.path)?;
            for (key, field) in [
                ("tool_allowlist", "mcp.tool_allowlist"),
                ("resource_allowlist", "mcp.resource_allowlist"),
                ("prompt_allowlist", "mcp.prompt_allowlist"),
            ] {
                mapping_string_list(mcp, key, &self.path, field)?;
            }
        }
        let server_transport = match mcp
            .and_then(|mapping| mapping.get(Value::String("server_transport".to_owned())))
        {
            None => "stdio",
            Some(Value::String(value)) => value.as_str(),
            Some(_) => {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.server_transport",
                    "server_transport must be a string",
                ))
            }
        };
        if !matches!(server_transport, "http" | "streamable_http") {
            if server_transport != "stdio" {
                return Err(EvaError::unsupported("unsupported MCP server transport")
                    .with_context("server_transport", server_transport));
            }
            for (field, value) in [
                (
                    "command",
                    self.extra.get(Value::String("command".to_owned())),
                ),
                ("args", self.extra.get(Value::String("args".to_owned()))),
                (
                    "mcp.endpoint",
                    mcp.and_then(|mapping| mapping.get(Value::String("endpoint".to_owned()))),
                ),
                (
                    "mcp.headers",
                    mcp.and_then(|mapping| mapping.get(Value::String("headers".to_owned()))),
                ),
                (
                    "mcp.http",
                    mcp.and_then(|mapping| mapping.get(Value::String("http".to_owned()))),
                ),
                (
                    "endpoint",
                    self.extra.get(Value::String("endpoint".to_owned())),
                ),
                (
                    "headers",
                    self.extra.get(Value::String("headers".to_owned())),
                ),
                ("http", self.extra.get(Value::String("http".to_owned()))),
            ] {
                if value.is_none() {
                    continue;
                }
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    field,
                    "field is not valid in the MCP stdio configuration union",
                ));
            }
            let command = mcp
                .and_then(|mapping| mapping.get(Value::String("command".to_owned())))
                .ok_or_else(|| {
                    invalid_config(
                        CONFIG_TYPE,
                        &self.path,
                        "mcp.command",
                        "MCP stdio transport requires command",
                    )
                })?;
            let command = command.as_str().ok_or_else(|| {
                invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.command",
                    "MCP stdio command must be a string",
                )
            })?;
            if command.trim().is_empty() || command != command.trim() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.command",
                    "MCP stdio command must be non-empty and trimmed",
                ));
            }
            if let Some(mcp) = mcp {
                mapping_argv(mcp, "args", &self.path, "mcp.args")?;
            }
            return Ok(None);
        }
        for (field, value) in [
            (
                "mcp.command",
                mcp.and_then(|mapping| mapping.get(Value::String("command".to_owned()))),
            ),
            (
                "mcp.args",
                mcp.and_then(|mapping| mapping.get(Value::String("args".to_owned()))),
            ),
            (
                "command",
                self.extra.get(Value::String("command".to_owned())),
            ),
            ("args", self.extra.get(Value::String("args".to_owned()))),
        ] {
            if value.is_some() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    field,
                    "MCP process fields are only valid for stdio transport",
                ));
            }
        }
        validate_mcp_http_headers(&self.path, &self.extra, mcp)?;
        let nested_endpoint =
            mcp.and_then(|mapping| mapping.get(Value::String("endpoint".to_owned())));
        let nested_endpoint = match nested_endpoint {
            None => None,
            Some(Value::String(value)) => Some(value.as_str()),
            Some(_) => {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.endpoint",
                    "MCP endpoint must be a string",
                ))
            }
        };
        let top_level_endpoint = match self.extra.get(Value::String("endpoint".to_owned())) {
            None => None,
            Some(Value::String(value)) => Some(value.as_str()),
            Some(_) => {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "endpoint",
                    "top-level MCP endpoint must be a string",
                ))
            }
        };
        if let (Some(nested), Some(top_level)) = (nested_endpoint, top_level_endpoint) {
            if nested != top_level {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.endpoint",
                    "nested and top-level MCP endpoints must not conflict",
                ));
            }
        }
        let endpoint = nested_endpoint
            .or(top_level_endpoint)
            .ok_or_else(|| {
                invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.endpoint",
                    "Streamable HTTP transport requires endpoint",
                )
            })?
            .to_owned();
        let Some(http) = self.deep_extra_mapping(&["mcp", "http"]) else {
            if self.deep_extra_value(&["mcp", "http"]).is_some() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    &self.path,
                    "mcp.http",
                    "MCP HTTP policy must be a mapping",
                ));
            }
            let config = McpHttpManifestConfig {
                endpoint,
                trust_roots: Vec::new(),
                client_certificate_ref: None,
                client_private_key_ref: None,
                redirect_mode: "deny".to_owned(),
                redirect_max_hops: None,
                allowed_origins: Vec::new(),
            };
            validate_mcp_http_manifest_config(&self.path, &config, false)?;
            return Ok(Some(config));
        };

        reject_unknown_mcp_http_fields(http, &self.path)?;
        let trust_roots =
            mapping_string_list(http, "trust_roots", &self.path, "mcp.http.trust_roots")?;
        let allowed_origins = mapping_string_list(
            http,
            "allowed_origins",
            &self.path,
            "mcp.http.allowed_origins",
        )?;
        let (client_certificate_ref, client_private_key_ref) =
            parse_mcp_client_auth_mapping(http, &self.path)?;
        let (redirect_mode, redirect_max_hops) = parse_mcp_redirect_mapping(http, &self.path)?;
        let config = McpHttpManifestConfig {
            endpoint,
            trust_roots,
            client_certificate_ref,
            client_private_key_ref,
            redirect_mode,
            redirect_max_hops,
            allowed_origins,
        };
        validate_mcp_http_manifest_config(&self.path, &config, false)?;
        Ok(Some(config))
    }

    /// 解析工作流 Skill 清单使用的对象 Schema 子集。
    ///
    /// 只提取 type、required 及属性的 type/enum；未知或类型不符的 Schema 字段被跳过，
    /// 因为这里是扩展读取器而非完整 JSON Schema 验证器。
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

    /// 沿扩展映射路径返回原始 YAML 值。
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

    /// Rejects plaintext credential-bearing fields for production environments.
    pub fn validate_for_environment(&self, environment: &str) -> Result<(), EvaError> {
        let production = matches!(
            environment.to_ascii_lowercase().as_str(),
            "prod" | "production"
        );
        if let Some(config) = self.mcp_http_manifest_config()? {
            validate_mcp_http_manifest_config(&self.path, &config, production)?;
            if production {
                for (field, reference) in [
                    (
                        "mcp.http.client_auth.cert_ref",
                        config.client_certificate_ref.as_deref(),
                    ),
                    (
                        "mcp.http.client_auth.key_ref",
                        config.client_private_key_ref.as_deref(),
                    ),
                ] {
                    let Some(reference) = reference else {
                        continue;
                    };
                    if reference.starts_with("env:") {
                        validate_env_credential_reference(self, field, reference)?;
                    } else if reference.starts_with("vault://")
                        && !self
                            .provider
                            .vault_secrets
                            .iter()
                            .any(|secret| secret.secret_ref == reference)
                    {
                        return Err(invalid_config(
                            CONFIG_TYPE,
                            &self.path,
                            field,
                            "client auth vault reference is not declared by credentials.vault",
                        ));
                    }
                }
            }
        }
        if production {
            validate_production_manifest_secrets(self, &self.extra, "")?;
        }
        Ok(())
    }

    /// 仅对 Hardware 传输解析 `hardware` 扩展区的强类型配置。
    /// Parses the typed hardware adapter config preserved under `hardware`.
    pub fn hardware_config(&self) -> Result<Option<HardwareAdapterConfig>, EvaError> {
        if self.transport != AdapterTransport::Hardware {
            return Ok(None);
        }
        HardwareAdapterConfig::from_manifest(self).map(Some)
    }
}

fn mapping_string_list(
    mapping: &Mapping,
    key: &str,
    path: &Path,
    field: &str,
) -> Result<Vec<String>, EvaError> {
    let Some(value) = mapping.get(Value::String(key.to_owned())) else {
        return Ok(Vec::new());
    };
    let values = value.as_sequence().ok_or_else(|| {
        invalid_config(CONFIG_TYPE, path, field, "MCP field must be a string list")
    })?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.as_str().ok_or_else(|| {
                invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}[{index}]"),
                    "MCP list entry must be a string",
                )
            })?;
            if value.trim().is_empty() || value != value.trim() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}[{index}]"),
                    "MCP list entry must be non-empty and trimmed",
                ));
            }
            Ok(value.to_owned())
        })
        .collect()
}

fn mapping_argv(
    mapping: &Mapping,
    key: &str,
    path: &Path,
    field: &str,
) -> Result<Vec<String>, EvaError> {
    let Some(value) = mapping.get(Value::String(key.to_owned())) else {
        return Ok(Vec::new());
    };
    let values = value.as_sequence().ok_or_else(|| {
        invalid_config(CONFIG_TYPE, path, field, "MCP argv must be a string list")
    })?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.as_str().ok_or_else(|| {
                invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}[{index}]"),
                    "MCP argv entry must be a string",
                )
            })?;
            if value.contains('\0') {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}[{index}]"),
                    "MCP argv entry must not contain NUL",
                ));
            }
            Ok(value.to_owned())
        })
        .collect()
}

fn validate_mcp_http_headers(
    path: &Path,
    extra: &Mapping,
    mcp: Option<&Mapping>,
) -> Result<(), EvaError> {
    let key = |name: &str| Value::String(name.to_owned());
    let top_level = extra.get(key("headers"));
    let legacy_http = extra
        .get(key("http"))
        .and_then(Value::as_mapping)
        .and_then(|mapping| mapping.get(key("headers")));
    let nested = mcp.and_then(|mapping| mapping.get(key("headers")));
    let mut seen = BTreeSet::new();
    for (field, value) in [
        ("headers", top_level),
        ("http.headers", legacy_http),
        ("mcp.headers", nested),
    ] {
        let Some(value) = value else {
            continue;
        };
        let headers = value.as_mapping().ok_or_else(|| {
            invalid_config(CONFIG_TYPE, path, field, "MCP headers must be a mapping")
        })?;
        for (name, value) in headers {
            let name = name.as_str().ok_or_else(|| {
                invalid_config(CONFIG_TYPE, path, field, "MCP header names must be strings")
            })?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}.{name}"),
                    "MCP header name is invalid",
                ));
            }
            let normalized = name.to_ascii_lowercase();
            if is_transport_controlled_header(&normalized) {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}.{name}"),
                    "MCP header is controlled by the HTTP transport",
                ));
            }
            if !seen.insert(normalized) {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}.{name}"),
                    "MCP header names must be unique ignoring ASCII case",
                ));
            }
            let value = value.as_str().ok_or_else(|| {
                invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}.{name}"),
                    "MCP header values must be strings",
                )
            })?;
            if value
                .chars()
                .any(|character| character != '\t' && character.is_control())
            {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    format!("{field}.{name}"),
                    "MCP header values must not contain control characters other than HTAB",
                ));
            }
        }
    }
    Ok(())
}

fn is_transport_controlled_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "connection"
            | "content-type"
            | "accept"
            | "content-length"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-connection"
    )
}

fn reject_unknown_mcp_fields(mapping: &Mapping, path: &Path) -> Result<(), EvaError> {
    for key in mapping.keys() {
        let key = key.as_str().ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp",
                "MCP configuration keys must be strings",
            )
        })?;
        if !matches!(
            key,
            "server_transport"
                | "command"
                | "args"
                | "endpoint"
                | "headers"
                | "http"
                | "tool_allowlist"
                | "resource_allowlist"
                | "prompt_allowlist"
        ) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                format!("mcp.{key}"),
                "unsupported MCP configuration field",
            ));
        }
    }
    Ok(())
}

fn reject_unknown_mcp_http_fields(mapping: &Mapping, path: &Path) -> Result<(), EvaError> {
    for key in mapping.keys() {
        let key = key.as_str().ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http",
                "MCP HTTP policy keys must be strings",
            )
        })?;
        if !matches!(
            key,
            "trust_roots" | "client_auth" | "redirect" | "allowed_origins"
        ) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                format!("mcp.http.{key}"),
                "unsupported MCP HTTP policy field",
            ));
        }
    }
    Ok(())
}

fn parse_mcp_client_auth_mapping(
    mapping: &Mapping,
    path: &Path,
) -> Result<(Option<String>, Option<String>), EvaError> {
    let Some(value) = mapping.get(Value::String("client_auth".to_owned())) else {
        return Ok((None, None));
    };
    if value.as_str() == Some("none") {
        return Ok((None, None));
    }
    let auth = value.as_mapping().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.client_auth",
            "client_auth must be none or a mapping",
        )
    })?;
    for key in auth.keys() {
        let key = key.as_str().ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.client_auth",
                "client_auth keys must be strings",
            )
        })?;
        if !matches!(key, "cert_ref" | "key_ref") {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                format!("mcp.http.client_auth.{key}"),
                "unsupported client_auth field",
            ));
        }
    }
    let certificate_value = auth
        .get(Value::String("cert_ref".to_owned()))
        .ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.client_auth.cert_ref",
                "client certificate reference is required",
            )
        })?;
    let certificate_ref = certificate_value.as_str().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.client_auth.cert_ref",
            "client certificate reference must be a string",
        )
    })?;
    let private_key_value = auth
        .get(Value::String("key_ref".to_owned()))
        .ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.client_auth.key_ref",
                "client private-key reference is required",
            )
        })?;
    let private_key_ref = private_key_value.as_str().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.client_auth.key_ref",
            "client private-key reference must be a string",
        )
    })?;
    if certificate_ref.trim().is_empty()
        || private_key_ref.trim().is_empty()
        || certificate_ref != certificate_ref.trim()
        || private_key_ref != private_key_ref.trim()
    {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.client_auth",
            "client auth references must be non-empty and trimmed",
        ));
    }
    Ok((
        Some(certificate_ref.to_owned()),
        Some(private_key_ref.to_owned()),
    ))
}

fn parse_mcp_redirect_mapping(
    mapping: &Mapping,
    path: &Path,
) -> Result<(String, Option<u64>), EvaError> {
    let Some(value) = mapping.get(Value::String("redirect".to_owned())) else {
        return Ok(("deny".to_owned(), None));
    };
    if let Some(mode) = value.as_str() {
        if !matches!(mode, "deny" | "same_origin") {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.redirect",
                "unsupported redirect policy",
            ));
        }
        return Ok((mode.to_owned(), None));
    }
    let redirect = value.as_mapping().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.redirect",
            "redirect must be a mode string or mapping",
        )
    })?;
    for key in redirect.keys() {
        let key = key.as_str().ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.redirect",
                "redirect keys must be strings",
            )
        })?;
        if !matches!(key, "mode" | "max_hops") {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                format!("mcp.http.redirect.{key}"),
                "unsupported redirect field",
            ));
        }
    }
    let mode_value = redirect
        .get(Value::String("mode".to_owned()))
        .ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.redirect.mode",
                "redirect mapping requires mode",
            )
        })?;
    let mode = mode_value.as_str().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.redirect.mode",
            "redirect mode must be a string",
        )
    })?;
    if !matches!(mode, "deny" | "same_origin") {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.redirect.mode",
            "unsupported redirect policy",
        ));
    }
    let max_hops = match redirect.get(Value::String("max_hops".to_owned())) {
        None => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.redirect.max_hops",
                "max_hops must be an integer",
            )
        })?),
    };
    Ok((mode.to_owned(), max_hops))
}

fn validate_mcp_http_manifest_config(
    path: &Path,
    config: &McpHttpManifestConfig,
    production: bool,
) -> Result<(), EvaError> {
    let endpoint_origin = canonical_mcp_origin(&config.endpoint, false)?;
    let endpoint_scheme = config
        .endpoint
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_default();
    let mut roots = BTreeSet::new();
    for root in &config.trust_roots {
        validate_mcp_trust_root(path, "mcp.http.trust_roots", root)?;
        if !roots.insert(root) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.trust_roots",
                "duplicate trust root reference",
            ));
        }
    }
    for (field, value) in [
        (
            "mcp.http.client_auth.cert_ref",
            config.client_certificate_ref.as_deref(),
        ),
        (
            "mcp.http.client_auth.key_ref",
            config.client_private_key_ref.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            validate_mcp_credential_ref(path, field, value)?;
        }
    }
    if endpoint_scheme == "http" && !roots.is_empty() {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.trust_roots",
            "trust roots require an HTTPS endpoint",
        ));
    }
    if endpoint_scheme == "http"
        && (config.client_certificate_ref.is_some() || config.client_private_key_ref.is_some())
    {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.client_auth",
            "client certificate authentication requires an HTTPS endpoint",
        ));
    }
    match config.redirect_mode.as_str() {
        "deny" => {
            if config.redirect_max_hops.is_some_and(|value| value != 0) {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "mcp.http.redirect.max_hops",
                    "deny redirect policy requires max_hops=0",
                ));
            }
        }
        "same_origin" => {
            let hops = config.redirect_max_hops.unwrap_or(3);
            if !(1..=10).contains(&hops) {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "mcp.http.redirect.max_hops",
                    "same_origin max_hops must be between 1 and 10",
                ));
            }
        }
        _ => {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.redirect.mode",
                "unsupported redirect policy",
            ))
        }
    }
    let mut origins = BTreeSet::new();
    for origin in &config.allowed_origins {
        if origin.contains('*') {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.allowed_origins",
                "wildcard origins are not allowed",
            ));
        }
        let canonical = canonical_mcp_origin(origin, true)?;
        if canonical != *origin {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.allowed_origins",
                "origin must use canonical spelling",
            ));
        }
        if !origins.insert(origin) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "mcp.http.allowed_origins",
                "duplicate origin policy entry",
            ));
        }
    }
    if !origins.is_empty() && !origins.contains(&endpoint_origin) {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.allowed_origins",
            "origin policy must include the endpoint origin",
        ));
    }
    if production && endpoint_scheme == "https" && roots.is_empty() {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.trust_roots",
            "production HTTPS requires an explicit trust policy",
        ));
    }
    if production && endpoint_scheme == "https" && origins.is_empty() {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "mcp.http.allowed_origins",
            "production HTTPS requires an explicit origin policy",
        ));
    }
    Ok(())
}

fn validate_mcp_credential_ref(path: &Path, field: &str, value: &str) -> Result<(), EvaError> {
    if value.is_empty() || value != value.trim() {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            field,
            "credential reference must be non-empty and trimmed",
        ));
    }
    if let Some(name) = value.strip_prefix("env:") {
        let mut bytes = name.bytes();
        let valid = name.len() <= 128
            && bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if !valid {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                field,
                "env credential reference has an invalid name",
            ));
        }
        return Ok(());
    }
    if let Some(body) = value.strip_prefix("vault://") {
        let mut parts = body.split('#');
        let secret_path = parts.next().unwrap_or_default();
        let key = parts.next();
        let path_valid = !secret_path.is_empty()
            && secret_path.len() <= 384
            && secret_path.split('/').all(is_valid_vault_segment);
        let key_valid = key.is_none_or(is_valid_vault_key);
        if value.len() <= 512 && path_valid && key_valid && parts.next().is_none() {
            return Ok(());
        }
    }
    Err(invalid_config(
        CONFIG_TYPE,
        path,
        field,
        "credential reference must use env: or vault://",
    ))
}

fn validate_mcp_trust_root(path: &Path, field: &str, root: &str) -> Result<(), EvaError> {
    if root.is_empty()
        || root != root.trim()
        || root
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            field,
            "trust root references must be non-empty and trimmed",
        ));
    }
    if root == "system" {
        return Ok(());
    }
    if let Some(file) = root.strip_prefix("file:") {
        let invalid = file.is_empty()
            || file.starts_with(['/', '\\'])
            || file
                .split(['/', '\\'])
                .any(|segment| segment.is_empty() || segment == "..")
            || file
                .split(['/', '\\'])
                .next()
                .is_some_and(|segment| segment.contains(':'));
        if invalid {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                field,
                "file trust root must stay within the project-relative root",
            ));
        }
        return Ok(());
    }
    if let Some(reference) = root.strip_prefix("pem:") {
        if reference.is_empty() || reference.starts_with('-') || reference.contains("BEGIN ") {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                field,
                "inline PEM bytes are not allowed; use a reference or digest",
            ));
        }
        return Ok(());
    }
    Err(invalid_config(
        CONFIG_TYPE,
        path,
        field,
        "trust root must use system, file:, or pem: reference",
    ))
}

fn canonical_mcp_origin(value: &str, origin_only: bool) -> Result<String, EvaError> {
    let (scheme, rest) = value.split_once("://").ok_or_else(|| {
        EvaError::invalid_argument("MCP endpoint must include an http or https scheme")
    })?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(EvaError::unsupported("MCP endpoint scheme is unsupported")
            .with_context("scheme", scheme));
    }
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(EvaError::invalid_argument(
            "MCP endpoint contains whitespace",
        ));
    }
    if rest.contains('#') {
        return Err(EvaError::invalid_argument(
            "MCP endpoint must not contain a fragment",
        ));
    }
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let raw_authority = &rest[..authority_end];
    if raw_authority.is_empty() || raw_authority.contains('@') || raw_authority.contains('*') {
        return Err(EvaError::invalid_argument(
            "MCP endpoint authority is malformed",
        ));
    }
    let authority = canonical_mcp_authority(&scheme, raw_authority)?;
    if origin_only && rest != authority {
        // The comparison above is intentionally strict; default-port spellings
        // are rejected rather than silently changing the manifest digest.
        return Err(EvaError::invalid_argument(
            "MCP origin policy entry must contain only canonical authority",
        ));
    }
    Ok(format!("{scheme}://{authority}"))
}

fn canonical_mcp_authority(scheme: &str, authority: &str) -> Result<String, EvaError> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| EvaError::invalid_argument("MCP IPv6 authority is malformed"))?;
        let host = &authority[1..end];
        let host = host
            .parse::<Ipv6Addr>()
            .map_err(|_| EvaError::invalid_argument("MCP IPv6 address is malformed"))?;
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            let value = suffix
                .strip_prefix(':')
                .ok_or_else(|| EvaError::invalid_argument("MCP IPv6 authority is malformed"))?;
            Some(parse_mcp_port(value)?)
        };
        let host = format!("[{host}]");
        return Ok(match port {
            Some(port) if port != default_port => format!("{host}:{port}"),
            _ => host,
        });
    }
    if authority.bytes().filter(|byte| *byte == b':').count() > 1 {
        return Err(EvaError::invalid_argument(
            "MCP IPv6 addresses must use brackets",
        ));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .map(|(host, port)| (host, Some(parse_mcp_port(port))))
        .unwrap_or((authority, None));
    let port = port.transpose()?;
    if host.is_empty()
        || host
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err(EvaError::invalid_argument("MCP endpoint host is malformed"));
    }
    let host = host.to_ascii_lowercase();
    Ok(match port {
        Some(port) if port != default_port => format!("{host}:{port}"),
        _ => host,
    })
}

fn parse_mcp_port(value: &str) -> Result<u16, EvaError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| EvaError::invalid_argument("MCP endpoint port is invalid"))?;
    if port == 0 {
        return Err(EvaError::invalid_argument(
            "MCP endpoint port must be non-zero",
        ));
    }
    Ok(port)
}

fn parse_provider_config(
    path: &Path,
    transport: AdapterTransport,
    supervision: RawProviderSupervision,
    credentials: RawProviderCredentials,
    credential_env: &[String],
    extra: &Mapping,
) -> Result<ProviderConfig, EvaError> {
    reject_unknown_fields(path, "supervision", &supervision.extra)?;
    reject_unknown_fields(path, "credentials", &credentials.extra)?;
    let process_backed = provider_transport_supports_process(transport, extra);
    let restart = parse_provider_restart(path, supervision.restart, process_backed)?;
    let run_as = parse_provider_run_as(path, supervision.run_as, process_backed)?;
    let vault_secrets = parse_provider_vault_refs(path, credentials.vault, credential_env)?;
    Ok(ProviderConfig {
        restart,
        run_as,
        vault_secrets,
    })
}

fn parse_provider_restart(
    path: &Path,
    raw: Option<RawProviderRestart>,
    process_backed: bool,
) -> Result<ProviderRestartConfig, EvaError> {
    let Some(raw) = raw else {
        return Ok(ProviderRestartConfig::default());
    };
    reject_unknown_fields(path, "supervision.restart", &raw.extra)?;
    let mode_text = raw.mode.ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "supervision.restart.mode",
            "explicit provider restart config requires mode",
        )
    })?;
    let mode = ProviderRestartMode::parse(&mode_text).map_err(|error| {
        with_field_context(error, CONFIG_TYPE, path, "supervision.restart.mode")
    })?;
    let max_attempts = u32::try_from(raw.max_attempts.unwrap_or(0)).map_err(|_| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "supervision.restart.max_attempts",
            "provider restart max_attempts exceeds u32",
        )
    })?;
    let backoff_ms = raw.backoff_ms.unwrap_or(0);
    match mode {
        ProviderRestartMode::None if max_attempts != 0 || backoff_ms != 0 => {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "supervision.restart",
                "restart mode none requires zero max_attempts and backoff_ms",
            ));
        }
        ProviderRestartMode::OnFailure | ProviderRestartMode::Always
            if max_attempts == 0 || backoff_ms == 0 =>
        {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "supervision.restart",
                "automatic restart requires positive max_attempts and backoff_ms",
            ));
        }
        _ => {}
    }
    if mode != ProviderRestartMode::None && !process_backed {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "supervision.restart.mode",
            "automatic restart requires a process-backed Adapter transport",
        ));
    }
    Ok(ProviderRestartConfig {
        mode,
        max_attempts,
        backoff_ms,
    })
}

fn parse_provider_run_as(
    path: &Path,
    raw: Option<RawProviderRunAs>,
    process_backed: bool,
) -> Result<ProviderRunAsIdentity, EvaError> {
    let Some(raw) = raw else {
        return Ok(ProviderRunAsIdentity::Current);
    };
    reject_unknown_fields(path, "supervision.run_as", &raw.extra)?;
    let kind = raw.kind.as_deref().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "supervision.run_as.kind",
            "explicit run_as config requires kind",
        )
    })?;
    let identity = match kind {
        "current" => {
            if raw.uid.is_some() || raw.gid.is_some() || raw.account.is_some() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "supervision.run_as",
                    "current identity cannot declare uid, gid, or account",
                ));
            }
            ProviderRunAsIdentity::Current
        }
        "unix" => {
            if raw.account.is_some() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "supervision.run_as.account",
                    "Unix identity cannot declare a Windows account",
                ));
            }
            let uid = required_u32(path, "supervision.run_as.uid", raw.uid)?;
            let gid = required_u32(path, "supervision.run_as.gid", raw.gid)?;
            ProviderRunAsIdentity::Unix { uid, gid }
        }
        "windows" => {
            if raw.uid.is_some() || raw.gid.is_some() {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "supervision.run_as",
                    "Windows identity cannot declare Unix uid or gid",
                ));
            }
            let account = raw.account.ok_or_else(|| {
                invalid_config(
                    CONFIG_TYPE,
                    path,
                    "supervision.run_as.account",
                    "Windows identity requires an account",
                )
            })?;
            if account.trim().is_empty()
                || account.trim() != account
                || account.len() > 256
                || account.chars().any(char::is_control)
            {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "supervision.run_as.account",
                    "Windows account is empty, untrimmed, oversized, or contains controls",
                ));
            }
            ProviderRunAsIdentity::Windows { account }
        }
        _ => {
            return Err(EvaError::unsupported("unsupported provider run_as kind")
                .with_context("run_as_kind", kind)
                .with_context("config_type", CONFIG_TYPE)
                .with_context("path", path.display().to_string())
                .with_context("field", "supervision.run_as.kind"));
        }
    };
    if identity != ProviderRunAsIdentity::Current && !process_backed {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            "supervision.run_as.kind",
            "non-current identity requires a process-backed Adapter transport",
        ));
    }
    Ok(identity)
}

fn parse_provider_vault_refs(
    path: &Path,
    raw: Vec<RawProviderVaultSecretRef>,
    credential_env: &[String],
) -> Result<Vec<ProviderVaultSecretRef>, EvaError> {
    let allowed = credential_env
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut parsed = Vec::with_capacity(raw.len());
    for (index, entry) in raw.into_iter().enumerate() {
        let prefix = format!("credentials.vault[{index}]");
        reject_unknown_fields(path, &prefix, &entry.extra)?;
        let env = entry.env.ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                format!("{prefix}.env"),
                "vault secret reference requires an env target",
            )
        })?;
        validate_env_name(path, &format!("{prefix}.env"), &env)?;
        if !allowed.contains(env.as_str()) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                format!("{prefix}.env"),
                "vault secret env target is not allowed by permissions.env",
            ));
        }
        if !seen.insert(env.clone()) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                format!("{prefix}.env"),
                "vault secret env target is duplicated",
            ));
        }
        let secret_ref = entry.secret_ref.ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                format!("{prefix}.ref"),
                "vault secret reference requires ref",
            )
        })?;
        validate_vault_secret_ref(path, &format!("{prefix}.ref"), &secret_ref)?;
        parsed.push(ProviderVaultSecretRef { env, secret_ref });
    }
    parsed.sort();
    Ok(parsed)
}

fn validated_credential_env(extra: &Mapping, path: &Path) -> Result<Vec<String>, EvaError> {
    let Some(permissions) = extra.get(Value::String("permissions".to_owned())) else {
        return Ok(Vec::new());
    };
    let permissions = permissions.as_mapping().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "permissions",
            "permissions must be an object",
        )
    })?;
    let Some(env) = permissions.get(Value::String("env".to_owned())) else {
        return Ok(Vec::new());
    };
    let env = env.as_sequence().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            path,
            "permissions.env",
            "permissions.env must be a string list",
        )
    })?;
    let mut seen = BTreeSet::new();
    let mut names = Vec::with_capacity(env.len());
    for (index, value) in env.iter().enumerate() {
        let field = format!("permissions.env[{index}]");
        let name = value.as_str().ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                &field,
                "credential env target must be a string",
            )
        })?;
        validate_env_name(path, &field, name)?;
        if !seen.insert(name.to_owned()) {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                field,
                "credential env target is duplicated",
            ));
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

fn validate_env_name(path: &Path, field: &str, value: &str) -> Result<(), EvaError> {
    let mut bytes = value.bytes();
    let valid = value.len() <= 128
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid && !is_dangerous_credential_env(value) {
        Ok(())
    } else {
        Err(invalid_config(
            CONFIG_TYPE,
            path,
            field,
            "credential env target is invalid or reserved by the provider supervisor",
        ))
    }
}

/// Credential values are injected into a child process, so names that alter
/// executable lookup, dynamic loading, interpreter startup, shell parsing, or
/// the supervisor's own session boundary must never be manifest-controlled.
/// Environment names are compared case-insensitively because Windows treats
/// them case-insensitively even though Unix does not.
fn is_dangerous_credential_env(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "PATHEXT"
            | "COMSPEC"
            | "PYTHONPATH"
            | "NODE_OPTIONS"
            | "BASH_ENV"
            | "ENV"
            | "IFS"
            | "EVA_PROVIDER_SESSION_ID"
            | "EVA_PROVIDER_SESSION_TOKEN"
    ) || upper.starts_with("LD_")
        || upper.starts_with("DYLD_")
        || upper.starts_with("EVA_PROVIDER_SESSION_")
}

fn validate_vault_secret_ref(path: &Path, field: &str, value: &str) -> Result<(), EvaError> {
    let Some(body) = value.strip_prefix("vault://") else {
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            field,
            "vault reference must start with vault://",
        ));
    };
    let mut parts = body.split('#');
    let secret_path = parts.next().unwrap_or_default();
    let key = parts.next();
    let has_extra_fragment = parts.next().is_some();
    let path_valid = !secret_path.is_empty()
        && secret_path.len() <= 384
        && secret_path.split('/').all(is_valid_vault_segment);
    let key_valid = key.is_none_or(is_valid_vault_key);
    if value.len() <= 512 && path_valid && key_valid && !has_extra_fragment {
        Ok(())
    } else {
        Err(invalid_config(
            CONFIG_TYPE,
            path,
            field,
            "vault reference path or key is invalid",
        ))
    }
}

fn is_valid_vault_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_vault_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn required_u32(path: &Path, field: &str, value: Option<u64>) -> Result<u32, EvaError> {
    value
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                field,
                "run_as numeric identity is required and must fit u32",
            )
        })
}

fn reject_unknown_fields(path: &Path, prefix: &str, extra: &Mapping) -> Result<(), EvaError> {
    if let Some((key, _)) = extra.iter().next() {
        let key = key.as_str().unwrap_or("<non-string>");
        return Err(invalid_config(
            CONFIG_TYPE,
            path,
            format!("{prefix}.{key}"),
            "provider config contains an unsupported field",
        ));
    }
    Ok(())
}

fn provider_transport_supports_process(transport: AdapterTransport, extra: &Mapping) -> bool {
    match transport {
        AdapterTransport::Stdio => true,
        AdapterTransport::Skill => {
            extra
                .get(Value::String("command".to_owned()))
                .and_then(Value::as_str)
                .is_some()
                || extra
                    .get(Value::String("skill".to_owned()))
                    .and_then(Value::as_mapping)
                    .and_then(|skill| skill.get(Value::String("runner".to_owned())))
                    .and_then(Value::as_mapping)
                    .and_then(|runner| runner.get(Value::String("command".to_owned())))
                    .and_then(Value::as_str)
                    .is_some()
        }
        AdapterTransport::Mcp => extra
            .get(Value::String("mcp".to_owned()))
            .and_then(Value::as_mapping)
            .and_then(|mcp| mcp.get(Value::String("server_transport".to_owned())))
            .and_then(Value::as_str)
            .is_none_or(|value| value == "stdio"),
        _ => false,
    }
}

fn validate_production_manifest_secrets(
    manifest: &AdapterManifest,
    mapping: &Mapping,
    prefix: &str,
) -> Result<(), EvaError> {
    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            return Err(invalid_config(
                CONFIG_TYPE,
                &manifest.path,
                prefix,
                "production manifest contains a non-string field name",
            ));
        };
        let field = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        if matches!(key, "input_schema" | "output_schema" | "schema") {
            continue;
        }
        if matches!(key, "args" | "endpoint") {
            validate_no_embedded_production_secret(manifest, value, &field)?;
        }
        if key.eq_ignore_ascii_case("headers") {
            validate_production_headers(manifest, value, &field)?;
            continue;
        }
        if is_secret_field_name(key) {
            let Some(reference) = value.as_str() else {
                return Err(production_plaintext_error(manifest, &field));
            };
            validate_env_credential_reference(manifest, &field, reference)?;
            continue;
        }
        match value {
            Value::Mapping(nested) => {
                validate_production_manifest_secrets(manifest, nested, &field)?;
            }
            Value::Sequence(values) => {
                for (index, value) in values.iter().enumerate() {
                    if let Value::Mapping(nested) = value {
                        validate_production_manifest_secrets(
                            manifest,
                            nested,
                            &format!("{field}[{index}]"),
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_production_headers(
    manifest: &AdapterManifest,
    value: &Value,
    field: &str,
) -> Result<(), EvaError> {
    let headers = value.as_mapping().ok_or_else(|| {
        invalid_config(
            CONFIG_TYPE,
            &manifest.path,
            field,
            "production headers must be an object",
        )
    })?;
    for (name, value) in headers {
        let Some(name) = name.as_str() else {
            return Err(invalid_config(
                CONFIG_TYPE,
                &manifest.path,
                field,
                "production header name must be a string",
            ));
        };
        if is_sensitive_header(name) {
            let header_field = format!("{field}.{name}");
            let Some(reference) = value.as_str() else {
                return Err(production_plaintext_error(manifest, &header_field));
            };
            validate_env_credential_reference(manifest, &header_field, reference)?;
        }
    }
    Ok(())
}

fn validate_env_credential_reference(
    manifest: &AdapterManifest,
    field: &str,
    value: &str,
) -> Result<(), EvaError> {
    if let Some(env) = value.strip_prefix("env:") {
        validate_env_name(&manifest.path, field, env)?;
        if !manifest
            .nested_extra_string_list("permissions", "env")
            .iter()
            .any(|allowed| allowed == env)
        {
            return Err(invalid_config(
                CONFIG_TYPE,
                &manifest.path,
                field,
                "production credential env reference is not allowed by permissions.env",
            ));
        }
        return Ok(());
    }
    Err(production_plaintext_error(manifest, field))
}

fn validate_no_embedded_production_secret(
    manifest: &AdapterManifest,
    value: &Value,
    field: &str,
) -> Result<(), EvaError> {
    let mut values = Vec::new();
    match value {
        Value::String(value) => values.push(value.as_str()),
        Value::Sequence(sequence) => {
            values.extend(sequence.iter().filter_map(Value::as_str));
        }
        _ => return Ok(()),
    }
    for value in values {
        let normalized = value.to_ascii_lowercase();
        let exact_flag = matches!(
            normalized.as_str(),
            "--token"
                | "--password"
                | "--secret"
                | "--api-key"
                | "--api_key"
                | "--access-token"
                | "--client-secret"
        );
        let assignment = [
            "--token=",
            "--password=",
            "--secret=",
            "--api-key=",
            "--api_key=",
            "--access-token=",
            "--client-secret=",
            "?token=",
            "&token=",
            "?password=",
            "&password=",
            "?secret=",
            "&secret=",
            "?api_key=",
            "&api_key=",
            "?api-key=",
            "&api-key=",
            "?apikey=",
            "&apikey=",
            "?access_token=",
            "&access_token=",
            "?auth_token=",
            "&auth_token=",
            "?client_secret=",
            "&client_secret=",
        ]
        .iter()
        .any(|marker| normalized.contains(marker));
        if exact_flag || assignment || contains_url_userinfo_secret(&normalized) {
            return Err(production_plaintext_error(manifest, field));
        }
    }
    Ok(())
}

fn contains_url_userinfo_secret(value: &str) -> bool {
    let Some((_, remainder)) = value.split_once("://") else {
        return false;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

fn is_secret_field_name(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "secret"
            | "token"
            | "password"
            | "api_key"
            | "apikey"
            | "access_token"
            | "accesstoken"
            | "auth_token"
            | "authtoken"
            | "client_secret"
            | "clientsecret"
            | "private_key"
            | "privatekey"
    )
}

fn is_sensitive_header(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase().replace('_', "-");
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
            | "x-access-token"
            | "cookie"
    ) || normalized.contains("api-key")
        || normalized.contains("access-token")
        || normalized.contains("auth-token")
        || normalized.contains("client-key")
        || normalized.contains("private-key")
        || normalized
            .split('-')
            .any(|part| matches!(part, "token" | "secret" | "password" | "credential"))
}

fn production_plaintext_error(manifest: &AdapterManifest, field: &str) -> EvaError {
    invalid_config(
        CONFIG_TYPE,
        &manifest.path,
        field,
        "production Adapter manifests must use allowlisted env: references or credentials.vault",
    )
}

impl HardwareAdapterConfig {
    /// 从扩展字段构建硬件配置，并为错误附加清单路径和精确字段。
    ///
    /// 未声明总线、协议、claim 或驱动时使用保守默认值；真实驱动类别只被解析和保留，
    /// 不会在配置加载阶段启动设备或授予句柄。超范围整数和空身份/驱动标识失败关闭。
    fn from_manifest(manifest: &AdapterManifest) -> Result<Self, EvaError> {
        let bus = parse_hardware_field(
            manifest,
            &["hardware", "bus"],
            "hardware.bus",
            HardwareBusKind::parse,
        )?
        .unwrap_or(HardwareBusKind::Usb);
        let identity = HardwareIdentityConfig {
            logical_name: manifest
                .deep_extra_string(&["hardware", "identity", "logical_name"])
                .unwrap_or_else(|| manifest.id.as_str())
                .to_owned(),
            device_class: manifest
                .deep_extra_string(&["hardware", "identity", "device_class"])
                .unwrap_or("hardware")
                .to_owned(),
        };
        if identity.logical_name.trim().is_empty() || identity.device_class.trim().is_empty() {
            return Err(invalid_config(
                CONFIG_TYPE,
                &manifest.path,
                "hardware.identity",
                "logical_name and device_class cannot be empty",
            ));
        }

        let protocol_kind = parse_hardware_field(
            manifest,
            &["hardware", "protocol", "kind"],
            "hardware.protocol.kind",
            HardwareProtocolKind::parse,
        )?
        .unwrap_or(HardwareProtocolKind::Simulated);
        let driver_kind = parse_hardware_field(
            manifest,
            &["hardware", "driver", "kind"],
            "hardware.driver.kind",
            HardwareDriverKind::parse,
        )?
        .unwrap_or(HardwareDriverKind::Simulated);
        let driver_id = manifest
            .deep_extra_string(&["hardware", "driver", "id"])
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}-{}-driver", manifest.id.as_str(), driver_kind.as_str()));
        if driver_id.trim().is_empty() {
            return Err(invalid_config(
                CONFIG_TYPE,
                &manifest.path,
                "hardware.driver.id",
                "driver id cannot be empty",
            ));
        }

        Ok(Self {
            bus,
            match_rule: HardwareMatchConfig {
                vendor_id: hardware_string(manifest, &["hardware", "match", "vendor_id"]),
                product_id: hardware_string(manifest, &["hardware", "match", "product_id"]),
                serial: hardware_string(manifest, &["hardware", "match", "serial"]),
                address: hardware_string(manifest, &["hardware", "match", "address"]),
                path: hardware_string(manifest, &["hardware", "match", "path"]),
                service_uuid: hardware_string(manifest, &["hardware", "match", "service_uuid"]),
            },
            identity,
            protocol: HardwareProtocolConfig {
                kind: protocol_kind,
                baud_rate: optional_u32(
                    manifest,
                    &["hardware", "protocol", "baud_rate"],
                    "hardware.protocol.baud_rate",
                )?,
                endpoint: hardware_string(manifest, &["hardware", "protocol", "endpoint"]),
                service_uuid: hardware_string(manifest, &["hardware", "protocol", "service_uuid"]),
            },
            hotplug: HardwareHotplugConfig {
                claim: parse_hardware_field(
                    manifest,
                    &["hardware", "hotplug", "claim"],
                    "hardware.hotplug.claim",
                    HardwareClaimMode::parse,
                )?
                .unwrap_or(HardwareClaimMode::Exclusive),
                reconnect_backoff_ms: manifest
                    .deep_extra_u64(&["hardware", "hotplug", "reconnect_backoff_ms"])
                    .unwrap_or(0),
                max_reconnect_attempts: optional_u32(
                    manifest,
                    &["hardware", "hotplug", "max_reconnect_attempts"],
                    "hardware.hotplug.max_reconnect_attempts",
                )?
                .unwrap_or(0),
            },
            driver: HardwareDriverConfig {
                driver_id,
                kind: driver_kind,
            },
        })
    }
}

impl HardwareBusKind {
    /// 解析受支持总线及 network 兼容别名。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "usb" => Ok(Self::Usb),
            "serial" => Ok(Self::Serial),
            "ble" => Ok(Self::Ble),
            "socket" | "network" => Ok(Self::Socket),
            "vendor_sdk" => Ok(Self::VendorSdk),
            _ => Err(EvaError::unsupported("unsupported hardware bus").with_context("bus", value)),
        }
    }

    /// 返回稳定清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Serial => "serial",
            Self::Ble => "ble",
            Self::Socket => "socket",
            Self::VendorSdk => "vendor_sdk",
        }
    }
}

impl HardwareProtocolKind {
    /// 解析受支持硬件协议类别。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "simulated" => Ok(Self::Simulated),
            "line_json" => Ok(Self::LineJson),
            "binary" => Ok(Self::Binary),
            "modbus_rtu" => Ok(Self::ModbusRtu),
            "ble_gatt" => Ok(Self::BleGatt),
            "tcp_socket" => Ok(Self::TcpSocket),
            "udp_socket" => Ok(Self::UdpSocket),
            "vendor_sdk" => Ok(Self::VendorSdk),
            _ => Err(EvaError::unsupported("unsupported hardware protocol")
                .with_context("protocol", value)),
        }
    }

    /// 返回稳定清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simulated => "simulated",
            Self::LineJson => "line_json",
            Self::Binary => "binary",
            Self::ModbusRtu => "modbus_rtu",
            Self::BleGatt => "ble_gatt",
            Self::TcpSocket => "tcp_socket",
            Self::UdpSocket => "udp_socket",
            Self::VendorSdk => "vendor_sdk",
        }
    }
}

impl HardwareClaimMode {
    /// 解析受支持硬件租用模式。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "exclusive" => Ok(Self::Exclusive),
            "shared_read" => Ok(Self::SharedRead),
            _ => Err(EvaError::unsupported("unsupported hardware claim mode")
                .with_context("claim", value)),
        }
    }

    /// 返回稳定清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::SharedRead => "shared_read",
        }
    }
}

impl HardwareDriverKind {
    /// 解析受支持驱动类别及 network 兼容别名。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "simulated" => Ok(Self::Simulated),
            "usb" => Ok(Self::Usb),
            "serial" => Ok(Self::Serial),
            "ble" => Ok(Self::Ble),
            "socket" | "network" => Ok(Self::Socket),
            "vendor_sdk" => Ok(Self::VendorSdk),
            _ => Err(EvaError::unsupported("unsupported hardware driver kind")
                .with_context("driver_kind", value)),
        }
    }

    /// 返回稳定清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simulated => "simulated",
            Self::Usb => "usb",
            Self::Serial => "serial",
            Self::Ble => "ble",
            Self::Socket => "socket",
            Self::VendorSdk => "vendor_sdk",
        }
    }
}

/// 从指定硬件扩展路径读取字符串并用提供的解析器规范化。
fn parse_hardware_field<T>(
    manifest: &AdapterManifest,
    path: &[&str],
    field: &'static str,
    parse: impl FnOnce(&str) -> Result<T, EvaError>,
) -> Result<Option<T>, EvaError> {
    manifest
        .deep_extra_string(path)
        .map(parse)
        .transpose()
        .map_err(|error| with_field_context(error, CONFIG_TYPE, &manifest.path, field))
}

/// 读取硬件扩展整数并拒绝超出 `u32` 的值。
fn optional_u32(
    manifest: &AdapterManifest,
    path: &[&str],
    field: &'static str,
) -> Result<Option<u32>, EvaError> {
    manifest
        .deep_extra_u64(path)
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                invalid_config(
                    CONFIG_TYPE,
                    &manifest.path,
                    field,
                    "integer value exceeds u32 range",
                )
            })
        })
        .transpose()
}

/// 读取硬件扩展字符串并复制为拥有值。
fn hardware_string(manifest: &AdapterManifest, path: &[&str]) -> Option<String> {
    manifest.deep_extra_string(path).map(str::to_owned)
}

/// 将 YAML 映射投影为确定有序的纯字符串映射。
fn string_map(mapping: &Mapping) -> BTreeMap<String, String> {
    mapping
        .iter()
        .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), value.as_str()?.to_owned())))
        .collect()
}

impl AdapterTransport {
    /// 从 YAML 稳定拼写解析传输，未知值失败关闭。
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

    /// 返回 Schema 和序列化使用的稳定清单拼写。
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

/// 仅负责 YAML 形状反序列化、尚未进行语义验证的 Adapter 清单。
#[derive(Debug, Deserialize)]
struct RawAdapterManifest {
    /// 原始 Adapter 标识。
    id: String,
    /// 原始显示名称。
    name: String,
    /// 原始清单版本。
    version: String,
    /// 原始启用标志。
    enabled: bool,
    /// 原始传输拼写。
    transport: String,
    /// 原始能力名列表。
    #[serde(default)]
    capabilities: Vec<String>,
    /// Provider supervision settings owned by W3.
    #[serde(default)]
    supervision: RawProviderSupervision,
    /// Provider credential references owned by W3.
    #[serde(default)]
    credentials: RawProviderCredentials,
    /// 未由核心模型占用的顶层扩展字段。
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Default, Deserialize)]
struct RawProviderSupervision {
    #[serde(default)]
    restart: Option<RawProviderRestart>,
    #[serde(default)]
    run_as: Option<RawProviderRunAs>,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Default, Deserialize)]
struct RawProviderRestart {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    max_attempts: Option<u64>,
    #[serde(default)]
    backoff_ms: Option<u64>,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Default, Deserialize)]
struct RawProviderRunAs {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    uid: Option<u64>,
    #[serde(default)]
    gid: Option<u64>,
    #[serde(default)]
    account: Option<String>,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Default, Deserialize)]
struct RawProviderCredentials {
    #[serde(default)]
    vault: Vec<RawProviderVaultSecretRef>,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Default, Deserialize)]
struct RawProviderVaultSecretRef {
    #[serde(default)]
    env: Option<String>,
    #[serde(default, rename = "ref")]
    secret_ref: Option<String>,
    #[serde(flatten)]
    extra: Mapping,
}

impl TryFrom<Value> for AdapterManifest {
    /// 值转换失败时使用的统一配置错误类型。
    type Error = EvaError;

    /// 从内存 YAML 值解析并验证清单。
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawAdapterManifest::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse Adapter manifest")
                .with_context("yaml_error", error.to_string())
        })?;
        Self::try_from_raw(PathBuf::new(), raw)
    }
}

#[cfg(test)]
/// Adapter 加载、扩展读取和硬件强类型解析测试。
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use serde_yaml::Value;

    /// 返回包含示例配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证示例 Adapter 清单可成功规范化。
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
    /// 验证未知传输不会被宽松接受。
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
    /// 验证非法能力名在加载阶段失败关闭。
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
    /// 验证嵌套扩展字符串列表可被下游读取。
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
    /// 验证任意深度扩展字符串读取路径。
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
    /// 验证硬件扩展可规范化为完整强类型配置。
    fn adapter_manifest_parses_hardware_typed_config() {
        let manifest = load_adapter_manifest(
            workspace_root().join("config/adapters/hardware/scale-main.yaml"),
        )
        .unwrap();

        let hardware = manifest.hardware_config().unwrap().unwrap();

        assert_eq!(hardware.bus, HardwareBusKind::Usb);
        assert_eq!(hardware.identity.logical_name, "main-scale");
        assert_eq!(hardware.identity.device_class, "scale");
        assert_eq!(hardware.protocol.kind, HardwareProtocolKind::LineJson);
        assert_eq!(hardware.protocol.baud_rate, Some(115200));
        assert_eq!(hardware.hotplug.claim, HardwareClaimMode::Exclusive);
        assert_eq!(hardware.hotplug.reconnect_backoff_ms, 1000);
        assert_eq!(hardware.hotplug.max_reconnect_attempts, 5);
        assert_eq!(hardware.driver.kind, HardwareDriverKind::Simulated);
    }

    #[test]
    /// 验证真实驱动类别仅被保留，不在配置阶段执行。
    fn hardware_config_reserves_real_driver_kinds() {
        assert_eq!(HardwareBusKind::parse("serial").unwrap().as_str(), "serial");
        assert_eq!(HardwareBusKind::parse("ble").unwrap().as_str(), "ble");
        assert_eq!(HardwareBusKind::parse("socket").unwrap().as_str(), "socket");
        assert_eq!(
            HardwareBusKind::parse("vendor_sdk").unwrap().as_str(),
            "vendor_sdk"
        );
        assert_eq!(
            HardwareProtocolKind::parse("tcp_socket").unwrap().as_str(),
            "tcp_socket"
        );
        assert_eq!(
            HardwareDriverKind::parse("vendor_sdk").unwrap().as_str(),
            "vendor_sdk"
        );
    }

    #[test]
    /// 验证运行时扩展数值和字符串映射保持可读。
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
    /// 验证 Skill 使用的对象 Schema 子集可被提取。
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

    fn parse_test_manifest(yaml: &str) -> Result<AdapterManifest, EvaError> {
        AdapterManifest::try_from(serde_yaml::from_str::<Value>(yaml).unwrap())
    }

    fn provider_manifest_yaml(transport: &str, provider: &str) -> String {
        format!(
            r#"id: provider-test
name: Provider Test
version: 1.0.0
enabled: true
transport: {transport}
capabilities:
  - repo.analyze
permissions:
  env:
    - API_TOKEN
    - SECOND_TOKEN
limits: {{}}
routing: {{}}
{provider}
"#
        )
    }

    #[test]
    fn legacy_manifest_defaults_provider_config() {
        let manifest = parse_test_manifest(
            r#"
id: legacy-provider
name: Legacy Provider
version: 1.0.0
enabled: true
transport: stdio
capabilities:
  - repo.analyze
permissions: {}
limits: {}
routing: {}
"#,
        )
        .unwrap();

        assert_eq!(manifest.provider, ProviderConfig::default());
    }

    #[test]
    fn adapter_schema_provider_enums_match_canonical_parser() {
        let schema_path = workspace_root().join("config/schemas/adapter.schema.json");
        let schema: Value =
            serde_yaml::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();
        let schema_value = |path: &[&str]| {
            path.iter().fold(&schema, |value, key| {
                value
                    .as_mapping()
                    .and_then(|mapping| mapping.get(Value::String((*key).to_owned())))
                    .unwrap()
            })
        };
        let restart_modes = schema_value(&[
            "properties",
            "supervision",
            "properties",
            "restart",
            "properties",
            "mode",
            "enum",
        ])
        .as_sequence()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
        assert_eq!(restart_modes, vec!["none", "on_failure", "always"]);
        for mode in restart_modes {
            assert_eq!(ProviderRestartMode::parse(mode).unwrap().as_str(), mode);
        }

        let run_as_kinds = schema_value(&[
            "properties",
            "supervision",
            "properties",
            "run_as",
            "properties",
            "kind",
            "enum",
        ])
        .as_sequence()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
        assert_eq!(run_as_kinds, vec!["current", "unix", "windows"]);

        let mcp_transports = schema_value(&[
            "properties",
            "mcp",
            "properties",
            "server_transport",
            "enum",
        ])
        .as_sequence()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
        assert_eq!(mcp_transports, vec!["stdio", "http", "streamable_http"]);
    }

    #[test]
    fn provider_restart_modes_and_identities_parse() {
        let none = parse_test_manifest(&provider_manifest_yaml(
            "stdio",
            "supervision:\n  restart:\n    mode: none\n    max_attempts: 0\n    backoff_ms: 0\n  run_as:\n    kind: current\n",
        ))
        .unwrap();
        assert_eq!(none.provider.restart.mode, ProviderRestartMode::None);
        assert_eq!(none.provider.run_as, ProviderRunAsIdentity::Current);

        let on_failure = parse_test_manifest(&provider_manifest_yaml(
            "stdio",
            "supervision:\n  restart:\n    mode: on_failure\n    max_attempts: 3\n    backoff_ms: 1000\n  run_as:\n    kind: unix\n    uid: 1000\n    gid: 1001\n",
        ))
        .unwrap();
        assert_eq!(
            on_failure.provider.restart,
            ProviderRestartConfig {
                mode: ProviderRestartMode::OnFailure,
                max_attempts: 3,
                backoff_ms: 1000,
            }
        );
        assert_eq!(
            on_failure.provider.run_as,
            ProviderRunAsIdentity::Unix {
                uid: 1000,
                gid: 1001
            }
        );

        let always = parse_test_manifest(&provider_manifest_yaml(
            "stdio",
            "supervision:\n  restart:\n    mode: always\n    max_attempts: 2\n    backoff_ms: 250\n  run_as:\n    kind: windows\n    account: LocalService\n",
        ))
        .unwrap();
        assert_eq!(always.provider.restart.mode, ProviderRestartMode::Always);
        assert_eq!(
            always.provider.run_as,
            ProviderRunAsIdentity::Windows {
                account: "LocalService".to_owned()
            }
        );
    }

    #[test]
    fn provider_restart_and_identity_invalid_combinations_fail_closed() {
        let invalid = [
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  restart:\n    mode: none\n    max_attempts: 1\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  restart:\n    mode: on_failure\n    max_attempts: 1\n",
            ),
            provider_manifest_yaml(
                "http",
                "supervision:\n  restart:\n    mode: always\n    max_attempts: 1\n    backoff_ms: 1\n",
            ),
            provider_manifest_yaml(
                "http",
                "supervision:\n  run_as:\n    kind: unix\n    uid: 1\n    gid: 1\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: current\n    uid: 1\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: unix\n    account: LocalService\n    uid: 1\n    gid: 1\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: windows\n    uid: 1\n    gid: 1\n    account: LocalService\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: unix\n    uid: 1\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: windows\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: windows\n    account: ' LocalService'\n",
            ),
            provider_manifest_yaml(
                "stdio",
                "supervision:\n  run_as:\n    kind: unix\n    uid: 4294967296\n    gid: 1\n",
            ),
        ];

        for yaml in invalid {
            let error = parse_test_manifest(&yaml).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidArgument, "{yaml}");
        }

        let unknown = parse_test_manifest(&provider_manifest_yaml(
            "stdio",
            "supervision:\n  run_as:\n    kind: container\n",
        ))
        .unwrap_err();
        assert_eq!(unknown.kind(), ErrorKind::Unsupported);

        let duplicate_env =
            provider_manifest_yaml("stdio", "").replace("    - SECOND_TOKEN", "    - API_TOKEN");
        assert_eq!(
            parse_test_manifest(&duplicate_env).unwrap_err().kind(),
            ErrorKind::InvalidArgument
        );

        let reserved_env = provider_manifest_yaml("stdio", "")
            .replace("    - API_TOKEN", "    - EVA_PROVIDER_SESSION_ID");
        assert_eq!(
            parse_test_manifest(&reserved_env).unwrap_err().kind(),
            ErrorKind::InvalidArgument
        );

        for dangerous in [
            "PATH",
            "pathext",
            "COMSPEC",
            "LD_PRELOAD",
            "dyld_insert_libraries",
            "PYTHONPATH",
            "NODE_OPTIONS",
            "BASH_ENV",
            "ENV",
            "IFS",
            "EVA_PROVIDER_SESSION_CUSTOM",
        ] {
            let yaml = provider_manifest_yaml("stdio", "")
                .replace("    - API_TOKEN", &format!("    - {dangerous}"));
            assert_eq!(
                parse_test_manifest(&yaml).unwrap_err().kind(),
                ErrorKind::InvalidArgument,
                "dangerous credential env should be rejected: {dangerous}"
            );
        }
    }

    #[test]
    fn mcp_http_rejects_process_supervision() {
        let stdio = parse_test_manifest(&provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: stdio\n  command: provider\nsupervision:\n  restart:\n    mode: on_failure\n    max_attempts: 1\n    backoff_ms: 1\n  run_as:\n    kind: unix\n    uid: 1\n    gid: 1\n",
        ))
        .unwrap();
        assert_eq!(stdio.provider.restart.mode, ProviderRestartMode::OnFailure);

        let yaml = provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: http\n  endpoint: https://example.test/mcp\nsupervision:\n  restart:\n    mode: on_failure\n    max_attempts: 1\n    backoff_ms: 1\n",
        );
        let error = parse_test_manifest(&yaml).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);

        let yaml = provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: http\n  endpoint: https://example.test/mcp\nsupervision:\n  run_as:\n    kind: unix\n    uid: 1\n    gid: 1\n",
        );
        let error = parse_test_manifest(&yaml).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn mcp_streamable_http_manifest_projects_canonical_policy() {
        let manifest = parse_test_manifest(&provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: streamable_http\n  endpoint: HTTPS://Example.TEST:443/mcp\n  http:\n    trust_roots: [system, 'file:certs/root.pem', 'pem:sha256:root-ca']\n    client_auth:\n      cert_ref: env:API_TOKEN\n      key_ref: env:SECOND_TOKEN\n    redirect:\n      mode: same_origin\n      max_hops: 3\n    allowed_origins: [https://example.test]\n",
        ))
        .unwrap();

        let config = manifest.mcp_http_manifest_config().unwrap().unwrap();
        assert_eq!(config.endpoint, "HTTPS://Example.TEST:443/mcp");
        assert_eq!(config.redirect_mode, "same_origin");
        assert_eq!(config.redirect_max_hops, Some(3));
        assert_eq!(config.allowed_origins, vec!["https://example.test"]);
        let debug = format!("{config:?}");
        assert!(!debug.contains("API_TOKEN"));
        assert!(!debug.contains("SECOND_TOKEN"));
        assert!(!debug.contains("Example.TEST"));
        assert!(manifest.validate_for_environment("production").is_ok());
    }

    #[test]
    fn mcp_http_manifest_rejects_ambiguous_or_malformed_policy() {
        let invalid = [
            "endpoint: http://fallback.test/mcp\nmcp:\n  server_transport: streamable_http\n  endpoint: 42\n",
            "endpoint: 42\nmcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: ftp://example.test/mcp\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://user@example.test/mcp\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test:0/mcp\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp#fragment\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    unknown: true\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    client_auth: plaintext\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    client_auth:\n      certificate_ref: env:API_TOKEN\n      key_ref: env:SECOND_TOKEN\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    client_auth:\n      cert_ref: 42\n      key_ref: env:SECOND_TOKEN\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    redirect:\n      max_hops: 0\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    redirect:\n      mode: 42\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    redirect:\n      mode: same_origin\n      max_hops: 0\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    trust_roots: ['file:/outside.pem']\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    trust_roots: ['pem:-----BEGIN CERTIFICATE-----']\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    trust_roots: [system]\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    client_auth:\n      cert_ref: env:API_TOKEN\n      key_ref: env:SECOND_TOKEN\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  http:\n    allowed_origins: ['*']\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  headers:\n    Authorization: 42\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  headers:\n    X-Token: \"bad\\0value\"\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  headers:\n    X-Token: \"bad\\x7fvalue\"\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  headers:\n    Host: example.test\n",
            "headers:\n  Authorization: env:API_TOKEN\nmcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  headers:\n    authorization: env:SECOND_TOKEN\n",
            "mcp:\n  server_transport: stdio\n  command: provider\n  http:\n    trust_roots: [system]\n",
            "mcp:\n  server_transport: stdio\n  command: provider\n  endpoint: http://example.test/mcp\n",
            "mcp:\n  server_transport: stdio\n  command: provider\n  headers:\n    X-Token: ignored\n",
            "mcp:\n  server_transport: streamable_http\n  command: ignored\n  endpoint: http://example.test/mcp\n",
            "mcp:\n  server_transport: streamable_http\n  args: [ignored]\n  endpoint: http://example.test/mcp\n",
            "mcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n  typo: ignored\n",
            "mcp:\n  server_transport: stdio\n  command: 42\n",
            "mcp:\n  server_transport: stdio\n  command: provider\n  args: [valid, 42]\n",
            "mcp:\n  server_transport: stdio\n  command: provider\n  args: [\"bad\\0arg\"]\n",
            "mcp:\n  server_transport: stdio\n  command: provider\n  tool_allowlist: [valid, 42]\n",
            "endpoint: http://example.test/mcp\nmcp:\n  server_transport: stdio\n  command: provider\n",
            "headers:\n  X-Token: ignored\nmcp:\n  server_transport: stdio\n  command: provider\n",
            "command: ignored\nmcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n",
            "args: [ignored]\nmcp:\n  server_transport: streamable_http\n  endpoint: http://example.test/mcp\n",
        ];
        for policy in invalid {
            let error = parse_test_manifest(&provider_manifest_yaml("mcp", policy)).unwrap_err();
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::InvalidArgument | ErrorKind::Unsupported
                ),
                "{policy}"
            );
        }

        let conflict = parse_test_manifest(&provider_manifest_yaml(
            "mcp",
            "endpoint: http://top.test/mcp\nmcp:\n  server_transport: streamable_http\n  endpoint: http://nested.test/mcp\n",
        ));
        assert!(conflict.is_err());
    }

    #[test]
    fn mcp_stdio_manifest_preserves_native_argv_boundaries() {
        let manifest = parse_test_manifest(&provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: stdio\n  command: provider\n  args: ['', ' value ']\n",
        ))
        .unwrap();

        assert_eq!(
            manifest.nested_extra_string_list("mcp", "args"),
            vec!["".to_owned(), " value ".to_owned()]
        );
    }

    #[test]
    fn production_mcp_https_requires_explicit_trust_and_origin_policy() {
        let missing = parse_test_manifest(&provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: streamable_http\n  endpoint: https://example.test/mcp\n",
        ))
        .unwrap();
        assert!(missing.validate_for_environment("production").is_err());

        let trust_only = parse_test_manifest(&provider_manifest_yaml(
            "mcp",
            "mcp:\n  server_transport: streamable_http\n  endpoint: https://example.test/mcp\n  http:\n    trust_roots: [system]\n",
        ))
        .unwrap();
        assert!(trust_only.validate_for_environment("production").is_err());
    }

    #[test]
    fn builtin_skill_rejects_process_only_supervision() {
        let builtin = provider_manifest_yaml(
            "skill",
            "skill:\n  id: code-review\n  entry:\n    type: codex_skill\nsupervision:\n  run_as:\n    kind: unix\n    uid: 1000\n    gid: 1000\n",
        );
        let error = parse_test_manifest(&builtin).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "field" && value == "supervision.run_as.kind"));

        let builtin_restart = provider_manifest_yaml(
            "skill",
            "skill:\n  id: code-review\n  entry:\n    type: codex_skill\nsupervision:\n  restart:\n    mode: on_failure\n    max_attempts: 1\n    backoff_ms: 1\n",
        );
        assert_eq!(
            parse_test_manifest(&builtin_restart).unwrap_err().kind(),
            ErrorKind::InvalidArgument
        );

        let runner = parse_test_manifest(&provider_manifest_yaml(
            "skill",
            "skill:\n  id: code-review\n  entry:\n    type: command\n  runner:\n    command: provider\n    args:\n      - --stdio\nsupervision:\n  restart:\n    mode: on_failure\n    max_attempts: 1\n    backoff_ms: 1\n  run_as:\n    kind: unix\n    uid: 1000\n    gid: 1000\n",
        ))
        .unwrap();
        assert_eq!(runner.provider.restart.mode, ProviderRestartMode::OnFailure);
        assert_eq!(
            runner.provider.run_as,
            ProviderRunAsIdentity::Unix {
                uid: 1000,
                gid: 1000
            }
        );
    }

    #[test]
    fn vault_refs_are_allowlisted_sorted_and_strict() {
        let manifest = parse_test_manifest(&provider_manifest_yaml(
            "stdio",
            "credentials:\n  vault:\n    - env: SECOND_TOKEN\n      ref: vault://providers/z/token#value\n    - env: API_TOKEN\n      ref: vault://providers/a/token\n",
        ))
        .unwrap();
        assert_eq!(
            manifest.provider.vault_secrets,
            vec![
                ProviderVaultSecretRef {
                    env: "API_TOKEN".to_owned(),
                    secret_ref: "vault://providers/a/token".to_owned(),
                },
                ProviderVaultSecretRef {
                    env: "SECOND_TOKEN".to_owned(),
                    secret_ref: "vault://providers/z/token#value".to_owned(),
                },
            ]
        );

        let invalid = [
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: vault://providers/a/token\n    - env: API_TOKEN\n      ref: vault://providers/b/token\n",
            "credentials:\n  vault:\n    - env: UNKNOWN_TOKEN\n      ref: vault://providers/a/token\n",
            "credentials:\n  vault:\n    - env: EVA_PROVIDER_SESSION_TOKEN\n      ref: vault://providers/a/token\n",
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: https://providers/a/token\n",
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: vault://providers/../token\n",
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: vault://providers/a/token#one#two\n",
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: vault://\n",
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: vault://providers//token\n",
            "credentials:\n  vault:\n    - env: API_TOKEN\n      ref: vault://providers/a/token#\n",
        ];
        for provider in invalid {
            let error =
                parse_test_manifest(&provider_manifest_yaml("stdio", provider)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidArgument, "{provider}");
        }
    }

    #[test]
    fn production_secret_validation_rejects_literals_but_dev_remains_compatible() {
        let literal_header = parse_test_manifest(
            r#"
id: production-header
name: Production Header
version: 1.0.0
enabled: true
transport: http
endpoint: https://example.test/api
headers:
  Authorization: Bearer plaintext-token
permissions:
  env:
    - API_TOKEN
capabilities:
  - chat.reply
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(literal_header
            .validate_for_environment("production")
            .is_err());
        assert!(literal_header.validate_for_environment("dev").is_ok());

        for header in ["X-Custom-Secret", "X-Client-Key", "X-Access-Credential"] {
            let yaml = format!(
                "id: production-custom-header\nname: Production Custom Header\nversion: 1.0.0\nenabled: true\ntransport: http\nendpoint: https://example.test/api\nheaders:\n  {header}: plaintext-token\npermissions:\n  env: [API_TOKEN]\ncapabilities: [chat.reply]\nlimits: {{}}\nrouting: {{}}\n"
            );
            let manifest = parse_test_manifest(&yaml).unwrap();
            assert!(
                manifest.validate_for_environment("production").is_err(),
                "{header}"
            );
        }

        let allowlisted_env = parse_test_manifest(
            r#"
id: production-allowed-env
name: Production Allowed Env
version: 1.0.0
enabled: true
transport: http
endpoint: https://example.test/api
headers:
  Authorization: env:API_TOKEN
permissions:
  env: [API_TOKEN]
capabilities:
  - chat.reply
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(allowlisted_env
            .validate_for_environment("production")
            .is_ok());

        let unallowlisted_env = parse_test_manifest(
            r#"
id: production-env
name: Production Env
version: 1.0.0
enabled: true
transport: http
endpoint: https://example.test/api
headers:
  Authorization: env:NOT_ALLOWLISTED
permissions:
  env: [API_TOKEN]
capabilities:
  - chat.reply
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(unallowlisted_env.validate_for_environment("prod").is_err());

        let direct_vault_header = parse_test_manifest(
            r#"
id: production-vault-header
name: Production Vault Header
version: 1.0.0
enabled: true
transport: http
endpoint: https://example.test/api
headers:
  Authorization: vault://providers/api/token
permissions:
  env: [API_TOKEN]
capabilities:
  - chat.reply
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(direct_vault_header
            .validate_for_environment("production")
            .is_err());

        let embedded_secret = parse_test_manifest(
            r#"
id: production-args
name: Production Args
version: 1.0.0
enabled: true
transport: stdio
command: provider
args: ["--token=plaintext-token"]
permissions: {}
capabilities:
  - repo.analyze
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(embedded_secret
            .validate_for_environment("production")
            .is_err());

        let endpoint_secret = parse_test_manifest(
            r#"
id: production-endpoint
name: Production Endpoint
version: 1.0.0
enabled: true
transport: http
endpoint: https://example.test/api?password=plaintext-password
permissions: {}
capabilities:
  - chat.reply
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(endpoint_secret
            .validate_for_environment("production")
            .is_err());

        let userinfo_secret = parse_test_manifest(
            r#"
id: production-userinfo
name: Production Userinfo
version: 1.0.0
enabled: true
transport: http
endpoint: https://user:plaintext-password@example.test/api
permissions: {}
capabilities:
  - chat.reply
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(userinfo_secret
            .validate_for_environment("production")
            .is_err());

        let schema_password = parse_test_manifest(
            r#"
id: production-schema
name: Production Schema
version: 1.0.0
enabled: true
transport: skill
skill:
  input_schema:
    type: object
    properties:
      password:
        type: string
permissions: {}
capabilities:
  - code.review
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(schema_password
            .validate_for_environment("production")
            .is_ok());
    }
}
