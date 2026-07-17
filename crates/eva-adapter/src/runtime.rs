//! 适配器运行时的统一授权调用入口。
//!
//! 外部进程和网络传输必须先经路由、监督器准入与高风险策略判定，运行时才签发仅绑定本次
//! 请求的凭据作用域。无论传输成功还是失败，已取得的执行槽都会进入完成态；可观测性写入
//! 采用尽力而为语义，不会覆盖适配器调用本身的结果。
//! Authorized Adapter runtime probes and controlled invocation envelopes.

use crate::manifest::AdapterHandle;
use crate::process_backend::{OsProcessBackend, ProviderProcessHandle, ProviderProcessSpawner};
use crate::registry::AdapterRegistry;
use crate::restart::{decide_restart, due_at_ms, RestartDecision, RestartOutcome};
use crate::router::{AdapterRouteRequest, AdapterRouter};
use crate::supervisor::{
    InMemoryProviderSupervisor, ProviderCredentialScope, ProviderExecutionOutcome,
    ProviderExecutionRequest, ProviderExecutionSlot, ProviderSupervisor,
};
use crate::transports;
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind,
    MetricLabels, MetricName, MetricPoint, MetricSink, SpanId, TraceFields,
};
use eva_policy::{HighRiskAction, PolicyDomainSet, RuntimePolicyGate, RuntimePolicyRequest};
use eva_storage::{FileSystemProviderProcessTable, ProviderProcessSnapshot};
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "authorized transport execution with timeout and audit";

/// Spawns a provider through the OS backend and immediately fences its real
/// identity into the already-admitted durable process record.
struct RegisteredProviderSpawner<'a> {
    backend: OsProcessBackend,
    supervisor: &'a RefCell<InMemoryProviderSupervisor>,
    slot: &'a ProviderExecutionSlot,
}

enum ProviderRestartStep {
    Retry {
        delay_ms: u64,
        snapshot: ProviderProcessSnapshot,
    },
    Terminal(ProviderProcessSnapshot),
}

impl ProviderProcessSpawner for RegisteredProviderSpawner<'_> {
    fn spawn_provider(&self, command: Command) -> Result<ProviderProcessHandle, EvaError> {
        let mut process = self.backend.spawn_provider(command)?;
        let identity = process.identity().clone();
        let registration = self
            .supervisor
            .borrow_mut()
            .register_process_identity(self.slot, &identity);
        if let Err(error) = registration {
            let _ = process.terminate();
            return Err(error
                .with_context("session_id", &self.slot.session_id)
                .with_context("pid", identity.pid.to_string()));
        }
        Ok(process)
    }
}

/// 描述一次尚未路由的能力调用及其调用方可设置参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInvocation {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `provider` 字段对应的值。
    pub provider: Option<AdapterId>,
    /// 记录 `input` 字段对应的值。
    pub input: String,
    /// 保存运行时监督器签发的内部凭据作用域；公共调用方不能直接构造或注入该值。
    credential_scope: Option<ProviderCredentialScope>,
}

/// 表示 `AdapterProbeReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterProbeReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `transport` 字段对应的值。
    pub transport: AdapterTransport,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `capabilities` 字段对应的值。
    pub capabilities: Vec<CapabilityName>,
    /// 记录 `detail` 字段对应的值。
    pub detail: String,
}

/// 表示 `AdapterInvokeReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInvokeReport {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `transport` 字段对应的值。
    pub transport: AdapterTransport,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `output` 字段对应的值。
    pub output: String,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
    /// 记录 `trace` 字段对应的值。
    pub trace: TraceFields,
}

/// 聚合适配器路由、策略判定、进程监督和可选可观测性后端的调用运行时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRuntime {
    /// 保存从项目清单解析出的已授权适配器句柄。
    registry: AdapterRegistry,
    /// 按能力与显式提供者请求选择登记句柄。
    router: AdapterRouter,
    /// 以内部可变性串行推进执行槽、限流窗口和熔断状态。
    supervisor: RefCell<InMemoryProviderSupervisor>,
    /// 在签发外部提供者凭据前执行高风险策略判定。
    policy_gate: RuntimePolicyGate,
    /// 指定尽力而为的审计、指标和追踪落盘目录；未配置时不产生落盘副作用。
    observability_backend: Option<PathBuf>,
}

impl AdapterInvocation {
    /// 创建并初始化当前类型的实例。
    pub fn new(request_id: RequestId, capability: CapabilityName) -> Self {
        Self {
            request_id,
            capability,
            provider: None,
            input: String::new(),
            credential_scope: None,
        }
    }

    /// 设置 `provider` 并返回更新后的实例。
    pub fn with_provider(mut self, provider: AdapterId) -> Self {
        self.provider = Some(provider);
        self
    }

    /// 设置 `input` 并返回更新后的实例。
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    /// 设置 `credential_scope` 并返回更新后的实例。
    pub(crate) fn with_credential_scope(mut self, scope: ProviderCredentialScope) -> Self {
        self.credential_scope = Some(scope);
        self
    }

    /// 执行 `credential_scope` 对应的处理逻辑。
    pub fn credential_scope(&self) -> Option<&ProviderCredentialScope> {
        self.credential_scope.as_ref()
    }

    /// 执行 `trace_for_adapter` 对应的处理逻辑。
    pub fn trace_for_adapter(&self, adapter_id: &AdapterId) -> TraceFields {
        TraceFields::default()
            .with_request_id(self.request_id.clone())
            .with_adapter_id(adapter_id.clone())
            .with_capability(self.capability.clone())
            .with_provider(adapter_id.as_str())
            .with_span_id(
                SpanId::parse("adapter.invoke")
                    .expect("static adapter span identifiers use the observability character set"),
            )
    }
}

impl AdapterRuntime {
    /// 根据输入构造当前类型，作为 `from_registry` 的标准入口。
    pub fn from_registry(registry: AdapterRegistry) -> Self {
        Self::from_registry_with_policy(
            registry,
            RuntimePolicyGate::new(PolicyDomainSet::default()),
        )
    }

    /// 根据输入构造当前类型，作为 `from_registry_with_policy` 的标准入口。
    fn from_registry_with_policy(
        registry: AdapterRegistry,
        policy_gate: RuntimePolicyGate,
    ) -> Self {
        Self::from_registry_with_policy_and_supervisor(
            registry,
            policy_gate,
            InMemoryProviderSupervisor::new(),
        )
    }

