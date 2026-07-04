//! Runtime lifecycle and generation management boundary.

pub mod drain;
pub mod generation;
pub mod rollback;
pub mod supervisor;

pub use drain::{DrainCoordinator, DrainPlan, DrainStatus};
pub use generation::{GenerationController, GenerationState, RuntimeGeneration};
pub use rollback::{RollbackCoordinator, RollbackPlan};
pub use supervisor::{InMemorySupervisor, RuntimeHealth, SupervisorReport};
