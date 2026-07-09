//! Runtime lifecycle and generation management boundary.

pub mod apply_lock;
pub mod drain;
pub mod generation;
pub mod handoff;
pub mod rollback;
pub mod service_manager;
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
pub use service_manager::{
    FakeServiceManagerAdapter, ServiceManagerAdapter, ServiceManagerDefinition,
    ServiceManagerHandoffReport, ServiceManagerHandoffRequest, ServiceManagerInspectRequest,
    ServiceManagerKind, ServiceManagerRollbackReport, ServiceManagerRollbackRequest,
    ServiceManagerStatusReport,
};
pub use supervisor::{InMemorySupervisor, RuntimeHealth, SupervisorReport};
