//! 本模块提供 `hardware` 相关实现。
//! Hardware Adapter transport with device identity and hotplug policy.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::{EvaError, RequestId};
use eva_hardware::{
    DeviceCandidate, DeviceId, DeviceRegistry, DriverBinding, DriverOperation,
    HardwareDriverRegistry, SimulatedDriver,
};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "hardware Adapter transport with device identity and hotplug policy";

/// 执行 `invoke` 对应的受控流程。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let trace = invocation.trace_for_adapter(&handle.id);
    let logical_name = handle
        .hardware_logical_name
        .as_deref()
        .unwrap_or_else(|| handle.id.as_str());
    let candidate = DeviceCandidate::for_adapter(handle.id.clone(), logical_name)?;
    let mut registry = DeviceRegistry::from_candidates(&[candidate])?;
    let device_id = DeviceId::parse(&format!("{}:{}", handle.id.as_str(), logical_name))?;
    let request_id = invocation.request_id.clone();
    let lease = registry.claim(&device_id, request_id.clone())?;
    let driver_id = handle
        .hardware_driver_id
        .clone()
        .unwrap_or_else(|| format!("{}-simulated-driver", handle.id.as_str()));
    let driver_kind = handle
        .hardware_driver_kind
        .as_deref()
        .unwrap_or("simulated");
    if driver_kind != "simulated" {
        return Err(
            EvaError::unsupported("hardware driver kind is reserved but not implemented")
                .with_context("driver_kind", driver_kind)
                .with_context("driver_id", &driver_id),
        );
    }
    let binding = DriverBinding::new(
        driver_id.clone(),
        invocation.capability.clone(),
        handle
            .hardware_device_class
            .as_deref()
            .unwrap_or("hardware"),
    )?;
    let mut driver_registry = HardwareDriverRegistry::new();
    driver_registry.register(SimulatedDriver::new(binding))?;
    let output = driver_registry.invoke(
        &driver_id,
        &lease,
        DriverOperation::new(
            RequestId::parse(request_id.as_str())?,
            invocation.capability.clone(),
        )
        .with_input(invocation.input.clone()),
    )?;
    registry.release(&lease)?;

    let mut audit = output.audit;
    audit.push("transport:hardware".to_owned());
    audit.push("lease:released".to_owned());
    Ok(AdapterInvokeReport {
        request_id: invocation.request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability: invocation.capability,
        status: output.status,
        output: output.output,
        audit,
        trace,
    })
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AdapterHandle;
    use eva_config::AdapterTransport;
    use eva_core::{AdapterId, CapabilityName};
    use std::collections::BTreeMap;

    /// 验证 `hardware_transport_returns_simulated_audit_only` 场景下的预期行为。
    #[test]
    fn hardware_transport_returns_simulated_audit_only() {
        let mut handle = AdapterHandle {
            id: AdapterId::parse("scale-main").unwrap(),
            name: "Scale".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Hardware,
            capabilities: vec![CapabilityName::parse("hardware.scale.read").unwrap()],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: None,
            method: None,
            credential_env: Vec::new(),
            provider: eva_config::ProviderConfig::default(),
            timeout_ms: None,
            max_concurrency: None,
            output_limit_bytes: None,
            max_prompt_bytes: None,
            rate_limit: None,
            circuit_breaker: None,
            headers: BTreeMap::new(),
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            skill_path: None,
            skill_entry_type: None,
            skill_runner_command: None,
            skill_runner_args: Vec::new(),
            skill_artifact_root: None,
            skill_input_schema: None,
            hardware_logical_name: Some("main-scale".to_owned()),
            hardware_device_class: Some("scale".to_owned()),
            hardware_driver_id: Some("scale-main-simulated-driver".to_owned()),
            hardware_driver_kind: Some("simulated".to_owned()),
            bindings: Vec::new(),
        };
        handle.capabilities.sort();
        let report = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-hardware-1").unwrap(),
                CapabilityName::parse("hardware.scale.read").unwrap(),
            ),
        )
        .unwrap();

        assert_eq!(report.status, "completed");
        assert!(report.audit.contains(&"raw_io:false".to_owned()));
        assert!(report.audit.contains(&"lease:released".to_owned()));
    }
}
