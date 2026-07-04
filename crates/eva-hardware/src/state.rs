//! Hardware runtime state ownership.

use eva_core::{AdapterId, EvaError};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "hardware runtime state ownership";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceBus {
    Usb,
    Serial,
    Ble,
    Network,
    VendorSdk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceTrust {
    Manifest,
    Observed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceHealth {
    Candidate,
    Available,
    Claimed,
    Disconnected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub id: DeviceId,
    pub logical_name: String,
    pub device_class: String,
    pub bus: DeviceBus,
    pub adapter_id: AdapterId,
    pub trust: DeviceTrust,
}

impl DeviceBus {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "usb" => Ok(Self::Usb),
            "serial" => Ok(Self::Serial),
            "ble" => Ok(Self::Ble),
            "network" => Ok(Self::Network),
            "vendor_sdk" => Ok(Self::VendorSdk),
            _ => Err(EvaError::unsupported("unsupported hardware bus").with_context("bus", value)),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Serial => "serial",
            Self::Ble => "ble",
            Self::Network => "network",
            Self::VendorSdk => "vendor_sdk",
        }
    }
}

impl DeviceTrust {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Observed => "observed",
            Self::Rejected => "rejected",
        }
    }
}

impl DeviceHealth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Available => "available",
            Self::Claimed => "claimed",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
        }
    }
}

impl DeviceId {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        if value.trim().is_empty() {
            return Err(EvaError::invalid_argument("device id cannot be empty"));
        }
        if value.trim() != value || value.contains('/') || value.contains('\\') {
            return Err(EvaError::invalid_argument(
                "device id must be a stable slug",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl DeviceIdentity {
    pub fn new(
        id: DeviceId,
        logical_name: impl Into<String>,
        device_class: impl Into<String>,
        bus: DeviceBus,
        adapter_id: AdapterId,
        trust: DeviceTrust,
    ) -> Result<Self, EvaError> {
        let logical_name = logical_name.into();
        let device_class = device_class.into();
        if logical_name.trim().is_empty() || device_class.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "device logical name and class are required",
            ));
        }
        Ok(Self {
            id,
            logical_name,
            device_class,
            bus,
            adapter_id,
            trust,
        })
    }
}
