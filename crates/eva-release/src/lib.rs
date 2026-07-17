//! 发布加固、就绪度、安全、性能和迁移契约。
//! Release hardening, readiness, security, performance, and migration contracts.

pub mod apt;
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
pub mod homebrew;
/// 迁移指南和兼容性政策。
pub mod migration;
pub mod package_metadata;
pub mod package_output;
/// 性能预算与基线报告。
pub mod performance;
/// 外部安全扫描证据及其验证。
pub mod scanner;
/// 内置安全审查发现。
pub mod security;
pub mod winget;

pub use apt::{generate_apt_metadata, AptMetadata};
pub use artifact::{
    ReleaseArtifactEvidence, ReleaseArtifactSignature, ReleaseArtifactSigningKey,
    ReleaseArtifactSubject, ReleaseArtifactVerificationReport, ReleaseProvenanceEvidence,
};
pub use benchmark::{
    ReleaseBenchmarkEvidence, ReleaseBenchmarkMeasurement, ReleaseBenchmarkVerificationReport,
};
pub use checklist::{
    PlatformReadiness, ReleaseArtifactEvidenceCandidate, ReleaseDocumentEvidenceCandidate,
    ReleaseGate, ReleaseGateStatus, ReleaseHardeningService, ReleaseReadinessReport,
    StabilityScenario, VerifiedReleaseEvidenceBundle,
};
pub use distribution::{
    ReleaseDistributionEvidence, ReleaseDistributionVerificationReport,
    ReleaseInstallSmokeEvidence, ReleasePackageDryRunEvidence,
};
pub use evidence::{
    verify_evidence_bundle, EvidenceEnvelope, EvidenceIntegrityBlocker, EvidenceKind,
    EvidenceSubject, EvidenceVerificationReport, ProductionEvidenceBlocker,
    ProductionEvidenceContext, ProductionEvidenceExecutorRule, ProductionEvidencePolicy,
    ReleaseCaptureEvidence, ReleaseEvidenceManifest, ReleaseEvidenceManifestEntry,
    ReleaseEvidenceScope, ReleaseEvidenceType, ReleasePlatformEvidence,
    ReleasePlatformEvidenceBundle, ReleasePlatformEvidenceInput, ReleasePlatformVerificationReport,
    EVIDENCE_ENVELOPE_FORMAT, PRODUCTION_EVIDENCE_MAX_AGE_MS,
    PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS, RELEASE_COMMAND_CAPTURE_FORMAT,
    RELEASE_EVIDENCE_MANIFEST_FORMAT, RELEASE_PLATFORM_BUNDLE_FORMAT,
    RELEASE_PLATFORM_EVIDENCE_FORMAT, RELEASE_PLATFORM_INDEX_FORMAT,
};
pub use homebrew::generate_homebrew_formula;
pub use migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
pub use package_metadata::{CanonicalPackageMetadata, PackageArtifactMetadata};
pub use package_output::{write_package_manager_metadata, PackageMetadataOutput};
pub use performance::{
    release_benchmark_budget_ms, PerformanceBaselineReport, PerformanceBudget,
    PerformanceObservation, PerformanceObservationKind, ReleaseBenchmarkBudgetPolicy,
    RELEASE_BENCHMARK_BUDGET_POLICIES,
};
pub use scanner::{
    ReleaseSecurityScanEvidence, ReleaseSecurityScanFinding, ReleaseSecurityScanVerificationReport,
};
pub use security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};
pub use winget::{generate_winget_manifests, WingetManifestSet};

/// 本模块的架构职责：收集发布加固证据并暴露稳定的就绪度契约。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "collect release hardening evidence and expose stable readiness contracts";
