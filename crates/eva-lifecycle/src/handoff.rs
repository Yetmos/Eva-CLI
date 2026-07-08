//! Blue-green supervisor handoff and release pointer mutation.

use crate::{
    DrainCoordinator, DrainStatus, GenerationState, InMemorySupervisor, RollbackCoordinator,
    RollbackPlan, RuntimeGeneration, RuntimeHealth, UpgradeApplyLock, UpgradeApplyPlan,
};
use eva_core::EvaError;
use eva_policy::PolicyDecision;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "blue-green supervisor handoff and release pointer mutation boundary";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBinaryProbe {
    pub binary_path: String,
    pub status: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePointerMutation {
    pub pointer_path: String,
    pub previous_generation: String,
    pub active_generation: String,
    pub release_ref: String,
    pub status: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorHandoffReport {
    pub plan_id: String,
    pub status: String,
    pub apply_allowed: bool,
    pub mutation_executed: bool,
    pub lock: UpgradeApplyLock,
    pub runtime_binary: RuntimeBinaryProbe,
    pub active_generation: String,
    pub previous_generation: String,
    pub release_ref: String,
    pub release_pointer: Option<ReleasePointerMutation>,
    pub rollback_plan: Option<RollbackPlan>,
    pub steps: Vec<String>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

pub trait SupervisorStateStore {
    fn prepare_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError>;
    fn commit_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError>;
    fn write_release_pointer(&mut self, mutation: &ReleasePointerMutation) -> Result<(), EvaError>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemorySupervisorStateStore {
    pub prepared: Option<SupervisorHandoffReport>,
    pub committed: Option<SupervisorHandoffReport>,
    pub pointer: Option<ReleasePointerMutation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemSupervisorStateStore {
    root: PathBuf,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SupervisorHandoffCoordinator;

pub struct SupervisorHandoffRequest<'a> {
    pub plan: &'a UpgradeApplyPlan,
    pub lock: UpgradeApplyLock,
    pub supervisor_policy: &'a PolicyDecision,
    pub pointer_policy: &'a PolicyDecision,
    pub runtime_binary: RuntimeBinaryProbe,
    pub health: RuntimeHealth,
}

impl RuntimeBinaryProbe {
    pub fn simulated(binary_path: impl Into<String>) -> Self {
        let binary_path = binary_path.into();
        Self {
            binary_path: binary_path.clone(),
            status: "ready".to_owned(),
            audit: vec![
                "runtime.binary:simulated".to_owned(),
                format!("runtime.binary:{binary_path}"),
            ],
        }
    }

    pub fn unavailable(binary_path: impl Into<String>) -> Self {
        let binary_path = binary_path.into();
        Self {
            binary_path: binary_path.clone(),
            status: "unavailable".to_owned(),
            audit: vec![
                "runtime.binary:unavailable".to_owned(),
                format!("runtime.binary:{binary_path}"),
            ],
        }
    }

    pub fn ensure_ready(&self) -> Result<(), EvaError> {
        if self.status == "ready" {
            Ok(())
        } else {
            Err(EvaError::unavailable("runtime binary probe failed")
                .with_context("runtime_binary", &self.binary_path)
                .with_context("status", &self.status))
        }
    }
}

impl FileSystemSupervisorStateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SupervisorStateStore for InMemorySupervisorStateStore {
    fn prepare_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        self.prepared = Some(report.clone());
        Ok(())
    }

    fn commit_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        self.committed = Some(report.clone());
        Ok(())
    }

    fn write_release_pointer(&mut self, mutation: &ReleasePointerMutation) -> Result<(), EvaError> {
        self.pointer = Some(mutation.clone());
        Ok(())
    }
}

impl SupervisorStateStore for FileSystemSupervisorStateStore {
    fn prepare_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create supervisor state store")
                .with_context("state_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        write_state_file(&self.root.join("handoff.prepared"), report_payload(report))
    }

    fn commit_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create supervisor state store")
                .with_context("state_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        write_state_file(&self.root.join("handoff.committed"), report_payload(report))
    }

    fn write_release_pointer(&mut self, mutation: &ReleasePointerMutation) -> Result<(), EvaError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create supervisor state store")
                .with_context("state_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        write_state_file(
            &self.root.join(&mutation.pointer_path),
            pointer_payload(mutation),
        )
    }
}

impl SupervisorHandoffCoordinator {
    pub fn handoff<S: SupervisorStateStore>(
        &self,
        store: &mut S,
        request: SupervisorHandoffRequest<'_>,
    ) -> Result<SupervisorHandoffReport, EvaError> {
        let SupervisorHandoffRequest {
            plan,
            lock,
            supervisor_policy,
            pointer_policy,
            runtime_binary,
            health,
        } = request;
        supervisor_policy.ensure_allowed()?;
        pointer_policy.ensure_allowed()?;

        if runtime_binary.ensure_ready().is_err() {
            let rollback = RollbackCoordinator.plan_generation_lifecycle_rollback(
                plan.to_generation.clone(),
                plan.from_generation.clone(),
                "runtime binary probe failed",
                None,
                None,
            )?;
            let report = blocked_report(
                plan,
                lock,
                runtime_binary,
                rollback,
                supervisor_policy,
                pointer_policy,
                BlockedHandoffReason::RuntimeBinaryUnavailable,
            );
            store.prepare_handoff(&report)?;
            return Ok(report);
        }

        let active = RuntimeGeneration::new(
            plan.from_generation.clone(),
            plan.from_release.clone(),
            GenerationState::Active,
        )?;
        let mut supervisor = InMemorySupervisor::new(active)?;
        supervisor.start_candidate(plan.to_generation.clone(), plan.to_release.clone())?;

        if !health.healthy {
            let rollback = RollbackCoordinator.plan_generation_lifecycle_rollback(
                plan.to_generation.clone(),
                plan.from_generation.clone(),
                health.message.clone(),
                None,
                None,
            )?;
            let report = blocked_report(
                plan,
                lock,
                runtime_binary,
                rollback,
                supervisor_policy,
                pointer_policy,
                BlockedHandoffReason::CandidateHealthFailed,
            );
            store.prepare_handoff(&report)?;
            return Ok(report);
        }

        supervisor.commit_candidate(health)?;
        let drain = DrainCoordinator.plan_generation_swap_drain(
            plan.from_generation.clone(),
            plan.to_generation.clone(),
            0,
            30_000,
        )?;
        if drain.plan.status != DrainStatus::Completed {
            return Err(
                EvaError::unavailable("old generation drain did not complete")
                    .with_context("generation", plan.from_generation.as_str())
                    .with_context("status", drain.plan.status.as_str()),
            );
        }

        let mutation = ReleasePointerMutation {
            pointer_path: "state/release-pointer".to_owned(),
            previous_generation: plan.from_generation.as_str().to_owned(),
            active_generation: plan.to_generation.as_str().to_owned(),
            release_ref: plan.to_release.clone(),
            status: "committed".to_owned(),
            audit: vec![
                "release.pointer:policy_allowed".to_owned(),
                "release.pointer:written".to_owned(),
                format!("from:{}", plan.from_generation.as_str()),
                format!("to:{}", plan.to_generation.as_str()),
            ],
        };

        let mut audit = vec![
            "supervisor.handoff:plan_parsed".to_owned(),
            "supervisor.handoff:policy_allowed".to_owned(),
            "release.pointer:policy_allowed".to_owned(),
            "supervisor.handoff:lock_reused".to_owned(),
            "supervisor.handoff:candidate_started".to_owned(),
            "supervisor.handoff:health_check_passed".to_owned(),
            "supervisor.handoff:old_generation_drained".to_owned(),
            "release.pointer:committed".to_owned(),
        ];
        audit.extend(
            supervisor_policy
                .audit
                .iter()
                .map(|entry| format!("policy:{entry}")),
        );
        audit.extend(
            pointer_policy
                .audit
                .iter()
                .map(|entry| format!("policy:{entry}")),
        );
        audit.extend(runtime_binary.audit.iter().cloned());
        audit.extend(drain.audit.iter().cloned());
        audit.extend(supervisor.report().audit.iter().cloned());

        let report = SupervisorHandoffReport {
            plan_id: plan.plan_id.clone(),
            status: "committed".to_owned(),
            apply_allowed: true,
            mutation_executed: true,
            lock,
            runtime_binary,
            active_generation: plan.to_generation.as_str().to_owned(),
            previous_generation: plan.from_generation.as_str().to_owned(),
            release_ref: plan.to_release.clone(),
            release_pointer: Some(mutation.clone()),
            rollback_plan: None,
            steps: vec![
                "verify supervisor and release pointer policies".to_owned(),
                "probe candidate runtime binary".to_owned(),
                "start candidate runtime generation".to_owned(),
                "commit healthy candidate generation".to_owned(),
                "drain previous active generation".to_owned(),
                "write release pointer".to_owned(),
                "persist supervisor handoff state".to_owned(),
            ],
            risks: vec![
                "V1.10.5 uses a local supervisor adapter smoke rather than a daemonized service manager".to_owned(),
                "release pointer mutation is scoped to the configured supervisor state store".to_owned(),
            ],
            audit,
        };

        store.prepare_handoff(&report)?;
        store.write_release_pointer(&mutation)?;
        store.commit_handoff(&report)?;
        Ok(report)
    }
}

fn blocked_report(
    plan: &UpgradeApplyPlan,
    lock: UpgradeApplyLock,
    runtime_binary: RuntimeBinaryProbe,
    rollback: RollbackPlan,
    supervisor_policy: &PolicyDecision,
    pointer_policy: &PolicyDecision,
    reason: BlockedHandoffReason,
) -> SupervisorHandoffReport {
    let mut audit = vec![
        "supervisor.handoff:plan_parsed".to_owned(),
        "supervisor.handoff:policy_allowed".to_owned(),
        "release.pointer:policy_allowed".to_owned(),
        reason.audit_marker(),
        "supervisor.handoff:rollback_required".to_owned(),
    ];
    audit.extend(
        supervisor_policy
            .audit
            .iter()
            .map(|entry| format!("policy:{entry}")),
    );
    audit.extend(
        pointer_policy
            .audit
            .iter()
            .map(|entry| format!("policy:{entry}")),
    );
    audit.extend(runtime_binary.audit.iter().cloned());
    SupervisorHandoffReport {
        plan_id: plan.plan_id.clone(),
        status: "blocked".to_owned(),
        apply_allowed: false,
        mutation_executed: false,
        lock,
        runtime_binary,
        active_generation: plan.from_generation.as_str().to_owned(),
        previous_generation: plan.from_generation.as_str().to_owned(),
        release_ref: plan.from_release.clone(),
        release_pointer: None,
        rollback_plan: Some(rollback),
        steps: reason.steps(),
        risks: reason.risks(),
        audit,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockedHandoffReason {
    RuntimeBinaryUnavailable,
    CandidateHealthFailed,
}

impl BlockedHandoffReason {
    fn audit_marker(self) -> String {
        match self {
            Self::RuntimeBinaryUnavailable => {
                "supervisor.handoff:runtime_binary_unavailable".to_owned()
            }
            Self::CandidateHealthFailed => "supervisor.handoff:candidate_health_failed".to_owned(),
        }
    }

    fn steps(self) -> Vec<String> {
        match self {
            Self::RuntimeBinaryUnavailable => vec![
                "verify supervisor and release pointer policies".to_owned(),
                "probe candidate runtime binary".to_owned(),
                "block handoff before candidate start".to_owned(),
                "emit rollback plan".to_owned(),
            ],
            Self::CandidateHealthFailed => vec![
                "verify supervisor and release pointer policies".to_owned(),
                "probe candidate runtime binary".to_owned(),
                "start candidate runtime generation".to_owned(),
                "block handoff on candidate health failure".to_owned(),
                "emit rollback plan".to_owned(),
            ],
        }
    }

    fn risks(self) -> Vec<String> {
        match self {
            Self::RuntimeBinaryUnavailable => vec![
                "candidate runtime binary was unavailable before handoff".to_owned(),
                "previous generation remains active".to_owned(),
            ],
            Self::CandidateHealthFailed => vec![
                "candidate runtime health failed before release pointer mutation".to_owned(),
                "previous generation remains active".to_owned(),
            ],
        }
    }
}

fn write_state_file(path: &Path, payload: String) -> Result<(), EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            EvaError::internal("failed to create supervisor state directory")
                .with_context("path", parent.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            EvaError::internal("failed to open supervisor state file")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    file.write_all(payload.as_bytes()).map_err(|error| {
        EvaError::internal("failed to write supervisor state file")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

fn report_payload(report: &SupervisorHandoffReport) -> String {
    format!(
        "plan_id={}\nstatus={}\napply_allowed={}\nmutation_executed={}\nactive_generation={}\nprevious_generation={}\nrelease_ref={}\nlock_id={}\nrollback={}\n",
        report.plan_id,
        report.status,
        report.apply_allowed,
        report.mutation_executed,
        report.active_generation,
        report.previous_generation,
        report.release_ref,
        report.lock.lock_id,
        report
            .rollback_plan
            .as_ref()
            .map(|rollback| rollback.status.as_str())
            .unwrap_or("none")
    )
}

fn pointer_payload(mutation: &ReleasePointerMutation) -> String {
    format!(
        "active_generation={}\nprevious_generation={}\nrelease_ref={}\nstatus={}\n",
        mutation.active_generation,
        mutation.previous_generation,
        mutation.release_ref,
        mutation.status
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryUpgradeApplyLockStore, UpgradeApplyCoordinator};
    use eva_core::GenerationId;
    use eva_policy::{HighRiskAction, PolicyDecision};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn plan() -> UpgradeApplyPlan {
        UpgradeApplyPlan::new(
            "plan-1",
            GenerationId::parse("gen-v14").unwrap(),
            GenerationId::parse("gen-v15").unwrap(),
            "1.4.0",
            "1.5.0",
        )
        .unwrap()
    }

    fn lock(plan: &UpgradeApplyPlan) -> UpgradeApplyLock {
        let mut locks = InMemoryUpgradeApplyLockStore::new();
        UpgradeApplyCoordinator
            .acquire_lock(&mut locks, plan, "cli")
            .unwrap()
            .lock
    }

    fn allowed(action: HighRiskAction) -> PolicyDecision {
        PolicyDecision {
            action,
            allowed: true,
            reason: "allowed by test".to_owned(),
            audit: vec![format!("runtime:{}:allowed", action.as_str())],
        }
    }

    fn denied(action: HighRiskAction) -> PolicyDecision {
        PolicyDecision {
            action,
            allowed: false,
            reason: "denied by test".to_owned(),
            audit: vec![format!("runtime:{}:denied", action.as_str())],
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eva-lifecycle-handoff-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn handoff_commits_release_pointer_after_policy_lock_health_and_drain() {
        let plan = plan();
        let mut store = InMemorySupervisorStateStore::default();
        let supervisor_policy = allowed(HighRiskAction::SupervisorHandoff);
        let pointer_policy = allowed(HighRiskAction::ReleasePointerMutation);

        let report = SupervisorHandoffCoordinator
            .handoff(
                &mut store,
                SupervisorHandoffRequest {
                    plan: &plan,
                    lock: lock(&plan),
                    supervisor_policy: &supervisor_policy,
                    pointer_policy: &pointer_policy,
                    runtime_binary: RuntimeBinaryProbe::simulated("target/debug/eva"),
                    health: RuntimeHealth::healthy(plan.to_generation.clone()),
                },
            )
            .unwrap();

        assert_eq!(report.status, "committed");
        assert!(report.apply_allowed);
        assert!(report.mutation_executed);
        assert_eq!(store.pointer.unwrap().active_generation, "gen-v15");
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "release.pointer:committed"));
    }

    #[test]
    fn handoff_requires_pointer_policy_before_mutation() {
        let plan = plan();
        let mut store = InMemorySupervisorStateStore::default();
        let supervisor_policy = allowed(HighRiskAction::SupervisorHandoff);
        let pointer_policy = denied(HighRiskAction::ReleasePointerMutation);

        let error = SupervisorHandoffCoordinator
            .handoff(
                &mut store,
                SupervisorHandoffRequest {
                    plan: &plan,
                    lock: lock(&plan),
                    supervisor_policy: &supervisor_policy,
                    pointer_policy: &pointer_policy,
                    runtime_binary: RuntimeBinaryProbe::simulated("target/debug/eva"),
                    health: RuntimeHealth::healthy(plan.to_generation.clone()),
                },
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
        assert!(store.pointer.is_none());
    }

    #[test]
    fn handoff_health_failure_blocks_pointer_and_returns_rollback() {
        let plan = plan();
        let mut store = InMemorySupervisorStateStore::default();
        let supervisor_policy = allowed(HighRiskAction::SupervisorHandoff);
        let pointer_policy = allowed(HighRiskAction::ReleasePointerMutation);

        let report = SupervisorHandoffCoordinator
            .handoff(
                &mut store,
                SupervisorHandoffRequest {
                    plan: &plan,
                    lock: lock(&plan),
                    supervisor_policy: &supervisor_policy,
                    pointer_policy: &pointer_policy,
                    runtime_binary: RuntimeBinaryProbe::simulated("target/debug/eva"),
                    health: RuntimeHealth {
                        generation_id: plan.to_generation.clone(),
                        healthy: false,
                        message: "candidate failed smoke".to_owned(),
                    },
                },
            )
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert!(!report.mutation_executed);
        assert!(report.rollback_plan.is_some());
        assert!(store.pointer.is_none());
    }

    #[test]
    fn filesystem_store_persists_handoff_state_and_pointer() {
        let plan = plan();
        let root = temp_dir("fs");
        let mut store = FileSystemSupervisorStateStore::new(&root);
        let supervisor_policy = allowed(HighRiskAction::SupervisorHandoff);
        let pointer_policy = allowed(HighRiskAction::ReleasePointerMutation);

        let report = SupervisorHandoffCoordinator
            .handoff(
                &mut store,
                SupervisorHandoffRequest {
                    plan: &plan,
                    lock: lock(&plan),
                    supervisor_policy: &supervisor_policy,
                    pointer_policy: &pointer_policy,
                    runtime_binary: RuntimeBinaryProbe::simulated("target/debug/eva"),
                    health: RuntimeHealth::healthy(plan.to_generation.clone()),
                },
            )
            .unwrap();

        assert_eq!(report.status, "committed");
        assert!(root.join("handoff.prepared").exists());
        assert!(root.join("handoff.committed").exists());
        assert!(root.join("state/release-pointer").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
