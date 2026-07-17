use crate::HighRiskAction;
use eva_core::EvaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MutationOperation {
    DaemonShutdown,
    DaemonTaskSubmit,
    DaemonTaskCancel,
    DaemonAgentDrain,
    DaemonAgentReload,
    RestoreApply,
    RestoreRollback,
    UpgradeHandoff,
    ReleasePointerPromote,
    HardwareBind,
    ServiceInstall,
    ServiceStart,
    ServiceStop,
    ServiceRestart,
    MemoryMaintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationGate {
    AuthenticatedDaemon,
    HighRisk(HighRiskAction),
    ServiceManager,
    MemoryPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationInventoryEntry {
    pub operation: MutationOperation,
    pub stable_name: &'static str,
    pub gate: MutationGate,
}

pub const MUTATION_INVENTORY: &[MutationInventoryEntry] = &[
    entry(
        MutationOperation::DaemonShutdown,
        "daemon.shutdown",
        MutationGate::AuthenticatedDaemon,
    ),
    entry(
        MutationOperation::DaemonTaskSubmit,
        "daemon.task_submit",
        MutationGate::AuthenticatedDaemon,
    ),
    entry(
        MutationOperation::DaemonTaskCancel,
        "daemon.task_cancel",
        MutationGate::AuthenticatedDaemon,
    ),
    entry(
        MutationOperation::DaemonAgentDrain,
        "daemon.agent_drain",
        MutationGate::AuthenticatedDaemon,
    ),
    entry(
        MutationOperation::DaemonAgentReload,
        "daemon.agent_reload",
        MutationGate::AuthenticatedDaemon,
    ),
    entry(
        MutationOperation::RestoreApply,
        "restore.apply",
        MutationGate::HighRisk(HighRiskAction::RestoreApply),
    ),
    entry(
        MutationOperation::RestoreRollback,
        "restore.rollback",
        MutationGate::HighRisk(HighRiskAction::RestoreApply),
    ),
    entry(
        MutationOperation::UpgradeHandoff,
        "upgrade.handoff",
        MutationGate::HighRisk(HighRiskAction::SupervisorHandoff),
    ),
    entry(
        MutationOperation::ReleasePointerPromote,
        "release.pointer_promote",
        MutationGate::HighRisk(HighRiskAction::ReleasePointerMutation),
    ),
    entry(
        MutationOperation::HardwareBind,
        "hardware.bind",
        MutationGate::HighRisk(HighRiskAction::HardwareBind),
    ),
    entry(
        MutationOperation::ServiceInstall,
        "service.install",
        MutationGate::ServiceManager,
    ),
    entry(
        MutationOperation::ServiceStart,
        "service.start",
        MutationGate::ServiceManager,
    ),
    entry(
        MutationOperation::ServiceStop,
        "service.stop",
        MutationGate::ServiceManager,
    ),
    entry(
        MutationOperation::ServiceRestart,
        "service.restart",
        MutationGate::ServiceManager,
    ),
    entry(
        MutationOperation::MemoryMaintenance,
        "memory.maintenance",
        MutationGate::MemoryPolicy,
    ),
];

const fn entry(
    operation: MutationOperation,
    stable_name: &'static str,
    gate: MutationGate,
) -> MutationInventoryEntry {
    MutationInventoryEntry {
        operation,
        stable_name,
        gate,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationDecision {
    pub operation: MutationOperation,
    pub allowed: bool,
    pub reason: String,
    pub audit: Vec<String>,
}

impl MutationDecision {
    pub fn authenticated_daemon(operation: MutationOperation, authenticated: bool) -> Self {
        let registered = MUTATION_INVENTORY
            .iter()
            .any(|e| e.operation == operation && e.gate == MutationGate::AuthenticatedDaemon);
        let allowed = authenticated && registered;
        Self {
            operation,
            allowed,
            reason: if allowed {
                "authenticated daemon mutation gate allowed"
            } else {
                "daemon mutation requires authenticated active lease ownership"
            }
            .into(),
            audit: vec![format!(
                "mutation.policy:{}:{}",
                stable_name(operation),
                if allowed { "allow" } else { "deny" }
            )],
        }
    }
    pub fn ensure_allowed(&self) -> Result<(), EvaError> {
        if self.allowed {
            Ok(())
        } else {
            Err(
                EvaError::permission_denied("mutation policy denied operation")
                    .with_context("operation", stable_name(self.operation))
                    .with_context("reason", &self.reason),
            )
        }
    }
}

pub fn stable_name(operation: MutationOperation) -> &'static str {
    MUTATION_INVENTORY
        .iter()
        .find(|e| e.operation == operation)
        .map(|e| e.stable_name)
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[test]
    fn inventory_is_bidirectional_unique_and_every_operation_has_a_gate() {
        let operations = MUTATION_INVENTORY
            .iter()
            .map(|e| e.operation)
            .collect::<BTreeSet<_>>();
        let names = MUTATION_INVENTORY
            .iter()
            .map(|e| e.stable_name)
            .collect::<BTreeSet<_>>();
        assert_eq!(operations.len(), MUTATION_INVENTORY.len());
        assert_eq!(names.len(), MUTATION_INVENTORY.len());
        for op in [
            MutationOperation::DaemonShutdown,
            MutationOperation::DaemonTaskSubmit,
            MutationOperation::DaemonTaskCancel,
            MutationOperation::DaemonAgentDrain,
            MutationOperation::DaemonAgentReload,
            MutationOperation::RestoreApply,
            MutationOperation::RestoreRollback,
            MutationOperation::UpgradeHandoff,
            MutationOperation::ReleasePointerPromote,
            MutationOperation::HardwareBind,
            MutationOperation::ServiceInstall,
            MutationOperation::ServiceStart,
            MutationOperation::ServiceStop,
            MutationOperation::ServiceRestart,
            MutationOperation::MemoryMaintenance,
        ] {
            assert!(operations.contains(&op));
        }
    }
    #[test]
    fn unauthenticated_daemon_decision_denies() {
        assert!(
            MutationDecision::authenticated_daemon(MutationOperation::DaemonTaskSubmit, false)
                .ensure_allowed()
                .is_err()
        );
    }
}
