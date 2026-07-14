//! 通过能力绑定和设备租约调用硬件驱动，隔离原始设备访问。
//!
//! 驱动注册表按稳定标识拒绝重复项；调用只传递受控操作封装。模拟器契约套件明确验证能力
//! 不匹配会被拒绝、审计不允许原始 I/O，且输出不得泄露设备句柄或平台路径。
//! Driver binding behind policy-controlled interfaces.

use crate::registry::DeviceLease;
use eva_core::{CapabilityName, EvaError, RequestId};
use std::collections::BTreeMap;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "driver binding behind policy-controlled interfaces";

/// 表示 `DriverBinding` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverBinding {
    /// 记录 `driver_id` 字段对应的值。
    pub driver_id: String,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `device_class` 字段对应的值。
    pub device_class: String,
    /// 记录 `read_only` 字段对应的值。
    pub read_only: bool,
}

/// 表示 `DriverOperation` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverOperation {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `input` 字段对应的值。
    pub input: String,
}

/// 表示 `DriverOutput` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverOutput {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `output` 字段对应的值。
    pub output: String,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 约定 `HardwareDriver` 实现需要满足的接口。
pub trait HardwareDriver {
    /// 返回 `binding` 对应的数据视图。
    fn binding(&self) -> &DriverBinding;
    /// 执行 `invoke` 对应的受控流程。
    fn invoke(
        &self,
        lease: &DeviceLease,
        operation: DriverOperation,
    ) -> Result<DriverOutput, EvaError>;
}

/// 表示 `HardwareDriverRegistry` 数据结构。
#[derive(Default)]
pub struct HardwareDriverRegistry {
    /// 记录 `drivers` 字段对应的值。
    drivers: BTreeMap<String, Box<dyn HardwareDriver>>,
}

/// 表示 `SimulatorContractReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatorContractReport {
    /// 记录 `driver_id` 字段对应的值。
    pub driver_id: String,
    /// 记录 `device_id` 字段对应的值。
    pub device_id: String,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `raw_io_allowed` 字段对应的值。
    pub raw_io_allowed: bool,
    /// 记录 `raw_handle_exposed` 字段对应的值。
    pub raw_handle_exposed: bool,
    /// 记录 `capability_mismatch_rejected` 字段对应的值。
    pub capability_mismatch_rejected: bool,
}

/// 表示 `SimulatedDriver` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedDriver {
    /// 记录 `binding` 字段对应的值。
    binding: DriverBinding,
}

impl DriverBinding {
    /// 创建并初始化当前类型的实例。
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
    /// 创建并初始化当前类型的实例。
    pub fn new(request_id: RequestId, capability: CapabilityName) -> Self {
        Self {
            request_id,
            capability,
            input: String::new(),
        }
    }

    /// 设置 `input` 并返回更新后的实例。
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }
}

impl SimulatedDriver {
    /// 创建并初始化当前类型的实例。
    pub fn new(binding: DriverBinding) -> Self {
        Self { binding }
    }
}

impl HardwareDriverRegistry {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记 `register` 对应的数据或状态。
    pub fn register<D>(&mut self, driver: D) -> Result<(), EvaError>
    where
        D: HardwareDriver + 'static,
    {
        let driver_id = driver.binding().driver_id.clone();
        if self.drivers.contains_key(&driver_id) {
            return Err(EvaError::conflict("hardware driver already registered")
                .with_context("driver_id", driver_id));
        }
        self.drivers.insert(driver_id, Box::new(driver));
        Ok(())
    }

    /// 返回 `list_bindings` 对应的数据视图。
    pub fn list_bindings(&self) -> Vec<&DriverBinding> {
        self.drivers
            .values()
            .map(|driver| driver.binding())
            .collect()
    }

    /// 返回 `binding` 对应的数据视图。
    pub fn binding(&self, driver_id: &str) -> Option<&DriverBinding> {
        self.drivers.get(driver_id).map(|driver| driver.binding())
    }

