//! Adapter manifest loading and normalization.

use crate::{invalid_config, read_yaml_file, require_non_empty, with_field_context, EvaError};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareAdapterConfig {
    pub bus: HardwareBusKind,
    pub match_rule: HardwareMatchConfig,
    pub identity: HardwareIdentityConfig,
    pub protocol: HardwareProtocolConfig,
    pub hotplug: HardwareHotplugConfig,
    pub driver: HardwareDriverConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareBusKind {
    Usb,
    Serial,
    Ble,
    Socket,
    VendorSdk,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareMatchConfig {
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub serial: Option<String>,
    pub address: Option<String>,
    pub path: Option<String>,
    pub service_uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareIdentityConfig {
    pub logical_name: String,
    pub device_class: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProtocolConfig {
    pub kind: HardwareProtocolKind,
    pub baud_rate: Option<u32>,
    pub endpoint: Option<String>,
    pub service_uuid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareProtocolKind {
    Simulated,
    LineJson,
    Binary,
    ModbusRtu,
    BleGatt,
    TcpSocket,
    UdpSocket,
    VendorSdk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareHotplugConfig {
    pub claim: HardwareClaimMode,
    pub reconnect_backoff_ms: u64,
    pub max_reconnect_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareClaimMode {
    Exclusive,
    SharedRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareDriverConfig {
    pub driver_id: String,
    pub kind: HardwareDriverKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareDriverKind {
    Simulated,
    Usb,
    Serial,
    Ble,
    Socket,
    VendorSdk,
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

    /// Returns an unsigned integer from a nested manifest extension path.
    pub fn deep_extra_u64(&self, path: &[&str]) -> Option<u64> {
        self.deep_extra_value(path).and_then(Value::as_u64)
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

    /// Parses the typed hardware adapter config preserved under `hardware`.
    pub fn hardware_config(&self) -> Result<Option<HardwareAdapterConfig>, EvaError> {
        if self.transport != AdapterTransport::Hardware {
            return Ok(None);
        }
        HardwareAdapterConfig::from_manifest(self).map(Some)
    }
}

impl HardwareAdapterConfig {
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
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "exclusive" => Ok(Self::Exclusive),
            "shared_read" => Ok(Self::SharedRead),
            _ => Err(EvaError::unsupported("unsupported hardware claim mode")
                .with_context("claim", value)),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::SharedRead => "shared_read",
        }
    }
}

impl HardwareDriverKind {
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

fn hardware_string(manifest: &AdapterManifest, path: &[&str]) -> Option<String> {
    manifest.deep_extra_string(path).map(str::to_owned)
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
