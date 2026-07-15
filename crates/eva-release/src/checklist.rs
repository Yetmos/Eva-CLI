//! 发布就绪度检查清单的聚合与闭环报告。
//! Release readiness checklist aggregation.

use crate::artifact::{
    ReleaseArtifactEvidence, ReleaseArtifactSigningKey, ReleaseArtifactVerificationReport,
};
use crate::benchmark::{ReleaseBenchmarkEvidence, ReleaseBenchmarkVerificationReport};
use crate::distribution::{ReleaseDistributionEvidence, ReleaseDistributionVerificationReport};
use crate::evidence::{
    verify_evidence_bundle, EvidenceEnvelope, EvidenceKind, EvidenceSubject,
    EvidenceVerificationReport, ProductionEvidenceBlocker, ProductionEvidencePolicy,
    ReleaseEvidenceManifest, ReleaseEvidenceScope, ReleaseEvidenceType,
};
use crate::migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
use crate::performance::{PerformanceBaselineReport, PerformanceBudget};
use crate::scanner::{ReleaseSecurityScanEvidence, ReleaseSecurityScanVerificationReport};
use crate::security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};
use eva_core::EvaError;
use eva_mcp::{McpCompatibilityEvidenceKind, McpCompatibilityMatrix, McpCompatibilityReport};

/// 当前就绪度基线对应的语义版本。
const CURRENT_RELEASE_VERSION: &str = "1.11.5-alpha";
/// 面向报告文本的当前发布标签。
const CURRENT_RELEASE_LABEL: &str = "V1.11.5-alpha";

/// 单个发布门禁的标准化结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseGateStatus {
    /// 证据满足当前发布要求。
    Pass,
    /// 存在需跟踪风险，但不阻塞当前发布级别。
    Warn,
    /// 必要证据缺失或失败，必须阻止发布。
    Blocked,
}

impl ReleaseGateStatus {
    /// 返回用于报告和审计的稳定状态字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Blocked => "blocked",
        }
    }

    /// 判断状态是否应阻塞发布。
    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::Blocked)
    }

    /// 判断状态是否为非阻塞警告。
    pub const fn is_warning(self) -> bool {
        matches!(self, Self::Warn)
    }
}

/// 一个发布要求及其证据、必要性和补救措施。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGate {
    /// 门禁的稳定全局标识。
    pub id: String,
    /// 门禁所属能力或风险域。
    pub domain: String,
    /// 支撑该门禁结论的最强证据来源分类。
    pub evidence_kind: EvidenceKind,
    /// 当前证据得出的门禁状态。
    pub status: ReleaseGateStatus,
    /// 该门禁是否参与总体 ready/blocked 判定。
    pub required: bool,
    /// 门禁目的与结论摘要。
    pub summary: String,
    /// 支撑结论的命令、工件或审计记录。
    pub evidence: Vec<String>,
    /// 未通过或警告状态下的后续动作。
    pub remediation: Vec<String>,
}

/// 一个操作系统目标的命令、路径和 CI 就绪度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformReadiness {
    /// 标准化操作系统名称。
    pub os: String,
    /// CI 和文档使用的默认 shell。
    pub shell: String,
    /// 该平台需要覆盖的路径语义。
    pub path_model: String,
    /// 平台烟雾测试结论。
    pub status: ReleaseGateStatus,
    /// 在该平台必须运行的发布命令。
    pub required_commands: Vec<String>,
    /// CI 覆盖和平台限制说明。
    pub notes: Vec<String>,
}

/// 一个长任务、取消、升级或恢复的稳定性场景。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StabilityScenario {
    /// 场景的稳定标识。
    pub id: String,
    /// 场景当前门禁状态。
    pub status: ReleaseGateStatus,
    /// 需要成立的稳定性行为。
    pub scenario: String,
    /// 支撑行为的测试或命令证据。
    pub evidence: Vec<String>,
    /// 故障发生后对操作者承诺的恢复语义。
    pub recovery_contract: String,
}

/// V1.x alpha 内部闭环要求与生产外部依赖的分离报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V1xClosureReport {
    /// `ready_with_external_blockers` 或 `blocked` 状态。
    pub status: String,
    /// 闭环范围和外部依赖边界摘要。
    pub summary: String,
    /// 必须纳入 alpha 闭环的门禁标识。
    pub required_gate_ids: Vec<String>,
    /// 当前已明确通过的必需门禁。
    pub passed_required_gate_ids: Vec<String>,
    /// 报告中完全缺失的必需门禁。
    pub missing_required_gate_ids: Vec<String>,
    /// 存在但未达到 Pass 的必需门禁。
    pub blocking_required_gate_ids: Vec<String>,
    /// 只有提供真实发布证据时才加入的生产门禁。
    pub optional_production_gate_ids: Vec<String>,
    /// 凭据、平台环境和真实设施等仓库外阻塞项。
    pub blocked_external_items: Vec<String>,
}

/// 一个目标平台范围的完整发布就绪度报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReadinessReport {
    /// 报告对应发布版本。
    pub version: String,
    /// `ready` 或 `blocked` 总体状态。
    pub status: String,
    /// 报告明确绑定的 alpha 或 production evidence scope。
    pub evidence_scope: ReleaseEvidenceScope,
    /// `all` 或单个操作系统目标。
    pub target: String,
    /// 目标范围内的平台就绪度。
    pub platforms: Vec<PlatformReadiness>,
    /// 关键故障与恢复场景。
    pub stability: Vec<StabilityScenario>,
    /// 内置及可选外部证据转换出的全部门禁。
    pub gates: Vec<ReleaseGate>,
    /// V1.x alpha 内部闭环报告。
    pub closure: V1xClosureReport,
    /// 聚合过程和可选外部证据审计记录。
    pub audit: Vec<String>,
}

/// 待 verifier 绑定到真实 artifact bytes 的 artifact evidence 输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactEvidenceCandidate {
    evidence: ReleaseArtifactEvidence,
    envelope: EvidenceEnvelope,
    subject_bytes: Vec<u8>,
}

impl ReleaseArtifactEvidenceCandidate {
    /// 创建尚未获得 production 信任的 artifact 候选；只能交给 bundle verifier。
    pub fn new(
        evidence: ReleaseArtifactEvidence,
        envelope: EvidenceEnvelope,
        subject_bytes: Vec<u8>,
    ) -> Self {
        Self {
            evidence,
            envelope,
            subject_bytes,
        }
    }
}

/// 待 verifier 将 canonical typed manifest 绑定到 envelope 的文档 evidence 输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDocumentEvidenceCandidate<T> {
    evidence: T,
    envelope: EvidenceEnvelope,
}

impl<T> ReleaseDocumentEvidenceCandidate<T> {
    /// 创建尚未验证的 typed evidence/envelope 对。
    pub fn new(evidence: T, envelope: EvidenceEnvelope) -> Self {
        Self { evidence, envelope }
    }
}

/// 只能由完整性 verifier 构造、可安全进入 scoped checklist 的 evidence bundle。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedReleaseEvidenceBundle {
    manifest: ReleaseEvidenceManifest,
    artifact_kind: Option<EvidenceKind>,
    distribution_kind: Option<EvidenceKind>,
    security_scan_kind: Option<EvidenceKind>,
    benchmark_kind: Option<EvidenceKind>,
    artifact: Option<ReleaseArtifactEvidence>,
    distribution: Option<ReleaseDistributionEvidence>,
    security_scan: Option<ReleaseSecurityScanEvidence>,
    benchmark: Option<ReleaseBenchmarkEvidence>,
    verification: EvidenceVerificationReport,
}

impl VerifiedReleaseEvidenceBundle {
    /// 校验 alpha manifest、外部可信提交、全部 typed commit 与 subject digest。
    ///
    /// Production scope 必须改用 verify_production，防止调用方遗漏 consumer-owned policy。
    #[allow(clippy::too_many_arguments)]
    pub fn verify(
        manifest: ReleaseEvidenceManifest,
        expected_source_commit: &str,
        artifact: Option<ReleaseArtifactEvidenceCandidate>,
        distribution: Option<ReleaseDocumentEvidenceCandidate<ReleaseDistributionEvidence>>,
        security_scan: Option<ReleaseDocumentEvidenceCandidate<ReleaseSecurityScanEvidence>>,
        benchmark: Option<ReleaseDocumentEvidenceCandidate<ReleaseBenchmarkEvidence>>,
    ) -> Result<Self, EvaError> {
        if manifest.scope == ReleaseEvidenceScope::Production {
            return Err(EvaError::conflict(
                "production release evidence requires a consumer policy",
            )
            .with_context(
                "blocked_reasons",
                ProductionEvidenceBlocker::PolicyRequired.as_str(),
            ));
        }
        Self::verify_inner(
            manifest,
            expected_source_commit,
            artifact,
            distribution,
            security_scan,
            benchmark,
            None,
        )
    }

