use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, write_risk_lines_text, CommonOptions,
    OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{AdapterId, EvaError, RequestId};
use eva_hardware::{
    discover_project_devices, DeviceCandidate, HardwareDiscoveryReport, OsPermissionCheck,
    OsPermissionProvider, PlatformOsPermissionProvider, RegisteredDevice,
};
use eva_observability::TraceFields;
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HardwareCommand {
    List(CommonOptions),
    Probe(HardwareProbeOptions),
    Bind(HardwareBindOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HardwareProbeOptions {
    common: CommonOptions,
    adapter_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HardwareBindOptions {
    common: CommonOptions,
    adapter_id: String,
    request_id: String,
    apply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HardwareBindPlan {
    adapter_id: AdapterId,
    request_id: RequestId,
    status: String,
    apply: bool,
    mutation_executed: bool,
    device: Option<DeviceCandidate>,
    permission: Option<OsPermissionCheck>,
    steps: Vec<String>,
    risks: Vec<String>,
    audit: Vec<String>,
}

pub(super) fn parse_hardware_command(args: &[String]) -> Result<HardwareCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing hardware subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(HardwareCommand::List(parse_common_options(rest)?)),
        "probe" => Ok(HardwareCommand::Probe(parse_hardware_probe_options(rest)?)),
        "bind" => Ok(HardwareCommand::Bind(parse_hardware_bind_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown hardware subcommand")
                .with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_hardware<W, E>(
    command: HardwareCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        HardwareCommand::List(options) => {
            let trace = trace_for("cli.hardware.list");
            match load_project_config(&options.project_root)
                .and_then(|project| discover_project_devices(&project))
            {
                Ok(report) => {
                    write_hardware_list(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "hardware.list", &error, &trace)
                }
            }
        }
        HardwareCommand::Probe(options) => {
            let trace = trace_for("cli.hardware.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| discover_project_devices(&project))
                .and_then(|report| probe_hardware_candidates(report, options.adapter_id.as_deref()))
            {
                Ok(candidates) => {
                    write_hardware_probe(stdout, options.common.output, &candidates, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "hardware.probe",
                    &error,
                    &trace,
                ),
            }
        }
        HardwareCommand::Bind(options) => {
            let trace = trace_for("cli.hardware.bind");
            match load_project_config(&options.common.project_root)
                .and_then(|project| hardware_bind_plan(&project, &options))
            {
                Ok(plan) => {
                    write_hardware_bind(stdout, options.common.output, &plan, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "hardware.bind",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn parse_hardware_probe_options(args: &[String]) -> Result<HardwareProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = Some(required_option(args, index, "adapter option")?.clone());
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    if let Some(adapter_id) = &adapter_id {
        AdapterId::parse(adapter_id)?;
    }
    Ok(HardwareProbeOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
    })
}

fn parse_hardware_bind_options(args: &[String]) -> Result<HardwareBindOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut adapter_id = "scale-main".to_owned();
    let mut request_id = "req-hardware-1".to_owned();
    let mut apply = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--adapter" | "--adapter-id" => {
                index += 1;
                adapter_id = required_option(args, index, "adapter option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--apply" => apply = true,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    AdapterId::parse(&adapter_id)?;
    RequestId::parse(&request_id)?;
    Ok(HardwareBindOptions {
        common: parse_common_options(&passthrough)?,
        adapter_id,
        request_id,
        apply,
    })
}

fn probe_hardware_candidates(
    report: HardwareDiscoveryReport,
    adapter_id: Option<&str>,
) -> Result<Vec<DeviceCandidate>, EvaError> {
    let mut candidates = report.candidates;
    if let Some(adapter_id) = adapter_id {
        candidates.retain(|candidate| candidate.identity.adapter_id.as_str() == adapter_id);
        if candidates.is_empty() {
            return Err(
                EvaError::not_found("hardware adapter candidate does not exist")
                    .with_context("adapter_id", adapter_id),
            );
        }
    }
    Ok(candidates)
}

fn hardware_bind_plan(
    project: &ProjectConfig,
    options: &HardwareBindOptions,
) -> Result<HardwareBindPlan, EvaError> {
    let adapter_id = AdapterId::parse(&options.adapter_id)?;
    let request_id = RequestId::parse(&options.request_id)?;
    let report = discover_project_devices(project)?;
    let device = report
        .candidates
        .into_iter()
        .find(|candidate| candidate.identity.adapter_id == adapter_id);
    let adapter = project
        .adapters
        .iter()
        .find(|adapter| adapter.id == adapter_id);
    let mut policy_request =
        RuntimePolicyRequest::new(HighRiskAction::HardwareBind).with_adapter(adapter_id.clone());
    if let Some(candidate) = &device {
        policy_request = policy_request.with_bus(candidate.identity.bus.as_str());
    }
    if let Some(capability) = adapter
        .and_then(|adapter| adapter.capabilities.first())
        .cloned()
    {
        policy_request = policy_request.with_capability(capability);
    }
    if let Some(timeout_ms) =
        adapter.and_then(|adapter| adapter.nested_extra_u64("limits", "timeout_ms"))
    {
        policy_request = policy_request.with_timeout_ms(timeout_ms);
    }
    let policy_decision = RuntimePolicyGate::from_project(project)?.decide(policy_request);
    let permission = device.as_ref().map(hardware_permission_check).transpose()?;
    let permission_denied = matches!(permission.as_ref(), Some(check) if !check.granted);
    let status = match &device {
        Some(candidate) if candidate.rejected_reason.is_none() && options.apply => "ready_to_apply",
        Some(candidate) if candidate.rejected_reason.is_none() => "planned",
        Some(_) => "blocked",
        None => "missing",
    };
    let status = if options.apply && (!policy_decision.allowed || permission_denied) {
        "blocked"
    } else {
        status
    }
    .to_owned();
    let mut risks = vec!["hardware binding is plan-first; no raw I/O is opened by CLI".to_owned()];
    if options.apply {
        risks.push(
            "--apply only validates the logical plan in V1.3; physical claims remain runtime-gated"
                .to_owned(),
        );
    }
    if !policy_decision.allowed {
        risks.push(format!(
            "runtime policy denied hardware bind apply path: {}",
            policy_decision.reason
        ));
    }
    if let Some(permission) = &permission {
        risks.push(format!(
            "OS permission provider {} reports {} for {}",
            permission.source,
            if permission.granted {
                "granted"
            } else {
                "denied"
            },
            permission.permission
        ));
        if !permission.granted {
            risks.extend(permission.remediation.iter().cloned());
        }
    }
    if let Some(candidate) = &device {
        if let Some(reason) = &candidate.rejected_reason {
            risks.push(reason.clone());
        }
    }
    Ok(HardwareBindPlan {
        adapter_id,
        request_id,
        status,
        apply: options.apply,
        mutation_executed: false,
        device,
        permission,
        steps: vec![
            "discover hardware manifest candidate".to_owned(),
            "verify adapter manifest and policy boundary".to_owned(),
            "evaluate runtime policy domain gate".to_owned(),
            "evaluate OS permission provider before driver start".to_owned(),
            "create logical DeviceRegistry lease".to_owned(),
            "route invocation through AdapterRuntime hardware transport".to_owned(),
            "release logical lease and emit audit".to_owned(),
        ],
        risks,
        audit: policy_decision.audit,
    })
}

fn hardware_permission_check(candidate: &DeviceCandidate) -> Result<OsPermissionCheck, EvaError> {
    let provider = PlatformOsPermissionProvider::current_process();
    let registered = RegisteredDevice {
        identity: candidate.identity.clone(),
        health: candidate.health,
        source_path: candidate.source_path.clone(),
        claimed_by: None,
    };
    provider.check(&registered)
}

fn write_hardware_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &HardwareDiscoveryReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva hardware candidates").map_err(write_error_kind)?;
            for candidate in &report.candidates {
                writeln!(
                    writer,
                    "  - {} adapter={} bus={} health={} trust={} handle_granted={}",
                    candidate.identity.id.as_str(),
                    candidate.identity.adapter_id,
                    candidate.identity.bus.as_str(),
                    candidate.health.as_str(),
                    candidate.identity.trust.as_str(),
                    candidate.handle_granted
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "hardware.list",
                EXIT_OK,
                &hardware_candidates_json(&report.candidates),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_hardware_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    candidates: &[DeviceCandidate],
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva hardware probe").map_err(write_error_kind)?;
            for candidate in candidates {
                writeln!(
                    writer,
                    "  - {} status={} reason={}",
                    candidate.identity.id.as_str(),
                    candidate.health.as_str(),
                    candidate.rejected_reason.as_deref().unwrap_or("ok")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "hardware.probe",
                EXIT_OK,
                &hardware_candidates_json(candidates),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_hardware_bind<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    plan: &HardwareBindPlan,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Hardware bind plan").map_err(write_error_kind)?;
            writeln!(writer, "adapter: {}", plan.adapter_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", plan.status).map_err(write_error_kind)?;
            if let Some(permission) = &plan.permission {
                writeln!(
                    writer,
                    "permission: {} granted={} source={}",
                    permission.permission, permission.granted, permission.source
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "apply: {}", plan.apply).map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", plan.mutation_executed)
                .map_err(write_error_kind)?;
            write_hardware_operator_summary_text(writer, plan)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "hardware.bind",
                EXIT_OK,
                &hardware_bind_plan_json(plan),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_hardware_operator_summary_text<W: Write>(
    writer: &mut W,
    plan: &HardwareBindPlan,
) -> Result<(), EvaError> {
    writeln!(writer, "operator_summary: hardware.bind").map_err(write_error_kind)?;
    writeln!(writer, "plan_id: {}", plan.request_id).map_err(write_error_kind)?;
    writeln!(writer, "confirm_token: not_required_plan_only").map_err(write_error_kind)?;
    writeln!(writer, "target: {}", hardware_bind_target(plan)).map_err(write_error_kind)?;
    writeln!(writer, "final_state: {}", plan.status).map_err(write_error_kind)?;
    writeln!(writer, "rollback_path: none; no raw I/O handle granted").map_err(write_error_kind)?;
    write_risk_lines_text(writer, &plan.risks)
}

fn hardware_bind_target(plan: &HardwareBindPlan) -> String {
    plan.device
        .as_ref()
        .map(|device| {
            format!(
                "{} ({})",
                device.identity.logical_name,
                device.identity.id.as_str()
            )
        })
        .unwrap_or_else(|| plan.adapter_id.as_str().to_owned())
}

fn hardware_candidates_json(candidates: &[DeviceCandidate]) -> String {
    let entries = candidates.iter().map(hardware_candidate_json);
    format!(
        "{{\"candidate_count\":{},\"candidates\":{}}}",
        candidates.len(),
        json_array(entries)
    )
}

fn hardware_candidate_json(candidate: &DeviceCandidate) -> String {
    format!(
        "{{\"device_id\":{},\"adapter_id\":{},\"logical_name\":{},\"device_class\":{},\"bus\":{},\"trust\":{},\"health\":{},\"vendor_id\":{},\"product_id\":{},\"serial\":{},\"protocol\":{},\"handle_granted\":{},\"rejected_reason\":{},\"source_path\":{}}}",
        json_string(candidate.identity.id.as_str()),
        json_string(candidate.identity.adapter_id.as_str()),
        json_string(&candidate.identity.logical_name),
        json_string(&candidate.identity.device_class),
        json_string(candidate.identity.bus.as_str()),
        json_string(candidate.identity.trust.as_str()),
        json_string(candidate.health.as_str()),
        option_json(candidate.vendor_id.as_deref()),
        option_json(candidate.product_id.as_deref()),
        option_json(candidate.serial.as_deref()),
        option_json(candidate.protocol.as_deref()),
        candidate.handle_granted,
        option_json(candidate.rejected_reason.as_deref()),
        json_string(&candidate.source_path)
    )
}

fn hardware_bind_plan_json(plan: &HardwareBindPlan) -> String {
    format!(
        "{{\"adapter_id\":{},\"request_id\":{},\"status\":{},\"apply\":{},\"mutation_executed\":{},\"device\":{},\"permission\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}",
        json_string(plan.adapter_id.as_str()),
        json_string(plan.request_id.as_str()),
        json_string(&plan.status),
        plan.apply,
        plan.mutation_executed,
        plan.device
            .as_ref()
            .map(hardware_candidate_json)
            .unwrap_or_else(|| "null".to_owned()),
        plan.permission
            .as_ref()
            .map(hardware_permission_json)
            .unwrap_or_else(|| "null".to_owned()),
        json_array(plan.steps.iter().map(|step| json_string(step))),
        json_array(plan.risks.iter().map(|risk| json_string(risk))),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
    )
}

fn hardware_permission_json(permission: &OsPermissionCheck) -> String {
    format!(
        "{{\"device_id\":{},\"bus\":{},\"permission\":{},\"granted\":{},\"os\":{},\"user\":{},\"source\":{},\"device_path\":{},\"raw_device_path_exposed\":{},\"remediation\":{}}}",
        json_string(permission.device_id.as_str()),
        json_string(&permission.bus),
        json_string(&permission.permission),
        permission.granted,
        json_string(&permission.os),
        json_string(&permission.user),
        json_string(&permission.source),
        json_string(&permission.device_path),
        permission.raw_device_path_exposed,
        json_array(permission.remediation.iter().map(|item| json_string(item)))
    )
}
