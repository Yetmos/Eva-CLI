//! 发布分发证据与安装烟雾验证契约。
//! Release distribution evidence and installer smoke verification contracts.

use crate::evidence::{EvidenceEnvelope, EvidenceKind};
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

/// 本模块的架构职责：验证多平台安装流程、文档和包管理器演练证据。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "release distribution and package-manager dry-run evidence contract";

/// 当前支持的发布分发证据清单格式。
pub const DISTRIBUTION_EVIDENCE_FORMAT: &str = "eva.release.distribution_evidence.v1";

/// 一个操作系统和目标平台上的完整安装生命周期烟雾证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInstallSmokeEvidence {
    /// 标准化操作系统名称。
    pub os: String,
    /// Rust target triple 或等价平台目标。
    pub target: String,
    /// 被安装的发布包文件名。
    pub artifact: String,
    /// zip、tar.gz 等包装格式。
    pub package_format: String,
    /// 实际执行的安装命令。
    pub install_command: String,
    /// 安装后验证命令。
    pub smoke_command: String,
    /// 实际验证的卸载命令。
    pub uninstall_command: String,
    /// 实际验证的升级命令。
    pub upgrade_command: String,
    /// 整个安装生命周期的结果状态。
    pub status: String,
}

/// 发布包在某个包管理器中的非破坏性演练证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePackageDryRunEvidence {
    /// 包管理器或制品平台标识。
    pub manager: String,
    /// 被验证的包名或镜像引用。
    pub package: String,
    /// 演练覆盖的平台目标。
    pub target: String,
    /// 生成证据的 dry-run 或 inspect 命令。
    pub command: String,
    /// 演练结果状态。
    pub status: String,
}

/// 与发布来源绑定的分发文档、平台烟雾和包演练证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDistributionEvidence {
    /// 证据清单格式版本。
    pub format: String,
    /// 被验证发布版本。
    pub version: String,
    /// 被验证来源标签。
    pub source_tag: String,
    /// 被验证来源完整提交哈希。
    pub source_commit: String,
    /// 安装文档的仓库相对 Markdown 路径。
    pub install_doc: String,
    /// 卸载文档的仓库相对 Markdown 路径。
    pub uninstall_doc: String,
    /// 升级文档的仓库相对 Markdown 路径。
    pub upgrade_doc: String,
    /// 各操作系统的安装生命周期烟雾证据。
    pub install_smokes: Vec<ReleaseInstallSmokeEvidence>,
    /// 一项或多项包管理器演练证据。
    pub package_dry_runs: Vec<ReleasePackageDryRunEvidence>,
}

/// 分发证据的发布门禁验证结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDistributionVerificationReport {
    /// `verified` 或 `blocked` 状态。
    pub status: String,
    /// 证据对应发布版本。
    pub version: String,
    /// 证据对应来源标签。
    pub source_tag: String,
    /// 证据对应来源提交。
    pub source_commit: String,
    /// 全部平台烟雾证据。
    pub platform_smokes: Vec<ReleaseInstallSmokeEvidence>,
    /// 全部包管理器演练证据。
    pub package_dry_runs: Vec<ReleasePackageDryRunEvidence>,
    /// 三类分发文档路径是否齐全。
    pub install_docs_verified: bool,
    /// 是否至少有一项且全部包管理器演练通过。
    pub package_dry_runs_verified: bool,
    /// 缺失平台、失败烟雾或失败演练的具体风险。
    pub risks: Vec<String>,
    /// 来源、文档和逐项证据审计记录。
    pub audit: Vec<String>,
}

impl ReleaseInstallSmokeEvidence {
    #[allow(clippy::too_many_arguments)]
    /// 校验平台、工件、命令和状态后创建安装烟雾证据。
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

    /// 判断完整安装生命周期是否明确通过。
    pub fn is_passed(&self) -> bool {
        self.status == "passed"
    }
}

impl ReleasePackageDryRunEvidence {
    /// 校验包管理器、目标、命令和状态后创建演练证据。
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

    /// 判断包管理器演练是否明确通过。
    pub fn is_passed(&self) -> bool {
        self.status == "passed"
    }
}

impl ReleaseDistributionEvidence {
    #[allow(clippy::too_many_arguments)]
    /// 创建与版本和完整来源提交绑定的分发证据。
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

    /// 将规范化 distribution evidence 文档绑定到统一信封。
    ///
    /// 当前 subject 是 `to_manifest()` 的精确 UTF-8 字节；其中的命令字段只是清单
    /// 声明，真实安装输出必须另行通过 `EvidenceEnvelope::from_subject_bytes` 绑定。
    pub fn to_envelope(
        &self,
        kind: EvidenceKind,
        source: impl Into<String>,
        environment: impl Into<String>,
        executor: impl Into<String>,
        timestamp: u128,
    ) -> Result<EvidenceEnvelope, EvaError> {
        let subject = self.to_manifest();
        let reparsed = Self::parse_manifest(&subject)?;
        if &reparsed != self {
            return Err(EvaError::invalid_argument(
                "release distribution evidence manifest is not canonical",
            ));
        }
        EvidenceEnvelope::from_subject_bytes(
            kind,
            source,
            self.source_commit.clone(),
            environment,
            executor,
            timestamp,
            subject.as_bytes(),
        )
    }

