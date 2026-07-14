//! Adapter 清单的加载、扩展字段读取与硬件配置规范化。
//! Adapter manifest loading and normalization.

use crate::{invalid_config, read_yaml_file, require_non_empty, with_field_context, EvaError};
use eva_core::{AdapterId, CapabilityName};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
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
    /// 由 Adapter、运行时或策略 crate 解释的扩展字段。
    /// Additional fields owned by adapter/runtime/policy crates.
    pub extra: Mapping,
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

    /// 仅对 Hardware 传输解析 `hardware` 扩展区的强类型配置。
    /// Parses the typed hardware adapter config preserved under `hardware`.
    pub fn hardware_config(&self) -> Result<Option<HardwareAdapterConfig>, EvaError> {
        if self.transport != AdapterTransport::Hardware {
            return Ok(None);
        }
        HardwareAdapterConfig::from_manifest(self).map(Some)
    }
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
    /// 未由核心模型占用的顶层扩展字段。
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
}