    /// 校验 production 完整覆盖、可信时间窗、执行器及全部完整性声明。
    #[allow(clippy::too_many_arguments)]
    pub fn verify_production(
        manifest: ReleaseEvidenceManifest,
        expected_source_commit: &str,
        artifact: Option<ReleaseArtifactEvidenceCandidate>,
        distribution: Option<ReleaseDocumentEvidenceCandidate<ReleaseDistributionEvidence>>,
        security_scan: Option<ReleaseDocumentEvidenceCandidate<ReleaseSecurityScanEvidence>>,
        benchmark: Option<ReleaseDocumentEvidenceCandidate<ReleaseBenchmarkEvidence>>,
        policy: &ProductionEvidencePolicy,
    ) -> Result<Self, EvaError> {
        if manifest.scope != ReleaseEvidenceScope::Production {
            return Err(EvaError::invalid_argument(
                "production release evidence verifier requires production scope",
            )
            .with_context("manifest_scope", manifest.scope.as_str()));
        }
        Self::verify_inner(
            manifest,
            expected_source_commit,
            artifact,
            distribution,
            security_scan,
            benchmark,
            Some(policy),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn verify_inner(
        manifest: ReleaseEvidenceManifest,
        expected_source_commit: &str,
        artifact: Option<ReleaseArtifactEvidenceCandidate>,
        distribution: Option<ReleaseDocumentEvidenceCandidate<ReleaseDistributionEvidence>>,
        security_scan: Option<ReleaseDocumentEvidenceCandidate<ReleaseSecurityScanEvidence>>,
        benchmark: Option<ReleaseDocumentEvidenceCandidate<ReleaseBenchmarkEvidence>>,
        production_policy: Option<&ProductionEvidencePolicy>,
    ) -> Result<Self, EvaError> {
        if manifest.source_commit != expected_source_commit {
            return Err(EvaError::conflict(
                "release evidence manifest source commit does not match trusted commit",
            )
            .with_context("blocked_reasons", "evidence_source_commit_mismatch"));
        }

        let candidate_present = |evidence_type| match evidence_type {
            ReleaseEvidenceType::Artifact => artifact.is_some(),
            ReleaseEvidenceType::Distribution => distribution.is_some(),
            ReleaseEvidenceType::SecurityScan => security_scan.is_some(),
            ReleaseEvidenceType::Benchmark => benchmark.is_some(),
        };
        for evidence_type in ReleaseEvidenceType::ALL {
            let manifest_present = manifest
                .entries
                .iter()
                .any(|entry| entry.evidence_type == evidence_type);
            if manifest_present != candidate_present(evidence_type) {
                return Err(EvaError::invalid_argument(
                    "release evidence manifest entries do not match loaded evidence",
                )
                .with_context("entry_type", evidence_type.as_str())
                .with_context("manifest_present", manifest_present.to_string())
                .with_context(
                    "loaded_evidence_present",
                    candidate_present(evidence_type).to_string(),
                ));
            }
        }

        let distribution_subject = distribution
            .as_ref()
            .map(|candidate| candidate.evidence.to_manifest().into_bytes());
        let security_scan_subject = security_scan
            .as_ref()
            .map(|candidate| candidate.evidence.to_manifest().into_bytes());
        let benchmark_subject = benchmark
            .as_ref()
            .map(|candidate| candidate.evidence.to_manifest().into_bytes());
        let mut subjects = Vec::with_capacity(manifest.entries.len());
        if let Some(candidate) = &artifact {
            subjects.push(
                candidate
                    .evidence
                    .verification_subject(&candidate.envelope, &candidate.subject_bytes),
            );
        }
        if let (Some(candidate), Some(subject_bytes)) = (&distribution, &distribution_subject) {
            subjects.push(
                EvidenceSubject::new(&candidate.envelope, subject_bytes).with_source_commit_claim(
                    "distribution_evidence",
                    &candidate.evidence.source_commit,
                ),
            );
        }
        if let (Some(candidate), Some(subject_bytes)) = (&security_scan, &security_scan_subject) {
            subjects.push(
                EvidenceSubject::new(&candidate.envelope, subject_bytes).with_source_commit_claim(
                    "security_scan_evidence",
                    &candidate.evidence.source_commit,
                ),
            );
        }
        if let (Some(candidate), Some(subject_bytes)) = (&benchmark, &benchmark_subject) {
            subjects.push(
                EvidenceSubject::new(&candidate.envelope, subject_bytes).with_source_commit_claim(
                    "benchmark_evidence",
                    &candidate.evidence.source_commit,
                ),
            );
        }
        let mut verification = verify_evidence_bundle(expected_source_commit, &subjects)?;
        if !verification.is_verified() {
            return Err(EvaError::conflict(
                "release evidence manifest integrity verification was blocked",
            )
            .with_context(
                "blocked_reasons",
                verification
                    .blocked_reasons
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .with_context("subject_count", verification.subject_count.to_string())
            .with_context(
                "verified_subject_count",
                verification.verified_subject_count.to_string(),
            ));
        }

        if let Some(policy) = production_policy {
            let envelope_entries = [
                (
                    ReleaseEvidenceType::Artifact,
                    artifact.as_ref().map(|candidate| &candidate.envelope),
                ),
                (
                    ReleaseEvidenceType::Distribution,
                    distribution.as_ref().map(|candidate| &candidate.envelope),
                ),
                (
                    ReleaseEvidenceType::SecurityScan,
                    security_scan.as_ref().map(|candidate| &candidate.envelope),
                ),
                (
                    ReleaseEvidenceType::Benchmark,
                    benchmark.as_ref().map(|candidate| &candidate.envelope),
                ),
            ];
            let release_identities = [
                artifact.as_ref().map(|candidate| {
                    (
                        ReleaseEvidenceType::Artifact,
                        candidate.evidence.version.as_str(),
                        candidate.evidence.source_tag.as_str(),
                    )
                }),
                distribution.as_ref().map(|candidate| {
                    (
                        ReleaseEvidenceType::Distribution,
                        candidate.evidence.version.as_str(),
                        candidate.evidence.source_tag.as_str(),
                    )
                }),
                security_scan.as_ref().map(|candidate| {
                    (
                        ReleaseEvidenceType::SecurityScan,
                        candidate.evidence.version.as_str(),
                        candidate.evidence.source_tag.as_str(),
                    )
                }),
                benchmark.as_ref().map(|candidate| {
                    (
                        ReleaseEvidenceType::Benchmark,
                        candidate.evidence.version.as_str(),
                        candidate.evidence.source_tag.as_str(),
                    )
                }),
            ];
            verification.audit.extend(verify_production_policy(
                &manifest,
                policy,
                &envelope_entries,
                &release_identities,
            )?);
        }

        let artifact_kind = artifact.as_ref().map(|candidate| candidate.envelope.kind);
        let distribution_kind = distribution
            .as_ref()
            .map(|candidate| candidate.envelope.kind);
        let security_scan_kind = security_scan
            .as_ref()
            .map(|candidate| candidate.envelope.kind);
        let benchmark_kind = benchmark.as_ref().map(|candidate| candidate.envelope.kind);

        Ok(Self {
            manifest,
            artifact_kind,
            distribution_kind,
            security_scan_kind,
            benchmark_kind,
            artifact: artifact.map(|candidate| candidate.evidence),
            distribution: distribution.map(|candidate| candidate.evidence),
            security_scan: security_scan.map(|candidate| candidate.evidence),
            benchmark: benchmark.map(|candidate| candidate.evidence),
            verification,
        })
    }

    /// 返回已验证 manifest 的 scope。
    pub fn scope(&self) -> ReleaseEvidenceScope {
        self.manifest.scope
    }

    /// 返回已验证 manifest 的条目数，不暴露本地文件路径。
    pub fn entry_count(&self) -> usize {
        self.manifest.entries.len()
    }

    /// 返回完整性验证报告供审计摘要使用。
    pub fn verification(&self) -> &EvidenceVerificationReport {
        &self.verification
    }
}

type ProductionEnvelopeEntry<'a> = (ReleaseEvidenceType, Option<&'a EvidenceEnvelope>);
type ProductionReleaseIdentity<'a> = (ReleaseEvidenceType, &'a str, &'a str);
type ProductionCaptureIdentity<'a> = (&'a str, &'a str, &'a str, u128);

/// Apply policy owned by the production consumer after subject integrity has been proven.
fn verify_production_policy(
    manifest: &ReleaseEvidenceManifest,
    policy: &ProductionEvidencePolicy,
    envelope_entries: &[ProductionEnvelopeEntry<'_>],
    release_identities: &[Option<ProductionReleaseIdentity<'_>>],
) -> Result<Vec<String>, EvaError> {
    let mut blockers = Vec::new();
    let mut failures = Vec::new();
    let mut seen_subjects: Vec<(&str, ReleaseEvidenceType)> = Vec::new();
    let mut seen_identities: Vec<(ProductionCaptureIdentity<'_>, ReleaseEvidenceType)> = Vec::new();
    let mut seen_paths: Vec<(&str, ReleaseEvidenceType, &str)> = Vec::new();

    let mut record = |blocker: ProductionEvidenceBlocker, detail: String| {
        if !blockers.contains(&blocker) {
            blockers.push(blocker);
        }
        failures.push(detail);
    };

    if manifest.canonical_digest() != policy.expected_manifest_digest() {
        record(
            ProductionEvidenceBlocker::ManifestDigestMismatch,
            ProductionEvidenceBlocker::ManifestDigestMismatch
                .as_str()
                .to_owned(),
        );
    }

    for evidence_type in ReleaseEvidenceType::ALL {
        if !manifest
            .entries
            .iter()
            .any(|entry| entry.evidence_type == evidence_type)
        {
            record(
                ProductionEvidenceBlocker::CoverageMissing,
                format!(
                    "{}:{}",
                    ProductionEvidenceBlocker::CoverageMissing,
                    evidence_type
                ),
            );
        }
    }

    for entry in &manifest.entries {
        let paths = [
            Some((entry.evidence_path.as_str(), "evidence")),
            Some((entry.envelope_path.as_str(), "envelope")),
            entry.subject_path.as_deref().map(|path| (path, "subject")),
        ];
        for (path, label) in paths.into_iter().flatten() {
            if let Some((_, previous_type, previous_label)) =
                seen_paths.iter().find(|(seen, _, _)| *seen == path)
            {
                record(
                    ProductionEvidenceBlocker::IdentityConflict,
                    format!(
                        "{}:{}:{}:{}:{}",
                        ProductionEvidenceBlocker::IdentityConflict,
                        previous_type,
                        previous_label,
                        entry.evidence_type,
                        label
                    ),
                );
            } else {
                seen_paths.push((path, entry.evidence_type, label));
            }
        }
    }

    let maximum_timestamp = policy
        .trusted_now_ms()
        .saturating_add(policy.max_future_skew_ms());
    for (evidence_type, envelope) in envelope_entries {
        let Some(envelope) = envelope else {
            continue;
        };
        let manifest_entry = manifest
            .entries
            .iter()
            .find(|entry| entry.evidence_type == *evidence_type);
        match manifest_entry.and_then(|entry| entry.envelope_digest.as_deref()) {
            None => record(
                ProductionEvidenceBlocker::EnvelopeDigestMissing,
                format!(
                    "{}:{}",
                    ProductionEvidenceBlocker::EnvelopeDigestMissing,
                    evidence_type
                ),
            ),
            Some(expected_digest) if expected_digest != envelope.canonical_digest() => record(
                ProductionEvidenceBlocker::EnvelopeDigestMismatch,
                format!(
                    "{}:{}",
                    ProductionEvidenceBlocker::EnvelopeDigestMismatch,
                    evidence_type
                ),
            ),
            Some(_) => {}
        }
        if envelope.kind != EvidenceKind::Measurement {
            record(
                ProductionEvidenceBlocker::KindNotMeasurement,
                format!(
                    "{}:{}:{}",
                    ProductionEvidenceBlocker::KindNotMeasurement,
                    evidence_type,
                    envelope.kind
                ),
            );
        }
        if envelope.timestamp > maximum_timestamp {
            record(
                ProductionEvidenceBlocker::FutureTimestamp,
                format!(
                    "{}:{}",
                    ProductionEvidenceBlocker::FutureTimestamp,
                    evidence_type
                ),
            );
        } else if policy.trusted_now_ms().saturating_sub(envelope.timestamp) > policy.max_age_ms() {
            record(
                ProductionEvidenceBlocker::Stale,
                format!("{}:{}", ProductionEvidenceBlocker::Stale, evidence_type),
            );
        }
        if !policy.trusts_executor(*evidence_type, &envelope.executor) {
            record(
                ProductionEvidenceBlocker::ExecutorUntrusted,
                format!(
                    "{}:{}",
                    ProductionEvidenceBlocker::ExecutorUntrusted,
                    evidence_type
                ),
            );
        }

        if let Some((_, previous_type)) = seen_subjects
            .iter()
            .find(|(digest, _)| *digest == envelope.subject_digest)
        {
            record(
                ProductionEvidenceBlocker::SubjectDuplicate,
                format!(
                    "{}:{}:{}",
                    ProductionEvidenceBlocker::SubjectDuplicate,
                    previous_type,
                    evidence_type
                ),
            );
        } else {
            seen_subjects.push((&envelope.subject_digest, *evidence_type));
        }

        let identity = (
            envelope.source.as_str(),
            envelope.environment.as_str(),
            envelope.executor.as_str(),
            envelope.timestamp,
        );
        if let Some((_, previous_type)) = seen_identities.iter().find(|(seen, _)| *seen == identity)
        {
            record(
                ProductionEvidenceBlocker::IdentityConflict,
                format!(
                    "{}:{}:{}",
                    ProductionEvidenceBlocker::IdentityConflict,
                    previous_type,
                    evidence_type
                ),
            );
        } else {
            seen_identities.push((identity, *evidence_type));
        }
    }

    let mut expected_release_identity: Option<(&str, &str, ReleaseEvidenceType)> = None;
    for (evidence_type, version, source_tag) in release_identities.iter().flatten() {
        if *version != policy.expected_version() || *source_tag != policy.expected_source_tag() {
            record(
                ProductionEvidenceBlocker::ReleaseIdentityConflict,
                format!(
                    "{}:{}:expected_release",
                    ProductionEvidenceBlocker::ReleaseIdentityConflict,
                    evidence_type
                ),
            );
        }
        if let Some((expected_version, expected_tag, expected_type)) = expected_release_identity {
            if *version != expected_version || *source_tag != expected_tag {
                record(
                    ProductionEvidenceBlocker::ReleaseIdentityConflict,
                    format!(
                        "{}:{}:{}",
                        ProductionEvidenceBlocker::ReleaseIdentityConflict,
                        expected_type,
                        evidence_type
                    ),
                );
            }
        } else {
            expected_release_identity = Some((version, source_tag, *evidence_type));
        }
    }

    if !blockers.is_empty() {
        return Err(EvaError::conflict(
            "production release evidence policy verification was blocked",
        )
        .with_context(
            "blocked_reasons",
            blockers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )
        .with_context("policy_failures", failures.join(",")));
    }

    Ok(vec![
        "production_evidence.coverage:complete".to_owned(),
        format!(
            "production_evidence.trusted_now_ms:{}",
            policy.trusted_now_ms()
        ),
        format!("production_evidence.max_age_ms:{}", policy.max_age_ms()),
        format!(
            "production_evidence.max_future_skew_ms:{}",
            policy.max_future_skew_ms()
        ),
        format!(
            "production_evidence.expected_version:{}",
            policy.expected_version()
        ),
        format!(
            "production_evidence.expected_source_tag:{}",
            policy.expected_source_tag()
        ),
        format!(
            "production_evidence.expected_run_id:{}",
            policy.expected_run_id()
        ),
        format!(
            "production_evidence.expected_run_attempt:{}",
            policy.expected_run_attempt()
        ),
        format!(
            "production_evidence.expected_manifest_digest:{}",
            policy.expected_manifest_digest()
        ),
        "production_evidence.envelope_digests:verified".to_owned(),
        "production_evidence.kind:measurement".to_owned(),
        "production_evidence.executor_policy:verified".to_owned(),
        "production_evidence.uniqueness:verified".to_owned(),
        "production_evidence.release_identity:consistent".to_owned(),
    ])
}

/// 构建当前发布加固报告和兼容性基线的无状态服务。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseHardeningService;

impl ReleaseHardeningService {
    /// 创建当前 V1.x 发布加固服务。
    pub fn v15() -> Self {
        Self
    }

    /// 只使用仓库内置基线生成指定平台范围的就绪度报告。
    pub fn readiness(&self, target: impl Into<String>) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, None, None, None)
    }

    /// 在 alpha 兼容基线中加入签名工件和来源证明门禁。
    pub fn readiness_with_artifact_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseArtifactEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), Some(evidence), None, None, None)
    }

    /// 在 alpha 兼容基线中加入多平台分发门禁。
    pub fn readiness_with_distribution_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseDistributionEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, Some(evidence), None, None)
    }

    /// 在 alpha 兼容基线中加入外部安全扫描门禁。
    pub fn readiness_with_security_scan_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseSecurityScanEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, None, Some(evidence), None)
    }

    /// 在 alpha 兼容基线中加入 benchmark typed evidence 门禁。
    pub fn readiness_with_benchmark_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseBenchmarkEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, None, None, Some(evidence))
    }

    /// 一次性聚合任意组合的旧 typed evidence，报告 scope 始终为 alpha。
    pub fn readiness_with_release_evidence(
        &self,
        target: impl Into<String>,
        artifact_evidence: Option<&ReleaseArtifactEvidence>,
        distribution_evidence: Option<&ReleaseDistributionEvidence>,
        security_scan_evidence: Option<&ReleaseSecurityScanEvidence>,
        benchmark_evidence: Option<&ReleaseBenchmarkEvidence>,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(
            target.into(),
            artifact_evidence,
            distribution_evidence,
            security_scan_evidence,
            benchmark_evidence,
        )
    }

    /// 只接受 verifier 构造的 bundle，并按其不可变 scope 聚合发布证据。
    pub fn readiness_with_verified_release_evidence(
        &self,
        target: impl Into<String>,
        bundle: &VerifiedReleaseEvidenceBundle,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        let mut report = self.readiness_inner(
            target.into(),
            bundle.artifact.as_ref(),
            bundle.distribution.as_ref(),
            bundle.security_scan.as_ref(),
            bundle.benchmark.as_ref(),
        )?;
        set_gate_evidence_kind(
            &mut report.gates,
            "REL-ARTIFACT-PROVENANCE-001",
            bundle.artifact_kind,
        )?;
        set_gate_evidence_kind(
            &mut report.gates,
            "REL-DISTRIBUTION-001",
            bundle.distribution_kind,
        )?;
        set_gate_evidence_kind(
            &mut report.gates,
            "REL-SECURITY-SCAN-001",
            bundle.security_scan_kind,
        )?;
        set_gate_evidence_kind(
            &mut report.gates,
            "REL-BENCHMARK-001",
            bundle.benchmark_kind,
        )?;
        if bundle.scope() == ReleaseEvidenceScope::Production {
            enforce_production_measurement_gates(&mut report);
        }
        report.evidence_scope = bundle.scope();
        Ok(report)
    }

    /// 校验目标、构建全部内置门禁、验证可选证据并计算最终状态。
    ///
    /// 聚合顺序固定：平台/稳定性、内置安全/性能/迁移、各运行时能力门禁、可选外部
    /// 证据，最后生成 V1.x closure 门禁。可选生产证据仅在传入时成为 required 门禁；
    /// 一旦传入却验证失败，就会阻塞总体状态。总体只检查 `required && blocked`，Warn
    /// 会被计数但不阻塞。closure 只覆盖预定义的 alpha 内部门禁，将凭据和真实设施
    /// 明确列为外部阻塞，避免错误宣称生产条件已经满足。
    fn readiness_inner(
        &self,
        target: String,
        artifact_evidence: Option<&ReleaseArtifactEvidence>,
        distribution_evidence: Option<&ReleaseDistributionEvidence>,
        security_scan_evidence: Option<&ReleaseSecurityScanEvidence>,
        benchmark_evidence: Option<&ReleaseBenchmarkEvidence>,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        if target.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "release readiness target cannot be empty",
            ));
        }
        if !matches!(target.as_str(), "all" | "windows" | "linux" | "macos") {
            return Err(EvaError::invalid_argument(
                "release readiness target must be all, windows, linux, or macos",
            )
            .with_context("target", target));
        }

        // 先生成所有内部基线门禁，closure 才能检查是否有缺失或未通过项。
        let platforms = self.platforms(&target);
        let security = self.security_review();
        let performance = self.performance_baseline();
        let migration = self.migration_guide("1.5.1", CURRENT_RELEASE_VERSION)?;
        let stability = self.stability_scenarios();
        let mut gates = self.core_gates(&platforms, &stability);
        gates.extend(security.findings.iter().map(security_gate));
        gates.extend(performance.budgets.iter().map(performance_gate));
        gates.push(migration_gate(&migration));
        gates.push(durable_backend_gate());
        gates.push(durable_eventbus_gate());
        gates.push(durable_task_audit_artifact_gate());
        gates.push(durable_runtime_recovery_gate());
        gates.push(durable_diagnostics_gate());
        gates.push(lua_vm_execution_gate());
        gates.push(lua_host_bindings_gate());
        gates.push(lua_resource_limits_gate());
        gates.push(lua_hot_reload_lifecycle_gate());
        gates.push(signed_backup_archive_gate());
        gates.push(restore_apply_gate());
        gates.push(supervisor_handoff_gate());
        gates.push(service_manager_abstraction_gate());
        gates.push(daemon_runtime_gate());
        let mcp_compatibility_report = McpCompatibilityMatrix::v1137_fixture().verify()?;
        gates.push(mcp_compatibility_matrix_gate(Some(
            &mcp_compatibility_report,
        )));
        gates.push(provider_supervision_gate());
        gates.push(hardware_safety_release_gate());
        gates.push(observability_policy_gate());
        gates.push(public_json_contract_gate());
        // 外部证据不在未提供时伪造通过；一旦提供，就转成 required 发布门禁。
        let artifact_report = artifact_evidence
            .map(|evidence| evidence.verify(&ReleaseArtifactSigningKey::local_development()));
        if let Some(report) = artifact_report.as_ref() {
            gates.push(release_artifact_provenance_gate(report));
        }
        let distribution_report = distribution_evidence.map(ReleaseDistributionEvidence::verify);
        if let Some(report) = distribution_report.as_ref() {
            gates.push(release_distribution_gate(report));
        }
        let security_scan_report = security_scan_evidence.map(ReleaseSecurityScanEvidence::verify);
        if let Some(report) = security_scan_report.as_ref() {
            gates.push(release_security_scan_gate(report));
        }
        let benchmark_report = benchmark_evidence.map(ReleaseBenchmarkEvidence::verify);
        if let Some(report) = benchmark_report.as_ref() {
            gates.push(release_benchmark_gate(report));
        }
        // closure 门禁必须最后加入，避免它把自身当成闭环前置条件。
        let closure = v1x_closure_report(&gates);
        gates.push(v1x_closure_gate(&closure));

        let status = if gates
            .iter()
            .any(|gate| gate.required && gate.status.is_blocking())
        {
            "blocked"
        } else {
            "ready"
        }
        .to_owned();

        Ok(ReleaseReadinessReport {
            version: CURRENT_RELEASE_VERSION.to_owned(),
            status,
            evidence_scope: ReleaseEvidenceScope::Alpha,
            target,
            platforms,
            stability,
            gates,
            closure,
            audit: release_audit(
                artifact_report.as_ref(),
                distribution_report.as_ref(),
                security_scan_report.as_ref(),
                benchmark_report.as_ref(),
            ),
        })
    }

    /// 返回当前版本内置的高风险边界安全审查。
    ///
    /// passed 项表示仓库内已有可引用证据；tracked 项保留非阻塞警告和未来补救措施，
    /// 不会伪装为已经实现真实服务管理器等生产集成。
    pub fn security_review(&self) -> SecurityReviewReport {
        SecurityReviewReport {
            version: CURRENT_RELEASE_VERSION.to_owned(),
            status: "reviewed".to_owned(),
            findings: vec![
                SecurityFinding::passed(
                    "SEC-POLICY-001",
                    "policy",
                    SecuritySeverity::High,
                    "effective policy narrows permissions and rejects request expansion",
                    vec![
                        "eva-policy unit tests cover permission narrowing".to_owned(),
                        "CLI policy denials map to stable exit code 3".to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-SANDBOX-001",
                    "lua_sandbox",
                    SecuritySeverity::High,
                    "Lua host exposes controlled context snapshots instead of host handles",
                    vec![
                        "eva-lua-host sandbox rejects forbidden tokens".to_owned(),
                        "memory context output contains counts and audit, not raw host APIs"
                            .to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-SECRET-001",
                    "secret_redaction",
                    SecuritySeverity::Medium,
                    "backup manifests retain redaction metadata for sensitive entries",
                    vec![
                        "release pointer entry is redacted in V1.4 backup smoke".to_owned(),
                        "JSON writers do not emit secret values from policy context".to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-MCP-001",
                    "mcp",
                    SecuritySeverity::High,
                    "MCP tools are allowlisted and probes do not start external servers",
                    vec![
                        "mcp probe blocks unlisted tool delete_repo".to_owned(),
                        "MCP runtime remains in-memory for V1.5 release hardening".to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-HW-001",
                    "hardware",
                    SecuritySeverity::High,
                    "hardware discovery never grants raw I/O handles from CLI",
                    vec![
                        "hardware candidates report handle_granted=false".to_owned(),
                        "hardware bind remains plan-first even with --apply".to_owned(),
                        "V1.15.1 hardware bind reports platform OS permission evidence with remediation and raw_device_path_exposed=false".to_owned(),
                        "PlatformOsPermissionProvider blocks driver start before lease claim when permission is missing".to_owned(),
                        "V1.15.4 daemon hotplug subscriber publishes typed logical-state events and reports raw_handles_exposed=false".to_owned(),
                        "hotplug watcher crash path releases active hardware leases".to_owned(),
                        "V1.15.5 release check records simulator parity, permission denial, lease cleanup, and hotplug smoke evidence".to_owned(),
                    ],
                ),
                SecurityFinding::tracked(
                    "SEC-LIFE-001",
                    "restore_and_upgrade",
                    SecuritySeverity::Medium,
                    "restore and upgrade mutations are gated while service-manager integration remains future work",
                    vec![
                        "restore plan reports apply_allowed=false".to_owned(),
                        "restore apply gate verifies confirmation, archive evidence, policy approval, lock, and health before reporting apply_allowed=true".to_owned(),
                        "restore apply keeps no-step plans mutation_executed=false and reports mutation_executed=true only after staged file mutation commits".to_owned(),
                        "restore rollback apply verifies pre-restore archive entries and transaction log before reverse mutation".to_owned(),
                        "restore apply and rollback expose operator confirmation with confirm token, target root, affected count, state flags, and irreversible warning".to_owned(),
                        "upgrade apply can commit a controlled supervisor handoff and release pointer mutation inside the configured state store after policy approval".to_owned(),
                    ],
                    vec![
                        "replace the local supervisor adapter smoke with a real service-manager adapter before daemonized production handoff".to_owned(),
                    ],
                ),
            ],
            audit: vec![
                "security_review:policy_sandbox_secret_mcp_hardware_lifecycle".to_owned(),
                "known_future_apply_paths_are_tracked_not_enabled".to_owned(),
            ],
        }
    }

    /// 返回用于静态发布烟雾的未测量契约性能预算。
    ///
    /// 这些阈值不附带任何伪造 observed_ms；真实基准证据应通过
    /// `ReleaseBenchmarkEvidence` 作为独立 required 门禁传入。
    pub fn performance_baseline(&self) -> PerformanceBaselineReport {
        let budgets = vec![
            PerformanceBudget::unmeasured(
                "eventbus.publish",
                "single in-memory publish latency",
                5,
                "contract threshold only; no eventbus publish measurement attached",
            ),
            PerformanceBudget::unmeasured(
                "scheduler.fanout",
                "basic topic routing fanout latency",
                10,
                "contract threshold only; no scheduler fanout measurement attached",
            ),
            PerformanceBudget::unmeasured(
                "adapter.probe",
                "side-effect-free adapter probe latency",
                15,
                "contract threshold only; no adapter probe measurement attached",
            ),
            PerformanceBudget::unmeasured(
                "memory.context",
                "request context assembly latency",
                25,
                "contract threshold only; no memory context measurement attached",
            ),
            PerformanceBudget::unmeasured(
                "backup.create",
                "in-memory backup artifact creation latency",
                50,
                "contract threshold only; no backup creation measurement attached",
            ),
            PerformanceBudget::unmeasured(
                "release.check",
                "release hardening report generation latency",
                20,
                "contract threshold only; no release check measurement attached",
            ),
        ];

        PerformanceBaselineReport {
            version: CURRENT_RELEASE_VERSION.to_owned(),
            status: "unmeasured".to_owned(),
            budgets,
            audit: vec![
                "performance:baseline:v1.5".to_owned(),
                "performance_observation:unmeasured".to_owned(),
                "budgets_are_contractual_thresholds_without_runtime_observations".to_owned(),
            ],
        }
    }

    /// 为非空源/目标版本生成当前兼容迁移指南。
    pub fn migration_guide(
        &self,
        from: impl Into<String>,
        to: impl Into<String>,
    ) -> Result<MigrationGuide, EvaError> {
        let from = from.into();
        let to = to.into();
        if from.trim().is_empty() || to.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "migration guide requires non-empty versions",
            ));
        }
        Ok(MigrationGuide {
            from_version: from,
            to_version: to,
            status: "compatible".to_owned(),
            breaking_changes: Vec::new(),
            steps: vec![
                MigrationStep::new(
                    "build",
                    format!("rebuild the workspace with version {CURRENT_RELEASE_VERSION}"),
                    "cargo build --release",
                    false,
                ),
                MigrationStep::new(
                    "smoke",
                    "run the V1.0 to V1.6 smoke commands before release checks",
                    "cargo run -- upgrade check --output json",
                    false,
                ),
                MigrationStep::new(
                    "release-check",
                    "run the aggregate release readiness command",
                    "cargo run -- release check --output json",
                    false,
                ),
                MigrationStep::new(
                    "security-review",
                    "review tracked security findings before enabling real restore or process handoff",
                    "cargo run -- release security --output json",
                    false,
                ),
                MigrationStep::new(
                    "performance-baseline",
                    "compare EventBus, Scheduler, Adapter, memory, backup, and release budgets",
                    "cargo run -- release perf --output json",
                    false,
                ),
            ],
            compatibility_policy: CompatibilityPolicy::v15(),
            audit: vec![
                "migration:v1.5.1_to_v1.11.5-alpha:no_breaking_changes".to_owned(),
                "json_envelope_and_exit_codes_remain_stable".to_owned(),
                "durable_task_audit_artifact_additive_alpha_baseline".to_owned(),
            ],
        })
    }

    /// 构造三平台就绪度矩阵，并按请求目标筛选。
    fn platforms(&self, target: &str) -> Vec<PlatformReadiness> {
        let all = vec![
            PlatformReadiness {
                os: "windows".to_owned(),
                shell: "pwsh".to_owned(),
                path_model: "drive-letter and backslash tolerant project roots".to_owned(),
                status: ReleaseGateStatus::Pass,
                required_commands: smoke_commands(),
                notes: vec![
                    "CI matrix includes windows-latest".to_owned(),
                    "CLI accepts --project paths through PathBuf".to_owned(),
                ],
            },
            PlatformReadiness {
                os: "linux".to_owned(),
                shell: "pwsh".to_owned(),
                path_model: "case-sensitive POSIX paths".to_owned(),
                status: ReleaseGateStatus::Pass,
                required_commands: smoke_commands(),
                notes: vec![
                    "CI matrix includes ubuntu-latest".to_owned(),
                    "website and docs validation run on Ubuntu".to_owned(),
                ],
            },
            PlatformReadiness {
                os: "macos".to_owned(),
                shell: "pwsh".to_owned(),
                path_model: "case-insensitive default filesystem with POSIX separators".to_owned(),
                status: ReleaseGateStatus::Pass,
                required_commands: smoke_commands(),
                notes: vec![
                    "CI matrix includes macos-latest".to_owned(),
                    "no platform-specific native provider is started in this alpha release"
                        .to_owned(),
                ],
            },
        ];
        if target == "all" {
            all
        } else {
            all.into_iter()
                .filter(|platform| platform.os == target)
                .collect()
        }
    }

    /// 返回任务、取消、升级和持久恢复的内置稳定性场景。
    fn stability_scenarios(&self) -> Vec<StabilityScenario> {
        vec![
            StabilityScenario {
                id: "STAB-TASK-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "long or failed task can be inspected after execution".to_owned(),
                evidence: vec![
                    "task status/logs/cancel read latest .eva task report".to_owned(),
                    "timeout run can replay dead letters".to_owned(),
                ],
                recovery_contract: "operators keep task JSON/text output as diagnostic evidence"
                    .to_owned(),
            },
            StabilityScenario {
                id: "STAB-CANCEL-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "cancellation is explicit and auditable".to_owned(),
                evidence: vec![
                    "run --cancel generates cancelled task".to_owned(),
                    "task cancel records rejected late cancellation on completed task".to_owned(),
                ],
                recovery_contract: "cancel does not mutate completed terminal task state"
                    .to_owned(),
            },
            StabilityScenario {
                id: "STAB-UPGRADE-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "upgrade and restore operations are planned before apply".to_owned(),
                evidence: vec![
                    "restore plan apply_allowed=false".to_owned(),
                    "upgrade check returns drain and rollback plans".to_owned(),
                ],
                recovery_contract: "future apply path must keep backup snapshot before handoff"
                    .to_owned(),
            },
            StabilityScenario {
                id: "STAB-RECOVERY-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "durable runtime recovery can scan, redrive, and audit restart evidence"
                    .to_owned(),
                evidence: vec![
                    "recover_task_store_with_audit covers clean start".to_owned(),
                    "recover_task_store_with_redrive_and_audit covers restart redrive".to_owned(),
                    "corrupt task store returns stable error".to_owned(),
                ],
                recovery_contract:
                    "recovery never redrives acked events and records durable audit evidence"
                        .to_owned(),
            },
        ]
    }

    /// 将平台、稳定性和文档基线转换为核心 required 门禁。
    fn core_gates(
        &self,
        platforms: &[PlatformReadiness],
        stability: &[StabilityScenario],
    ) -> Vec<ReleaseGate> {
        let platform_status = status_from(platforms.iter().map(|platform| platform.status));
        let stability_status = status_from(stability.iter().map(|scenario| scenario.status));
        vec![
            ReleaseGate {
                id: "REL-PLATFORM-001".to_owned(),
                domain: "cross_platform".to_owned(),
                evidence_kind: EvidenceKind::Declaration,
                status: platform_status,
                required: true,
                summary: "Windows, Linux, and macOS CI matrix smoke commands are release gates"
                    .to_owned(),
                evidence: platforms
                    .iter()
                    .map(|platform| format!("{}:{}", platform.os, platform.status.as_str()))
                    .collect(),
                remediation: vec!["fix platform-specific path or shell behavior before release".to_owned()],
            },
            ReleaseGate {
                id: "REL-STABILITY-001".to_owned(),
                domain: "stability".to_owned(),
                evidence_kind: EvidenceKind::Declaration,
                status: stability_status,
                required: true,
                summary: "task, cancellation, dead-letter, restore, and upgrade recovery paths are auditable"
                    .to_owned(),
                evidence: stability
                    .iter()
                    .map(|scenario| format!("{}:{}", scenario.id, scenario.status.as_str()))
                    .collect(),
                remediation: vec!["add a failing scenario to release check before enabling apply".to_owned()],
            },
            ReleaseGate {
                id: "REL-DOCS-001".to_owned(),
                domain: "docs".to_owned(),
                evidence_kind: EvidenceKind::Declaration,
                status: ReleaseGateStatus::Pass,
                required: true,
                summary: format!("{CURRENT_RELEASE_LABEL} README, version management, GitHub Packages, install/upgrade/uninstall docs, migration, compatibility, and release notes are part of the release surface"),
                evidence: vec![
                    "crates/eva-release/README.md".to_owned(),
                    "docs/en/release/version-management-plan.md".to_owned(),
                    "docs/en/release/github-packages-publishing.md".to_owned(),
                    "docs/en/release/install-upgrade-uninstall.md".to_owned(),
                    "docs/en/release/v1.5-migration-guide.md".to_owned(),
                    "docs/en/release/v1.5-compatibility-policy.md".to_owned(),
                    "docs/en/release/release-notes-v1.11.5.md".to_owned(),
                ],
                remediation: vec!["update docs and i18n validation before tagging release".to_owned()],
            },
        ]
    }
}

