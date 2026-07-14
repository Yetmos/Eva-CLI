//! Capability 注册、provider 规划、探测和受控调用子命令。

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

/// 未显式指定时使用的已知 capability。
const DEFAULT_CAPABILITY: &str = "repo.analyze";
/// Capability 调用的默认审计请求 ID。
const DEFAULT_REQUEST_ID: &str = "req-capability-1";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability 子命令及其已解析选项。
pub(super) enum CapabilityCommand {
    /// 列出已注册 capability 与 provider 计划。
    List(
        /// capability 列表命令共享的项目根目录与输出格式。
        CommonOptions,
    ),
    /// 探测 capability 的候选 provider。
    Probe(
        /// 已解析的 capability 名称、可选 Adapter 过滤条件与公共选项。
        CapabilityProbeOptions,
    ),
    /// dry-run 或在显式确认后调用 capability。
    Call(
        /// 已解析的 capability 调用目标、载荷、确认标志与公共选项。
        CapabilityCallOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability 探测选项。
pub(super) struct CapabilityProbeOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 要探测的规范 capability 名称。
    capability: String,
    /// 可选显式 provider Adapter ID。
    provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability 调用选项，默认保持 dry-run 语义。
pub(super) struct CapabilityCallOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 要调用的 capability 名称。
    capability: String,
    /// 可选显式 provider Adapter ID。
    provider: Option<String>,
    /// 调用输入文本。
    input: String,
    /// 调用和审计请求 ID。
    request_id: String,
    /// 必须与 request ID 精确匹配的可选确认值。
    confirm: Option<String>,
    /// 即使确认存在也禁止执行调用的显式 dry-run 标志。
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability 列表报告。
struct CapabilityListReport {
    /// 按项目 manifest 顺序排列的 capability 项。
    capabilities: Vec<CapabilityListEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability manifest 与派生 provider 计划的输出投影。
struct CapabilityListEntry {
    /// Manifest 稳定 ID。
    manifest_id: String,
    /// 面向用户的名称。
    name: String,
    /// Manifest 版本。
    version: String,
    /// 规范 capability 名称。
    capability: String,
    /// Capability 实现类别。
    kind: String,
    /// Manifest 启用状态。
    enabled: bool,
    /// Descriptor 声明的主 provider 文本。
    provider: String,
    /// 按优先级排序的 provider 候选。
    providers: Vec<ProviderPlanEntry>,
    /// Provider 必须具备的 Adapter capability。
    required_adapter_capabilities: Vec<String>,
    /// Manifest 来源路径。
    manifest_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability 探测结果及权限门禁证据。
struct CapabilityProbeReport {
    /// ready 或 degraded 状态。
    status: String,
    /// 已探测 capability 名称。
    capability: String,
    /// Router 生成的 provider 计划。
    provider_plan: CapabilityProviderPlan,
    /// 各候选 provider 的探测结果。
    providers: Vec<ProviderProbeEntry>,
    /// Manifest 派生权限门禁结果。
    permission_gate: GateReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capability dry-run 或真实调用的完整报告。
struct CapabilityCallReport {
    /// executed 或 dry_run 状态。
    status: String,
    /// 调用请求 ID。
    request_id: String,
    /// 规范 capability 名称。
    capability: String,
    /// 输入 UTF-8 字节数。
    input_size: usize,
    /// Router 生成的 provider 计划。
    provider_plan: CapabilityProviderPlan,
    /// Manifest 派生权限门禁结果。
    permission_gate: GateReport,
    /// 各 provider 的运行时策略决策。
    runtime_policy: Vec<PolicyDecision>,
    /// 确认值是否与请求 ID 匹配。
    confirmed: bool,
    /// 是否实际调用了 host/runtime。
    invocation_executed: bool,
    /// 是否发生外部状态变更；当前 capability CLI 路径固定为 false。
    mutation_executed: bool,
    /// 真实调用时的可选响应。
    response: Option<InvokeResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Provider 计划中的候选及来源。
struct ProviderPlanEntry {
    /// 适配器提供方标识。
    provider: String,
    /// 候选来源，例如 manifest 或显式覆盖。
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 单个 provider 的只读探测结果。
struct ProviderProbeEntry {
    /// 适配器提供方标识。
    provider: String,
    /// 候选来源。
    source: String,
    /// 探测状态。
    status: String,
    /// 可用时的 Adapter transport。
    transport: Option<String>,
    /// 成功详情或失败消息。
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 简化的权限门禁结果。
struct GateReport {
    /// 派生权限是否允许计划。
    allowed: bool,
    /// 可审计的允许或拒绝原因。
    reason: String,
}

/// 解析 `capability list|probe|call` 子命令。
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

/// 加载项目后执行 Capability 命令；所有门禁拒绝通过统一错误契约返回。
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

/// 解析 capability/provider 探测选项，并校验两个强类型名称。
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

/// 解析调用输入、请求、确认与 dry-run 选项；重复 capability 由专用 setter 拒绝。
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

/// 按 manifest 顺序将项目 capability 与注册表 descriptor 合并为列表报告。
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

/// 生成 provider 计划、派生最小权限并逐个探测候选。
/// 权限门禁在探测前执行，避免未授权 provider 即使只读也被访问。
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

/// 规划并可选执行 capability 调用。
///
/// 确认必须与 request ID 精确匹配；Manifest 权限和 RuntimePolicyGate 均在 host 调用前
/// 完成。只有 `confirmed && !dry_run` 才执行，其他路径仍返回完整计划而无副作用。
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

/// 从项目 manifest 构建 CapabilityRegistry，重复或无效 descriptor 直接失败。
fn capability_registry_from_project(
    project: &ProjectConfig,
) -> Result<CapabilityRegistry, EvaError> {
    let mut registry = CapabilityRegistry::new();
    for manifest in &project.capabilities {
        registry.register(CapabilityDescriptor::from_manifest(manifest))?;
    }
    Ok(registry)
}

/// 将单个 manifest 与其 descriptor/provider 计划投影为列表项。
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

/// 精确查找 capability descriptor；未注册时返回带名称上下文的 NotFound。
fn descriptor_for<'a>(
    registry: &'a CapabilityRegistry,
    capability: &CapabilityName,
) -> Result<&'a CapabilityDescriptor, EvaError> {
    registry.get(capability).ok_or_else(|| {
        EvaError::not_found("capability is not registered")
            .with_context("capability", capability.as_str())
    })
}

/// 从 provider 计划派生最小权限集合，只允许目标 capability、依赖能力和 manifest provider。
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

/// 逐个探测 provider；单个 provider 故障降为 blocked 项而不丢失其他候选结果。
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

/// 为每个 provider 构造 AdapterInvoke 决策，并对 Skill transport 追加 SkillRun 门禁。
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

/// 将内部 provider 候选投影为稳定输出项。
fn provider_plan_entries(plan: &CapabilityProviderPlan) -> Vec<ProviderPlanEntry> {
    plan.providers
        .iter()
        .map(|candidate| ProviderPlanEntry {
            provider: candidate.provider.as_str().to_owned(),
            source: candidate.source.as_str().to_owned(),
        })
        .collect()
}

/// 设置唯一 capability 参数，拒绝位置参数与选项重复指定。
fn set_capability_once(slot: &mut Option<String>, value: String) -> Result<(), EvaError> {
    if slot.is_some() {
        return Err(EvaError::invalid_argument("duplicate capability"));
    }
    *slot = Some(value);
    Ok(())
}

/// 输出 capability 清单和 provider 计划。
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

/// 输出 provider 探测与权限门禁结果。
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

/// 输出调用计划、确认、执行事实、策略决策和可选响应。
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

/// 将 Capability 列表报告编码为 JSON。
fn capability_list_json(report: &CapabilityListReport) -> String {
    let entries = report.capabilities.iter().map(capability_list_entry_json);
    format!("{{\"capabilities\":{}}}", json_array(entries))
}

/// 将单个 Capability 列表项编码为 JSON。
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

/// 将 Capability 探测报告编码为 JSON。
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

/// 将 Capability 调用报告编码为 JSON，明确区分 invocation 与 mutation。
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

/// 将 provider 计划及 allowlist/依赖字段编码为 JSON。
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

/// 将单个 provider 计划候选编码为 JSON。
fn provider_plan_entry_json(entry: &ProviderPlanEntry) -> String {
    format!(
        "{{\"provider\":{},\"source\":{}}}",
        json_string(&entry.provider),
        json_string(&entry.source)
    )
}

/// 将单个 provider 探测结果编码为 JSON。
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

/// 将简化门禁结果编码为 JSON。
fn gate_json(gate: &GateReport) -> String {
    format!(
        "{{\"allowed\":{},\"reason\":{}}}",
        gate.allowed,
        json_string(&gate.reason)
    )
}

/// 将 RuntimePolicyGate 决策及审计字段编码为 JSON。
fn policy_decision_json(decision: &PolicyDecision) -> String {
    format!(
        "{{\"action\":{},\"allowed\":{},\"reason\":{},\"audit\":{}}}",
        json_string(decision.action.as_str()),
        decision.allowed,
        json_string(&decision.reason),
        json_array(decision.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将 InvokeResponse 的状态、输出和结构化错误编码为 JSON。
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

/// 将 InvokeStatus 映射为稳定小写状态值。
fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
    }
}
