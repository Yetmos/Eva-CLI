use super::{
    join_capabilities, json_array, json_string, parse_common_options, required_option,
    success_envelope, trace_for, write_command_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_adapter::AdapterRuntime;
use eva_config::{load_project_config, AdapterTransport};
use eva_core::{AdapterId, EvaError};
use eva_mcp::{InMemoryMcpClient, McpAllowlist, McpProbeReport};
use eva_observability::TraceFields;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum McpCommand {
    List(CommonOptions),
    Probe(McpProbeOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpProbeOptions {
    common: CommonOptions,
    adapter_id: String,
    tool: Option<String>,
}

pub(super) fn parse_mcp_command(args: &[String]) -> Result<McpCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing mcp subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(McpCommand::List(parse_common_options(rest)?)),
        "probe" => Ok(McpCommand::Probe(parse_mcp_probe_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown mcp subcommand").with_context("subcommand", value))
        }
    }
}

fn parse_mcp_probe_options(args: &[String]) -> Result<McpProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut tool = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            "--tool" => {
                index += 1;
                tool = Some(required_option(args, index, "tool option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(McpProbeOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id: adapter_id.unwrap_or_else(|| "github-mcp".to_owned()),
        tool,
    })
}

pub(super) fn execute_mcp<W, E>(
    command: McpCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        McpCommand::List(options) => {
            let trace = trace_for("cli.mcp.list");
            match load_project_config(&options.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
            {
                Ok(runtime) => {
                    write_mcp_list(stdout, options.output, &runtime, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "mcp.list", &error, &trace)
                }
            }
        }
        McpCommand::Probe(options) => {
            let trace = trace_for("cli.mcp.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
                .and_then(|runtime| probe_mcp_runtime(&runtime, &options))
            {
                Ok(report) => {
                    write_mcp_probe(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.common.output, "mcp.probe", &error, &trace)
                }
            }
        }
    }
}

fn probe_mcp_runtime(
    runtime: &AdapterRuntime,
    options: &McpProbeOptions,
) -> Result<McpProbeReport, EvaError> {
    let adapter_id = AdapterId::parse(&options.adapter_id)?;
    let handle = runtime.registry().get(&adapter_id).ok_or_else(|| {
        EvaError::not_found("MCP adapter does not exist")
            .with_context("adapter_id", adapter_id.as_str())
    })?;
    if handle.transport != AdapterTransport::Mcp {
        return Err(
            EvaError::invalid_argument("adapter is not an MCP transport")
                .with_context("adapter_id", handle.id.as_str())
                .with_context("transport", handle.transport.as_str()),
        );
    }
    let tool = options
        .tool
        .clone()
        .or_else(|| handle.mcp_tools.first().cloned())
        .ok_or_else(|| {
            EvaError::not_found("MCP adapter has no allowlisted tools")
                .with_context("adapter_id", handle.id.as_str())
        })?;
    let client = InMemoryMcpClient::new(
        handle.id.clone(),
        McpAllowlist::from_tools(handle.mcp_tools.iter().cloned())?,
    );
    Ok(client.probe_tool(&tool))
}

fn write_mcp_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    runtime: &AdapterRuntime,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva MCP adapters").map_err(write_error_kind)?;
            for handle in runtime
                .list()
                .into_iter()
                .filter(|handle| handle.transport == AdapterTransport::Mcp)
            {
                writeln!(
                    writer,
                    "  - {} tools={} capabilities={}",
                    handle.id,
                    handle.mcp_tools.join(","),
                    join_capabilities(&handle.capabilities)
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("mcp.list", EXIT_OK, &mcp_list_json(runtime), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_mcp_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &McpProbeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "MCP probe").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", report.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "tool: {}", report.tool).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status.as_str()).map_err(write_error_kind)?;
            writeln!(writer, "detail: {}", report.message).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("mcp.probe", EXIT_OK, &mcp_probe_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn mcp_list_json(runtime: &AdapterRuntime) -> String {
    let entries = runtime
        .list()
        .into_iter()
        .filter(|handle| handle.transport == AdapterTransport::Mcp)
        .map(|handle| {
            format!(
                "{{\"adapter_id\":{},\"tools\":{},\"capabilities\":{},\"enabled\":{}}}",
                json_string(handle.id.as_str()),
                json_array(handle.mcp_tools.iter().map(|tool| json_string(tool))),
                json_array(
                    handle
                        .capabilities
                        .iter()
                        .map(|capability| json_string(capability.as_str()))
                ),
                handle.enabled
            )
        });
    format!("{{\"mcp_adapters\":{}}}", json_array(entries))
}

fn mcp_probe_json(report: &McpProbeReport) -> String {
    format!(
        "{{\"adapter_id\":{},\"tool\":{},\"status\":{},\"message\":{}}}",
        json_string(report.adapter_id.as_str()),
        json_string(&report.tool),
        json_string(report.status.as_str()),
        json_string(&report.message)
    )
}
