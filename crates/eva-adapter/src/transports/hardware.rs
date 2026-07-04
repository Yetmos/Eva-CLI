//! Hardware Adapter transport with device identity and hotplug policy.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::{EvaError, RequestId};
use eva_hardware::{
    DeviceCandidate, DeviceId, DeviceRegistry, DriverBinding, DriverOperation, HardwareDriver,
    SimulatedDriver,
};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "hardware Adapter transport with device identity and hotplug policy";

pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let logical_name = handle
        .hardware_logical_name
        .as_deref()
        .unwrap_or_else(|| handle.id.as_str());
    let candidate = DeviceCandidate::for_adapter(handle.id.clone(), logical_name)?;
    let mut registry = DeviceRegistry::from_candidates(&[candidate])?;
    let device_id = DeviceId::parse(&format!("{}:{}", handle.id.as_str(), logical_name))?;
    let request_id = invocation.request_id.clone();
    let lease = registry.claim(&device_id, request_id.clone())?;
    let binding = DriverBinding::new(
        format!("{}-simulated-driver", handle.id.as_str()),
        invocation.capability.clone(),
        handle
            .hardware_device_class
            .as_deref()
            .unwrap_or("hardware"),
    )?;
    let driver = SimulatedDriver::new(binding);
    let output = driver.invoke(
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AdapterHandle;
    use eva_config::AdapterTransport;
    use eva_core::{AdapterId, CapabilityName};

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
            mcp_tools: Vec::new(),
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            hardware_logical_name: Some("main-scale".to_owned()),
            hardware_device_class: Some("scale".to_owned()),
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
