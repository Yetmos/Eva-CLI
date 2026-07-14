//! 外部安全扫描器证据的解析与验证契约。
//! External security scanner evidence verification contracts.

use crate::security::SecuritySeverity;
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

/// 本模块的架构职责：将外部安全扫描结果绑定到发布来源并生成门禁证据。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "external security scanner evidence contract";

/// 当前支持的安全扫描证据清单格式。
pub const SECURITY_SCAN_EVIDENCE_FORMAT: &str = "eva.release.security_scan_evidence.v1";

/// 外部扫描器报告的单个依赖或包安全发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSecurityScanFinding {
    /// 漏洞公告或扫描发现的稳定标识。
    pub id: String,
    /// 受影响包名称。
    pub package: String,
    /// 被扫描的包版本。
    pub version: String,
    /// 标准化后的严重级别。
    pub severity: SecuritySeverity,
    /// 风险摘要。
    pub summary: String,
    /// 推荐的修复或升级动作。
    pub remediation: String,
}

/// 与具体版本、标签和完整提交绑定的安全扫描原始证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSecurityScanEvidence {
    /// 证据清单格式版本。
    pub format: String,
    /// 被扫描的发布版本。
    pub version: String,
    /// 被扫描的来源标签。
    pub source_tag: String,
    /// 被扫描的完整 40 字符提交哈希。
    pub source_commit: String,
    /// 扫描器名称。
    pub scanner: String,
    /// 扫描器版本。
    pub scanner_version: String,
    /// 扫描命令的总体状态。
    pub scan_status: String,
    /// 生成证据的命令。
    pub command: String,
    /// 扫描器输出的标准化发现列表。
    pub findings: Vec<ReleaseSecurityScanFinding>,
}

/// 安全扫描证据的发布门禁验证结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSecurityScanVerificationReport {
    /// `verified` 或 `blocked` 验证状态。
    pub status: String,
    /// 证据对应的发布版本。
    pub version: String,
    /// 证据对应的来源标签。
    pub source_tag: String,
    /// 证据对应的完整提交哈希。
    pub source_commit: String,
    /// 实际使用的扫描器。
    pub scanner: String,
    /// 实际使用的扫描器版本。
    pub scanner_version: String,
    /// 原始扫描总体状态。
    pub scan_status: String,
    /// 全部标准化发现。
    pub findings: Vec<ReleaseSecurityScanFinding>,
    /// 高或严重级别的强制阻塞发现。
    pub blocking_findings: Vec<ReleaseSecurityScanFinding>,
    /// 扫描未通过或存在阻塞发现时的具体风险。
    pub risks: Vec<String>,
    /// 清单、工具、来源和发现审计记录。
    pub audit: Vec<String>,
}

impl ReleaseSecurityScanFinding {
    /// 校验所有字段并将严重级别字符串规范化为枚举。
    pub fn new(
        id: impl Into<String>,
        package: impl Into<String>,
        version: impl Into<String>,
        severity: impl Into<String>,
        summary: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let id = validate_token("security scan finding id", id.into())?;
        let package = validate_non_empty("security scan finding package", package.into())?;
        let version = validate_non_empty("security scan finding version", version.into())?;
        let severity = parse_severity(severity.into())?;
        let summary = validate_non_empty("security scan finding summary", summary.into())?;
        let remediation =
            validate_non_empty("security scan finding remediation", remediation.into())?;
        Ok(Self {
            id,
            package,
            version,
            severity,
            summary,
            remediation,
        })
    }

    /// 判断发现是否达到高或严重级别的发布阻塞阈值。
    pub fn is_blocking(&self) -> bool {
        matches!(
            self.severity,
            SecuritySeverity::High | SecuritySeverity::Critical
        )
    }
}