    /// 根据输入构造当前类型，作为 `from_registry_with_policy_and_supervisor` 的标准入口。
    fn from_registry_with_policy_and_supervisor(
        registry: AdapterRegistry,
        policy_gate: RuntimePolicyGate,
        supervisor: InMemoryProviderSupervisor,
    ) -> Self {
        let router = AdapterRouter::new(registry.clone());
        Self {
            registry,
            router,
            supervisor: RefCell::new(supervisor),
            policy_gate,
            observability_backend: None,
        }
    }

    #[cfg(test)]
    /// Builds a runtime around an explicitly prepared supervisor for admission tests.
    pub(crate) fn from_registry_with_supervisor_for_test(
        registry: AdapterRegistry,
        supervisor: InMemoryProviderSupervisor,
    ) -> Self {
        Self::from_registry_with_policy_and_supervisor(
            registry,
            RuntimePolicyGate::new(PolicyDomainSet::default()),
            supervisor,
        )
    }

    /// 根据输入构造当前类型，作为 `from_project` 的标准入口。
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        let registry = AdapterRegistry::from_project(project)?;
        let policy_gate = RuntimePolicyGate::from_project(project)?;
        Ok(Self::from_registry_with_policy(registry, policy_gate)
            .with_observability_backend(default_observability_backend(project)))
    }

    /// 根据输入构造当前类型，作为 `from_project_with_provider_process_table` 的标准入口。
    pub fn from_project_with_provider_process_table(
        project: &ProjectConfig,
        process_table: FileSystemProviderProcessTable,
    ) -> Result<Self, EvaError> {
        let registry = AdapterRegistry::from_project(project)?;
        let policy_gate = RuntimePolicyGate::from_project(project)?;
        Ok(Self::from_registry_with_policy_and_supervisor(
            registry,
            policy_gate,
            InMemoryProviderSupervisor::with_process_table(process_table),
        )
        .with_observability_backend(default_observability_backend(project)))
    }

    /// 根据输入构造当前类型，作为 `from_registry_with_provider_process_table` 的标准入口。
    pub fn from_registry_with_provider_process_table(
        registry: AdapterRegistry,
        process_table: FileSystemProviderProcessTable,
    ) -> Self {
        Self::from_registry_with_policy_and_supervisor(
            registry,
            RuntimePolicyGate::new(PolicyDomainSet::default()),
            InMemoryProviderSupervisor::with_process_table(process_table),
        )
    }

    /// 执行 `registry` 对应的处理逻辑。
    pub fn registry(&self) -> &AdapterRegistry {
        &self.registry
    }

    /// 执行 `router` 对应的受控流程。
    pub fn router(&self) -> &AdapterRouter {
        &self.router
    }

    /// 返回 `list` 对应的数据视图。
    pub fn list(&self) -> Vec<&crate::manifest::AdapterHandle> {
        self.registry.list()
    }

    /// 执行 `provider_processes` 对应的处理逻辑。
    pub fn provider_processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        self.supervisor.borrow().processes()
    }

    /// 设置 `observability_backend` 并返回更新后的实例。
    pub fn with_observability_backend(mut self, root: impl Into<PathBuf>) -> Self {
        self.observability_backend = Some(root.into());
        self
    }

    /// 执行 `probe_adapter` 对应的处理逻辑。
    pub fn probe_adapter(&self, adapter_id: &AdapterId) -> Result<AdapterProbeReport, EvaError> {
        let handle = self.registry.get(adapter_id).ok_or_else(|| {
            EvaError::not_found("Adapter provider does not exist")
                .with_context("adapter_id", adapter_id.as_str())
        })?;
        Ok(AdapterProbeReport {
            adapter_id: handle.id.clone(),
            transport: handle.transport,
            status: handle.health().as_str().to_owned(),
            capabilities: handle.capabilities.clone(),
            detail: if handle.enabled {
                "authorized handle is registered; probe has no external side effects".to_owned()
            } else {
                "adapter manifest is disabled".to_owned()
            },
        })
    }

    /// 执行 `probe_capability` 对应的处理逻辑。
    pub fn probe_capability(
        &self,
        capability: CapabilityName,
        provider: Option<AdapterId>,
    ) -> Result<AdapterProbeReport, EvaError> {
        let mut request = AdapterRouteRequest::new(capability);
        if let Some(provider) = provider {
            request = request.with_provider(provider);
        }
        let route = self.router.route(&request)?;
        self.probe_adapter(&route.handle.id)
    }

    /// 路由并执行调用；拒绝调用方伪造的凭据作用域，再按传输类型决定是否进入监督流程。
    pub fn invoke(&self, invocation: AdapterInvocation) -> Result<AdapterInvokeReport, EvaError> {
        if invocation.credential_scope().is_some() {
            return Err(EvaError::permission_denied(
                "caller supplied provider credential scope is not allowed",
            ));
        }
        let mut request = AdapterRouteRequest::new(invocation.capability.clone());
        if let Some(provider) = invocation.provider.clone() {
            request = request.with_provider(provider);
        }
        let route = self.router.route(&request)?;
        let handle = route.handle;

        if should_supervise(handle.transport) {
            return self.invoke_supervised(handle, invocation);
        }
        dispatch_transport(&handle, invocation)
    }

    /// 在执行槽和策略授权边界内调用外部提供者。
    ///
    /// 准入失败时不会创建凭据作用域；取得槽后，即使策略拒绝或传输失败也必须调用
    /// `complete` 释放槽并更新熔断状态。完成记录失败会作为本次调用错误返回，因为此时
    /// 无法保证监督器看到的生命周期与实际执行一致。
    fn invoke_supervised(
        &self,
        handle: crate::manifest::AdapterHandle,
        invocation: AdapterInvocation,
    ) -> Result<AdapterInvokeReport, EvaError> {
        let provider_trace = invocation.trace_for_adapter(&handle.id);
        let execution_request = ProviderExecutionRequest::from_handle(&handle, &invocation)
            .with_retry_backoff_ms(
                self.policy_gate
                    .adapter_retry_backoff_ms(&invocation.capability),
            );
        // 先占用受监督执行槽，确保后续凭据会话受并发、速率和熔断限制约束。
        let slot = match self.supervisor.borrow_mut().acquire(execution_request) {
            Ok(slot) => slot,
            Err(error) => {
                self.record_provider_observability(
                    &handle,
                    &provider_trace,
                    "admission_failed",
                    AuditOutcome::Blocked,
                    Some(&error),
                    None,
                );
                return Err(error);
            }
        };
        let credential_scope =
            ProviderCredentialScope::from_slot(&slot, invocation.capability.clone());
        let policy_decision = self.policy_gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::ProviderCredentialSession)
                .with_adapter(handle.id.clone())
                .with_provider(slot.adapter_id.clone())
                .with_capability(invocation.capability.clone()),
        );
        let policy_audit = policy_decision.audit.clone();
        if !policy_decision.allowed {
            // 策略拒绝发生在槽创建之后，因此必须先完成失败快照再向调用方返回拒绝。
            let snapshot = self.supervisor.borrow_mut().complete(
                &slot,
                ProviderExecutionOutcome {
                    health: "failed".to_owned(),
                    last_error: Some(policy_decision.reason.clone()),
                },
            )?;
            let error = policy_decision
                .ensure_allowed()
                .expect_err("denied provider credential session returns an error");
            self.record_provider_observability(
                &handle,
                &provider_trace,
                "policy_denied",
                AuditOutcome::Blocked,
                Some(&error),
                Some(&snapshot),
            );
            return Err(error);
        }
        let invocation = invocation.with_credential_scope(credential_scope);
        loop {
            let process_spawner = RegisteredProviderSpawner {
                backend: OsProcessBackend::new(),
                supervisor: &self.supervisor,
                slot: &slot,
            };
            let result = dispatch_transport_with_spawner(
                &handle,
                invocation.clone(),
                Some(&process_spawner),
            );
            match result {
                Ok(mut report) if report.status == "completed" => {
                    let snapshot = self
                        .supervisor
                        .borrow_mut()
                        .complete(&slot, ProviderExecutionOutcome::completed(&report.status))?;
                    report.audit.extend(policy_audit.clone());
                    append_supervisor_audit(&mut report.audit, &snapshot);
                    self.record_provider_observability(
                        &handle,
                        &report.trace,
                        &report.status,
                        AuditOutcome::Ok,
                        None,
                        Some(&snapshot),
                    );
                    return Ok(report);
                }
                Ok(mut report) => {
                    let reason = format!("adapter returned status {}", report.status);
                    let completed = self
                        .supervisor
                        .borrow_mut()
                        .complete(&slot, ProviderExecutionOutcome::completed(&report.status))?;
                    match self.restart_after_failure(
                        &handle,
                        &slot,
                        &completed,
                        &reason,
                        report_status_restart_eligible(&report.status),
                    )? {
                        ProviderRestartStep::Retry { delay_ms, snapshot } => {
                            self.record_provider_observability(
                                &handle,
                                &report.trace,
                                "restart_pending",
                                AuditOutcome::Planned,
                                None,
                                Some(&snapshot),
                            );
                            thread_sleep_restart(delay_ms);
                            self.supervisor
                                .borrow_mut()
                                .prepare_restart(&slot, epoch_ms())?;
                        }
                        ProviderRestartStep::Terminal(snapshot) => {
                            report.audit.extend(policy_audit.clone());
                            append_supervisor_audit(&mut report.audit, &snapshot);
                            self.record_provider_observability(
                                &handle,
                                &report.trace,
                                &report.status,
                                AuditOutcome::Failed,
                                None,
                                Some(&snapshot),
                            );
                            return Ok(report);
                        }
                    }
                }
                Err(error) => {
                    let completed = self
                        .supervisor
                        .borrow_mut()
                        .complete(&slot, ProviderExecutionOutcome::failed(&error))?;
                    match self.restart_after_failure(
                        &handle,
                        &slot,
                        &completed,
                        error.message(),
                        error.is_retryable(),
                    )? {
                        ProviderRestartStep::Retry { delay_ms, snapshot } => {
                            self.record_provider_observability(
                                &handle,
                                &provider_trace,
                                "restart_pending",
                                AuditOutcome::Planned,
                                Some(&error),
                                Some(&snapshot),
                            );
                            thread_sleep_restart(delay_ms);
                            self.supervisor
                                .borrow_mut()
                                .prepare_restart(&slot, epoch_ms())?;
                        }
                        ProviderRestartStep::Terminal(snapshot) => {
                            self.record_provider_observability(
                                &handle,
                                &provider_trace,
                                "failed",
                                AuditOutcome::Failed,
                                Some(&error),
                                Some(&snapshot),
                            );
                            return Err(error
                                .with_context(
                                    "restart_attempts",
                                    snapshot.restart_attempts.to_string(),
                                )
                                .with_context(
                                    "restart_max_attempts",
                                    snapshot.restart_max_attempts.to_string(),
                                )
                                .with_context("restart_state", snapshot.restart_state));
                        }
                    }
                }
            }
        }
    }

    fn restart_after_failure(
        &self,
        handle: &AdapterHandle,
        slot: &ProviderExecutionSlot,
        snapshot: &ProviderProcessSnapshot,
        reason: &str,
        eligible: bool,
    ) -> Result<ProviderRestartStep, EvaError> {
        if !eligible {
            let failed = self.supervisor.borrow_mut().fail_restart(slot, reason)?;
            return Ok(ProviderRestartStep::Terminal(failed));
        }
        match decide_restart(
            handle.provider.restart,
            snapshot.restart_attempts,
            RestartOutcome::Failure,
            &slot.session_id,
        ) {
            RestartDecision::NoRestart => Ok(ProviderRestartStep::Terminal(snapshot.clone())),
            RestartDecision::BudgetExhausted => {
                let exhausted = self.supervisor.borrow_mut().exhaust_restart(slot, reason)?;
                Ok(ProviderRestartStep::Terminal(exhausted))
            }
            RestartDecision::Restart { attempt, delay_ms } => {
                let due_at_ms = due_at_ms(epoch_ms(), delay_ms);
                let pending = self
                    .supervisor
                    .borrow_mut()
                    .schedule_restart(slot, attempt, due_at_ms, reason)?;
                Ok(ProviderRestartStep::Retry {
                    delay_ms,
                    snapshot: pending,
                })
            }
        }
    }

    /// 登记 `record_provider_observability` 对应的数据或状态。
    fn record_provider_observability(
        &self,
        handle: &AdapterHandle,
        trace: &TraceFields,
        status: &str,
        outcome: AuditOutcome,
        error: Option<&EvaError>,
        snapshot: Option<&ProviderProcessSnapshot>,
    ) {
        let Some(root) = &self.observability_backend else {
            return;
        };
        let Ok(span_id) = SpanId::parse("provider.supervisor.invoke") else {
            return;
        };
        let mut pipeline = BestEffortObservabilityPipeline::open(root);
        let observed_trace = trace.child_span(span_id);
        let mut event = AuditEvent::new(
            AuditAction::ProviderSupervised,
            outcome,
            observed_trace.clone(),
        )
        .with_message("provider supervisor invocation observed")
        .with_field("adapter_id", handle.id.as_str())
        .with_field("transport", handle.transport.as_str())
        .with_field("status", status);
        if let Some(capability) = &trace.capability {
            event = event.with_field("capability", capability.as_str());
        }
        if let Some(snapshot) = snapshot {
            event = event
                .with_field("session_id", snapshot.session_id.as_str())
                .with_field("provider_process_id", snapshot.provider_process_id.as_str())
                .with_field("provider_health", snapshot.health.as_str());
        }
        if let Some(error) = error {
            event = event
                .with_field("error_kind", error.kind().as_str())
                .with_field("error", error.message());
        }
        let _ = AuditSink::record(&mut pipeline, event);

        if let Ok(name) = MetricName::parse("provider.supervisor.invocations") {
            let capability = trace
                .capability
                .as_ref()
                .map(|value| value.as_str())
                .unwrap_or("unknown");
            let _ = MetricSink::record(
                &mut pipeline,
                MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                    MetricLabels::provider(handle.id.as_str(), capability, handle.id.as_str())
                        .with("transport", handle.transport.as_str())
                        .with("status", status)
                        .with("supervised", "true"),
                ),
            );
        }

        let _ = pipeline.export_span(
            "provider.supervisor.invoke",
            &observed_trace,
            &[
                ("component", "adapter"),
                ("transport", handle.transport.as_str()),
                ("status", status),
            ],
        );
    }
}

