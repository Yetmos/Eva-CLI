//! 统一发布证据信封及其稳定清单契约。
//! Unified release evidence envelope and stable manifest contract.

use eva_core::EvaError;
use eva_storage::ArtifactRecord;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

/// 本模块的架构职责：为所有发布证据提供统一的分类、来源、执行环境和主题身份。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "uniform release evidence identity and provenance envelope";

/// 当前支持的统一发布证据信封格式。
pub const EVIDENCE_ENVELOPE_FORMAT: &str = "eva.release.evidence_envelope.v1";

/// 当前支持的统一发布证据索引清单格式。
pub const RELEASE_EVIDENCE_MANIFEST_FORMAT: &str = "eva.release.evidence_manifest.v1";

/// Canonical platform evidence subject emitted by a native release job.
pub const RELEASE_PLATFORM_EVIDENCE_FORMAT: &str = "eva.release.platform_subject.v1";

/// JSON index format that points at a platform subject and envelope.
pub const RELEASE_PLATFORM_INDEX_FORMAT: &str = "eva.release.platform_evidence.v1";

/// Canonical aggregate of platform evidence subjects.
pub const RELEASE_PLATFORM_BUNDLE_FORMAT: &str = "eva.release.platform_bundle.v1";

/// Capture manifest format written by `capture-release-evidence.ps1`.
pub const RELEASE_COMMAND_CAPTURE_FORMAT: &str = "eva.release.command_capture.v1";

/// Production evidence is accepted for at most 24 hours after capture.
pub const PRODUCTION_EVIDENCE_MAX_AGE_MS: u128 = 24 * 60 * 60 * 1_000;

/// Production evidence may lead the consumer clock by at most five minutes.
pub const PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS: u128 = 5 * 60 * 1_000;

/// 仅用于复用 ArtifactRecord 的确定性 SHA-256 实现，不表示写入 artifact store。
const EVIDENCE_SUBJECT_KEY: &str = "release/evidence/subject";

/// v1 清单唯一允许的字段集合；新增字段必须升级格式或显式扩展本集合。
const EVIDENCE_ENVELOPE_FIELDS: [&str; 8] = [
    "format",
    "kind",
    "source",
    "source_commit",
    "environment",
    "executor",
    "timestamp",
    "subject_digest",
];

/// 发布证据的来源强度分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceKind {
    /// 描述预期行为或静态门禁的声明，不证明行为已经执行。
    Declaration,
    /// 由测试替身或受控样例产生的证据，不代表生产环境实测。
    Fixture,
    /// 由操作员确认或记录的证据，不替代机器测量。
    Operator,
    /// 由真实命令、系统或环境运行产生的机器测量。
    Measurement,
}

/// 发布检查所处的证据强度范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReleaseEvidenceScope {
    /// 保持现有 V1.x alpha 门禁与旧 evidence 参数兼容。
    Alpha,
    /// 只允许由统一 manifest 提供且受外部提交约束的证据。
    Production,
}

impl ReleaseEvidenceScope {
    /// 返回 CLI 和 manifest 共用的稳定小写标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alpha => "alpha",
            Self::Production => "production",
        }
    }

    /// 解析 alpha 或 production，未知范围失败关闭。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "alpha" => Ok(Self::Alpha),
            "production" => Ok(Self::Production),
            _ => Err(EvaError::invalid_argument(
                "release evidence scope must be alpha or production",
            )
            .with_context("scope", value)),
        }
    }
}

impl fmt::Display for ReleaseEvidenceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 统一 manifest 中可供当前 release checklist 消费的 evidence 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReleaseEvidenceType {
    /// 签名发布工件、真实工件字节及 provenance。
    Artifact,
    /// 多平台安装与包分发演练。
    Distribution,
    /// 外部安全扫描结果。
    SecurityScan,
    /// 生产基准测量。
    Benchmark,
}

impl ReleaseEvidenceType {
    /// 当前统一 manifest 支持的全部 evidence 类型，顺序也是 canonical 排序顺序。
    pub const ALL: [Self; 4] = [
        Self::Artifact,
        Self::Distribution,
        Self::SecurityScan,
        Self::Benchmark,
    ];

    /// 返回 manifest 使用的稳定类型标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::Distribution => "distribution",
            Self::SecurityScan => "security_scan",
            Self::Benchmark => "benchmark",
        }
    }

    /// 解析当前 checklist 支持的 evidence 类型。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "artifact" => Ok(Self::Artifact),
            "distribution" => Ok(Self::Distribution),
            "security_scan" => Ok(Self::SecurityScan),
            "benchmark" => Ok(Self::Benchmark),
            _ => Err(EvaError::invalid_argument(
                "unsupported release evidence manifest entry type",
            )
            .with_context("entry_type", value)),
        }
    }
}

impl fmt::Display for ReleaseEvidenceType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A consumer-owned executor allow-list rule for one production evidence type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionEvidenceExecutorRule {
    evidence_type: ReleaseEvidenceType,
    executor_prefix: String,
}

impl ProductionEvidenceExecutorRule {
    /// Trust executors whose identity starts with a delimiter-terminated namespace.
    pub fn prefix(
        evidence_type: ReleaseEvidenceType,
        executor_prefix: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let executor_prefix = validate_manifest_text(
            "production evidence executor prefix",
            executor_prefix.into(),
        )?;
        if !executor_prefix.ends_with([':', '/']) {
            return Err(EvaError::invalid_argument(
                "production evidence executor prefix must end with ':' or '/'",
            )
            .with_context("entry_type", evidence_type.as_str()));
        }
        Ok(Self {
            evidence_type,
            executor_prefix,
        })
    }

    /// Return the evidence type governed by this rule.
    pub const fn evidence_type(&self) -> ReleaseEvidenceType {
        self.evidence_type
    }

    /// Match a canonical namespace/run/attempt/job identity against external run context.
    pub fn matches(
        &self,
        executor: &str,
        expected_run_id: &str,
        expected_run_attempt: &str,
    ) -> bool {
        let Some(identity) = executor.strip_prefix(&self.executor_prefix) else {
            return false;
        };
        let mut parts = identity.split('/');
        let (Some(run_id), Some(run_attempt), Some(job), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        run_id == expected_run_id
            && run_attempt == expected_run_attempt
            && !job.is_empty()
            && job
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }
}

/// External facts supplied by the production consumer rather than an evidence producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionEvidenceContext {
    trusted_now_ms: u128,
    expected_version: String,
    expected_source_tag: String,
    expected_run_id: String,
    expected_run_attempt: String,
    expected_manifest_digest: String,
}

impl ProductionEvidenceContext {
    /// Validate the consumer clock, release identity, and GitHub Actions run identity.
    pub fn new(
        trusted_now_ms: u128,
        expected_version: impl Into<String>,
        expected_source_tag: impl Into<String>,
        expected_run_id: impl Into<String>,
        expected_run_attempt: impl Into<String>,
        expected_manifest_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        if trusted_now_ms == 0 {
            return Err(EvaError::invalid_argument(
                "production evidence trusted time must be greater than zero",
            ));
        }
        let expected_version = validate_manifest_text(
            "production evidence expected version",
            expected_version.into(),
        )?;
        let expected_source_tag = validate_manifest_text(
            "production evidence expected source tag",
            expected_source_tag.into(),
        )?;
        if expected_source_tag != format!("v{expected_version}") {
            return Err(EvaError::invalid_argument(
                "production evidence expected source tag must be v<expected_version>",
            ));
        }
        let expected_run_id = validate_canonical_positive_decimal(
            "production evidence expected run id",
            expected_run_id.into(),
        )
        .map_err(|error| {
            error.with_context(
                "blocked_reasons",
                ProductionEvidenceBlocker::TrustedRunRequired.as_str(),
            )
        })?;
        let expected_run_attempt = validate_canonical_positive_decimal(
            "production evidence expected run attempt",
            expected_run_attempt.into(),
        )
        .map_err(|error| {
            error.with_context(
                "blocked_reasons",
                ProductionEvidenceBlocker::TrustedRunRequired.as_str(),
            )
        })?;
        let expected_manifest_digest = expected_manifest_digest.into();
        validate_canonical_subject_digest(&expected_manifest_digest).map_err(|error| {
            error.with_context(
                "blocked_reasons",
                ProductionEvidenceBlocker::ManifestDigestInvalid.as_str(),
            )
        })?;
        Ok(Self {
            trusted_now_ms,
            expected_version,
            expected_source_tag,
            expected_run_id,
            expected_run_attempt,
            expected_manifest_digest,
        })
    }
}

/// Consumer-owned production coverage, freshness, and executor trust policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionEvidencePolicy {
    context: ProductionEvidenceContext,
    max_age_ms: u128,
    max_future_skew_ms: u128,
    executor_rules: Vec<ProductionEvidenceExecutorRule>,
}

impl ProductionEvidencePolicy {
    /// Build a policy that requires rules for every currently supported evidence type.
    pub fn new(
        context: ProductionEvidenceContext,
        max_age_ms: u128,
        max_future_skew_ms: u128,
        executor_rules: Vec<ProductionEvidenceExecutorRule>,
    ) -> Result<Self, EvaError> {
        if max_age_ms == 0 {
            return Err(EvaError::invalid_argument(
                "production evidence maximum age must be greater than zero",
            ));
        }

        let mut rules = BTreeSet::new();
        let mut covered_types = BTreeSet::new();
        for rule in &executor_rules {
            if !rules.insert((rule.evidence_type, rule.executor_prefix.as_str())) {
                return Err(EvaError::invalid_argument(
                    "production evidence executor rule is duplicated",
                )
                .with_context("entry_type", rule.evidence_type.as_str()));
            }
            covered_types.insert(rule.evidence_type);
        }
        let missing_types = ReleaseEvidenceType::ALL
            .into_iter()
            .filter(|evidence_type| !covered_types.contains(evidence_type))
            .map(ReleaseEvidenceType::as_str)
            .collect::<Vec<_>>();
        if !missing_types.is_empty() {
            return Err(EvaError::invalid_argument(
                "production evidence policy must define executors for every evidence type",
            )
            .with_context("missing_entry_types", missing_types.join(",")));
        }

        Ok(Self {
            context,
            max_age_ms,
            max_future_skew_ms,
            executor_rules,
        })
    }

    /// Build the repository's GitHub Actions production policy at a caller-trusted time.
    pub fn github_actions(
        trusted_now_ms: u128,
        expected_run_id: impl Into<String>,
        expected_run_attempt: impl Into<String>,
        expected_manifest_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let expected_version = env!("CARGO_PKG_VERSION");
        let context = ProductionEvidenceContext::new(
            trusted_now_ms,
            expected_version,
            format!("v{expected_version}"),
            expected_run_id,
            expected_run_attempt,
            expected_manifest_digest,
        )?;
        Self::new(
            context,
            PRODUCTION_EVIDENCE_MAX_AGE_MS,
            PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS,
            vec![
                ProductionEvidenceExecutorRule::prefix(
                    ReleaseEvidenceType::Artifact,
                    "github-actions:release-artifact/",
                )?,
                ProductionEvidenceExecutorRule::prefix(
                    ReleaseEvidenceType::Distribution,
                    "github-actions:release-distribution/",
                )?,
                ProductionEvidenceExecutorRule::prefix(
                    ReleaseEvidenceType::SecurityScan,
                    "github-actions:release-security-scan/",
                )?,
                ProductionEvidenceExecutorRule::prefix(
                    ReleaseEvidenceType::Benchmark,
                    "github-actions:release-benchmark/",
                )?,
            ],
        )
    }

    /// Return the externally supplied current epoch used for freshness checks.
    pub const fn trusted_now_ms(&self) -> u128 {
        self.context.trusted_now_ms
    }

    /// Return the maximum accepted evidence age.
    pub const fn max_age_ms(&self) -> u128 {
        self.max_age_ms
    }

    /// Return the maximum accepted clock lead.
    pub const fn max_future_skew_ms(&self) -> u128 {
        self.max_future_skew_ms
    }

    /// Return the release version the consumer is evaluating.
    pub fn expected_version(&self) -> &str {
        &self.context.expected_version
    }

    /// Return the release tag the consumer is evaluating.
    pub fn expected_source_tag(&self) -> &str {
        &self.context.expected_source_tag
    }

    /// Return the externally trusted GitHub Actions run identifier.
    pub fn expected_run_id(&self) -> &str {
        &self.context.expected_run_id
    }

    /// Return the externally trusted GitHub Actions run attempt.
    pub fn expected_run_attempt(&self) -> &str {
        &self.context.expected_run_attempt
    }

    /// Return the externally trusted digest of the canonical release manifest.
    pub fn expected_manifest_digest(&self) -> &str {
        &self.context.expected_manifest_digest
    }

    /// Test an executor against rules selected by the evidence type.
    pub fn trusts_executor(&self, evidence_type: ReleaseEvidenceType, executor: &str) -> bool {
        self.executor_rules
            .iter()
            .filter(|rule| rule.evidence_type == evidence_type)
            .any(|rule| {
                rule.matches(
                    executor,
                    &self.context.expected_run_id,
                    &self.context.expected_run_attempt,
                )
            })
    }
}

/// Stable machine blockers emitted by production evidence policy verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProductionEvidenceBlocker {
    /// The production command was invoked without a unified evidence manifest.
    ManifestRequired,
    /// The consumer did not provide an external trusted source commit.
    TrustedCommitRequired,
    /// A required evidence type is absent from the production manifest.
    CoverageMissing,
    /// A production envelope is not a machine measurement.
    KindNotMeasurement,
    /// An envelope predates the consumer-owned freshness window.
    Stale,
    /// An envelope is too far ahead of the consumer-owned clock.
    FutureTimestamp,
    /// An executor is outside the consumer-owned allow-list.
    ExecutorUntrusted,
    /// The consumer did not provide an external run ID and attempt.
    TrustedRunRequired,
    /// The consumer did not provide a trusted canonical manifest digest.
    ManifestDigestRequired,
    /// A manifest or entry digest is not canonical SHA-256.
    ManifestDigestInvalid,
    /// Canonical manifest bytes do not match the external digest.
    ManifestDigestMismatch,
    /// A production manifest entry omits its envelope digest.
    EnvelopeDigestMissing,
    /// A declared envelope digest is not canonical SHA-256.
    EnvelopeDigestInvalid,
    /// Canonical envelope bytes do not match their trusted manifest entry.
    EnvelopeDigestMismatch,
    /// Two evidence types claim the same subject digest.
    SubjectDuplicate,
    /// Two subjects reuse the same capture identity.
    IdentityConflict,
    /// Typed evidence disagrees on release version or source tag.
    ReleaseIdentityConflict,
    /// Production verification was attempted without a consumer policy.
    PolicyRequired,
}

impl ProductionEvidenceBlocker {
    /// Return the stable CLI/error-context code for this blocker.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestRequired => "production_evidence_manifest_required",
            Self::TrustedCommitRequired => "production_evidence_trusted_commit_required",
            Self::CoverageMissing => "production_evidence_coverage_missing",
            Self::KindNotMeasurement => "production_evidence_kind_not_measurement",
            Self::Stale => "production_evidence_stale",
            Self::FutureTimestamp => "production_evidence_future_timestamp",
            Self::ExecutorUntrusted => "production_evidence_executor_untrusted",
            Self::TrustedRunRequired => "production_evidence_trusted_run_required",
            Self::ManifestDigestRequired => "production_evidence_manifest_digest_required",
            Self::ManifestDigestInvalid => "production_evidence_manifest_digest_invalid",
            Self::ManifestDigestMismatch => "production_evidence_manifest_digest_mismatch",
            Self::EnvelopeDigestMissing => "production_evidence_envelope_digest_missing",
            Self::EnvelopeDigestInvalid => "production_evidence_envelope_digest_invalid",
            Self::EnvelopeDigestMismatch => "production_evidence_envelope_digest_mismatch",
            Self::SubjectDuplicate => "production_evidence_subject_duplicate",
            Self::IdentityConflict => "production_evidence_identity_conflict",
            Self::ReleaseIdentityConflict => "production_evidence_release_identity_conflict",
            Self::PolicyRequired => "production_evidence_policy_required",
        }
    }
}

impl fmt::Display for ProductionEvidenceBlocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 统一 manifest 中一类 gate evidence、信封和可选真实主题的相对引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEvidenceManifestEntry {
    /// 当前 checklist 消费的 evidence 类型。
    pub evidence_type: ReleaseEvidenceType,
    /// 相对统一 manifest 目录的 typed evidence 文档路径。
    pub evidence_path: String,
    /// 相对统一 manifest 目录的 EvidenceEnvelope 路径。
    pub envelope_path: String,
    /// Canonical envelope bytes digest; production policy requires it.
    pub envelope_digest: Option<String>,
    /// artifact 的真实归档/二进制路径；其他类型的主题固定为 canonical evidence 文档。
    pub subject_path: Option<String>,
}

