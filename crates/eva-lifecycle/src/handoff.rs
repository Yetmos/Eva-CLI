//! 蓝绿 Supervisor 交接与发布指针变更。
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

/// 本模块的架构职责：约束蓝绿交接和发布指针写入的门禁与持久化顺序。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "blue-green supervisor handoff and release pointer mutation boundary";

/// 候选运行时二进制的可用性探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBinaryProbe {
    /// 被探测的二进制路径或测试标识。
    pub binary_path: String,
    /// 探测状态，当前就绪值为 `ready`。
    pub status: String,
    /// 探测来源与结果的审计记录。
    pub audit: Vec<String>,
}

/// 发布指针从旧代际切换到新代际的持久化描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePointerMutation {
    /// 相对于 Supervisor 状态存储根目录的指针路径。
    pub pointer_path: String,
    /// 切换前的活动代际。
    pub previous_generation: String,
    /// 切换后的活动代际。
    pub active_generation: String,
    /// 新活动代际对应的发布引用。
    pub release_ref: String,
    /// 指针变更状态。
    pub status: String,
    /// 策略放行和指针写入的审计记录。
    pub audit: Vec<String>,
}

/// 一次 Supervisor 蓝绿交接的完整结果与证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorHandoffReport {
    /// 对应的升级计划标识。
    pub plan_id: String,
    /// `committed` 或 `blocked` 交接状态。
    pub status: String,
    /// 所有交接门禁是否已通过。
    pub apply_allowed: bool,
    /// 是否已经执行发布指针等状态变更。
    pub mutation_executed: bool,
    /// 复用的升级应用锁证据。
    pub lock: UpgradeApplyLock,
    /// 候选运行时二进制探测结果。
    pub runtime_binary: RuntimeBinaryProbe,
    /// 报告结束时仍为活动状态的代际。
    pub active_generation: String,
    /// 交接前的活动代际。
    pub previous_generation: String,
    /// 报告结束时活动代际对应的发布引用。
    pub release_ref: String,
    /// 成功交接时写入的发布指针描述。
    pub release_pointer: Option<ReleasePointerMutation>,
    /// 交接被阻塞时生成但尚未执行的回滚计划。
    pub rollback_plan: Option<RollbackPlan>,
    /// 已执行或建议执行的有序步骤。
    pub steps: Vec<String>,
    /// 当前实现限制与失败风险。
    pub risks: Vec<String>,
    /// 策略、探测、排空和状态转换的聚合审计记录。
    pub audit: Vec<String>,
}

/// Supervisor 交接状态的持久化抽象。
///
/// 实现必须保留“准备记录 -> 发布指针 -> 提交记录”的调用顺序语义。接口本身不提供
/// 跨三个写入的事务，因此部分 I/O 失败可能留下可恢复的中间状态，读取方应结合
/// prepared/committed 记录判断是否完成。
pub trait SupervisorStateStore {
    /// 在任何发布指针写入前持久化交接意图和门禁结果。
    fn prepare_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError>;
    /// 在发布指针成功写入后持久化交接完成标记。
    fn commit_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError>;
    /// 写入新的活动代际发布指针。
    fn write_release_pointer(&mut self, mutation: &ReleasePointerMutation) -> Result<(), EvaError>;
}

/// 供测试和进程内编排使用的 Supervisor 状态存储。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemorySupervisorStateStore {
    /// 最近一次准备阶段报告。
    pub prepared: Option<SupervisorHandoffReport>,
    /// 最近一次提交阶段报告。
    pub committed: Option<SupervisorHandoffReport>,
    /// 最近一次发布指针变更。
    pub pointer: Option<ReleasePointerMutation>,
}

/// 将 Supervisor 状态写入指定目录的文件存储。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemSupervisorStateStore {
    /// prepared、committed 与发布指针文件的共同根目录。
    root: PathBuf,
}

/// 执行交接门禁、代际提升、排空和持久化的无状态协调器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SupervisorHandoffCoordinator;

/// 一次蓝绿 Supervisor 交接所需的全部门禁输入。
pub struct SupervisorHandoffRequest<'a> {
    /// 已校验的源、目标代际升级计划。
    pub plan: &'a UpgradeApplyPlan,
    /// 与计划关联的已获取升级锁。
    pub lock: UpgradeApplyLock,
    /// Supervisor 交接高风险动作的策略决策。
    pub supervisor_policy: &'a PolicyDecision,
    /// 发布指针变更高风险动作的独立策略决策。
    pub pointer_policy: &'a PolicyDecision,
    /// 候选运行时二进制探测结果。
    pub runtime_binary: RuntimeBinaryProbe,
    /// 候选代际健康探测结果。
    pub health: RuntimeHealth,
}

impl RuntimeBinaryProbe {
    /// 构造供本地烟雾测试使用的就绪探测结果。
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

    /// 构造明确不可用的二进制探测结果。
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

    /// 验证探测状态为就绪，否则返回带路径和状态的不可用错误。
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
    /// 创建以指定目录为持久化边界的状态存储。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 返回状态存储根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SupervisorStateStore for InMemorySupervisorStateStore {
    /// 保存准备阶段报告副本。
    fn prepare_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        self.prepared = Some(report.clone());
        Ok(())
    }

    /// 保存提交阶段报告副本。
    fn commit_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        self.committed = Some(report.clone());
        Ok(())
    }

    /// 保存发布指针变更副本。
    fn write_release_pointer(&mut self, mutation: &ReleasePointerMutation) -> Result<(), EvaError> {
        self.pointer = Some(mutation.clone());
        Ok(())
    }
}

