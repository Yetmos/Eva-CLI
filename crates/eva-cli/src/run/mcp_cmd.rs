//! MCP Adapter 列表与 allowlist 探测子命令；不会启动或调用真实外部服务器。

use super::{
    join_capabilities, json_array, json_string, parse_common_options, required_option,
    success_envelope, trace_for, write_command_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_adapter::AdapterRuntime;
use eva_config::{load_project_config, AdapterTransport};
use eva_core::{AdapterId, EvaError};
use eva_mcp::{
    compatibility::McpCompatibilityMeasurement, InMemoryMcpClient, McpAllowlist, McpProbeReport,
};
use eva_observability::TraceFields;
use eva_storage::atomic_write;
use std::io::Write;
use std::path::PathBuf;

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
    /// Run the explicit, local compatibility measurement.
    CompatibilityMeasure(McpCompatibilityMeasureOptions),
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

#[derive(Debug, Clone, PartialEq, Eq)]
/// Options for the explicit compatibility measurement producer.
pub(super) struct McpCompatibilityMeasureOptions {
    /// Human-readable or stable JSON display output.
    output: OutputFormat,
    /// Optional canonical subject path for a later W0 evidence capture.
    subject_output: Option<PathBuf>,
}

/// 解析 `mcp list|probe`，未知子命令返回不支持错误。
pub(super) fn parse_mcp_command(args: &[String]) -> Result<McpCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing mcp subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(McpCommand::List(parse_common_options(rest)?)),
        "probe" => Ok(McpCommand::Probe(parse_mcp_probe_options(rest)?)),
        "compatibility" => parse_mcp_compatibility_command(rest),
        value => {
            Err(EvaError::unsupported("unknown mcp subcommand").with_context("subcommand", value))
        }
    }
}

/// Parse the deliberately explicit `mcp compatibility measure` command.
fn parse_mcp_compatibility_command(args: &[String]) -> Result<McpCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing mcp compatibility subcommand"))?;
    match subcommand.as_str() {
        "measure" => Ok(McpCommand::CompatibilityMeasure(
            parse_mcp_compatibility_measure_options(rest)?,
        )),
        value => Err(
            EvaError::unsupported("unknown mcp compatibility subcommand")
                .with_context("subcommand", value),
        ),
    }
}

/// Parse display and canonical-subject options without accepting project configuration options.
fn parse_mcp_compatibility_measure_options(
    args: &[String],
) -> Result<McpCompatibilityMeasureOptions, EvaError> {
    let mut output = OutputFormat::Text;
    let mut subject_output = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--output" | "-o" => {
                index += 1;
                output = OutputFormat::parse(required_option(args, index, "output option")?)?;
            }
            "--subject-output" => {
                index += 1;
                let value = required_option(args, index, "subject output option")?;
                let path = PathBuf::from(value);
                if value.is_empty() || path.file_name().is_none() {
                    return Err(EvaError::invalid_argument(
                        "MCP compatibility subject output must name a file",
                    ));
                }
                subject_output = Some(path);
            }
            unknown => {
                return Err(EvaError::unsupported("unknown option").with_context("option", unknown));
            }
        }
        index += 1;
    }

    Ok(McpCompatibilityMeasureOptions {
        output,
        subject_output,
    })
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
        McpCommand::CompatibilityMeasure(options) => {
            let trace = trace_for("cli.mcp.compatibility.measure");
            match measure_mcp_compatibility(&options) {
                Ok(measurement) => {
                    write_mcp_compatibility_measurement(
                        stdout,
                        options.output,
                        &measurement,
                        options.subject_output.is_some(),
                        &trace,
                    )?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.output,
                    "mcp.compatibility.measure",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// Run the sealed producer and atomically persist its canonical subject when requested.
fn measure_mcp_compatibility(
    options: &McpCompatibilityMeasureOptions,
) -> Result<McpCompatibilityMeasurement, EvaError> {
    let measurement = McpCompatibilityMeasurement::measure_loopback()?;
    if let Some(path) = &options.subject_output {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .map_err(|error| {
                    EvaError::internal("failed to resolve MCP compatibility subject path")
                        .with_context("io_error", error.to_string())
                })?
                .join(path)
        };
        atomic_write(&path, measurement.subject_bytes()).map_err(|error| {
            EvaError::internal("failed to persist MCP compatibility subject")
                .with_context("io_error", error.to_string())
        })?;
    }
    Ok(measurement)
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

/// Display only path-free facts and digests; the canonical subject remains a separate file.
fn write_mcp_compatibility_measurement<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    measurement: &McpCompatibilityMeasurement,
    subject_written: bool,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "MCP compatibility measurement").map_err(write_error_kind)?;
            writeln!(
                writer,
                "server: {}@{}",
                measurement.server_name(),
                measurement.server_version()
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "protocol: {}", measurement.protocol_version())
                .map_err(write_error_kind)?;
            writeln!(writer, "transport: {}", measurement.transport().as_str())
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "tls: completed={} peer={} protocol={} handshakes={}",
                measurement.tls().handshake_completed(),
                measurement.tls().peer_name(),
                measurement.tls().protocol(),
                measurement.tls().handshake_count()
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "tool: {}", measurement.tool_name()).map_err(write_error_kind)?;
            writeln!(
                writer,
                "schema: {} bytes={}",
                measurement.schema().sha256(),
                measurement.schema().bytes()
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "output: {} bytes={}",
                measurement.output().sha256(),
                measurement.output().bytes()
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "abort: socket_closed={} session_deleted={} reader_joined={} sessions_after={} readers_after={} cleanup_pending_after={}",
                measurement.abort().socket_closed(),
                measurement.abort().session_deleted(),
                measurement.abort().reader_joined(),
                measurement.abort().sessions_after(),
                measurement.abort().readers_after(),
                measurement.abort().cleanup_pending_after()
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "subject: {} bytes={} written={subject_written}",
                measurement.canonical_digest(),
                measurement.subject_bytes().len()
            )
            .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "mcp.compatibility.measure",
                EXIT_OK,
                &mcp_compatibility_measurement_json(measurement, subject_written),
                trace,
            )
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