/// Only transport statuses that represent a transient provider execution
/// failure may consume the durable restart budget. Deterministic protocol
/// conflicts, such as an output limit breach, are terminal and must not
/// launch the provider a second time.
fn report_status_restart_eligible(status: &str) -> bool {
    matches!(status, "failed" | "timeout")
}

/// 执行 `default_observability_backend` 对应的处理逻辑。
fn default_observability_backend(project: &ProjectConfig) -> PathBuf {
    let data_dir = project
        .eva
        .runtime
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".eva/data"));
    if data_dir.is_absolute() {
        data_dir.join("observability")
    } else {
        project.project_root.join(data_dir).join("observability")
    }
}

/// 将已完成路由和授权的调用分派到唯一传输实现，不在此处重复选择提供者。
fn dispatch_transport(
    handle: &crate::manifest::AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    dispatch_transport_with_spawner(handle, invocation, None)
}

/// Dispatch a transport while optionally supplying the central provider
/// process registrar. Direct callers retain the old default OS backend; the
/// supervised runtime passes a slot-bound registrar.
fn dispatch_transport_with_spawner(
    handle: &crate::manifest::AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
) -> Result<AdapterInvokeReport, EvaError> {
    match handle.transport {
        AdapterTransport::Mcp => {
            transports::mcp::invoke_with_spawner(handle, invocation, process_spawner)
        }
        AdapterTransport::Skill => {
            transports::skill::invoke_with_spawner(handle, invocation, process_spawner)
        }
        AdapterTransport::Builtin
        | AdapterTransport::LuaCapability
        | AdapterTransport::Eventbus => transports::builtin::invoke(handle, invocation),
        AdapterTransport::Hardware => transports::hardware::invoke(handle, invocation),
        AdapterTransport::Stdio => {
            transports::stdio::invoke_with_spawner(handle, invocation, process_spawner)
        }
        AdapterTransport::Http => transports::http::invoke(handle, invocation),
    }
}

