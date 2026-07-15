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

/// 统一 manifest 中一类 gate evidence、信封和可选真实主题的相对引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEvidenceManifestEntry {
    /// 当前 checklist 消费的 evidence 类型。
    pub evidence_type: ReleaseEvidenceType,
    /// 相对统一 manifest 目录的 typed evidence 文档路径。
    pub evidence_path: String,
    /// 相对统一 manifest 目录的 EvidenceEnvelope 路径。
    pub envelope_path: String,
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
            subject_path,
        })
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
                || !matches!(parts[2], "type" | "evidence" | "envelope" | "subject")
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
                ReleaseEvidenceManifestEntry::new(
                    ReleaseEvidenceType::parse(&required_manifest_field(
                        &fields,
                        &format!("{prefix}.type"),
                    )?)?,
                    required_manifest_field(&fields, &format!("{prefix}.evidence"))?,
                    required_manifest_field(&fields, &format!("{prefix}.envelope"))?,
                    fields.get(&format!("{prefix}.subject")).cloned(),
                )
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
            if let Some(subject_path) = &entry.subject_path {
                output.push_str(&format!("entry.{index}.subject={subject_path}\n"));
            }
        }
        output
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