    /// 执行 `invoke` 对应的受控流程。
    pub fn invoke(
        &self,
        driver_id: &str,
        lease: &DeviceLease,
        operation: DriverOperation,
    ) -> Result<DriverOutput, EvaError> {
        let driver = self.drivers.get(driver_id).ok_or_else(|| {
            EvaError::not_found("hardware driver is not registered")
                .with_context("driver_id", driver_id)
        })?;
        driver.invoke(lease, operation)
    }
}

/// 执行 `run_simulator_contract_suite` 对应的受控流程。
pub fn run_simulator_contract_suite<D>(
    driver: &D,
    lease: &DeviceLease,
) -> Result<SimulatorContractReport, EvaError>
where
    D: HardwareDriver,
{
    let binding = driver.binding();
    let output = driver.invoke(
        lease,
        DriverOperation::new(lease.request_id.clone(), binding.capability.clone())
            .with_input("simulator-contract"),
    )?;
    let audit_text = output.audit.join("\n");
    let raw_io_allowed = !output.audit.iter().any(|entry| entry == "raw_io:false");
    let raw_handle_exposed = audit_text.contains("raw_handle:")
        || output.output.contains("raw_handle:")
        || audit_text.contains("/dev/")
        || output.output.contains("/dev/");
    let capability_mismatch_rejected = match CapabilityName::parse("hardware.raw.write") {
        Ok(raw_capability) if raw_capability != binding.capability => driver
            .invoke(
                lease,
                DriverOperation::new(lease.request_id.clone(), raw_capability),
            )
            .is_err_and(|error| error.kind() == eva_core::ErrorKind::PermissionDenied),
        _ => true,
    };

    let report = SimulatorContractReport {
        driver_id: binding.driver_id.clone(),
        device_id: lease.device_id.as_str().to_owned(),
        capability: binding.capability.clone(),
        raw_io_allowed,
        raw_handle_exposed,
        capability_mismatch_rejected,
    };
    if report.raw_io_allowed || report.raw_handle_exposed || !report.capability_mismatch_rejected {
        return Err(EvaError::internal("simulator driver contract failed")
            .with_context("driver_id", &report.driver_id));
    }
    Ok(report)
}

impl HardwareDriver for SimulatedDriver {
    /// 返回 `binding` 对应的数据视图。
    fn binding(&self) -> &DriverBinding {
        &self.binding
    }

    /// 执行 `invoke` 对应的受控流程。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DeviceId;

    /// 验证 `simulated_driver_requires_matching_capability` 场景下的预期行为。
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

    /// 验证 `driver_registry_invokes_registered_simulator` 场景下的预期行为。
    #[test]
    fn driver_registry_invokes_registered_simulator() {
        let binding = DriverBinding::new(
            "scale-sim",
            CapabilityName::parse("hardware.scale.read").unwrap(),
            "scale",
        )
        .unwrap();
        let mut registry = HardwareDriverRegistry::new();
        registry
            .register(SimulatedDriver::new(binding.clone()))
            .unwrap();
        let lease = DeviceLease {
            device_id: DeviceId::parse("scale-main:main-scale").unwrap(),
            request_id: RequestId::parse("req-hardware-1").unwrap(),
            exclusive: true,
        };

        let output = registry
            .invoke(
                "scale-sim",
                &lease,
                DriverOperation::new(
                    RequestId::parse("req-hardware-1").unwrap(),
                    binding.capability,
                ),
            )
            .unwrap();

        assert_eq!(output.status, "completed");
        assert!(output.audit.contains(&"raw_io:false".to_owned()));
        assert_eq!(registry.list_bindings().len(), 1);
    }

    /// 验证 `simulator_contract_suite_rejects_raw_io_and_capability_bypass` 场景下的预期行为。
    #[test]
    fn simulator_contract_suite_rejects_raw_io_and_capability_bypass() {
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

        let report = run_simulator_contract_suite(&driver, &lease).unwrap();

        assert!(!report.raw_io_allowed);
        assert!(!report.raw_handle_exposed);
        assert!(report.capability_mismatch_rejected);
    }
}