/// Encode the public measurement receipt without raw protocol payloads or local endpoint data.
fn mcp_compatibility_measurement_json(
    measurement: &McpCompatibilityMeasurement,
    subject_written: bool,
) -> String {
    format!(
        concat!(
            "{{",
            "\"evidence_kind\":\"measurement\",",
            "\"server\":{{\"name\":{},\"version\":{}}},",
            "\"protocol_version\":{},",
            "\"transport\":{},",
            "\"tls\":{{\"handshake_completed\":{},\"peer_name\":{},\"protocol\":{},\"handshake_count\":{}}},",
            "\"tool_name\":{},",
            "\"schema\":{{\"sha256\":{},\"bytes\":{}}},",
            "\"output\":{{\"sha256\":{},\"bytes\":{}}},",
            "\"observations\":{{\"initialize_server_info\":{},\"tools_list_schema\":{},\"tools_call_result\":{}}},",
            "\"abort\":{{\"socket_closed\":{},\"session_deleted\":{},\"reader_joined\":{},\"sessions_after\":{},\"readers_after\":{},\"cleanup_pending_after\":{}}},",
            "\"subject\":{{\"sha256\":{},\"bytes\":{},\"written\":{}}}",
            "}}"
        ),
        json_string(measurement.server_name()),
        json_string(measurement.server_version()),
        json_string(measurement.protocol_version()),
        json_string(measurement.transport().as_str()),
        measurement.tls().handshake_completed(),
        json_string(measurement.tls().peer_name()),
        json_string(measurement.tls().protocol()),
        measurement.tls().handshake_count(),
        json_string(measurement.tool_name()),
        json_string(measurement.schema().sha256()),
        measurement.schema().bytes(),
        json_string(measurement.output().sha256()),
        measurement.output().bytes(),
        measurement.observations().initialize_server_info(),
        measurement.observations().tools_list_schema(),
        measurement.observations().tools_call_result(),
        measurement.abort().socket_closed(),
        measurement.abort().session_deleted(),
        measurement.abort().reader_joined(),
        measurement.abort().sessions_after(),
        measurement.abort().readers_after(),
        measurement.abort().cleanup_pending_after(),
        json_string(measurement.canonical_digest()),
        measurement.subject_bytes().len(),
        subject_written,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn compatibility_measure_requires_the_explicit_measure_subcommand() {
        assert!(parse_mcp_command(&strings(&["compatibility"])).is_err());
        assert!(parse_mcp_command(&strings(&["compatibility", "fixture"])).is_err());
        assert!(
            parse_mcp_command(&strings(&["compatibility", "measure", "--project", ".",])).is_err()
        );
    }

    #[test]
    fn compatibility_measure_atomically_writes_a_canonical_subject() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "eva-mcp-compatibility-cli-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let subject = root.join("measurement.evidence");
        let command = parse_mcp_command(&strings(&[
            "compatibility",
            "measure",
            "--subject-output",
            subject.to_str().unwrap(),
            "--output",
            "json",
        ]))
        .unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = execute_mcp(command, &mut stdout, &mut stderr).unwrap();
        let stdout = String::from_utf8(stdout).unwrap();

        assert_eq!(exit_code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        assert!(stderr.is_empty());
        let manifest = fs::read_to_string(&subject).unwrap();
        assert!(stdout.contains("\"command\":\"mcp.compatibility.measure\""));
        assert!(stdout.contains("\"evidence_kind\":\"measurement\""));
        assert!(stdout.contains("\"written\":true"));
        assert!(!stdout.contains("loopback-ok"));
        assert!(!stdout.contains(subject.to_string_lossy().as_ref()));
        assert!(manifest.starts_with("format=eva.mcp-compatibility.v1\n"));
        assert!(manifest.contains("evidence_kind=measurement\n"));
        assert!(manifest.contains("abort_cleanup_pending_after=0\n"));
        assert_eq!(manifest.lines().count(), 24);
        assert!(manifest.ends_with('\n'));

        fs::remove_dir_all(root).unwrap();
    }
}