impl ReleaseSecurityScanEvidence {
    #[allow(clippy::too_many_arguments)]
    /// 创建与发布来源及扫描工具版本绑定的安全扫描证据。
    pub fn new(
        version: impl Into<String>,
        source_tag: impl Into<String>,
        source_commit: impl Into<String>,
        scanner: impl Into<String>,
        scanner_version: impl Into<String>,
        scan_status: impl Into<String>,
        command: impl Into<String>,
        findings: Vec<ReleaseSecurityScanFinding>,
    ) -> Result<Self, EvaError> {
        let version = validate_version(version.into())?;
        let source_tag = validate_token("release source tag", source_tag.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        let scanner = validate_token("security scanner", scanner.into())?;
        let scanner_version =
            validate_non_empty("security scanner version", scanner_version.into())?;
        let scan_status = validate_status(scan_status.into())?;
        let command = validate_non_empty("security scanner command", command.into())?;
        Ok(Self {
            format: SECURITY_SCAN_EVIDENCE_FORMAT.to_owned(),
            version,
            source_tag,
            source_commit,
            scanner,
            scanner_version,
            scan_status,
            command,
            findings,
        })
    }

    /// 从严格 `key=value` 清单解析安全扫描证据。
    ///
    /// 重复字段、缺失必填字段、未知格式和非法索引字段都会失败；发现索引通过有序
    /// 集合恢复，保证序列化后验证顺序稳定。构造器会再次执行全部字段约束。
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        if required(&fields, "format")? != SECURITY_SCAN_EVIDENCE_FORMAT {
            return Err(EvaError::invalid_argument(
                "unsupported release security scan evidence format",
            )
            .with_context("format", required(&fields, "format")?));
        }

        let findings = indexed_fields(&fields, "finding")
            .into_iter()
            .map(|index| {
                ReleaseSecurityScanFinding::new(
                    required_indexed(&fields, "finding", index, "id")?,
                    required_indexed(&fields, "finding", index, "package")?,
                    required_indexed(&fields, "finding", index, "version")?,
                    required_indexed(&fields, "finding", index, "severity")?,
                    required_indexed(&fields, "finding", index, "summary")?,
                    required_indexed(&fields, "finding", index, "remediation")?,
                )
            })
            .collect::<Result<Vec<_>, EvaError>>()?;

        Self::new(
            required(&fields, "version")?,
            required(&fields, "source_tag")?,
            required(&fields, "source_commit")?,
            required(&fields, "scanner")?,
            required(&fields, "scanner_version")?,
            required(&fields, "scan_status")?,
            required(&fields, "command")?,
            findings,
        )
    }

    /// 根据扫描状态和发现严重级别生成失败关闭的验证报告。
    ///
    /// 只有扫描器明确返回 `passed` 且没有高/严重发现时才为 verified。低和中级发现
    /// 会保留在完整列表中但不自动阻塞；跳过、失败或被阻塞的扫描即使没有发现也不
    /// 能作为干净证据。
    pub fn verify(&self) -> ReleaseSecurityScanVerificationReport {
        let mut risks = Vec::new();
        if self.scan_status != "passed" {
            risks.push(format!("security scanner status is {}", self.scan_status));
        }

        let blocking_findings = self
            .findings
            .iter()
            .filter(|finding| finding.is_blocking())
            .cloned()
            .collect::<Vec<_>>();
        for finding in &blocking_findings {
            risks.push(format!(
                "security scanner finding {} is {} severity",
                finding.id,
                finding.severity.as_str()
            ));
        }

        let status = if risks.is_empty() {
            "verified"
        } else {
            "blocked"
        }
        .to_owned();

        let mut audit = vec![
            "release.security_scan:manifest_parsed".to_owned(),
            format!("release.security_scan.scanner:{}", self.scanner),
            format!(
                "release.security_scan.scanner_version:{}",
                self.scanner_version
            ),
            format!("release.security_scan.source_commit:{}", self.source_commit),
            format!("release.security_scan.status:{}", self.scan_status),
        ];
        audit.extend(self.findings.iter().map(|finding| {
            format!(
                "release.security_scan.finding:{}:{}:{}",
                finding.id,
                finding.package,
                finding.severity.as_str()
            )
        }));

        ReleaseSecurityScanVerificationReport {
            status,
            version: self.version.clone(),
            source_tag: self.source_tag.clone(),
            source_commit: self.source_commit.clone(),
            scanner: self.scanner.clone(),
            scanner_version: self.scanner_version.clone(),
            scan_status: self.scan_status.clone(),
            findings: self.findings.clone(),
            blocking_findings,
            risks,
            audit,
        }
    }

    /// 以稳定字段和发现顺序序列化安全扫描证据清单。
    pub fn to_manifest(&self) -> String {
        let mut lines = vec![
            format!("format={}", self.format),
            format!("version={}", self.version),
            format!("source_tag={}", self.source_tag),
            format!("source_commit={}", self.source_commit),
            format!("scanner={}", self.scanner),
            format!("scanner_version={}", self.scanner_version),
            format!("scan_status={}", self.scan_status),
            format!("command={}", self.command),
        ];
        for (index, finding) in self.findings.iter().enumerate() {
            lines.push(format!("finding.{index}.id={}", finding.id));
            lines.push(format!("finding.{index}.package={}", finding.package));
            lines.push(format!("finding.{index}.version={}", finding.version));
            lines.push(format!(
                "finding.{index}.severity={}",
                finding.severity.as_str()
            ));
            lines.push(format!("finding.{index}.summary={}", finding.summary));
            lines.push(format!(
                "finding.{index}.remediation={}",
                finding.remediation
            ));
        }
        format!("{}\n", lines.join("\n"))
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
                "release security scan evidence line must use key=value format",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(EvaError::invalid_argument(
                "release security scan evidence key cannot be empty",
            ));
        }
        if fields
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(EvaError::invalid_argument(
                "release security scan evidence field is duplicated",
            )
            .with_context("field", key));
        }
    }
    Ok(fields)
}