impl SupervisorStateStore for FileSystemSupervisorStateStore {
    /// 创建状态目录并写入 `handoff.prepared` 恢复锚点。
    fn prepare_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create supervisor state store")
                .with_context("state_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        write_state_file(&self.root.join("handoff.prepared"), report_payload(report))
    }

    /// 创建状态目录并写入 `handoff.committed` 完成标记。
    fn commit_handoff(&mut self, report: &SupervisorHandoffReport) -> Result<(), EvaError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create supervisor state store")
                .with_context("state_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        write_state_file(&self.root.join("handoff.committed"), report_payload(report))
    }

    /// 在状态根目录下写入发布指针载荷。
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
    /// 按严格门禁顺序执行蓝绿交接并持久化证据。
    ///
    /// 顺序为：两项策略均放行、二进制可用、启动候选、健康通过、提升候选、确认旧
    /// 代际排空，最后才构造并写入发布指针。二进制或健康门禁失败会保存 blocked
    /// 准备报告并返回回滚计划，但不会写指针。成功路径按 prepared、pointer、committed
    /// 写入；存储接口不提供跨文件原子事务，所以中途 I/O 失败会返回错误并保留前序
    /// 证据，恢复方必须据此完成或回退，不能把 prepared 当作已提交。
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
        // 策略检查必须先于二进制探测和任何候选代际状态变更。
        supervisor_policy.ensure_allowed()?;
        pointer_policy.ensure_allowed()?;

        // 二进制不可用时尚未启动候选，保持旧代际和发布指针完全不变。
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

        // 候选启动后仍先验证健康；失败只落准备证据，不进入指针写入阶段。
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

        // 活动代际提升后，必须确认旧代际完成排空才允许发布指针指向新代际。
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

        // 顺序本身构成崩溃恢复协议：准备文件先落盘，提交文件最后落盘。
        store.prepare_handoff(&report)?;
        store.write_release_pointer(&mutation)?;
        store.commit_handoff(&report)?;
        Ok(report)
    }
}

/// 构造不执行任何发布指针变更的阻塞交接报告。
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

/// 交接在发布指针写入前被阻塞的原因类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockedHandoffReason {
    /// 候选二进制在启动前不可用。
    RuntimeBinaryUnavailable,
    /// 候选已经启动但未通过健康检查。
    CandidateHealthFailed,
}

impl BlockedHandoffReason {
    /// 返回与阻塞阶段对应的稳定审计标记。
    fn audit_marker(self) -> String {
        match self {
            Self::RuntimeBinaryUnavailable => {
                "supervisor.handoff:runtime_binary_unavailable".to_owned()
            }
            Self::CandidateHealthFailed => "supervisor.handoff:candidate_health_failed".to_owned(),
        }
    }

    /// 返回该阻塞原因下已完成和后续所需的步骤。
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

    /// 返回该阻塞原因对应的风险说明。
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

/// 将状态载荷写入目标文件，并附加完整 I/O 错误上下文。
///
/// 当前实现以 truncate 后覆盖写入单个文件，不承诺写入过程对崩溃原子；上层通过
/// prepared/committed 两阶段文件和固定写序恢复。不得把本辅助函数单独视为事务提交。
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

/// 将交接报告的关键恢复字段编码为稳定行式载荷。
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

/// 将发布指针变更编码为稳定行式载荷。
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
/// 交接门禁、失败回滚与文件持久化顺序测试。
mod tests {
    use super::*;
    use crate::{InMemoryUpgradeApplyLockStore, UpgradeApplyCoordinator};
    use eva_core::GenerationId;
    use eva_policy::{HighRiskAction, PolicyDecision};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 构造有效的固定升级计划。
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

    /// 为测试计划获取内存升级锁。
    fn lock(plan: &UpgradeApplyPlan) -> UpgradeApplyLock {
        let mut locks = InMemoryUpgradeApplyLockStore::new();
        UpgradeApplyCoordinator
            .acquire_lock(&mut locks, plan, "cli")
            .unwrap()
            .lock
    }

    /// 构造指定高风险动作已放行的测试策略决策。
    fn allowed(action: HighRiskAction) -> PolicyDecision {
        PolicyDecision {
            action,
            allowed: true,
            reason: "allowed by test".to_owned(),
            audit: vec![format!("runtime:{}:allowed", action.as_str())],
        }
    }

    /// 构造指定高风险动作被拒绝的测试策略决策。
    fn denied(action: HighRiskAction) -> PolicyDecision {
        PolicyDecision {
            action,
            allowed: false,
            reason: "denied by test".to_owned(),
            audit: vec![format!("runtime:{}:denied", action.as_str())],
        }
    }

    /// 创建进程与时间戳隔离的临时状态目录。
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
    /// 验证策略、锁、健康和排空均通过后才提交发布指针。
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
    /// 验证发布指针策略拒绝时不发生任何指针变更。
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
    /// 验证候选健康失败会阻止指针并返回回滚计划。
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
    /// 验证文件存储持久化准备、指针和提交三类状态。
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
