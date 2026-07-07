//! Capability/provider permission gates before adapter execution.

use crate::selection::{CapabilityProviderCandidate, CapabilityProviderPlan};
use eva_core::{CapabilityName, EvaError};
use eva_policy::PermissionSet;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability and provider permission gate before execution";

/// Side-effect-free gate that verifies a capability provider candidate is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityPermissionGate {
    permissions: PermissionSet,
}

impl CapabilityPermissionGate {
    pub fn new(permissions: PermissionSet) -> Self {
        Self { permissions }
    }

    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }

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

    pub fn ensure_plan_allowed(&self, plan: &CapabilityProviderPlan) -> Result<(), EvaError> {
        self.ensure_capability_allowed(&plan.capability)?;
        self.ensure_required_adapter_capabilities_allowed(plan)?;
        for candidate in &plan.providers {
            self.ensure_provider_allowed(plan, candidate)?;
        }
        Ok(())
    }

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
    fn default() -> Self {
        Self::new(PermissionSet::deny_all())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityProviderSelection, CapabilityProviderSource};
    use eva_core::{AdapterId, ErrorKind};

    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    fn plan() -> CapabilityProviderPlan {
        CapabilityProviderSelection::new(
            None,
            Some(adapter("codex-cli")),
            vec![adapter("fallback-cli")],
            vec![capability("repo.analyze")],
        )
        .plan_for(capability("repo.summary"), None)
    }

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