/// 将 verified envelope 的真实分类绑定回对应的外部 evidence gate。
fn set_gate_evidence_kind(
    gates: &mut [ReleaseGate],
    gate_id: &str,
    evidence_kind: Option<EvidenceKind>,
) -> Result<(), EvaError> {
    let Some(evidence_kind) = evidence_kind else {
        return Ok(());
    };
    let gate = gates
        .iter_mut()
        .find(|gate| gate.id == gate_id)
        .ok_or_else(|| {
            EvaError::internal("verified release evidence gate was not generated")
                .with_context("gate_id", gate_id)
        })?;
    gate.evidence_kind = evidence_kind;
    Ok(())
}

/// production scope 只允许 required gate 由机器 measurement 支撑。
fn enforce_production_measurement_gates(report: &mut ReleaseReadinessReport) {
    let mut blocked_reasons = Vec::new();
    for gate in &mut report.gates {
        if gate.required && gate.evidence_kind != EvidenceKind::Measurement {
            gate.status = ReleaseGateStatus::Blocked;
            let reason = format!(
                "evidence_kind_not_measured:{}:{}",
                gate.id, gate.evidence_kind
            );
            if !gate.remediation.contains(&reason) {
                gate.remediation.push(reason.clone());
            }
            blocked_reasons.push(reason);
        }
    }
    report.audit.extend(blocked_reasons);
    report.status = if report
        .gates
        .iter()
        .any(|gate| gate.required && gate.status.is_blocking())
    {
        "blocked"
    } else {
        "ready"
    }
    .to_owned();
}

/// 将 MCP crate 的非循环依赖分类映射为统一 release evidence kind。
const fn mcp_evidence_kind(kind: McpCompatibilityEvidenceKind) -> EvidenceKind {
    match kind {
        McpCompatibilityEvidenceKind::Fixture => EvidenceKind::Fixture,
        McpCompatibilityEvidenceKind::Measurement => EvidenceKind::Measurement,
    }
}

