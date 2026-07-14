//! 在适配器执行前实施能力与提供者的显式许可门禁。
//!
//! 判定采用默认拒绝：请求能力、清单声明的依赖能力、提供者权限以及清单提供者范围必须
//! 同时满足。门禁无副作用，失败会携带稳定的 `gate` 上下文，便于上层区分拒绝发生在哪一层。
//! Capability/provider permission gates before adapter execution.

use crate::selection::{CapabilityProviderCandidate, CapabilityProviderPlan};
use eva_core::{CapabilityName, EvaError};
use eva_policy::PermissionSet;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability and provider permission gate before execution";

/// 表示 `CapabilityPermissionGate` 数据结构。
/// Side-effect-free gate that verifies a capability provider candidate is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityPermissionGate {
    /// 记录 `permissions` 字段对应的值。
    permissions: PermissionSet,
}

impl CapabilityPermissionGate {
    /// 创建并初始化当前类型的实例。
    pub fn new(permissions: PermissionSet) -> Self {
        Self { permissions }
    }

    /// 返回 `permissions` 对应的数据视图。
    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }

    /// 校验 `ensure_capability_allowed` 对应的约束，不满足时返回明确错误。
    pub fn ensure_capability_allowed(&self, capability: &CapabilityName) -> Result<(), EvaError> {
        if self.permissions.explicitly_allows_capability(capability) {
            return Ok(());
        }

        Err(
            EvaError::permission_denied("capability is not explicitly allowed")
                .with_context("capability", capability.as_str())
                .with_context("gate", "capability"),
        )
    }

    /// 校验 `ensure_provider_allowed` 对应的约束，不满足时返回明确错误。
    pub fn ensure_provider_allowed(
        &self,
        plan: &CapabilityProviderPlan,
        candidate: &CapabilityProviderCandidate,
    ) -> Result<(), EvaError> {
        self.ensure_capability_allowed(&plan.capability)?;
        self.ensure_required_adapter_capabilities_allowed(plan)?;
        if !self
            .permissions
            .explicitly_allows_adapter(&candidate.provider)
        {
            return Err(
                EvaError::permission_denied("adapter provider is not explicitly allowed")
                    .with_context("capability", plan.capability.as_str())
                    .with_context("provider", candidate.provider.as_str())
                    .with_context("provider_source", candidate.source.as_str())
                    .with_context("gate", "adapter"),
            );
        }
        if !plan.allows_manifest_provider(&candidate.provider) {
            return Err(EvaError::permission_denied(
                "adapter provider is not allowed by capability manifest",
            )
            .with_context("capability", plan.capability.as_str())
            .with_context("provider", candidate.provider.as_str())
            .with_context("provider_source", candidate.source.as_str())
            .with_context("gate", "manifest"));
        }

        Ok(())
    }

    /// 校验 `ensure_plan_allowed` 对应的约束，不满足时返回明确错误。
    pub fn ensure_plan_allowed(&self, plan: &CapabilityProviderPlan) -> Result<(), EvaError> {
        self.ensure_capability_allowed(&plan.capability)?;
        self.ensure_required_adapter_capabilities_allowed(plan)?;
        for candidate in &plan.providers {
            self.ensure_provider_allowed(plan, candidate)?;
        }
        Ok(())
    }

    /// 校验 `ensure_required_adapter_capabilities_allowed` 对应的约束，不满足时返回明确错误。
    fn ensure_required_adapter_capabilities_allowed(
        &self,
        plan: &CapabilityProviderPlan,
    ) -> Result<(), EvaError> {
        for capability in &plan.required_adapter_capabilities {
            if !self.permissions.explicitly_allows_capability(capability) {
                return Err(EvaError::permission_denied(
                    "required adapter capability is not explicitly allowed",
                )
                .with_context("capability", plan.capability.as_str())
                .with_context("required_capability", capability.as_str())
                .with_context("gate", "adapter_capability"));
            }
        }
        Ok(())
    }
}

impl Default for CapabilityPermissionGate {
    /// 创建默认拒绝所有能力与提供者的门禁。
    fn default() -> Self {
        Self::new(PermissionSet::deny_all())
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityProviderSelection, CapabilityProviderSource};
    use eva_core::{AdapterId, ErrorKind};

    /// 执行 `adapter` 对应的处理逻辑。
    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    /// 执行 `capability` 对应的处理逻辑。
    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    /// 执行 `plan` 对应的处理逻辑。
    fn plan() -> CapabilityProviderPlan {
        CapabilityProviderSelection::new(
            None,
            Some(adapter("codex-cli")),
            vec![adapter("fallback-cli")],
            vec![capability("repo.analyze")],
        )
        .plan_for(capability("repo.summary"), None)
    }

    /// 验证 `default_gate_rejects_ungranted_capability` 场景下的预期行为。
    #[test]
    fn default_gate_rejects_ungranted_capability() {
        let gate = CapabilityPermissionGate::default();
        let plan = plan();

        let error = gate.ensure_plan_allowed(&plan).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "gate" && value == "capability"));
    }

    /// 验证 `gate_rejects_provider_not_in_permissions` 场景下的预期行为。
    #[test]
    fn gate_rejects_provider_not_in_permissions() {
        let gate = CapabilityPermissionGate::new(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_capability(capability("repo.analyze")),
        );
        let plan = plan();

        let error = gate.ensure_plan_allowed(&plan).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "gate" && value == "adapter"));
    }

    /// 验证 `gate_rejects_required_adapter_capability_not_in_permissions` 场景下的预期行为。
    #[test]
    fn gate_rejects_required_adapter_capability_not_in_permissions() {
        let gate = CapabilityPermissionGate::new(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_adapter(adapter("codex-cli"))
                .allow_adapter(adapter("fallback-cli")),
        );
        let plan = plan();

        let error = gate.ensure_plan_allowed(&plan).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "gate" && value == "adapter_capability"));
    }

    /// 验证 `gate_rejects_explicit_provider_outside_manifest_allowlist` 场景下的预期行为。
    #[test]
    fn gate_rejects_explicit_provider_outside_manifest_allowlist() {
        let selection = CapabilityProviderSelection::new(
            None,
            Some(adapter("codex-cli")),
            Vec::new(),
            Vec::new(),
        );
        let plan = selection.plan_for(capability("repo.summary"), Some(adapter("shadow-cli")));
        let gate = CapabilityPermissionGate::new(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_adapter(adapter("shadow-cli"))
                .allow_adapter(adapter("codex-cli")),
        );

        let error = gate
            .ensure_provider_allowed(&plan, &plan.providers[0])
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            plan.providers[0].source,
            CapabilityProviderSource::ExplicitRequest
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "gate" && value == "manifest"));
    }

    /// 验证 `gate_allows_manifest_and_permission_granted_plan` 场景下的预期行为。
    #[test]
    fn gate_allows_manifest_and_permission_granted_plan() {
        let gate = CapabilityPermissionGate::new(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_capability(capability("repo.analyze"))
                .allow_adapter(adapter("codex-cli"))
                .allow_adapter(adapter("fallback-cli")),
        );

        gate.ensure_plan_allowed(&plan()).unwrap();
    }
}
