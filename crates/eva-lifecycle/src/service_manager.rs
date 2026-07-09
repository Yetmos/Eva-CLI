//! OS service-manager abstraction boundary.

use crate::{RuntimeHealth, UpgradeApplyPlan};
use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "OS service-manager adapter trait, fake handoff, and rollback evidence boundary";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerKind {
    Fake,
    WindowsService,
    Systemd,
    Launchd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerDefinition {
    pub enabled: bool,
    pub kind: ServiceManagerKind,
    pub service_name: String,
    pub unit_name: Option<String>,
    pub runtime_binary: Option<String>,
    pub candidate_runtime_binary: Option<String>,
    pub start_on_boot: bool,
    pub restart_supervisor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerStatusReport {
    pub kind: ServiceManagerKind,
    pub service_name: String,
    pub configured: bool,
    pub production_adapter: bool,
    pub active_generation: Option<String>,
    pub active_release: Option<String>,
    pub candidate_generation: Option<String>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerHandoffReport {
    pub plan_id: String,
    pub kind: ServiceManagerKind,
    pub service_name: String,
    pub status: String,
    pub handoff_executed: bool,
    pub rollback_required: bool,
    pub active_generation: String,
    pub previous_generation: String,
    pub release_ref: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerRollbackReport {
    pub plan_id: String,
    pub kind: ServiceManagerKind,
    pub service_name: String,
    pub status: String,
    pub rollback_executed: bool,
    pub active_generation: String,
    pub release_ref: String,
    pub reason: String,
    pub audit: Vec<String>,
}

pub struct ServiceManagerInspectRequest<'a> {
    pub definition: &'a ServiceManagerDefinition,
}

pub struct ServiceManagerHandoffRequest<'a> {
    pub definition: &'a ServiceManagerDefinition,
    pub plan: &'a UpgradeApplyPlan,
    pub candidate_health: RuntimeHealth,
}

pub struct ServiceManagerRollbackRequest<'a> {
    pub definition: &'a ServiceManagerDefinition,
    pub plan: &'a UpgradeApplyPlan,
    pub reason: &'a str,
}

pub trait ServiceManagerAdapter {
    fn kind(&self) -> ServiceManagerKind;

    fn inspect(
        &self,
        request: ServiceManagerInspectRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError>;

    fn handoff(
        &mut self,
        request: ServiceManagerHandoffRequest<'_>,
    ) -> Result<ServiceManagerHandoffReport, EvaError>;

    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FakeServiceManagerAdapter {
    active_generation: Option<String>,
    active_release: Option<String>,
    candidate_generation: Option<String>,
}

impl ServiceManagerKind {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "fake" => Ok(Self::Fake),
            "windows_service" | "windows-service" | "windows" => Ok(Self::WindowsService),
            "systemd" => Ok(Self::Systemd),
            "launchd" => Ok(Self::Launchd),
            _ => Err(
                EvaError::invalid_argument("unsupported service manager kind")
                    .with_context("kind", value),
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::WindowsService => "windows_service",
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
        }
    }

    pub fn production_adapter(self) -> bool {
        !matches!(self, Self::Fake)
    }
}

impl ServiceManagerDefinition {
    pub fn new(
        enabled: bool,
        kind: ServiceManagerKind,
        service_name: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let service_name = stable_non_empty(service_name.into(), "service_name")?;
        Ok(Self {
            enabled,
            kind,
            service_name,
            unit_name: None,
            runtime_binary: None,
            candidate_runtime_binary: None,
            start_on_boot: false,
            restart_supervisor: false,
        })
    }

    pub fn production_adapter_enabled(&self) -> bool {
        self.enabled && self.kind.production_adapter()
    }
}

impl FakeServiceManagerAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_active_generation(
        generation: impl Into<String>,
        release: impl Into<String>,
    ) -> Self {
        Self {
            active_generation: Some(generation.into()),
            active_release: Some(release.into()),
            candidate_generation: None,
        }
    }

    fn ensure_fake(definition: &ServiceManagerDefinition) -> Result<(), EvaError> {
        if definition.kind == ServiceManagerKind::Fake {
            Ok(())
        } else {
            Err(EvaError::unsupported(
                "fake service manager adapter cannot execute platform service manager kind",
            )
            .with_context("kind", definition.kind.as_str())
            .with_context("service_name", &definition.service_name))
        }
    }

    fn ensure_enabled(definition: &ServiceManagerDefinition) -> Result<(), EvaError> {
        if definition.enabled {
            Ok(())
        } else {
            Err(EvaError::invalid_argument("service manager is not enabled")
                .with_context("service_name", &definition.service_name))
        }
    }
}

impl ServiceManagerAdapter for FakeServiceManagerAdapter {
    fn kind(&self) -> ServiceManagerKind {
        ServiceManagerKind::Fake
    }

    fn inspect(
        &self,
        request: ServiceManagerInspectRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Ok(ServiceManagerStatusReport {
            kind: ServiceManagerKind::Fake,
            service_name: request.definition.service_name.clone(),
            configured: request.definition.enabled,
            production_adapter: false,
            active_generation: self.active_generation.clone(),
            active_release: self.active_release.clone(),
            candidate_generation: self.candidate_generation.clone(),
            audit: vec![
                "service_manager.fake:inspect".to_owned(),
                format!(
                    "service_manager.service:{}",
                    request.definition.service_name
                ),
            ],
        })
    }

    fn handoff(
        &mut self,
        request: ServiceManagerHandoffRequest<'_>,
    ) -> Result<ServiceManagerHandoffReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        self.candidate_generation = Some(request.plan.to_generation.as_str().to_owned());

        if !request.candidate_health.healthy {
            return Ok(ServiceManagerHandoffReport {
                plan_id: request.plan.plan_id.clone(),
                kind: ServiceManagerKind::Fake,
                service_name: request.definition.service_name.clone(),
                status: "blocked".to_owned(),
                handoff_executed: false,
                rollback_required: true,
                active_generation: request.plan.from_generation.as_str().to_owned(),
                previous_generation: request.plan.from_generation.as_str().to_owned(),
                release_ref: request.plan.from_release.clone(),
                audit: vec![
                    "service_manager.fake:candidate_started".to_owned(),
                    "service_manager.fake:candidate_health_failed".to_owned(),
                    format!(
                        "service_manager.health:{}",
                        request.candidate_health.message
                    ),
                ],
            });
        }

        self.active_generation = Some(request.plan.to_generation.as_str().to_owned());
        self.active_release = Some(request.plan.to_release.clone());
        self.candidate_generation = None;
        Ok(ServiceManagerHandoffReport {
            plan_id: request.plan.plan_id.clone(),
            kind: ServiceManagerKind::Fake,
            service_name: request.definition.service_name.clone(),
            status: "committed".to_owned(),
            handoff_executed: true,
            rollback_required: false,
            active_generation: request.plan.to_generation.as_str().to_owned(),
            previous_generation: request.plan.from_generation.as_str().to_owned(),
            release_ref: request.plan.to_release.clone(),
            audit: vec![
                "service_manager.fake:candidate_started".to_owned(),
                "service_manager.fake:candidate_health_passed".to_owned(),
                "service_manager.fake:handoff_committed".to_owned(),
            ],
        })
    }

    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        let reason = stable_non_empty(request.reason.to_owned(), "reason")?;
        self.active_generation = Some(request.plan.from_generation.as_str().to_owned());
        self.active_release = Some(request.plan.from_release.clone());
        self.candidate_generation = None;
        Ok(ServiceManagerRollbackReport {
            plan_id: request.plan.plan_id.clone(),
            kind: ServiceManagerKind::Fake,
            service_name: request.definition.service_name.clone(),
            status: "rolled_back".to_owned(),
            rollback_executed: true,
            active_generation: request.plan.from_generation.as_str().to_owned(),
            release_ref: request.plan.from_release.clone(),
            reason: reason.clone(),
            audit: vec![
                "service_manager.fake:rollback_committed".to_owned(),
                format!("service_manager.rollback.reason:{reason}"),
            ],
        })
    }
}

