//! 运行时生命周期与代际管理边界。
//! Runtime lifecycle and generation management boundary.

/// 升级应用锁的获取与持久化。
pub mod apply_lock;
/// 旧运行时代际的排空计划与证据。
pub mod drain;
/// 运行时代际状态机。
pub mod generation;
/// 蓝绿 Supervisor 交接与发布指针写入。
pub mod handoff;
/// 失败交接后的回滚规划。
pub mod rollback;
/// Non-shell service-manager command execution and bounded evidence.
pub mod service_command;
/// Host-kind validation and bound service command execution.
pub mod service_factory;
/// 操作系统服务管理器抽象与测试适配器。
pub mod service_manager;
/// 进程内 Supervisor 所有权模型。
pub mod supervisor;

pub use apply_lock::{
    FileSystemUpgradeApplyLockStore, InMemoryUpgradeApplyLockStore, UpgradeApplyCoordinator,
    UpgradeApplyLock, UpgradeApplyPlan, UpgradeApplyReport,
};
pub use drain::{DrainCoordinator, DrainPlan, DrainStatus, GenerationDrainEvidence};
pub use generation::{GenerationController, GenerationState, RuntimeGeneration};
pub use handoff::{
    FileSystemSupervisorStateStore, InMemorySupervisorStateStore, ReleasePointerMutation,
    RuntimeBinaryProbe, SupervisorHandoffCoordinator, SupervisorHandoffReport,
    SupervisorHandoffRequest, SupervisorStateStore,
};
pub use rollback::{RollbackCoordinator, RollbackPlan};
pub use service_command::{
    ProcessServiceCommandExecutor, ServiceCommand, ServiceCommandArg, ServiceCommandArgVisibility,
    ServiceCommandExecution, ServiceCommandExecutor, ServiceCommandLimits, ServiceCommandReport,
    ServiceCommandStream, ServiceCommandTermination, ValidatedServiceCommandTarget,
    DEFAULT_SERVICE_COMMAND_OUTPUT_LIMIT_BYTES, DEFAULT_SERVICE_COMMAND_TIMEOUT,
};
pub use service_factory::{
    HostBoundServiceCommandExecutor, ServiceHostPlatform, ServiceManagerFactory,
};
pub use service_manager::{
    FakeServiceManagerAdapter, ServiceManagerAdapter, ServiceManagerDefinition,
    ServiceManagerHandoffReport, ServiceManagerHandoffRequest, ServiceManagerInspectRequest,
    ServiceManagerKind, ServiceManagerMutationReport, ServiceManagerMutationRequest,
    ServiceManagerOperation, ServiceManagerRollbackReport, ServiceManagerRollbackRequest,
    ServiceManagerState, ServiceManagerStatusReport, ServiceManagerStatusRequest,
};
pub use supervisor::{InMemorySupervisor, RuntimeHealth, SupervisorReport};
