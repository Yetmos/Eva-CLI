//! 发布就绪、Security、Performance 和 Migration 门禁子命令；可读取外部证据覆盖内置基线。

use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_command_error_with_exit_code, write_error_kind,
    CommonOptions, OutputFormat, EXIT_CONFIG, EXIT_OK, EXIT_POLICY, EXIT_PRODUCTION_BLOCKED,
    EXIT_RUNTIME_UNAVAILABLE,
};
use eva_core::{ErrorKind, EvaError};
use eva_observability::TraceFields;
use eva_release::{
    verify_evidence_bundle, write_package_manager_metadata, CanonicalPackageMetadata,
    CompatibilityPolicy, EvidenceEnvelope, EvidenceKind, EvidenceSubject, MigrationGuide,
    MigrationStep, PerformanceBaselineReport, PerformanceBudget, PlatformReadiness,
    ProductionEvidenceBlocker, ProductionEvidencePolicy, ReleaseArtifactEvidence,
    ReleaseArtifactEvidenceCandidate, ReleaseBenchmarkEvidence, ReleaseDistributionEvidence,
    ReleaseDocumentEvidenceCandidate, ReleaseEvidenceManifest, ReleaseEvidenceScope,
    ReleaseEvidenceType, ReleaseGate, ReleaseHardeningService, ReleaseReadinessReport,
    ReleaseSecurityScanEvidence, SecurityFinding, SecurityReviewReport, StabilityScenario,
    VerifiedReleaseEvidenceBundle,
};
use std::fs;
use std::io::{Read, Write};
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::fd::AsRawFd;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
use std::os::unix::fs::MetadataExt as UnixMetadataExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Release 子命令及其已解析选项。
pub(super) enum ReleaseCommand {
    /// 聚合平台、稳定性、恢复、分发、安全和性能门禁。
    Check(
        /// 已解析的发布标识、证据目录与公共选项。
        ReleaseCheckOptions,
    ),
    /// 运行内置安全评审。
    Security(
        /// 安全评审命令共享的项目根目录与输出格式。
        CommonOptions,
    ),
    /// 对性能基线或外部 benchmark 证据执行预算检查。
    Perf(
        /// 已解析的可选 benchmark 证据路径与公共选项。
        ReleasePerfOptions,
    ),
    /// 生成版本间迁移指南。
    Migration(
        /// 已解析的起始版本、目标版本与公共选项。
        ReleaseMigrationOptions,
    ),
    PackageMetadata(ReleasePackageMetadataOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// External GitHub Actions run tuple supplied by the release consumer.
struct ReleaseExpectedRun {
    id: String,
    attempt: String,
    manifest_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 发布就绪检查选项及可选外部证据文件。
pub(super) struct ReleaseCheckOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// all、windows、linux 或 macos 等目标平台。
    target: String,
    /// alpha 兼容范围或 production 强证据范围。
    scope: ReleaseEvidenceScope,
    /// 可选统一 evidence 索引清单。
    evidence_manifest: Option<PathBuf>,
    /// 由 CI checkout 等外部可信上下文提供的完整提交。
    expected_source_commit: Option<String>,
    /// 由 CI invocation context 提供、不能从 evidence 推导的 run ID。
    expected_run: Option<Box<ReleaseExpectedRun>>,
    /// 可选签名产物证据。
    artifact_evidence: Option<PathBuf>,
    /// 可选安装/分发烟测证据。
    distribution_evidence: Option<PathBuf>,
    /// 可选安全扫描器证据。
    security_scan_evidence: Option<PathBuf>,
    /// 可选真实性能测量证据。
    benchmark_evidence: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 性能门禁选项。
pub(super) struct ReleasePerfOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 可选 benchmark 证据；缺省使用服务内置基线。
    benchmark_evidence: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 发布迁移指南的源/目标版本。
pub(super) struct ReleaseMigrationOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 当前版本文本。
    from_version: String,
    /// 目标版本文本。
    to_version: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleasePackageMetadataOptions {
    common: CommonOptions,
    manifest: PathBuf,
    output_root: PathBuf,
    installed_size_kib: u64,
}

/// 不暴露本地路径或 subject 内容的 evidence 输入摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseEvidenceSummary {
    source: &'static str,
    entry_count: usize,
    normalized_envelope_count: usize,
    integrity_status: &'static str,
    expected_commit_source: &'static str,
    manifest_digest: Option<String>,
    manifest_digest_source: &'static str,
    gate_provenance: Vec<ReleaseGateProvenance>,
}

impl ReleaseEvidenceSummary {
    fn none() -> Self {
        Self {
            source: "none",
            entry_count: 0,
            normalized_envelope_count: 0,
            integrity_status: "not_applicable",
            expected_commit_source: "none",
            manifest_digest: None,
            manifest_digest_source: "none",
            gate_provenance: Vec::new(),
        }
    }
}

/// Public, path-free projection of one verified envelope for a release gate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseGateProvenance {
    evidence_type: ReleaseEvidenceType,
    source: String,
    source_commit: String,
    environment: String,
    executor: String,
    timestamp_ms: u128,
    subject_digest: String,
    envelope_digest: String,
}

impl ReleaseGateProvenance {
    fn from_envelope(evidence_type: ReleaseEvidenceType, envelope: &EvidenceEnvelope) -> Self {
        Self {
            evidence_type,
            source: envelope.source.clone(),
            source_commit: envelope.source_commit.clone(),
            environment: envelope.environment.clone(),
            executor: envelope.executor.clone(),
            timestamp_ms: envelope.timestamp,
            subject_digest: envelope.subject_digest.clone(),
            envelope_digest: envelope.canonical_digest(),
        }
    }
}

/// artifact gate evidence、统一信封与真实工件字节。
struct LoadedArtifactEvidence {
    evidence: ReleaseArtifactEvidence,
    envelope: EvidenceEnvelope,
    subject_bytes: Vec<u8>,
}

/// 以 canonical typed manifest 作为统一信封主题的 evidence。
struct LoadedDocumentEvidence<T> {
    evidence: T,
    envelope: EvidenceEnvelope,
    subject_bytes: Vec<u8>,
}

/// release check 一次调用中已解析、归一化且完成完整性校验的 evidence 集合。
struct LoadedReleaseEvidence {
    verified_bundle: Option<VerifiedReleaseEvidenceBundle>,
    artifact: Option<LoadedArtifactEvidence>,
    distribution: Option<LoadedDocumentEvidence<ReleaseDistributionEvidence>>,
    security_scan: Option<LoadedDocumentEvidence<ReleaseSecurityScanEvidence>>,
    benchmark: Option<LoadedDocumentEvidence<ReleaseBenchmarkEvidence>>,
    summary: ReleaseEvidenceSummary,
}

impl LoadedReleaseEvidence {
    fn empty() -> Self {
        Self {
            verified_bundle: None,
            artifact: None,
            distribution: None,
            security_scan: None,
            benchmark: None,
            summary: ReleaseEvidenceSummary::none(),
        }
    }

    /// Snapshot verified envelope identity before candidates move into the release verifier.
    fn gate_provenance(&self) -> Vec<ReleaseGateProvenance> {
        [
            (
                ReleaseEvidenceType::Artifact,
                self.artifact.as_ref().map(|item| &item.envelope),
            ),
            (
                ReleaseEvidenceType::Distribution,
                self.distribution.as_ref().map(|item| &item.envelope),
            ),
            (
                ReleaseEvidenceType::SecurityScan,
                self.security_scan.as_ref().map(|item| &item.envelope),
            ),
            (
                ReleaseEvidenceType::Benchmark,
                self.benchmark.as_ref().map(|item| &item.envelope),
            ),
        ]
        .into_iter()
        .filter_map(|(evidence_type, envelope)| {
            envelope.map(|envelope| ReleaseGateProvenance::from_envelope(evidence_type, envelope))
        })
        .collect()
    }

