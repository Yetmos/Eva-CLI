//! Device discovery and trusted identity matching.

use crate::state::{DeviceBus, DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::{AdapterId, EvaError};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "device discovery and trusted identity matching";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCandidate {
    pub identity: DeviceIdentity,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub serial: Option<String>,
    pub protocol: Option<String>,
    pub health: DeviceHealth,
    pub source_path: String,
    pub handle_granted: bool,
    pub rejected_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareDiscoveryReport {
    pub candidates: Vec<DeviceCandidate>,
}

pub fn discover_project_devices(
    project: &ProjectConfig,
) -> Result<HardwareDiscoveryReport, EvaError> {
    let mut candidates = Vec::new();
    for adapter in &project.adapters {
        if adapter.transport != AdapterTransport::Hardware {
            continue;
        }
        let bus = adapter
            .deep_extra_string(&["hardware", "bus"])
            .map(DeviceBus::parse)
            .transpose()?
            .unwrap_or(DeviceBus::Usb);
        let logical_name = adapter
            .deep_extra_string(&["hardware", "identity", "logical_name"])
            .unwrap_or_else(|| adapter.id.as_str());
        let device_class = adapter
            .deep_extra_string(&["hardware", "identity", "device_class"])
            .unwrap_or("hardware");
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
                logical_name,
                device_class,
                bus,
                adapter.id.clone(),
                trust,
            )?,
            vendor_id: adapter
                .deep_extra_string(&["hardware", "match", "vendor_id"])
                .map(str::to_owned),
            product_id: adapter
                .deep_extra_string(&["hardware", "match", "product_id"])
                .map(str::to_owned),
            serial: adapter
                .deep_extra_string(&["hardware", "match", "serial"])
                .map(str::to_owned),
            protocol: adapter
                .deep_extra_string(&["hardware", "protocol", "kind"])
                .map(str::to_owned),
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

impl DeviceCandidate {
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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

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
