//! 本模块提供 `builtin` 相关实现。
//! Built-in local Adapter transport envelopes.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::EvaError;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "built-in local Adapter transport";

/// 执行 `invoke` 对应的受控流程。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    super::validate_process_free_identity(handle)?;
    let trace = invocation.trace_for_adapter(&handle.id);
    Ok(AdapterInvokeReport {
        request_id: invocation.request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability: invocation.capability,
        status: "completed".to_owned(),
        output: format!(
            "{{\"transport\":\"{}\",\"adapter_id\":\"{}\",\"mode\":\"controlled-envelope\"}}",
            handle.transport.as_str(),
            handle.id.as_str()
        ),
        audit: vec![format!("adapter.invoked:{}", handle.id.as_str())],
        trace,
    })
}
