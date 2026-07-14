//! MCP Adapter 列表与 allowlist 探测子命令；不会启动或调用真实外部服务器。

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
/// MCP 子命令及其已解析选项。
pub(super) enum McpCommand {
    /// 列出 MCP transport Adapter 及允许的工具。
    List(
        /// MCP 列表命令共享的项目根目录与输出格式。
        CommonOptions,
    ),
    /// 在内存客户端中探测一个 allowlist 工具。
    Probe(
        /// 已解析的 Adapter、工具名、载荷与公共选项。
        McpProbeOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// MCP 工具探测选项。
pub(super) struct McpProbeOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 必须解析为 MCP transport 的 Adapter ID。
    adapter_id: String,
    /// 可选工具名；缺省时使用清单中的第一个工具。
    tool: Option<String>,
}

/// 解析 `mcp list|probe`，未知子命令返回不支持错误。
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

/// 解析 MCP 探测选项，并保留 `github-mcp` 作为兼容默认 Adapter。
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

/// 加载 Adapter 运行时并执行只读 MCP 命令。
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

/// 在清单 allowlist 边界内探测工具。
///
/// 先验证 Adapter 存在且 transport 为 MCP，再从显式工具或 allowlist 首项选取目标；
/// `InMemoryMcpClient` 只验证授权与协议数据，不会产生外部网络或进程副作用。
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

/// 输出 MCP Adapter、allowlist 工具和 capability 集合。
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

/// 输出单个 MCP 工具探测结果。
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

/// 将 MCP transport 句柄过滤并编码为稳定 JSON。
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

/// 将 MCP 探测报告编码为 JSON。
fn mcp_probe_json(report: &McpProbeReport) -> String {
    format!(
        "{{\"adapter_id\":{},\"tool\":{},\"status\":{},\"message\":{}}}",
        json_string(report.adapter_id.as_str()),
        json_string(&report.tool),
        json_string(report.status.as_str()),
        json_string(&report.message)
    )
}