/// 合并内置基线与实际提供的外部证据审计记录。
///
/// 未提供的可选报告不会生成虚假的 verified 标记；已提供报告的原始审计会完整追加，
/// 使最终报告可追溯到扫描器、工件、分发和基准来源。
fn release_audit(
    artifact_report: Option<&ReleaseArtifactVerificationReport>,
    distribution_report: Option<&ReleaseDistributionVerificationReport>,
    security_scan_report: Option<&ReleaseSecurityScanVerificationReport>,
    benchmark_report: Option<&ReleaseBenchmarkVerificationReport>,
) -> Vec<String> {
    let mut audit = vec![
        "release:readiness:v1.11.5-alpha".to_owned(),
        "no_unauthorized_destructive_restore_or_process_switch".to_owned(),
        "all_external_capability_checks_are_plan_or_probe_first".to_owned(),
        "durable_backend_layout_baseline_ready".to_owned(),
        "durable_eventbus_redrive_baseline_ready".to_owned(),
        "durable_task_audit_artifact_baseline_ready".to_owned(),
        "durable_runtime_recovery_checkpoint_ready".to_owned(),
        "durable_diagnostics_smoke_ready".to_owned(),
        "lua_vm_execution_boundary_ready".to_owned(),
        "lua_host_bindings_ready".to_owned(),
        "lua_resource_limits_ready".to_owned(),
        "lua_hot_reload_lifecycle_ready".to_owned(),
        "signed_backup_archive_baseline_ready".to_owned(),
        "restore_apply_gate_baseline_ready".to_owned(),
        "supervisor_handoff_baseline_ready".to_owned(),
        "service_manager_abstraction_ready".to_owned(),
        "daemon_runtime_readiness_gate_ready".to_owned(),
        "mcp_compatibility_matrix_ready".to_owned(),
        "provider_supervision_readiness_gate_ready".to_owned(),
        "hardware_safety_release_gate_ready".to_owned(),
        "observability_policy_release_gate_ready".to_owned(),
        "public_json_contract_diff_ready".to_owned(),
        "v1x_closure_report_ready".to_owned(),
    ];
    if let Some(report) = artifact_report {
        if report.status == "verified" {
            audit.push("signed_artifact_provenance_verified".to_owned());
        } else {
            audit.push("signed_artifact_provenance_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    if let Some(report) = distribution_report {
        if report.status == "verified" {
            audit.push("distribution_install_smoke_verified".to_owned());
        } else {
            audit.push("distribution_install_smoke_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    if let Some(report) = security_scan_report {
        if report.status == "verified" {
            audit.push("external_security_scan_verified".to_owned());
        } else {
            audit.push("external_security_scan_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    if let Some(report) = benchmark_report {
        if report.status == "verified" {
            audit.push("production_benchmark_verified".to_owned());
        } else {
            audit.push("production_benchmark_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    audit
}

/// 将签名工件和来源证明验证报告转换为 required 发布门禁。
fn release_artifact_provenance_gate(report: &ReleaseArtifactVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("artifact:{}", report.artifact_name),
        format!("target:{}", report.target),
        format!("digest:{}", report.artifact_digest),
        format!("source_commit:{}", report.source_commit),
        format!("signature_verified:{}", report.signature_verified),
        format!("provenance_verified:{}", report.provenance_verified),
    ];
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-ARTIFACT-PROVENANCE-001".to_owned(),
        domain: "release_artifact_provenance".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.1 signed release artifact and provenance evidence are verified".to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
    }
}

/// 将多平台分发验证报告转换为 required 发布门禁。
fn release_distribution_gate(report: &ReleaseDistributionVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("version:{}", report.version),
        format!("source_commit:{}", report.source_commit),
        format!("install_docs_verified:{}", report.install_docs_verified),
        format!(
            "package_dry_runs_verified:{}",
            report.package_dry_runs_verified
        ),
    ];
    evidence.extend(report.platform_smokes.iter().map(|smoke| {
        format!(
            "install_smoke:{}:{}:{}:{}",
            smoke.os, smoke.target, smoke.artifact, smoke.status
        )
    }));
    evidence.extend(report.package_dry_runs.iter().map(|dry_run| {
        format!(
            "package_dry_run:{}:{}:{}",
            dry_run.manager, dry_run.target, dry_run.status
        )
    }));
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-DISTRIBUTION-001".to_owned(),
        domain: "release_distribution".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.2 cross-platform installer smoke and package-manager dry-run evidence are verified".to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
    }
}

/// 将外部扫描验证报告转换为 required 安全门禁。
fn release_security_scan_gate(report: &ReleaseSecurityScanVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("scanner:{}", report.scanner),
        format!("scanner_version:{}", report.scanner_version),
        format!("scan_status:{}", report.scan_status),
        format!("source_commit:{}", report.source_commit),
        format!("finding_count:{}", report.findings.len()),
        format!("blocking_findings:{}", report.blocking_findings.len()),
    ];
    evidence.extend(report.findings.iter().map(|finding| {
        format!(
            "finding:{}:{}:{}",
            finding.id,
            finding.package,
            finding.severity.as_str()
        )
    }));
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-SECURITY-SCAN-001".to_owned(),
        domain: "external_security_scan".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.3 external security scanner evidence has no high or critical findings"
            .to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
    }
}

/// 将生产基准验证报告转换为 required 性能门禁。
fn release_benchmark_gate(report: &ReleaseBenchmarkVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("benchmark_status:{}", report.benchmark_status),
        format!("source_commit:{}", report.source_commit),
        format!("measurement_count:{}", report.measurements.len()),
        format!("regression_count:{}", report.regressions.len()),
    ];
    evidence.extend(report.measurements.iter().map(|measurement| {
        format!(
            "measurement:{}:{}ms/{}ms_policy:{}ms_claimed:{}samples",
            measurement.component,
            measurement.observed_ms,
            measurement
                .trusted_budget_ms()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            measurement.budget_ms,
            measurement.sample_count
        )
    }));
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-BENCHMARK-001".to_owned(),
        domain: "production_benchmark".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.3 production benchmark evidence stays within configured budgets".to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
    }
}

impl ReleaseReadinessReport {
    /// 统计 required 且处于 Blocked 的门禁数量。
    pub fn blocking_count(&self) -> usize {
        self.gates
            .iter()
            .filter(|gate| gate.required && gate.status.is_blocking())
            .count()
    }

    /// 统计所有处于 Warn 的门禁数量，无论是否 required。
    pub fn warning_count(&self) -> usize {
        self.gates
            .iter()
            .filter(|gate| gate.status.is_warning())
            .count()
    }
}

/// 以 Blocked 优先、Warn 次之的规则归并一组门禁状态。
///
/// 空输入按 Pass 处理；调用方只应对已经建立存在性约束的内置集合使用该函数。
fn status_from<I>(statuses: I) -> ReleaseGateStatus
where
    I: IntoIterator<Item = ReleaseGateStatus>,
{
    let mut has_warn = false;
    for status in statuses {
        if status.is_blocking() {
            return ReleaseGateStatus::Blocked;
        }
        has_warn |= status.is_warning();
    }
    if has_warn {
        ReleaseGateStatus::Warn
    } else {
        ReleaseGateStatus::Pass
    }
}

/// 将内置安全发现映射为发布门禁，高/严重级别才标为 required。
fn security_gate(finding: &SecurityFinding) -> ReleaseGate {
    ReleaseGate {
        id: finding.id.clone(),
        domain: format!("security:{}", finding.boundary),
        evidence_kind: EvidenceKind::Declaration,
        status: finding.status,
        required: finding.severity.is_required_gate(),
        summary: finding.summary.clone(),
        evidence: finding.evidence.clone(),
        remediation: finding.remediation.clone(),
    }
}

/// 将一个性能预算映射为 required 发布门禁。
fn performance_gate(budget: &PerformanceBudget) -> ReleaseGate {
    let summary = match budget.observation.as_ref() {
        Some(observation) if budget.is_measured() => format!(
            "{} measured {}ms against {}ms budget",
            budget.component, observation.observed_ms, budget.budget_ms
        ),
        Some(observation) => format!(
            "{} has synthetic {}ms estimate against {}ms budget and remains unmeasured",
            budget.component, observation.observed_ms, budget.budget_ms
        ),
        None => format!(
            "{} has a {}ms budget with no runtime measurement",
            budget.component, budget.budget_ms
        ),
    };
    let mut evidence = vec![
        format!("performance.observation_kind:{}", budget.observation_kind()),
        budget.evidence.clone(),
    ];
    if let Some(observation) = &budget.observation {
        evidence.push(observation.evidence.clone());
    }
    let remediation = if !budget.is_measured() {
        vec![
            "capture and attach a trusted benchmark measurement before production release"
                .to_owned(),
        ]
    } else if budget.status.is_blocking() {
        vec!["investigate regression before widening public performance budget".to_owned()]
    } else {
        Vec::new()
    };
    ReleaseGate {
        id: format!("PERF-{}", budget.component.replace('.', "-").to_uppercase()),
        domain: "performance".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: budget.status,
        required: true,
        summary,
        evidence,
        remediation,
    }
}

/// 根据是否存在破坏性变化构造迁移兼容门禁。
fn migration_gate(guide: &MigrationGuide) -> ReleaseGate {
    ReleaseGate {
        id: "REL-MIGRATION-001".to_owned(),
        domain: "migration".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: if guide.breaking_changes.is_empty() {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Warn
        },
        required: true,
        summary: format!(
            "migration {} -> {} has {} breaking changes",
            guide.from_version,
            guide.to_version,
            guide.breaking_changes.len()
        ),
        evidence: guide
            .steps
            .iter()
            .map(|step| step.command.clone())
            .collect(),
        remediation: vec!["document and test every breaking change before release".to_owned()],
    }
}

/// 构造持久化后端布局和恢复边界门禁。
fn durable_backend_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-BACKEND-001".to_owned(),
        domain: "durable_backend".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.1 durable backend schema, layout, read-only verification, and migration lock baseline are implemented".to_owned(),
        evidence: vec![
            "crates/eva-storage/src/durable_backend.rs".to_owned(),
            "cargo test -p eva-storage".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.1 Done".to_owned(),
            "docs/en/release/release-notes-v1.6.1.md".to_owned(),
        ],
        remediation: vec![
            "do not build V1.6.2 durable EventBus on an unverified backend manifest or migration lock".to_owned(),
        ],
    }
}

/// 构造持久 EventBus 重放与死信处理门禁。
fn durable_eventbus_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-EVENTBUS-001".to_owned(),
        domain: "durable_eventbus".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.2 durable EventBus publish/ack/fail, queryable dead-letter store, and redrive baseline are implemented".to_owned(),
        evidence: vec![
            "crates/eva-storage/src/event_log.rs FileSystemEventLog".to_owned(),
            "crates/eva-eventbus/src/durable.rs DurableEventBus".to_owned(),
            "cargo test -p eva-storage".to_owned(),
            "cargo test -p eva-eventbus".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.2 Done".to_owned(),
            "docs/en/release/release-notes-v1.6.2.md".to_owned(),
        ],
        remediation: vec![
            "do not build crash recovery on top of EventBus records without preserving publish/ack/fail and dead-letter redrive round trips".to_owned(),
        ],
    }
}

/// 构造持久任务审计工件门禁。
fn durable_task_audit_artifact_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-STORES-001".to_owned(),
        domain: "durable_task_audit_artifact".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.3 durable task store adapter, audit sink, and artifact metadata hardening are implemented".to_owned(),
        evidence: vec![
            "crates/eva-storage/src/task_state.rs FileSystemTaskStateStore::from_durable_layout".to_owned(),
            "crates/eva-storage/src/audit_store.rs FileSystemAuditSink".to_owned(),
            "crates/eva-storage/src/artifact_store.rs FileSystemArtifactStore v2 metadata".to_owned(),
            "cargo test -p eva-storage".to_owned(),
            "cargo test -p eva-cli task_commands_can_use_durable_backend_task_store".to_owned(),
            "cargo test -p eva-backup".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.3 Done".to_owned(),
            "docs/en/release/release-notes-v1.6.3.md".to_owned(),
        ],
        remediation: vec![
            "do not start V1.6.4 crash recovery without preserving durable task snapshot, audit query, and artifact metadata corruption tests".to_owned(),
        ],
    }
}

/// 构造运行时重启恢复和重投证据门禁。
fn durable_runtime_recovery_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-RECOVERY-001".to_owned(),
        domain: "durable_runtime_recovery".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.4 runtime recovery scanner, event redrive checkpoint, and durable recovery audit smoke are implemented".to_owned(),
        evidence: vec![
            "crates/eva-runtime/src/recovery.rs RuntimeRecoveryCoordinator".to_owned(),
            "crates/eva-eventbus/src/durable.rs DurableEventBus::redrive_dead_letter".to_owned(),
            "crates/eva-observability/src/audit.rs AuditAction::RuntimeRecovered".to_owned(),
            "cargo test -p eva-runtime recovery".to_owned(),
            "cargo test -p eva-eventbus durable".to_owned(),
            "cargo test -p eva-cli recovery".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not enable provider process recovery without preserving ack skip, redrive policy, and durable audit tests".to_owned(),
        ],
    }
}

/// 构造持久诊断命令与损坏数据失败语义门禁。
fn durable_diagnostics_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-DIAGNOSTICS-001".to_owned(),
        domain: "durable_diagnostics".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.5 durable backend diagnostics report schema, migration, and pending redrive counts through inspect.durable".to_owned(),
        evidence: vec![
            "crates/eva-runtime/src/diagnostics.rs inspect_durable_backend".to_owned(),
            "crates/eva-cli/src/run.rs inspect.durable JSON envelope".to_owned(),
            "cargo test -p eva-runtime diagnostics".to_owned(),
            "cargo test -p eva-cli inspect_durable".to_owned(),
            "cargo run -- inspect durable --durable-backend .eva/ci-durable --output json".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.5 Done".to_owned(),
        ],
        remediation: vec![
            "do not remove inspect.durable from CI smoke while durable backend fields are part of the release surface".to_owned(),
        ],
    }
}

/// 构造 Lua 虚拟机真实执行边界门禁。
fn lua_vm_execution_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-VM-EXECUTION-001".to_owned(),
        domain: "lua_vm_execution".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.1 Lua VM adapter, restricted standard library, real on_event execution, and stable error mapping are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/vm.rs LuaVmAdapter and MluaVmAdapter".to_owned(),
            "crates/eva-lua-host/src/bindings.rs real VM execution and compatibility fallback".to_owned(),
            "cargo test -p eva-lua-host".to_owned(),
            "cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability".to_owned(),
            "cargo test -p eva-cli run_basic_example_json_succeeds".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.1 Done"
                .to_owned(),
        ],
        remediation: vec![
            "do not add resource-limit behavior without preserving the LuaVmAdapter boundary and restricted standard library tests".to_owned(),
        ],
    }
}

/// 构造 Lua 受控宿主绑定门禁。
fn lua_host_bindings_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-HOST-BINDINGS-001".to_owned(),
        domain: "lua_host_bindings".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.2 read-only Lua ctx, host log/audit, and ctx.tools.call capability binding are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/bindings.rs run_on_event_with_tools".to_owned(),
            "crates/eva-lua-host/src/vm.rs ctx.tools.call".to_owned(),
            "examples/basic/config/agents/root-agent/main.lua direct config.lint tool call".to_owned(),
            "cargo test -p eva-lua-host".to_owned(),
            "cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability".to_owned(),
            "cargo test -p eva-cli run_basic_example_json_succeeds".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.2 Done".to_owned(),
        ],
        remediation: vec![
            "do not expose raw provider, file, socket, process, memory service, knowledge service, or audit sink handles through Lua ctx".to_owned(),
            "keep unknown and disabled ctx.tools.call capability requests rejected through the host boundary".to_owned(),
        ],
    }
}