impl ReleaseEvidenceManifestEntry {
    /// 创建路径受限且 subject 语义与 evidence 类型一致的 manifest 项。
    pub fn new(
        evidence_type: ReleaseEvidenceType,
        evidence_path: impl Into<String>,
        envelope_path: impl Into<String>,
        subject_path: Option<String>,
    ) -> Result<Self, EvaError> {
        let evidence_path = validate_manifest_relative_path(
            "release evidence manifest evidence path",
            evidence_path.into(),
        )?;
        let envelope_path = validate_manifest_relative_path(
            "release evidence manifest envelope path",
            envelope_path.into(),
        )?;
        let subject_path = subject_path
            .map(|path| {
                validate_manifest_relative_path("release evidence manifest subject path", path)
            })
            .transpose()?;

        match (evidence_type, subject_path.is_some()) {
            (ReleaseEvidenceType::Artifact, false) => {
                return Err(EvaError::invalid_argument(
                    "artifact evidence manifest entry requires a subject path",
                ));
            }
            (ReleaseEvidenceType::Artifact, true) | (_, false) => {}
            (_, true) => {
                return Err(EvaError::invalid_argument(
                    "only artifact evidence manifest entries may declare a subject path",
                )
                .with_context("entry_type", evidence_type.as_str()));
            }
        }

        Ok(Self {
            evidence_type,
            evidence_path,
            envelope_path,
            envelope_digest: None,
            subject_path,
        })
    }

    /// Bind this entry to the canonical envelope bytes without changing alpha defaults.
    pub fn with_envelope_digest(
        mut self,
        envelope_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let envelope_digest = envelope_digest.into();
        validate_canonical_subject_digest(&envelope_digest).map_err(|error| {
            error.with_context(
                "blocked_reasons",
                ProductionEvidenceBlocker::EnvelopeDigestInvalid.as_str(),
            )
        })?;
        self.envelope_digest = Some(envelope_digest);
        Ok(self)
    }
}

/// 将一组 typed evidence 和信封绑定到同一 scope 与来源提交的索引清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEvidenceManifest {
    /// manifest 格式版本。
    pub format: String,
    /// alpha 或 production 证据范围。
    pub scope: ReleaseEvidenceScope,
    /// 由调用方外部信任上下文复验的来源提交声明。
    pub source_commit: String,
    /// 类型唯一且顺序稳定的 evidence 引用。
    pub entries: Vec<ReleaseEvidenceManifestEntry>,
}

impl ReleaseEvidenceManifest {
    /// 创建非空、同类型唯一且提交格式规范的统一 evidence manifest。
    pub fn new(
        scope: ReleaseEvidenceScope,
        source_commit: impl Into<String>,
        mut entries: Vec<ReleaseEvidenceManifestEntry>,
    ) -> Result<Self, EvaError> {
        let source_commit = source_commit.into();
        validate_canonical_source_commit(&source_commit)?;
        if entries.is_empty() {
            return Err(EvaError::invalid_argument(
                "release evidence manifest must contain at least one entry",
            ));
        }
        let mut evidence_types = BTreeSet::new();
        for entry in &entries {
            if !evidence_types.insert(entry.evidence_type) {
                return Err(EvaError::invalid_argument(
                    "release evidence manifest entry type is duplicated",
                )
                .with_context("entry_type", entry.evidence_type.as_str()));
            }
        }
        entries.sort_by_key(|entry| entry.evidence_type);
        Ok(Self {
            format: RELEASE_EVIDENCE_MANIFEST_FORMAT.to_owned(),
            scope,
            source_commit,
            entries,
        })
    }

    /// 从严格索引键值清单解析 scope、提交和 evidence 引用。
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        let format = required_manifest_field(&fields, "format")?;
        if format != RELEASE_EVIDENCE_MANIFEST_FORMAT {
            return Err(
                EvaError::invalid_argument("unsupported release evidence manifest format")
                    .with_context("format", format),
            );
        }
        let scope = ReleaseEvidenceScope::parse(&required_manifest_field(&fields, "scope")?)?;
        let source_commit = required_manifest_field(&fields, "source_commit")?;

        let mut indexes = BTreeSet::new();
        for field in fields.keys() {
            if matches!(field.as_str(), "format" | "scope" | "source_commit") {
                continue;
            }
            let parts = field.split('.').collect::<Vec<_>>();
            if parts.len() != 3
                || parts[0] != "entry"
                || !matches!(
                    parts[2],
                    "type" | "evidence" | "envelope" | "envelope_digest" | "subject"
                )
            {
                return Err(EvaError::invalid_argument(
                    "release evidence manifest contains an unknown field",
                )
                .with_context("field", field));
            }
            let index = parts[1].parse::<usize>().map_err(|error| {
                EvaError::invalid_argument(
                    "release evidence manifest entry index must be a non-negative integer",
                )
                .with_context("field", field)
                .with_context("parse_error", error.to_string())
            })?;
            indexes.insert(index);
        }
        for (expected, actual) in indexes.iter().copied().enumerate() {
            if expected != actual {
                return Err(EvaError::invalid_argument(
                    "release evidence manifest entry indexes must be contiguous from zero",
                )
                .with_context("expected_index", expected.to_string())
                .with_context("actual_index", actual.to_string()));
            }
        }

        let entries = indexes
            .into_iter()
            .map(|index| {
                let prefix = format!("entry.{index}");
                let entry = ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::parse(&required_manifest_field(
                        &fields,
                        &format!("{prefix}.type"),
                    )?)?,
                    required_manifest_field(&fields, &format!("{prefix}.evidence"))?,
                    required_manifest_field(&fields, &format!("{prefix}.envelope"))?,
                    fields.get(&format!("{prefix}.subject")).cloned(),
                )?;
                if let Some(digest) = fields.get(&format!("{prefix}.envelope_digest")) {
                    entry.with_envelope_digest(digest)
                } else {
                    Ok(entry)
                }
            })
            .collect::<Result<Vec<_>, EvaError>>()?;
        Self::new(scope, source_commit, entries)
    }

    /// 以固定顶层和索引字段顺序输出 canonical manifest。
    pub fn to_manifest(&self) -> String {
        let mut output = format!(
            "format={}\nscope={}\nsource_commit={}\n",
            self.format, self.scope, self.source_commit
        );
        for (index, entry) in self.entries.iter().enumerate() {
            output.push_str(&format!(
                "entry.{index}.type={}\nentry.{index}.evidence={}\nentry.{index}.envelope={}\n",
                entry.evidence_type, entry.evidence_path, entry.envelope_path
            ));
            if let Some(envelope_digest) = &entry.envelope_digest {
                output.push_str(&format!(
                    "entry.{index}.envelope_digest={envelope_digest}\n"
                ));
            }
            if let Some(subject_path) = &entry.subject_path {
                output.push_str(&format!("entry.{index}.subject={subject_path}\n"));
            }
        }
        output
    }

    /// Hash canonical manifest bytes for binding to an external consumer context.
    pub fn canonical_digest(&self) -> String {
        digest_subject(self.to_manifest().as_bytes())
    }
}

/// 证据完整性验证失败时使用的稳定机器码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceIntegrityBlocker {
    /// 信封格式已被改写或不受当前 verifier 支持。
    EnvelopeFormatInvalid,
    /// 来源、环境、执行者或时间戳不再满足必填身份约束。
    EnvelopeIdentityInvalid,
    /// 来源提交不是规范的小写 40 字符十六进制 SHA。
    SourceCommitInvalid,
    /// 来源提交与可信 checkout/build commit 不一致。
    SourceCommitMismatch,
    /// 主题摘要不是规范的小写 `sha256:<64 hex>`。
    SubjectDigestInvalid,
    /// 主题原始字节重算摘要后与声明不一致。
    SubjectDigestMismatch,
    /// 主题原始字节长度与声明不一致。
    SubjectSizeMismatch,
    /// 同一 bundle 中出现多个不同来源提交。
    BundleMixedSourceCommit,
    /// 平台 subject 的公开身份字段不满足 canonical 约束。
    PlatformIdentityInvalid,
    /// 平台 subject 的 tag 与外部可信 tag 不一致。
    PlatformTagMismatch,
    /// 平台 subject 的 commit 与外部可信 commit 不一致。
    PlatformCommitMismatch,
    /// 平台 subject 的 run id 与外部可信 run id 不一致。
    PlatformRunIdMismatch,
    /// 平台 subject 的 run attempt 与外部可信 attempt 不一致。
    PlatformRunAttemptMismatch,
    /// 平台 target 与 OS/architecture 映射不一致。
    PlatformTargetInvalid,
    /// 平台 artifact 的声明与原始 bytes 不一致。
    PlatformArtifactMismatch,
    /// capture manifest 的格式或内容无效。
    CaptureManifestInvalid,
    /// capture manifest 摘要与原始 JSON 不一致。
    CaptureManifestDigestMismatch,
    /// capture manifest 声明的原始流大小不一致。
    CaptureStreamSizeMismatch,
    /// capture manifest 声明的 stdout 摘要不一致。
    CaptureStdoutDigestMismatch,
    /// capture manifest 声明的 stderr 摘要不一致。
    CaptureStderrDigestMismatch,
    /// capture 必须是成功 outcome。
    CaptureOutcomeInvalid,
    /// capture 的命令、runner 或可信 run/platform 身份不一致。
    CaptureIdentityMismatch,
    /// toolchain capture 与平台 subject 不一致。
    ToolchainCaptureMismatch,
    /// toolchain stdout 与 subject 中的规范化版本不一致。
    ToolchainMismatch,
    /// smoke stdout 没有报告 subject 中 release tag 对应的 Eva 版本。
    PlatformSmokeMismatch,
    /// envelope bytes 的摘要或 subject 绑定不一致。
    EnvelopeDigestMismatch,
    /// bundle 条目顺序不是 canonical 顺序。
    BundleOrderInvalid,
    /// bundle 包含重复的平台身份。
    BundleDuplicateEntry,
    /// bundle 条目索引存在缺口。
    BundleIndexHole,
    /// bundle 包含未知字段或未知条目。
    BundleUnknownEntry,
    /// bundle 条目之间存在相互冲突的身份/工件声明。
    BundleConflict,
    /// bundle 自身摘要与内容重算值不一致。
    BundleDigestMismatch,
}

impl EvidenceIntegrityBlocker {
    /// 返回供 gate、JSON 和日志复用的稳定 blocker code。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvelopeFormatInvalid => "evidence_envelope_format_invalid",
            Self::EnvelopeIdentityInvalid => "evidence_envelope_identity_invalid",
            Self::SourceCommitInvalid => "evidence_source_commit_invalid",
            Self::SourceCommitMismatch => "evidence_source_commit_mismatch",
            Self::SubjectDigestInvalid => "evidence_subject_digest_invalid",
            Self::SubjectDigestMismatch => "evidence_subject_digest_mismatch",
            Self::SubjectSizeMismatch => "evidence_subject_size_mismatch",
            Self::BundleMixedSourceCommit => "evidence_bundle_mixed_source_commit",
            Self::PlatformIdentityInvalid => "evidence_platform_identity_invalid",
            Self::PlatformTagMismatch => "evidence_platform_tag_mismatch",
            Self::PlatformCommitMismatch => "evidence_platform_commit_mismatch",
            Self::PlatformRunIdMismatch => "evidence_platform_run_id_mismatch",
            Self::PlatformRunAttemptMismatch => "evidence_platform_run_attempt_mismatch",
            Self::PlatformTargetInvalid => "evidence_platform_target_invalid",
            Self::PlatformArtifactMismatch => "evidence_platform_artifact_mismatch",
            Self::CaptureManifestInvalid => "evidence_capture_manifest_invalid",
            Self::CaptureManifestDigestMismatch => "evidence_capture_manifest_digest_mismatch",
            Self::CaptureStreamSizeMismatch => "evidence_capture_stream_size_mismatch",
            Self::CaptureStdoutDigestMismatch => "evidence_capture_stdout_digest_mismatch",
            Self::CaptureStderrDigestMismatch => "evidence_capture_stderr_digest_mismatch",
            Self::CaptureOutcomeInvalid => "evidence_capture_outcome_invalid",
            Self::CaptureIdentityMismatch => "evidence_capture_identity_mismatch",
            Self::ToolchainCaptureMismatch => "evidence_toolchain_capture_mismatch",
            Self::ToolchainMismatch => "evidence_toolchain_mismatch",
            Self::PlatformSmokeMismatch => "evidence_platform_smoke_mismatch",
            Self::EnvelopeDigestMismatch => "evidence_envelope_digest_mismatch",
            Self::BundleOrderInvalid => "evidence_bundle_order_invalid",
            Self::BundleDuplicateEntry => "evidence_bundle_duplicate_entry",
            Self::BundleIndexHole => "evidence_bundle_index_hole",
            Self::BundleUnknownEntry => "evidence_bundle_unknown_entry",
            Self::BundleConflict => "evidence_bundle_conflict",
            Self::BundleDigestMismatch => "evidence_bundle_digest_mismatch",
        }
    }
}

impl fmt::Display for EvidenceIntegrityBlocker {
    /// 以稳定机器码展示 blocker。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl EvidenceKind {
    /// 所有受支持种类，顺序同时作为稳定展示顺序。
    pub const ALL: [Self; 4] = [
        Self::Declaration,
        Self::Fixture,
        Self::Operator,
        Self::Measurement,
    ];

    /// 返回清单中使用的稳定小写标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::Fixture => "fixture",
            Self::Operator => "operator",
            Self::Measurement => "measurement",
        }
    }

    /// 从稳定小写标识解析证据种类，未知值失败关闭。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "declaration" => Ok(Self::Declaration),
            "fixture" => Ok(Self::Fixture),
            "operator" => Ok(Self::Operator),
            "measurement" => Ok(Self::Measurement),
            _ => Err(
                EvaError::invalid_argument("unsupported release evidence kind")
                    .with_context("kind", value),
            ),
        }
    }
}

impl fmt::Display for EvidenceKind {
    /// 以稳定清单标识展示证据种类。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 将一项发布证据绑定到来源提交、执行环境和被测主题的统一信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceEnvelope {
    /// 证据信封格式版本。
    pub format: String,
    /// 声明、fixture、操作员记录或机器测量分类。
    pub kind: EvidenceKind,
    /// 产生证据的命令、工作流、模块或外部系统标识。
    pub source: String,
    /// 产生证据时声明的来源提交；其哈希语义由 provenance verifier 校验。
    pub source_commit: String,
    /// 证据运行环境的非敏感身份描述。
    pub environment: String,
    /// 执行命令或采集证据的 runner、服务或操作员身份。
    pub executor: String,
    /// 证据完成时的 Unix epoch 毫秒时间戳。
    pub timestamp: u128,
    /// 被测主题摘要声明；其算法和实际字节一致性由 subject verifier 校验。
    pub subject_digest: String,
}

/// 一个信封、主题原始字节及同一主题的额外完整性声明。
#[derive(Debug, Clone)]
pub struct EvidenceSubject<'a> {
    /// 声明来源提交和主题摘要的信封。
    pub envelope: &'a EvidenceEnvelope,
    /// 从 artifact、命令输出或 canonical evidence 文档读取的原始字节。
    pub subject_bytes: &'a [u8],
    /// artifact/provenance 等嵌套结构中的额外来源提交声明。
    source_commit_claims: Vec<(&'static str, &'a str)>,
    /// 嵌套结构中必须指向同一主题字节的额外摘要声明。
    subject_digest_claims: Vec<(&'static str, &'a str)>,
    /// 嵌套结构中必须等于主题实际长度的额外 size 声明。
    subject_size_claims: Vec<(&'static str, u64)>,
}

impl<'a> EvidenceSubject<'a> {
    /// 将信封与独立读取的主题字节配对用于验证。
    pub fn new(envelope: &'a EvidenceEnvelope, subject_bytes: &'a [u8]) -> Self {
        Self {
            envelope,
            subject_bytes,
            source_commit_claims: Vec::new(),
            subject_digest_claims: Vec::new(),
            subject_size_claims: Vec::new(),
        }
    }

    /// 添加一个必须与可信 build commit 一致的嵌套提交声明。
    pub fn with_source_commit_claim(mut self, label: &'static str, source_commit: &'a str) -> Self {
        self.source_commit_claims.push((label, source_commit));
        self
    }

    /// 添加一个必须与同一主题原始字节匹配的嵌套摘要声明。
    pub fn with_subject_digest_claim(
        mut self,
        label: &'static str,
        subject_digest: &'a str,
    ) -> Self {
        self.subject_digest_claims.push((label, subject_digest));
        self
    }

    /// 添加一个必须与同一主题原始字节长度匹配的嵌套 size 声明。
    pub fn with_subject_size_claim(mut self, label: &'static str, subject_size_bytes: u64) -> Self {
        self.subject_size_claims.push((label, subject_size_bytes));
        self
    }
}

/// The digest/size claims copied from one command capture manifest.
///
/// The manifest itself is deliberately not trusted by this type.  A verifier
/// must pass the original manifest, stdout, and stderr bytes to
/// [`ReleaseCaptureEvidence::verify_bytes`] so every claim is recomputed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseCaptureEvidence {
    pub id: String,
    pub outcome: String,
    pub manifest_digest: String,
    pub manifest_size: u64,
    pub stdout_digest: String,
    pub stdout_size: u64,
    pub stderr_digest: String,
    pub stderr_size: u64,
}

