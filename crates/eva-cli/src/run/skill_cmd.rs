use super::{
    join_capabilities, json_array, json_string, option_json, parse_common_options, required_option,
    success_envelope, trace_for, trace_json, write_command_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_adapter::{AdapterInvocation, AdapterInvokeReport, AdapterRuntime};
use eva_config::{load_project_config, AdapterTransport, ProjectConfig};
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_observability::TraceFields;
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SkillCommand {
    List(CommonOptions),
    Run(SkillRunOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillRunOptions {
    common: CommonOptions,
    adapter_id: Option<String>,
    skill_id: Option<String>,
    capability: Option<String>,
    input: String,
    request_id: String,
}

pub(super) fn parse_skill_command(args: &[String]) -> Result<SkillCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing skill subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(SkillCommand::List(parse_common_options(rest)?)),
        "run" => Ok(SkillCommand::Run(parse_skill_run_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown skill subcommand").with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_skill<W, E>(
    command: SkillCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        SkillCommand::List(options) => {
            let trace = trace_for("cli.skill.list");
            match load_project_config(&options.project_root)
                .and_then(|project| AdapterRuntime::from_project(&project))
            {
                Ok(runtime) => {
                    write_skill_list(stdout, options.output, &runtime, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "skill.list", &error, &trace)
                }
            }
        }
        SkillCommand::Run(options) => {
            let trace = trace_for("cli.skill.run");
            match load_project_config(&options.common.project_root).and_then(|project| {
                let runtime = AdapterRuntime::from_project(&project)?;
                run_skill_runtime(&runtime, &project, &options)
            }) {
                Ok(report) => {
                    write_adapter_invoke(
                        stdout,
                        options.common.output,
                        "skill.run",
                        &report,
                        &trace,
                    )?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.common.output, "skill.run", &error, &trace)
                }
            }
        }
    }
}

fn parse_skill_run_options(args: &[String]) -> Result<SkillRunOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut skill_id = None;
    let mut capability = None;
    let mut input = "{}".to_owned();
    let mut request_id = "req-skill-1".to_owned();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            "--skill" | "--skill-id" => {
                index += 1;
                skill_id = Some(required_option(args, index, "skill option")?.clone());
            }
            "--capability" => {
                index += 1;
                capability = Some(required_option(args, index, "capability option")?.clone());
            }
            "--input" => {
                index += 1;
                input = required_option(args, index, "input option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    RequestId::parse(&request_id)?;
    Ok(SkillRunOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
        skill_id,
        capability,
        input,
        request_id,
    })
}

fn run_skill_runtime(
    runtime: &AdapterRuntime,
    project: &ProjectConfig,
    options: &SkillRunOptions,
) -> Result<AdapterInvokeReport, EvaError> {
    let capability = options
        .capability
        .as_deref()
        .map(CapabilityName::parse)
        .transpose()?
        .unwrap_or_else(|| CapabilityName::parse("workflow.code_review").unwrap());
    let provider = if let Some(adapter_id) = &options.adapter_id {
        Some(AdapterId::parse(adapter_id)?)
    } else if let Some(skill_id) = &options.skill_id {
        runtime
            .list()
            .into_iter()
            .find(|handle| {
                handle.transport == AdapterTransport::Skill
                    && handle.skill_name() == Some(skill_id.as_str())
            })
            .map(|handle| handle.id.clone())
    } else {
        None
    };
    if let Some(provider) = &provider {
        if let Some(handle) = runtime.registry().get(provider) {
            if handle.transport == AdapterTransport::Skill {
                let decision = RuntimePolicyGate::from_project(project)?.decide(
                    RuntimePolicyRequest::new(HighRiskAction::SkillRun)
                        .with_tool(handle.skill_runtime_gate.as_deref().unwrap_or("")),
                );
                decision.ensure_allowed()?;
            }
        }
    }
    let invocation = AdapterInvocation::new(RequestId::parse(&options.request_id)?, capability)
        .with_input(options.input.clone());
    let invocation = if let Some(provider) = provider {
        invocation.with_provider(provider)
    } else {
        invocation
    };
    runtime.invoke(invocation)
}

fn write_skill_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    runtime: &AdapterRuntime,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva skills").map_err(write_error_kind)?;
            for handle in runtime
                .list()
                .into_iter()
                .filter(|handle| handle.transport == AdapterTransport::Skill)
            {
                writeln!(
                    writer,
                    "  - {} skill={} gate={} capabilities={}",
                    handle.id,
                    handle.skill_name().unwrap_or(""),
                    handle.skill_runtime_gate.as_deref().unwrap_or(""),
                    join_capabilities(&handle.capabilities)
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("skill.list", EXIT_OK, &skill_list_json(runtime), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_adapter_invoke<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    command: &str,
    report: &AdapterInvokeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK {command}").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", report.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "capability: {}", report.capability).map_err(write_error_kind)?;
            writeln!(writer, "transport: {}", report.transport.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "output: {}", report.output).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(command, EXIT_OK, &adapter_invoke_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn skill_list_json(runtime: &AdapterRuntime) -> String {
    let entries = runtime
        .list()
        .into_iter()
        .filter(|handle| handle.transport == AdapterTransport::Skill)
        .map(|handle| {
            format!(
                "{{\"adapter_id\":{},\"skill_id\":{},\"skill_kind\":{},\"runtime_gate\":{},\"capabilities\":{},\"enabled\":{}}}",
                json_string(handle.id.as_str()),
                option_json(handle.skill_name()),
                option_json(handle.skill_kind.as_deref()),
                option_json(handle.skill_runtime_gate.as_deref()),
                json_array(handle.capabilities.iter().map(|capability| json_string(capability.as_str()))),
                handle.enabled
            )
        });
    format!("{{\"skills\":{}}}", json_array(entries))
}

fn adapter_invoke_json(report: &AdapterInvokeReport) -> String {
    format!(
        "{{\"request_id\":{},\"adapter_id\":{},\"transport\":{},\"capability\":{},\"status\":{},\"output\":{},\"audit\":{},\"trace\":{}}}",
        json_string(report.request_id.as_str()),
        json_string(report.adapter_id.as_str()),
        json_string(report.transport.as_str()),
        json_string(report.capability.as_str()),
        json_string(&report.status),
        json_string(&report.output),
        json_array(report.audit.iter().map(|entry| json_string(entry))),
        trace_json(&report.trace)
    )
}