/// 构造 Lua 指令、内存和超时资源限制门禁。
fn lua_resource_limits_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-RESOURCE-LIMITS-001".to_owned(),
        domain: "lua_resource_limits".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.3 Lua wall-clock timeout, instruction budget, cancellation token, and memory budget limits are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/vm.rs LuaExecutionLimits".to_owned(),
            "crates/eva-lua-host/src/bindings.rs cancellation and memory-budget host tests".to_owned(),
            "crates/eva-runtime/src/basic.rs BasicRunOptions timeout/cancel Lua limits".to_owned(),
            "cargo test -p eva-lua-host".to_owned(),
            "cargo test -p eva-runtime timeout_basic_run_records_dead_letter_and_replay".to_owned(),
            "cargo test -p eva-runtime cancelled_basic_run_returns_task_record".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.3 Done".to_owned(),
        ],
        remediation: vec![
            "do not add Lua hot reload or generation swap without preserving timeout, instruction budget, cancellation, and memory limit hooks".to_owned(),
            "keep capability calls behind cancellation-aware Lua execution so cancelled scripts cannot continue side effects".to_owned(),
        ],
    }
}

/// 构造 Lua 热重载生命周期和失败回退门禁。
fn lua_hot_reload_lifecycle_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-HOT-RELOAD-001".to_owned(),
        domain: "lua_hot_reload_lifecycle".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.4 Lua shadow load, generation route gating, drain evidence, and rollback audit boundaries remain implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/hot_reload.rs LuaShadowLoader".to_owned(),
            "crates/eva-scheduler/src/generation.rs GenerationRouteGate".to_owned(),
            "crates/eva-lifecycle/src/drain.rs GenerationDrainEvidence".to_owned(),
            "crates/eva-lifecycle/src/rollback.rs plan_generation_lifecycle_rollback".to_owned(),
            "cargo test -p eva-lua-host shadow_load".to_owned(),
            "cargo test -p eva-scheduler generation".to_owned(),
            "cargo test -p eva-lifecycle drain rollback".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not promote a candidate generation unless shadow load is healthy".to_owned(),
            "keep old-generation drain and rollback audit evidence attached to every generation switch".to_owned(),
        ],
    }
}

/// 构造签名及可选密封备份归档门禁。
fn signed_backup_archive_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-BACKUP-ARCHIVE-001".to_owned(),
        domain: "backup_archive".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.10.3 signed backup archive, optional archive sealing, remote target contract, and pre-restore evidence checks are implemented".to_owned(),
        evidence: vec![
            "crates/eva-backup/src/archive.rs BackupArchiveCodec".to_owned(),
            "crates/eva-backup/src/restore_apply.rs PreRestoreBackupEvidence".to_owned(),
            "cargo test -p eva-backup backup_service_can_encrypt_archive_and_record_remote_target".to_owned(),
            "cargo test -p eva-cli restore_apply_dry_run_validates_durable_backup".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.10.3 Done".to_owned(),
        ],
        remediation: vec![
            "do not enable destructive restore unless signed archive verification and pre-restore evidence remain blocking gates".to_owned(),
        ],
    }
}

/// 构造恢复应用的证据、策略、锁、健康和事务回滚门禁。
fn restore_apply_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-RESTORE-APPLY-GATE-001".to_owned(),
        domain: "restore_apply_gate".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.10.4/V1.14.4 restore apply confirmation, policy approval, filesystem lock, health gate, staged file mutation, rollback-required transaction evidence, rollback apply, and operator confirmation are implemented".to_owned(),
        evidence: vec![
            "crates/eva-backup/src/restore_apply.rs RestoreApplyCoordinator, RestoreMutationEngine, and RestoreRollbackEngine".to_owned(),
            "crates/eva-cli/src/run.rs restore apply/rollback --lock-store policy, health, and mutation gate".to_owned(),
            "cargo test -p eva-backup restore_apply".to_owned(),
            "cargo test -p eva-cli restore_apply".to_owned(),
            "cargo test -p eva-cli restore_rollback".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.14.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not execute restore rollback apply unless confirmation, evidence, policy, rollback lock, health, staged plan, transaction log, and pre-restore archive entries still pass".to_owned(),
            "keep mutation_executed explicit: false for no-step plans, true only after staged mutation commits".to_owned(),
        ],
    }
}

/// 构造蓝绿 Supervisor 交接与发布指针顺序门禁。
fn supervisor_handoff_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-SUPERVISOR-HANDOFF-001".to_owned(),
        domain: "supervisor_handoff".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.10.5 supervisor blue-green handoff, release pointer mutation, persisted state, and rollback-on-health-failure baseline are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lifecycle/src/handoff.rs SupervisorHandoffCoordinator".to_owned(),
            "crates/eva-cli/src/run.rs upgrade apply --state-store".to_owned(),
            "cargo test -p eva-lifecycle handoff".to_owned(),
            "cargo test -p eva-cli upgrade_apply".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.10.5 Done".to_owned(),
        ],
        remediation: vec![
            "do not write release pointer without supervisor.handoff and release.pointer_mutation policy approval".to_owned(),
            "keep rollback plans attached to failed candidate health checks".to_owned(),
            "replace local runtime-binary smoke with a production service-manager adapter before daemonized handoff".to_owned(),
        ],
    }
}

/// 构造操作系统服务管理器抽象边界门禁。
fn service_manager_abstraction_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-SERVICE-MANAGER-ABSTRACTION-001".to_owned(),
        domain: "service_manager_abstraction".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.14.5 OS service-manager adapter trait, typed config, fake handoff, and rollback evidence are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lifecycle/src/service_manager.rs ServiceManagerAdapter and FakeServiceManagerAdapter".to_owned(),
            "crates/eva-config/src/eva_yaml.rs service_manager typed config".to_owned(),
            "cargo test -p eva-lifecycle service_manager".to_owned(),
            "cargo test -p eva-config service_manager".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.14.5 Done".to_owned(),
        ],
        remediation: vec![
            "keep fake adapter limited to local tests and explicit fake config".to_owned(),
            "do not claim Windows Service, systemd, or launchd handoff until V1.14.6 platform adapters pass controlled tests".to_owned(),
        ],
    }
}

/// 构造守护进程启动、状态、关闭和恢复就绪门禁。
fn daemon_runtime_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DAEMON-RUNTIME-001".to_owned(),
        domain: "daemon_runtime".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.12 daemon process boundary, filesystem mailbox control, durable task lifecycle, scheduler retry tick, and daemon-backed agent drain/reload mutation readiness are implemented".to_owned(),
        evidence: vec![
            "crates/eva-runtime/src/daemon.rs start_daemon and run_control_loop".to_owned(),
            "crates/eva-runtime/src/scheduler_retry.rs run_scheduler_retry_tick".to_owned(),
            "crates/eva-cli/src/run/daemon_cmd.rs daemon start/status/shutdown/submit/cancel/drain/reload".to_owned(),
            "crates/eva-cli/src/run/agent_cmd.rs daemon-backed agent drain/reload fallback".to_owned(),
            "cargo test -p eva-runtime daemon_control_status_and_shutdown_round_trip_has_trace_id".to_owned(),
            "cargo test -p eva-runtime daemon_control_loop_ticks_scheduler_retry_once".to_owned(),
            "cargo test -p eva-runtime daemon_drain_mutates_agent_control_state".to_owned(),
            "cargo test -p eva-runtime daemon_reload_mutates_generation_route_state".to_owned(),
            "cargo test -p eva-cli daemon_control_status_and_shutdown_round_trip_via_cli".to_owned(),
            "cargo test -p eva-cli agent_drain_and_reload_use_daemon_mutation_when_available".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.12.6 Done".to_owned(),
        ],
        remediation: vec![
            "keep daemon readiness limited to local foreground/filesystem control until OS service-manager adapters exist".to_owned(),
            "do not claim production hot reload or provider supervision without provider execution-state recovery tests".to_owned(),
        ],
    }
}

/// 根据 MCP 兼容矩阵是否存在且验证通过构造门禁。
///
/// 缺失报告直接 Blocked；存在报告时只有总体兼容状态通过才 Pass，并保留各客户端
/// 证据。这样“未运行矩阵”不会被误解为“没有不兼容项”。
fn mcp_compatibility_matrix_gate(report: Option<&McpCompatibilityReport>) -> ReleaseGate {
    match report {
        Some(report) => {
            let status = if report.status == "compatible" {
                ReleaseGateStatus::Pass
            } else {
                ReleaseGateStatus::Blocked
            };
            let mut evidence = report.audit.clone();
            evidence.extend(
                report
                    .failures
                    .iter()
                    .map(|failure| format!("mcp.compat.failure:{failure}")),
            );
            ReleaseGate {
                id: "REL-MCP-COMPAT-001".to_owned(),
                domain: "mcp_compatibility".to_owned(),
                evidence_kind: mcp_evidence_kind(report.evidence_kind()),
                status,
                required: true,
                summary: "MCP transport, schema, stream lifecycle, and server-surface compatibility matrix is present".to_owned(),
                evidence,
                remediation: if status.is_blocking() {
                    vec![
                        "provide a passing MCP compatibility matrix before release".to_owned(),
                        "cover stdio/http JSON-RPC, stream abort/cleanup, schema support, and explicit-tool server gate".to_owned(),
                    ]
                } else {
                    Vec::new()
                },
            }
        }
        None => ReleaseGate {
            id: "REL-MCP-COMPAT-001".to_owned(),
            domain: "mcp_compatibility".to_owned(),
            evidence_kind: EvidenceKind::Declaration,
            status: ReleaseGateStatus::Blocked,
            required: true,
            summary: "MCP compatibility matrix is required before release".to_owned(),
            evidence: vec!["mcp.compatibility_matrix:missing".to_owned()],
            remediation: vec![
                "generate or attach an MCP compatibility matrix fixture".to_owned(),
                "verify stream abort/cleanup and explicit-tool server gate evidence".to_owned(),
            ],
        },
    }
}

/// 构造外部 Provider 启动、监控与退出隔离门禁。
fn provider_supervision_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-PROVIDER-SUPERVISION-001".to_owned(),
        domain: "provider_supervision".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "Provider stdio/http/MCP/Skill supervision, credential scope, admission limits, stream artifacts, recovery, and MCP compatibility gates are present".to_owned(),
        evidence: vec![
            "provider.supervisor_slot:stdio,http,mcp,skill".to_owned(),
            "provider.process_table:in_memory_snapshot_and_durable_mirror".to_owned(),
            "provider.credential_session:scoped_env_header_token_redaction".to_owned(),
            "provider.admission:concurrency_rate_circuit_backoff".to_owned(),
            "provider.stream_artifact:bounded_redacted_summary".to_owned(),
            "provider.recovery:daemon_restart_interrupted_backoff_evidence".to_owned(),
            "provider.mcp_compatibility_gate:REL-MCP-COMPAT-001".to_owned(),
            "provider.os_process_supervisor:not_claimed".to_owned(),
            "cargo test -p eva-adapter supervisor runtime::tests stream::tests".to_owned(),
            "cargo test -p eva-runtime recovery".to_owned(),
            "cargo test -p eva-storage provider_process".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.13.8 Done".to_owned(),
        ],
        remediation: Vec::new(),
    }
}

/// 构造硬件权限、租约、模拟器和热插拔安全门禁。
fn hardware_safety_release_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-HARDWARE-SAFETY-001".to_owned(),
        domain: "hardware_safety".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.15.5 hardware safety release gate records simulator parity, permission denial, lease cleanup, and hotplug smoke evidence for alpha".to_owned(),
        evidence: vec![
            "hardware.safety.release_mode:alpha_simulator_only".to_owned(),
            "simulator_parity:run_simulator_contract_suite rejects raw I/O and capability bypass".to_owned(),
            "permission_denial:PlatformOsPermissionProvider reports remediation and raw_device_path_exposed=false before lease claim".to_owned(),
            "lease_cleanup:driver crash and hotplug watcher crash release active hardware leases".to_owned(),
            "hotplug_smoke:daemon manifest snapshot subscriber publishes typed events, persists hardware-hotplug.state, and reports raw_handles_exposed=false".to_owned(),
            "real_hardware_fixture:not_required_for_alpha".to_owned(),
            "cargo test -p eva-hardware simulator_contract_suite_rejects_raw_io_and_capability_bypass".to_owned(),
            "cargo test -p eva-hardware permission".to_owned(),
            "cargo test -p eva-hardware hotplug".to_owned(),
            "cargo test -p eva-runtime daemon_hotplug_subscriber_persists_state_across_restart".to_owned(),
            "cargo test -p eva-cli daemon_start_foreground_smoke_reports_verified_boundaries".to_owned(),
        ],
        remediation: vec![
            "production releases must attach real or virtual hardware fixture evidence before claiming real USB/serial/BLE/socket/vendor SDK I/O".to_owned(),
            "do not remove permission denial, lease cleanup, raw handle suppression, or hotplug smoke checks when replacing simulator-only alpha evidence".to_owned(),
        ],
    }
}

/// 构造公共 JSON 信封和兼容性差异套件门禁。
fn public_json_contract_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-JSON-CONTRACT-001".to_owned(),
        domain: "cli_json_contract".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "Public CLI JSON envelope and command data contracts are protected by additive-compatible golden subset diffs".to_owned(),
        evidence: vec![
            "scripts/validate-cli-json-contracts.ps1".to_owned(),
            "contracts/cli-json/version.json".to_owned(),
            "contracts/cli-json/run-basic.json".to_owned(),
            "contracts/cli-json/capability-list.json".to_owned(),
            "contracts/cli-json/hardware-bind.json".to_owned(),
            "contracts/cli-json/restore-plan.json".to_owned(),
            "contracts/cli-json/upgrade-check.json".to_owned(),
            "contracts/cli-json/release-check.json".to_owned(),
            "contract.diff:golden_subset_allows_additive_fields".to_owned(),
            "contract.diff:missing_or_renamed_fields_block".to_owned(),
        ],
        remediation: vec![
            "run ./scripts/validate-cli-json-contracts.ps1 before release".to_owned(),
            "treat removed or renamed JSON fields as breaking unless a new compatibility window is documented".to_owned(),
        ],
    }
}

/// 构造可观测性字段、脱敏、导出与保留策略门禁。
fn observability_policy_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-OBSERVABILITY-POLICY-001".to_owned(),
        domain: "observability_policy".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "Runtime observability audit wiring, tracing bridge, OTLP exporter smoke, and retention policy are recorded for V1.x closure".to_owned(),
        evidence: vec![
            "runtime_audit_sink_wiring_v1.16.1".to_owned(),
            "tracing_subscriber_bridge_v1.16.2".to_owned(),
            "opentelemetry_sdk_exporter_v1.16.3".to_owned(),
            "observability_retention_policy_v1.16.4".to_owned(),
            "BestEffortObservabilityPipeline covers daemon/provider/task/restore paths".to_owned(),
            "FileObservabilitySink retention/rotation/corrupt-record policy is tested".to_owned(),
            "database_sink:policy_kind_only_not_claimed_as_real_backend".to_owned(),
        ],
        remediation: vec![
            "do not claim production telemetry until a real database sink and retention scheduler are implemented".to_owned(),
            "keep tracing, OTLP exporter smoke, and JSONL retention tests in the V1.x release evidence".to_owned(),
        ],
    }
}

/// 返回 V1.x alpha 闭环必须明确存在并通过的稳定门禁标识集合。
fn v1x_closure_required_gate_ids() -> Vec<&'static str> {
    vec![
        "REL-DAEMON-RUNTIME-001",
        "REL-MCP-COMPAT-001",
        "REL-PROVIDER-SUPERVISION-001",
        "REL-RESTORE-APPLY-GATE-001",
        "REL-SERVICE-MANAGER-ABSTRACTION-001",
        "REL-HARDWARE-SAFETY-001",
        "REL-OBSERVABILITY-POLICY-001",
        "REL-JSON-CONTRACT-001",
    ]
}

