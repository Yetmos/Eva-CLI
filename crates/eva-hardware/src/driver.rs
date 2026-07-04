//! Driver binding behind policy-controlled interfaces.

use crate::registry::DeviceLease;
use eva_core::{CapabilityName, EvaError, RequestId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "driver binding behind policy-controlled interfaces";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverBinding {
    pub driver_id: String,
    pub capability: CapabilityName,
    pub device_class: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverOperation {
    pub request_id: RequestId,
    pub capability: CapabilityName,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverOutput {
    pub request_id: RequestId,
    pub capability: CapabilityName,
    pub status: String,
    pub output: String,
    pub audit: Vec<String>,
}

pub trait HardwareDriver {
    fn binding(&self) -> &DriverBinding;
    fn invoke(
        &self,
        lease: &DeviceLease,
        operation: DriverOperation,
    ) -> Result<DriverOutput, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedDriver {
    binding: DriverBinding,
}

impl DriverBinding {
    pub fn new(
        driver_id: impl Into<String>,
        capability: CapabilityName,
        device_class: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let driver_id = driver_id.into();
        let device_class = device_class.into();
        if driver_id.trim().is_empty() || device_class.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "driver id and device class are required",
            ));
        }
        Ok(Self {
            driver_id,
            capability,
            device_class,
            read_only: true,
        })
    }
}

impl DriverOperation {
    pub fn new(request_id: RequestId, capability: CapabilityName) -> Self {
        Self {
            request_id,
            capability,
            input: String::new(),
        }
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }
}

impl SimulatedDriver {
    pub fn new(binding: DriverBinding) -> Self {
        Self { binding }
    }
}

impl HardwareDriver for SimulatedDriver {
    fn binding(&self) -> &DriverBinding {
        &self.binding
    }

    fn invoke(
        &self,
        lease: &DeviceLease,
        operation: DriverOperation,
    ) -> Result<DriverOutput, EvaError> {
        if operation.capability != self.binding.capability {
            return Err(
                EvaError::permission_denied("driver binding does not expose capability")
                    .with_context("capability", operation.capability.as_str())
                    .with_context("driver", &self.binding.driver_id),
            );
        }
        Ok(DriverOutput {
            request_id: operation.request_id,
            capability: operation.capability,
            status: "completed".to_owned(),
            output: format!(
                "simulated hardware read from {} input={}",
                lease.device_id.as_str(),
                operation.input
            ),
            audit: vec![
                format!("driver:{}", self.binding.driver_id),
                format!("device:{}", lease.device_id.as_str()),
                "raw_io:false".to_owned(),
                "mode:simulated".to_owned(),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DeviceId;

    #[test]
    fn simulated_driver_requires_matching_capability() {
        let binding = DriverBinding::new(
            "scale-sim",
            CapabilityName::parse("hardware.scale.read").unwrap(),
            "scale",
        )
        .unwrap();
        let driver = SimulatedDriver::new(binding);
        let lease = DeviceLease {
            device_id: DeviceId::parse("scale-main:main-scale").unwrap(),
            request_id: RequestId::parse("req-hardware-1").unwrap(),
            exclusive: true,
        };

        let error = driver
            .invoke(
                &lease,
                DriverOperation::new(
                    RequestId::parse("req-hardware-1").unwrap(),
                    CapabilityName::parse("hardware.raw.write").unwrap(),
                ),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }
}
