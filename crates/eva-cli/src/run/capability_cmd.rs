use super::{
    json_array, json_string, option_json, parse_common_options, required_option, success_envelope,
    trace_for, write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_adapter::{AdapterBackedCapabilityHost, AdapterRuntime};
use eva_capability::{
    CapabilityDescriptor, CapabilityHostApi, CapabilityPermissionGate, CapabilityProviderPlan,
    CapabilityRegistry, CapabilityRouter,
};
use eva_config::{
    load_project_config, manifest::capability::CapabilityManifest, AdapterTransport, ProjectConfig,
};
use eva_core::{
    AdapterId, CapabilityName, EvaError, InvokeInput, InvokeRequest, InvokeResponse, InvokeStatus,
    InvokeTarget, RequestId,
};
use eva_observability::TraceFields;
use eva_policy::{
    HighRiskAction, PermissionSet, PolicyDecision, RuntimePolicyGate, RuntimePolicyRequest,
};
use std::io::Write;

const DEFAULT_CAPABILITY: &str = "repo.analyze";
const DEFAULT_REQUEST_ID: &str = "req-capability-1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CapabilityCommand {
    List(CommonOptions),
    Probe(CapabilityProbeOptions),
    Call(CapabilityCallOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CapabilityProbeOptions {
    common: CommonOptions,
    capability: String,
    provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CapabilityCallOptions {
    common: CommonOptions,
    capability: String,
    provider: Option<String>,
    input: String,
    request_id: String,
    confirm: Option<String>,
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityListReport {
    capabilities: Vec<CapabilityListEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityListEntry {
    manifest_id: String,
    name: String,
    version: String,
    capability: String,
    kind: String,
    enabled: bool,
    provider: String,
    providers: Vec<ProviderPlanEntry>,
    required_adapter_capabilities: Vec<String>,
    manifest_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityProbeReport {
    status: String,
    capability: String,
    provider_plan: CapabilityProviderPlan,
    providers: Vec<ProviderProbeEntry>,
    permission_gate: GateReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityCallReport {
    status: String,
    request_id: String,
    capability: String,
    input_size: usize,
    provider_plan: CapabilityProviderPlan,
    permission_gate: GateReport,
    runtime_policy: Vec<PolicyDecision>,
    confirmed: bool,
    invocation_executed: bool,
    mutation_executed: bool,
    response: Option<InvokeResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderPlanEntry {
    provider: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderProbeEntry {
    provider: String,
    source: String,
    status: String,
    transport: Option<String>,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GateReport {
    allowed: bool,
    reason: String,
}

pub(super) fn parse_capability_command(args: &[String]) -> Result<CapabilityCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing capability subcommand"))?;
    match subcommand.as_str() {
        "list" => Ok(CapabilityCommand::List(parse_common_options(rest)?)),
        "probe" => Ok(CapabilityCommand::Probe(parse_capability_probe_options(
            rest,
        )?)),
        "call" => Ok(CapabilityCommand::Call(parse_capability_call_options(
            rest,
        )?)),
        value => Err(EvaError::unsupported("unknown capability subcommand")
            .with_context("subcommand", value)),
    }
}

pub(super) fn execute_capability<W, E>(
    command: CapabilityCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        CapabilityCommand::List(options) => {
            let trace = trace_for("cli.capability.list");
            match load_project_config(&options.project_root).and_then(|project| {
                let registry = capability_registry_from_project(&project)?;
                create_capability_list(&project, &registry)
            }) {
                Ok(report) => {
                    write_capability_list(stdout, options.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    write_command_error(stderr, options.output, "capability.list", &error, &trace)
                }
            }
        }
        CapabilityCommand::Probe(options) => {
            let trace = trace_for("cli.capability.probe");
            match load_project_config(&options.common.project_root)
                .and_then(|project| create_capability_probe(&project, &options))
            {
                Ok(report) => {
                    write_capability_probe(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "capability.probe",
                    &error,
                    &trace,
                ),
            }
        }
        CapabilityCommand::Call(options) => {
            let trace = trace_for("cli.capability.call");
            match load_project_config(&options.common.project_root)
                .and_then(|project| create_capability_call(&project, &options))
            {
                Ok(report) => {
                    write_capability_call(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "capability.call",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn parse_capability_probe_options(args: &[String]) -> Result<CapabilityProbeOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut capability = None;
    let mut provider = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--project" | "--project-root" | "-p" | "--output" | "-o" => {
                passthrough.push(args[index].clone());
                index += 1;
                passthrough.push(required_option(args, index, "common option")?.clone());
            }
            "--capability" => {
                index += 1;
                capability = Some(required_option(args, index, "capability option")?.clone());
            }
            "--provider" | "--adapter" | "--adapter-id" => {
                index += 1;
                provider = Some(required_option(args, index, "provider option")?.clone());
            }
            value if value.starts_with('-') => passthrough.push(args[index].clone()),
            value => set_capability_once(&mut capability, value.to_owned())?,
        }
        index += 1;
    }
    let capability = capability.unwrap_or_else(|| DEFAULT_CAPABILITY.to_owned());
    CapabilityName::parse(&capability)?;
    if let Some(value) = &provider {
        AdapterId::parse(value)?;
    }
    Ok(CapabilityProbeOptions {
        common: parse_common_options(&passthrough)?,
        capability,
        provider,
    })
}

fn parse_capability_call_options(args: &[String]) -> Result<CapabilityCallOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut capability = None;
    let mut provider = None;
    let mut input = "{}".to_owned();
    let mut request_id = DEFAULT_REQUEST_ID.to_owned();
    let mut confirm = None;
    let mut dry_run = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--project" | "--project-root" | "-p" | "--output" | "-o" => {
                passthrough.push(args[index].clone());
                index += 1;
                passthrough.push(required_option(args, index, "common option")?.clone());
            }
            "--capability" => {
                index += 1;
                capability = Some(required_option(args, index, "capability option")?.clone());
            }
            "--provider" | "--adapter" | "--adapter-id" => {
                index += 1;
                provider = Some(required_option(args, index, "provider option")?.clone());
            }
            "--input" | "--payload" => {
                index += 1;
                input = required_option(args, index, "input option")?.clone();
            }
            "--request-id" | "--request" | "--task-id" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--confirm" => {
                index += 1;
                confirm = Some(required_option(args, index, "confirm option")?.clone());
            }
            "--dry-run" => dry_run = true,
            value if value.starts_with('-') => passthrough.push(args[index].clone()),
            value => set_capability_once(&mut capability, value.to_owned())?,
        }
        index += 1;
    }
    let capability = capability.unwrap_or_else(|| DEFAULT_CAPABILITY.to_owned());
    CapabilityName::parse(&capability)?;
    if let Some(value) = &provider {
        AdapterId::parse(value)?;
    }
    RequestId::parse(&request_id)?;
    if let Some(value) = &confirm {
        RequestId::parse(value)?;
    }
    Ok(CapabilityCallOptions {
        common: parse_common_options(&passthrough)?,
        capability,
        provider,
        input,
        request_id,
        confirm,
        dry_run,
    })
}

fn create_capability_list(
    project: &ProjectConfig,
    registry: &CapabilityRegistry,
) -> Result<CapabilityListReport, EvaError> {
    let capabilities = project
        .capabilities
        .iter()
        .map(|manifest| capability_list_entry(manifest, registry))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CapabilityListReport { capabilities })
}

fn create_capability_probe(
    project: &ProjectConfig,
    options: &CapabilityProbeOptions,
) -> Result<CapabilityProbeReport, EvaError> {
    let registry = capability_registry_from_project(project)?;
    let runtime = AdapterRuntime::from_project(project)?;
    let capability = CapabilityName::parse(&options.capability)?;
    let explicit_provider = options
        .provider
        .as_deref()
        .map(AdapterId::parse)
        .transpose()?;
    let plan = descriptor_for(&registry, &capability)?.provider_plan(explicit_provider);
    let permissions = permissions_for_plan(&plan);
    CapabilityPermissionGate::new(permissions).ensure_plan_allowed(&plan)?;
    let providers = probe_providers(&runtime, &plan);
    Ok(CapabilityProbeReport {
        status: if providers
            .iter()
            .any(|provider| provider.status == "blocked")
        {
            "degraded".to_owned()
        } else {
            "ready".to_owned()
        },
        capability: capability.as_str().to_owned(),
        provider_plan: plan,
        providers,
        permission_gate: GateReport {
            allowed: true,
            reason: "manifest and derived permission allow provider plan".to_owned(),
        },
    })
}

fn create_capability_call(
    project: &ProjectConfig,
    options: &CapabilityCallOptions,
) -> Result<CapabilityCallReport, EvaError> {
    if let Some(confirm) = &options.confirm {
        if confirm != &options.request_id {
            return Err(EvaError::conflict(
                "capability call confirmation does not match request id",
            )
            .with_context("request_id", &options.request_id)
            .with_context("confirm", confirm));
        }
    }

    let registry = capability_registry_from_project(project)?;
    let router = CapabilityRouter::new(registry.clone());
    let runtime = AdapterRuntime::from_project(project)?;
    let capability = CapabilityName::parse(&options.capability)?;
    let explicit_provider = options
        .provider
        .as_deref()
        .map(AdapterId::parse)
        .transpose()?;
    let request = InvokeRequest::new(
        RequestId::parse(&options.request_id)?,
        InvokeTarget::Capability(capability.clone()),
        InvokeInput::text(options.input.clone()),
    );
    let plan = router.provider_plan(&request, explicit_provider.clone())?;
    let permissions = permissions_for_plan(&plan);
    CapabilityPermissionGate::new(permissions.clone()).ensure_plan_allowed(&plan)?;
    let runtime_policy = runtime_policy_decisions(project, &runtime, &plan)?;
    for decision in &runtime_policy {
        decision.ensure_allowed()?;
    }
    let confirmed = options.confirm.as_deref() == Some(options.request_id.as_str());
    let should_execute = confirmed && !options.dry_run;

    let response = if should_execute {
        if plan.is_empty() {
            Some(router.invoke(request)?)
        } else {
            let host = AdapterBackedCapabilityHost::new(router, runtime, permissions);
            Some(host.invoke_with_provider(request, explicit_provider)?)
        }
    } else {
        None
    };

    Ok(CapabilityCallReport {
        status: if should_execute {
            "executed"
        } else {
            "dry_run"
        }
        .to_owned(),
        request_id: options.request_id.clone(),
        capability: capability.as_str().to_owned(),
        input_size: options.input.len(),
        provider_plan: plan,
        permission_gate: GateReport {
            allowed: true,
            reason: "manifest and derived permission allow provider plan".to_owned(),
        },
        runtime_policy,
        confirmed,
        invocation_executed: should_execute,
        mutation_executed: false,
        response,
    })
}

fn capability_registry_from_project(
    project: &ProjectConfig,
) -> Result<CapabilityRegistry, EvaError> {
    let mut registry = CapabilityRegistry::new();
    for manifest in &project.capabilities {
        registry.register(CapabilityDescriptor::from_manifest(manifest))?;
    }
    Ok(registry)
}

fn capability_list_entry(
    manifest: &CapabilityManifest,
    registry: &CapabilityRegistry,
) -> Result<CapabilityListEntry, EvaError> {
    let descriptor = descriptor_for(registry, &manifest.capability)?;
    let plan = descriptor.provider_plan(None);
    Ok(CapabilityListEntry {
        manifest_id: manifest.id.as_str().to_owned(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        capability: manifest.capability.as_str().to_owned(),
        kind: manifest.kind.as_str().to_owned(),
        enabled: manifest.enabled,
        provider: descriptor.provider.clone(),
        providers: provider_plan_entries(&plan),
        required_adapter_capabilities: plan
            .required_adapter_capabilities
            .iter()
            .map(|capability| capability.as_str().to_owned())
            .collect(),
        manifest_path: manifest.path.display().to_string(),
    })
}

fn descriptor_for<'a>(
    registry: &'a CapabilityRegistry,
    capability: &CapabilityName,
) -> Result<&'a CapabilityDescriptor, EvaError> {
    registry.get(capability).ok_or_else(|| {
        EvaError::not_found("capability is not registered")
            .with_context("capability", capability.as_str())
    })
}

fn permissions_for_plan(plan: &CapabilityProviderPlan) -> PermissionSet {
    let mut permissions = PermissionSet::deny_all().allow_capability(plan.capability.clone());
    for capability in &plan.required_adapter_capabilities {
        permissions = permissions.allow_capability(capability.clone());
    }
    for provider in &plan.manifest_allowed_providers {
        permissions = permissions.allow_adapter(provider.clone());
    }
    permissions
}

fn probe_providers(
    runtime: &AdapterRuntime,
    plan: &CapabilityProviderPlan,
) -> Vec<ProviderProbeEntry> {
    if plan.providers.is_empty() {
        return Vec::new();
    }
    plan.providers
        .iter()
        .map(
            |candidate| match runtime.probe_adapter(&candidate.provider) {
                Ok(report) => ProviderProbeEntry {
                    provider: candidate.provider.as_str().to_owned(),
                    source: candidate.source.as_str().to_owned(),
                    status: report.status,
                    transport: Some(report.transport.as_str().to_owned()),
                    detail: report.detail,
                },
                Err(error) => ProviderProbeEntry {
                    provider: candidate.provider.as_str().to_owned(),
                    source: candidate.source.as_str().to_owned(),
                    status: "blocked".to_owned(),
                    transport: None,
                    detail: error.message().to_owned(),
                },
            },
        )
        .collect()
}

fn runtime_policy_decisions(
    project: &ProjectConfig,
    runtime: &AdapterRuntime,
    plan: &CapabilityProviderPlan,
) -> Result<Vec<PolicyDecision>, EvaError> {
    if plan.providers.is_empty() {
        return Ok(Vec::new());
    }
    let gate = RuntimePolicyGate::from_project(project)?;
    let mut decisions = Vec::new();
    for candidate in &plan.providers {
        let Some(handle) = runtime.registry().get(&candidate.provider) else {
            return Err(EvaError::not_found("Adapter provider does not exist")
                .with_context("adapter_id", candidate.provider.as_str()));
        };
        decisions.push(
            gate.decide(
                RuntimePolicyRequest::new(HighRiskAction::AdapterInvoke)
                    .with_capability(plan.capability.clone())
                    .with_provider(candidate.provider.clone())
                    .with_timeout_ms(handle.timeout_ms.unwrap_or(0)),
            ),
        );
        if handle.transport == AdapterTransport::Skill {
            decisions.push(
                gate.decide(
                    RuntimePolicyRequest::new(HighRiskAction::SkillRun)
                        .with_tool(handle.skill_runtime_gate.as_deref().unwrap_or(""))
                        .with_capability(plan.capability.clone())
                        .with_provider(candidate.provider.clone()),
                ),
            );
        }
    }
    Ok(decisions)
}

fn provider_plan_entries(plan: &CapabilityProviderPlan) -> Vec<ProviderPlanEntry> {
    plan.providers
        .iter()
        .map(|candidate| ProviderPlanEntry {
            provider: candidate.provider.as_str().to_owned(),
            source: candidate.source.as_str().to_owned(),
        })
        .collect()
}

fn set_capability_once(slot: &mut Option<String>, value: String) -> Result<(), EvaError> {
    if slot.is_some() {
        return Err(EvaError::invalid_argument("duplicate capability"));
    }
    *slot = Some(value);
    Ok(())
}

fn write_capability_list<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &CapabilityListReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva capabilities").map_err(write_error_kind)?;
            for capability in &report.capabilities {
                writeln!(
                    writer,
                    "  - {} kind={} enabled={} provider={} providers={}",
                    capability.capability,
                    capability.kind,
                    capability.enabled,
                    capability.provider,
                    capability
                        .providers
                        .iter()
                        .map(|provider| provider.provider.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "capability.list",
                EXIT_OK,
                &capability_list_json(report),
                trace,
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_capability_probe<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &CapabilityProbeReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Capability probe").map_err(write_error_kind)?;
            writeln!(writer, "capability: {}", report.capability).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "providers: {}",
                report
                    .providers
                    .iter()
                    .map(|provider| provider.provider.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "capability.probe",
                EXIT_OK,
                &capability_probe_json(report),
                trace,
            )
        )
        .map_err(write_error_kind),
    }
}

fn write_capability_call<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &CapabilityCallReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Capability call").map_err(write_error_kind)?;
            writeln!(writer, "request: {}", report.request_id).map_err(write_error_kind)?;
            writeln!(writer, "capability: {}", report.capability).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "invocation_executed: {}",
                report.invocation_executed
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", report.mutation_executed)
                .map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "capability.call",
                EXIT_OK,
                &capability_call_json(report),
                trace,
            )
        )
        .map_err(write_error_kind),
    }
}

fn capability_list_json(report: &CapabilityListReport) -> String {
    let entries = report.capabilities.iter().map(capability_list_entry_json);
    format!("{{\"capabilities\":{}}}", json_array(entries))
}

fn capability_list_entry_json(entry: &CapabilityListEntry) -> String {
    format!(
        "{{\"manifest_id\":{},\"name\":{},\"version\":{},\"capability\":{},\"kind\":{},\"enabled\":{},\"provider\":{},\"providers\":{},\"required_adapter_capabilities\":{},\"manifest_path\":{}}}",
        json_string(&entry.manifest_id),
        json_string(&entry.name),
        json_string(&entry.version),
        json_string(&entry.capability),
        json_string(&entry.kind),
        entry.enabled,
        json_string(&entry.provider),
        json_array(entry.providers.iter().map(provider_plan_entry_json)),
        json_array(
            entry
                .required_adapter_capabilities
                .iter()
                .map(|capability| json_string(capability))
        ),
        json_string(&entry.manifest_path)
    )
}

fn capability_probe_json(report: &CapabilityProbeReport) -> String {
    format!(
        "{{\"status\":{},\"capability\":{},\"provider_plan\":{},\"providers\":{},\"permission_gate\":{}}}",
        json_string(&report.status),
        json_string(&report.capability),
        provider_plan_json(&report.provider_plan),
        json_array(report.providers.iter().map(provider_probe_entry_json)),
        gate_json(&report.permission_gate)
    )
}

fn capability_call_json(report: &CapabilityCallReport) -> String {
    format!(
        "{{\"status\":{},\"request_id\":{},\"capability\":{},\"input_size\":{},\"provider_plan\":{},\"permission_gate\":{},\"runtime_policy\":{},\"confirmed\":{},\"invocation_executed\":{},\"mutation_executed\":{},\"response\":{}}}",
        json_string(&report.status),
        json_string(&report.request_id),
        json_string(&report.capability),
        report.input_size,
        provider_plan_json(&report.provider_plan),
        gate_json(&report.permission_gate),
        json_array(report.runtime_policy.iter().map(policy_decision_json)),
        report.confirmed,
        report.invocation_executed,
        report.mutation_executed,
        report
            .response
            .as_ref()
            .map(invoke_response_json)
            .unwrap_or_else(|| "null".to_owned())
    )
}

fn provider_plan_json(plan: &CapabilityProviderPlan) -> String {
    format!(
        "{{\"capability\":{},\"providers\":{},\"manifest_allowed_providers\":{},\"required_adapter_capabilities\":{}}}",
        json_string(plan.capability.as_str()),
        json_array(plan.providers.iter().map(|candidate| {
            format!(
                "{{\"provider\":{},\"source\":{}}}",
                json_string(candidate.provider.as_str()),
                json_string(candidate.source.as_str())
            )
        })),
        json_array(
            plan.manifest_allowed_providers
                .iter()
                .map(|provider| json_string(provider.as_str()))
        ),
        json_array(
            plan.required_adapter_capabilities
                .iter()
                .map(|capability| json_string(capability.as_str()))
        )
    )
}

fn provider_plan_entry_json(entry: &ProviderPlanEntry) -> String {
    format!(
        "{{\"provider\":{},\"source\":{}}}",
        json_string(&entry.provider),
        json_string(&entry.source)
    )
}

fn provider_probe_entry_json(entry: &ProviderProbeEntry) -> String {
    format!(
        "{{\"provider\":{},\"source\":{},\"status\":{},\"transport\":{},\"detail\":{}}}",
        json_string(&entry.provider),
        json_string(&entry.source),
        json_string(&entry.status),
        option_json(entry.transport.as_deref()),
        json_string(&entry.detail)
    )
}

fn gate_json(gate: &GateReport) -> String {
    format!(
        "{{\"allowed\":{},\"reason\":{}}}",
        gate.allowed,
        json_string(&gate.reason)
    )
}

fn policy_decision_json(decision: &PolicyDecision) -> String {
    format!(
        "{{\"action\":{},\"allowed\":{},\"reason\":{},\"audit\":{}}}",
        json_string(decision.action.as_str()),
        decision.allowed,
        json_string(&decision.reason),
        json_array(decision.audit.iter().map(|entry| json_string(entry)))
    )
}

fn invoke_response_json(response: &InvokeResponse) -> String {
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
            .map(|error| {
                format!(
                    "{{\"kind\":{},\"message\":{}}}",
                    json_string(error.kind().as_str()),
                    json_string(error.message())
                )
            })
            .unwrap_or_else(|| "null".to_owned())
    )
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