impl ReleaseCaptureEvidence {
    /// Parse the producer JSON and bind its identity while hashing the exact
    /// manifest/stdout/stderr bytes supplied by the caller.
    pub fn from_manifest_bytes(
        manifest_bytes: &[u8],
        stdout_bytes: &[u8],
        stderr_bytes: &[u8],
    ) -> Result<Self, EvaError> {
        let root = match JsonParser::new(manifest_bytes).parse() {
            Ok(JsonValue::Object(value)) => value,
            _ => {
                return Err(EvaError::invalid_argument(
                    "release capture manifest must be a JSON object",
                ))
            }
        };
        let format = json_string(&root, &["format"]).ok_or_else(|| {
            EvaError::invalid_argument("release capture manifest is missing format")
        })?;
        if format != RELEASE_COMMAND_CAPTURE_FORMAT {
            return Err(
                EvaError::invalid_argument("unsupported release command capture format")
                    .with_context("format", format),
            );
        }
        let id = json_string(&root, &["capture_id"]).ok_or_else(|| {
            EvaError::invalid_argument("release capture manifest is missing capture_id")
        })?;
        let outcome = json_string(&root, &["outcome"]).ok_or_else(|| {
            EvaError::invalid_argument("release capture manifest is missing outcome")
        })?;
        Self::from_capture_bytes(id, outcome, manifest_bytes, stdout_bytes, stderr_bytes)
    }

    /// Construct a capture claim from the exact bytes written by the producer.
    pub fn from_capture_bytes(
        id: impl Into<String>,
        outcome: impl Into<String>,
        manifest_bytes: &[u8],
        stdout_bytes: &[u8],
        stderr_bytes: &[u8],
    ) -> Result<Self, EvaError> {
        let id = validate_capture_id(id.into())?;
        let outcome = validate_capture_outcome(outcome.into())?;
        Ok(Self {
            id,
            outcome,
            manifest_digest: digest_subject(manifest_bytes),
            manifest_size: manifest_bytes.len() as u64,
            stdout_digest: digest_subject(stdout_bytes),
            stdout_size: stdout_bytes.len() as u64,
            stderr_digest: digest_subject(stderr_bytes),
            stderr_size: stderr_bytes.len() as u64,
        })
    }

    /// Construct a claim when the producer already has canonical digest fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        outcome: impl Into<String>,
        manifest_digest: impl Into<String>,
        manifest_size: u64,
        stdout_digest: impl Into<String>,
        stdout_size: u64,
        stderr_digest: impl Into<String>,
        stderr_size: u64,
    ) -> Result<Self, EvaError> {
        let id = validate_capture_id(id.into())?;
        let outcome = validate_capture_outcome(outcome.into())?;
        let manifest_digest = manifest_digest.into();
        validate_canonical_subject_digest(&manifest_digest)?;
        let stdout_digest = validate_digest_value("capture stdout digest", stdout_digest.into())?;
        let stderr_digest = validate_digest_value("capture stderr digest", stderr_digest.into())?;
        Ok(Self {
            id,
            outcome,
            manifest_digest,
            manifest_size,
            stdout_digest,
            stdout_size,
            stderr_digest,
            stderr_size,
        })
    }

    /// Verify all claims against the raw manifest and stream bytes.
    pub fn verify_bytes(
        &self,
        manifest_bytes: &[u8],
        stdout_bytes: &[u8],
        stderr_bytes: &[u8],
    ) -> Vec<EvidenceIntegrityBlocker> {
        let mut blockers = Vec::new();
        let manifest_digest = digest_subject(manifest_bytes);
        if self.manifest_digest != manifest_digest
            || self.manifest_size != manifest_bytes.len() as u64
        {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureManifestDigestMismatch,
            );
        }
        if self.stdout_digest != digest_subject(stdout_bytes) {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureStdoutDigestMismatch,
            );
        }
        if self.stderr_digest != digest_subject(stderr_bytes) {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureStderrDigestMismatch,
            );
        }
        if self.stdout_size != stdout_bytes.len() as u64
            || self.stderr_size != stderr_bytes.len() as u64
        {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureStreamSizeMismatch,
            );
        }
        if self.outcome != "success" {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureOutcomeInvalid,
            );
        }
        blockers
    }

    fn to_manifest_lines(&self, prefix: &str, output: &mut String) {
        output.push_str(&format!(
            "{prefix}.id={}\n{prefix}.outcome={}\n{prefix}.manifest_digest={}\n{prefix}.manifest_size={}\n{prefix}.stdout_digest={}\n{prefix}.stdout_size={}\n{prefix}.stderr_digest={}\n{prefix}.stderr_size={}\n",
            self.id,
            self.outcome,
            self.manifest_digest,
            self.manifest_size,
            self.stdout_digest,
            self.stdout_size,
            self.stderr_digest,
            self.stderr_size,
        ));
    }
}

/// Canonical platform-level evidence.  The `envelope` is bound to the
/// canonical subject bytes returned by [`Self::to_subject_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePlatformEvidence {
    pub format: String,
    pub tag: String,
    pub commit: String,
    pub os: String,
    pub arch: String,
    pub toolchain: String,
    pub run_id: String,
    pub run_attempt: String,
    pub job: String,
    pub artifact_name: String,
    pub artifact_target: String,
    pub artifact_digest: String,
    pub artifact_size: u64,
    pub capture: ReleaseCaptureEvidence,
    pub toolchain_capture: ReleaseCaptureEvidence,
    pub envelope: EvidenceEnvelope,
    /// Digest of the canonical envelope bytes captured alongside the subject.
    pub envelope_digest: String,
}

impl ReleasePlatformEvidence {
    /// Create and bind a platform subject to an existing evidence envelope.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tag: impl Into<String>,
        commit: impl Into<String>,
        os: impl Into<String>,
        arch: impl Into<String>,
        toolchain: impl Into<String>,
        run_id: impl Into<String>,
        run_attempt: impl Into<String>,
        job: impl Into<String>,
        artifact_name: impl Into<String>,
        artifact_target: impl Into<String>,
        artifact_digest: impl Into<String>,
        artifact_size: u64,
        capture: ReleaseCaptureEvidence,
        toolchain_capture: ReleaseCaptureEvidence,
        envelope: EvidenceEnvelope,
    ) -> Result<Self, EvaError> {
        let envelope_digest = digest_subject(envelope.to_manifest().as_bytes());
        let value = Self {
            format: RELEASE_PLATFORM_EVIDENCE_FORMAT.to_owned(),
            tag: validate_release_tag(tag.into())?,
            commit: commit.into(),
            os: normalize_subject_os(os.into())?,
            arch: normalize_subject_arch(arch.into())?,
            toolchain: validate_toolchain(toolchain.into())?,
            run_id: validate_positive_decimal("platform run id", run_id.into())?,
            run_attempt: validate_positive_decimal("platform run attempt", run_attempt.into())?,
            job: validate_platform_text("platform job", job.into())?,
            artifact_name: validate_platform_text("platform artifact name", artifact_name.into())?,
            artifact_target: validate_platform_text(
                "platform artifact target",
                artifact_target.into(),
            )?,
            artifact_digest: validate_digest_value(
                "platform artifact digest",
                artifact_digest.into(),
            )?,
            artifact_size,
            capture,
            toolchain_capture,
            envelope,
            envelope_digest,
        };
        validate_canonical_source_commit(&value.commit)?;
        validate_target_mapping(&value.os, &value.arch, &value.artifact_target)?;
        validate_artifact_name_mapping(&value.tag, &value.artifact_target, &value.artifact_name)?;
        let subject_digest = digest_subject(&value.to_subject_bytes());
        if value.envelope.subject_digest != subject_digest {
            return Err(EvaError::invalid_argument(
                "platform evidence envelope subject digest does not match canonical subject",
            )
            .with_context("expected_digest", subject_digest)
            .with_context("actual_digest", value.envelope.subject_digest.clone()));
        }
        Ok(value)
    }

    /// Return the canonical subject bytes, excluding the envelope wrapper.
    pub fn to_subject_bytes(&self) -> Vec<u8> {
        self.to_manifest().into_bytes()
    }

    /// Return canonical key/value subject text.
    pub fn to_manifest(&self) -> String {
        let mut output = format!(
            "format={}\ntag={}\ncommit={}\nos={}\narch={}\ntoolchain={}\nrun_id={}\nrun_attempt={}\njob={}\nartifact.name={}\nartifact.target={}\nartifact.digest={}\nartifact.size_bytes={}\n",
            RELEASE_PLATFORM_EVIDENCE_FORMAT,
            self.tag,
            self.commit,
            self.os,
            self.arch,
            self.toolchain,
            self.run_id,
            self.run_attempt,
            self.job,
            self.artifact_name,
            self.artifact_target,
            self.artifact_digest,
            self.artifact_size,
        );
        self.capture.to_manifest_lines("capture", &mut output);
        self.toolchain_capture
            .to_manifest_lines("toolchain_capture", &mut output);
        output
    }

    /// Parse a canonical subject and bind it to an envelope.
    pub fn parse_manifest(data: &str, envelope: EvidenceEnvelope) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        let expected = [
            "format",
            "tag",
            "commit",
            "os",
            "arch",
            "toolchain",
            "run_id",
            "run_attempt",
            "job",
            "artifact.name",
            "artifact.target",
            "artifact.digest",
            "artifact.size_bytes",
        ];
        for key in fields.keys() {
            if !expected.contains(&key.as_str())
                && !is_platform_capture_field(key, "capture")
                && !is_platform_capture_field(key, "toolchain_capture")
            {
                return Err(EvaError::invalid_argument(
                    "platform evidence subject contains an unknown field",
                )
                .with_context("field", key));
            }
        }
        if required(&fields, "format")? != RELEASE_PLATFORM_EVIDENCE_FORMAT {
            return Err(EvaError::invalid_argument(
                "unsupported release platform evidence format",
            ));
        }
        let capture = parse_capture_fields(&fields, "capture")?;
        let toolchain_capture = parse_capture_fields(&fields, "toolchain_capture")?;
        let artifact_size = parse_u64_field(&fields, "artifact.size_bytes")?;
        let value = Self::new(
            required(&fields, "tag")?,
            required(&fields, "commit")?,
            required(&fields, "os")?,
            required(&fields, "arch")?,
            required(&fields, "toolchain")?,
            required(&fields, "run_id")?,
            required(&fields, "run_attempt")?,
            required(&fields, "job")?,
            required(&fields, "artifact.name")?,
            required(&fields, "artifact.target")?,
            required(&fields, "artifact.digest")?,
            artifact_size,
            capture,
            toolchain_capture,
            envelope,
        )?;
        if value.to_manifest() != data {
            return Err(EvaError::invalid_argument(
                "release platform evidence subject is not canonical",
            ));
        }
        Ok(value)
    }

    /// Verify this subject against trusted run identity and all raw bytes.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_with_bytes(
        &self,
        expected_tag: &str,
        expected_commit: &str,
        expected_run_id: &str,
        expected_run_attempt: &str,
        capture_manifest: &[u8],
        capture_stdout: &[u8],
        capture_stderr: &[u8],
        toolchain_manifest: &[u8],
        toolchain_stdout: &[u8],
        toolchain_stderr: &[u8],
        artifact_bytes: &[u8],
    ) -> ReleasePlatformVerificationReport {
        let mut report = ReleasePlatformVerificationReport {
            status: "verified".to_owned(),
            ..Default::default()
        };
        let identity_checks = [
            (
                &self.tag,
                expected_tag,
                EvidenceIntegrityBlocker::PlatformTagMismatch,
            ),
            (
                &self.commit,
                expected_commit,
                EvidenceIntegrityBlocker::PlatformCommitMismatch,
            ),
            (
                &self.run_id,
                expected_run_id,
                EvidenceIntegrityBlocker::PlatformRunIdMismatch,
            ),
            (
                &self.run_attempt,
                expected_run_attempt,
                EvidenceIntegrityBlocker::PlatformRunAttemptMismatch,
            ),
        ];
        for (actual, expected, blocker) in identity_checks {
            if actual != expected {
                report.record(blocker);
            }
        }
        if self.format != RELEASE_PLATFORM_EVIDENCE_FORMAT
            || !is_valid_release_tag(&self.tag)
            || !is_canonical_source_commit(&self.commit)
            || !is_valid_manifest_text(&self.os)
            || !is_valid_manifest_text(&self.arch)
            || !is_valid_manifest_text(&self.toolchain)
            || !is_positive_decimal(&self.run_id)
            || !is_positive_decimal(&self.run_attempt)
            || !is_valid_manifest_text(&self.job)
            || !is_valid_manifest_text(&self.artifact_name)
            || !is_valid_manifest_text(&self.artifact_target)
        {
            report.record(EvidenceIntegrityBlocker::PlatformIdentityInvalid);
        }
        if validate_target_mapping(&self.os, &self.arch, &self.artifact_target).is_err()
            || validate_artifact_name_mapping(&self.tag, &self.artifact_target, &self.artifact_name)
                .is_err()
        {
            report.record(EvidenceIntegrityBlocker::PlatformTargetInvalid);
        }
        if self.artifact_digest != digest_subject(artifact_bytes)
            || self.artifact_size != artifact_bytes.len() as u64
        {
            report.record(EvidenceIntegrityBlocker::PlatformArtifactMismatch);
        }
        for blocker in self
            .capture
            .verify_bytes(capture_manifest, capture_stdout, capture_stderr)
        {
            report.record(blocker);
        }
        for blocker in verify_capture_manifest(
            &self.capture,
            capture_manifest,
            capture_stdout,
            capture_stderr,
            CaptureRole::Smoke,
            &self.run_id,
            &self.run_attempt,
            &self.os,
            &self.arch,
            &self.job,
            &self.envelope.executor,
        ) {
            report.record(blocker);
        }
        for blocker in self.toolchain_capture.verify_bytes(
            toolchain_manifest,
            toolchain_stdout,
            toolchain_stderr,
        ) {
            report.record(match blocker {
                EvidenceIntegrityBlocker::CaptureManifestDigestMismatch
                | EvidenceIntegrityBlocker::CaptureStdoutDigestMismatch
                | EvidenceIntegrityBlocker::CaptureStderrDigestMismatch
                | EvidenceIntegrityBlocker::CaptureStreamSizeMismatch
                | EvidenceIntegrityBlocker::CaptureOutcomeInvalid => {
                    EvidenceIntegrityBlocker::ToolchainCaptureMismatch
                }
                other => other,
            });
        }
        for blocker in verify_capture_manifest(
            &self.toolchain_capture,
            toolchain_manifest,
            toolchain_stdout,
            toolchain_stderr,
            CaptureRole::Toolchain,
            &self.run_id,
            &self.run_attempt,
            &self.os,
            &self.arch,
            &self.job,
            &self.envelope.executor,
        ) {
            report.record(match blocker {
                EvidenceIntegrityBlocker::CaptureManifestInvalid
                | EvidenceIntegrityBlocker::CaptureManifestDigestMismatch
                | EvidenceIntegrityBlocker::CaptureStdoutDigestMismatch
                | EvidenceIntegrityBlocker::CaptureStderrDigestMismatch
                | EvidenceIntegrityBlocker::CaptureStreamSizeMismatch
                | EvidenceIntegrityBlocker::CaptureOutcomeInvalid => {
                    EvidenceIntegrityBlocker::ToolchainCaptureMismatch
                }
                EvidenceIntegrityBlocker::CaptureIdentityMismatch => {
                    EvidenceIntegrityBlocker::ToolchainCaptureMismatch
                }
                other => other,
            });
        }
        let normalized_toolchain = normalize_toolchain_output(toolchain_stdout);
        if normalized_toolchain != self.toolchain || !normalized_toolchain.starts_with("rustc ") {
            report.record(EvidenceIntegrityBlocker::ToolchainMismatch);
        }
        let expected_version = self.tag.strip_prefix('v').unwrap_or_default();
        let expected_smoke = format!("eva {expected_version}");
        if normalize_capture_output(capture_stdout).lines().next() != Some(&expected_smoke) {
            report.record(EvidenceIntegrityBlocker::PlatformSmokeMismatch);
        }
        let expected_source = format!("release-platform:{}", self.artifact_target);
        let expected_environment = format!("{}-{};{}", self.os, self.arch, self.toolchain);
        let capture_timestamp = capture_finished_at_millis(capture_manifest);
        let toolchain_timestamp = capture_finished_at_millis(toolchain_manifest);
        let expected_timestamp = capture_timestamp
            .zip(toolchain_timestamp)
            .map(|(capture, toolchain)| capture.max(toolchain));
        if self.envelope.kind != EvidenceKind::Measurement
            || self.envelope.source != expected_source
            || self.envelope.environment != expected_environment
            || expected_timestamp != Some(self.envelope.timestamp)
        {
            report.record(EvidenceIntegrityBlocker::EnvelopeIdentityInvalid);
        }
        let subject_digest = digest_subject(&self.to_subject_bytes());
        if self.envelope.subject_digest != subject_digest {
            report.record(EvidenceIntegrityBlocker::EnvelopeDigestMismatch);
        }
        if self.envelope_digest != digest_subject(self.envelope.to_manifest().as_bytes()) {
            report.record(EvidenceIntegrityBlocker::EnvelopeDigestMismatch);
        }
        let envelope_report = self
            .envelope
            .verify_subject(expected_commit, &self.to_subject_bytes());
        if let Ok(envelope_report) = envelope_report {
            for blocker in envelope_report.blocked_reasons {
                report.record(blocker);
            }
        } else {
            report.record(EvidenceIntegrityBlocker::EnvelopeDigestMismatch);
        }
        report
    }

    /// Alias used by callers that prefer a shorter verifier name.
    #[allow(clippy::too_many_arguments)]
    pub fn verify(
        &self,
        expected_tag: &str,
        expected_commit: &str,
        expected_run_id: &str,
        expected_run_attempt: &str,
        capture_manifest: &[u8],
        capture_stdout: &[u8],
        capture_stderr: &[u8],
        toolchain_manifest: &[u8],
        toolchain_stdout: &[u8],
        toolchain_stderr: &[u8],
        artifact_bytes: &[u8],
    ) -> ReleasePlatformVerificationReport {
        self.verify_with_bytes(
            expected_tag,
            expected_commit,
            expected_run_id,
            expected_run_attempt,
            capture_manifest,
            capture_stdout,
            capture_stderr,
            toolchain_manifest,
            toolchain_stdout,
            toolchain_stderr,
            artifact_bytes,
        )
    }
}

