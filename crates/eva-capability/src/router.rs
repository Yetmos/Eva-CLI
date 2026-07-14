//! 为能力请求生成确定的提供者计划，并在返回前执行权限门禁。
//!
//! 显式提供者只影响候选顺序，不会绕过能力清单或运行时权限；无效目标、禁用能力和空计划
//! 均在进入适配器运行时前失败。
//! Capability routing before provider execution.

use crate::host_api::CapabilityHostApi;
use crate::registry::{CapabilityDescriptor, CapabilityRegistry};
use crate::selection::CapabilityProviderPlan;
use crate::CapabilityPermissionGate;
use eva_core::{AdapterId, EvaError, InvokeOutput, InvokeRequest, InvokeResponse, InvokeTarget};
use eva_policy::PermissionSet;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability routing before provider execution";

/// 表示 `CapabilityRouter` 数据结构。
/// V0.4 router for builtin capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRouter {
    /// 记录 `registry` 字段对应的值。
    registry: CapabilityRegistry,
}

impl CapabilityRouter {
    /// 创建并初始化当前类型的实例。
    pub fn new(registry: CapabilityRegistry) -> Self {
        Self { registry }
    }

    /// 设置 `v04_builtins` 并返回更新后的实例。
    pub fn with_v04_builtins() -> Self {
        Self::new(CapabilityRegistry::with_v04_builtins())
    }

    /// 执行 `registry` 对应的处理逻辑。
    pub fn registry(&self) -> &CapabilityRegistry {
        &self.registry
    }

    /// 执行 `provider_plan` 对应的处理逻辑。
    pub fn provider_plan(
        &self,
        request: &InvokeRequest,
        explicit_provider: Option<AdapterId>,
    ) -> Result<CapabilityProviderPlan, EvaError> {
        let descriptor = self.descriptor_for_request(request)?;
        self.ensure_enabled(descriptor)?;
        Ok(descriptor.provider_plan(explicit_provider))
    }

    /// 执行 `authorized_provider_plan` 对应的处理逻辑。
    pub fn authorized_provider_plan(
        &self,
        request: &InvokeRequest,
        explicit_provider: Option<AdapterId>,
        permissions: &PermissionSet,
    ) -> Result<CapabilityProviderPlan, EvaError> {
        let plan = self.provider_plan(request, explicit_provider)?;
        CapabilityPermissionGate::new(permissions.clone()).ensure_plan_allowed(&plan)?;
        Ok(plan)
    }

    /// 执行 `invoke_descriptor` 对应的受控流程。
    fn invoke_descriptor(
        &self,
        descriptor: &CapabilityDescriptor,
        request: InvokeRequest,
    ) -> Result<InvokeResponse, EvaError> {
        self.ensure_enabled(descriptor)?;

        let text = request.input().as_text().unwrap_or_default();
        let output = match descriptor.name.as_str() {
            "config.lint" => format!(
                "{{\"valid\":true,\"findings\":[],\"input\":\"{}\"}}",
                escape_json(text)
            ),
            "runtime.echo" => format!("{{\"echo\":\"{}\"}}", escape_json(text)),
            value => {
                return Err(EvaError::unsupported("capability has no builtin provider")
                    .with_context("capability", value))
            }
        };
        Ok(InvokeResponse::completed(
            request.request_id().clone(),
            InvokeOutput::text(output),
        ))
    }

    /// 执行 `descriptor_for_request` 对应的处理逻辑。
    fn descriptor_for_request(
        &self,
        request: &InvokeRequest,
    ) -> Result<&CapabilityDescriptor, EvaError> {
        let capability = match request.target() {
            InvokeTarget::Capability(capability) => capability,
            _ => {
                return Err(EvaError::invalid_argument(
                    "capability router requires capability target",
                ))
            }
        };
        self.registry.get(capability).ok_or_else(|| {
            EvaError::not_found("capability is not registered")
                .with_context("capability", capability.as_str())
        })
    }

    /// 校验 `ensure_enabled` 对应的约束，不满足时返回明确错误。
    fn ensure_enabled(&self, descriptor: &CapabilityDescriptor) -> Result<(), EvaError> {
        if descriptor.enabled {
            return Ok(());
        }

        Err(EvaError::permission_denied("capability is disabled")
            .with_context("capability", descriptor.name.as_str()))
    }
}

impl CapabilityHostApi for CapabilityRouter {
    /// 执行 `invoke` 对应的受控流程。
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
        let descriptor = self.descriptor_for_request(&request)?;
        self.invoke_descriptor(descriptor, request)
    }
}

