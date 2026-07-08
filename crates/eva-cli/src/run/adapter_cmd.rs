use super::{
    join_capabilities, json_array, json_string, parse_common_options, required_option,
    success_envelope, trace_for, write_command_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_adapter::{AdapterProbeReport, AdapterRuntime};
use eva_config::load_project_config;
use eva_core::{AdapterId, CapabilityName, EvaError};
use eva_observability::TraceFields;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AdapterCommand {
    List(CommonOptions),
    Probe(AdapterProbeOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AdapterProbeOptions {
    common: CommonOptions,
    adapter_id: Option<String>,
    capability: Option<String>,
    provider: Option<String>,
}

pub(super) fn parse_adapter_command(args: &[String]) -> Result<AdapterCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing adapter subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(AdapterCommand::List(parse_common_options(rest)?)),
        "probe" => Ok(AdapterCommand::Probe(parse_adapter_probe_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown adapter subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_adapter_probe_options(args: &[String]) -> Result<AdapterProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut capability = None;
    let mut provider = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            "--capability" => {
                index += 1;
                capability = Some(required_option(args, index, "capability option")?.clone());
            }
            "--provider" => {
                index += 1;
                provider = Some(required_option(args, index, "provider option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(AdapterProbeOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
        capability,
        provider,
    })
}

pub(super) fn execute_adapter<W, E>(
    command: AdapterCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        AdapterCommand::List(options) => {
            let trace = trace_for("cli.adapter.list");
            match load_project_config(&options.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
            {
                Ok(runtime) => {
                    write_adapter_list(stdout, options.output, &runtime, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "adapter.list", &error, &trace)
                }
            }
        }
        AdapterCommand::Probe(options) => {
            let trace = trace_for("cli.adapter.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
                .and_then(|runtime| probe_adapter_runtime(&runtime, &options))
            {
                Ok(report) => {
                    write_adapter_probe(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "adapter.probe",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn probe_adapter_runtime(
    runtime: &AdapterRuntime,
    options: &AdapterProbeOptions,
) -> Result<AdapterProbeReport, EvaError> {
    if let Some(adapter_id) = &options.adapter_id {
        return runtime.probe_adapter(&AdapterId::parse(adapter_id)?);
    }
    let capability = options
        .capability
        .as_deref()
        .map(CapabilityName::parse)
        .transpose()?
        .unwrap_or_else(|| CapabilityName::parse("workflow.code_review").unwrap());
    let provider = options
        .provider
        .as_deref()
        .map(AdapterId::parse)
        .transpose()?;
    runtime.probe_capability(capability, provider)
}

fn write_adapter_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    runtime: &AdapterRuntime,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva adapters").map_err(write_error_kind)?;
            for handle in runtime.list() {
                writeln!(
                    writer,
                    "  - {} transport={} enabled={} health={} capabilities={}",
                    handle.id,
                    handle.transport.as_str(),
                    handle.enabled,
                    handle.health().as_str(),
                    join_capabilities(&handle.capabilities)
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("adapter.list", EXIT_OK, &adapter_list_json(runtime), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_adapter_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &AdapterProbeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Adapter probe").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", report.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "transport: {}", report.transport.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "capabilities: {}",
                join_capabilities(&report.capabilities)
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "detail: {}", report.detail).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("adapter.probe", EXIT_OK, &adapter_probe_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn adapter_list_json(runtime: &AdapterRuntime) -> String {
    let entries = runtime.list().into_iter().map(|handle| {
        format!(
            "{{\"id\":{},\"name\":{},\"version\":{},\"enabled\":{},\"health\":{},\"transport\":{},\"capabilities\":{},\"mcp_tools\":{},\"skill_id\":{},\"source_path\":{}}}",
            json_string(handle.id.as_str()),
            json_string(&handle.name),
            json_string(&handle.version),
            handle.enabled,
            json_string(handle.health().as_str()),
            json_string(handle.transport.as_str()),
            json_array(handle.capabilities.iter().map(|capability| json_string(capability.as_str()))),
            json_array(handle.mcp_tools.iter().map(|tool| json_string(tool))),
            super::option_json(handle.skill_name()),
            json_string(&handle.source_path)
        )
    });
    format!("{{\"adapters\":{}}}", json_array(entries))
}

fn adapter_probe_json(report: &AdapterProbeReport) -> String {
    format!(
        "{{\"adapter_id\":{},\"transport\":{},\"status\":{},\"capabilities\":{},\"detail\":{}}}",
        json_string(report.adapter_id.as_str()),
        json_string(report.transport.as_str()),
        json_string(&report.status),
        json_array(
            report
                .capabilities
                .iter()
                .map(|capability| json_string(capability.as_str()))
        ),
        json_string(&report.detail)
    )
}