fn stable_non_empty(value: String, field: &'static str) -> Result<String, EvaError> {
    if value.trim().is_empty() {
        Err(
            EvaError::invalid_argument("service manager field cannot be empty")
                .with_context("field", field),
        )
    } else if value.trim() != value {
        Err(EvaError::invalid_argument(
            "service manager field cannot contain leading or trailing whitespace",
        )
        .with_context("field", field))
    } else if value.contains('\n') || value.contains('\r') {
        Err(
            EvaError::invalid_argument("service manager field cannot contain line breaks")
                .with_context("field", field),
        )
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::GenerationId;

    fn plan() -> UpgradeApplyPlan {
        UpgradeApplyPlan::new(
            "plan-service",
            GenerationId::parse("gen-v14").unwrap(),
            GenerationId::parse("gen-v15").unwrap(),
            "1.14.0",
            "1.15.0",
        )
        .unwrap()
    }

    #[test]
    fn fake_service_manager_handoff_and_rollback_are_auditable() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-dev").unwrap();
        let plan = plan();
        let mut adapter = FakeServiceManagerAdapter::with_active_generation("gen-v14", "1.14.0");

        let report = adapter
            .handoff(ServiceManagerHandoffRequest {
                definition: &definition,
                plan: &plan,
                candidate_health: RuntimeHealth::healthy(plan.to_generation.clone()),
            })
            .unwrap();

        assert_eq!(report.status, "committed");
        assert!(report.handoff_executed);
        assert!(!report.rollback_required);
        assert_eq!(report.active_generation, "gen-v15");
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "service_manager.fake:handoff_committed"));

        let rollback = adapter
            .rollback(ServiceManagerRollbackRequest {
                definition: &definition,
                plan: &plan,
                reason: "candidate validation failed after handoff",
            })
            .unwrap();

        assert_eq!(rollback.status, "rolled_back");
        assert!(rollback.rollback_executed);
        assert_eq!(rollback.active_generation, "gen-v14");
        assert!(rollback
            .audit
            .iter()
            .any(|entry| entry == "service_manager.fake:rollback_committed"));
    }

    #[test]
    fn fake_service_manager_blocks_failed_candidate_without_switching_active() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-dev").unwrap();
        let plan = plan();
        let mut adapter = FakeServiceManagerAdapter::with_active_generation("gen-v14", "1.14.0");

        let report = adapter
            .handoff(ServiceManagerHandoffRequest {
                definition: &definition,
                plan: &plan,
                candidate_health: RuntimeHealth {
                    generation_id: plan.to_generation.clone(),
                    healthy: false,
                    message: "health check failed".to_owned(),
                },
            })
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert!(!report.handoff_executed);
        assert!(report.rollback_required);
        assert_eq!(report.active_generation, "gen-v14");
    }

    #[test]
    fn fake_adapter_rejects_platform_service_manager_kind() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Systemd, "eva-prod").unwrap();
        let adapter = FakeServiceManagerAdapter::new();

        let error = adapter
            .inspect(ServiceManagerInspectRequest {
                definition: &definition,
            })
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unsupported);
    }
}
