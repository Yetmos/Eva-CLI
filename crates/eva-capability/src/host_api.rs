//! 本模块提供 `host_api` 相关实现。
//! Typed host API traits available to Lua and Agent runtimes.

use eva_core::{AdapterId, EvaError, InvokeRequest, InvokeResponse};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "typed host API traits available to Lua and Agent runtimes";

/// 约定 `CapabilityHostApi` 实现需要满足的接口。
/// Minimal host API that controlled Lua and Agent runtimes can invoke.
pub trait CapabilityHostApi {
    /// 执行 `invoke` 对应的受控流程。
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError>;

    /// 执行 `invoke_with_provider` 对应的受控流程。
    fn invoke_with_provider(
        &self,
        request: InvokeRequest,
        explicit_provider: Option<AdapterId>,
    ) -> Result<InvokeResponse, EvaError> {
        if let Some(provider) = explicit_provider {
            return Err(EvaError::unsupported(
                "capability host does not support explicit provider routing",
            )
            .with_context("provider", provider.as_str()));
        }
        self.invoke(request)
    }
}