    /// 复验全部 envelope、canonical subject 和 typed evidence 的来源提交。
    fn verify_integrity(&self, expected_source_commit: &str) -> Result<(), EvaError> {
        let mut subjects = Vec::new();
        if let Some(artifact) = &self.artifact {
            subjects.push(
                EvidenceSubject::new(&artifact.envelope, &artifact.subject_bytes)
                    .with_source_commit_claim(
                        "legacy_artifact_evidence",
                        &artifact.evidence.source_commit,
                    ),
            );
        }
        if let Some(distribution) = &self.distribution {
            subjects.push(
                EvidenceSubject::new(&distribution.envelope, &distribution.subject_bytes)
                    .with_source_commit_claim(
                        "distribution_evidence",
                        &distribution.evidence.source_commit,
                    ),
            );
        }
        if let Some(security_scan) = &self.security_scan {
            subjects.push(
                EvidenceSubject::new(&security_scan.envelope, &security_scan.subject_bytes)
                    .with_source_commit_claim(
                        "security_scan_evidence",
                        &security_scan.evidence.source_commit,
                    ),
            );
        }
        if let Some(benchmark) = &self.benchmark {
            subjects.push(
                EvidenceSubject::new(&benchmark.envelope, &benchmark.subject_bytes)
                    .with_source_commit_claim(
                        "benchmark_evidence",
                        &benchmark.evidence.source_commit,
                    ),
            );
        }

        let report = verify_evidence_bundle(expected_source_commit, &subjects)?;
        if report.is_verified() {
            Ok(())
        } else {
            Err(
                EvaError::conflict("release evidence manifest integrity verification was blocked")
                    .with_context(
                        "blocked_reasons",
                        report
                            .blocked_reasons
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                    )
                    .with_context("subject_count", report.subject_count.to_string())
                    .with_context(
                        "verified_subject_count",
                        report.verified_subject_count.to_string(),
                    ),
            )
        }
    }
}

/// 解析 `release check|security|perf|migration` 子命令。
pub(super) fn parse_release_command(args: &[String]) -> Result<ReleaseCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing release subcommand"))?;
    match subcommand.as_str() {
        "check" => Ok(ReleaseCommand::Check(parse_release_check_options(rest)?)),
        "security" => Ok(ReleaseCommand::Security(parse_common_options(rest)?)),
        "perf" | "performance" => Ok(ReleaseCommand::Perf(parse_release_perf_options(rest)?)),
        "migration" => Ok(ReleaseCommand::Migration(parse_release_migration_options(
            rest,
        )?)),
        "package-metadata" => Ok(ReleaseCommand::PackageMetadata(
            parse_release_package_metadata_options(rest)?,
        )),
        value => {
            Err(EvaError::unsupported("unknown release subcommand")
                .with_context("subcommand", value))
        }
    }
}
fn parse_release_package_metadata_options(
    args: &[String],
) -> Result<ReleasePackageMetadataOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut manifest = None;
    let mut output_root = None;
    let mut installed_size_kib = 1u64;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "generate" => {}
            "--manifest" => {
                index += 1;
                manifest = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "package metadata manifest",
                )?));
            }
            "--output-root" => {
                index += 1;
                output_root = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "package metadata output root",
                )?));
            }
            "--installed-size-kib" => {
                index += 1;
                installed_size_kib = required_option(args, index, "installed size")?
                    .parse()
                    .map_err(|_| EvaError::invalid_argument("installed size must be an integer"))?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    Ok(ReleasePackageMetadataOptions {
        common: parse_common_options(&passthrough)?,
        manifest: manifest
            .ok_or_else(|| EvaError::invalid_argument("package metadata manifest is required"))?,
        output_root: output_root.ok_or_else(|| {
            EvaError::invalid_argument("package metadata output root is required")
        })?,
        installed_size_kib,
    })
}

/// 解析发布目标和四类外部证据路径；空目标在读取证据前即失败。
fn parse_release_check_options(args: &[String]) -> Result<ReleaseCheckOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut target = "all".to_owned();
    let mut scope = ReleaseEvidenceScope::Alpha;
    let mut evidence_manifest = None;
    let mut expected_source_commit = None;
    let mut expected_run_id = None;
    let mut expected_run_attempt = None;
    let mut expected_manifest_digest = None;
    let mut scope_seen = false;
    let mut evidence_manifest_seen = false;
    let mut expected_source_commit_seen = false;
    let mut expected_run_id_seen = false;
    let mut expected_run_attempt_seen = false;
    let mut expected_manifest_digest_seen = false;
    let mut artifact_evidence = None;
    let mut distribution_evidence = None;
    let mut security_scan_evidence = None;
    let mut benchmark_evidence = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--target" | "--platform" => {
                index += 1;
                target = required_option(args, index, "release target option")?.clone();
            }
            "--scope" => {
                if scope_seen {
                    return Err(EvaError::invalid_argument(
                        "release evidence scope option is duplicated",
                    ));
                }
                scope_seen = true;
                index += 1;
                scope = ReleaseEvidenceScope::parse(required_option(
                    args,
                    index,
                    "release evidence scope option",
                )?)?;
            }
            "--evidence-manifest" => {
                if evidence_manifest_seen {
                    return Err(EvaError::invalid_argument(
                        "release evidence manifest option is duplicated",
                    ));
                }
                evidence_manifest_seen = true;
                index += 1;
                evidence_manifest = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "release evidence manifest option",
                )?));
            }
            "--expected-source-commit" => {
                if expected_source_commit_seen {
                    return Err(EvaError::invalid_argument(
                        "expected source commit option is duplicated",
                    ));
                }
                expected_source_commit_seen = true;
                index += 1;
                expected_source_commit =
                    Some(required_option(args, index, "expected source commit option")?.clone());
            }
            "--expected-run-id" => {
                if expected_run_id_seen {
                    return Err(EvaError::invalid_argument(
                        "expected run id option is duplicated",
                    ));
                }
                expected_run_id_seen = true;
                index += 1;
                expected_run_id =
                    Some(required_option(args, index, "expected run id option")?.clone());
            }
            "--expected-run-attempt" => {
                if expected_run_attempt_seen {
                    return Err(EvaError::invalid_argument(
                        "expected run attempt option is duplicated",
                    ));
                }
                expected_run_attempt_seen = true;
                index += 1;
                expected_run_attempt =
                    Some(required_option(args, index, "expected run attempt option")?.clone());
            }
            "--expected-manifest-digest" => {
                if expected_manifest_digest_seen {
                    return Err(EvaError::invalid_argument(
                        "expected manifest digest option is duplicated",
                    ));
                }
                expected_manifest_digest_seen = true;
                index += 1;
                expected_manifest_digest =
                    Some(required_option(args, index, "expected manifest digest option")?.clone());
            }
            "--artifact-evidence" | "--artifact-evidence-file" => {
                index += 1;
                artifact_evidence = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "artifact evidence option",
                )?));
            }
            "--distribution-evidence" | "--distribution-evidence-file" => {
                index += 1;
                distribution_evidence = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "distribution evidence option",
                )?));
            }
            "--security-scan-evidence" | "--scanner-evidence" | "--security-scan-evidence-file" => {
                index += 1;
                security_scan_evidence = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "security scan evidence option",
                )?));
            }
            "--benchmark-evidence" | "--benchmark-evidence-file" => {
                index += 1;
                benchmark_evidence = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "benchmark evidence option",
                )?));
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    if target.trim().is_empty() {
        return Err(EvaError::invalid_argument("release target cannot be empty"));
    }
    let expected_run = match (expected_run_id, expected_run_attempt) {
        (Some(id), Some(attempt)) => Some(Box::new(ReleaseExpectedRun {
            id,
            attempt,
            manifest_digest: expected_manifest_digest,
        })),
        (None, None) => None,
        _ => {
            return Err(EvaError::invalid_argument(
                "expected run id and attempt options must be provided together",
            ))
        }
    };
    Ok(ReleaseCheckOptions {
        common: parse_common_options(&passthrough)?,
        target,
        scope,
        evidence_manifest,
        expected_source_commit,
        expected_run,
        artifact_evidence,
        distribution_evidence,
        security_scan_evidence,
        benchmark_evidence,
    })
}

