//! 统一发布证据信封及其稳定清单契约。
//! Unified release evidence envelope and stable manifest contract.

use eva_core::EvaError;
use std::collections::BTreeMap;
use std::fmt;

/// 本模块的架构职责：为所有发布证据提供统一的分类、来源、执行环境和主题身份。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "uniform release evidence identity and provenance envelope";

/// 当前支持的统一发布证据信封格式。
pub const EVIDENCE_ENVELOPE_FORMAT: &str = "eva.release.evidence_envelope.v1";

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

#[cfg(test)]
/// 统一信封的分类、必填字段和稳定清单往返测试。
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
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
}
