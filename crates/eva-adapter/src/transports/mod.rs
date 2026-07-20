//! 本模块提供 `mod` 相关实现。
//! Adapter transport boundaries.

use crate::manifest::AdapterHandle;
use eva_config::ProviderRunAsIdentity;
use eva_core::EvaError;

/// 声明 `builtin` 子模块。
pub mod builtin;
/// 声明 `eventbus` 子模块。
pub mod eventbus;
/// 声明 `hardware` 子模块。
pub mod hardware;
/// 声明 `http` 子模块。
pub mod http;
/// 声明 `lua_capability` 子模块。
pub mod lua_capability;
/// 声明 `mcp` 子模块。
pub mod mcp;
/// 声明 `skill` 子模块。
pub mod skill;
/// 声明 `stdio` 子模块。
pub mod stdio;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "authorized Adapter transport implementations";

pub(crate) fn validate_process_free_identity(handle: &AdapterHandle) -> Result<(), EvaError> {
    if matches!(&handle.provider.run_as, ProviderRunAsIdentity::Current) {
        Ok(())
    } else {
        Err(EvaError::permission_denied(
            "process-free Adapter transport cannot apply a run-as identity",
        )
        .with_context("adapter_id", handle.id.as_str())
        .with_context("transport", handle.transport.as_str())
        .with_context("run_as_kind", handle.provider.run_as.kind()))
    }
}

/// Process-free local transports have no child environment or vault handoff.
/// A credential declaration is therefore a configuration error, not something
/// the transport may silently ignore.
pub(crate) fn validate_process_free_credentials(handle: &AdapterHandle) -> Result<(), EvaError> {
    let declared = !handle.credential_env.is_empty()
        || !handle.provider.vault_secrets.is_empty()
        || handle
            .headers
            .values()
            .any(|value| value.strip_prefix("env:").is_some());
    if declared {
        return Err(EvaError::unsupported(
            "process-free Adapter transport cannot consume provider credentials",
        )
        .with_provider_code("process_free_credentials")
        .with_context("adapter_id", handle.id.as_str())
        .with_context("transport", handle.transport.as_str()));
    }
    Ok(())
}
