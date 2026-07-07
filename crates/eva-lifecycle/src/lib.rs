//! Runtime lifecycle and generation management boundary.

pub mod apply_lock;
pub mod drain;
pub mod generation;
pub mod rollback;
pub mod supervisor;

pub use apply_lock::{
    FileSystemUpgradeApplyLockStore, InMemoryUpgradeApplyLockStore, UpgradeApplyCoordinator,
    UpgradeApplyLock, UpgradeApplyPlan, UpgradeApplyReport,
};
pub use drain::{DrainCoordinator, DrainPlan, DrainStatus, GenerationDrainEvidence};
pub use generation::{GenerationController, GenerationState, RuntimeGeneration};
pub use rollback::{RollbackCoordinator, RollbackPlan};
pub use supervisor::{InMemorySupervisor, RuntimeHealth, SupervisorReport};