/// 解析可选 benchmark 证据和公共选项。
fn parse_release_perf_options(args: &[String]) -> Result<ReleasePerfOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut benchmark_evidence = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--benchmark-evidence" | "--benchmark-evidence-file" => {
                index += 1;
                benchmark_evidence = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "benchmark evidence option",
                )?));
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    Ok(ReleasePerfOptions {
        common: parse_common_options(&passthrough)?,
        benchmark_evidence,
    })
}

/// 解析源/目标版本并拒绝空值。
fn parse_release_migration_options(args: &[String]) -> Result<ReleaseMigrationOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut from_version = "1.5.1".to_owned();
    let mut to_version = "1.11.5-alpha".to_owned();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from" | "--from-version" => {
                index += 1;
                from_version = required_option(args, index, "from version option")?.clone();
            }
            "--to" | "--to-version" => {
                index += 1;
                to_version = required_option(args, index, "to version option")?.clone();
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    if from_version.trim().is_empty() || to_version.trim().is_empty() {
        return Err(EvaError::invalid_argument(
            "release migration versions cannot be empty",
        ));
    }
    Ok(ReleaseMigrationOptions {
        common: parse_common_options(&passthrough)?,
        from_version,
        to_version,
    })
}

/// 执行发布门禁并按失败性质选择退出码。
///
/// Alpha readiness blocker 保持配置类退出码，production evidence/policy blocker 使用独立
/// 策略退出码；运行时、不可用和内部错误继续通过统一错误分类映射。
pub(super) fn execute_release<W, E>(
    command: ReleaseCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    let service = ReleaseHardeningService::v15();
    match command {
        ReleaseCommand::PackageMetadata(options) => {
            let trace = trace_for("cli.release.package_metadata");
            let result = (|| {
                let bytes = fs::read(&options.manifest).map_err(|e| {
                    EvaError::not_found("package metadata manifest cannot be read")
                        .with_context("io_error", e.to_string())
                })?;
                if bytes.len() > 1024 * 1024 {
                    return Err(EvaError::invalid_argument(
                        "package metadata manifest is too large",
                    ));
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    EvaError::invalid_argument("package metadata manifest must be UTF-8")
                })?;
                let metadata = CanonicalPackageMetadata::parse_manifest(text)?;
                write_package_manager_metadata(
                    &metadata,
                    &options.output_root,
                    options.installed_size_kib,
                )
            })();
            match result {
                Ok(report) => {
                    let files = report
                        .files
                        .iter()
                        .filter_map(|p| p.strip_prefix(&options.output_root).ok())
                        .map(|p| json_string(&p.to_string_lossy().replace('\\', "/")));
                    writeln!(
                        stdout,
                        "{}",
                        success_envelope(
                            "release.package-metadata.generate",
                            EXIT_OK,
                            &format!("{{\"files\":{}}}", json_array(files)),
                            &trace
                        )
                    )
                    .map_err(write_error_kind)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "release.package-metadata.generate",
                    &error,
                    &trace,
                ),
            }
        }
        ReleaseCommand::Check(options) => {
            let trace = trace_for("cli.release.check");
            let report = (|| {
                let evidence = load_release_check_evidence(&options)?;
                let report = if let Some(bundle) = evidence.verified_bundle.as_ref() {
                    service.readiness_with_verified_release_evidence(&options.target, bundle)?
                } else {
                    if options.scope == ReleaseEvidenceScope::Production {
                        return Err(EvaError::invalid_argument(
                            "production release check requires an evidence manifest",
                        )
                        .with_context(
                            "blocked_reasons",
                            ProductionEvidenceBlocker::ManifestRequired.as_str(),
                        ));
                    }
                    service.readiness_with_release_evidence(
                        &options.target,
                        evidence.artifact.as_ref().map(|loaded| &loaded.evidence),
                        evidence
                            .distribution
                            .as_ref()
                            .map(|loaded| &loaded.evidence),
                        evidence
                            .security_scan
                            .as_ref()
                            .map(|loaded| &loaded.evidence),
                        evidence.benchmark.as_ref().map(|loaded| &loaded.evidence),
                    )?
                };
                Ok::<_, EvaError>((report, evidence.summary))
            })();
            match report {
                Ok((report, summary)) => {
                    let exit_code = if report.blocking_count() == 0 {
                        EXIT_OK
                    } else if report.evidence_scope == ReleaseEvidenceScope::Production {
                        EXIT_PRODUCTION_BLOCKED
                    } else {
                        EXIT_CONFIG
                    };
                    write_release_check(
                        stdout,
                        options.common.output,
                        exit_code,
                        &summary,
                        &report,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
                Err(error) => {
                    if options.scope == ReleaseEvidenceScope::Production
                        && is_production_blocking_error(&error)
                    {
                        write_command_error_with_exit_code(
                            stderr,
                            options.common.output,
                            "release.check",
                            EXIT_PRODUCTION_BLOCKED,
                            &error,
                            &trace,
                        )
                    } else {
                        write_command_error(
                            stderr,
                            options.common.output,
                            "release.check",
                            &error,
                            &trace,
                        )
                    }
                }
            }
        }
        ReleaseCommand::Security(options) => {
            let trace = trace_for("cli.release.security");
            let report = service.security_review();
            write_release_security(stdout, options.output, &report, &trace)?;
            Ok(if report.blocking_findings() == 0 {
                EXIT_OK
            } else {
                EXIT_POLICY
            })
        }
        ReleaseCommand::Perf(options) => {
            let trace = trace_for("cli.release.perf");
            let report = (|| {
                let benchmark_evidence = options
                    .benchmark_evidence
                    .as_ref()
                    .map(|path| read_release_benchmark_evidence(path))
                    .transpose()?;
                Ok::<PerformanceBaselineReport, EvaError>(
                    benchmark_evidence
                        .as_ref()
                        .map(ReleaseBenchmarkEvidence::to_performance_report)
                        .unwrap_or_else(|| service.performance_baseline()),
                )
            })();
            match report {
                Ok(report) => {
                    // 无 evidence 的 alpha 诊断保持可执行，但只报告 unmeasured；真实失败仍非零。
                    let exit_code = if (report.status == "within_budget"
                        && report.over_budget_count() == 0
                        && report.unmeasured_count() == 0)
                        || (report.status == "unmeasured" && report.over_budget_count() == 0)
                    {
                        EXIT_OK
                    } else {
                        EXIT_RUNTIME_UNAVAILABLE
                    };
                    write_release_perf(stdout, options.common.output, exit_code, &report, &trace)?;
                    Ok(exit_code)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "release.perf",
                    &error,
                    &trace,
                ),
            }
        }
        ReleaseCommand::Migration(options) => {
            let trace = trace_for("cli.release.migration");
            match service.migration_guide(&options.from_version, &options.to_version) {
                Ok(guide) => {
                    write_release_migration(stdout, options.common.output, &guide, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "release.migration",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// Production input and policy rejections are one public release-decision class.
/// Runtime/unavailable/internal failures retain the global CLI error mapping.
fn is_production_blocking_error(error: &EvaError) -> bool {
    matches!(
        error.kind(),
        ErrorKind::InvalidArgument
            | ErrorKind::NotFound
            | ErrorKind::Conflict
            | ErrorKind::PermissionDenied
    )
}

/// 根据 scope 选择统一 manifest 或 alpha 兼容参数，并在聚合前完成 envelope 校验。
fn load_release_check_evidence(
    options: &ReleaseCheckOptions,
) -> Result<LoadedReleaseEvidence, EvaError> {
    let legacy_count = [
        options.artifact_evidence.is_some(),
        options.distribution_evidence.is_some(),
        options.security_scan_evidence.is_some(),
        options.benchmark_evidence.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    if options.evidence_manifest.is_some() && legacy_count > 0 {
        return Err(EvaError::invalid_argument(
            "release evidence manifest cannot be combined with legacy evidence options",
        ));
    }
    if options.scope == ReleaseEvidenceScope::Production && legacy_count > 0 {
        return Err(EvaError::invalid_argument(
            "production release check does not accept legacy evidence options",
        ));
    }

    if let Some(manifest_path) = &options.evidence_manifest {
        load_manifest_release_evidence(options, manifest_path)
    } else if legacy_count > 0 {
        load_legacy_release_evidence(options)
    } else {
        Ok(LoadedReleaseEvidence::empty())
    }
}

/// 读取统一 manifest 的全部引用；production 的 expected commit 必须来自外部参数。
fn load_manifest_release_evidence(
    options: &ReleaseCheckOptions,
    manifest_path: &Path,
) -> Result<LoadedReleaseEvidence, EvaError> {
    let canonical_manifest_path = canonical_file(manifest_path, "evidence_manifest")?;
    let data = fs::read_to_string(&canonical_manifest_path).map_err(|error| {
        EvaError::not_found("failed to read release evidence manifest")
            .with_context("evidence_manifest", manifest_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let manifest = ReleaseEvidenceManifest::parse_manifest(&data).map_err(|error| {
        error.with_context("evidence_manifest", manifest_path.display().to_string())
    })?;
    if manifest.scope != options.scope {
        return Err(EvaError::invalid_argument(
            "release evidence manifest scope does not match CLI scope",
        )
        .with_context("cli_scope", options.scope.as_str())
        .with_context("manifest_scope", manifest.scope.as_str()));
    }
    if options.scope == ReleaseEvidenceScope::Production && options.expected_source_commit.is_none()
    {
        return Err(EvaError::invalid_argument(
            "production release check requires --expected-source-commit",
        )
        .with_context(
            "blocked_reasons",
            ProductionEvidenceBlocker::TrustedCommitRequired.as_str(),
        ));
    }
    let expected_source_commit = options
        .expected_source_commit
        .clone()
        .unwrap_or_else(|| manifest.source_commit.clone());
    if manifest.source_commit != expected_source_commit {
        return Err(EvaError::conflict(
            "release evidence manifest source commit does not match trusted commit",
        )
        .with_context("blocked_reasons", "evidence_source_commit_mismatch"));
    }

    let base = canonical_manifest_path.parent().ok_or_else(|| {
        EvaError::invalid_argument("release evidence manifest must have a parent directory")
    })?;
    let mut loaded = LoadedReleaseEvidence::empty();
    for entry in &manifest.entries {
        let evidence_bytes = read_manifest_reference(base, &entry.evidence_path, "evidence")?;
        let evidence_data = manifest_utf8(&evidence_bytes, "evidence")?;
        let envelope_bytes = read_manifest_reference(base, &entry.envelope_path, "envelope")?;
        let envelope_data = manifest_utf8(&envelope_bytes, "envelope")?;
        let envelope = EvidenceEnvelope::parse_manifest(&envelope_data)?;

        match entry.evidence_type {
            ReleaseEvidenceType::Artifact => {
                let evidence = ReleaseArtifactEvidence::parse_manifest(&evidence_data)?;
                let subject_bytes = read_manifest_reference(
                    base,
                    entry
                        .subject_path
                        .as_deref()
                        .expect("artifact entries are validated with subject paths"),
                    "subject",
                )?;
                loaded.artifact = Some(LoadedArtifactEvidence {
                    evidence,
                    envelope,
                    subject_bytes,
                });
            }
            ReleaseEvidenceType::Distribution => {
                let evidence = ReleaseDistributionEvidence::parse_manifest(&evidence_data)?;
                let subject_bytes = evidence.to_manifest().into_bytes();
                loaded.distribution = Some(LoadedDocumentEvidence {
                    evidence,
                    envelope,
                    subject_bytes,
                });
            }
            ReleaseEvidenceType::SecurityScan => {
                let evidence = ReleaseSecurityScanEvidence::parse_manifest(&evidence_data)?;
                let subject_bytes = evidence.to_manifest().into_bytes();
                loaded.security_scan = Some(LoadedDocumentEvidence {
                    evidence,
                    envelope,
                    subject_bytes,
                });
            }
            ReleaseEvidenceType::Benchmark => {
                let evidence = ReleaseBenchmarkEvidence::parse_manifest(&evidence_data)?;
                let subject_bytes = evidence.to_manifest().into_bytes();
                loaded.benchmark = Some(LoadedDocumentEvidence {
                    evidence,
                    envelope,
                    subject_bytes,
                });
            }
        }
    }
    let entry_count = manifest.entries.len();
    let manifest_digest = manifest.canonical_digest();
    let gate_provenance = loaded.gate_provenance();
    let artifact = loaded.artifact.take().map(|candidate| {
        ReleaseArtifactEvidenceCandidate::new(
            candidate.evidence,
            candidate.envelope,
            candidate.subject_bytes,
        )
    });
    let distribution = loaded.distribution.take().map(|candidate| {
        ReleaseDocumentEvidenceCandidate::new(candidate.evidence, candidate.envelope)
    });
    let security_scan = loaded.security_scan.take().map(|candidate| {
        ReleaseDocumentEvidenceCandidate::new(candidate.evidence, candidate.envelope)
    });
    let benchmark = loaded.benchmark.take().map(|candidate| {
        ReleaseDocumentEvidenceCandidate::new(candidate.evidence, candidate.envelope)
    });
    let verified_bundle = if options.scope == ReleaseEvidenceScope::Production {
        let expected_run = options.expected_run.as_deref().ok_or_else(|| {
            EvaError::invalid_argument(
                "production release check requires --expected-run-id and --expected-run-attempt",
            )
            .with_context(
                "blocked_reasons",
                ProductionEvidenceBlocker::TrustedRunRequired.as_str(),
            )
        })?;
        let expected_manifest_digest =
            expected_run.manifest_digest.as_deref().ok_or_else(|| {
                EvaError::invalid_argument(
                    "production release check requires --expected-manifest-digest",
                )
                .with_context(
                    "blocked_reasons",
                    ProductionEvidenceBlocker::ManifestDigestRequired.as_str(),
                )
            })?;
        let policy = ProductionEvidencePolicy::github_actions(
            trusted_current_epoch_ms()?,
            &expected_run.id,
            &expected_run.attempt,
            expected_manifest_digest,
        )?;
        VerifiedReleaseEvidenceBundle::verify_production(
            manifest,
            &expected_source_commit,
            artifact,
            distribution,
            security_scan,
            benchmark,
            &policy,
        )?
    } else {
        VerifiedReleaseEvidenceBundle::verify(
            manifest,
            &expected_source_commit,
            artifact,
            distribution,
            security_scan,
            benchmark,
        )?
    };
    loaded.summary = ReleaseEvidenceSummary {
        source: "manifest",
        entry_count,
        normalized_envelope_count: entry_count,
        integrity_status: "verified",
        expected_commit_source: if options.expected_source_commit.is_some() {
            "external_option"
        } else {
            "manifest_claim_alpha_only"
        },
        manifest_digest: Some(manifest_digest),
        manifest_digest_source: if options.scope == ReleaseEvidenceScope::Production {
            "external_option"
        } else {
            "computed_manifest_alpha_only"
        },
        gate_provenance,
    };
    loaded.verified_bundle = Some(verified_bundle);
    Ok(loaded)
}

/// Read the consumer clock independently of any producer-owned timestamp.
fn trusted_current_epoch_ms() -> Result<u128, EvaError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            EvaError::internal("system clock is before the Unix epoch")
                .with_context("clock_error", error.to_string())
        })
}

/// 将旧四参数归一化为 declaration envelope，但不补造运行身份或 artifact 字节。
fn load_legacy_release_evidence(
    options: &ReleaseCheckOptions,
) -> Result<LoadedReleaseEvidence, EvaError> {
    let mut loaded = LoadedReleaseEvidence::empty();
    if let Some(path) = &options.artifact_evidence {
        let evidence = read_release_artifact_evidence(path)?;
        let subject_bytes = evidence.to_manifest().into_bytes();
        let envelope = legacy_manifest_envelope(
            "legacy:artifact-evidence-manifest",
            &evidence.source_commit,
            &subject_bytes,
        )?;
        loaded.artifact = Some(LoadedArtifactEvidence {
            evidence,
            envelope,
            subject_bytes,
        });
    }
    if let Some(path) = &options.distribution_evidence {
        let evidence = read_release_distribution_evidence(path)?;
        let subject_bytes = evidence.to_manifest().into_bytes();
        let envelope = legacy_manifest_envelope(
            "legacy:distribution-evidence-manifest",
            &evidence.source_commit,
            &subject_bytes,
        )?;
        loaded.distribution = Some(LoadedDocumentEvidence {
            evidence,
            envelope,
            subject_bytes,
        });
    }
    if let Some(path) = &options.security_scan_evidence {
        let evidence = read_release_security_scan_evidence(path)?;
        let subject_bytes = evidence.to_manifest().into_bytes();
        let envelope = legacy_manifest_envelope(
            "legacy:security-scan-evidence-manifest",
            &evidence.source_commit,
            &subject_bytes,
        )?;
        loaded.security_scan = Some(LoadedDocumentEvidence {
            evidence,
            envelope,
            subject_bytes,
        });
    }
    if let Some(path) = &options.benchmark_evidence {
        let evidence = read_release_benchmark_evidence(path)?;
        let subject_bytes = evidence.to_manifest().into_bytes();
        let envelope = legacy_manifest_envelope(
            "legacy:benchmark-evidence-manifest",
            &evidence.source_commit,
            &subject_bytes,
        )?;
        loaded.benchmark = Some(LoadedDocumentEvidence {
            evidence,
            envelope,
            subject_bytes,
        });
    }

    let expected_source_commit = options
        .expected_source_commit
        .as_deref()
        .or_else(|| {
            loaded
                .artifact
                .as_ref()
                .map(|item| item.evidence.source_commit.as_str())
        })
        .or_else(|| {
            loaded
                .distribution
                .as_ref()
                .map(|item| item.evidence.source_commit.as_str())
        })
        .or_else(|| {
            loaded
                .security_scan
                .as_ref()
                .map(|item| item.evidence.source_commit.as_str())
        })
        .or_else(|| {
            loaded
                .benchmark
                .as_ref()
                .map(|item| item.evidence.source_commit.as_str())
        })
        .expect("legacy loader is called only when at least one option is present")
        .to_owned();
    loaded.verify_integrity(&expected_source_commit)?;
    let entry_count = [
        loaded.artifact.is_some(),
        loaded.distribution.is_some(),
        loaded.security_scan.is_some(),
        loaded.benchmark.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    let gate_provenance = loaded.gate_provenance();
    loaded.summary = ReleaseEvidenceSummary {
        source: "legacy_alpha",
        entry_count,
        normalized_envelope_count: entry_count,
        integrity_status: "verified",
        expected_commit_source: if options.expected_source_commit.is_some() {
            "external_option"
        } else {
            "legacy_evidence_claim_alpha_only"
        },
        manifest_digest: None,
        manifest_digest_source: "none",
        gate_provenance,
    };
    Ok(loaded)
}

/// 旧参数缺少采集身份，使用固定弱分类与 sentinel 元数据，避免补造 production 测量。
fn legacy_manifest_envelope(
    source: &str,
    source_commit: &str,
    subject_bytes: &[u8],
) -> Result<EvidenceEnvelope, EvaError> {
    EvidenceEnvelope::from_subject_bytes(
        EvidenceKind::Declaration,
        source,
        source_commit,
        "legacy-unbound",
        "legacy-cli-input",
        1,
        subject_bytes,
    )
}

/// 将 manifest 自身解析为真实文件并拒绝目录。
fn canonical_file(path: &Path, context: &str) -> Result<PathBuf, EvaError> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        EvaError::not_found("release evidence file is missing")
            .with_context(context, path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    if !canonical.is_file() {
        return Err(
            EvaError::invalid_argument("release evidence path must reference a file")
                .with_context(context, path.display().to_string()),
        );
    }
    Ok(canonical)
}

/// 从受限相对引用打开同一 handle，并在读取前后复验 canonical target 未漂移。
fn read_manifest_reference(
    base: &Path,
    reference: &str,
    context: &str,
) -> Result<Vec<u8>, EvaError> {
    let joined = base.join(reference);
    let canonical_before = canonical_file(&joined, context)?;
    if !canonical_before.starts_with(base) {
        return Err(EvaError::invalid_argument(
            "release evidence manifest reference escapes its directory",
        )
        .with_context("reference_kind", context));
    }
    let mut file = fs::File::open(&canonical_before).map_err(|error| {
        EvaError::not_found("failed to open release evidence manifest reference")
            .with_context("reference_kind", context)
            .with_context("io_error", error.to_string())
    })?;
    let opened_metadata = file.metadata().map_err(|error| {
        EvaError::not_found("failed to inspect release evidence manifest reference")
            .with_context("reference_kind", context)
            .with_context("io_error", error.to_string())
    })?;
    if !opened_metadata.is_file() {
        return Err(EvaError::invalid_argument(
            "release evidence manifest reference must be a regular file",
        )
        .with_context("reference_kind", context));
    }
    let canonical_open = canonical_file(&joined, context)?;
    if canonical_open != canonical_before || !canonical_open.starts_with(base) {
        return Err(EvaError::conflict(
            "release evidence manifest reference changed while opening",
        )
        .with_context("reference_kind", context));
    }
    if !opened_file_matches_checked_path(&canonical_open, &file)? {
        return Err(EvaError::conflict(
            "release evidence manifest reference identity changed before opening",
        )
        .with_context("reference_kind", context));
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data).map_err(|error| {
        EvaError::not_found("failed to read release evidence manifest reference")
            .with_context("reference_kind", context)
            .with_context("io_error", error.to_string())
    })?;
    let canonical_after = canonical_file(&joined, context)?;
    if canonical_after != canonical_before || !canonical_after.starts_with(base) {
        return Err(EvaError::conflict(
            "release evidence manifest reference changed while reading",
        )
        .with_context("reference_kind", context));
    }
    Ok(data)
}

/// 将 manifest 文本从 UTF-8 bytes 解码，拒绝平台默认编码和有损替换。
fn manifest_utf8(data: &[u8], context: &str) -> Result<String, EvaError> {
    String::from_utf8(data.to_vec()).map_err(|error| {
        EvaError::invalid_argument("release evidence manifest reference must be UTF-8")
            .with_context("reference_kind", context)
            .with_context("utf8_error", error.to_string())
    })
}

/// Linux/Android 通过 procfs 查询首个已打开 handle 的最终路径。
#[cfg(any(target_os = "linux", target_os = "android"))]
fn opened_file_matches_checked_path(
    checked_path: &Path,
    opened: &fs::File,
) -> Result<bool, EvaError> {
    let final_path =
        fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd())).map_err(|error| {
            EvaError::not_found("failed to resolve opened release evidence handle")
                .with_context("io_error", error.to_string())
        })?;
    Ok(final_path == checked_path)
}

/// macOS 及其他 Unix 平台比较路径和已打开 handle 的设备/inode 身份。
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn opened_file_matches_checked_path(
    checked_path: &Path,
    opened: &fs::File,
) -> Result<bool, EvaError> {
    let expected = fs::metadata(checked_path).map_err(|error| {
        EvaError::not_found("failed to inspect checked release evidence path")
            .with_context("io_error", error.to_string())
    })?;
    let opened = opened.metadata().map_err(|error| {
        EvaError::not_found("failed to inspect opened release evidence handle")
            .with_context("io_error", error.to_string())
    })?;
    Ok(expected.dev() == opened.dev() && expected.ino() == opened.ino())
}

/// Windows compares the checked path and opened handle by stable volume/file identity.
#[cfg(windows)]
fn opened_file_matches_checked_path(
    checked_path: &Path,
    opened: &fs::File,
) -> Result<bool, EvaError> {
    let checked = fs::File::open(checked_path).map_err(|error| {
        EvaError::not_found("failed to open checked release evidence path")
            .with_context("io_error", error.to_string())
    })?;
    Ok(windows_file_identity(&checked, "checked_path")?
        == windows_file_identity(opened, "opened_handle")?)
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct WindowsFileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(windows)]
const WINDOWS_FILE_ID_INFO_CLASS: i32 = 18;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    #[link_name = "GetFileInformationByHandleEx"]
    fn get_file_information_by_handle_ex(
        file: *mut std::ffi::c_void,
        file_information_class: i32,
        file_information: *mut std::ffi::c_void,
        buffer_size: u32,
    ) -> i32;
}

/// Reads the Win32 `FILE_ID_INFO` tuple for one already-open handle.
#[cfg(windows)]
fn windows_file_identity(
    file: &fs::File,
    identity_subject: &str,
) -> Result<WindowsFileIdentity, EvaError> {
    let mut identity = WindowsFileIdentity {
        volume_serial_number: 0,
        file_id: [0; 16],
    };
    // SAFETY: `file` stays open for the call and `identity` is a correctly sized,
    // writable FILE_ID_INFO buffer for FileIdInfo (class 18).
    let succeeded = unsafe {
        get_file_information_by_handle_ex(
            file.as_raw_handle().cast(),
            WINDOWS_FILE_ID_INFO_CLASS,
            (&mut identity as *mut WindowsFileIdentity).cast(),
            std::mem::size_of::<WindowsFileIdentity>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(
            EvaError::not_found("failed to read release evidence file identity")
                .with_context("identity_subject", identity_subject)
                .with_context("io_error", std::io::Error::last_os_error().to_string()),
        );
    }
    Ok(identity)
}

/// 读取签名产物证据 manifest，并在 I/O/解析失败上附加文件路径。
fn read_release_artifact_evidence(path: &Path) -> Result<ReleaseArtifactEvidence, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "release artifact evidence file is missing"
        } else {
            "failed to read release artifact evidence file"
        };
        EvaError::not_found(message)
            .with_context("artifact_evidence", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ReleaseArtifactEvidence::parse_manifest(&data)
        .map_err(|error| error.with_context("artifact_evidence", path.display().to_string()))
}

/// 读取分发与安装烟测证据，并保留路径上下文。
fn read_release_distribution_evidence(
    path: &Path,
) -> Result<ReleaseDistributionEvidence, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "release distribution evidence file is missing"
        } else {
            "failed to read release distribution evidence file"
        };
        EvaError::not_found(message)
            .with_context("distribution_evidence", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ReleaseDistributionEvidence::parse_manifest(&data)
        .map_err(|error| error.with_context("distribution_evidence", path.display().to_string()))
}

/// 读取外部安全扫描证据，并保留路径上下文。
fn read_release_security_scan_evidence(
    path: &Path,
) -> Result<ReleaseSecurityScanEvidence, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "release security scan evidence file is missing"
        } else {
            "failed to read release security scan evidence file"
        };
        EvaError::not_found(message)
            .with_context("security_scan_evidence", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ReleaseSecurityScanEvidence::parse_manifest(&data)
        .map_err(|error| error.with_context("security_scan_evidence", path.display().to_string()))
}

/// 读取 benchmark 实测证据，并保留路径上下文。
fn read_release_benchmark_evidence(path: &Path) -> Result<ReleaseBenchmarkEvidence, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "release benchmark evidence file is missing"
        } else {
            "failed to read release benchmark evidence file"
        };
        EvaError::not_found(message)
            .with_context("benchmark_evidence", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ReleaseBenchmarkEvidence::parse_manifest(&data)
        .map_err(|error| error.with_context("benchmark_evidence", path.display().to_string()))
}

/// 输出发布 readiness、closure、平台、场景和 gate 摘要。
fn write_release_check<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    evidence_summary: &ReleaseEvidenceSummary,
    report: &ReleaseReadinessReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Release readiness").map_err(write_error_kind)?;
            writeln!(writer, "version: {}", report.version).map_err(write_error_kind)?;
            writeln!(writer, "target: {}", report.target).map_err(write_error_kind)?;
            writeln!(writer, "evidence_scope: {}", report.evidence_scope)
                .map_err(write_error_kind)?;
            writeln!(writer, "evidence_source: {}", evidence_summary.source)
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "blocking_gates: {}", report.blocking_count())
                .map_err(write_error_kind)?;
            writeln!(writer, "warning_gates: {}", report.warning_count())
                .map_err(write_error_kind)?;
            writeln!(writer, "closure_status: {}", report.closure.status)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "closure_external_blockers: {}",
                report.closure.blocked_external_items.len()
            )
            .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "release.check",
                exit_code,
                &release_check_json(evidence_summary, report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 输出安全评审 finding 与 blocker 数量。
fn write_release_security<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &SecurityReviewReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Security review").map_err(write_error_kind)?;
            writeln!(writer, "version: {}", report.version).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "findings: {}", report.findings.len()).map_err(write_error_kind)?;
            writeln!(writer, "blocking_findings: {}", report.blocking_findings())
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "release.security",
                EXIT_OK,
                &security_review_json(report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 输出性能预算、观测值和超预算数量。
fn write_release_perf<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    report: &PerformanceBaselineReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Performance baseline").map_err(write_error_kind)?;
            writeln!(writer, "version: {}", report.version).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "budgets: {}", report.budgets.len()).map_err(write_error_kind)?;
            writeln!(writer, "measured: {}", report.measured_count()).map_err(write_error_kind)?;
            writeln!(writer, "unmeasured: {}", report.unmeasured_count())
                .map_err(write_error_kind)?;
            writeln!(writer, "over_budget: {}", report.over_budget_count())
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "release.perf",
                exit_code,
                &performance_baseline_json(report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 输出版本迁移步骤和兼容策略。
fn write_release_migration<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    guide: &MigrationGuide,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Migration guide").map_err(write_error_kind)?;
            writeln!(writer, "{} -> {}", guide.from_version, guide.to_version)
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", guide.status).map_err(write_error_kind)?;
            writeln!(writer, "breaking_changes: {}", guide.breaking_changes.len())
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "release.migration",
                EXIT_OK,
                &migration_guide_json(guide),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 将完整发布 readiness 报告编码为稳定 JSON。
fn release_check_json(
    evidence_summary: &ReleaseEvidenceSummary,
    report: &ReleaseReadinessReport,
) -> String {
    format!(
        "{{\"version\":{},\"status\":{},\"target\":{},\"evidence_scope\":{},\"evidence_manifest\":{},\"blocking_gates\":{},\"warning_gates\":{},\"platforms\":{},\"stability\":{},\"gates\":{},\"closure\":{},\"audit\":{}}}",
        json_string(&report.version),
        json_string(&report.status),
        json_string(&report.target),
        json_string(report.evidence_scope.as_str()),
        release_evidence_summary_json(evidence_summary),
        report.blocking_count(),
        report.warning_count(),
        json_array(report.platforms.iter().map(platform_readiness_json)),
        json_array(report.stability.iter().map(stability_scenario_json)),
        json_array(
            report
                .gates
                .iter()
                .map(|gate| release_gate_json(gate, evidence_summary))
        ),
        v1x_closure_json(report),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将路径脱敏后的 manifest/legacy normalization 摘要编码为 JSON。
fn release_evidence_summary_json(summary: &ReleaseEvidenceSummary) -> String {
    format!(
        "{{\"source\":{},\"entry_count\":{},\"normalized_envelope_count\":{},\"integrity_status\":{},\"expected_commit_source\":{},\"manifest_digest\":{},\"manifest_digest_source\":{}}}",
        json_string(summary.source),
        summary.entry_count,
        summary.normalized_envelope_count,
        json_string(summary.integrity_status),
        json_string(summary.expected_commit_source),
        option_json(summary.manifest_digest.as_deref()),
        json_string(summary.manifest_digest_source),
    )
}

/// 将 V1.x closure gate 的覆盖、缺口和审计证据编码为 JSON。
fn v1x_closure_json(report: &ReleaseReadinessReport) -> String {
    let closure = &report.closure;
    format!(
        "{{\"status\":{},\"summary\":{},\"required_gate_ids\":{},\"passed_required_gate_ids\":{},\"missing_required_gate_ids\":{},\"blocking_required_gate_ids\":{},\"optional_production_gate_ids\":{},\"blocked_external_items\":{}}}",
        json_string(&closure.status),
        json_string(&closure.summary),
        json_array(closure.required_gate_ids.iter().map(|item| json_string(item))),
        json_array(
            closure
                .passed_required_gate_ids
                .iter()
                .map(|item| json_string(item))
        ),
        json_array(
            closure
                .missing_required_gate_ids
                .iter()
                .map(|item| json_string(item))
        ),
        json_array(
            closure
                .blocking_required_gate_ids
                .iter()
                .map(|item| json_string(item))
        ),
        json_array(
            closure
                .optional_production_gate_ids
                .iter()
                .map(|item| json_string(item))
        ),
        json_array(
            closure
                .blocked_external_items
                .iter()
                .map(|item| json_string(item))
        )
    )
}

/// 将单个平台就绪报告编码为 JSON。
fn platform_readiness_json(platform: &PlatformReadiness) -> String {
    format!(
        "{{\"os\":{},\"shell\":{},\"path_model\":{},\"status\":{},\"required_commands\":{},\"notes\":{}}}",
        json_string(&platform.os),
        json_string(&platform.shell),
        json_string(&platform.path_model),
        json_string(platform.status.as_str()),
        json_array(platform.required_commands.iter().map(|command| json_string(command))),
        json_array(platform.notes.iter().map(|note| json_string(note)))
    )
}

/// 将稳定性场景的状态、步骤和证据编码为 JSON。
fn stability_scenario_json(scenario: &StabilityScenario) -> String {
    format!(
        "{{\"id\":{},\"status\":{},\"scenario\":{},\"evidence\":{},\"recovery_contract\":{}}}",
        json_string(&scenario.id),
        json_string(scenario.status.as_str()),
        json_string(&scenario.scenario),
        json_array(scenario.evidence.iter().map(|entry| json_string(entry))),
        json_string(&scenario.recovery_contract)
    )
}

/// 将单个发布 gate、其 path-free provenance 及 blocker/risk 编码为 JSON。
fn release_gate_json(gate: &ReleaseGate, summary: &ReleaseEvidenceSummary) -> String {
    format!(
        "{{\"id\":{},\"domain\":{},\"evidence_kind\":{},\"provenance\":{},\"status\":{},\"required\":{},\"summary\":{},\"evidence\":{},\"remediation\":{}}}",
        json_string(&gate.id),
        json_string(&gate.domain),
        json_string(gate.evidence_kind.as_str()),
        release_gate_provenance_json(&gate.id, summary),
        json_string(gate.status.as_str()),
        gate.required,
        json_string(&gate.summary),
        json_array(gate.evidence.iter().map(|entry| json_string(entry))),
        json_array(gate.remediation.iter().map(|entry| json_string(entry)))
    )
}

/// Project a verified envelope onto its stable external gate; built-ins stay explicitly null.
fn release_gate_provenance_json(gate_id: &str, summary: &ReleaseEvidenceSummary) -> String {
    let evidence_type = match gate_id {
        "REL-ARTIFACT-PROVENANCE-001" => Some(ReleaseEvidenceType::Artifact),
        "REL-DISTRIBUTION-001" => Some(ReleaseEvidenceType::Distribution),
        "REL-SECURITY-SCAN-001" => Some(ReleaseEvidenceType::SecurityScan),
        "REL-BENCHMARK-001" => Some(ReleaseEvidenceType::Benchmark),
        _ => None,
    };
    let provenance = evidence_type.and_then(|evidence_type| {
        summary
            .gate_provenance
            .iter()
            .find(|item| item.evidence_type == evidence_type)
    });
    let timestamp = provenance
        .map(|item| item.timestamp_ms.to_string())
        .unwrap_or_else(|| "null".to_owned());

    format!(
        "{{\"evidence_type\":{},\"source\":{},\"source_commit\":{},\"environment\":{},\"executor\":{},\"timestamp_ms\":{},\"subject_digest\":{},\"envelope_digest\":{}}}",
        option_json(evidence_type.map(ReleaseEvidenceType::as_str)),
        option_json(provenance.map(|item| item.source.as_str())),
        option_json(provenance.map(|item| item.source_commit.as_str())),
        option_json(provenance.map(|item| item.environment.as_str())),
        option_json(provenance.map(|item| item.executor.as_str())),
        timestamp,
        option_json(provenance.map(|item| item.subject_digest.as_str())),
        option_json(provenance.map(|item| item.envelope_digest.as_str())),
    )
}

/// 将安全评审汇总和 findings 编码为 JSON。
fn security_review_json(report: &SecurityReviewReport) -> String {
    format!(
        "{{\"version\":{},\"status\":{},\"blocking_findings\":{},\"findings\":{},\"audit\":{}}}",
        json_string(&report.version),
        json_string(&report.status),
        report.blocking_findings(),
        json_array(report.findings.iter().map(security_finding_json)),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将单条安全 finding 的严重度、状态和 remediation 编码为 JSON。
fn security_finding_json(finding: &SecurityFinding) -> String {
    format!(
        "{{\"id\":{},\"boundary\":{},\"severity\":{},\"status\":{},\"summary\":{},\"evidence\":{},\"remediation\":{}}}",
        json_string(&finding.id),
        json_string(&finding.boundary),
        json_string(finding.severity.as_str()),
        json_string(finding.status.as_str()),
        json_string(&finding.summary),
        json_array(finding.evidence.iter().map(|entry| json_string(entry))),
        json_array(finding.remediation.iter().map(|entry| json_string(entry)))
    )
}

/// 将性能基线状态与预算项编码为 JSON。
fn performance_baseline_json(report: &PerformanceBaselineReport) -> String {
    format!(
        "{{\"version\":{},\"status\":{},\"measured\":{},\"unmeasured\":{},\"over_budget\":{},\"budgets\":{},\"audit\":{}}}",
        json_string(&report.version),
        json_string(&report.status),
        report.measured_count(),
        report.unmeasured_count(),
        report.over_budget_count(),
        json_array(report.budgets.iter().map(performance_budget_json)),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将单项性能预算的阈值、观测值和状态编码为 JSON。
fn performance_budget_json(budget: &PerformanceBudget) -> String {
    let observed_ms = budget
        .observed_ms()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let observation_evidence = option_json(
        budget
            .observation
            .as_ref()
            .map(|observation| observation.evidence.as_str()),
    );
    format!(
        "{{\"component\":{},\"metric\":{},\"budget_ms\":{},\"observed_ms\":{},\"observation_kind\":{},\"status\":{},\"evidence\":{},\"observation_evidence\":{}}}",
        json_string(&budget.component),
        json_string(&budget.metric),
        budget.budget_ms,
        observed_ms,
        json_string(budget.observation_kind()),
        json_string(budget.status.as_str()),
        json_string(&budget.evidence),
        observation_evidence
    )
}

/// 将迁移指南、步骤和兼容策略编码为 JSON。
fn migration_guide_json(guide: &MigrationGuide) -> String {
    format!(
        "{{\"from_version\":{},\"to_version\":{},\"status\":{},\"breaking_changes\":{},\"steps\":{},\"compatibility_policy\":{},\"audit\":{}}}",
        json_string(&guide.from_version),
        json_string(&guide.to_version),
        json_string(&guide.status),
        json_array(guide.breaking_changes.iter().map(|entry| json_string(entry))),
        json_array(guide.steps.iter().map(migration_step_json)),
        compatibility_policy_json(&guide.compatibility_policy),
        json_array(guide.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将单个迁移步骤及是否必需编码为 JSON。
fn migration_step_json(step: &MigrationStep) -> String {
    format!(
        "{{\"id\":{},\"summary\":{},\"command\":{},\"requires_manual_review\":{}}}",
        json_string(&step.id),
        json_string(&step.summary),
        json_string(&step.command),
        step.requires_manual_review
    )
}

/// 将兼容窗口、破坏性变更和弃用策略编码为 JSON。
fn compatibility_policy_json(policy: &CompatibilityPolicy) -> String {
    format!(
        "{{\"cli_json_envelope\":{},\"exit_codes\":{},\"config_schema\":{},\"command_surface\":{},\"deprecation_window\":{},\"public_contracts\":{}}}",
        json_string(&policy.cli_json_envelope),
        json_string(&policy.exit_codes),
        json_string(&policy.config_schema),
        json_string(&policy.command_surface),
        json_string(&policy.deprecation_window),
        json_array(policy.public_contracts.iter().map(|contract| json_string(contract)))
    )
}

#[cfg(test)]
mod evidence_path_tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 创建独立目录，避免并行 identity 测试共享同一文件对象。
    fn identity_fixture() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "eva-release-evidence-identity-{}-{suffix}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    /// 锁定 production policy 拒绝与运行时/内部故障的退出码分类边界。
    fn production_blocking_error_classifier_preserves_runtime_failures() {
        for error in [
            EvaError::invalid_argument("invalid evidence"),
            EvaError::not_found("missing evidence"),
            EvaError::conflict("conflicting evidence"),
            EvaError::permission_denied("evidence denied"),
        ] {
            assert!(is_production_blocking_error(&error), "{error:?}");
        }
        for error in [
            EvaError::timeout("verification timed out"),
            EvaError::unavailable("verifier unavailable"),
            EvaError::unsupported("verifier unsupported"),
            EvaError::internal("verification failed internally"),
        ] {
            assert!(!is_production_blocking_error(&error), "{error:?}");
        }
    }

    #[test]
    /// 验证检查路径与同一已打开 handle 的平台文件身份一致。
    fn opened_handle_matches_checked_file_identity() {
        let root = identity_fixture();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("subject.bin");
        fs::write(&path, b"subject").unwrap();
        let opened = fs::File::open(&path).unwrap();

        assert!(opened_file_matches_checked_path(&path, &opened).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证另一个文件不能替代已完成路径检查的 handle。
    fn opened_handle_rejects_different_file_identity() {
        let root = identity_fixture();
        fs::create_dir_all(&root).unwrap();
        let checked_path = root.join("checked.bin");
        let opened_path = root.join("opened.bin");
        fs::write(&checked_path, b"same-size").unwrap();
        fs::write(&opened_path, b"same-size").unwrap();
        let opened = fs::File::open(&opened_path).unwrap();

        assert!(!opened_file_matches_checked_path(&checked_path, &opened).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    /// A Windows hard-link alias must compare by file identity rather than path spelling.
    fn opened_handle_accepts_windows_hard_link_identity() {
        let root = identity_fixture();
        fs::create_dir_all(&root).unwrap();
        let opened_path = root.join("opened.bin");
        let checked_path = root.join("checked.bin");
        fs::write(&opened_path, b"same-file").unwrap();
        fs::hard_link(&opened_path, &checked_path).unwrap();
        let opened = fs::File::open(&opened_path).unwrap();

        assert!(opened_file_matches_checked_path(&checked_path, &opened).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证目录内 symlink 不能把 manifest reference 指向 bundle 外部。
    fn manifest_reference_rejects_symlink_escape() {
        let root = identity_fixture();
        let bundle = root.join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        let outside = root.join("outside.bin");
        fs::write(&outside, b"outside").unwrap();
        let link = bundle.join("subject.bin");
        #[cfg(unix)]
        symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        if let Err(error) = symlink_file(&outside, &link) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                fs::remove_dir_all(root).unwrap();
                return;
            }
            panic!("failed to create test symlink: {error}");
        }

        let canonical_bundle = fs::canonicalize(&bundle).unwrap();
        let error =
            read_manifest_reference(&canonical_bundle, "subject.bin", "subject").unwrap_err();
        assert!(error.message().contains("escapes its directory"));
        fs::remove_dir_all(root).unwrap();
    }
}