/// Verification result for one platform subject or an aggregate bundle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleasePlatformVerificationReport {
    pub status: String,
    pub blocked_reasons: Vec<EvidenceIntegrityBlocker>,
    pub audit: Vec<String>,
}

impl ReleasePlatformVerificationReport {
    pub fn is_verified(&self) -> bool {
        self.blocked_reasons.is_empty()
    }

    fn record(&mut self, blocker: EvidenceIntegrityBlocker) {
        if self.status.is_empty() {
            self.status = "verified".to_owned();
        }
        self.status = "blocked".to_owned();
        push_unique(&mut self.blocked_reasons, blocker);
    }
}

/// Raw bytes associated with one platform evidence entry.
#[derive(Debug, Clone, Copy)]
pub struct ReleasePlatformEvidenceInput<'a> {
    pub evidence: &'a ReleasePlatformEvidence,
    pub capture_manifest: &'a [u8],
    pub capture_stdout: &'a [u8],
    pub capture_stderr: &'a [u8],
    pub toolchain_manifest: &'a [u8],
    pub toolchain_stdout: &'a [u8],
    pub toolchain_stderr: &'a [u8],
    pub artifact: &'a [u8],
}

/// Deterministically ordered platform evidence aggregate with a self-checking
/// digest.  Coverage policy is intentionally outside this type (W0-L08).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePlatformEvidenceBundle {
    pub format: String,
    pub entries: Vec<ReleasePlatformEvidence>,
    pub bundle_digest: String,
}

impl ReleasePlatformEvidenceBundle {
    /// Sort entries by stable platform identity and calculate the bundle digest.
    pub fn new(mut entries: Vec<ReleasePlatformEvidence>) -> Result<Self, EvaError> {
        if entries.is_empty() {
            return Err(EvaError::invalid_argument(
                "release platform evidence bundle must contain at least one entry",
            ));
        }
        entries.sort_by_key(platform_sort_key);
        for pair in entries.windows(2) {
            if platform_identity_key(&pair[0]) == platform_identity_key(&pair[1]) {
                return Err(EvaError::invalid_argument(
                    "release platform evidence bundle contains duplicate platform identity",
                ));
            }
        }
        let mut bundle = Self {
            format: RELEASE_PLATFORM_BUNDLE_FORMAT.to_owned(),
            entries,
            bundle_digest: String::new(),
        };
        bundle.bundle_digest = digest_subject(&bundle.canonical_payload());
        Ok(bundle)
    }

    /// Canonical bytes covered by `bundle_digest`.
    pub fn canonical_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        for entry in &self.entries {
            let subject = entry.to_subject_bytes();
            let envelope = entry.envelope.to_manifest();
            append_length_delimited(&mut payload, &subject);
            append_length_delimited(&mut payload, envelope.as_bytes());
        }
        payload
    }

    /// Serialize the bundle, including hex-encoded canonical subject and envelope bytes.
    pub fn to_manifest(&self) -> String {
        let mut output = format!(
            "format={}\nbundle_digest={}\nentry_count={}\n",
            self.format,
            self.bundle_digest,
            self.entries.len()
        );
        for (index, entry) in self.entries.iter().enumerate() {
            output.push_str(&format!(
                "entry.{index}.subject_hex={}\nentry.{index}.envelope_hex={}\n",
                hex_encode(&entry.to_subject_bytes()),
                hex_encode(entry.envelope.to_manifest().as_bytes()),
            ));
        }
        output
    }

    /// Parse a canonical bundle manifest.  Entry subjects are decoded and
    /// rebound to their serialized envelopes before duplicate/order checks.
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        if required(&fields, "format")? != RELEASE_PLATFORM_BUNDLE_FORMAT {
            return Err(EvaError::invalid_argument(
                "unsupported release platform bundle format",
            ));
        }
        let entry_count = parse_u64_field(&fields, "entry_count")? as usize;
        let mut indexes = BTreeSet::new();
        for key in fields.keys() {
            if matches!(key.as_str(), "format" | "bundle_digest" | "entry_count") {
                continue;
            }
            let Some(rest) = key.strip_prefix("entry.") else {
                return Err(EvaError::invalid_argument(
                    "release platform bundle contains an unknown field",
                )
                .with_context("field", key));
            };
            let Some((index, suffix)) = rest.split_once('.') else {
                return Err(EvaError::invalid_argument(
                    "release platform bundle contains an unknown entry field",
                )
                .with_context("field", key));
            };
            let index = index.parse::<usize>().map_err(|error| {
                EvaError::invalid_argument("release platform bundle entry index is invalid")
                    .with_context("field", key)
                    .with_context("parse_error", error.to_string())
            })?;
            if !matches!(suffix, "subject_hex" | "envelope_hex") {
                return Err(EvaError::invalid_argument(
                    "release platform bundle contains an unknown entry field",
                )
                .with_context("field", key));
            }
            indexes.insert(index);
        }
        if indexes.len() != entry_count
            || indexes
                .iter()
                .copied()
                .enumerate()
                .any(|(expected, actual)| expected != actual)
        {
            return Err(EvaError::invalid_argument(
                "release platform bundle entry indexes must be contiguous from zero",
            ));
        }
        let mut entries = Vec::with_capacity(entry_count);
        for index in indexes {
            let subject_hex = required(&fields, &format!("entry.{index}.subject_hex"))?;
            let envelope_hex = required(&fields, &format!("entry.{index}.envelope_hex"))?;
            let subject = hex_decode(&subject_hex)?;
            let envelope_bytes = hex_decode(&envelope_hex)?;
            let envelope_text = String::from_utf8(envelope_bytes).map_err(|error| {
                EvaError::invalid_argument("release platform bundle envelope is not UTF-8")
                    .with_context("parse_error", error.to_string())
            })?;
            let envelope = EvidenceEnvelope::parse_manifest(&envelope_text)?;
            if envelope.to_manifest() != envelope_text {
                return Err(EvaError::invalid_argument(
                    "release platform bundle envelope is not canonical",
                ));
            }
            let subject_text = String::from_utf8(subject).map_err(|error| {
                EvaError::invalid_argument("release platform bundle subject is not UTF-8")
                    .with_context("parse_error", error.to_string())
            })?;
            entries.push(ReleasePlatformEvidence::parse_manifest(
                &subject_text,
                envelope,
            )?);
        }
        if entries
            .windows(2)
            .any(|pair| platform_sort_key(&pair[0]) > platform_sort_key(&pair[1]))
        {
            return Err(EvaError::invalid_argument(
                "release platform bundle entries must use canonical platform order",
            ));
        }
        let bundle = Self::new(entries)?;
        let claimed = required(&fields, "bundle_digest")?;
        if claimed != bundle.bundle_digest {
            return Err(EvaError::invalid_argument(
                "release platform bundle digest does not match canonical entries",
            )
            .with_context("expected_digest", bundle.bundle_digest)
            .with_context("actual_digest", claimed));
        }
        Ok(bundle)
    }

    /// Verify every entry and the self-digest against trusted run identity.
    pub fn verify(
        &self,
        expected_tag: &str,
        expected_commit: &str,
        expected_run_id: &str,
        expected_run_attempt: &str,
        inputs: &[ReleasePlatformEvidenceInput<'_>],
    ) -> ReleasePlatformVerificationReport {
        let mut report = ReleasePlatformVerificationReport {
            status: "verified".to_owned(),
            ..Default::default()
        };
        if self.format != RELEASE_PLATFORM_BUNDLE_FORMAT {
            report.record(EvidenceIntegrityBlocker::BundleUnknownEntry);
        }
        if self.entries.is_empty() {
            report.record(EvidenceIntegrityBlocker::BundleUnknownEntry);
        }
        if self
            .entries
            .windows(2)
            .any(|pair| platform_sort_key(&pair[0]) > platform_sort_key(&pair[1]))
        {
            report.record(EvidenceIntegrityBlocker::BundleOrderInvalid);
        }
        let mut identities = BTreeSet::new();
        for entry in &self.entries {
            if !identities.insert(platform_identity_key(entry)) {
                report.record(EvidenceIntegrityBlocker::BundleDuplicateEntry);
            }
        }
        if self.bundle_digest != digest_subject(&self.canonical_payload()) {
            report.record(EvidenceIntegrityBlocker::BundleDigestMismatch);
        }
        if inputs.len() != self.entries.len() {
            report.record(EvidenceIntegrityBlocker::BundleUnknownEntry);
        }
        for entry in &self.entries {
            let Some(input) = inputs.iter().find(|input| {
                platform_identity_key(input.evidence) == platform_identity_key(entry)
            }) else {
                report.record(EvidenceIntegrityBlocker::BundleUnknownEntry);
                continue;
            };
            let entry_report = entry.verify_with_bytes(
                expected_tag,
                expected_commit,
                expected_run_id,
                expected_run_attempt,
                input.capture_manifest,
                input.capture_stdout,
                input.capture_stderr,
                input.toolchain_manifest,
                input.toolchain_stdout,
                input.toolchain_stderr,
                input.artifact,
            );
            for blocker in entry_report.blocked_reasons {
                report.record(blocker);
            }
        }
        let mut commits = BTreeSet::new();
        let mut tags = BTreeSet::new();
        for entry in &self.entries {
            commits.insert(entry.commit.clone());
            tags.insert(entry.tag.clone());
        }
        if commits.len() > 1 || tags.len() > 1 {
            report.record(EvidenceIntegrityBlocker::BundleConflict);
        }
        report
    }
}

/// 单项或 bundle 的来源提交与主题摘要验证结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceVerificationReport {
    /// `verified` 或 `blocked`。
    pub status: String,
    /// verifier 从可信 checkout 或 CI context 接收的构建提交。
    pub expected_source_commit: String,
    /// 本次输入的主题总数；空 bundle 的 coverage 由后续 policy 处理。
    pub subject_count: usize,
    /// 单项格式、提交和摘要全部通过的主题数。
    pub verified_subject_count: usize,
    /// 已去重且按首次发现顺序排列的稳定 blocker code。
    pub blocked_reasons: Vec<EvidenceIntegrityBlocker>,
    /// 不包含主题原始字节的验证审计记录。
    pub audit: Vec<String>,
}

impl EvidenceVerificationReport {
    /// 所有输入主题和 bundle 一致性都通过时返回 true。
    pub fn is_verified(&self) -> bool {
        self.status == "verified"
    }

    /// 添加去重 blocker 并把总体状态转为 blocked。
    fn record_blocker(&mut self, blocker: EvidenceIntegrityBlocker) {
        self.status = "blocked".to_owned();
        if !self.blocked_reasons.contains(&blocker) {
            self.blocked_reasons.push(blocker);
        }
    }
}

impl EvidenceEnvelope {
    /// 校验全部必填身份字段后创建版本化证据信封。
    pub fn new(
        kind: EvidenceKind,
        source: impl Into<String>,
        source_commit: impl Into<String>,
        environment: impl Into<String>,
        executor: impl Into<String>,
        timestamp: u128,
        subject_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let source = validate_manifest_text("release evidence source", source.into())?;
        let source_commit =
            validate_manifest_text("release evidence source commit", source_commit.into())?;
        let environment =
            validate_manifest_text("release evidence environment", environment.into())?;
        let executor = validate_manifest_text("release evidence executor", executor.into())?;
        if timestamp == 0 {
            return Err(EvaError::invalid_argument(
                "release evidence timestamp must be greater than zero",
            ));
        }
        let subject_digest =
            validate_manifest_text("release evidence subject digest", subject_digest.into())?;

        Ok(Self {
            format: EVIDENCE_ENVELOPE_FORMAT.to_owned(),
            kind,
            source,
            source_commit,
            environment,
            executor,
            timestamp,
            subject_digest,
        })
    }

    /// 从已有摘要声明创建生产者信封，并要求提交和摘要使用规范格式。
    ///
    /// 该入口适用于已经由 artifact store 或受控构建流程计算摘要的主题。消费方仍须
    /// 调用 `verify_subject` 对独立读取的原始字节重新计算摘要。
    pub fn from_subject_digest(
        kind: EvidenceKind,
        source: impl Into<String>,
        source_commit: impl Into<String>,
        environment: impl Into<String>,
        executor: impl Into<String>,
        timestamp: u128,
        subject_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let source_commit = source_commit.into();
        validate_canonical_source_commit(&source_commit)?;
        let subject_digest = subject_digest.into();
        validate_canonical_subject_digest(&subject_digest)?;
        Self::new(
            kind,
            source,
            source_commit,
            environment,
            executor,
            timestamp,
            subject_digest,
        )
    }

    /// 从真实 artifact、命令输出或 canonical 文档字节重算摘要并创建信封。
    pub fn from_subject_bytes(
        kind: EvidenceKind,
        source: impl Into<String>,
        source_commit: impl Into<String>,
        environment: impl Into<String>,
        executor: impl Into<String>,
        timestamp: u128,
        subject_bytes: &[u8],
    ) -> Result<Self, EvaError> {
        Self::from_subject_digest(
            kind,
            source,
            source_commit,
            environment,
            executor,
            timestamp,
            digest_subject(subject_bytes),
        )
    }

    /// 从严格键值清单解析信封，并通过构造器重新执行全部字段约束。
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        reject_unknown_fields(&fields)?;
        let format = required(&fields, "format")?;
        if format != EVIDENCE_ENVELOPE_FORMAT {
            return Err(
                EvaError::invalid_argument("unsupported release evidence envelope format")
                    .with_context("format", format),
            );
        }

        Self::new(
            EvidenceKind::parse(&required(&fields, "kind")?)?,
            required(&fields, "source")?,
            required(&fields, "source_commit")?,
            required(&fields, "environment")?,
            required(&fields, "executor")?,
            parse_timestamp(required(&fields, "timestamp")?)?,
            required(&fields, "subject_digest")?,
        )
    }

    /// 以固定字段顺序序列化信封，供文件存储和摘要计算复用。
    pub fn to_manifest(&self) -> String {
        format!(
            "format={}\nkind={}\nsource={}\nsource_commit={}\nenvironment={}\nexecutor={}\ntimestamp={}\nsubject_digest={}\n",
            self.format,
            self.kind,
            self.source,
            self.source_commit,
            self.environment,
            self.executor,
            self.timestamp,
            self.subject_digest,
        )
    }

    /// Hash all canonical envelope fields so a trusted manifest can detect rewrites.
    pub fn canonical_digest(&self) -> String {
        digest_subject(self.to_manifest().as_bytes())
    }

    /// 使用可信构建提交和独立读取的主题字节验证当前信封。
    pub fn verify_subject(
        &self,
        expected_source_commit: &str,
        subject_bytes: &[u8],
    ) -> Result<EvidenceVerificationReport, EvaError> {
        verify_evidence_bundle(
            expected_source_commit,
            &[EvidenceSubject::new(self, subject_bytes)],
        )
    }
}

