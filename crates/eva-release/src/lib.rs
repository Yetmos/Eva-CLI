//! 发布加固、就绪度、安全、性能和迁移契约。
//! Release hardening, readiness, security, performance, and migration contracts.

/// 发布工件签名和来源证明证据。
pub mod artifact;
/// 生产基准测量证据及其验证。
pub mod benchmark;
/// 发布门禁聚合与就绪度报告。
pub mod checklist;
/// 多平台安装烟雾与打包演练证据。
pub mod distribution;
/// 统一发布证据分类、来源和主题身份信封。
pub mod evidence;
/// 迁移指南和兼容性政策。
pub mod migration;
/// 性能预算与基线报告。
pub mod performance;
/// 外部安全扫描证据及其验证。
pub mod scanner;
/// 内置安全审查发现。
pub mod security;

pub use artifact::{
    ReleaseArtifactEvidence, ReleaseArtifactSignature, ReleaseArtifactSigningKey,
    ReleaseArtifactSubject, ReleaseArtifactVerificationReport, ReleaseProvenanceEvidence,
};
pub use benchmark::{
    ReleaseBenchmarkEvidence, ReleaseBenchmarkMeasurement, ReleaseBenchmarkVerificationReport,
};
pub use checklist::{
    PlatformReadiness, ReleaseGate, ReleaseGateStatus, ReleaseHardeningService,
    ReleaseReadinessReport, StabilityScenario,
};
pub use distribution::{
    ReleaseDistributionEvidence, ReleaseDistributionVerificationReport,
    ReleaseInstallSmokeEvidence, ReleasePackageDryRunEvidence,
};
pub use evidence::{EvidenceEnvelope, EvidenceKind, EVIDENCE_ENVELOPE_FORMAT};
pub use migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
pub use performance::{PerformanceBaselineReport, PerformanceBudget};
pub use scanner::{
    ReleaseSecurityScanEvidence, ReleaseSecurityScanFinding, ReleaseSecurityScanVerificationReport,
};
pub use security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};

/// 本模块的架构职责：收集发布加固证据并暴露稳定的就绪度契约。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "collect release hardening evidence and expose stable readiness contracts";
