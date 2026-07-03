//! Workflow skill Adapter transport envelopes.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "controlled workflow skill Adapter transport";

pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let skill = handle.skill_name().ok_or_else(|| {
        EvaError::invalid_argument("Skill adapter is missing skill.id")
            .with_context("adapter_id", handle.id.as_str())
    })?;
    if handle.skill_runtime_gate.as_deref() != Some("normal") {
        return Err(
            EvaError::permission_denied("Skill runtime gate is not allowed")
                .with_context("adapter_id", handle.id.as_str())
                .with_context(
                    "runtime_gate",
                    handle.skill_runtime_gate.as_deref().unwrap_or(""),
                ),
        );
    }
    Ok(AdapterInvokeReport {
        request_id: invocation.request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability: invocation.capability,
        status: "completed".to_owned(),
        output: format!(
            "{{\"transport\":\"skill\",\"adapter_id\":\"{}\",\"skill\":\"{}\",\"runtime_gate\":\"normal\",\"mode\":\"controlled-envelope\",\"input\":\"{}\"}}",
            escape_json(handle.id.as_str()),
            escape_json(skill),
            escape_json(&invocation.input)
        ),
        audit: vec![
            format!("adapter.invoked:{}", handle.id.as_str()),
            format!("skill.run:{skill}"),
        ],
    })
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value => escaped.push(value),
        }
    }
    escaped
}
