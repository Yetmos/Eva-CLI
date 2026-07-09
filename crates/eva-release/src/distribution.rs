//! Release distribution evidence and installer smoke verification contracts.

use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "release distribution and package-manager dry-run evidence contract";

pub const DISTRIBUTION_EVIDENCE_FORMAT: &str = "eva.release.distribution_evidence.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInstallSmokeEvidence {
    pub os: String,
    pub target: String,
    pub artifact: String,
    pub package_format: String,
    pub install_command: String,
    pub smoke_command: String,
    pub uninstall_command: String,
    pub upgrade_command: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePackageDryRunEvidence {
    pub manager: String,
    pub package: String,
    pub target: String,
    pub command: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDistributionEvidence {
    pub format: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub install_doc: String,
    pub uninstall_doc: String,
    pub upgrade_doc: String,
    pub install_smokes: Vec<ReleaseInstallSmokeEvidence>,
    pub package_dry_runs: Vec<ReleasePackageDryRunEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDistributionVerificationReport {
    pub status: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub platform_smokes: Vec<ReleaseInstallSmokeEvidence>,
    pub package_dry_runs: Vec<ReleasePackageDryRunEvidence>,
    pub install_docs_verified: bool,
    pub package_dry_runs_verified: bool,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

impl ReleaseInstallSmokeEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        os: impl Into<String>,
        target: impl Into<String>,
        artifact: impl Into<String>,
        package_format: impl Into<String>,
        install_command: impl Into<String>,
        smoke_command: impl Into<String>,
        uninstall_command: impl Into<String>,
        upgrade_command: impl Into<String>,
        status: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let os = validate_os(os.into())?;
        let target = validate_token("release install target", target.into())?;
        let artifact = validate_artifact_name(artifact.into())?;
        let package_format = validate_token("release package format", package_format.into())?;
        let install_command =
            validate_non_empty("release install command", install_command.into())?;
        let smoke_command = validate_non_empty("release smoke command", smoke_command.into())?;
        let uninstall_command =
            validate_non_empty("release uninstall command", uninstall_command.into())?;
        let upgrade_command =
            validate_non_empty("release upgrade command", upgrade_command.into())?;
        let status = validate_status(status.into())?;
        Ok(Self {
            os,
            target,
            artifact,
            package_format,
            install_command,
            smoke_command,
            uninstall_command,
            upgrade_command,
            status,
        })
    }

    pub fn is_passed(&self) -> bool {
        self.status == "passed"
    }
}

impl ReleasePackageDryRunEvidence {
    pub fn new(
        manager: impl Into<String>,
        package: impl Into<String>,
        target: impl Into<String>,
        command: impl Into<String>,
        status: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let manager = validate_token("release package manager", manager.into())?;
        let package = validate_non_empty("release package name", package.into())?;
        let target = validate_non_empty("release package target", target.into())?;
        let command = validate_non_empty("release package dry-run command", command.into())?;
        let status = validate_status(status.into())?;
        Ok(Self {
            manager,
            package,
            target,
            command,
            status,
        })
    }

    pub fn is_passed(&self) -> bool {
        self.status == "passed"
    }
}

impl ReleaseDistributionEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: impl Into<String>,
        source_tag: impl Into<String>,
        source_commit: impl Into<String>,
        install_doc: impl Into<String>,
        uninstall_doc: impl Into<String>,
        upgrade_doc: impl Into<String>,
        install_smokes: Vec<ReleaseInstallSmokeEvidence>,
        package_dry_runs: Vec<ReleasePackageDryRunEvidence>,
    ) -> Result<Self, EvaError> {
        let version = validate_version(version.into())?;
        let source_tag = validate_token("release source tag", source_tag.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        let install_doc = validate_doc_path("release install doc", install_doc.into())?;
        let uninstall_doc = validate_doc_path("release uninstall doc", uninstall_doc.into())?;
        let upgrade_doc = validate_doc_path("release upgrade doc", upgrade_doc.into())?;
        Ok(Self {
            format: DISTRIBUTION_EVIDENCE_FORMAT.to_owned(),
            version,
            source_tag,
            source_commit,
            install_doc,
            uninstall_doc,
            upgrade_doc,
            install_smokes,
            package_dry_runs,
        })
    }

    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        if required(&fields, "format")? != DISTRIBUTION_EVIDENCE_FORMAT {
            return Err(EvaError::invalid_argument(
                "unsupported release distribution evidence format",
            )
            .with_context("format", required(&fields, "format")?));
        }

        let install_smokes = indexed_fields(&fields, "smoke")
            .into_iter()
            .map(|index| {
                ReleaseInstallSmokeEvidence::new(
                    required_indexed(&fields, "smoke", index, "os")?,
                    required_indexed(&fields, "smoke", index, "target")?,
                    required_indexed(&fields, "smoke", index, "artifact")?,
                    required_indexed(&fields, "smoke", index, "package_format")?,
                    required_indexed(&fields, "smoke", index, "install_command")?,
                    required_indexed(&fields, "smoke", index, "smoke_command")?,
                    required_indexed(&fields, "smoke", index, "uninstall_command")?,
                    required_indexed(&fields, "smoke", index, "upgrade_command")?,
                    required_indexed(&fields, "smoke", index, "status")?,
                )
            })
            .collect::<Result<Vec<_>, EvaError>>()?;

        let package_dry_runs = indexed_fields(&fields, "package")
            .into_iter()
            .map(|index| {
                ReleasePackageDryRunEvidence::new(
                    required_indexed(&fields, "package", index, "manager")?,
                    required_indexed(&fields, "package", index, "package")?,
                    required_indexed(&fields, "package", index, "target")?,
                    required_indexed(&fields, "package", index, "command")?,
                    required_indexed(&fields, "package", index, "status")?,
                )
            })
            .collect::<Result<Vec<_>, EvaError>>()?;

        Self::new(
            required(&fields, "version")?,
            required(&fields, "source_tag")?,
            required(&fields, "source_commit")?,
            required(&fields, "docs.install")?,
            required(&fields, "docs.uninstall")?,
            required(&fields, "docs.upgrade")?,
            install_smokes,
            package_dry_runs,
        )
    }

    pub fn verify(&self) -> ReleaseDistributionVerificationReport {
        let mut risks = Vec::new();
        let mut passed_platforms = BTreeSet::new();
        for smoke in &self.install_smokes {
            if smoke.is_passed() {
                passed_platforms.insert(smoke.os.clone());
            } else {
                risks.push(format!(
                    "install smoke for {} {} is {}",
                    smoke.os, smoke.target, smoke.status
                ));
            }
        }

        for os in ["windows", "linux", "macos"] {
            if !passed_platforms.contains(os) {
                risks.push(format!("missing passed install smoke for {os}"));
            }
        }

        if self.package_dry_runs.is_empty() {
            risks.push("package manager dry-run evidence is missing".to_owned());
        }
        for dry_run in &self.package_dry_runs {
            if !dry_run.is_passed() {
                risks.push(format!(
                    "package manager dry-run for {} {} is {}",
                    dry_run.manager, dry_run.target, dry_run.status
                ));
            }
        }

        let install_docs_verified = !self.install_doc.trim().is_empty()
            && !self.uninstall_doc.trim().is_empty()
            && !self.upgrade_doc.trim().is_empty();
        if !install_docs_verified {
            risks.push("install, uninstall, and upgrade docs must be recorded".to_owned());
        }
        let package_dry_runs_verified = !self.package_dry_runs.is_empty()
            && self
                .package_dry_runs
                .iter()
                .all(ReleasePackageDryRunEvidence::is_passed);

        let status = if risks.is_empty() {
            "verified"
        } else {
            "blocked"
        }
        .to_owned();
        let mut audit = vec![
            "release.distribution:manifest_parsed".to_owned(),
            format!("release.distribution.version:{}", self.version),
            format!("release.distribution.source_commit:{}", self.source_commit),
            format!("release.distribution.docs.install:{}", self.install_doc),
            format!("release.distribution.docs.uninstall:{}", self.uninstall_doc),
            format!("release.distribution.docs.upgrade:{}", self.upgrade_doc),
        ];
        audit.extend(self.install_smokes.iter().map(|smoke| {
            format!(
                "release.distribution.install_smoke:{}:{}:{}",
                smoke.os, smoke.target, smoke.status
            )
        }));
        audit.extend(self.package_dry_runs.iter().map(|dry_run| {
            format!(
                "release.distribution.package_dry_run:{}:{}:{}",
                dry_run.manager, dry_run.target, dry_run.status
            )
        }));

        ReleaseDistributionVerificationReport {
            status,
            version: self.version.clone(),
            source_tag: self.source_tag.clone(),
            source_commit: self.source_commit.clone(),
            platform_smokes: self.install_smokes.clone(),
            package_dry_runs: self.package_dry_runs.clone(),
            install_docs_verified,
            package_dry_runs_verified,
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
            format!("docs.install={}", self.install_doc),
            format!("docs.uninstall={}", self.uninstall_doc),
            format!("docs.upgrade={}", self.upgrade_doc),
        ];
        for (index, smoke) in self.install_smokes.iter().enumerate() {
            lines.push(format!("smoke.{index}.os={}", smoke.os));
            lines.push(format!("smoke.{index}.target={}", smoke.target));
            lines.push(format!("smoke.{index}.artifact={}", smoke.artifact));
            lines.push(format!(
                "smoke.{index}.package_format={}",
                smoke.package_format
            ));
            lines.push(format!(
                "smoke.{index}.install_command={}",
                smoke.install_command
            ));
            lines.push(format!(
                "smoke.{index}.smoke_command={}",
                smoke.smoke_command
            ));
            lines.push(format!(
                "smoke.{index}.uninstall_command={}",
                smoke.uninstall_command
            ));
            lines.push(format!(
                "smoke.{index}.upgrade_command={}",
                smoke.upgrade_command
            ));
            lines.push(format!("smoke.{index}.status={}", smoke.status));
        }
        for (index, dry_run) in self.package_dry_runs.iter().enumerate() {
            lines.push(format!("package.{index}.manager={}", dry_run.manager));
            lines.push(format!("package.{index}.package={}", dry_run.package));
            lines.push(format!("package.{index}.target={}", dry_run.target));
            lines.push(format!("package.{index}.command={}", dry_run.command));
            lines.push(format!("package.{index}.status={}", dry_run.status));
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
                "release distribution evidence line must use key=value format",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(EvaError::invalid_argument(
                "release distribution evidence key cannot be empty",
            ));
        }
        if fields
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(EvaError::invalid_argument(
                "release distribution evidence field is duplicated",
            )
            .with_context("field", key));
        }
    }
    Ok(fields)
}

fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release distribution evidence is missing required field")
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

fn validate_os(value: String) -> Result<String, EvaError> {
    let value = validate_token("release install os", value)?;
    if matches!(value.as_str(), "windows" | "linux" | "macos") {
        Ok(value)
    } else {
        Err(
            EvaError::invalid_argument("release install os must be windows, linux, or macos")
                .with_context("os", value),
        )
    }
}

fn validate_status(value: String) -> Result<String, EvaError> {
    let value = validate_token("release distribution evidence status", value)?;
    if matches!(value.as_str(), "passed" | "blocked" | "failed" | "skipped") {
        Ok(value)
    } else {
        Err(EvaError::invalid_argument(
            "release distribution evidence status must be passed, blocked, failed, or skipped",
        )
        .with_context("status", value))
    }
}

fn validate_doc_path(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_non_empty(field, value)?;
    if value.contains("..") || value.contains('\\') || !value.ends_with(".md") {
        return Err(EvaError::invalid_argument(
            "release distribution doc path must be a repository markdown path",
        )
        .with_context("path", value));
    }
    Ok(value)
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

fn validate_artifact_name(value: String) -> Result<String, EvaError> {
    let value = validate_token("release install artifact", value)?;
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(EvaError::invalid_argument(
            "release install artifact must be a stable file name",
        )
        .with_context("artifact", value));
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

    fn smoke(
        os: &str,
        target: &str,
        artifact: &str,
        package_format: &str,
        status: &str,
    ) -> ReleaseInstallSmokeEvidence {
        ReleaseInstallSmokeEvidence::new(
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

    fn dry_run(status: &str) -> ReleasePackageDryRunEvidence {
        ReleasePackageDryRunEvidence::new(
            "ghcr",
            "ghcr.io/yetmos/eva-cli",
            "linux/amd64+linux/arm64",
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.11.4-alpha",
            status,
        )
        .unwrap()
    }

    fn distribution_evidence() -> ReleaseDistributionEvidence {
        ReleaseDistributionEvidence::new(
            "1.11.4-alpha",
            "v1.11.4-alpha",
            COMMIT,
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            vec![
                smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.11.4-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                    "passed",
                ),
                smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.11.4-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                    "passed",
                ),
                smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.11.4-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                    "passed",
                ),
            ],
            vec![dry_run("passed")],
        )
        .unwrap()
    }

    #[test]
    fn distribution_evidence_round_trips_and_verifies() {
        let evidence =
            ReleaseDistributionEvidence::parse_manifest(&distribution_evidence().to_manifest())
                .unwrap();

        let report = evidence.verify();

        assert_eq!(report.status, "verified");
        assert!(report.install_docs_verified);
        assert!(report.package_dry_runs_verified);
        assert!(report.risks.is_empty());
    }

    #[test]
    fn missing_platform_smoke_blocks_distribution_verification() {
        let mut evidence = distribution_evidence();
        evidence.install_smokes.retain(|smoke| smoke.os != "linux");

        let report = evidence.verify();

        assert_eq!(report.status, "blocked");
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "missing passed install smoke for linux"));
    }

    #[test]
    fn failed_package_dry_run_blocks_distribution_verification() {
        let mut evidence = distribution_evidence();
        evidence.package_dry_runs = vec![dry_run("failed")];

        let report = evidence.verify();

        assert_eq!(report.status, "blocked");
        assert!(!report.package_dry_runs_verified);
        assert!(report.risks.iter().any(
            |risk| risk == "package manager dry-run for ghcr linux/amd64+linux/arm64 is failed"
        ));
    }
}