/// 按 `escape_json` 的协议约定生成输出。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityProviderSelection, CapabilityProviderSource, CapabilityRegistry};
    use eva_core::{CapabilityId, CapabilityName, ErrorKind, InvokeInput, RequestId};
    use eva_policy::PermissionSet;

    /// 验证 `builtin_config_lint_returns_completed_response` 场景下的预期行为。
    #[test]
    fn builtin_config_lint_returns_completed_response() {
        let router = CapabilityRouter::with_v04_builtins();
        let request = InvokeRequest::new(
            RequestId::parse("req-1").unwrap(),
            InvokeTarget::Capability(CapabilityName::parse("config.lint").unwrap()),
            InvokeInput::text("config"),
        );

        let response = router.invoke(request).unwrap();

        assert!(response.is_success());
        assert!(response
            .output()
            .unwrap()
            .as_text()
            .unwrap()
            .contains("valid"));
    }

    /// 验证 `provider_plan_rejects_disabled_capability_before_selection` 场景下的预期行为。
    #[test]
    fn provider_plan_rejects_disabled_capability_before_selection() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(CapabilityDescriptor {
                id: CapabilityId::parse("runtime-echo-disabled").unwrap(),
                name: CapabilityName::parse("runtime.echo").unwrap(),
                enabled: false,
                provider: "builtin".to_owned(),
                provider_selection: Default::default(),
            })
            .unwrap();
        let router = CapabilityRouter::new(registry);
        let request = InvokeRequest::new(
            RequestId::parse("req-disabled-plan").unwrap(),
            InvokeTarget::Capability(CapabilityName::parse("runtime.echo").unwrap()),
            InvokeInput::text("hello"),
        );

        let error = router.provider_plan(&request, None).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    /// 验证 `authorized_provider_plan_rejects_denied_provider` 场景下的预期行为。
    #[test]
    fn authorized_provider_plan_rejects_denied_provider() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(CapabilityDescriptor {
                id: CapabilityId::parse("repo-summary").unwrap(),
                name: CapabilityName::parse("repo.summary").unwrap(),
                enabled: true,
                provider: "codex-cli".to_owned(),
                provider_selection: CapabilityProviderSelection::new(
                    None,
                    Some(AdapterId::parse("codex-cli").unwrap()),
                    Vec::new(),
                    Vec::new(),
                ),
            })
            .unwrap();
        let router = CapabilityRouter::new(registry);
        let request = InvokeRequest::new(
            RequestId::parse("req-auth-plan-denied").unwrap(),
            InvokeTarget::Capability(CapabilityName::parse("repo.summary").unwrap()),
            InvokeInput::text("repo"),
        );
        let permissions = PermissionSet::deny_all()
            .allow_capability(CapabilityName::parse("repo.summary").unwrap());

        let error = router
            .authorized_provider_plan(&request, None, &permissions)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "gate" && value == "adapter"));
    }

    /// 验证 `authorized_provider_plan_preserves_explicit_provider_source` 场景下的预期行为。
    #[test]
    fn authorized_provider_plan_preserves_explicit_provider_source() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(CapabilityDescriptor {
                id: CapabilityId::parse("repo-summary").unwrap(),
                name: CapabilityName::parse("repo.summary").unwrap(),
                enabled: true,
                provider: "codex-cli".to_owned(),
                provider_selection: CapabilityProviderSelection::new(
                    None,
                    Some(AdapterId::parse("codex-cli").unwrap()),
                    vec![AdapterId::parse("fallback-cli").unwrap()],
                    Vec::new(),
                ),
            })
            .unwrap();
        let router = CapabilityRouter::new(registry);
        let request = InvokeRequest::new(
            RequestId::parse("req-auth-plan-ok").unwrap(),
            InvokeTarget::Capability(CapabilityName::parse("repo.summary").unwrap()),
            InvokeInput::text("repo"),
        );
        let permissions = PermissionSet::deny_all()
            .allow_capability(CapabilityName::parse("repo.summary").unwrap())
            .allow_adapter(AdapterId::parse("fallback-cli").unwrap())
            .allow_adapter(AdapterId::parse("codex-cli").unwrap());

        let plan = router
            .authorized_provider_plan(
                &request,
                Some(AdapterId::parse("fallback-cli").unwrap()),
                &permissions,
            )
            .unwrap();

        assert_eq!(plan.providers[0].provider.as_str(), "fallback-cli");
        assert_eq!(
            plan.providers[0].source,
            CapabilityProviderSource::ExplicitRequest
        );
    }
}