/// 对必需门禁执行显式存在性和 Pass 状态检查，生成闭环报告。
///
/// Warn 也被视为未闭环，而不是仅检查 Blocked；缺失门禁单独报告。只有所有预定义
/// 内部门禁均 Pass 时状态才是 ready_with_external_blockers。生产签名凭据、分发仓库、
/// 平台服务环境、硬件夹具和数据库 sink 保留在外部阻塞列表，不影响 alpha 内部闭环，
/// 但报告也不会声称这些生产条件已完成。
fn v1x_closure_report(gates: &[ReleaseGate]) -> V1xClosureReport {
    let required_gate_ids: Vec<String> = v1x_closure_required_gate_ids()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut passed_required_gate_ids = Vec::new();
    let mut missing_required_gate_ids = Vec::new();
    let mut blocking_required_gate_ids = Vec::new();

    for gate_id in &required_gate_ids {
        match gates.iter().find(|gate| gate.id == *gate_id) {
            Some(gate) if gate.status == ReleaseGateStatus::Pass => {
                passed_required_gate_ids.push(gate_id.clone());
            }
            Some(gate) if gate.status.is_blocking() => {
                blocking_required_gate_ids.push(gate_id.clone());
            }
            Some(_) => blocking_required_gate_ids.push(gate_id.clone()),
            None => missing_required_gate_ids.push(gate_id.clone()),
        }
    }

    let status = if missing_required_gate_ids.is_empty() && blocking_required_gate_ids.is_empty() {
        "ready_with_external_blockers"
    } else {
        "blocked"
    }
    .to_owned();

    V1xClosureReport {
        status,
        summary: "V1.x alpha closure covers daemon, MCP, provider supervision, restore, service-manager abstraction, hardware safety, observability policy, and public JSON contract gates while recording production-only blockers separately".to_owned(),
        required_gate_ids,
        passed_required_gate_ids,
        missing_required_gate_ids,
        blocking_required_gate_ids,
        optional_production_gate_ids: vec![
            "REL-ARTIFACT-PROVENANCE-001".to_owned(),
            "REL-DISTRIBUTION-001".to_owned(),
            "REL-SECURITY-SCAN-001".to_owned(),
            "REL-BENCHMARK-001".to_owned(),
        ],
        blocked_external_items: vec![
            "production_signing_attestation_credentials".to_owned(),
            "homebrew_winget_apt_repository_credentials".to_owned(),
            "platform_service_manager_test_environment".to_owned(),
            "real_or_virtual_hardware_fixture".to_owned(),
            "production_database_sink_and_retention_scheduler".to_owned(),
        ],
    }
}

/// 将闭环报告转换为最终 required 门禁，并展开内部和外部证据。
fn v1x_closure_gate(closure: &V1xClosureReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("closure.status:{}", closure.status),
        format!(
            "closure.required_gate_count:{}",
            closure.required_gate_ids.len()
        ),
        format!(
            "closure.passed_required_gate_count:{}",
            closure.passed_required_gate_ids.len()
        ),
        format!(
            "closure.missing_required_gate_count:{}",
            closure.missing_required_gate_ids.len()
        ),
        format!(
            "closure.blocking_required_gate_count:{}",
            closure.blocking_required_gate_ids.len()
        ),
    ];
    evidence.extend(
        closure
            .required_gate_ids
            .iter()
            .map(|gate_id| format!("closure.required_gate:{gate_id}")),
    );
    evidence.extend(
        closure
            .blocked_external_items
            .iter()
            .map(|item| format!("closure.external_blocker:{item}")),
    );

    ReleaseGate {
        id: "REL-V1X-CLOSURE-001".to_owned(),
        domain: "v1x_closure".to_owned(),
        evidence_kind: EvidenceKind::Declaration,
        status: if closure.status == "blocked" {
            ReleaseGateStatus::Blocked
        } else {
            ReleaseGateStatus::Pass
        },
        required: true,
        summary: "V1.x closure report aggregates completed readiness gates and records external production blockers without claiming them complete".to_owned(),
        evidence,
        remediation: vec![
            "resolve missing or blocking required gates before claiming V1.x closure".to_owned(),
            "keep production-only blockers listed until credentials, platform service tests, hardware fixtures, and database sink exist".to_owned(),
        ],
    }
}

/// 返回每个平台发布矩阵必须运行的基础烟雾命令。
fn smoke_commands() -> Vec<String> {
    vec![
        "cargo fmt --check".to_owned(),
        "cargo clippy --workspace --all-targets -- -D warnings".to_owned(),
        "cargo test --workspace".to_owned(),
        "cargo test -p eva-lua-host".to_owned(),
        "./scripts/validate-cli-json-contracts.ps1".to_owned(),
        "cargo run -- --version".to_owned(),
        "cargo run -- inspect durable --durable-backend .eva/ci-durable --output json".to_owned(),
        "cargo run -- release check --output json".to_owned(),
    ]
}

#[cfg(test)]
/// 内置门禁、闭环计算及四类外部发布证据的聚合测试。
mod tests {
    use super::*;

    /// 工件和其他外部证据共用的完整来源提交。
    const ARTIFACT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    /// 测试发布工件使用的合法 SHA-256 摘要。
    const ARTIFACT_DIGEST: &str =
        "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";

    /// 构造供 scoped checklist 边界验证 scope 的最小统一清单。
    fn evidence_manifest(scope: ReleaseEvidenceScope) -> ReleaseEvidenceManifest {
        ReleaseEvidenceManifest::new(
            scope,
            ARTIFACT_COMMIT,
            vec![crate::evidence::ReleaseEvidenceManifestEntry::new(
                crate::evidence::ReleaseEvidenceType::Benchmark,
                "benchmark.evidence",
                "benchmark.envelope",
                None,
            )
            .unwrap()],
        )
        .unwrap()
    }

