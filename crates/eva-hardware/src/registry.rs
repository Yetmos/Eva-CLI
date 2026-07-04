//! Claimed hardware device registry.

use crate::discovery::DeviceCandidate;
use crate::state::{DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
use eva_core::{EvaError, RequestId};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "claimed hardware device registry";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredDevice {
    pub identity: DeviceIdentity,
    pub health: DeviceHealth,
    pub source_path: String,
    pub claimed_by: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLease {
    pub device_id: DeviceId,
    pub request_id: RequestId,
    pub exclusive: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeviceRegistry {
    devices: BTreeMap<DeviceId, RegisteredDevice>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_candidates(candidates: &[DeviceCandidate]) -> Result<Self, EvaError> {
        let mut registry = Self::new();
        for candidate in candidates {
            if candidate.identity.trust == DeviceTrust::Rejected {
                continue;
            }
            registry.register(candidate)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, candidate: &DeviceCandidate) -> Result<(), EvaError> {
        if candidate.identity.trust == DeviceTrust::Rejected {
            return Err(EvaError::permission_denied(
                "rejected hardware candidate cannot be registered",
            )
            .with_context("device_id", candidate.identity.id.as_str()));
        }
        if self.devices.contains_key(&candidate.identity.id) {
            return Err(EvaError::conflict("device already registered")
                .with_context("device_id", candidate.identity.id.as_str()));
        }
        self.devices.insert(
            candidate.identity.id.clone(),
            RegisteredDevice {
                identity: candidate.identity.clone(),
                health: candidate.health,
                source_path: candidate.source_path.clone(),
                claimed_by: None,
            },
        );
        Ok(())
    }

    pub fn list(&self) -> Vec<&RegisteredDevice> {
        self.devices.values().collect()
    }

    pub fn get(&self, device_id: &DeviceId) -> Option<&RegisteredDevice> {
        self.devices.get(device_id)
    }

    pub fn claim(
        &mut self,
        device_id: &DeviceId,
        request_id: RequestId,
    ) -> Result<DeviceLease, EvaError> {
        let device = self.devices.get_mut(device_id).ok_or_else(|| {
            EvaError::not_found("device is not registered")
                .with_context("device_id", device_id.as_str())
        })?;
        if device.claimed_by.is_some() {
            return Err(EvaError::conflict("device is already claimed")
                .with_context("device_id", device_id.as_str()));
        }
        if !matches!(
            device.health,
            DeviceHealth::Available | DeviceHealth::Candidate
        ) {
            return Err(EvaError::unavailable("device is not claimable")
                .with_context("device_id", device_id.as_str())
                .with_context("health", device.health.as_str()));
        }
        device.claimed_by = Some(request_id.clone());
        device.health = DeviceHealth::Claimed;
        Ok(DeviceLease {
            device_id: device_id.clone(),
            request_id,
            exclusive: true,
        })
    }

    pub fn release(&mut self, lease: &DeviceLease) -> Result<(), EvaError> {
        let device = self.devices.get_mut(&lease.device_id).ok_or_else(|| {
            EvaError::not_found("device is not registered")
                .with_context("device_id", lease.device_id.as_str())
        })?;
        if device.claimed_by.as_ref() != Some(&lease.request_id) {
            return Err(
                EvaError::permission_denied("device lease does not match claimant")
                    .with_context("device_id", lease.device_id.as_str()),
            );
        }
        device.claimed_by = None;
        device.health = DeviceHealth::Available;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DeviceCandidate;
    use eva_core::AdapterId;

    #[test]
    fn registry_claims_and_releases_logical_lease() {
        let candidate =
            DeviceCandidate::for_adapter(AdapterId::parse("scale-main").unwrap(), "main-scale")
                .unwrap();
        let mut registry = DeviceRegistry::from_candidates(&[candidate]).unwrap();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();
        let request_id = RequestId::parse("req-hardware-1").unwrap();

        let lease = registry.claim(&device_id, request_id).unwrap();
        assert_eq!(
            registry.get(&device_id).unwrap().health,
            DeviceHealth::Claimed
        );

        registry.release(&lease).unwrap();
        assert_eq!(
            registry.get(&device_id).unwrap().health,
            DeviceHealth::Available
        );
    }

    #[test]
    fn registry_rejects_duplicate_claims() {
        let candidate =
            DeviceCandidate::for_adapter(AdapterId::parse("scale-main").unwrap(), "main-scale")
                .unwrap();
        let mut registry = DeviceRegistry::from_candidates(&[candidate]).unwrap();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();
        registry
            .claim(&device_id, RequestId::parse("req-hardware-1").unwrap())
            .unwrap();

        let error = registry
            .claim(&device_id, RequestId::parse("req-hardware-2").unwrap())
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }
}