/// 读取必填字段，缺失时返回包含字段名的错误。
fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release security scan evidence is missing required field")
            .with_context("required_field", key)
    })
}

/// 读取指定索引下的必填复合字段。
fn required_indexed(
    fields: &BTreeMap<String, String>,
    prefix: &str,
    index: usize,
    field: &str,
) -> Result<String, EvaError> {
    required(fields, &format!("{prefix}.{index}.{field}"))
}

/// 从复合键中收集指定前缀下的有效数字索引并排序去重。
fn indexed_fields(fields: &BTreeMap<String, String>, prefix: &str) -> BTreeSet<usize> {
    fields
        .keys()
        .filter_map(|key| key.strip_prefix(&format!("{prefix}.")))
        .filter_map(|remaining| remaining.split_once('.').map(|(index, _)| index))
        .filter_map(|index| index.parse::<usize>().ok())
        .collect()
}

/// 解析安全扫描严重级别。
fn parse_severity(value: String) -> Result<SecuritySeverity, EvaError> {
    let value = validate_token("security scan severity", value)?;
    match value.as_str() {
        "low" => Ok(SecuritySeverity::Low),
        "medium" => Ok(SecuritySeverity::Medium),
        "high" => Ok(SecuritySeverity::High),
        "critical" => Ok(SecuritySeverity::Critical),
        _ => Err(EvaError::invalid_argument(
            "security scan severity must be low, medium, high, or critical",
        )
        .with_context("severity", value)),
    }
}

/// 校验扫描总体状态属于受支持集合。
fn validate_status(value: String) -> Result<String, EvaError> {
    let value = validate_token("security scan status", value)?;
    if matches!(value.as_str(), "passed" | "failed" | "blocked" | "skipped") {
        Ok(value)
    } else {
        Err(EvaError::invalid_argument(
            "security scan status must be passed, failed, blocked, or skipped",
        )
        .with_context("status", value))
    }
}

/// 校验发布版本为非空且不含空白的单个标记。
fn validate_version(value: String) -> Result<String, EvaError> {
    let value = validate_non_empty("release version", value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument("release version cannot contain whitespace")
                .with_context("version", value),
        );
    }
    Ok(value)
}

/// 校验不允许包含空白的稳定证据标记。
fn validate_token(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_non_empty(field, value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument(format!("{field} cannot contain whitespace"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验文本非空且首尾无空白。
fn validate_non_empty(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验来源提交为完整 40 字符十六进制哈希。
fn validate_commit(value: String) -> Result<String, EvaError> {
    let value = validate_token("release source commit", value)?;
    if value.len() != 40 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(EvaError::invalid_argument(
            "release source commit must be a full 40-character hex sha",
        )
        .with_context("source_commit", value));
    }
    Ok(value)
}

#[cfg(test)]
/// 安全扫描清单往返和阻塞阈值测试。
mod tests {
    use super::*;

    /// 测试证据使用的完整提交哈希。
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// 构造指定严重级别的测试发现。
    fn finding(severity: &str) -> ReleaseSecurityScanFinding {
        ReleaseSecurityScanFinding::new(
            "RUSTSEC-0000-0000",
            "demo-crate",
            "1.0.0",
            severity,
            "demo advisory",
            "upgrade demo-crate",
        )
        .unwrap()
    }

    /// 构造指定扫描状态和发现列表的测试证据。
    fn evidence(
        scan_status: &str,
        findings: Vec<ReleaseSecurityScanFinding>,
    ) -> ReleaseSecurityScanEvidence {
        ReleaseSecurityScanEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            COMMIT,
            "cargo-audit",
            "1.0.0",
            scan_status,
            "cargo audit --json",
            findings,
        )
        .unwrap()
    }

    #[test]
    /// 验证干净且 passed 的证据可往返清单并通过门禁。
    fn scanner_evidence_round_trips_and_verifies_clean_scan() {
        let parsed = ReleaseSecurityScanEvidence::parse_manifest(
            &evidence("passed", Vec::new()).to_manifest(),
        )
        .unwrap();

        let report = parsed.verify();

        assert_eq!(report.status, "verified");
        assert!(report.risks.is_empty());
        assert!(report.blocking_findings.is_empty());
    }

    #[test]
    /// 验证高严重级别发现会阻塞扫描证据。
    fn high_severity_finding_blocks_scanner_evidence() {
        let report = evidence("passed", vec![finding("high")]).verify();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_findings.len(), 1);
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "security scanner finding RUSTSEC-0000-0000 is high severity"));
    }

    #[test]
    /// 验证 skipped 状态即使没有发现也不能通过门禁。
    fn missing_or_skipped_scanner_blocks_evidence() {
        let report = evidence("skipped", Vec::new()).verify();

        assert_eq!(report.status, "blocked");
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "security scanner status is skipped"));
    }
}
