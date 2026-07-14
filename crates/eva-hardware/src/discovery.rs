//! 本模块提供 `discovery` 相关实现。
//! Device discovery and trusted identity matching.

use crate::state::{DeviceBus, DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
use eva_config::{AdapterTransport, HardwareBusKind, ProjectConfig};
use eva_core::{AdapterId, EvaError};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "device discovery and trusted identity matching";

/// 表示 `DeviceCandidate` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCandidate {
    /// 记录 `identity` 字段对应的值。
    pub identity: DeviceIdentity,
    /// 记录 `vendor_id` 字段对应的值。
    pub vendor_id: Option<String>,
    /// 记录 `product_id` 字段对应的值。
    pub product_id: Option<String>,
    /// 记录 `serial` 字段对应的值。
    pub serial: Option<String>,
    /// 记录 `protocol` 字段对应的值。
    pub protocol: Option<String>,
    /// 记录 `health` 字段对应的值。
    pub health: DeviceHealth,
    /// 记录 `source_path` 字段对应的值。
    pub source_path: String,
    /// 记录 `handle_granted` 字段对应的值。
    pub handle_granted: bool,
    /// 记录 `rejected_reason` 字段对应的值。
    pub rejected_reason: Option<String>,
}

/// 表示 `HardwareDiscoveryReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareDiscoveryReport {
    /// 记录 `candidates` 字段对应的值。
    pub candidates: Vec<DeviceCandidate>,
}

/// 执行 `discover_project_devices` 对应的处理逻辑。
pub fn discover_project_devices(
    project: &ProjectConfig,
) -> Result<HardwareDiscoveryReport, EvaError> {
    let mut candidates = Vec::new();
    for adapter in &project.adapters {
        if adapter.transport != AdapterTransport::Hardware {
            continue;
        }
        let hardware = adapter.hardware_config()?.ok_or_else(|| {
            EvaError::invalid_argument("hardware adapter missing hardware config")
                .with_context("adapter_id", adapter.id.as_str())
        })?;
        let bus = device_bus_from_config(hardware.bus);
        let logical_name = hardware.identity.logical_name;
        let device_class = hardware.identity.device_class;
        let id = DeviceId::parse(&format!("{}:{}", adapter.id.as_str(), logical_name))?;
        let trust = if adapter.enabled {
            DeviceTrust::Manifest
        } else {
            DeviceTrust::Rejected
        };
        let health = if adapter.enabled {
            DeviceHealth::Available
        } else {
            DeviceHealth::Disconnected
        };
        candidates.push(DeviceCandidate {
            identity: DeviceIdentity::new(
                id,
                &logical_name,
                &device_class,
                bus,
                adapter.id.clone(),
                trust,
            )?,
            vendor_id: hardware.match_rule.vendor_id,
            product_id: hardware.match_rule.product_id,
            serial: hardware.match_rule.serial,
            protocol: Some(hardware.protocol.kind.as_str().to_owned()),
            health,
            source_path: adapter.path.display().to_string(),
            handle_granted: false,
            rejected_reason: (!adapter.enabled)
                .then(|| "hardware adapter manifest is disabled".to_owned()),
        });
    }
    candidates.sort_by(|left, right| left.identity.id.cmp(&right.identity.id));
    Ok(HardwareDiscoveryReport { candidates })
}

/// 执行 `device_bus_from_config` 对应的处理逻辑。
fn device_bus_from_config(bus: HardwareBusKind) -> DeviceBus {
    match bus {
        HardwareBusKind::Usb => DeviceBus::Usb,
        HardwareBusKind::Serial => DeviceBus::Serial,
        HardwareBusKind::Ble => DeviceBus::Ble,
        HardwareBusKind::Socket => DeviceBus::Socket,
        HardwareBusKind::VendorSdk => DeviceBus::VendorSdk,
    }
}

impl DeviceCandidate {
    /// 执行 `for_adapter` 对应的处理逻辑。
    pub fn for_adapter(adapter_id: AdapterId, logical_name: &str) -> Result<Self, EvaError> {
        Ok(Self {
            identity: DeviceIdentity::new(
                DeviceId::parse(&format!("{}:{}", adapter_id.as_str(), logical_name))?,
                logical_name,
                "simulated",
                DeviceBus::Usb,
                adapter_id,
                DeviceTrust::Manifest,
            )?,
            vendor_id: None,
            product_id: None,
            serial: None,
            protocol: Some("simulated".to_owned()),
            health: DeviceHealth::Available,
            source_path: "test".to_owned(),
            handle_granted: false,
            rejected_reason: None,
        })
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 验证 `project_discovery_returns_non_authorizing_candidates` 场景下的预期行为。
    #[test]
    fn project_discovery_returns_non_authorizing_candidates() {
        let project = load_project_config(workspace_root()).unwrap();
        let report = discover_project_devices(&project).unwrap();

        assert!(report
            .candidates
            .iter()
            .any(|candidate| candidate.identity.adapter_id.as_str() == "scale-main"));
        assert!(report
            .candidates
            .iter()
            .all(|candidate| !candidate.handle_granted));
    }
}