/// 验证 bundle 中每个主题的格式、来源提交和重算摘要，并拒绝混合提交。
///
/// `expected_source_commit` 必须来自可信 checkout/build context，不能从 bundle 自身
/// 推导。空 bundle 在本一致性层保持 verified；缺少必需 evidence 由 coverage policy
/// 处理。
pub fn verify_evidence_bundle(
    expected_source_commit: &str,
    subjects: &[EvidenceSubject<'_>],
) -> Result<EvidenceVerificationReport, EvaError> {
    validate_canonical_source_commit(expected_source_commit)?;
    let mut report = EvidenceVerificationReport {
        status: "verified".to_owned(),
        expected_source_commit: expected_source_commit.to_owned(),
        subject_count: subjects.len(),
        verified_subject_count: 0,
        blocked_reasons: Vec::new(),
        audit: vec![format!(
            "evidence.integrity.expected_source_commit:{expected_source_commit}"
        )],
    };
    let mut source_commits = BTreeSet::new();

    for (index, subject) in subjects.iter().enumerate() {
        let envelope = subject.envelope;
        let mut subject_verified = true;

        if envelope.format != EVIDENCE_ENVELOPE_FORMAT {
            report.record_blocker(EvidenceIntegrityBlocker::EnvelopeFormatInvalid);
            subject_verified = false;
        }

        let mut invalid_identity_fields = Vec::new();
        if !is_valid_manifest_text(&envelope.source) {
            invalid_identity_fields.push("source");
        }
        if !is_valid_manifest_text(&envelope.environment) {
            invalid_identity_fields.push("environment");
        }
        if !is_valid_manifest_text(&envelope.executor) {
            invalid_identity_fields.push("executor");
        }
        if envelope.timestamp == 0 {
            invalid_identity_fields.push("timestamp");
        }
        if !invalid_identity_fields.is_empty() {
            report.record_blocker(EvidenceIntegrityBlocker::EnvelopeIdentityInvalid);
            report.audit.push(format!(
                "evidence.integrity.subject.{index}.invalid_identity_fields:{}",
                invalid_identity_fields.join(",")
            ));
            subject_verified = false;
        }

        if is_canonical_source_commit(&envelope.source_commit) {
            source_commits.insert(envelope.source_commit.clone());
        }
        if let Some(blocker) =
            source_commit_blocker(expected_source_commit, &envelope.source_commit)
        {
            report.record_blocker(blocker);
            subject_verified = false;
        }
        for (label, source_commit) in &subject.source_commit_claims {
            if is_canonical_source_commit(source_commit) {
                source_commits.insert((*source_commit).to_owned());
            }
            if let Some(blocker) = source_commit_blocker(expected_source_commit, source_commit) {
                report.record_blocker(blocker);
                subject_verified = false;
            }
            report.audit.push(format!(
                "evidence.integrity.subject.{index}.{label}.source_commit_checked"
            ));
        }

        let actual_digest = digest_subject(subject.subject_bytes);
        if let Some(blocker) = subject_digest_blocker(&envelope.subject_digest, &actual_digest) {
            report.record_blocker(blocker);
            subject_verified = false;
        }
        for (label, subject_digest) in &subject.subject_digest_claims {
            if let Some(blocker) = subject_digest_blocker(subject_digest, &actual_digest) {
                report.record_blocker(blocker);
                subject_verified = false;
            }
            report.audit.push(format!(
                "evidence.integrity.subject.{index}.{label}.subject_digest_checked"
            ));
        }
        for (label, subject_size_bytes) in &subject.subject_size_claims {
            if u128::from(*subject_size_bytes) != subject.subject_bytes.len() as u128 {
                report.record_blocker(EvidenceIntegrityBlocker::SubjectSizeMismatch);
                subject_verified = false;
            }
            report.audit.push(format!(
                "evidence.integrity.subject.{index}.{label}.subject_size_checked"
            ));
        }

        if subject_verified {
            report.verified_subject_count += 1;
        }
        report.audit.push(format!(
            "evidence.integrity.subject.{index}.digest_recomputed:{actual_digest}"
        ));
    }

    if source_commits.len() > 1 {
        report.record_blocker(EvidenceIntegrityBlocker::BundleMixedSourceCommit);
    }
    Ok(report)
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn validate_platform_text(field: &str, value: String) -> Result<String, EvaError> {
    validate_manifest_text(field, value)
}

fn validate_release_tag(value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text("platform release tag", value)?;
    if !is_valid_release_tag(&value) {
        return Err(EvaError::invalid_argument(
            "platform release tag must use canonical v<semver> form",
        )
        .with_context("release_tag", value));
    }
    Ok(value)
}

fn is_valid_release_tag(value: &str) -> bool {
    let Some(version) = value.strip_prefix('v') else {
        return false;
    };
    let mut parts = version.splitn(2, '-');
    let core = parts.next().unwrap_or_default();
    let numeric = core.split('.').collect::<Vec<_>>();
    if numeric.len() != 3
        || numeric
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return false;
    }
    parts.next().is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
    })
}

fn validate_canonical_positive_decimal(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text(field, value)?;
    if !is_positive_decimal(&value) {
        return Err(EvaError::invalid_argument(format!(
            "{field} must be a positive decimal integer"
        ))
        .with_context("value", value));
    }
    Ok(value)
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty() && value != "0" && value.chars().all(|ch| ch.is_ascii_digit())
}

fn validate_capture_id(value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text("release capture id", value)?;
    if !value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(EvaError::invalid_argument(
            "release capture id must use lowercase portable token characters",
        )
        .with_context("capture_id", value));
    }
    Ok(value)
}

fn validate_capture_outcome(value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text("release capture outcome", value)?;
    if !matches!(value.as_str(), "success" | "failure" | "timeout") {
        return Err(EvaError::invalid_argument(
            "release capture outcome must be success, failure, or timeout",
        )
        .with_context("outcome", value));
    }
    Ok(value)
}

fn validate_digest_value(field: &str, value: String) -> Result<String, EvaError> {
    validate_canonical_subject_digest(&value)
        .map(|_| value)
        .map_err(|error| error.with_context("field", field))
}

fn validate_toolchain(value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text("release platform toolchain", value)?;
    if value.len() > 512 {
        return Err(EvaError::invalid_argument(
            "release platform toolchain is too long",
        ));
    }
    Ok(value)
}

fn normalize_subject_os(value: String) -> Result<String, EvaError> {
    let value = validate_platform_text("platform os", value)?;
    normalize_platform_os(&value)
        .map(ToOwned::to_owned)
        .ok_or_else(|| EvaError::invalid_argument("release platform os is unsupported"))
}

fn normalize_subject_arch(value: String) -> Result<String, EvaError> {
    let value = validate_platform_text("platform arch", value)?;
    normalize_platform_arch(&value)
        .map(ToOwned::to_owned)
        .ok_or_else(|| EvaError::invalid_argument("release platform architecture is unsupported"))
}

fn validate_target_mapping(os: &str, arch: &str, target: &str) -> Result<(), EvaError> {
    let expected = match (os, arch) {
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    };
    if expected == Some(target) {
        Ok(())
    } else {
        Err(EvaError::invalid_argument(
            "release platform artifact target does not match os and arch",
        )
        .with_context("os", os)
        .with_context("arch", arch)
        .with_context("target", target))
    }
}

fn validate_artifact_name_mapping(tag: &str, target: &str, name: &str) -> Result<(), EvaError> {
    let version = tag.strip_prefix('v').unwrap_or_default();
    let suffix = if target == "x86_64-pc-windows-msvc" {
        "zip"
    } else {
        "tar.gz"
    };
    let expected = format!("eva-cli-{version}-{target}.{suffix}");
    if name == expected {
        Ok(())
    } else {
        Err(EvaError::invalid_argument(
            "release platform artifact name does not match tag and target",
        )
        .with_context("expected_name", expected)
        .with_context("actual_name", name))
    }
}

fn parse_u64_field(fields: &BTreeMap<String, String>, key: &str) -> Result<u64, EvaError> {
    required(fields, key)?.parse::<u64>().map_err(|error| {
        EvaError::invalid_argument("release evidence numeric field is invalid")
            .with_context("field", key)
            .with_context("parse_error", error.to_string())
    })
}

fn parse_capture_fields(
    fields: &BTreeMap<String, String>,
    prefix: &str,
) -> Result<ReleaseCaptureEvidence, EvaError> {
    ReleaseCaptureEvidence::new(
        required(fields, &format!("{prefix}.id"))?,
        required(fields, &format!("{prefix}.outcome"))?,
        required(fields, &format!("{prefix}.manifest_digest"))?,
        parse_u64_field(fields, &format!("{prefix}.manifest_size"))?,
        required(fields, &format!("{prefix}.stdout_digest"))?,
        parse_u64_field(fields, &format!("{prefix}.stdout_size"))?,
        required(fields, &format!("{prefix}.stderr_digest"))?,
        parse_u64_field(fields, &format!("{prefix}.stderr_size"))?,
    )
}

fn is_platform_capture_field(field: &str, prefix: &str) -> bool {
    let Some(suffix) = field
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('.'))
    else {
        return false;
    };
    matches!(
        suffix,
        "id" | "outcome"
            | "manifest_digest"
            | "manifest_size"
            | "stdout_digest"
            | "stdout_size"
            | "stderr_digest"
            | "stderr_size"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureRole {
    Smoke,
    Toolchain,
}

/// Verify the structured fields in a command capture manifest after its raw
/// bytes have already been hashed.  A small local JSON reader keeps this crate
/// dependency-free while still rejecting identity/stream metadata tampering.
#[allow(clippy::too_many_arguments)]
fn verify_capture_manifest(
    claim: &ReleaseCaptureEvidence,
    manifest_bytes: &[u8],
    stdout_bytes: &[u8],
    stderr_bytes: &[u8],
    role: CaptureRole,
    expected_run_id: &str,
    expected_run_attempt: &str,
    expected_os: &str,
    expected_arch: &str,
    expected_job: &str,
    expected_executor: &str,
) -> Vec<EvidenceIntegrityBlocker> {
    let mut blockers = Vec::new();
    let root = match JsonParser::new(manifest_bytes).parse() {
        Ok(JsonValue::Object(value)) => value,
        _ => {
            blockers.push(EvidenceIntegrityBlocker::CaptureManifestInvalid);
            return blockers;
        }
    };
    let format = json_string(&root, &["format"]);
    let capture_id = json_string(&root, &["capture_id"]);
    let outcome = json_string(&root, &["outcome"]);
    if format.as_deref() != Some(RELEASE_COMMAND_CAPTURE_FORMAT)
        || capture_id.as_deref() != Some(claim.id.as_str())
    {
        push_unique(
            &mut blockers,
            EvidenceIntegrityBlocker::CaptureManifestInvalid,
        );
    }
    if outcome.as_deref() != Some(claim.outcome.as_str()) {
        push_unique(
            &mut blockers,
            EvidenceIntegrityBlocker::CaptureManifestInvalid,
        );
    }
    if claim.outcome != "success" {
        push_unique(
            &mut blockers,
            EvidenceIntegrityBlocker::CaptureOutcomeInvalid,
        );
    }
    let executable = json_string(&root, &["executable"]);
    let executable_leaf = executable.as_deref().map(executable_leaf_name);
    let executable_valid = match role {
        CaptureRole::Smoke => {
            matches!(executable_leaf.as_deref(), Some("eva" | "eva.exe"))
        }
        CaptureRole::Toolchain => {
            matches!(executable_leaf.as_deref(), Some("rustc" | "rustc.exe"))
        }
    };
    let argv_valid = matches!(root.get("argv"), Some(JsonValue::Array(values)) if matches!(values.as_slice(), [JsonValue::String(argument)] if argument == "--version"));
    let exit_code_valid =
        matches!(root.get("exit_code"), Some(JsonValue::Number(value)) if value == "0");
    if !executable_valid || !argv_valid {
        push_unique(
            &mut blockers,
            EvidenceIntegrityBlocker::CaptureIdentityMismatch,
        );
    }
    if !exit_code_valid {
        push_unique(
            &mut blockers,
            EvidenceIntegrityBlocker::CaptureOutcomeInvalid,
        );
    }
    if !capture_runner_matches(
        &root,
        expected_run_id,
        expected_run_attempt,
        expected_os,
        expected_arch,
        expected_job,
        expected_executor,
    ) {
        push_unique(
            &mut blockers,
            EvidenceIntegrityBlocker::CaptureIdentityMismatch,
        );
    }
    for (name, bytes, expected_digest, expected_size, digest_blocker) in [
        (
            "stdout",
            stdout_bytes,
            claim.stdout_digest.as_str(),
            claim.stdout_size,
            EvidenceIntegrityBlocker::CaptureStdoutDigestMismatch,
        ),
        (
            "stderr",
            stderr_bytes,
            claim.stderr_digest.as_str(),
            claim.stderr_size,
            EvidenceIntegrityBlocker::CaptureStderrDigestMismatch,
        ),
    ] {
        let Some(JsonValue::Object(stream)) = root.get(name) else {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureManifestInvalid,
            );
            continue;
        };
        let digest = stream.get("sha256").and_then(JsonValue::as_string);
        let size = stream.get("byte_count").and_then(JsonValue::as_u64);
        if digest != Some(expected_digest) {
            push_unique(&mut blockers, digest_blocker);
        }
        if size != Some(expected_size) || size != Some(bytes.len() as u64) {
            push_unique(
                &mut blockers,
                EvidenceIntegrityBlocker::CaptureStreamSizeMismatch,
            );
        }
    }
    blockers
}

fn executable_leaf_name(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn capture_runner_matches(
    root: &BTreeMap<String, JsonValue>,
    expected_run_id: &str,
    expected_run_attempt: &str,
    expected_os: &str,
    expected_arch: &str,
    expected_job: &str,
    expected_executor: &str,
) -> bool {
    let Some(JsonValue::Object(runner)) = root.get("runner") else {
        return false;
    };
    let field = |name: &str| runner.get(name).and_then(JsonValue::as_string);
    let Some(provider) = field("provider") else {
        return false;
    };
    let Some(identity) = field("identity") else {
        return false;
    };
    let Some(name) = field("name") else {
        return false;
    };
    let Some(os) = field("os").and_then(normalize_platform_os) else {
        return false;
    };
    let Some(arch) = field("architecture").and_then(normalize_platform_arch) else {
        return false;
    };
    let expected_identity =
        format!("{name}/{expected_run_id}/{expected_run_attempt}/{expected_job}");
    provider == "github-actions"
        && is_valid_manifest_text(identity)
        && is_valid_manifest_text(name)
        && identity == expected_identity
        && identity == expected_executor
        && field("run_id") == Some(expected_run_id)
        && field("run_attempt") == Some(expected_run_attempt)
        && field("job") == Some(expected_job)
        && os == expected_os
        && arch == expected_arch
}

fn normalize_platform_os(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "windows" => Some("windows"),
        "linux" => Some("linux"),
        "macos" | "mac" | "darwin" => Some("macos"),
        _ => None,
    }
}

fn normalize_platform_arch(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "x64" | "amd64" | "x86_64" => Some("x86_64"),
        "arm64" | "aarch64" => Some("aarch64"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum JsonValue {
    Object(BTreeMap<String, JsonValue>),
    Array(Vec<JsonValue>),
    String(String),
    Number(String),
    Bool(bool),
    Null,
}

impl JsonValue {
    fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => value.parse().ok(),
            _ => None,
        }
    }
}

fn json_string(root: &BTreeMap<String, JsonValue>, path: &[&str]) -> Option<String> {
    let mut current = root.get(path.first().copied()?)?;
    for key in &path[1..] {
        current = match current {
            JsonValue::Object(object) => object.get(*key)?,
            _ => return None,
        };
    }
    current.as_string().map(ToOwned::to_owned)
}