    /// 构造可选择 signed 标志的发布工件证据。
    fn artifact_evidence(signed: bool) -> ReleaseArtifactEvidence {
        let key = ReleaseArtifactSigningKey::local_development();
        let artifact = crate::artifact::ReleaseArtifactSubject::new(
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "eva",
            ARTIFACT_DIGEST,
            4096,
            signed,
        )
        .unwrap();
        let provenance = crate::artifact::ReleaseProvenanceEvidence::new(
            "github-actions",
            ARTIFACT_COMMIT,
            "cargo-build-release-locked-bin-eva",
            "release",
            "spdx:release-evidence/eva.spdx.json",
            "passed",
        )
        .unwrap();
        let signature = crate::artifact::ReleaseArtifactSignature::new(
            key.key_id(),
            crate::artifact::RELEASE_SIGNATURE_ALGORITHM,
            "pending",
        )
        .unwrap();
        let mut evidence = ReleaseArtifactEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            artifact,
            provenance,
            signature,
        )
        .unwrap();
        evidence.signature = evidence.sign(&key);
        evidence
    }

    /// 构造指定操作系统和状态的安装烟雾证据。
    fn install_smoke(
        os: &str,
        target: &str,
        artifact: &str,
        package_format: &str,
        status: &str,
    ) -> crate::distribution::ReleaseInstallSmokeEvidence {
        crate::distribution::ReleaseInstallSmokeEvidence::new(
            os,
            target,
            artifact,
            package_format,
            format!("install {artifact}"),
            "eva --version",
            format!("uninstall {artifact}"),
            format!("upgrade {artifact}"),
            status,
        )
        .unwrap()
    }

    /// 构造指定状态的包管理器演练证据。
    fn package_dry_run(status: &str) -> crate::distribution::ReleasePackageDryRunEvidence {
        crate::distribution::ReleasePackageDryRunEvidence::new(
            "ghcr",
            "ghcr.io/yetmos/eva-cli",
            "linux/amd64+linux/arm64",
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.11.5-alpha",
            status,
        )
        .unwrap()
    }

    /// 构造覆盖三平台的分发证据，并允许替换包演练状态。
    fn distribution_evidence(
        package_status: &str,
    ) -> crate::distribution::ReleaseDistributionEvidence {
        crate::distribution::ReleaseDistributionEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            vec![
                install_smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.11.5-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                    "passed",
                ),
                install_smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                    "passed",
                ),
                install_smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.11.5-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                    "passed",
                ),
            ],
            vec![package_dry_run(package_status)],
        )
        .unwrap()
    }

    /// 构造指定严重级别的外部安全扫描发现。
    fn security_scan_finding(severity: &str) -> crate::scanner::ReleaseSecurityScanFinding {
        crate::scanner::ReleaseSecurityScanFinding::new(
            "RUSTSEC-0000-0000",
            "demo-crate",
            "1.0.0",
            severity,
            "demo advisory",
            "upgrade demo-crate",
        )
        .unwrap()
    }

    /// 构造指定发现列表的 passed 安全扫描证据。
    fn security_scan_evidence(
        status: &str,
        findings: Vec<crate::scanner::ReleaseSecurityScanFinding>,
    ) -> crate::scanner::ReleaseSecurityScanEvidence {
        crate::scanner::ReleaseSecurityScanEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            "cargo-audit",
            "1.0.0",
            status,
            "cargo audit --json",
            findings,
        )
        .unwrap()
    }

    /// 构造指定观测耗时的基准测量。
    fn benchmark_measurement(observed_ms: u64) -> crate::benchmark::ReleaseBenchmarkMeasurement {
        crate::benchmark::ReleaseBenchmarkMeasurement::new(
            "release.check",
            "release check wall time",
            5_000,
            observed_ms,
            3,
            "target/release/eva release check --output json",
            "github-actions-ubuntu-latest",
        )
        .unwrap()
    }

    /// 构造包含单项测量的 passed 基准证据。
    fn benchmark_evidence(
        status: &str,
        observed_ms: u64,
    ) -> crate::benchmark::ReleaseBenchmarkEvidence {
        crate::benchmark::ReleaseBenchmarkEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            status,
            vec![benchmark_measurement(observed_ms)],
        )
        .unwrap()
    }

    const PRODUCTION_TRUSTED_NOW_MS: u128 = 1_784_073_600_000;

    struct ProductionBundleFixture {
        manifest: ReleaseEvidenceManifest,
        artifact: Option<ReleaseArtifactEvidenceCandidate>,
        distribution: Option<ReleaseDocumentEvidenceCandidate<ReleaseDistributionEvidence>>,
        security_scan: Option<ReleaseDocumentEvidenceCandidate<ReleaseSecurityScanEvidence>>,
        benchmark: Option<ReleaseDocumentEvidenceCandidate<ReleaseBenchmarkEvidence>>,
    }

    impl ProductionBundleFixture {
        fn bind_envelope_digests(&mut self) {
            let digests = [
                (
                    ReleaseEvidenceType::Artifact,
                    self.artifact
                        .as_ref()
                        .map(|candidate| candidate.envelope.canonical_digest()),
                ),
                (
                    ReleaseEvidenceType::Distribution,
                    self.distribution
                        .as_ref()
                        .map(|candidate| candidate.envelope.canonical_digest()),
                ),
                (
                    ReleaseEvidenceType::SecurityScan,
                    self.security_scan
                        .as_ref()
                        .map(|candidate| candidate.envelope.canonical_digest()),
                ),
                (
                    ReleaseEvidenceType::Benchmark,
                    self.benchmark
                        .as_ref()
                        .map(|candidate| candidate.envelope.canonical_digest()),
                ),
            ];
            for entry in &mut self.manifest.entries {
                entry.envelope_digest = digests
                    .iter()
                    .find(|(evidence_type, _)| *evidence_type == entry.evidence_type)
                    .and_then(|(_, digest)| digest.clone());
            }
        }

        fn verify(
            self,
            policy: &ProductionEvidencePolicy,
        ) -> Result<VerifiedReleaseEvidenceBundle, EvaError> {
            VerifiedReleaseEvidenceBundle::verify_production(
                self.manifest,
                ARTIFACT_COMMIT,
                self.artifact,
                self.distribution,
                self.security_scan,
                self.benchmark,
                policy,
            )
        }
    }

    fn production_manifest() -> ReleaseEvidenceManifest {
        ReleaseEvidenceManifest::new(
            ReleaseEvidenceScope::Production,
            ARTIFACT_COMMIT,
            vec![
                crate::evidence::ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::Artifact,
                    "artifact.evidence",
                    "artifact.envelope",
                    Some("artifact.tar.gz".to_owned()),
                )
                .unwrap(),
                crate::evidence::ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::Distribution,
                    "distribution.evidence",
                    "distribution.envelope",
                    None,
                )
                .unwrap(),
                crate::evidence::ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::SecurityScan,
                    "security-scan.evidence",
                    "security-scan.envelope",
                    None,
                )
                .unwrap(),
                crate::evidence::ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::Benchmark,
                    "benchmark.evidence",
                    "benchmark.envelope",
                    None,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    fn production_bundle_fixture() -> ProductionBundleFixture {
        let timestamp = PRODUCTION_TRUSTED_NOW_MS - 1_000;
        let mut artifact = artifact_evidence(true);
        artifact.artifact.size_bytes = 2;
        artifact.signature = artifact.sign(&ReleaseArtifactSigningKey::local_development());
        let artifact_envelope = artifact
            .to_envelope(
                EvidenceKind::Measurement,
                "checklist:artifact",
                "ubuntu-x86_64",
                "github-actions:release-artifact/123/1/artifact",
                timestamp,
            )
            .unwrap();
        let distribution = distribution_evidence("passed");
        let distribution_envelope = distribution
            .to_envelope(
                EvidenceKind::Measurement,
                "checklist:distribution",
                "release-matrix",
                "github-actions:release-distribution/123/1/distribution",
                timestamp,
            )
            .unwrap();
        let security_scan = security_scan_evidence("passed", Vec::new());
        let security_scan_envelope = security_scan
            .to_envelope(
                EvidenceKind::Measurement,
                "checklist:security-scan",
                "ubuntu-x86_64",
                "github-actions:release-security-scan/123/1/security",
                timestamp,
            )
            .unwrap();
        let benchmark = benchmark_evidence("passed", 120);
        let benchmark_envelope = benchmark
            .to_envelope(
                EvidenceKind::Measurement,
                "checklist:benchmark",
                "ubuntu-x86_64",
                "github-actions:release-benchmark/123/1/benchmark",
                timestamp,
            )
            .unwrap();

        let mut fixture = ProductionBundleFixture {
            manifest: production_manifest(),
            artifact: Some(ReleaseArtifactEvidenceCandidate::new(
                artifact,
                artifact_envelope,
                b"ok".to_vec(),
            )),
            distribution: Some(ReleaseDocumentEvidenceCandidate::new(
                distribution,
                distribution_envelope,
            )),
            security_scan: Some(ReleaseDocumentEvidenceCandidate::new(
                security_scan,
                security_scan_envelope,
            )),
            benchmark: Some(ReleaseDocumentEvidenceCandidate::new(
                benchmark,
                benchmark_envelope,
            )),
        };
        fixture.bind_envelope_digests();
        fixture
    }

    fn production_policy(fixture: &ProductionBundleFixture) -> ProductionEvidencePolicy {
        ProductionEvidencePolicy::github_actions(
            PRODUCTION_TRUSTED_NOW_MS,
            "123",
            "1",
            fixture.manifest.canonical_digest(),
        )
        .unwrap()
    }

    fn context_contains(error: &EvaError, key: &str, value: &str) -> bool {
        error
            .context()
            .entries()
            .iter()
            .any(|(actual_key, actual_value)| actual_key == key && actual_value.contains(value))
    }

    #[test]
    /// 验证 manifest entry 与实际加载 evidence 不一致时无法构造 trusted bundle。
    fn verified_bundle_rejects_missing_manifest_candidate() {
        let error = VerifiedReleaseEvidenceBundle::verify(
            evidence_manifest(ReleaseEvidenceScope::Alpha),
            ARTIFACT_COMMIT,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();

        assert_eq!(
            error.message(),
            "release evidence manifest entries do not match loaded evidence"
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "entry_type" && value == "benchmark"));
    }

    #[test]
    /// 验证 alpha manifest 保持可选择 coverage 和弱内置证据的兼容行为。
    fn verified_bundle_preserves_alpha_compatibility() {
        let evidence = benchmark_evidence("passed", 120);
        let envelope = evidence
            .to_envelope(
                EvidenceKind::Measurement,
                "checklist:test-benchmark",
                "test-runner",
                "eva-release-tests",
                PRODUCTION_TRUSTED_NOW_MS,
            )
            .unwrap();
        let bundle = VerifiedReleaseEvidenceBundle::verify(
            evidence_manifest(ReleaseEvidenceScope::Alpha),
            ARTIFACT_COMMIT,
            None,
            None,
            None,
            Some(ReleaseDocumentEvidenceCandidate::new(evidence, envelope)),
        )
        .unwrap();
        let report = ReleaseHardeningService::v15()
            .readiness_with_verified_release_evidence("all", &bundle)
            .unwrap();
        let benchmark_gate = report
            .gates
            .iter()
            .find(|gate| gate.id == "REL-BENCHMARK-001")
            .unwrap();
        let mcp_gate = report
            .gates
            .iter()
            .find(|gate| gate.id == "REL-MCP-COMPAT-001")
            .unwrap();
        let performance_gate = report
            .gates
            .iter()
            .find(|gate| gate.id == "PERF-RELEASE-CHECK")
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.evidence_scope, ReleaseEvidenceScope::Alpha);
        assert_eq!(benchmark_gate.evidence_kind, EvidenceKind::Measurement);
        assert_eq!(benchmark_gate.status, ReleaseGateStatus::Pass);
        assert_eq!(mcp_gate.evidence_kind, EvidenceKind::Fixture);
        assert_eq!(mcp_gate.status, ReleaseGateStatus::Pass);
        assert_eq!(performance_gate.status, ReleaseGateStatus::Warn);
        assert!(!mcp_gate
            .remediation
            .iter()
            .any(|item| item.starts_with("evidence_kind_not_measured:")));
    }

    #[test]
    /// 验证 production 不能绕过 consumer-owned policy 调用 alpha verifier。
    fn production_bundle_requires_explicit_consumer_policy() {
        let fixture = production_bundle_fixture();
        let error = VerifiedReleaseEvidenceBundle::verify(
            fixture.manifest,
            ARTIFACT_COMMIT,
            fixture.artifact,
            fixture.distribution,
            fixture.security_scan,
            fixture.benchmark,
        )
        .unwrap_err();

        assert!(context_contains(
            &error,
            "blocked_reasons",
            ProductionEvidenceBlocker::PolicyRequired.as_str()
        ));
    }

    #[test]
    /// 表驱动验证 production coverage、freshness、kind、trust、冲突和完整成功路径。
    fn production_policy_rejects_invalid_bundles_and_accepts_complete_manifest() {
        enum Case {
            Complete,
            OldestAccepted,
            FurthestFutureAccepted,
            Missing,
            Stale,
            Future,
            NonMeasurement,
            Untrusted,
            Conflicting,
        }

        let cases = [
            ("complete", Case::Complete, None),
            ("oldest_accepted", Case::OldestAccepted, None),
            (
                "furthest_future_accepted",
                Case::FurthestFutureAccepted,
                None,
            ),
            (
                "missing",
                Case::Missing,
                Some(ProductionEvidenceBlocker::CoverageMissing),
            ),
            ("stale", Case::Stale, Some(ProductionEvidenceBlocker::Stale)),
            (
                "future",
                Case::Future,
                Some(ProductionEvidenceBlocker::FutureTimestamp),
            ),
            (
                "non_measurement",
                Case::NonMeasurement,
                Some(ProductionEvidenceBlocker::KindNotMeasurement),
            ),
            (
                "untrusted",
                Case::Untrusted,
                Some(ProductionEvidenceBlocker::ExecutorUntrusted),
            ),
            (
                "conflicting",
                Case::Conflicting,
                Some(ProductionEvidenceBlocker::ReleaseIdentityConflict),
            ),
        ];

        for (name, case, expected_blocker) in cases {
            let mut fixture = production_bundle_fixture();
            match case {
                Case::Complete => {}
                Case::OldestAccepted => {
                    fixture.benchmark.as_mut().unwrap().envelope.timestamp =
                        PRODUCTION_TRUSTED_NOW_MS - crate::evidence::PRODUCTION_EVIDENCE_MAX_AGE_MS;
                }
                Case::FurthestFutureAccepted => {
                    fixture.benchmark.as_mut().unwrap().envelope.timestamp =
                        PRODUCTION_TRUSTED_NOW_MS
                            + crate::evidence::PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS;
                }
                Case::Missing => {
                    fixture
                        .manifest
                        .entries
                        .retain(|entry| entry.evidence_type != ReleaseEvidenceType::Benchmark);
                    fixture.benchmark = None;
                }
                Case::Stale => {
                    fixture.benchmark.as_mut().unwrap().envelope.timestamp =
                        PRODUCTION_TRUSTED_NOW_MS
                            - crate::evidence::PRODUCTION_EVIDENCE_MAX_AGE_MS
                            - 1;
                }
                Case::Future => {
                    fixture.benchmark.as_mut().unwrap().envelope.timestamp =
                        PRODUCTION_TRUSTED_NOW_MS
                            + crate::evidence::PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS
                            + 1;
                }
                Case::NonMeasurement => {
                    fixture.benchmark.as_mut().unwrap().envelope.kind = EvidenceKind::Declaration;
                }
                Case::Untrusted => {
                    fixture.benchmark.as_mut().unwrap().envelope.executor =
                        "local:release-benchmark/run-123".to_owned();
                }
                Case::Conflicting => {
                    let benchmark = fixture.benchmark.as_mut().unwrap();
                    benchmark.evidence.source_tag = "v1.11.6-alpha".to_owned();
                    benchmark.envelope = benchmark
                        .evidence
                        .to_envelope(
                            EvidenceKind::Measurement,
                            "checklist:benchmark",
                            "ubuntu-x86_64",
                            "github-actions:release-benchmark/123/1/benchmark",
                            PRODUCTION_TRUSTED_NOW_MS - 1_000,
                        )
                        .unwrap();
                }
            }
            fixture.bind_envelope_digests();
            let policy = production_policy(&fixture);

            match expected_blocker {
                None => {
                    let bundle = fixture
                        .verify(&policy)
                        .unwrap_or_else(|error| panic!("{name}: {error}"));
                    assert_eq!(bundle.scope(), ReleaseEvidenceScope::Production);
                    assert_eq!(bundle.entry_count(), ReleaseEvidenceType::ALL.len());
                    assert!(bundle
                        .verification()
                        .audit
                        .iter()
                        .any(|item| item == "production_evidence.coverage:complete"));
                }
                Some(blocker) => {
                    let error = fixture.verify(&policy).unwrap_err();
                    assert!(
                        context_contains(&error, "blocked_reasons", blocker.as_str()),
                        "{name}: {error:?}"
                    );
                }
            }
        }
    }

    #[test]
    /// 验证重复 subject 与重复 capture identity 都被唯一性 policy 阻断。
    fn production_policy_rejects_duplicate_subject_and_capture_identity() {
        let policy_fixture = production_bundle_fixture();
        let policy = production_policy(&policy_fixture);
        let artifact = EvidenceEnvelope::from_subject_digest(
            EvidenceKind::Measurement,
            "checklist:artifact",
            ARTIFACT_COMMIT,
            "ubuntu-x86_64",
            "github-actions:release-artifact/123/1/artifact",
            PRODUCTION_TRUSTED_NOW_MS,
            ARTIFACT_DIGEST,
        )
        .unwrap();
        let benchmark = EvidenceEnvelope::from_subject_digest(
            EvidenceKind::Measurement,
            "checklist:benchmark",
            ARTIFACT_COMMIT,
            "ubuntu-x86_64",
            "github-actions:release-benchmark/123/1/benchmark",
            PRODUCTION_TRUSTED_NOW_MS,
            ARTIFACT_DIGEST,
        )
        .unwrap();
        let duplicate_subject = verify_production_policy(
            &production_manifest(),
            &policy,
            &[
                (ReleaseEvidenceType::Artifact, Some(&artifact)),
                (ReleaseEvidenceType::Benchmark, Some(&benchmark)),
            ],
            &[],
        )
        .unwrap_err();
        assert!(context_contains(
            &duplicate_subject,
            "blocked_reasons",
            ProductionEvidenceBlocker::SubjectDuplicate.as_str()
        ));

        let common_rules = ReleaseEvidenceType::ALL
            .into_iter()
            .map(|evidence_type| {
                crate::evidence::ProductionEvidenceExecutorRule::prefix(
                    evidence_type,
                    "trusted:release/",
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let common_context = crate::evidence::ProductionEvidenceContext::new(
            PRODUCTION_TRUSTED_NOW_MS,
            CURRENT_RELEASE_VERSION,
            format!("v{CURRENT_RELEASE_VERSION}"),
            "123",
            "1",
            production_manifest().canonical_digest(),
        )
        .unwrap();
        let common_policy = ProductionEvidencePolicy::new(
            common_context,
            crate::evidence::PRODUCTION_EVIDENCE_MAX_AGE_MS,
            crate::evidence::PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS,
            common_rules,
        )
        .unwrap();
        let first = EvidenceEnvelope::from_subject_digest(
            EvidenceKind::Measurement,
            "checklist:shared-capture",
            ARTIFACT_COMMIT,
            "ubuntu-x86_64",
            "trusted:release/123/1/shared",
            PRODUCTION_TRUSTED_NOW_MS,
            ARTIFACT_DIGEST,
        )
        .unwrap();
        let second = EvidenceEnvelope::from_subject_digest(
            EvidenceKind::Measurement,
            "checklist:shared-capture",
            ARTIFACT_COMMIT,
            "ubuntu-x86_64",
            "trusted:release/123/1/shared",
            PRODUCTION_TRUSTED_NOW_MS,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let duplicate_identity = verify_production_policy(
            &production_manifest(),
            &common_policy,
            &[
                (ReleaseEvidenceType::Artifact, Some(&first)),
                (ReleaseEvidenceType::Benchmark, Some(&second)),
            ],
            &[],
        )
        .unwrap_err();
        assert!(context_contains(
            &duplicate_identity,
            "blocked_reasons",
            ProductionEvidenceBlocker::IdentityConflict.as_str()
        ));
    }

    #[test]
    /// 验证整包一致但不属于当前 release 的 version/tag 不能重放。
    fn production_policy_rejects_consistent_wrong_release_identity() {
        let fixture = production_bundle_fixture();
        let envelopes = [
            (
                ReleaseEvidenceType::Artifact,
                fixture
                    .artifact
                    .as_ref()
                    .map(|candidate| &candidate.envelope),
            ),
            (
                ReleaseEvidenceType::Distribution,
                fixture
                    .distribution
                    .as_ref()
                    .map(|candidate| &candidate.envelope),
            ),
            (
                ReleaseEvidenceType::SecurityScan,
                fixture
                    .security_scan
                    .as_ref()
                    .map(|candidate| &candidate.envelope),
            ),
            (
                ReleaseEvidenceType::Benchmark,
                fixture
                    .benchmark
                    .as_ref()
                    .map(|candidate| &candidate.envelope),
            ),
        ];
        let wrong_release = [
            Some((ReleaseEvidenceType::Artifact, "1.11.4", "v1.11.4")),
            Some((ReleaseEvidenceType::Distribution, "1.11.4", "v1.11.4")),
            Some((ReleaseEvidenceType::SecurityScan, "1.11.4", "v1.11.4")),
            Some((ReleaseEvidenceType::Benchmark, "1.11.4", "v1.11.4")),
        ];

        let error = verify_production_policy(
            &fixture.manifest,
            &production_policy(&fixture),
            &envelopes,
            &wrong_release,
        )
        .unwrap_err();
        assert!(context_contains(
            &error,
            "blocked_reasons",
            ProductionEvidenceBlocker::ReleaseIdentityConflict.as_str()
        ));
    }

    #[test]
    /// 验证通过 policy 的 production bundle 仍会阻断弱内置 required gate。
    fn complete_production_bundle_reaches_measurement_gate_enforcement() {
        let fixture = production_bundle_fixture();
        let policy = production_policy(&fixture);
        let bundle = fixture.verify(&policy).unwrap();
        let report = ReleaseHardeningService::v15()
            .readiness_with_verified_release_evidence("all", &bundle)
            .unwrap();
        let benchmark_gate = report
            .gates
            .iter()
            .find(|gate| gate.id == "REL-BENCHMARK-001")
            .unwrap();
        let mcp_gate = report
            .gates
            .iter()
            .find(|gate| gate.id == "REL-MCP-COMPAT-001")
            .unwrap();

        assert_eq!(report.evidence_scope, ReleaseEvidenceScope::Production);
        assert_eq!(report.status, "blocked");
        assert_eq!(benchmark_gate.evidence_kind, EvidenceKind::Measurement);
        assert_eq!(benchmark_gate.status, ReleaseGateStatus::Pass);
        assert_eq!(mcp_gate.evidence_kind, EvidenceKind::Fixture);
        assert_eq!(mcp_gate.status, ReleaseGateStatus::Blocked);
        assert!(mcp_gate
            .remediation
            .iter()
            .any(|item| { item == "evidence_kind_not_measured:REL-MCP-COMPAT-001:fixture" }));
    }

    #[test]
    /// 锁定 production policy blocker 的机器可读名称。
    fn production_policy_blocker_codes_are_stable() {
        assert_eq!(
            [
                ProductionEvidenceBlocker::ManifestRequired.as_str(),
                ProductionEvidenceBlocker::TrustedCommitRequired.as_str(),
                ProductionEvidenceBlocker::CoverageMissing.as_str(),
                ProductionEvidenceBlocker::KindNotMeasurement.as_str(),
                ProductionEvidenceBlocker::Stale.as_str(),
                ProductionEvidenceBlocker::FutureTimestamp.as_str(),
                ProductionEvidenceBlocker::ExecutorUntrusted.as_str(),
                ProductionEvidenceBlocker::TrustedRunRequired.as_str(),
                ProductionEvidenceBlocker::ManifestDigestRequired.as_str(),
                ProductionEvidenceBlocker::ManifestDigestInvalid.as_str(),
                ProductionEvidenceBlocker::ManifestDigestMismatch.as_str(),
                ProductionEvidenceBlocker::EnvelopeDigestMissing.as_str(),
                ProductionEvidenceBlocker::EnvelopeDigestInvalid.as_str(),
                ProductionEvidenceBlocker::EnvelopeDigestMismatch.as_str(),
                ProductionEvidenceBlocker::SubjectDuplicate.as_str(),
                ProductionEvidenceBlocker::IdentityConflict.as_str(),
                ProductionEvidenceBlocker::ReleaseIdentityConflict.as_str(),
                ProductionEvidenceBlocker::PolicyRequired.as_str(),
            ],
            [
                "production_evidence_manifest_required",
                "production_evidence_trusted_commit_required",
                "production_evidence_coverage_missing",
                "production_evidence_kind_not_measurement",
                "production_evidence_stale",
                "production_evidence_future_timestamp",
                "production_evidence_executor_untrusted",
                "production_evidence_trusted_run_required",
                "production_evidence_manifest_digest_required",
                "production_evidence_manifest_digest_invalid",
                "production_evidence_manifest_digest_mismatch",
                "production_evidence_envelope_digest_missing",
                "production_evidence_envelope_digest_invalid",
                "production_evidence_envelope_digest_mismatch",
                "production_evidence_subject_duplicate",
                "production_evidence_identity_conflict",
                "production_evidence_release_identity_conflict",
                "production_evidence_policy_required",
            ]
        );
    }

    #[test]
    /// 验证完整性通过的 measurement 仍不能用 producer 自报预算绕过 policy。
    fn verified_measurement_bundle_rejects_claimed_budget_override() {
        let mut fixture = production_bundle_fixture();
        let benchmark = fixture.benchmark.as_mut().unwrap();
        benchmark.evidence.measurements[0].observed_ms = 6_000;
        benchmark.evidence.measurements[0].budget_ms = 7_000;
        benchmark.envelope = benchmark
            .evidence
            .to_envelope(
                EvidenceKind::Measurement,
                "checklist:test-budget-override",
                "ubuntu-x86_64",
                "github-actions:release-benchmark/123/1/benchmark",
                PRODUCTION_TRUSTED_NOW_MS - 1_000,
            )
            .unwrap();
        fixture.bind_envelope_digests();
        let policy = production_policy(&fixture);
        let bundle = fixture.verify(&policy).unwrap();

        let report = ReleaseHardeningService::v15()
            .readiness_with_verified_release_evidence("all", &bundle)
            .unwrap();
        let gate = report
            .gates
            .iter()
            .find(|gate| gate.id == "REL-BENCHMARK-001")
            .unwrap();

        assert_eq!(gate.evidence_kind, EvidenceKind::Measurement);
        assert_eq!(gate.status, ReleaseGateStatus::Blocked);
        assert!(gate.remediation.iter().any(|item| {
            item == "benchmark release.check claimed 7000ms budget but policy requires 5000ms"
        }));
    }

    #[test]
    /// 验证默认内置 required 门禁没有阻塞项。
    fn readiness_has_no_blocking_required_gates() {
        let report = ReleaseHardeningService::v15().readiness("all").unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.evidence_scope, ReleaseEvidenceScope::Alpha);
        assert_eq!(report.blocking_count(), 0);
        assert_eq!(report.warning_count(), 7);
        let performance_gates = report
            .gates
            .iter()
            .filter(|gate| gate.domain == "performance")
            .collect::<Vec<_>>();
        assert_eq!(performance_gates.len(), 6);
        assert!(performance_gates.iter().all(|gate| {
            gate.status == ReleaseGateStatus::Warn
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "performance.observation_kind:unmeasured")
        }));
        assert!(report.gates.iter().all(|gate| {
            gate.evidence_kind == EvidenceKind::Declaration
                || (gate.id == "REL-MCP-COMPAT-001" && gate.evidence_kind == EvidenceKind::Fixture)
        }));
        assert!(report
            .gates
            .iter()
            .any(|gate| gate.domain == "cross_platform"));
        assert!(report.gates.iter().any(|gate| gate.domain == "migration"));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DOCS-001"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "docs/en/release/github-packages-publishing.md")
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "docs/en/release/install-upgrade-uninstall.md")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-BACKEND-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-EVENTBUS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-STORES-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-RECOVERY-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-DIAGNOSTICS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-VM-EXECUTION-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-HOST-BINDINGS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-RESOURCE-LIMITS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-HOT-RELOAD-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-BACKUP-ARCHIVE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-RESTORE-APPLY-GATE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SUPERVISOR-HANDOFF-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SERVICE-MANAGER-ABSTRACTION-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "service_manager_abstraction"
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DAEMON-RUNTIME-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "daemon_runtime"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item.contains("daemon_control_loop_ticks_scheduler_retry_once"))
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-MCP-COMPAT-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "mcp_compatibility"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "mcp.transport_count:2")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-PROVIDER-SUPERVISION-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "provider_supervision"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "provider.os_process_supervisor:not_claimed")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-HARDWARE-SAFETY-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "hardware_safety"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "hardware.safety.release_mode:alpha_simulator_only")
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "real_hardware_fixture:not_required_for_alpha")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-JSON-CONTRACT-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "cli_json_contract"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "scripts/validate-cli-json-contracts.ps1")
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "contracts/cli-json/release-check.json")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-OBSERVABILITY-POLICY-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "observability_policy"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "observability_retention_policy_v1.16.4")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-V1X-CLOSURE-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "v1x_closure"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "closure.required_gate:REL-JSON-CONTRACT-001")
                && gate.evidence.iter().any(|item| {
                    item == "closure.external_blocker:production_signing_attestation_credentials"
                })
        }));
        assert_eq!(report.closure.status, "ready_with_external_blockers");
        assert!(report.closure.missing_required_gate_ids.is_empty());
        assert!(report.closure.blocking_required_gate_ids.is_empty());
        assert!(report
            .closure
            .required_gate_ids
            .contains(&"REL-OBSERVABILITY-POLICY-001".to_owned()));
        assert!(report
            .closure
            .blocked_external_items
            .contains(&"production_signing_attestation_credentials".to_owned()));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "durable_diagnostics_smoke_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_vm_execution_boundary_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_host_bindings_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_resource_limits_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_hot_reload_lifecycle_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "signed_backup_archive_baseline_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "restore_apply_gate_baseline_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "supervisor_handoff_baseline_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "service_manager_abstraction_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "daemon_runtime_readiness_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "mcp_compatibility_matrix_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "provider_supervision_readiness_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "hardware_safety_release_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "public_json_contract_diff_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "observability_policy_release_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "v1x_closure_report_ready"));
    }

    #[test]
    /// 验证缺失 MCP 兼容矩阵时对应门禁失败关闭。
    fn mcp_compatibility_gate_blocks_missing_matrix() {
        let gate = mcp_compatibility_matrix_gate(None);

        assert_eq!(gate.id, "REL-MCP-COMPAT-001");
        assert_eq!(gate.status, ReleaseGateStatus::Blocked);
        assert!(gate.required);
        assert!(gate
            .evidence
            .contains(&"mcp.compatibility_matrix:missing".to_owned()));
    }

    #[test]
    /// 验证 Provider 监督门禁记录当前实现边界。
    fn provider_supervision_gate_records_current_boundaries() {
        let gate = provider_supervision_gate();

        assert_eq!(gate.id, "REL-PROVIDER-SUPERVISION-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert!(gate
            .evidence
            .contains(&"provider.mcp_compatibility_gate:REL-MCP-COMPAT-001".to_owned()));
        assert!(gate
            .evidence
            .contains(&"provider.os_process_supervisor:not_claimed".to_owned()));
    }

    #[test]
    /// 验证 alpha 硬件门禁接受明确标注的模拟器安全证据。
    fn hardware_safety_release_gate_accepts_alpha_simulator_only_evidence() {
        let gate = hardware_safety_release_gate();

        assert_eq!(gate.id, "REL-HARDWARE-SAFETY-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert_eq!(gate.domain, "hardware_safety");
        assert!(gate
            .evidence
            .contains(&"hardware.safety.release_mode:alpha_simulator_only".to_owned()));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("run_simulator_contract_suite") && item.contains("raw I/O")
        }));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("raw_device_path_exposed=false") && item.contains("before lease claim")
        }));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("hotplug watcher crash")
                && item.contains("release active hardware leases")
        }));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("raw_handles_exposed=false") && item.contains("hardware-hotplug.state")
        }));
        assert!(gate.remediation.iter().any(|item| {
            item.contains("real or virtual hardware fixture evidence")
                && item.contains("claiming real USB")
        }));
    }

    #[test]
    /// 验证公共 JSON 门禁记录加法兼容差异套件。
    fn public_json_contract_gate_records_additive_diff_suite() {
        let gate = public_json_contract_gate();

        assert_eq!(gate.id, "REL-JSON-CONTRACT-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert_eq!(gate.domain, "cli_json_contract");
        assert!(gate
            .evidence
            .contains(&"scripts/validate-cli-json-contracts.ps1".to_owned()));
        assert!(gate
            .evidence
            .contains(&"contracts/cli-json/version.json".to_owned()));
        assert!(gate
            .evidence
            .contains(&"contracts/cli-json/release-check.json".to_owned()));
        assert!(gate
            .evidence
            .iter()
            .any(|item| { item == "contract.diff:golden_subset_allows_additive_fields" }));
        assert!(gate
            .evidence
            .iter()
            .any(|item| { item == "contract.diff:missing_or_renamed_fields_block" }));
        assert!(gate.remediation.iter().any(|item| {
            item.contains("removed or renamed JSON fields") && item.contains("breaking")
        }));
    }

    #[test]
    /// 验证可观测性门禁记录当前保留策略边界。
    fn observability_policy_gate_records_v1164_boundary() {
        let gate = observability_policy_gate();

        assert_eq!(gate.id, "REL-OBSERVABILITY-POLICY-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert_eq!(gate.domain, "observability_policy");
        assert!(gate
            .evidence
            .contains(&"runtime_audit_sink_wiring_v1.16.1".to_owned()));
        assert!(gate
            .evidence
            .contains(&"tracing_subscriber_bridge_v1.16.2".to_owned()));
        assert!(gate
            .evidence
            .contains(&"opentelemetry_sdk_exporter_v1.16.3".to_owned()));
        assert!(gate
            .evidence
            .contains(&"observability_retention_policy_v1.16.4".to_owned()));
        assert!(gate
            .evidence
            .contains(&"database_sink:policy_kind_only_not_claimed_as_real_backend".to_owned()));
    }

    #[test]
    /// 验证外部生产阻塞项被记录但不阻塞 alpha 内部闭环。
    fn v1x_closure_gate_records_external_blockers_without_blocking_alpha() {
        let mcp_report = McpCompatibilityMatrix::v1137_fixture().verify().unwrap();
        let gates = vec![
            daemon_runtime_gate(),
            mcp_compatibility_matrix_gate(Some(&mcp_report)),
            provider_supervision_gate(),
            restore_apply_gate(),
            service_manager_abstraction_gate(),
            hardware_safety_release_gate(),
            observability_policy_gate(),
            public_json_contract_gate(),
        ];
        let closure = v1x_closure_report(&gates);
        let gate = v1x_closure_gate(&closure);

        assert_eq!(closure.status, "ready_with_external_blockers");
        assert!(closure.missing_required_gate_ids.is_empty());
        assert!(closure.blocking_required_gate_ids.is_empty());
        assert!(closure
            .optional_production_gate_ids
            .contains(&"REL-DISTRIBUTION-001".to_owned()));
        assert!(closure
            .blocked_external_items
            .contains(&"production_signing_attestation_credentials".to_owned()));
        assert_eq!(gate.id, "REL-V1X-CLOSURE-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert!(gate
            .evidence
            .contains(&"closure.status:ready_with_external_blockers".to_owned()));
        assert!(gate
            .evidence
            .contains(&"closure.required_gate:REL-OBSERVABILITY-POLICY-001".to_owned()));
        assert!(gate.evidence.contains(
            &"closure.external_blocker:production_signing_attestation_credentials".to_owned()
        ));
    }

    #[test]
    /// 验证有效签名工件证据使来源门禁通过。
    fn readiness_with_signed_artifact_evidence_passes_release_artifact_gate() {
        let evidence = artifact_evidence(true);
        let report = ReleaseHardeningService::v15()
            .readiness_with_artifact_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-ARTIFACT-PROVENANCE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "signed_artifact_provenance_verified"));
    }

    #[test]
    /// 验证未签名工件证据会阻塞来源门禁。
    fn readiness_with_unsigned_artifact_evidence_blocks_release_artifact_gate() {
        let evidence = artifact_evidence(false);
        let report = ReleaseHardeningService::v15()
            .readiness_with_artifact_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-ARTIFACT-PROVENANCE-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate
                    .remediation
                    .iter()
                    .any(|item| item == "release artifact is marked unsigned")
        }));
    }

    #[test]
    /// 验证完整分发证据使分发门禁通过。
    fn readiness_with_distribution_evidence_passes_release_distribution_gate() {
        let evidence = distribution_evidence("passed");
        let report = ReleaseHardeningService::v15()
            .readiness_with_distribution_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DISTRIBUTION-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "distribution_install_smoke_verified"));
    }

    #[test]
    /// 验证失败包演练会阻塞分发门禁。
    fn readiness_with_failed_package_dry_run_blocks_release_distribution_gate() {
        let evidence = distribution_evidence("failed");
        let report = ReleaseHardeningService::v15()
            .readiness_with_distribution_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DISTRIBUTION-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate
                    .remediation
                    .iter()
                    .any(|item| item.contains("package manager dry-run for ghcr"))
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "distribution_install_smoke_blocked"));
    }

    #[test]
    /// 验证工件和分发证据可同时加入并通过各自门禁。
    fn readiness_with_artifact_and_distribution_evidence_passes_both_gates() {
        let artifact = artifact_evidence(true);
        let distribution = distribution_evidence("passed");
        let report = ReleaseHardeningService::v15()
            .readiness_with_release_evidence(
                "all",
                Some(&artifact),
                Some(&distribution),
                None,
                None,
            )
            .unwrap();

        assert_eq!(report.status, "ready");
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-ARTIFACT-PROVENANCE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DISTRIBUTION-001" && gate.status == ReleaseGateStatus::Pass
        }));
    }

    #[test]
    /// 验证干净外部安全扫描证据使扫描门禁通过。
    fn readiness_with_clean_security_scan_evidence_passes_gate() {
        let evidence = security_scan_evidence("passed", Vec::new());
        let report = ReleaseHardeningService::v15()
            .readiness_with_security_scan_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SECURITY-SCAN-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "external_security_scan_verified"));
    }

    #[test]
    /// 验证高严重级别外部发现会阻塞扫描门禁。
    fn readiness_with_high_security_scan_finding_blocks_gate() {
        let evidence = security_scan_evidence("passed", vec![security_scan_finding("high")]);
        let report = ReleaseHardeningService::v15()
            .readiness_with_security_scan_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SECURITY-SCAN-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate.remediation.iter().any(|item| {
                    item == "security scanner finding RUSTSEC-0000-0000 is high severity"
                })
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "external_security_scan_blocked"));
    }

    #[test]
    /// 验证预算内生产基准证据使基准门禁通过。
    fn readiness_with_benchmark_evidence_passes_gate() {
        let evidence = benchmark_evidence("passed", 120);
        let report = ReleaseHardeningService::v15()
            .readiness_with_benchmark_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-BENCHMARK-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.evidence_kind == EvidenceKind::Declaration
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "production_benchmark_verified"));
    }

    #[test]
    /// 验证超预算生产测量会阻塞基准门禁。
    fn readiness_with_benchmark_regression_blocks_gate() {
        let evidence = benchmark_evidence("passed", 6_000);
        let report = ReleaseHardeningService::v15()
            .readiness_with_benchmark_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-BENCHMARK-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate.remediation.iter().any(|item| {
                    item == "benchmark release.check observed 6000ms over 5000ms budget"
                })
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "production_benchmark_blocked"));
    }

    #[test]
    /// 验证四类有效外部证据可同时通过各自 required 门禁。
    fn readiness_with_all_release_evidence_passes_release_gates() {
        let artifact = artifact_evidence(true);
        let distribution = distribution_evidence("passed");
        let security_scan = security_scan_evidence("passed", Vec::new());
        let benchmark = benchmark_evidence("passed", 120);
        let report = ReleaseHardeningService::v15()
            .readiness_with_release_evidence(
                "all",
                Some(&artifact),
                Some(&distribution),
                Some(&security_scan),
                Some(&benchmark),
            )
            .unwrap();

        assert_eq!(report.status, "ready");
        for gate_id in [
            "REL-ARTIFACT-PROVENANCE-001",
            "REL-DISTRIBUTION-001",
            "REL-SECURITY-SCAN-001",
            "REL-BENCHMARK-001",
        ] {
            assert!(report
                .gates
                .iter()
                .any(|gate| { gate.id == gate_id && gate.status == ReleaseGateStatus::Pass }));
        }
    }

    #[test]
    /// 验证就绪度报告可筛选到单个操作系统。
    fn readiness_can_filter_target_platform() {
        let report = ReleaseHardeningService::v15().readiness("windows").unwrap();

        assert_eq!(report.platforms.len(), 1);
        assert_eq!(report.platforms[0].os, "windows");
    }

    #[test]
    /// 验证未知平台目标在聚合前被拒绝。
    fn readiness_rejects_unknown_target_platform() {
        let error = ReleaseHardeningService::v15()
            .readiness("unix")
            .unwrap_err();

        assert!(error.message().contains("target must be"));
    }
}