    /// 从严格键值清单解析平台烟雾和包演练证据。
    ///
    /// 重复字段、缺失复合字段、未知格式和非法平台/状态均失败关闭；数字索引排序
    /// 保证不同解析运行得到相同的证据顺序。
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

    /// 验证 Windows、Linux、macOS 均有通过烟雾，并检查文档和包演练。
    ///
    /// 每个必需操作系统至少需要一项 passed；同一系统存在失败证据也会记录风险。
    /// 包管理器证据必须非空且全部 passed，文档路径必须三类齐全。只有没有任何风险
    /// 才返回 verified，避免缺失平台被当作未发现错误。
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

    /// 以稳定字段、烟雾和演练顺序序列化分发证据。
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

/// 读取必填分发证据字段。
fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release distribution evidence is missing required field")
            .with_context("required_field", key)
    })
}

/// 读取指定证据索引下的必填复合字段。
fn required_indexed(
    fields: &BTreeMap<String, String>,
    prefix: &str,
    index: usize,
    field: &str,
) -> Result<String, EvaError> {
    required(fields, &format!("{prefix}.{index}.{field}"))
}

/// 从复合键收集指定前缀下的有效数字索引并排序去重。
fn indexed_fields(fields: &BTreeMap<String, String>, prefix: &str) -> BTreeSet<usize> {
    fields
        .keys()
        .filter_map(|key| key.strip_prefix(&format!("{prefix}.")))
        .filter_map(|remaining| remaining.split_once('.').map(|(index, _)| index))
        .filter_map(|index| index.parse::<usize>().ok())
        .collect()
}

/// 校验操作系统属于发布矩阵支持的三类平台。
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

/// 校验烟雾或演练状态属于受支持集合。
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

/// 校验文档路径是无遍历的仓库相对 Markdown 路径。
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
    if value.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
        return Err(
            EvaError::invalid_argument(format!("{field} must fit on one manifest line"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验安装工件为不含路径分隔符或遍历片段的文件名。
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
/// 分发证据清单往返、平台覆盖和包演练门禁测试。
mod tests {
    use super::*;

    /// 测试证据使用的完整来源提交。
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// 构造指定平台、工件和状态的安装烟雾证据。
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

    /// 构造指定状态的包管理器演练证据。
    fn dry_run(status: &str) -> ReleasePackageDryRunEvidence {
        ReleasePackageDryRunEvidence::new(
            "ghcr",
            "ghcr.io/yetmos/eva-cli",
            "linux/amd64+linux/arm64",
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.11.5-alpha",
            status,
        )
        .unwrap()
    }

    /// 构造覆盖三平台且演练通过的完整分发证据。
    fn distribution_evidence() -> ReleaseDistributionEvidence {
        ReleaseDistributionEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            COMMIT,
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            vec![
                smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.11.5-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                    "passed",
                ),
                smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                    "passed",
                ),
                smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.11.5-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                    "passed",
                ),
            ],
            vec![dry_run("passed")],
        )
        .unwrap()
    }

    #[test]
    /// 验证完整证据可往返清单并通过全部分发门禁。
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
    /// 验证任一必需操作系统缺少通过烟雾会阻塞分发。
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
    /// 验证包管理器演练失败会阻塞分发证据。
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

    #[test]
    /// 验证 distribution canonical manifest 可由统一信封独立重算摘要。
    fn distribution_manifest_binds_to_evidence_envelope() {
        let evidence = distribution_evidence();
        let manifest = evidence.to_manifest();
        let envelope = evidence
            .to_envelope(
                EvidenceKind::Fixture,
                "distribution-manifest",
                "github-actions-ubuntu-latest",
                "github-actions:run-123",
                1_784_073_600_000,
            )
            .unwrap();

        let report = envelope
            .verify_subject(COMMIT, manifest.as_bytes())
            .unwrap();

        assert!(report.is_verified());
    }

    #[test]
    /// 验证 distribution 命令字段不能注入额外 smoke 行。
    fn distribution_manifest_rejects_line_injection() {
        let error = ReleaseInstallSmokeEvidence::new(
            "linux",
            "x86_64-unknown-linux-gnu",
            "eva.tar.gz",
            "tar.gz",
            "install eva\nsmoke.1.os=windows",
            "eva --version",
            "uninstall eva",
            "upgrade eva",
            "passed",
        )
        .unwrap_err();

        assert_eq!(
            error.message(),
            "release install command must fit on one manifest line"
        );

        let mut mutated = distribution_evidence();
        mutated.install_smokes[0].install_command = "install eva\nforged=value".to_owned();
        let error = mutated
            .to_envelope(
                EvidenceKind::Fixture,
                "distribution-manifest",
                "github-actions-ubuntu-latest",
                "github-actions:run-123",
                1_784_073_600_000,
            )
            .unwrap_err();
        assert_eq!(
            error.message(),
            "release distribution evidence manifest is not canonical"
        );
    }
}
