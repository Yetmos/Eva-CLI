use super::{
    json_array, json_string, parse_common_options, required_option, success_envelope, trace_for,
    write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_CONFIG, EXIT_OK,
    EXIT_POLICY, EXIT_RUNTIME_UNAVAILABLE,
};
use eva_core::EvaError;
use eva_observability::TraceFields;
use eva_release::{
    CompatibilityPolicy, MigrationGuide, MigrationStep, PerformanceBaselineReport,
    PerformanceBudget, PlatformReadiness, ReleaseArtifactEvidence, ReleaseBenchmarkEvidence,
    ReleaseDistributionEvidence, ReleaseGate, ReleaseHardeningService, ReleaseReadinessReport,
    ReleaseSecurityScanEvidence, SecurityFinding, SecurityReviewReport, StabilityScenario,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReleaseCommand {
    Check(ReleaseCheckOptions),
    Security(CommonOptions),
    Perf(ReleasePerfOptions),
    Migration(ReleaseMigrationOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleaseCheckOptions {
    common: CommonOptions,
    target: String,
    artifact_evidence: Option<PathBuf>,
    distribution_evidence: Option<PathBuf>,
    security_scan_evidence: Option<PathBuf>,
    benchmark_evidence: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleasePerfOptions {
    common: CommonOptions,
    benchmark_evidence: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleaseMigrationOptions {
    common: CommonOptions,
    from_version: String,
    to_version: String,
}

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
        value => {
            Err(EvaError::unsupported("unknown release subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_release_check_options(args: &[String]) -> Result<ReleaseCheckOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut target = "all".to_owned();
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
    Ok(ReleaseCheckOptions {
        common: parse_common_options(&passthrough)?,
        target,
        artifact_evidence,
        distribution_evidence,
        security_scan_evidence,
        benchmark_evidence,
    })
}

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
        ReleaseCommand::Check(options) => {
            let trace = trace_for("cli.release.check");
            let report = (|| {
                let artifact_evidence = options
                    .artifact_evidence
                    .as_ref()
                    .map(|path| read_release_artifact_evidence(path))
                    .transpose()?;
                let distribution_evidence = options
                    .distribution_evidence
                    .as_ref()
                    .map(|path| read_release_distribution_evidence(path))
                    .transpose()?;
                let security_scan_evidence = options
                    .security_scan_evidence
                    .as_ref()
                    .map(|path| read_release_security_scan_evidence(path))
                    .transpose()?;
                let benchmark_evidence = options
                    .benchmark_evidence
                    .as_ref()
                    .map(|path| read_release_benchmark_evidence(path))
                    .transpose()?;
                service.readiness_with_release_evidence(
                    &options.target,
                    artifact_evidence.as_ref(),
                    distribution_evidence.as_ref(),
                    security_scan_evidence.as_ref(),
                    benchmark_evidence.as_ref(),
                )
            })();
            match report {
                Ok(report) => {
                    write_release_check(stdout, options.common.output, &report, &trace)?;
                    Ok(if report.blocking_count() == 0 {
                        EXIT_OK
                    } else {
                        EXIT_CONFIG
                    })
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "release.check",
                    &error,
                    &trace,
                ),
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
                    write_release_perf(stdout, options.common.output, &report, &trace)?;
                    Ok(
                        if report.status == "within_budget" && report.over_budget_count() == 0 {
                            EXIT_OK
                        } else {
                            EXIT_RUNTIME_UNAVAILABLE
                        },
                    )
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

fn write_release_check<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &ReleaseReadinessReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Release readiness").map_err(write_error_kind)?;
            writeln!(writer, "version: {}", report.version).map_err(write_error_kind)?;
            writeln!(writer, "target: {}", report.target).map_err(write_error_kind)?;
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
            success_envelope("release.check", EXIT_OK, &release_check_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

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

fn write_release_perf<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &PerformanceBaselineReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Performance baseline").map_err(write_error_kind)?;
            writeln!(writer, "version: {}", report.version).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "budgets: {}", report.budgets.len()).map_err(write_error_kind)?;
            writeln!(writer, "over_budget: {}", report.over_budget_count())
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "release.perf",
                EXIT_OK,
                &performance_baseline_json(report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

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

fn release_check_json(report: &ReleaseReadinessReport) -> String {
    format!(
        "{{\"version\":{},\"status\":{},\"target\":{},\"blocking_gates\":{},\"warning_gates\":{},\"platforms\":{},\"stability\":{},\"gates\":{},\"closure\":{},\"audit\":{}}}",
        json_string(&report.version),
        json_string(&report.status),
        json_string(&report.target),
        report.blocking_count(),
        report.warning_count(),
        json_array(report.platforms.iter().map(platform_readiness_json)),
        json_array(report.stability.iter().map(stability_scenario_json)),
        json_array(report.gates.iter().map(release_gate_json)),
        v1x_closure_json(report),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

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

fn release_gate_json(gate: &ReleaseGate) -> String {
    format!(
        "{{\"id\":{},\"domain\":{},\"status\":{},\"required\":{},\"summary\":{},\"evidence\":{},\"remediation\":{}}}",
        json_string(&gate.id),
        json_string(&gate.domain),
        json_string(gate.status.as_str()),
        gate.required,
        json_string(&gate.summary),
        json_array(gate.evidence.iter().map(|entry| json_string(entry))),
        json_array(gate.remediation.iter().map(|entry| json_string(entry)))
    )
}

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

fn performance_baseline_json(report: &PerformanceBaselineReport) -> String {
    format!(
        "{{\"version\":{},\"status\":{},\"over_budget\":{},\"budgets\":{},\"audit\":{}}}",
        json_string(&report.version),
        json_string(&report.status),
        report.over_budget_count(),
        json_array(report.budgets.iter().map(performance_budget_json)),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn performance_budget_json(budget: &PerformanceBudget) -> String {
    format!(
        "{{\"component\":{},\"metric\":{},\"budget_ms\":{},\"observed_ms\":{},\"status\":{},\"evidence\":{}}}",
        json_string(&budget.component),
        json_string(&budget.metric),
        budget.budget_ms,
        budget.observed_ms,
        json_string(budget.status.as_str()),
        json_string(&budget.evidence)
    )
}

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

fn migration_step_json(step: &MigrationStep) -> String {
    format!(
        "{{\"id\":{},\"summary\":{},\"command\":{},\"requires_manual_review\":{}}}",
        json_string(&step.id),
        json_string(&step.summary),
        json_string(&step.command),
        step.requires_manual_review
    )
}

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
