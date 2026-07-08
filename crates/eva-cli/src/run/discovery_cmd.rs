use super::{
    json_array, json_string, option_json, parse_common_options, success_envelope, trace_for,
    write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::load_project_config;
use eva_core::EvaError;
use eva_discovery::{DiscoveryScanReport, DiscoveryService, DiscoverySourceReport};
use eva_observability::TraceFields;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DiscoveryCommand {
    Scan(CommonOptions),
}

pub(super) fn parse_discovery_command(args: &[String]) -> Result<DiscoveryCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing discovery subcommand"))?;
    match subcommand.as_str() {
        "scan" => Ok(DiscoveryCommand::Scan(parse_common_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown discovery subcommand")
                .with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_discovery<W, E>(
    command: DiscoveryCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        DiscoveryCommand::Scan(options) => {
            let trace = trace_for("cli.discovery.scan");
            match load_project_config(&options.project_root) {
                Ok(project) => {
                    let mut service = DiscoveryService::new();
                    let report = service.scan_project(&project);
                    write_discovery_scan(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "discovery.scan", &error, &trace)
                }
            }
        }
    }
}

fn write_discovery_scan<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &DiscoveryScanReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva discovery candidates").map_err(write_error_kind)?;
            for candidate in &report.candidates {
                writeln!(
                    writer,
                    "  - {} kind={} source={} trust={} handle_granted={}",
                    candidate.id,
                    candidate.kind.as_str(),
                    candidate.source,
                    candidate.trust.as_str(),
                    candidate.handle_granted
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "Sources").map_err(write_error_kind)?;
            for source in &report.source_reports {
                let rejected_reason = source.rejected_reason.as_deref().unwrap_or("-");
                writeln!(
                    writer,
                    "  - {} status={} cache_key={} timeout_ms={} elapsed_ms={} candidates={} rejected_reason={}",
                    source.source_id,
                    source.status,
                    source.cache_key,
                    source.timeout_ms,
                    source.elapsed_ms,
                    source.candidates.len(),
                    rejected_reason
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "discovery.scan",
                EXIT_OK,
                &discovery_scan_json(report),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn discovery_scan_json(report: &DiscoveryScanReport) -> String {
    let candidates = &report.candidates;
    let entries = candidates.iter().map(|candidate| {
        format!(
            "{{\"id\":{},\"kind\":{},\"source\":{},\"trust\":{},\"adapter_id\":{},\"capability\":{},\"handle_granted\":{},\"rejected_reason\":{}}}",
            json_string(&candidate.id),
            json_string(candidate.kind.as_str()),
            json_string(&candidate.source),
            json_string(candidate.trust.as_str()),
            option_json(candidate.adapter_id.as_ref().map(|id| id.as_str())),
            option_json(candidate.capability.as_ref().map(|capability| capability.as_str())),
            candidate.handle_granted,
            option_json(candidate.rejected_reason.as_deref())
        )
    });
    let source_reports = report
        .source_reports
        .iter()
        .map(discovery_source_report_json);
    format!(
        "{{\"candidate_count\":{},\"candidates\":{},\"source_report_count\":{},\"source_reports\":{}}}",
        candidates.len(),
        json_array(entries),
        report.source_reports.len(),
        json_array(source_reports)
    )
}

fn discovery_source_report_json(report: &DiscoverySourceReport) -> String {
    let rejected_count = report
        .candidates
        .iter()
        .filter(|candidate| candidate.rejected_reason.is_some())
        .count();
    format!(
        "{{\"source_id\":{},\"cache_key\":{},\"status\":{},\"timeout_ms\":{},\"elapsed_ms\":{},\"candidate_count\":{},\"rejected_candidate_count\":{},\"error\":{},\"rejected_reason\":{}}}",
        json_string(&report.source_id),
        json_string(&report.cache_key),
        json_string(&report.status),
        report.timeout_ms,
        report.elapsed_ms,
        report.candidates.len(),
        rejected_count,
        option_json(report.error.as_deref()),
        option_json(report.rejected_reason.as_deref())
    )
}
