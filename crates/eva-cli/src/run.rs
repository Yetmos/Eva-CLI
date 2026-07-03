//! CLI command parsing, output envelopes, and process exit mapping.

use crate::doctor::{doctor_project, CheckStatus, DoctorReport};
use crate::inspect::{inspect_project, InspectReport};
use eva_config::{load_project_config, schema_paths, ProjectConfig};
use eva_core::{ErrorKind, EvaError, InvokeStatus};
use eva_observability::{SpanId, TraceFields};
use eva_runtime::{BasicRunOptions, BasicRunReport, RuntimeBuilder};
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "parse Eva CLI commands and map results to stable output and exit codes";

const EXIT_OK: i32 = 0;
const EXIT_INTERNAL: i32 = 1;
const EXIT_CONFIG: i32 = 2;
const EXIT_POLICY: i32 = 3;
const EXIT_RUNTIME_UNAVAILABLE: i32 = 4;
const EXIT_USAGE: i32 = 64;

/// Process entry point for the root binary shim.
pub fn run() {
    let exit_code = run_with_args(env::args_os().skip(1), &mut io::stdout(), &mut io::stderr());
    std::process::exit(exit_code);
}

/// Testable CLI entry point.
pub fn run_with_args<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    let command = match parse_command(args) {
        Ok(Command::Help) => {
            let _ = stdout.write_all(help_text().as_bytes());
            return EXIT_OK;
        }
        Ok(command) => command,
        Err(error) => {
            let trace = trace_for("cli.parse");
            let exit_code = EXIT_USAGE;
            let _ = write_error(
                stderr,
                OutputFormat::Text,
                "parse",
                exit_code,
                &error,
                &trace,
            );
            return exit_code;
        }
    };

    match execute(command, stdout, stderr) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            let trace = trace_for("cli.execute");
            let exit_code = exit_code_for_error(&error);
            let _ = write_error(
                stderr,
                OutputFormat::Text,
                "execute",
                exit_code,
                &error,
                &trace,
            );
            exit_code
        }
    }
}