struct JsonParser<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn parse(mut self) -> Result<JsonValue, ()> {
        let value = self.value()?;
        self.ws();
        if self.offset == self.input.len() {
            Ok(value)
        } else {
            Err(())
        }
    }

    fn value(&mut self) -> Result<JsonValue, ()> {
        self.ws();
        match self.input.get(self.offset).copied() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(JsonValue::String(self.string()?)),
            Some(b'-' | b'0'..=b'9') => Ok(JsonValue::Number(self.number()?)),
            Some(b't') if self.literal(b"true") => Ok(JsonValue::Bool(true)),
            Some(b'f') if self.literal(b"false") => Ok(JsonValue::Bool(false)),
            Some(b'n') if self.literal(b"null") => Ok(JsonValue::Null),
            _ => Err(()),
        }
    }

    fn object(&mut self) -> Result<JsonValue, ()> {
        self.offset += 1;
        let mut object = BTreeMap::new();
        self.ws();
        if self.take(b'}') {
            return Ok(JsonValue::Object(object));
        }
        loop {
            self.ws();
            if !self.take(b'"') {
                return Err(());
            }
            self.offset -= 1;
            let key = self.string()?;
            self.ws();
            if !self.take(b':') {
                return Err(());
            }
            let value = self.value()?;
            if object.insert(key, value).is_some() {
                return Err(());
            }
            self.ws();
            if self.take(b'}') {
                return Ok(JsonValue::Object(object));
            }
            if !self.take(b',') {
                return Err(());
            }
        }
    }

    fn array(&mut self) -> Result<JsonValue, ()> {
        self.offset += 1;
        let mut values = Vec::new();
        self.ws();
        if self.take(b']') {
            return Ok(JsonValue::Array(values));
        }
        loop {
            values.push(self.value()?);
            self.ws();
            if self.take(b']') {
                return Ok(JsonValue::Array(values));
            }
            if !self.take(b',') {
                return Err(());
            }
        }
    }

    fn string(&mut self) -> Result<String, ()> {
        if !self.take(b'"') {
            return Err(());
        }
        let mut output = String::new();
        while let Some(byte) = self.input.get(self.offset).copied() {
            self.offset += 1;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let escaped = self.input.get(self.offset).copied().ok_or(())?;
                    self.offset += 1;
                    match escaped {
                        b'"' => output.push('"'),
                        b'\\' => output.push('\\'),
                        b'/' => output.push('/'),
                        b'b' => output.push('\u{0008}'),
                        b'f' => output.push('\u{000c}'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => {
                            let end = self.offset.checked_add(4).ok_or(())?;
                            let hex = self.input.get(self.offset..end).ok_or(())?;
                            let value = std::str::from_utf8(hex)
                                .ok()
                                .and_then(|value| u16::from_str_radix(value, 16).ok())
                                .and_then(|value| char::from_u32(u32::from(value)))
                                .ok_or(())?;
                            self.offset = end;
                            output.push(value);
                        }
                        _ => return Err(()),
                    }
                }
                byte if byte < 0x20 => return Err(()),
                byte if byte.is_ascii() => output.push(char::from(byte)),
                byte => {
                    let width = match byte {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(()),
                    };
                    let start = self.offset - 1;
                    let end = start.checked_add(width).ok_or(())?;
                    let encoded = self.input.get(start..end).ok_or(())?;
                    let decoded = std::str::from_utf8(encoded).map_err(|_| ())?;
                    let mut chars = decoded.chars();
                    output.push(chars.next().ok_or(())?);
                    if chars.next().is_some() {
                        return Err(());
                    }
                    self.offset = end;
                }
            }
        }
        Err(())
    }

    fn number(&mut self) -> Result<String, ()> {
        let start = self.offset;
        while let Some(byte) = self.input.get(self.offset).copied() {
            if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.offset += 1;
            } else {
                break;
            }
        }
        let value = std::str::from_utf8(&self.input[start..self.offset]).map_err(|_| ())?;
        if value.is_empty() {
            Err(())
        } else {
            Ok(value.to_owned())
        }
    }

    fn literal(&mut self, literal: &[u8]) -> bool {
        if self.input.get(self.offset..self.offset + literal.len()) == Some(literal) {
            self.offset += literal.len();
            true
        } else {
            false
        }
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.input.get(self.offset).copied() == Some(expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn ws(&mut self) {
        while self
            .input
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.offset += 1;
        }
    }
}

fn normalize_toolchain_output(bytes: &[u8]) -> String {
    normalize_capture_output(bytes).trim().to_owned()
}

fn normalize_capture_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn capture_finished_at_millis(manifest_bytes: &[u8]) -> Option<u128> {
    let JsonValue::Object(root) = JsonParser::new(manifest_bytes).parse().ok()? else {
        return None;
    };
    parse_utc_timestamp_millis(&json_string(&root, &["finished_at"])?).map(u128::from)
}

fn parse_utc_timestamp_millis(value: &str) -> Option<u64> {
    let value = value
        .strip_suffix('Z')
        .or_else(|| value.strip_suffix("+00:00"))?;
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i32>().ok()?;
    let month = date_parts.next()?.parse::<u32>().ok()?;
    let day = date_parts.next()?.parse::<u32>().ok()?;
    if date_parts.next().is_some() || year < 1970 || !valid_calendar_date(year, month, day) {
        return None;
    }
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<u32>().ok()?;
    let minute = time_parts.next()?.parse::<u32>().ok()?;
    let second_fraction = time_parts.next()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 {
        return None;
    }
    let (second, fraction) = second_fraction
        .split_once('.')
        .map_or((second_fraction, ""), |parts| parts);
    let second = second.parse::<u32>().ok()?;
    if second > 59 || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let millis_text = fraction.get(..fraction.len().min(3)).unwrap_or_default();
    let mut millis = millis_text.parse::<u32>().unwrap_or(0);
    for _ in millis_text.len()..3 {
        millis *= 10;
    }
    let days = days_since_unix_epoch(year, month, day)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(u64::from(hour) * 3_600)?
        .checked_add(u64::from(minute) * 60)?
        .checked_add(u64::from(second))?;
    seconds.checked_mul(1_000)?.checked_add(u64::from(millis))
}

fn valid_calendar_date(year: i32, month: u32, day: u32) -> bool {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn days_since_unix_epoch(year: i32, month: u32, day: u32) -> Option<u64> {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    u64::try_from(era * 146_097 + day_of_era - 719_468).ok()
}

fn platform_identity_key(entry: &ReleasePlatformEvidence) -> (String, String, String) {
    (
        entry.artifact_target.clone(),
        entry.os.clone(),
        entry.arch.clone(),
    )
}

fn platform_sort_key(entry: &ReleasePlatformEvidence) -> (String, String, String, String, String) {
    (
        entry.artifact_target.clone(),
        entry.os.clone(),
        entry.arch.clone(),
        entry.artifact_name.clone(),
        entry.job.clone(),
    )
}

fn append_length_delimited(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(value: &str) -> Result<Vec<u8>, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::invalid_argument(
            "release platform bundle hex value must have even length",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let chars = value.as_bytes();
    for index in (0..chars.len()).step_by(2) {
        let high = hex_nibble(chars[index]).ok_or_else(|| {
            EvaError::invalid_argument("release platform bundle hex value is invalid")
        })?;
        let low = hex_nibble(chars[index + 1]).ok_or_else(|| {
            EvaError::invalid_argument("release platform bundle hex value is invalid")
        })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

/// 计算主题原始字节的规范 SHA-256；不会持久化临时 ArtifactRecord。
fn digest_subject(subject_bytes: &[u8]) -> String {
    ArtifactRecord::new(EVIDENCE_SUBJECT_KEY, subject_bytes.to_vec()).digest
}

/// 返回来源提交声明对应的稳定 blocker；合法且匹配时返回 None。
fn source_commit_blocker(
    expected_source_commit: &str,
    source_commit: &str,
) -> Option<EvidenceIntegrityBlocker> {
    if !is_canonical_source_commit(source_commit) {
        Some(EvidenceIntegrityBlocker::SourceCommitInvalid)
    } else if source_commit != expected_source_commit {
        Some(EvidenceIntegrityBlocker::SourceCommitMismatch)
    } else {
        None
    }
}

/// 返回摘要声明对应的稳定 blocker；格式合法且与重算值一致时返回 None。
fn subject_digest_blocker(
    subject_digest: &str,
    actual_digest: &str,
) -> Option<EvidenceIntegrityBlocker> {
    if !is_canonical_subject_digest(subject_digest) {
        Some(EvidenceIntegrityBlocker::SubjectDigestInvalid)
    } else if subject_digest != actual_digest {
        Some(EvidenceIntegrityBlocker::SubjectDigestMismatch)
    } else {
        None
    }
}

/// 校验可信或生产者提交使用规范小写完整 SHA。
fn validate_canonical_source_commit(value: &str) -> Result<(), EvaError> {
    if !is_canonical_source_commit(value) {
        return Err(EvaError::invalid_argument(
            "release evidence source commit must be a canonical lowercase 40-character hex sha",
        )
        .with_context("source_commit", value));
    }
    Ok(())
}

/// 校验生产者摘要使用规范小写 SHA-256 格式。
fn validate_canonical_subject_digest(value: &str) -> Result<(), EvaError> {
    if !is_canonical_subject_digest(value) {
        return Err(EvaError::invalid_argument(
            "release evidence subject digest must be canonical lowercase sha256 hex",
        )
        .with_context("subject_digest", value));
    }
    Ok(())
}

/// 判断提交是否为规范小写 40 字符十六进制 SHA。
fn is_canonical_source_commit(value: &str) -> bool {
    value.len() == 40 && value.chars().all(is_lower_hex)
}

/// 判断摘要是否为规范小写 `sha256:<64 hex>`。
fn is_canonical_subject_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(is_lower_hex))
}

/// 判断字符是否属于规范小写十六进制字母表。
fn is_lower_hex(ch: char) -> bool {
    ch.is_ascii_digit() || matches!(ch, 'a'..='f')
}

/// 解析允许 BOM、空行和注释的严格键值清单，并拒绝重复键。
fn parse_key_value_manifest(data: &str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "release evidence envelope line must use key=value format",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(EvaError::invalid_argument(
                "release evidence envelope key cannot be empty",
            ));
        }
        if fields
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(EvaError::invalid_argument(
                "release evidence envelope field is duplicated",
            )
            .with_context("field", key));
        }
    }
    Ok(fields)
}

/// 读取必填信封字段。
fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release evidence envelope is missing required field")
            .with_context("required_field", key)
    })
}

/// 读取统一 evidence manifest 的必填字段并保留 manifest 语义错误。
fn required_manifest_field(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release evidence manifest is missing required field")
            .with_context("required_field", key)
    })
}

/// 拒绝当前格式未声明的字段，防止解析后静默丢失调用方认为已绑定的证据。
fn reject_unknown_fields(fields: &BTreeMap<String, String>) -> Result<(), EvaError> {
    for field in fields.keys() {
        if !EVIDENCE_ENVELOPE_FIELDS.contains(&field.as_str()) {
            return Err(EvaError::invalid_argument(
                "release evidence envelope contains an unknown field",
            )
            .with_context("field", field));
        }
    }
    Ok(())
}

/// 严格解析 Unix epoch 毫秒时间戳，零值由构造器拒绝。
fn parse_timestamp(value: String) -> Result<u128, EvaError> {
    value.parse::<u128>().map_err(|error| {
        EvaError::invalid_argument(
            "release evidence timestamp must be an epoch millisecond integer",
        )
        .with_context("timestamp", value)
        .with_context("parse_error", error.to_string())
    })
}

