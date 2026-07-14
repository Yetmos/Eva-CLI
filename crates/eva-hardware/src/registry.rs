//! 维护逻辑设备登记和请求绑定的独占租约。
//!
//! 设备必须先登记才能认领，同一设备在租约释放前不能被其他请求重复占用；释放操作校验
//! 完整租约身份，避免旧请求误释放后来者持有的设备。
//! Claimed hardware device registry.

use crate::discovery::DeviceCandidate;
use crate::state::{DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
use eva_core::{EvaError, RequestId};
use std::collections::BTreeMap;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "claimed hardware device registry";

/// 表示 `RegisteredDevice` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredDevice {
    /// 记录 `identity` 字段对应的值。
    pub identity: DeviceIdentity,
    /// 记录 `health` 字段对应的值。
    pub health: DeviceHealth,
    /// 记录 `source_path` 字段对应的值。
    pub source_path: String,
    /// 记录 `claimed_by` 字段对应的值。
    pub claimed_by: Option<RequestId>,
}

/// 表示 `DeviceLease` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLease {
    /// 记录 `device_id` 字段对应的值。
    pub device_id: DeviceId,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `exclusive` 字段对应的值。
    pub exclusive: bool,
}

/// 表示 `DeviceRegistry` 数据结构。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeviceRegistry {
    /// 记录 `devices` 字段对应的值。
    devices: BTreeMap<DeviceId, RegisteredDevice>,
}

impl DeviceRegistry {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 根据输入构造当前类型，作为 `from_candidates` 的标准入口。
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

    /// 登记 `register` 对应的数据或状态。
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

    /// 返回 `list` 对应的数据视图。
    pub fn list(&self) -> Vec<&RegisteredDevice> {
        self.devices.values().collect()
    }

    /// 返回 `get` 对应的数据视图。
    pub fn get(&self, device_id: &DeviceId) -> Option<&RegisteredDevice> {
        self.devices.get(device_id)
    }

    /// 执行 `claim` 对应的处理逻辑。
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

    /// 执行 `release` 对应的处理逻辑。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DeviceCandidate;
    use eva_core::AdapterId;

    /// 验证 `registry_claims_and_releases_logical_lease` 场景下的预期行为。
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

    /// 验证 `registry_rejects_duplicate_claims` 场景下的预期行为。
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
