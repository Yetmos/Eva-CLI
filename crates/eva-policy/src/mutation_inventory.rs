//! 中文：集中登记会改变外部状态的操作，以及每种操作必须通过的安全门禁。

use crate::HighRiskAction;
use eva_core::EvaError;

/// 中文：系统内所有受策略约束的状态修改操作；新增写路径时必须同步登记门禁。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MutationOperation {
    /// 中文：关闭守护进程。
    DaemonShutdown,
    /// 中文：向守护进程提交任务。
    DaemonTaskSubmit,
    /// 中文：取消守护进程中的任务。
    DaemonTaskCancel,
    /// 中文：让 Agent 进入排空状态。
    DaemonAgentDrain,
    /// 中文：重新加载 Agent。
    DaemonAgentReload,
    /// 中文：应用恢复计划。
    RestoreApply,
    /// 中文：回滚恢复操作。
    RestoreRollback,
    /// 中文：把运行控制权移交给新版本。
    UpgradeHandoff,
    /// 中文：推进当前发布指针。
    ReleasePointerPromote,
    /// 中文：绑定硬件设备。
    HardwareBind,
    /// 中文：安装系统服务。
    ServiceInstall,
    /// 中文：启动系统服务。
    ServiceStart,
    /// 中文：停止系统服务。
    ServiceStop,
    /// 中文：重启系统服务。
    ServiceRestart,
    /// 中文：卸载系统服务。
    ServiceUninstall,
    /// 中文：执行记忆存储维护。
    MemoryMaintenance,
}

/// 中文：状态修改操作必须通过的授权边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationGate {
    /// 中文：要求调用方持有已认证的活动守护进程租约。
    AuthenticatedDaemon,
    /// 中文：要求运行时策略显式允许对应高风险动作。
    HighRisk(HighRiskAction),
    /// 中文：要求系统服务管理器已配置且当前宿主适配器获准。
    ServiceManager,
    /// 中文：要求记忆子系统自身的维护策略允许。
    MemoryPolicy,
}

/// 中文：一个写操作在集中清单中的稳定名称与门禁映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationInventoryEntry {
    /// 中文：类型安全的写操作标识。
    pub operation: MutationOperation,
    /// 中文：用于配置、审计和错误上下文的稳定名称。
    pub stable_name: &'static str,
    /// 中文：执行操作前必须满足的门禁。
    pub gate: MutationGate,
}

/// 中文：所有已知写操作的唯一登记表，也是稳定名称查询和门禁校验的事实来源。
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
        MutationOperation::ServiceUninstall,
        "service.uninstall",
        MutationGate::ServiceManager,
    ),
    entry(
        MutationOperation::MemoryMaintenance,
        "memory.maintenance",
        MutationGate::MemoryPolicy,
    ),
];

/// 中文：在常量上下文中构造清单条目，避免重复结构体初始化样板。
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

/// 中文：一次写操作策略判定的结果及其可持久化审计证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationDecision {
    /// 中文：被判定的写操作。
    pub operation: MutationOperation,
    /// 中文：门禁是否允许继续执行。
    pub allowed: bool,
    /// 中文：面向操作员的确定性判定原因。
    pub reason: String,
    /// 中文：不含敏感数据的稳定审计记录。
    pub audit: Vec<String>,
}

impl MutationDecision {
    /// 中文：仅当操作登记为守护进程门禁且调用方已认证时允许执行。
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

    /// 中文：同时校验清单登记、服务管理配置和宿主适配器许可，三者全部满足才允许操作。
    /// Evaluates a service-manager mutation after the caller has validated the
    /// host adapter (or explicitly selected the development fake adapter).
    pub fn service_manager(
        operation: MutationOperation,
        configured: bool,
        adapter_allowed: bool,
    ) -> Self {
        let registered = MUTATION_INVENTORY.iter().any(|entry| {
            entry.operation == operation && entry.gate == MutationGate::ServiceManager
        });
        let allowed = registered && configured && adapter_allowed;
        let reason = if !registered {
            "service mutation is not registered in the mutation inventory"
        } else if !configured {
            "service manager configuration is disabled"
        } else if !adapter_allowed {
            "service manager adapter is not allowed for this host or mode"
        } else {
            "service manager mutation gate allowed"
        };
        Self {
            operation,
            allowed,
            reason: reason.to_owned(),
            audit: vec![format!(
                "mutation.policy:{}:{}",
                stable_name(operation),
                if allowed { "allow" } else { "deny" }
            )],
        }
    }
    /// 中文：把判定转换为控制流结果；拒绝时保留操作稳定名称和具体原因。
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

/// 中文：返回操作的审计稳定名称；未登记值使用防御性的 `unknown` 占位符。
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
            MutationOperation::ServiceUninstall,
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

    #[test]
    fn service_manager_decision_requires_inventory_config_and_adapter_gate() {
        assert!(
            MutationDecision::service_manager(MutationOperation::ServiceInstall, true, true)
                .ensure_allowed()
                .is_ok()
        );
        for decision in [
            MutationDecision::service_manager(MutationOperation::ServiceInstall, false, true),
            MutationDecision::service_manager(MutationOperation::ServiceInstall, true, false),
            MutationDecision::service_manager(MutationOperation::DaemonShutdown, true, true),
        ] {
            assert!(!decision.allowed);
            assert!(decision.ensure_allowed().is_err());
        }
    }
}