/// 执行 `should_supervise` 对应的处理逻辑。
fn should_supervise(transport: AdapterTransport) -> bool {
    matches!(
        transport,
        AdapterTransport::Mcp
            | AdapterTransport::Skill
            | AdapterTransport::Stdio
            | AdapterTransport::Http
    )
}

/// 执行 `append_supervisor_audit` 对应的处理逻辑。
fn append_supervisor_audit(audit: &mut Vec<String>, snapshot: &ProviderProcessSnapshot) {
    audit.push(format!("provider.session:{}", snapshot.session_id));
    audit.push(format!("provider.process:{}", snapshot.provider_process_id));
    audit.push(format!(
        "provider.manifest_digest:{}",
        snapshot.manifest_digest
    ));
    audit.push(format!("provider.health:{}", snapshot.health));
    audit.push(format!(
        "provider.restart.attempts:{}",
        snapshot.restart_attempts
    ));
    audit.push(format!(
        "provider.restart.max_attempts:{}",
        snapshot.restart_max_attempts
    ));
    audit.push(format!("provider.restart.state:{}", snapshot.restart_state));
    audit.push("provider.slot:released".to_owned());
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn thread_sleep_restart(delay_ms: u64) {
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{AdapterCircuitBreaker, AdapterHandle};
    use crate::registry::AdapterRegistry;
    use crate::supervisor::{
        ProviderCredentialScope, PROVIDER_SESSION_ID_HEADER, PROVIDER_SESSION_TOKEN_ENV,
        PROVIDER_SESSION_TOKEN_HEADER,
    };
    use eva_config::load_project_config;
    use eva_config::AdapterTransport;
    use eva_core::ErrorKind;
    use eva_storage::{
        DurableBackendOptions, FileSystemDurableBackend, FileSystemProviderProcessTable,
        ProviderProcessTable,
    };
    use std::collections::BTreeMap;
    use std::io::{BufRead, Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::thread;

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 验证 `runtime_invokes_skill_adapter_with_controlled_runner` 场景下的预期行为。
    #[test]
    fn runtime_invokes_skill_adapter_with_controlled_runner() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = AdapterRuntime::from_project(&project).unwrap();
        let invocation = AdapterInvocation::new(
            RequestId::parse("req-skill-1").unwrap(),
            CapabilityName::parse("workflow.code_review").unwrap(),
        )
        .with_provider(AdapterId::parse("code-review-skill").unwrap())
        .with_input("{\"scope\":\"current_diff\"}");

        let report = runtime.invoke(invocation).unwrap();

        assert_eq!(report.status, "completed");
        assert!(report.output.contains("code-review"));
        assert!(report.output.contains("builtin_codex_skill"));
        assert_eq!(
            report.trace.request_id.as_ref().map(|id| id.as_str()),
            Some("req-skill-1")
        );
        assert_eq!(
            report.trace.adapter_id.as_ref().map(|id| id.as_str()),
            Some("code-review-skill")
        );
        assert_eq!(
            report
                .trace
                .capability
                .as_ref()
                .map(|capability| capability.as_str()),
            Some("workflow.code_review")
        );
        assert!(report
            .audit
            .iter()
            .any(|entry| entry.starts_with("provider.session:")));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "provider.health:completed"));
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        assert!(!processes[0].active);
        assert_eq!(processes[0].health, "completed");
        assert_eq!(processes[0].adapter_id.as_str(), "code-review-skill");
    }

    /// 验证 `runtime_invokes_stdio_adapter_with_redacted_env` 场景下的预期行为。
    #[test]
    fn runtime_invokes_stdio_adapter_with_redacted_env() {
        let env_name = "EVA_TEST_STDIO_SECRET_RUNTIME";
        let secret = "stdio-runtime-secret";
        std::env::set_var(env_name, secret);
        let runtime = runtime_with_handle(stdio_handle(
            true,
            test_command(),
            env_echo_args(env_name),
            vec![env_name.to_owned()],
        ));

        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-stdio-runtime").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap();
        std::env::remove_var(env_name);

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(!report.output.contains("eva-provider-session:"));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report
            .audit
            .contains(&format!("credential_env:{env_name}:redacted")));
        assert!(report
            .audit
            .contains(&"credential.session_token:redacted".to_owned()));
        assert!(report
            .audit
            .contains(&"policy.action:provider.credential_session".to_owned()));
        assert!(report.audit.contains(&"shell:false".to_owned()));
        assert!(report.output.contains("\"process_id\":"));
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        let process = &processes[0];
        assert!(!process.active);
        assert!(process.record_version.0 >= 3);
        assert_eq!(process.attempt, 1);
        assert!(process.pid.is_some());
        assert!(process
            .process_start_token
            .as_deref()
            .is_some_and(|token| !token.is_empty()));
        assert!(process.process_group_id.is_some() || process.job_id.is_some());
    }

    #[test]
    fn runtime_crash_loop_never_exceeds_durable_restart_budget() {
        let counter = temp_root("restart-counter").join("attempts");
        std::fs::create_dir_all(counter.parent().unwrap()).unwrap();
        std::env::set_var("EVA_RESTART_COUNTER_FILE", &counter);
        let mut handle = stdio_handle(
            true,
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            vec![
                "--exact".to_owned(),
                "runtime::tests::restart_failure_server_helper".to_owned(),
                "--ignored".to_owned(),
                "--nocapture".to_owned(),
            ],
            Vec::new(),
        );
        handle.provider.restart = eva_config::ProviderRestartConfig {
            mode: eva_config::ProviderRestartMode::OnFailure,
            max_attempts: 2,
            backoff_ms: 1,
        };
        let runtime = runtime_with_handle(handle);
        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-provider-crash-loop").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap();
        std::env::remove_var("EVA_RESTART_COUNTER_FILE");

        assert_eq!(report.status, "failed");
        let attempts = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert_eq!(attempts, 3, "initial spawn plus two durable restarts");
        let process = &runtime.provider_processes().unwrap()[0];
        assert_eq!(process.restart_attempts, 2);
        assert_eq!(process.restart_max_attempts, 2);
        assert_eq!(process.restart_state, "exhausted");
        assert!(!process.active);
        assert!(process
            .audit
            .iter()
            .any(|entry| entry == "provider.restart:budget_exhausted"));
    }

    #[test]
    fn runtime_output_limit_is_terminal_without_restart() {
        let counter = temp_root("output-limit-counter").join("attempts");
        std::fs::create_dir_all(counter.parent().unwrap()).unwrap();
        std::env::set_var("EVA_OUTPUT_LIMIT_COUNTER_FILE", &counter);
        let mut handle = stdio_handle(
            true,
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            vec![
                "--exact".to_owned(),
                "runtime::tests::output_limit_server_helper".to_owned(),
                "--ignored".to_owned(),
                "--nocapture".to_owned(),
            ],
            Vec::new(),
        );
        handle.output_limit_bytes = Some(8);
        handle.provider.restart = eva_config::ProviderRestartConfig {
            mode: eva_config::ProviderRestartMode::OnFailure,
            max_attempts: 2,
            backoff_ms: 1,
        };
        let runtime = runtime_with_handle(handle);
        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-provider-output-limit").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap();
        std::env::remove_var("EVA_OUTPUT_LIMIT_COUNTER_FILE");

        assert_eq!(report.status, "output_limit_exceeded");
        let attempts = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert_eq!(
            attempts, 1,
            "deterministic output overflow must not restart"
        );
        let process = &runtime.provider_processes().unwrap()[0];
        assert_eq!(process.restart_attempts, 0);
        assert_eq!(process.restart_state, "failed");
        assert!(!process.active);
        assert!(process
            .audit
            .iter()
            .any(|entry| entry == "provider.restart:non_retryable"));
    }

    /// Verify MCP stdio uses the same central spawn/register path as plain
    /// stdio and leaves a completed, identity-bearing process record.
    #[test]
    fn runtime_invokes_mcp_stdio_with_registered_process_identity() {
        let runtime = runtime_with_handle(mcp_stdio_handle());
        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-mcp-stdio-runtime").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("mcp-stdio-test").unwrap())
                .with_input("{\"message\":\"hello\"}"),
            )
            .unwrap();

        assert_eq!(report.status, "completed");
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "mcp.stdio:started"));
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        let process = &processes[0];
        assert!(!process.active);
        assert!(process.record_version.0 >= 3);
        assert_eq!(process.attempt, 1);
        assert!(process.pid.is_some());
        assert!(process
            .process_start_token
            .as_deref()
            .is_some_and(|token| !token.is_empty()));
        assert!(process.process_group_id.is_some() || process.job_id.is_some());
    }

    /// A registration CAS failure must terminate the just-spawned process
    /// before the error reaches the transport/runtime completion path.
    #[test]
    fn registered_spawner_terminates_process_when_record_registration_fails() {
        let handle = stdio_handle(true, "registration-helper", Vec::new(), Vec::new());
        let invocation = AdapterInvocation::new(
            RequestId::parse("req-registration-failure").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(&handle, &invocation))
            .unwrap();
        supervisor
            .complete(
                &slot,
                ProviderExecutionOutcome {
                    health: "failed".to_owned(),
                    last_error: Some("forced registration failure".to_owned()),
                },
            )
            .unwrap();
        let supervisor = RefCell::new(supervisor);
        let registrar = RegisteredProviderSpawner {
            backend: OsProcessBackend::new(),
            supervisor: &supervisor,
            slot: &slot,
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "runtime::tests::registration_failure_server_helper",
                "--ignored",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let error = registrar.spawn_provider(command).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        let pid = error
            .context()
            .entries()
            .iter()
            .find(|(key, _)| key == "pid")
            .and_then(|(_, value)| value.parse::<u32>().ok())
            .expect("registration error carries spawned PID");
        wait_until_process_is_gone(pid);
    }

    /// Helper process used by the real MCP stdio registration test.
    #[test]
    #[ignore = "spawned by runtime_invokes_mcp_stdio_with_registered_process_identity"]
    fn mcp_stdio_server_helper() {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.unwrap();
            let response = if line.contains("\"method\":\"initialize\"") {
                Some("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fake-stdio\",\"version\":\"1\"}}}".to_owned())
            } else if line.contains("\"method\":\"tools/list\"") {
                Some("{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"analyze\",\"inputSchema\":{\"type\":\"object\"}}]}}".to_owned())
            } else if line.contains("\"method\":\"tools/call\"") {
                Some("{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}],\"isError\":false}}".to_owned())
            } else {
                None
            };
            if let Some(response) = response {
                println!("{response}");
                std::io::stdout().flush().unwrap();
            }
        }
    }

    /// Helper that remains alive until the registrar's failure path kills it.
    #[test]
    #[ignore = "spawned by registered_spawner_terminates_process_when_record_registration_fails"]
    fn registration_failure_server_helper() {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }

    #[test]
    #[ignore = "spawned by runtime_crash_loop_never_exceeds_durable_restart_budget"]
    fn restart_failure_server_helper() {
        let path = std::env::var("EVA_RESTART_COUNTER_FILE").unwrap();
        let current = std::fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(0);
        std::fs::write(path, (current + 1).to_string()).unwrap();
        std::process::exit(1);
    }

    #[test]
    #[ignore = "spawned by runtime_output_limit_is_terminal_without_restart"]
    fn output_limit_server_helper() {
        let path = std::env::var("EVA_OUTPUT_LIMIT_COUNTER_FILE").unwrap();
        let current = std::fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(0);
        std::fs::write(path, (current + 1).to_string()).unwrap();
        print!("0123456789abcdef");
        std::io::stdout().flush().unwrap();
    }

    /// 验证 `runtime_writes_provider_observability_when_backend_is_configured` 场景下的预期行为。
    #[test]
    fn runtime_writes_provider_observability_when_backend_is_configured() {
        let root = temp_root("provider-observability");
        let observability = root.join("observability");
        let runtime =
            runtime_with_handle(stdio_handle(true, test_command(), ok_args(), Vec::new()))
                .with_observability_backend(&observability);

        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-provider-observability").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap();

        assert_eq!(report.status, "completed");
        let audit = std::fs::read_to_string(observability.join("audit.jsonl")).unwrap();
        let metrics = std::fs::read_to_string(observability.join("metrics.jsonl")).unwrap();
        let spans = std::fs::read_to_string(observability.join("otel-spans.jsonl")).unwrap();
        assert!(audit.contains("\"action\":\"provider.supervised\""));
        assert!(audit.contains("\"request_id\":\"req-provider-observability\""));
        assert!(audit.contains("\"status\":\"completed\""));
        assert!(metrics.contains("\"name\":\"provider.supervisor.invocations\""));
        assert!(metrics.contains("\"surface\":\"provider\""));
        assert!(spans.contains("\"name\":\"provider.supervisor.invoke\""));

        std::fs::remove_dir_all(root).ok();
    }

    /// 验证 `runtime_rejects_cross_provider_credential_scope_before_start` 场景下的预期行为。
    #[test]
    fn runtime_rejects_cross_provider_credential_scope_before_start() {
        let handle = stdio_handle(
            true,
            "definitely-not-started",
            Vec::new(),
            vec!["EVA_CROSS_PROVIDER_SECRET".to_owned()],
        );
        let request_id = RequestId::parse("req-cross-provider-scope").unwrap();
        let capability = CapabilityName::parse("repo.analyze").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            "session-other-req",
            AdapterId::parse("other-provider").unwrap(),
            request_id.clone(),
            capability.clone(),
        );

        let error = dispatch_transport(
            &handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.message().contains("credential session"));
    }

    /// 验证 `runtime_rejects_disabled_stdio_provider_before_start` 场景下的预期行为。
    #[test]
    fn runtime_rejects_disabled_stdio_provider_before_start() {
        let runtime = runtime_with_handle(stdio_handle(
            false,
            "definitely-not-started",
            Vec::new(),
            Vec::new(),
        ));

        let error = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-disabled-stdio").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(runtime.provider_processes().unwrap().is_empty());
    }

    /// 验证 `runtime_releases_provider_slot_when_stdio_start_fails` 场景下的预期行为。
    #[test]
    fn runtime_releases_provider_slot_when_stdio_start_fails() {
        let runtime = runtime_with_handle(stdio_handle(
            true,
            "definitely-not-started",
            Vec::new(),
            Vec::new(),
        ));

        let error = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-stdio-start-fail").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        assert!(!processes[0].active);
        assert_eq!(processes[0].health, "failed");
        assert!(processes[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("failed to start stdio provider"));
        assert!(processes[0]
            .audit
            .iter()
            .any(|entry| entry == "provider.supervisor.failed"));
    }

    /// 验证 `runtime_can_mirror_provider_processes_to_durable_table` 场景下的预期行为。
    #[test]
    fn runtime_can_mirror_provider_processes_to_durable_table() {
        let root = temp_root("durable-provider-table");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let process_table = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            backend.acquire_runtime_writer().unwrap(),
        )
        .unwrap();
        let runtime = AdapterRuntime::from_registry_with_provider_process_table(
            registry_with_handle(stdio_handle(
                true,
                "definitely-not-started",
                Vec::new(),
                Vec::new(),
            )),
            process_table,
        );

        let error = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-durable-provider-table").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        let reader = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let processes = reader.list().unwrap();
        assert_eq!(processes.len(), 1);
        assert_eq!(
            processes[0].session_id,
            "session-stdio-test-req-durable-provider-table"
        );
        assert!(!processes[0].active);
        assert_eq!(processes[0].health, "failed");

        std::fs::remove_dir_all(root).ok();
    }

    /// 验证 `runtime_blocks_new_provider_process_while_circuit_is_open` 场景下的预期行为。
    #[test]
    fn runtime_blocks_new_provider_process_while_circuit_is_open() {
        let mut handle = stdio_handle(true, "definitely-not-started", Vec::new(), Vec::new());
        handle.circuit_breaker = Some(AdapterCircuitBreaker {
            failure_threshold: 1,
            recovery_window_ms: 60_000,
        });
        let runtime = runtime_with_handle(handle);

        let first = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-circuit-runtime-a").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();
        let second = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-circuit-runtime-b").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(first.kind(), ErrorKind::Unavailable);
        assert_eq!(second.kind(), ErrorKind::Unavailable);
        assert_eq!(
            second.provider_code().map(|code| code.as_str()),
            Some("provider_circuit_open")
        );
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].health, "circuit_open");
    }

    /// 验证 `runtime_invokes_http_adapter_and_redacts_credential_header` 场景下的预期行为。
    #[test]
    fn runtime_invokes_http_adapter_and_redacts_credential_header() {
        let env_name = "EVA_TEST_HTTP_SECRET_RUNTIME";
        let secret = "http-runtime-secret";
        std::env::set_var(env_name, secret);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/provider", listener.local_addr().unwrap());
        let server_secret = secret.to_owned();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 512];
            let header_end = loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    panic!("HTTP test client closed before headers were complete");
                }
                request_bytes.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = http_header_end(&request_bytes) {
                    break header_end;
                }
            };
            let header = String::from_utf8_lossy(&request_bytes[..header_end]);
            let content_length = http_content_length(&header);
            while request_bytes.len().saturating_sub(header_end + 4) < content_length {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request_bytes.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8_lossy(&request_bytes);
            assert!(request.contains("Authorization: http-runtime-secret"));
            assert!(request.contains(PROVIDER_SESSION_ID_HEADER));
            assert!(request.contains(PROVIDER_SESSION_TOKEN_HEADER));
            assert!(request.contains("{\"message\":\"hello\"}"));
            let session_token =
                http_header_value(&request, PROVIDER_SESSION_TOKEN_HEADER).unwrap_or_default();
            let body = format!("provider echoed {server_secret} {session_token}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let runtime = runtime_with_handle(http_handle(
            endpoint,
            BTreeMap::from([("Authorization".to_owned(), format!("env:{env_name}"))]),
            vec![env_name.to_owned()],
        ));

        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-http-runtime").unwrap(),
                    CapabilityName::parse("chat.reply").unwrap(),
                )
                .with_provider(AdapterId::parse("http-test").unwrap())
                .with_input("{\"message\":\"hello\"}"),
            )
            .unwrap();
        server.join().unwrap();
        std::env::remove_var(env_name);

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(!report.output.contains("eva-provider-session:"));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report.audit.contains(&format!(
            "credential_header:Authorization:env:{env_name}:redacted"
        )));
        assert!(report
            .audit
            .contains(&"credential.session_token:redacted".to_owned()));
    }

    /// 验证 `runtime_invokes_mcp_http_adapter_with_auth_headers` 场景下的预期行为。
    #[test]
    fn runtime_invokes_mcp_http_adapter_with_auth_headers() {
        let env_name = "EVA_TEST_MCP_HTTP_SECRET_RUNTIME";
        let secret = "mcp-http-runtime-secret";
        std::env::set_var(env_name, secret);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server_secret = secret.to_owned();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.contains("Authorization: mcp-http-runtime-secret"));
                assert!(request.contains(PROVIDER_SESSION_ID_HEADER));
                assert!(request.contains(PROVIDER_SESSION_TOKEN_HEADER));
                let session_token =
                    http_header_value(&request, PROVIDER_SESSION_TOKEN_HEADER).unwrap_or_default();
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default();
                let response_body = if body.contains("\"method\":\"initialize\"") {
                    "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fake-http\",\"version\":\"1\"}}}".to_owned()
                } else if body.contains("notifications/initialized") {
                    String::new()
                } else if body.contains("\"method\":\"tools/list\"") {
                    "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"list_issues\",\"inputSchema\":{\"type\":\"object\"}}]}}".to_owned()
                } else if body.contains("\"method\":\"tools/call\"") {
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok {server_secret} {session_token}\"}}],\"isError\":false}}}}"
                    )
                } else {
                    String::new()
                };
                let status = if body.contains("notifications/initialized") {
                    202
                } else {
                    200
                };
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });
        let runtime = runtime_with_handle(mcp_http_handle(
            endpoint,
            BTreeMap::from([("Authorization".to_owned(), format!("env:{env_name}"))]),
        ));

        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-mcp-http-runtime").unwrap(),
                    CapabilityName::parse("github.issue.list").unwrap(),
                )
                .with_provider(AdapterId::parse("mcp-http-test").unwrap())
                .with_input("{\"owner\":\"eva\"}"),
            )
            .unwrap();
        server.join().unwrap();
        std::env::remove_var(env_name);

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(!report.output.contains("eva-provider-session:"));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report.audit.contains(&format!(
            "mcp.credential_header:Authorization:env:{env_name}:redacted"
        )));
        assert!(report
            .audit
            .contains(&"credential.session_token:redacted".to_owned()));
        assert!(report
            .audit
            .contains(&"mcp.http.exchange_count:3".to_owned()));
    }

    /// 执行 `runtime_with_handle` 对应的受控流程。
    fn runtime_with_handle(handle: AdapterHandle) -> AdapterRuntime {
        AdapterRuntime::from_registry(registry_with_handle(handle))
    }

    /// 执行 `registry_with_handle` 对应的处理逻辑。
    fn registry_with_handle(handle: AdapterHandle) -> AdapterRegistry {
        let mut registry = AdapterRegistry::new();
        registry.register(handle).unwrap();
        registry
    }

    /// 执行 `http_header_end` 对应的处理逻辑。
    fn http_header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }

    /// 执行 `http_content_length` 对应的处理逻辑。
    fn http_content_length(header: &str) -> usize {
        header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    /// 执行 `http_header_value` 对应的处理逻辑。
    fn http_header_value(request: &str, header_name: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case(header_name) {
                Some(value.trim().to_owned())
            } else {
                None
            }
        })
    }

    /// 读取或解析 `read_http_request` 所需的数据，失败时保留错误语义。
    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        let mut request_bytes = Vec::new();
        let mut buffer = [0_u8; 512];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                panic!("HTTP test client closed before headers were complete");
            }
            request_bytes.extend_from_slice(&buffer[..read]);
            if let Some(header_end) = http_header_end(&request_bytes) {
                break header_end;
            }
        };
        let header = String::from_utf8_lossy(&request_bytes[..header_end]);
        let content_length = http_content_length(&header);
        while request_bytes.len().saturating_sub(header_end + 4) < content_length {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request_bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8_lossy(&request_bytes).into_owned()
    }

    /// 执行 `temp_root` 对应的处理逻辑。
    fn temp_root(name: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-runtime-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn wait_until_process_is_gone(pid: u32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while process_is_alive(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "process {pid} survived registration cleanup"
            );
            thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    fn process_is_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return false;
        }
        unsafe { CloseHandle(process) };
        true
    }

    #[cfg(not(any(unix, windows)))]
    fn process_is_alive(_pid: u32) -> bool {
        false
    }

    /// 执行 `stdio_handle` 对应的处理逻辑。
    fn stdio_handle(
        enabled: bool,
        command: impl Into<String>,
        args: Vec<String>,
        credential_env: Vec<String>,
    ) -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("stdio-test").unwrap(),
            name: "Stdio Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled,
            transport: AdapterTransport::Stdio,
            capabilities: vec![CapabilityName::parse("repo.analyze").unwrap()],
            source_path: "test".to_owned(),
            command: Some(command.into()),
            args,
            endpoint: None,
            method: None,
            credential_env,
            provider: eva_config::ProviderConfig::default(),
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers: BTreeMap::new(),
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            skill_path: None,
            skill_entry_type: None,
            skill_runner_command: None,
            skill_runner_args: Vec::new(),
            skill_artifact_root: None,
            skill_input_schema: None,
            hardware_logical_name: None,
            hardware_device_class: None,
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        }
    }

    /// 执行 `http_handle` 对应的处理逻辑。
    fn http_handle(
        endpoint: String,
        headers: BTreeMap<String, String>,
        credential_env: Vec<String>,
    ) -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("http-test").unwrap(),
            name: "HTTP Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Http,
            capabilities: vec![CapabilityName::parse("chat.reply").unwrap()],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: Some(endpoint),
            method: Some("POST".to_owned()),
            credential_env,
            provider: eva_config::ProviderConfig::default(),
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers,
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            skill_path: None,
            skill_entry_type: None,
            skill_runner_command: None,
            skill_runner_args: Vec::new(),
            skill_artifact_root: None,
            skill_input_schema: None,
            hardware_logical_name: None,
            hardware_device_class: None,
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        }
    }

    /// 执行 `mcp_http_handle` 对应的处理逻辑。
    fn mcp_http_handle(endpoint: String, headers: BTreeMap<String, String>) -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("mcp-http-test").unwrap(),
            name: "MCP HTTP Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Mcp,
            capabilities: vec![CapabilityName::parse("github.issue.list").unwrap()],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: Some(endpoint),
            method: None,
            credential_env: Vec::new(),
            provider: eva_config::ProviderConfig::default(),
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers,
            mcp_server_transport: Some("http".to_owned()),
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: vec!["list_issues".to_owned()],
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            skill_path: None,
            skill_entry_type: None,
            skill_runner_command: None,
            skill_runner_args: Vec::new(),
            skill_artifact_root: None,
            skill_input_schema: None,
            hardware_logical_name: None,
            hardware_device_class: None,
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        }
    }

    /// Build an MCP stdio handle whose command is this test binary, avoiding
    /// any dependency on an installed external server.
    fn mcp_stdio_handle() -> AdapterHandle {
        let mut handle = mcp_http_handle("http://127.0.0.1:1/mcp".to_owned(), BTreeMap::new());
        let command = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        handle.id = AdapterId::parse("mcp-stdio-test").unwrap();
        handle.name = "MCP Stdio Test".to_owned();
        handle.endpoint = None;
        handle.capabilities = vec![CapabilityName::parse("repo.analyze").unwrap()];
        handle.mcp_server_transport = Some("stdio".to_owned());
        handle.mcp_command = Some(command);
        handle.mcp_args = vec![
            "--exact".to_owned(),
            "runtime::tests::mcp_stdio_server_helper".to_owned(),
            "--ignored".to_owned(),
            "--nocapture".to_owned(),
        ];
        handle.mcp_tools = vec!["analyze".to_owned()];
        handle
    }

    /// 执行 `test_command` 对应的处理逻辑。
    #[cfg(windows)]
    fn test_command() -> &'static str {
        "powershell"
    }

    /// 执行 `test_command` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn test_command() -> &'static str {
        "sh"
    }

    /// 执行 `env_echo_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn env_echo_args(env_name: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "[Console]::Out.Write($env:{env_name}); [Console]::Out.Write($env:{PROVIDER_SESSION_TOKEN_ENV}); [Console]::Error.Write($env:{env_name}); [Console]::Error.Write($env:{PROVIDER_SESSION_TOKEN_ENV})"
            ),
        ]
    }

    /// 执行 `env_echo_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn env_echo_args(env_name: &str) -> Vec<String> {
        vec![
            "-c".to_owned(),
            format!(
                "printf \"${env_name}\"; printf \"${PROVIDER_SESSION_TOKEN_ENV}\"; printf \"${env_name}\" >&2; printf \"${PROVIDER_SESSION_TOKEN_ENV}\" >&2"
            ),
        ]
    }

    /// 执行 `ok_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn ok_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "[Console]::Out.Write('ok')".to_owned(),
        ]
    }

    /// 执行 `ok_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn ok_args() -> Vec<String> {
        vec!["-c".to_owned(), "printf ok".to_owned()]
    }
}