fn execute<W, E>(command: Command, stdout: &mut W, stderr: &mut E) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        Command::Help => unreachable!("help is handled before execution"),
        Command::Doctor(options) => {
            let trace = trace_for("cli.doctor");
            let report = doctor_project(&options.project_root);
            let exit_code = if report.has_errors() {
                EXIT_CONFIG
            } else {
                EXIT_OK
            };
            write_doctor(stdout, options.output, exit_code, &report, &trace)?;
            Ok(exit_code)
        }
        Command::ConfigValidate(options) => {
            let trace = trace_for("cli.config.validate");
            match load_project_config(&options.project_root) {
                Ok(project) => {
                    let report = ValidationReport::from_project(&project);
                    write_validation(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = EXIT_CONFIG;
                    write_error(
                        stderr,
                        options.output,
                        "config.validate",
                        exit_code,
                        &error,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        Command::Inspect(options) => {
            let trace = trace_for("cli.inspect");
            match load_project_config(&options.project_root)
                .and_then(|project| inspect_project(&project))
            {
                Ok(report) => {
                    write_inspect(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(stderr, options.output, "inspect", exit_code, &error, &trace)?;
                    Ok(exit_code)
                }
            }
        }
        Command::Run(options) => {
            let trace = trace_for("cli.run");
            match execute_run(options, stdout, stderr, &trace) {
                Ok(exit_code) => Ok(exit_code),
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(stderr, OutputFormat::Text, "run", exit_code, &error, &trace)?;
                    Ok(exit_code)
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Doctor(CommonOptions),
    ConfigValidate(CommonOptions),
    Inspect(CommonOptions),
    Run(RunOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommonOptions {
    project_root: PathBuf,
    output: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunOptions {
    common: CommonOptions,
    example: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationReport {
    project_root: String,
    eva_config_path: String,
    environment: String,
    hot_reload: bool,
    agents_total: usize,
    agents_enabled: usize,
    adapters_total: usize,
    adapters_enabled: usize,
    capabilities_total: usize,
    capabilities_enabled: usize,
    policies_total: usize,
    routes_total: usize,
    schema_files: Vec<String>,
}

fn parse_command<I>(args: I) -> Result<Command, EvaError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| EvaError::invalid_argument("command-line argument is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Command::Help);
    }

    match args[0].as_str() {
        "help" => Ok(Command::Help),
        "doctor" => Ok(Command::Doctor(parse_common_options(&args[1..])?)),
        "config" => parse_config_command(&args[1..]),
        "inspect" => Ok(Command::Inspect(parse_inspect_options(&args[1..])?)),
        "run" => Ok(Command::Run(parse_run_options(&args[1..])?)),
        unknown => Err(EvaError::unsupported("unknown command").with_context("command", unknown)),
    }
}

fn parse_run_options(args: &[String]) -> Result<RunOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut example = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--example" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for example option")
                })?;
                example = Some(value.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(RunOptions {
        common: parse_common_options(&passthrough)?,
        example,
    })
}

fn execute_run<W, E>(
    options: RunOptions,
    stdout: &mut W,
    stderr: &mut E,
    trace: &TraceFields,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match options.example.as_deref() {
        Some("basic") => {
            let project_root = options.common.project_root.join("examples").join("basic");
            match load_project_config(&project_root).and_then(|project| {
                let runtime = RuntimeBuilder::in_memory_v04().build(&project)?;
                runtime
                    .run_basic(&project, BasicRunOptions::default())
                    .map(|report| (project, runtime, report))
            }) {
                Ok((_project, _runtime, report)) => {
                    write_run(stdout, options.common.output, &report, trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(
                        stderr,
                        options.common.output,
                        "run",
                        exit_code,
                        &error,
                        trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        Some(example) => {
            let error = EvaError::unsupported("unknown run example")
                .with_context("example", example)
                .with_context("supported", "basic");
            let exit_code = EXIT_USAGE;
            write_error(
                stderr,
                options.common.output,
                "run",
                exit_code,
                &error,
                trace,
            )?;
            Ok(exit_code)
        }
        None => {
            let error = EvaError::unsupported("eva run requires an example in V0.4")
                .with_context("suggestion", "use `eva run --example basic`");
            let exit_code = EXIT_RUNTIME_UNAVAILABLE;
            write_error(
                stderr,
                options.common.output,
                "run",
                exit_code,
                &error,
                trace,
            )?;
            Ok(exit_code)
        }
    }
}

fn parse_config_command(args: &[String]) -> Result<Command, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing config subcommand"))?;
    match subcommand.as_str() {
        "validate" => Ok(Command::ConfigValidate(parse_common_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown config subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_inspect_options(args: &[String]) -> Result<CommonOptions, EvaError> {
    let filtered = args
        .iter()
        .filter(|arg| {
            !matches!(
                arg.as_str(),
                "all"
                    | "config"
                    | "runtime"
                    | "routes"
                    | "policy"
                    | "agents"
                    | "adapters"
                    | "capabilities"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    parse_common_options(&filtered)
}

fn parse_common_options(args: &[String]) -> Result<CommonOptions, EvaError> {
    let mut project_root = env::current_dir().map_err(|error| {
        EvaError::internal("failed to read current directory")
            .with_context("io_error", error.to_string())
    })?;
    let mut output = OutputFormat::Text;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--project" | "--project-root" | "-p" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for project option")
                })?;
                project_root = PathBuf::from(value);
            }
            "--output" | "-o" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for output option"))?;
                output = OutputFormat::parse(value)?;
            }
            unknown => {
                return Err(EvaError::unsupported("unknown option").with_context("option", unknown));
            }
        }
        index += 1;
    }

    Ok(CommonOptions {
        project_root,
        output,
    })
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "text" | "human" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err(EvaError::unsupported("unsupported output format")
                .with_context("output", value)
                .with_context("supported", "text,json")),
        }
    }
}

impl ValidationReport {
    fn from_project(project: &ProjectConfig) -> Self {
        let schemas = schema_paths(&project.roots);
        Self {
            project_root: display_path(&project.project_root),
            eva_config_path: display_path(&project.eva_config_path),
            environment: project.eva.runtime.env.clone(),
            hot_reload: project.eva.runtime.hot_reload,
            agents_total: project.agents.len(),
            agents_enabled: project.agents.iter().filter(|agent| agent.enabled).count(),
            adapters_total: project.adapters.len(),
            adapters_enabled: project
                .adapters
                .iter()
                .filter(|adapter| adapter.enabled)
                .count(),
            capabilities_total: project.capabilities.len(),
            capabilities_enabled: project
                .capabilities
                .iter()
                .filter(|capability| capability.enabled)
                .count(),
            policies_total: project.policies.len(),
            routes_total: project.routes.routes.len(),
            schema_files: vec![
                display_path(&schemas.eva),
                display_path(&schemas.agent),
                display_path(&schemas.adapter),
                display_path(&schemas.capability),
                display_path(&schemas.policy),
                display_path(&schemas.routes),
            ],
        }
    }
}

fn write_validation<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &ValidationReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK config validated").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(writer, "eva_config: {}", report.eva_config_path).map_err(write_error_kind)?;
            writeln!(writer, "environment: {}", report.environment).map_err(write_error_kind)?;
            writeln!(writer, "hot_reload: {}", report.hot_reload).map_err(write_error_kind)?;
            writeln!(
                writer,
                "agents: {} total, {} enabled",
                report.agents_total, report.agents_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "adapters: {} total, {} enabled",
                report.adapters_total, report.adapters_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "capabilities: {} total, {} enabled",
                report.capabilities_total, report.capabilities_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "policies: {}", report.policies_total).map_err(write_error_kind)?;
            writeln!(writer, "routes: {}", report.routes_total).map_err(write_error_kind)?;
            Ok(())
        }
        OutputFormat::Json => {
            let data = format!(
                "{{\"project_root\":{},\"eva_config_path\":{},\"environment\":{},\"hot_reload\":{},\"counts\":{{\"agents_total\":{},\"agents_enabled\":{},\"adapters_total\":{},\"adapters_enabled\":{},\"capabilities_total\":{},\"capabilities_enabled\":{},\"policies_total\":{},\"routes_total\":{}}},\"schema_files\":{}}}",
                json_string(&report.project_root),
                json_string(&report.eva_config_path),
                json_string(&report.environment),
                report.hot_reload,
                report.agents_total,
                report.agents_enabled,
                report.adapters_total,
                report.adapters_enabled,
                report.capabilities_total,
                report.capabilities_enabled,
                report.policies_total,
                report.routes_total,
                json_array(report.schema_files.iter().map(|path| json_string(path))),
            );
            writeln!(
                writer,
                "{}",
                success_envelope("config.validate", EXIT_OK, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}

fn write_doctor<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    report: &DoctorReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva doctor").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            for check in &report.checks {
                writeln!(
                    writer,
                    "[{}] {} - {}",
                    check.status.as_str(),
                    check.name,
                    check.message
                )
                .map_err(write_error_kind)?;
                if let Some(path) = &check.path {
                    writeln!(writer, "  path: {path}").map_err(write_error_kind)?;
                }
                if let Some(suggestion) = &check.suggestion {
                    writeln!(writer, "  suggestion: {suggestion}").map_err(write_error_kind)?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => {
            let checks = report
                .checks
                .iter()
                .map(|check| {
                    let mut fields = vec![
                        format!("\"name\":{}", json_string(&check.name)),
                        format!("\"status\":{}", json_string(check.status.as_str())),
                        format!("\"message\":{}", json_string(&check.message)),
                    ];
                    if let Some(path) = &check.path {
                        fields.push(format!("\"path\":{}", json_string(path)));
                    }
                    if let Some(suggestion) = &check.suggestion {
                        fields.push(format!("\"suggestion\":{}", json_string(suggestion)));
                    }
                    format!("{{{}}}", fields.join(","))
                })
                .collect::<Vec<_>>();
            let data = format!(
                "{{\"project_root\":{},\"checks\":{},\"error_count\":{},\"warning_count\":{}}}",
                json_string(&report.project_root),
                json_array(checks),
                report
                    .checks
                    .iter()
                    .filter(|check| check.status == CheckStatus::Error)
                    .count(),
                report
                    .checks
                    .iter()
                    .filter(|check| check.status == CheckStatus::Warning)
                    .count(),
            );
            writeln!(
                writer,
                "{}",
                success_envelope("doctor", exit_code, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}

fn write_inspect<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &InspectReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva inspect").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(writer, "environment: {}", report.environment).map_err(write_error_kind)?;
            writeln!(writer, "hot_reload: {}", report.hot_reload).map_err(write_error_kind)?;
            writeln!(writer, "agents:").map_err(write_error_kind)?;
            for agent in &report.agents {
                writeln!(
                    writer,
                    "  - {} enabled={} subscriptions={}",
                    agent.id,
                    agent.enabled,
                    agent.subscriptions.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "adapters:").map_err(write_error_kind)?;
            for adapter in &report.adapters {
                writeln!(
                    writer,
                    "  - {} transport={} enabled={} capabilities={}",
                    adapter.id,
                    adapter.transport,
                    adapter.enabled,
                    adapter.capabilities.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "capabilities:").map_err(write_error_kind)?;
            for capability in &report.capabilities {
                writeln!(
                    writer,
                    "  - {} capability={} kind={} enabled={} providers={}",
                    capability.id,
                    capability.capability,
                    capability.kind,
                    capability.enabled,
                    capability.providers.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "routes:").map_err(write_error_kind)?;
            for route in &report.routes {
                writeln!(
                    writer,
                    "  - {} delivery={} agents={}",
                    route.pattern,
                    route.delivery,
                    route.agents.join(",")
                )
                .map_err(write_error_kind)?;
            }
            writeln!(
                writer,
                "runtime: mode={} status={} generation={}",
                report.runtime.mode, report.runtime.status, report.runtime.generation_id
            )
            .map_err(write_error_kind)?;
            for service in &report.runtime.services {
                writeln!(
                    writer,
                    "  - {} state={} detail={}",
                    service.name, service.state, service.detail
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("inspect", EXIT_OK, &report.to_json(), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_run<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &BasicRunReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK run example=basic").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(
                writer,
                "runtime: mode={} generation={}",
                report.runtime_mode, report.generation_id
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "event: {} topic={} sequence={}",
                report.event_id, report.topic, report.receipt.sequence
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "deliveries:").map_err(write_error_kind)?;
            for delivery in &report.deliveries {
                writeln!(
                    writer,
                    "  - agent={} delivery={}",
                    delivery.agent_id,
                    delivery.delivery.as_str()
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "agent_runs:").map_err(write_error_kind)?;
            for run in &report.agent_runs {
                writeln!(
                    writer,
                    "  - agent={} status={} handler_status={}",
                    run.agent_id,
                    run.status.as_str(),
                    run.handler_status.as_deref().unwrap_or("")
                )
                .map_err(write_error_kind)?;
            }
            if let Some(response) = &report.capability_response {
                writeln!(
                    writer,
                    "capability: status={} output={}",
                    invoke_status(response.status()),
                    response
                        .output()
                        .and_then(|output| output.as_text())
                        .unwrap_or("")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("run", EXIT_OK, &run_report_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn run_report_json(report: &BasicRunReport) -> String {
    let deliveries = report.deliveries.iter().map(|delivery| {
        format!(
            "{{\"agent_id\":{},\"delivery\":{}}}",
            json_string(delivery.agent_id.as_str()),
            json_string(delivery.delivery.as_str())
        )
    });
    let agent_runs = report.agent_runs.iter().map(|run| {
        let error = run
            .error
            .as_ref()
            .map(|error| json_string(error.message()))
            .unwrap_or_else(|| "null".to_owned());
        format!(
            "{{\"agent_id\":{},\"event_id\":{},\"topic\":{},\"status\":{},\"handler_status\":{},\"output\":{},\"error\":{}}}",
            json_string(run.agent_id.as_str()),
            json_string(run.event_id.as_str()),
            json_string(run.topic.as_str()),
            json_string(run.status.as_str()),
            option_json(run.handler_status.as_deref()),
            option_json(run.output.as_deref()),
            error
        )
    });
    let lua_results = report.lua_results.iter().map(|result| {
        format!(
            "{{\"agent_id\":{},\"status\":{},\"topic\":{},\"note\":{},\"capability\":{},\"capability_input\":{}}}",
            json_string(result.agent_id.as_str()),
            json_string(&result.status),
            json_string(result.topic.as_str()),
            option_json(result.note.as_deref()),
            result
                .capability
                .as_ref()
                .map(|capability| json_string(capability.as_str()))
                .unwrap_or_else(|| "null".to_owned()),
            option_json(result.capability_input.as_deref())
        )
    });
    let capability_response = report
        .capability_response
        .as_ref()
        .map(capability_response_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"runtime_mode\":{},\"generation_id\":{},\"project_root\":{},\"event_id\":{},\"topic\":{},\"receipt\":{{\"event_id\":{},\"sequence\":{},\"topic\":{},\"target\":{}}},\"deliveries\":{},\"agent_runs\":{},\"lua_results\":{},\"capability_response\":{},\"audit\":{}}}",
        json_string(&report.runtime_mode),
        json_string(&report.generation_id),
        json_string(&report.project_root),
        json_string(&report.event_id),
        json_string(&report.topic),
        json_string(report.receipt.event_id.as_str()),
        report.receipt.sequence,
        json_string(report.receipt.topic.as_str()),
        json_string(&format!("{:?}", report.receipt.target)),
        json_array(deliveries),
        json_array(agent_runs),
        json_array(lua_results),
        capability_response,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn capability_response_json(response: &eva_core::InvokeResponse) -> String {
    format!(
        "{{\"request_id\":{},\"status\":{},\"output\":{},\"error\":{}}}",
        json_string(response.request_id().as_str()),
        json_string(invoke_status(response.status())),
        response
            .output()
            .and_then(|output| output.as_text())
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned()),
        response
            .error()
            .map(|error| json_string(error.message()))
            .unwrap_or_else(|| "null".to_owned())
    )
}

fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
    }
}

fn write_error<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    command: &str,
    exit_code: i32,
    error: &EvaError,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(
                writer,
                "ERROR {command} [{}] {}",
                error.kind().as_str(),
                error.message()
            )
            .map_err(write_error_kind)?;
            for (key, value) in error.context().entries() {
                writeln!(writer, "{key}: {value}").map_err(write_error_kind)?;
            }
            writeln!(writer, "suggestion: {}", suggestion_for_error(error))
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            error_envelope(command, exit_code, error, trace)
        )
        .map_err(write_error_kind),
    }
}

fn success_envelope(command: &str, exit_code: i32, data_json: &str, trace: &TraceFields) -> String {
    format!(
        "{{\"ok\":true,\"command\":{},\"exit_code\":{},\"data\":{},\"trace\":{}}}",
        json_string(command),
        exit_code,
        data_json,
        trace_json(trace)
    )
}

fn error_envelope(command: &str, exit_code: i32, error: &EvaError, trace: &TraceFields) -> String {
    let provider_code = error
        .provider_code()
        .map(|code| json_string(code.as_str()))
        .unwrap_or_else(|| "null".to_owned());
    let context = error
        .context()
        .entries()
        .iter()
        .map(|(key, value)| {
            format!(
                "{{\"key\":{},\"value\":{}}}",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "{{\"ok\":false,\"command\":{},\"exit_code\":{},\"error\":{{\"kind\":{},\"message\":{},\"retryable\":{},\"provider_code\":{},\"context\":{},\"suggestion\":{}}},\"trace\":{}}}",
        json_string(command),
        exit_code,
        json_string(error.kind().as_str()),
        json_string(error.message()),
        error.is_retryable(),
        provider_code,
        json_array(context),
        json_string(&suggestion_for_error(error)),
        trace_json(trace)
    )
}

fn trace_for(span_id: &str) -> TraceFields {
    TraceFields::default().with_span_id(
        SpanId::parse(span_id)
            .expect("static CLI span identifiers use the eva-observability character set"),
    )
}

fn trace_json(trace: &TraceFields) -> String {
    let fields = trace
        .entries()
        .into_iter()
        .map(|(key, value)| format!("{}:{}", json_string(key), json_string(&value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", fields.join(","))
}

pub(crate) fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value if value.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", value as u32));
            }
            value => escaped.push(value),
        }
    }
    escaped.push('"');
    escaped
}

pub(crate) fn json_array<I>(values: I) -> String
where
    I: IntoIterator<Item = String>,
{
    format!("[{}]", values.into_iter().collect::<Vec<_>>().join(","))
}

pub(crate) fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn exit_code_for_error(error: &EvaError) -> i32 {
    match error.kind() {
        ErrorKind::PermissionDenied => EXIT_POLICY,
        ErrorKind::Timeout | ErrorKind::Unavailable => EXIT_RUNTIME_UNAVAILABLE,
        ErrorKind::Unsupported => EXIT_RUNTIME_UNAVAILABLE,
        ErrorKind::InvalidArgument | ErrorKind::NotFound | ErrorKind::Conflict => EXIT_CONFIG,
        ErrorKind::Internal => EXIT_INTERNAL,
    }
}

fn suggestion_for_error(error: &EvaError) -> String {
    if let Some((_, suggestion)) = error
        .context()
        .entries()
        .iter()
        .find(|(key, _)| key == "suggestion")
    {
        return suggestion.clone();
    }

    match error.kind() {
        ErrorKind::InvalidArgument | ErrorKind::NotFound | ErrorKind::Conflict => {
            "确认 --project 指向 Eva workspace，并检查 config/eva.yaml、manifest、routes 与 schema 路径。"
                .to_owned()
        }
        ErrorKind::PermissionDenied => {
            "检查 policy 和 manifest 权限声明，确认请求没有扩大 effective policy。".to_owned()
        }
        ErrorKind::Timeout | ErrorKind::Unavailable | ErrorKind::Unsupported => {
            "该能力在当前版本不可用；先运行 eva doctor 和 eva inspect 查看 V0.3 可用边界。"
                .to_owned()
        }
        ErrorKind::Internal => "查看上方上下文并保留命令输出作为缺陷报告证据。".to_owned(),
    }
}

fn write_error_kind(error: io::Error) -> EvaError {
    EvaError::internal("failed to write CLI output").with_context("io_error", error.to_string())
}

fn help_text() -> &'static str {
    "Eva CLI\n\nUSAGE:\n  eva doctor [--project <path>] [--output text|json]\n  eva config validate [--project <path>] [--output text|json]\n  eva inspect [all|config|runtime] [--project <path>] [--output text|json]\n  eva run --example basic [--project <path>] [--output text|json]\n\nCommands:\n  doctor           Check workspace, configuration roots, schema files, and runtime boundaries.\n  config validate  Load eva.yaml plus split manifests and report stable diagnostics.\n  inspect          Show agents, adapters, capabilities, routes, policy summary, and runtime status.\n  run              Execute the V0.4 in-memory basic event loop.\n\nExit codes:\n  0 success\n  2 configuration or validation error\n  3 policy denied\n  4 runtime unavailable or unsupported in this version\n  5 external capability unavailable\n  64 command usage error\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn run_cli(args: &[&str]) -> (i32, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run_with_args(args.iter().map(OsString::from), &mut stdout, &mut stderr);
        (
            exit_code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[test]
    fn config_validate_json_succeeds_for_sample_project() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "config",
            "validate",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"ok\":true"));
        assert!(stdout.contains("\"command\":\"config.validate\""));
        assert!(stdout.contains("\"agents_total\""));
        assert!(stderr.is_empty());
    }

    #[test]
    fn inspect_text_reports_noop_runtime() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) =
            run_cli(&["inspect", "--project", root.to_str().unwrap()]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("runtime: mode=noop"));
        assert!(stdout.contains("agents:"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn unknown_command_is_usage_error() {
        let (exit_code, _stdout, stderr) = run_cli(&["missing"]);

        assert_eq!(exit_code, EXIT_USAGE);
        assert!(stderr.contains("unknown command"));
    }

    #[test]
    fn run_basic_example_json_succeeds() {
        let root = workspace_root();
        let (exit_code, stdout, stderr) = run_cli(&[
            "run",
            "--example",
            "basic",
            "--project",
            root.to_str().unwrap(),
            "--output",
            "json",
        ]);

        assert_eq!(exit_code, EXIT_OK, "{stderr}");
        assert!(stdout.contains("\"command\":\"run\""));
        assert!(stdout.contains("\"capability_response\""));
        assert!(stdout.contains("config.lint") || stdout.contains("valid"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn json_string_escapes_control_characters() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }
}