/// 校验可写入单行清单的非空、已去首尾空白文本。
fn validate_manifest_text(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    if value.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
        return Err(
            EvaError::invalid_argument(format!("{field} must fit on one manifest line"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// Require a canonical positive decimal so string comparisons have one representation.
fn validate_positive_decimal(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text(field, value)?;
    let parsed = value.parse::<u128>().map_err(|error| {
        EvaError::invalid_argument(format!("{field} must be a positive decimal integer"))
            .with_context("parse_error", error.to_string())
    })?;
    if parsed == 0 || parsed.to_string() != value {
        return Err(EvaError::invalid_argument(format!(
            "{field} must be a canonical positive decimal integer"
        )));
    }
    Ok(value)
}

/// 限制统一 manifest 引用为跨平台稳定的目录内相对路径。
fn validate_manifest_relative_path(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_manifest_text(field, value)?;
    if value.contains('\\')
        || value.contains(':')
        || value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(EvaError::invalid_argument(format!(
            "{field} must use a portable relative path"
        ))
        .with_context("path", value));
    }
    let path = Path::new(&value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EvaError::invalid_argument(format!(
            "{field} must stay within the evidence manifest directory"
        ))
        .with_context("path", value));
    }
    Ok(value)
}

/// 判断公开可变的信封文本是否仍满足单行、非空、已 trim 约束。
fn is_valid_manifest_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.trim() == value
        && !value.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0'))
}

#[cfg(test)]
/// 统一信封的分类、必填字段和稳定清单往返测试。
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_COMMIT: &str = "abcdef0123456789abcdef0123456789abcdef01";
    const DIGEST: &str = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";

    /// 为指定种类构造一项完整的固定测试信封。
    fn envelope(kind: EvidenceKind) -> EvidenceEnvelope {
        EvidenceEnvelope::new(
            kind,
            "cargo-test-eva-release",
            COMMIT,
            "windows-x86_64-rust-stable",
            "github-actions:run-123",
            1_784_073_600_000,
            DIGEST,
        )
        .unwrap()
    }

    /// 构造同时覆盖 artifact 特殊 subject 与 canonical 文档主题的统一清单。
    fn release_manifest(scope: ReleaseEvidenceScope) -> ReleaseEvidenceManifest {
        ReleaseEvidenceManifest::new(
            scope,
            COMMIT,
            vec![
                ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::Artifact,
                    "artifact/release.evidence",
                    "artifact/release.envelope",
                    Some("artifact/eva.tar.gz".to_owned()),
                )
                .unwrap(),
                ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::Benchmark,
                    "benchmark/release.evidence",
                    "benchmark/release.envelope",
                    None,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    /// 验证四类证据都使用相同字段完整往返，且 kind 保持稳定。
    fn all_evidence_kinds_round_trip() {
        for kind in EvidenceKind::ALL {
            let expected = envelope(kind);
            let parsed = EvidenceEnvelope::parse_manifest(&expected.to_manifest()).unwrap();

            assert_eq!(parsed, expected);
            assert_eq!(parsed.kind.as_str(), kind.as_str());
        }
    }

    #[test]
    /// 验证 alpha/production scope、索引顺序及 artifact subject 完整往返。
    fn release_evidence_manifest_round_trips_both_scopes() {
        for scope in [
            ReleaseEvidenceScope::Alpha,
            ReleaseEvidenceScope::Production,
        ] {
            let expected = release_manifest(scope);
            let canonical = expected.to_manifest();
            let parsed = ReleaseEvidenceManifest::parse_manifest(&canonical).unwrap();

            assert_eq!(parsed, expected);
            assert_eq!(parsed.to_manifest(), canonical);
            assert_eq!(
                parsed.entries[0].subject_path.as_deref(),
                Some("artifact/eva.tar.gz")
            );
            assert_eq!(parsed.entries[1].subject_path, None);
        }

        let mut reversed = release_manifest(ReleaseEvidenceScope::Alpha).entries;
        reversed.reverse();
        let normalized =
            ReleaseEvidenceManifest::new(ReleaseEvidenceScope::Alpha, COMMIT, reversed).unwrap();
        assert_eq!(
            normalized.entries[0].evidence_type,
            ReleaseEvidenceType::Artifact
        );
        assert_eq!(
            normalized.entries[1].evidence_type,
            ReleaseEvidenceType::Benchmark
        );

        let mut bound = release_manifest(ReleaseEvidenceScope::Production);
        bound.entries[0].envelope_digest = Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        );
        let parsed = ReleaseEvidenceManifest::parse_manifest(&bound.to_manifest()).unwrap();
        assert_eq!(parsed, bound);
        assert_eq!(parsed.canonical_digest(), bound.canonical_digest());
    }

    #[test]
    /// 验证 consumer policy 要求逐类型规则并按命名空间边界匹配执行器。
    fn production_policy_requires_complete_executor_rules_and_namespace_members() {
        let policy = ProductionEvidencePolicy::github_actions(
            1_784_073_600_000,
            "123",
            "1",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert!(policy.trusts_executor(
            ReleaseEvidenceType::Benchmark,
            "github-actions:release-benchmark/123/1/benchmark"
        ));
        assert!(!policy.trusts_executor(
            ReleaseEvidenceType::Benchmark,
            "github-actions:release-benchmark/"
        ));
        assert!(!policy.trusts_executor(
            ReleaseEvidenceType::Artifact,
            "github-actions:release-benchmark/123/1/benchmark"
        ));
        assert!(!policy.trusts_executor(
            ReleaseEvidenceType::Benchmark,
            "github-actions:release-benchmark/124/1/benchmark"
        ));
        assert!(!policy.trusts_executor(
            ReleaseEvidenceType::Benchmark,
            "github-actions:release-benchmark/123/2/benchmark"
        ));
        assert!(!policy.trusts_executor(
            ReleaseEvidenceType::Benchmark,
            "github-actions:release-benchmark/123/1/benchmark/extra"
        ));

        let context = ProductionEvidenceContext::new(
            1_784_073_600_000,
            env!("CARGO_PKG_VERSION"),
            format!("v{}", env!("CARGO_PKG_VERSION")),
            "123",
            "1",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let incomplete = ProductionEvidencePolicy::new(
            context,
            PRODUCTION_EVIDENCE_MAX_AGE_MS,
            PRODUCTION_EVIDENCE_MAX_FUTURE_SKEW_MS,
            vec![ProductionEvidenceExecutorRule::prefix(
                ReleaseEvidenceType::Artifact,
                "trusted:artifact/",
            )
            .unwrap()],
        )
        .unwrap_err();
        assert!(incomplete
            .message()
            .contains("must define executors for every evidence type"));
        assert!(incomplete.context().entries().iter().any(|(key, value)| {
            key == "missing_entry_types" && value == "distribution,security_scan,benchmark"
        }));

        let invalid_prefix =
            ProductionEvidenceExecutorRule::prefix(ReleaseEvidenceType::Artifact, "trusted")
                .unwrap_err();
        assert!(invalid_prefix.message().contains("must end with"));

        let invalid_run = ProductionEvidencePolicy::github_actions(
            1_784_073_600_000,
            "run-123",
            "1",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap_err();
        assert!(invalid_run.message().contains("positive decimal"));
        assert!(invalid_run.context().entries().iter().any(|(key, value)| {
            key == "blocked_reasons"
                && value == ProductionEvidenceBlocker::TrustedRunRequired.as_str()
        }));
    }

    #[test]
    /// 验证未知字段、索引缺口和重复类型都不能被静默忽略。
    fn release_evidence_manifest_rejects_ambiguous_structure() {
        let canonical = release_manifest(ReleaseEvidenceScope::Production).to_manifest();

        let unknown = format!("{canonical}entry.0.unbound=value\n");
        assert!(ReleaseEvidenceManifest::parse_manifest(&unknown)
            .unwrap_err()
            .message()
            .contains("unknown field"));

        let gap = canonical.replace("entry.1.", "entry.2.");
        assert!(ReleaseEvidenceManifest::parse_manifest(&gap)
            .unwrap_err()
            .message()
            .contains("contiguous"));

        let duplicate_type = format!(
            "{}entry.1.subject=artifact/duplicate.tar.gz\n",
            canonical.replace("entry.1.type=benchmark", "entry.1.type=artifact")
        );
        assert!(ReleaseEvidenceManifest::parse_manifest(&duplicate_type)
            .unwrap_err()
            .message()
            .contains("duplicated"));
    }

    #[test]
    /// 验证 artifact 必须引用真实 subject，其他类型不得伪装成独立 subject。
    fn release_evidence_manifest_enforces_subject_semantics() {
        let missing_artifact_subject = ReleaseEvidenceManifestEntry::new(
            ReleaseEvidenceType::Artifact,
            "artifact.evidence",
            "artifact.envelope",
            None,
        )
        .unwrap_err();
        assert!(missing_artifact_subject
            .message()
            .contains("requires a subject"));

        let benchmark_subject = ReleaseEvidenceManifestEntry::new(
            ReleaseEvidenceType::Benchmark,
            "benchmark.evidence",
            "benchmark.envelope",
            Some("stdout.log".to_owned()),
        )
        .unwrap_err();
        assert!(benchmark_subject.message().contains("only artifact"));
    }

    #[test]
    /// 验证绝对路径、父目录、Windows 分隔符和 drive-relative 路径都被拒绝。
    fn release_evidence_manifest_rejects_path_escape_forms() {
        for path in [
            "../outside.evidence",
            "nested/../outside.evidence",
            "nested/./outside.evidence",
            "nested//outside.evidence",
            "nested\\outside.evidence",
            "C:outside.evidence",
            "/outside.evidence",
        ] {
            let error = ReleaseEvidenceManifestEntry::new(
                ReleaseEvidenceType::Benchmark,
                path,
                "benchmark.envelope",
                None,
            )
            .unwrap_err();
            assert!(
                error.message().contains("relative path")
                    || error.message().contains("manifest directory"),
                "path={path} error={error:?}"
            );
        }
    }

    #[test]
    /// 验证格式及七个身份字段任一缺失时都失败关闭。
    fn missing_required_fields_are_rejected() {
        let manifest = envelope(EvidenceKind::Measurement).to_manifest();
        for missing in [
            "format",
            "kind",
            "source",
            "source_commit",
            "environment",
            "executor",
            "timestamp",
            "subject_digest",
        ] {
            let incomplete = manifest
                .lines()
                .filter(|line| line.split_once('=').map(|(key, _)| key) != Some(missing))
                .collect::<Vec<_>>()
                .join("\n");

            let error = EvidenceEnvelope::parse_manifest(&incomplete).unwrap_err();

            assert_eq!(
                error.context().entries(),
                &[("required_field".to_owned(), missing.to_owned())]
            );
        }
    }

    #[test]
    /// 验证未知 kind 不能降级为任一较弱分类。
    fn unknown_evidence_kind_is_rejected() {
        let manifest = envelope(EvidenceKind::Measurement)
            .to_manifest()
            .replace("kind=measurement", "kind=synthetic");

        let error = EvidenceEnvelope::parse_manifest(&manifest).unwrap_err();

        assert_eq!(error.message(), "unsupported release evidence kind");
        assert_eq!(
            error.context().entries(),
            &[("kind".to_owned(), "synthetic".to_owned())]
        );
    }

    #[test]
    /// 验证空白 executor 在构造和清单解析边界均被拒绝。
    fn empty_executor_is_rejected() {
        let error = EvidenceEnvelope::new(
            EvidenceKind::Operator,
            "operator-approval",
            COMMIT,
            "controlled-release-host",
            "  ",
            1_784_073_600_000,
            DIGEST,
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "release evidence executor must be non-empty and trimmed"
        );

        let manifest = envelope(EvidenceKind::Operator)
            .to_manifest()
            .replace("executor=github-actions:run-123", "executor=");
        assert!(EvidenceEnvelope::parse_manifest(&manifest).is_err());
    }

    #[test]
    /// 验证当前格式未声明的字段不会被静默忽略。
    fn unknown_manifest_field_is_rejected() {
        let manifest = format!(
            "{}unexpected=not-bound-to-subject\n",
            envelope(EvidenceKind::Fixture).to_manifest()
        );

        let error = EvidenceEnvelope::parse_manifest(&manifest).unwrap_err();

        assert_eq!(
            error.message(),
            "release evidence envelope contains an unknown field"
        );
        assert_eq!(
            error.context().entries(),
            &[("field".to_owned(), "unexpected".to_owned())]
        );
    }

    #[test]
    /// 验证生产者从原始字节计算规范摘要，消费方独立重算后通过。
    fn subject_bytes_are_hashed_and_verified() {
        let envelope = EvidenceEnvelope::from_subject_bytes(
            EvidenceKind::Measurement,
            "cargo-test-output",
            COMMIT,
            "windows-x86_64-rust-stable",
            "github-actions:run-123",
            1_784_073_600_000,
            b"ok",
        )
        .unwrap();

        let report = envelope.verify_subject(COMMIT, b"ok").unwrap();

        assert_eq!(envelope.subject_digest, DIGEST);
        assert!(report.is_verified());
        assert_eq!(report.subject_count, 1);
        assert_eq!(report.verified_subject_count, 1);
        assert!(report.blocked_reasons.is_empty());
    }

    #[test]
    /// 验证主题字节或合法摘要声明任一被替换都会得到同一稳定 mismatch code。
    fn tampered_subject_bytes_or_digest_are_blocked() {
        let envelope = EvidenceEnvelope::from_subject_bytes(
            EvidenceKind::Measurement,
            "cargo-test-output",
            COMMIT,
            "linux-x86_64-rust-stable",
            "github-actions:run-123",
            1_784_073_600_000,
            b"ok",
        )
        .unwrap();

        let bytes_report = envelope.verify_subject(COMMIT, b"tampered").unwrap();
        let mut digest_tampered = envelope.clone();
        digest_tampered.subject_digest = digest_subject(b"other");
        let digest_report = digest_tampered.verify_subject(COMMIT, b"ok").unwrap();

        assert_eq!(
            bytes_report.blocked_reasons,
            vec![EvidenceIntegrityBlocker::SubjectDigestMismatch]
        );
        assert_eq!(
            digest_report.blocked_reasons,
            vec![EvidenceIntegrityBlocker::SubjectDigestMismatch]
        );
    }

    #[test]
    /// 验证 bundle 不能用自身 commit 代替可信构建提交。
    fn wrong_build_commit_is_blocked() {
        let envelope = EvidenceEnvelope::from_subject_bytes(
            EvidenceKind::Fixture,
            "fixture-output",
            COMMIT,
            "controlled-fixture",
            "cargo-test",
            1_784_073_600_000,
            b"fixture",
        )
        .unwrap();

        let report = envelope.verify_subject(OTHER_COMMIT, b"fixture").unwrap();

        assert_eq!(
            report.blocked_reasons,
            vec![EvidenceIntegrityBlocker::SourceCommitMismatch]
        );
    }

    #[test]
    /// 验证每项摘要正确时，跨提交拼接 bundle 仍明确阻断。
    fn mixed_commit_bundle_is_blocked() {
        let first = EvidenceEnvelope::from_subject_bytes(
            EvidenceKind::Measurement,
            "first-command",
            COMMIT,
            "linux-x86_64",
            "runner:first",
            1_784_073_600_000,
            b"first",
        )
        .unwrap();
        let second = EvidenceEnvelope::from_subject_bytes(
            EvidenceKind::Measurement,
            "second-command",
            OTHER_COMMIT,
            "windows-x86_64",
            "runner:second",
            1_784_073_600_001,
            b"second",
        )
        .unwrap();

        let report = verify_evidence_bundle(
            COMMIT,
            &[
                EvidenceSubject::new(&first, b"first"),
                EvidenceSubject::new(&second, b"second"),
            ],
        )
        .unwrap();

        assert_eq!(
            report.blocked_reasons,
            vec![
                EvidenceIntegrityBlocker::SourceCommitMismatch,
                EvidenceIntegrityBlocker::BundleMixedSourceCommit,
            ]
        );
    }

    #[test]
    /// 验证 pub 字段被绕过构造器篡改后，消费 verifier 仍失败关闭。
    fn malformed_public_claims_are_blocked() {
        let mut envelope = envelope(EvidenceKind::Operator);
        envelope.format = "eva.release.evidence_envelope.v2".to_owned();
        envelope.source_commit = COMMIT.to_ascii_uppercase();
        envelope.subject_digest = "sha256:not-a-digest".to_owned();

        let report = envelope.verify_subject(COMMIT, b"ok").unwrap();

        assert_eq!(
            report.blocked_reasons,
            vec![
                EvidenceIntegrityBlocker::EnvelopeFormatInvalid,
                EvidenceIntegrityBlocker::SourceCommitInvalid,
                EvidenceIntegrityBlocker::SubjectDigestInvalid,
            ]
        );
    }

    #[test]
    /// 验证公开身份字段被清空、注入换行或置零后不能保持 verified。
    fn malformed_public_identity_fields_are_blocked() {
        let base = envelope(EvidenceKind::Operator);
        let mut source = base.clone();
        source.source = "operator\nsource=forged".to_owned();
        let mut environment = base.clone();
        environment.environment.clear();
        let mut executor = base.clone();
        executor.executor.clear();
        let mut timestamp = base;
        timestamp.timestamp = 0;

        for malformed in [source, environment, executor, timestamp] {
            let report = malformed.verify_subject(COMMIT, b"ok").unwrap();

            assert_eq!(
                report.blocked_reasons,
                vec![EvidenceIntegrityBlocker::EnvelopeIdentityInvalid]
            );
        }
    }

    #[test]
    /// 验证 verifier 的可信 expected commit 自身非法时返回调用方输入错误。
    fn invalid_expected_build_commit_is_rejected() {
        let error = envelope(EvidenceKind::Declaration)
            .verify_subject("HEAD", b"ok")
            .unwrap_err();

        assert_eq!(
            error.message(),
            "release evidence source commit must be a canonical lowercase 40-character hex sha"
        );
    }

    fn command_capture(
        id: &str,
        executable: &str,
        os: &str,
        arch: &str,
        stdout: &[u8],
        stderr: &[u8],
    ) -> (ReleaseCaptureEvidence, Vec<u8>) {
        let job = format!("native-{os}");
        let identity = format!("test-runner/123/2/{job}");
        let manifest = format!(
            "{{\"format\":\"{}\",\"capture_id\":\"{}\",\"executable\":\"{}\",\"argv\":[\"--version\"],\"outcome\":\"success\",\"finished_at\":\"2026-07-15T00:00:00.0000000+00:00\",\"exit_code\":0,\"runner\":{{\"provider\":\"github-actions\",\"identity\":\"{}\",\"name\":\"test-runner\",\"os\":\"{}\",\"architecture\":\"{}\",\"run_id\":\"123\",\"run_attempt\":\"2\",\"job\":\"{}\"}},\"stdout\":{{\"path\":\"{}.stdout\",\"byte_count\":{},\"sha256\":\"{}\"}},\"stderr\":{{\"path\":\"{}.stderr\",\"byte_count\":{},\"sha256\":\"{}\"}}}}\n",
            RELEASE_COMMAND_CAPTURE_FORMAT,
            id,
            executable,
            identity,
            os,
            arch,
            job,
            id,
            stdout.len(),
            digest_subject(stdout),
            id,
            stderr.len(),
            digest_subject(stderr),
        )
        .into_bytes();
        let evidence =
            ReleaseCaptureEvidence::from_manifest_bytes(&manifest, stdout, stderr).unwrap();
        (evidence, manifest)
    }

    fn platform_evidence(
        os: &str,
        arch: &str,
        target: &str,
        artifact_name: &str,
        artifact: &[u8],
        smoke: &ReleaseCaptureEvidence,
        toolchain: &ReleaseCaptureEvidence,
    ) -> ReleasePlatformEvidence {
        let placeholder = EvidenceEnvelope::new(
            EvidenceKind::Measurement,
            format!("release-platform:{target}"),
            COMMIT,
            format!("{os}-{arch};rustc 1.80.0"),
            format!("test-runner/123/2/native-{os}"),
            1_784_073_600_000,
            DIGEST,
        )
        .unwrap();
        let provisional = ReleasePlatformEvidence {
            format: RELEASE_PLATFORM_EVIDENCE_FORMAT.to_owned(),
            tag: "v1.11.5-alpha".to_owned(),
            commit: COMMIT.to_owned(),
            os: os.to_owned(),
            arch: arch.to_owned(),
            toolchain: "rustc 1.80.0".to_owned(),
            run_id: "123".to_owned(),
            run_attempt: "2".to_owned(),
            job: format!("native-{os}"),
            artifact_name: artifact_name.to_owned(),
            artifact_target: target.to_owned(),
            artifact_digest: digest_subject(artifact),
            artifact_size: artifact.len() as u64,
            capture: smoke.clone(),
            toolchain_capture: toolchain.clone(),
            envelope: placeholder,
            envelope_digest: String::new(),
        };
        let mut envelope = provisional.envelope.clone();
        envelope.subject_digest = digest_subject(&provisional.to_subject_bytes());
        ReleasePlatformEvidence::new(
            provisional.tag,
            provisional.commit,
            provisional.os,
            provisional.arch,
            provisional.toolchain,
            provisional.run_id,
            provisional.run_attempt,
            provisional.job,
            provisional.artifact_name,
            provisional.artifact_target,
            provisional.artifact_digest,
            provisional.artifact_size,
            provisional.capture,
            provisional.toolchain_capture,
            envelope,
        )
        .unwrap()
    }

    #[test]
    fn same_commit_platforms_aggregate_in_stable_order() {
        let artifact = b"archive";
        let smoke_stdout = b"eva 1.11.5-alpha\n";
        let toolchain_stdout = b"rustc 1.80.0\n";
        let (windows_smoke, windows_smoke_manifest) = command_capture(
            "native.windows.smoke",
            "eva.exe",
            "windows",
            "x86_64",
            smoke_stdout,
            b"",
        );
        let (windows_toolchain, windows_toolchain_manifest) = command_capture(
            "native.windows.toolchain",
            "rustc.exe",
            "windows",
            "x86_64",
            toolchain_stdout,
            b"",
        );
        let (linux_smoke, linux_smoke_manifest) = command_capture(
            "native.linux.smoke",
            "eva",
            "linux",
            "x86_64",
            smoke_stdout,
            b"",
        );
        let (linux_toolchain, linux_toolchain_manifest) = command_capture(
            "native.linux.toolchain",
            "rustc",
            "linux",
            "x86_64",
            toolchain_stdout,
            b"",
        );
        let (macos_smoke, macos_smoke_manifest) = command_capture(
            "native.macos.smoke",
            "eva",
            "macos",
            "aarch64",
            smoke_stdout,
            b"",
        );
        let (macos_toolchain, macos_toolchain_manifest) = command_capture(
            "native.macos.toolchain",
            "rustc",
            "macos",
            "aarch64",
            toolchain_stdout,
            b"",
        );
        let windows = platform_evidence(
            "windows",
            "x86_64",
            "x86_64-pc-windows-msvc",
            "eva-cli-1.11.5-alpha-x86_64-pc-windows-msvc.zip",
            artifact,
            &windows_smoke,
            &windows_toolchain,
        );
        let linux = platform_evidence(
            "linux",
            "x86_64",
            "x86_64-unknown-linux-gnu",
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            artifact,
            &linux_smoke,
            &linux_toolchain,
        );
        let macos = platform_evidence(
            "macos",
            "aarch64",
            "aarch64-apple-darwin",
            "eva-cli-1.11.5-alpha-aarch64-apple-darwin.tar.gz",
            artifact,
            &macos_smoke,
            &macos_toolchain,
        );
        let first = ReleasePlatformEvidenceBundle::new(vec![windows, linux, macos]).unwrap();
        let mut reversed = first.entries.clone();
        reversed.reverse();
        let second = ReleasePlatformEvidenceBundle::new(reversed).unwrap();

        assert_eq!(first.entries, second.entries);
        assert_eq!(first.bundle_digest, second.bundle_digest);
        assert_eq!(first.format, RELEASE_PLATFORM_BUNDLE_FORMAT);
        assert_eq!(first.entries[0].artifact_target, "aarch64-apple-darwin");

        let macos_entry = first
            .entries
            .iter()
            .find(|entry| entry.os == "macos")
            .unwrap();
        let linux_entry = first
            .entries
            .iter()
            .find(|entry| entry.os == "linux")
            .unwrap();
        let windows_entry = first
            .entries
            .iter()
            .find(|entry| entry.os == "windows")
            .unwrap();
        let inputs = [
            ReleasePlatformEvidenceInput {
                evidence: macos_entry,
                capture_manifest: &macos_smoke_manifest,
                capture_stdout: smoke_stdout,
                capture_stderr: b"",
                toolchain_manifest: &macos_toolchain_manifest,
                toolchain_stdout,
                toolchain_stderr: b"",
                artifact,
            },
            ReleasePlatformEvidenceInput {
                evidence: linux_entry,
                capture_manifest: &linux_smoke_manifest,
                capture_stdout: smoke_stdout,
                capture_stderr: b"",
                toolchain_manifest: &linux_toolchain_manifest,
                toolchain_stdout,
                toolchain_stderr: b"",
                artifact,
            },
            ReleasePlatformEvidenceInput {
                evidence: windows_entry,
                capture_manifest: &windows_smoke_manifest,
                capture_stdout: smoke_stdout,
                capture_stderr: b"",
                toolchain_manifest: &windows_toolchain_manifest,
                toolchain_stdout,
                toolchain_stderr: b"",
                artifact,
            },
        ];
        let report = first.verify("v1.11.5-alpha", COMMIT, "123", "2", &inputs);
        assert_eq!(report.status, "verified");
        assert!(report.is_verified());

        let mut noncanonical = first.clone();
        noncanonical.entries.reverse();
        let error =
            ReleasePlatformEvidenceBundle::parse_manifest(&noncanonical.to_manifest()).unwrap_err();
        assert!(error.message().contains("canonical platform order"));
    }

    #[test]
    fn platform_verifier_rejects_trusted_identity_and_raw_byte_tampering() {
        let artifact = b"archive";
        let smoke_stdout = b"eva 1.11.5-alpha\n";
        let toolchain_stdout = b"rustc 1.80.0\n";
        let (smoke, smoke_manifest) =
            command_capture("native.smoke", "eva", "linux", "x86_64", smoke_stdout, b"");
        let (toolchain, toolchain_manifest) = command_capture(
            "native.toolchain",
            "rustc",
            "linux",
            "x86_64",
            toolchain_stdout,
            b"",
        );
        let evidence = platform_evidence(
            "linux",
            "x86_64",
            "x86_64-unknown-linux-gnu",
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            artifact,
            &smoke,
            &toolchain,
        );
        let verify = |expected_tag: &str,
                      expected_commit: &str,
                      expected_run_id: &str,
                      expected_attempt: &str,
                      capture_manifest: &[u8],
                      capture_stdout: &[u8],
                      toolchain_stdout: &[u8],
                      artifact: &[u8]| {
            evidence.verify(
                expected_tag,
                expected_commit,
                expected_run_id,
                expected_attempt,
                capture_manifest,
                capture_stdout,
                b"",
                &toolchain_manifest,
                toolchain_stdout,
                b"",
                artifact,
            )
        };

        assert!(verify(
            "v2.0.0",
            COMMIT,
            "123",
            "2",
            &smoke_manifest,
            smoke_stdout,
            toolchain_stdout,
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::PlatformTagMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            OTHER_COMMIT,
            "123",
            "2",
            &smoke_manifest,
            smoke_stdout,
            toolchain_stdout,
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::PlatformCommitMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            COMMIT,
            "999",
            "2",
            &smoke_manifest,
            smoke_stdout,
            toolchain_stdout,
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::PlatformRunIdMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            COMMIT,
            "123",
            "3",
            &smoke_manifest,
            smoke_stdout,
            toolchain_stdout,
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::PlatformRunAttemptMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            COMMIT,
            "123",
            "2",
            b"{}",
            smoke_stdout,
            toolchain_stdout,
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::CaptureManifestDigestMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            COMMIT,
            "123",
            "2",
            &smoke_manifest,
            b"tampered",
            toolchain_stdout,
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::CaptureStdoutDigestMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            COMMIT,
            "123",
            "2",
            &smoke_manifest,
            smoke_stdout,
            b"rustc forged\n",
            artifact
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::ToolchainMismatch));
        assert!(verify(
            "v1.11.5-alpha",
            COMMIT,
            "123",
            "2",
            &smoke_manifest,
            smoke_stdout,
            toolchain_stdout,
            b"tampered"
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::PlatformArtifactMismatch));

        let mut envelope_tampered = evidence.clone();
        envelope_tampered.envelope.subject_digest = digest_subject(b"tampered");
        assert!(envelope_tampered
            .verify(
                "v1.11.5-alpha",
                COMMIT,
                "123",
                "2",
                &smoke_manifest,
                smoke_stdout,
                b"",
                &toolchain_manifest,
                toolchain_stdout,
                b"",
                artifact,
            )
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::EnvelopeDigestMismatch));
        let mut envelope_timestamp_tampered = evidence.clone();
        envelope_timestamp_tampered.envelope.timestamp += 1;
        assert!(envelope_timestamp_tampered
            .verify(
                "v1.11.5-alpha",
                COMMIT,
                "123",
                "2",
                &smoke_manifest,
                smoke_stdout,
                b"",
                &toolchain_manifest,
                toolchain_stdout,
                b"",
                artifact,
            )
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::EnvelopeDigestMismatch));
    }

    #[test]
    fn platform_verifier_rejects_semantic_identity_rewrites_after_rehash() {
        let artifact = b"archive";
        let smoke_stdout = b"eva 1.11.5-alpha\n";
        let toolchain_stdout = b"rustc 1.80.0\n";
        let (smoke, smoke_manifest) =
            command_capture("native.smoke", "eva", "linux", "x86_64", smoke_stdout, b"");
        let (toolchain, toolchain_manifest) = command_capture(
            "native.toolchain",
            "rustc",
            "linux",
            "x86_64",
            toolchain_stdout,
            b"",
        );
        let base = platform_evidence(
            "linux",
            "x86_64",
            "x86_64-unknown-linux-gnu",
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            artifact,
            &smoke,
            &toolchain,
        );
        let verify = |evidence: &ReleasePlatformEvidence,
                      smoke_manifest: &[u8],
                      smoke_stdout: &[u8],
                      toolchain_manifest: &[u8],
                      toolchain_stdout: &[u8]| {
            evidence.verify(
                "v1.11.5-alpha",
                COMMIT,
                "123",
                "2",
                smoke_manifest,
                smoke_stdout,
                b"",
                toolchain_manifest,
                toolchain_stdout,
                b"",
                artifact,
            )
        };
        assert!(verify(
            &base,
            &smoke_manifest,
            smoke_stdout,
            &toolchain_manifest,
            toolchain_stdout,
        )
        .is_verified());

        for rewritten in [
            String::from_utf8(smoke_manifest.clone())
                .unwrap()
                .replace("\"run_id\":\"123\"", "\"run_id\":\"999\""),
            String::from_utf8(smoke_manifest.clone())
                .unwrap()
                .replace("\"executable\":\"eva\"", "\"executable\":\"printf\""),
        ] {
            let rewritten_capture = ReleaseCaptureEvidence::from_manifest_bytes(
                rewritten.as_bytes(),
                smoke_stdout,
                b"",
            )
            .unwrap();
            let rewritten_evidence = platform_evidence(
                "linux",
                "x86_64",
                "x86_64-unknown-linux-gnu",
                "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
                artifact,
                &rewritten_capture,
                &toolchain,
            );
            let report = verify(
                &rewritten_evidence,
                rewritten.as_bytes(),
                smoke_stdout,
                &toolchain_manifest,
                toolchain_stdout,
            );
            assert!(report
                .blocked_reasons
                .contains(&EvidenceIntegrityBlocker::CaptureIdentityMismatch));
            assert!(!report
                .blocked_reasons
                .contains(&EvidenceIntegrityBlocker::CaptureManifestDigestMismatch));
        }

        let nonzero_manifest = String::from_utf8(smoke_manifest.clone())
            .unwrap()
            .replace("\"exit_code\":0", "\"exit_code\":7");
        let nonzero_capture = ReleaseCaptureEvidence::from_manifest_bytes(
            nonzero_manifest.as_bytes(),
            smoke_stdout,
            b"",
        )
        .unwrap();
        let nonzero_evidence = platform_evidence(
            "linux",
            "x86_64",
            "x86_64-unknown-linux-gnu",
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            artifact,
            &nonzero_capture,
            &toolchain,
        );
        let nonzero_report = verify(
            &nonzero_evidence,
            nonzero_manifest.as_bytes(),
            smoke_stdout,
            &toolchain_manifest,
            toolchain_stdout,
        );
        assert!(nonzero_report
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::CaptureOutcomeInvalid));
        assert!(!nonzero_report
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::CaptureManifestDigestMismatch));

        let wrong_smoke_stdout = b"eva 9.9.9\n";
        let (wrong_smoke, wrong_smoke_manifest) = command_capture(
            "native.smoke",
            "eva",
            "linux",
            "x86_64",
            wrong_smoke_stdout,
            b"",
        );
        let wrong_smoke_evidence = platform_evidence(
            "linux",
            "x86_64",
            "x86_64-unknown-linux-gnu",
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            artifact,
            &wrong_smoke,
            &toolchain,
        );
        assert!(verify(
            &wrong_smoke_evidence,
            &wrong_smoke_manifest,
            wrong_smoke_stdout,
            &toolchain_manifest,
            toolchain_stdout,
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::PlatformSmokeMismatch));

        let wrong_toolchain_stdout = b"cargo 1.80.0\n";
        let (wrong_toolchain, wrong_toolchain_manifest) = command_capture(
            "native.toolchain",
            "rustc",
            "linux",
            "x86_64",
            wrong_toolchain_stdout,
            b"",
        );
        let wrong_toolchain_evidence = platform_evidence(
            "linux",
            "x86_64",
            "x86_64-unknown-linux-gnu",
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            artifact,
            &smoke,
            &wrong_toolchain,
        );
        assert!(verify(
            &wrong_toolchain_evidence,
            &smoke_manifest,
            smoke_stdout,
            &wrong_toolchain_manifest,
            wrong_toolchain_stdout,
        )
        .blocked_reasons
        .contains(&EvidenceIntegrityBlocker::ToolchainMismatch));

        let mut envelope_rewritten = base.clone();
        envelope_rewritten.envelope.kind = EvidenceKind::Fixture;
        envelope_rewritten.envelope.source = "release-platform:forged".to_owned();
        envelope_rewritten.envelope.environment = "forged".to_owned();
        envelope_rewritten.envelope.timestamp += 1;
        envelope_rewritten.envelope_digest =
            digest_subject(envelope_rewritten.envelope.to_manifest().as_bytes());
        let envelope_report = verify(
            &envelope_rewritten,
            &smoke_manifest,
            smoke_stdout,
            &toolchain_manifest,
            toolchain_stdout,
        );
        assert!(envelope_report
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::EnvelopeIdentityInvalid));
        assert!(!envelope_report
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::EnvelopeDigestMismatch));

        let mut executor_rewritten = base;
        executor_rewritten.envelope.executor = "forged-runner".to_owned();
        executor_rewritten.envelope_digest =
            digest_subject(executor_rewritten.envelope.to_manifest().as_bytes());
        let executor_report = verify(
            &executor_rewritten,
            &smoke_manifest,
            smoke_stdout,
            &toolchain_manifest,
            toolchain_stdout,
        );
        assert!(executor_report
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::CaptureIdentityMismatch));
        assert!(!executor_report
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::EnvelopeDigestMismatch));
    }

    #[test]
    fn capture_json_parser_preserves_utf8_runner_identity() {
        let input = "{\"runner\":{\"identity\":\"runner-测试\"}}";
        let JsonValue::Object(root) = JsonParser::new(input.as_bytes()).parse().unwrap() else {
            panic!("capture fixture must parse as a JSON object");
        };
        assert_eq!(
            json_string(&root, &["runner", "identity"]).as_deref(),
            Some("runner-测试")
        );
    }

    #[test]
    fn platform_bundle_and_target_tampering_are_rejected() {
        let artifact = b"archive";
        let (smoke, _) = command_capture(
            "native.smoke",
            "eva.exe",
            "windows",
            "x86_64",
            b"eva 1.11.5-alpha\n",
            b"",
        );
        let (toolchain, _) = command_capture(
            "native.toolchain",
            "rustc.exe",
            "windows",
            "x86_64",
            b"rustc 1.80.0\n",
            b"",
        );
        let evidence = platform_evidence(
            "windows",
            "x86_64",
            "x86_64-pc-windows-msvc",
            "eva-cli-1.11.5-alpha-x86_64-pc-windows-msvc.zip",
            artifact,
            &smoke,
            &toolchain,
        );
        let mut bundle = ReleasePlatformEvidenceBundle::new(vec![evidence.clone()]).unwrap();
        bundle.bundle_digest = digest_subject(b"tampered");
        assert!(bundle
            .verify("v1.11.5-alpha", COMMIT, "123", "2", &[])
            .blocked_reasons
            .contains(&EvidenceIntegrityBlocker::BundleDigestMismatch));

        let error = ReleasePlatformEvidence::new(
            evidence.tag.clone(),
            evidence.commit.clone(),
            "linux",
            evidence.arch.clone(),
            evidence.toolchain.clone(),
            evidence.run_id.clone(),
            evidence.run_attempt.clone(),
            evidence.job.clone(),
            evidence.artifact_name.clone(),
            "x86_64-pc-windows-msvc",
            evidence.artifact_digest.clone(),
            evidence.artifact_size,
            evidence.capture.clone(),
            evidence.toolchain_capture.clone(),
            evidence.envelope.clone(),
        )
        .unwrap_err();
        assert!(error.message().contains("does not match os and arch"));

        let canonical = ReleasePlatformEvidenceBundle::new(vec![evidence])
            .unwrap()
            .to_manifest();
        let unknown = format!("{canonical}entry.0.unknown=value\n");
        assert!(ReleasePlatformEvidenceBundle::parse_manifest(&unknown)
            .unwrap_err()
            .message()
            .contains("unknown"));
        let hole = canonical
            .replace("entry.0.subject_hex", "entry.1.subject_hex")
            .replace("entry.0.envelope_hex", "entry.1.envelope_hex");
        assert!(ReleasePlatformEvidenceBundle::parse_manifest(&hole)
            .unwrap_err()
            .message()
            .contains("contiguous"));

        let platform_unknown =
            format!("{}capture.unknown=value\n", bundle.entries[0].to_manifest());
        assert!(ReleasePlatformEvidence::parse_manifest(
            &platform_unknown,
            bundle.entries[0].envelope.clone(),
        )
        .unwrap_err()
        .message()
        .contains("unknown field"));

        let noncanonical_subject = format!("{}\n", bundle.entries[0].to_manifest());
        assert!(ReleasePlatformEvidence::parse_manifest(
            &noncanonical_subject,
            bundle.entries[0].envelope.clone(),
        )
        .unwrap_err()
        .message()
        .contains("not canonical"));

        let canonical_bundle = bundle.to_manifest();
        let crlf_envelope = bundle.entries[0]
            .envelope
            .to_manifest()
            .replace('\n', "\r\n");
        let noncanonical_envelope_bundle = canonical_bundle.replace(
            &hex_encode(bundle.entries[0].envelope.to_manifest().as_bytes()),
            &hex_encode(crlf_envelope.as_bytes()),
        );
        assert!(
            ReleasePlatformEvidenceBundle::parse_manifest(&noncanonical_envelope_bundle)
                .unwrap_err()
                .message()
                .contains("envelope is not canonical")
        );
    }

    #[test]
    /// 验证完整性 blocker 的机器码文本和顺序保持稳定。
    fn integrity_blocker_codes_are_stable() {
        assert_eq!(
            [
                EvidenceIntegrityBlocker::EnvelopeFormatInvalid,
                EvidenceIntegrityBlocker::EnvelopeIdentityInvalid,
                EvidenceIntegrityBlocker::SourceCommitInvalid,
                EvidenceIntegrityBlocker::SourceCommitMismatch,
                EvidenceIntegrityBlocker::SubjectDigestInvalid,
                EvidenceIntegrityBlocker::SubjectDigestMismatch,
                EvidenceIntegrityBlocker::SubjectSizeMismatch,
                EvidenceIntegrityBlocker::BundleMixedSourceCommit,
            ]
            .map(EvidenceIntegrityBlocker::as_str),
            [
                "evidence_envelope_format_invalid",
                "evidence_envelope_identity_invalid",
                "evidence_source_commit_invalid",
                "evidence_source_commit_mismatch",
                "evidence_subject_digest_invalid",
                "evidence_subject_digest_mismatch",
                "evidence_subject_size_mismatch",
                "evidence_bundle_mixed_source_commit",
            ]
        );
    }
}
