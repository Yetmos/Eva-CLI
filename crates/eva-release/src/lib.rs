//! Release hardening, readiness, security, performance, and migration contracts.

pub mod checklist;
pub mod migration;
pub mod performance;
pub mod security;

pub use checklist::{
    PlatformReadiness, ReleaseGate, ReleaseGateStatus, ReleaseHardeningService,
    ReleaseReadinessReport, StabilityScenario,
};
pub use migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
pub use performance::{PerformanceBaselineReport, PerformanceBudget};
pub use security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "collect release hardening evidence and expose stable readiness contracts";
