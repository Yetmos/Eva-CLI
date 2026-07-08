//! External security scanner evidence verification contracts.

use crate::security::SecuritySeverity;
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "external security scanner evidence contract";

pub const SECURITY_SCAN_EVIDENCE_FORMAT: &str = "eva.release.security_scan_evidence.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSecurityScanFinding {
    pub id: String,
    pub package: String,
    pub version: String,
    pub severity: SecuritySeverity,
    pub summary: String,
    pub remediation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSecurityScanEvidence {
    pub format: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub scanner: String,
    pub scanner_version: String,
    pub scan_status: String,
    pub command: String,
    pub findings: Vec<ReleaseSecurityScanFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSecurityScanVerificationReport {
    pub status: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub scanner: String,
    pub scanner_version: String,
    pub scan_status: String,
    pub findings: Vec<ReleaseSecurityScanFinding>,
    pub blocking_findings: Vec<ReleaseSecurityScanFinding>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

impl ReleaseSecurityScanFinding {
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

    pub fn is_blocking(&self) -> bool {
        matches!(
            self.severity,
            SecuritySeverity::High | SecuritySeverity::Critical
        )
    }
}

impl ReleaseSecurityScanEvidence {
    #[allow(clippy::too_many_arguments)]
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

fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release security scan evidence is missing required field")
            .with_context("required_field", key)
    })
}

fn required_indexed(
    fields: &BTreeMap<String, String>,
    prefix: &str,
    index: usize,
    field: &str,
) -> Result<String, EvaError> {
    required(fields, &format!("{prefix}.{index}.{field}"))
}

fn indexed_fields(fields: &BTreeMap<String, String>, prefix: &str) -> BTreeSet<usize> {
    fields
        .keys()
        .filter_map(|key| key.strip_prefix(&format!("{prefix}.")))
        .filter_map(|remaining| remaining.split_once('.').map(|(index, _)| index))
        .filter_map(|index| index.parse::<usize>().ok())
        .collect()
}

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

fn validate_non_empty(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

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
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

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

    fn evidence(
        scan_status: &str,
        findings: Vec<ReleaseSecurityScanFinding>,
    ) -> ReleaseSecurityScanEvidence {
        ReleaseSecurityScanEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
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
    fn missing_or_skipped_scanner_blocks_evidence() {
        let report = evidence("skipped", Vec::new()).verify();

        assert_eq!(report.status, "blocked");
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "security scanner status is skipped"));
    }
}
