//! 管理外部提供者的准入、会话凭据和进程快照。
//!
//! `acquire` 在写入运行中快照前依次检查熔断、并发与速率限制，任一检查失败都不会占用
//! 执行槽；`complete` 负责释放槽并根据结果推进熔断状态。会话令牌只注入对应提供者的
//! 环境变量或请求头，审计和输出路径必须使用摘要或脱敏值。
//! Provider supervisor slots and process table integration.

use crate::manifest::{AdapterCircuitBreaker, AdapterHandle, AdapterRateLimit};
use crate::runtime::AdapterInvocation;
use eva_config::AdapterTransport;
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_storage::{
    FileSystemProviderProcessTable, InMemoryProviderProcessTable, ProviderProcessSnapshot,
    ProviderProcessTable,
};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider execution slots and process table mutation";
/// 定义 `PROVIDER_SESSION_ID_ENV` 常量。
pub const PROVIDER_SESSION_ID_ENV: &str = "EVA_PROVIDER_SESSION_ID";
/// 定义 `PROVIDER_SESSION_TOKEN_ENV` 常量。
pub const PROVIDER_SESSION_TOKEN_ENV: &str = "EVA_PROVIDER_SESSION_TOKEN";
/// 定义 `PROVIDER_SESSION_ID_HEADER` 常量。
pub const PROVIDER_SESSION_ID_HEADER: &str = "X-Eva-Provider-Session";
/// 定义 `PROVIDER_SESSION_TOKEN_HEADER` 常量。
pub const PROVIDER_SESSION_TOKEN_HEADER: &str = "X-Eva-Provider-Session-Token";

/// 汇总一次提供者执行在准入阶段所需的不可变事实和清单限额。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionRequest {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `transport` 字段对应的值。
    pub transport: AdapterTransport,
    /// 记录 `manifest_digest` 字段对应的值。
    pub manifest_digest: String,
    /// 记录 `start_command` 字段对应的值。
    pub start_command: String,
    /// 记录 `restart_policy` 字段对应的值。
    pub restart_policy: String,
    /// 限制该适配器同时处于运行状态的快照数量；`None` 表示不设此项限制。
    pub max_concurrency: Option<usize>,
    /// 定义按适配器隔离的固定窗口请求上限。
    pub rate_limit: Option<AdapterRateLimit>,
    /// 定义连续失败阈值和从开启态进入半开探测的恢复窗口。
    pub circuit_breaker: Option<AdapterCircuitBreaker>,
    /// 提供拒绝后建议的重试退避，仅进入错误上下文与进程快照，不负责主动重试。
    pub retry_backoff_ms: Option<u64>,
}

/// 表示已通过所有准入检查并写入运行中快照的执行槽。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionSlot {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `provider_process_id` 字段对应的值。
    pub provider_process_id: String,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 标记该槽是否是熔断恢复窗口后的唯一半开探测。
    pub half_open_probe: bool,
}

/// 绑定到单一会话、提供者、请求和能力的短生命周期凭据作用域。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCredentialScope {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 保存可审计的确定性摘要；实际注入令牌由摘要和会话标识临时派生。
    pub token_digest: String,
}

/// 表示 `ProviderExecutionOutcome` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionOutcome {
    /// 记录 `health` 字段对应的值。
    pub health: String,
    /// 记录 `last_error` 字段对应的值。
    pub last_error: Option<String>,
}

/// 约定提供者执行槽从准入到完成的成对生命周期接口。
pub trait ProviderSupervisor {
    /// 原子地完成准入检查并创建运行中快照；失败不得留下活动槽。
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError>;
    /// 释放指定槽、记录终态并推进熔断器；同一槽只能完成一次。
    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError>;
    /// 执行 `processes` 对应的处理逻辑。
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;
}

/// 在调用线程内维护准入状态，并可镜像快照到持久化进程表的监督器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryProviderSupervisor {
    /// 作为本实例判断活动槽的权威内存进程表。
    table: InMemoryProviderProcessTable,
    /// 可选的持久化镜像；写入失败会阻止报告准入或完成成功。
    durable_table: Option<FileSystemProviderProcessTable>,
    /// 按适配器隔离的固定窗口计数器。
    rate_windows: BTreeMap<AdapterId, ProviderRateWindow>,
    /// 按适配器隔离的连续失败、开启时间和半开探测状态。
    circuit_states: BTreeMap<AdapterId, ProviderCircuitState>,
}

/// 表示 `ProviderRateWindow` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderRateWindow {
    /// 记录 `started_at_ms` 字段对应的值。
    started_at_ms: u128,
    /// 记录 `count` 字段对应的值。
    count: u32,
}

/// 表示 `ProviderCircuitState` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ProviderCircuitState {
    /// 记录 `failure_count` 字段对应的值。
    failure_count: u32,
    /// 记录 `opened_at_ms` 字段对应的值。
    opened_at_ms: Option<u128>,
    /// 记录 `half_open_probe_active` 字段对应的值。
    half_open_probe_active: bool,
    /// 记录 `failure_threshold` 字段对应的值。
    failure_threshold: u32,
}

impl ProviderExecutionRequest {
    /// 根据输入构造当前类型，作为 `from_handle` 的标准入口。
    pub fn from_handle(handle: &AdapterHandle, invocation: &AdapterInvocation) -> Self {
        Self {
            request_id: invocation.request_id.clone(),
            adapter_id: handle.id.clone(),
            capability: invocation.capability.clone(),
            transport: handle.transport,
            manifest_digest: manifest_digest(handle),
            start_command: start_command(handle),
            restart_policy: "none".to_owned(),
            max_concurrency: handle.max_concurrency,
            rate_limit: handle.rate_limit,
            circuit_breaker: handle.circuit_breaker,
            retry_backoff_ms: None,
        }
    }

    /// 设置 `retry_backoff_ms` 并返回更新后的实例。
    pub fn with_retry_backoff_ms(mut self, retry_backoff_ms: Option<u64>) -> Self {
        self.retry_backoff_ms = retry_backoff_ms;
        self
    }
}

impl ProviderCredentialScope {
    /// 执行 `new_for_session` 对应的处理逻辑。
    pub fn new_for_session(
        session_id: impl Into<String>,
        adapter_id: AdapterId,
        request_id: RequestId,
        capability: CapabilityName,
    ) -> Self {
        let session_id = session_id.into();
        let token_digest =
            credential_token_digest(&session_id, &adapter_id, &request_id, &capability);
        Self {
            session_id,
            adapter_id,
            request_id,
            capability,
            token_digest,
        }
    }

    /// 根据输入构造当前类型，作为 `from_slot` 的标准入口。
    pub fn from_slot(slot: &ProviderExecutionSlot, capability: CapabilityName) -> Self {
        Self::new_for_session(
            slot.session_id.clone(),
            slot.adapter_id.clone(),
            slot.request_id.clone(),
            capability,
        )
    }

    /// 校验作用域是否精确绑定当前提供者、请求和能力，阻止跨请求或跨提供者复用。
    pub fn ensure_matches(
        &self,
        adapter_id: &AdapterId,
        request_id: &RequestId,
        capability: &CapabilityName,
    ) -> Result<(), EvaError> {
        if &self.adapter_id != adapter_id {
            return Err(EvaError::permission_denied(
                "provider credential session cannot be reused across providers",
            )
            .with_context("session_id", &self.session_id)
            .with_context("session_provider", self.adapter_id.as_str())
            .with_context("requested_provider", adapter_id.as_str()));
        }
        if &self.request_id != request_id || &self.capability != capability {
            return Err(EvaError::permission_denied(
                "provider credential session cannot be reused across requests",
            )
            .with_context("session_id", &self.session_id)
            .with_context("session_request", self.request_id.as_str())
            .with_context("requested_request", request_id.as_str()));
        }
        Ok(())
    }

    /// 执行 `audit_entries` 对应的处理逻辑。
    pub fn audit_entries(&self) -> Vec<String> {
        vec![
            "credential.scope:provider_session".to_owned(),
            format!("credential.session:{}", self.session_id),
            format!("credential.session_digest:{}", self.token_digest),
            "credential.session_token:redacted".to_owned(),
        ]
    }

    /// 执行 `apply_env` 对应的处理逻辑。
    pub(crate) fn apply_env(&self, env: &mut BTreeMap<String, String>) {
        env.insert(PROVIDER_SESSION_ID_ENV.to_owned(), self.session_id.clone());
        env.insert(PROVIDER_SESSION_TOKEN_ENV.to_owned(), self.session_token());
    }

    /// 执行 `apply_headers` 对应的处理逻辑。
    pub(crate) fn apply_headers(&self, headers: &mut BTreeMap<String, String>) {
        headers.insert(
            PROVIDER_SESSION_ID_HEADER.to_owned(),
            self.session_id.clone(),
        );
        headers.insert(
            PROVIDER_SESSION_TOKEN_HEADER.to_owned(),
            self.session_token(),
        );
    }

    /// 执行 `redaction_values` 对应的处理逻辑。
    pub(crate) fn redaction_values(&self) -> Vec<String> {
        vec![self.session_token()]
    }

    /// 执行 `session_token` 对应的处理逻辑。
    fn session_token(&self) -> String {
        format!(
            "eva-provider-session:{}:{}",
            self.session_id, self.token_digest
        )
    }
}

impl ProviderExecutionOutcome {
    /// 执行 `completed` 对应的处理逻辑。
    pub fn completed(status: &str) -> Self {
        Self {
            health: if status == "completed" {
                "completed".to_owned()
            } else {
                "failed".to_owned()
            },
            last_error: if status == "completed" {
                None
            } else {
                Some(format!("adapter returned status {status}"))
            },
        }
    }

    /// 执行 `failed` 对应的处理逻辑。
    pub fn failed(error: &EvaError) -> Self {
        Self {
            health: "failed".to_owned(),
            last_error: Some(format!("{}: {}", error.kind().as_str(), error.message())),
        }
    }
}

/// 校验可选凭据作用域；需要凭据的传输在缺失作用域时必须在 I/O 前失败。
pub(crate) fn validate_credential_scope_for_provider<'a>(
    scope: Option<&'a ProviderCredentialScope>,
    adapter_id: &AdapterId,
    request_id: &RequestId,
    capability: &CapabilityName,
    required: bool,
) -> Result<Option<&'a ProviderCredentialScope>, EvaError> {
    match scope {
        Some(scope) => {
            scope.ensure_matches(adapter_id, request_id, capability)?;
            Ok(Some(scope))
        }
        None if required => Err(EvaError::permission_denied(
            "provider credential session scope is required",
        )
        .with_context("adapter_id", adapter_id.as_str())
        .with_context("request_id", request_id.as_str())),
        None => Ok(None),
    }
}

/// 执行 `redact_provider_session_tokens` 对应的处理逻辑。
pub(crate) fn redact_provider_session_tokens(value: &str) -> String {
    let mut redacted = value.to_owned();
    while let Some(start) = redacted.find("eva-provider-session:") {
        let end = redacted[start..]
            .char_indices()
            .find_map(|(offset, ch)| {
                if offset > 0 && (ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\' | '<' | '>'))
                {
                    Some(start + offset)
                } else {
                    None
                }
            })
            .unwrap_or(redacted.len());
        redacted.replace_range(start..end, "[REDACTED]");
    }
    redacted
}

impl Default for InMemoryProviderSupervisor {
    /// 创建仅使用内存进程表且无历史限流状态的监督器。
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryProviderSupervisor {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self {
            table: InMemoryProviderProcessTable::new(),
            durable_table: None,
            rate_windows: BTreeMap::new(),
            circuit_states: BTreeMap::new(),
        }
    }

    /// 设置 `process_table` 并返回更新后的实例。
    pub fn with_process_table(durable_table: FileSystemProviderProcessTable) -> Self {
        Self {
            table: InMemoryProviderProcessTable::new(),
            durable_table: Some(durable_table),
            rate_windows: BTreeMap::new(),
            circuit_states: BTreeMap::new(),
        }
    }

    /// 执行 `active_for_adapter` 对应的处理逻辑。
    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        self.table.active_for_adapter(adapter_id)
    }
}

impl ProviderSupervisor for InMemoryProviderSupervisor {
    /// 按熔断、并发、速率的顺序完成准入，全部通过后才写入运行中快照。
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError> {
        let now = now_ms();
        // 熔断检查会占用半开探测权，因此其后任何检查失败都不能产生进程快照。
        let half_open_probe = self.admit_circuit(&request, now)?;
        self.admit_concurrency(&request)?;
        self.admit_rate_limit(&request, now)?;
        let session_id = session_id(&request.request_id, &request.adapter_id);
        let provider_process_id = provider_process_id(&request.request_id, &request.adapter_id);
        let limit_audit = limit_audit_entries(&request, half_open_probe);
        let mut snapshot = ProviderProcessSnapshot::running(
            session_id.clone(),
            provider_process_id.clone(),
            request.request_id.clone(),
            request.adapter_id.clone(),
            request.capability,
            request.transport.as_str(),
            request.manifest_digest,
            request.start_command,
            request.restart_policy,
        );
        snapshot.audit.extend(limit_audit);
        snapshot.retry_backoff_ms = request.retry_backoff_ms;
        self.upsert_process(snapshot)?;
        Ok(ProviderExecutionSlot {
            session_id,
            provider_process_id,
            request_id: request.request_id,
            adapter_id: request.adapter_id,
            half_open_probe,
        })
    }

    /// 将运行中快照释放为终态，再用同一结果更新熔断状态并同步持久化镜像。
    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = self.table.read(&slot.session_id)?;
        snapshot.release(outcome.health, outcome.last_error)?;
        self.record_circuit_outcome(slot, &mut snapshot);
        self.upsert_process(snapshot.clone())?;
        Ok(snapshot)
    }

    /// 执行 `processes` 对应的处理逻辑。
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        self.table.list()
    }
}

impl InMemoryProviderSupervisor {
    /// 执行 `upsert_process` 对应的处理逻辑。
    fn upsert_process(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError> {
        self.table.upsert(snapshot.clone())?;
        if let Some(table) = &mut self.durable_table {
            table.upsert(snapshot)?;
        }
        Ok(())
    }

    /// 对当前进程表中的活动快照计数，达到上限时返回可重试的稳定准入错误。
    fn admit_concurrency(&self, request: &ProviderExecutionRequest) -> Result<(), EvaError> {
        let Some(max_concurrency) = request.max_concurrency else {
            return Ok(());
        };
        let active = self.active_for_adapter(&request.adapter_id)?.len();
        if active >= max_concurrency {
            return Err(admission_error(
                request,
                "provider concurrency limit is exhausted",
                "provider_concurrency_limited",
                request.retry_backoff_ms,
            )
            .with_context("active", active.to_string())
            .with_context("max_concurrency", max_concurrency.to_string()));
        }
        Ok(())
    }

    /// 在适配器独立的固定时间窗内计数；被拒绝的请求不会增加窗口计数。
    fn admit_rate_limit(
        &mut self,
        request: &ProviderExecutionRequest,
        now: u128,
    ) -> Result<(), EvaError> {
        let Some(limit) = request.rate_limit else {
            return Ok(());
        };
        let window = self
            .rate_windows
            .entry(request.adapter_id.clone())
            .or_insert(ProviderRateWindow {
                started_at_ms: now,
                count: 0,
            });
        if now.saturating_sub(window.started_at_ms) >= u128::from(limit.window_ms) {
            window.started_at_ms = now;
            window.count = 0;
        }
        if window.count >= limit.max_requests {
            let elapsed = now.saturating_sub(window.started_at_ms);
            let retry_after_ms = u128::from(limit.window_ms)
                .saturating_sub(elapsed)
                .try_into()
                .unwrap_or(u64::MAX);
            return Err(admission_error(
                request,
                "provider rate limit is exhausted",
                "provider_rate_limited",
                Some(retry_after_ms),
            )
            .with_context("rate_limit_max_requests", limit.max_requests.to_string())
            .with_context("rate_limit_window_ms", limit.window_ms.to_string()));
        }
        window.count = window.count.saturating_add(1);
        Ok(())
    }

    /// 拒绝开启态请求，恢复窗口届满后仅允许一个半开探测槽继续。
    fn admit_circuit(
        &mut self,
        request: &ProviderExecutionRequest,
        now: u128,
    ) -> Result<bool, EvaError> {
        let Some(config) = request.circuit_breaker else {
            return Ok(false);
        };
        let state = self
            .circuit_states
            .entry(request.adapter_id.clone())
            .or_default();
        state.failure_threshold = config.failure_threshold;
        let Some(opened_at_ms) = state.opened_at_ms else {
            return Ok(false);
        };
        let elapsed = now.saturating_sub(opened_at_ms);
        if elapsed >= u128::from(config.recovery_window_ms) && !state.half_open_probe_active {
            state.half_open_probe_active = true;
            return Ok(true);
        }
        let retry_after_ms = u128::from(config.recovery_window_ms)
            .saturating_sub(elapsed)
            .try_into()
            .unwrap_or(u64::MAX);
        Err(admission_error(
            request,
            "provider circuit breaker is open",
            "provider_circuit_open",
            Some(retry_after_ms),
        )
        .with_context(
            "circuit_failure_threshold",
            config.failure_threshold.to_string(),
        )
        .with_context(
            "circuit_recovery_window_ms",
            config.recovery_window_ms.to_string(),
        ))
    }

    /// 根据执行终态关闭、累加或重新开启熔断器，并把状态变化写入快照审计。
    fn record_circuit_outcome(
        &mut self,
        slot: &ProviderExecutionSlot,
        snapshot: &mut ProviderProcessSnapshot,
    ) {
        let Some(state) = self.circuit_states.get_mut(&slot.adapter_id) else {
            return;
        };
        if snapshot.health == "completed" {
            state.failure_count = 0;
            state.opened_at_ms = None;
            state.half_open_probe_active = false;
            if slot.half_open_probe {
                snapshot.audit.push("provider.circuit:closed".to_owned());
            }
            return;
        }

        state.failure_count = state.failure_count.saturating_add(1);
        state.half_open_probe_active = false;
        if slot.half_open_probe
            || (state.failure_threshold > 0 && state.failure_count >= state.failure_threshold)
        {
            state.opened_at_ms = Some(now_ms());
            snapshot.health = "circuit_open".to_owned();
            snapshot.audit.push("provider.circuit:opened".to_owned());
            snapshot
                .audit
                .push("provider.health:circuit_open".to_owned());
        }
    }
}

/// 执行 `admission_error` 对应的处理逻辑。
fn admission_error(
    request: &ProviderExecutionRequest,
    message: &'static str,
    provider_code: &'static str,
    retry_after_ms: Option<u64>,
) -> EvaError {
    let mut error = EvaError::unavailable(message)
        .with_provider_code(provider_code)
        .with_context("adapter_id", request.adapter_id.as_str())
        .with_context("request_id", request.request_id.as_str())
        .with_context("capability", request.capability.as_str());
    if let Some(retry_after_ms) = retry_after_ms {
        error = error.with_context("retry_after_ms", retry_after_ms.to_string());
    }
    error
}

/// 执行 `limit_audit_entries` 对应的处理逻辑。
fn limit_audit_entries(request: &ProviderExecutionRequest, half_open_probe: bool) -> Vec<String> {
    let mut audit = Vec::new();
    if let Some(max_concurrency) = request.max_concurrency {
        audit.push(format!("provider.concurrency.max:{max_concurrency}"));
    }
    if let Some(rate_limit) = request.rate_limit {
        audit.push(format!(
            "provider.rate_limit:{}:{}",
            rate_limit.max_requests, rate_limit.window_ms
        ));
    }
    if let Some(circuit_breaker) = request.circuit_breaker {
        audit.push(format!(
            "provider.circuit.failure_threshold:{}",
            circuit_breaker.failure_threshold
        ));
        audit.push(format!(
            "provider.circuit.recovery_window_ms:{}",
            circuit_breaker.recovery_window_ms
        ));
    }
    if half_open_probe {
        audit.push("provider.circuit:half_open_probe".to_owned());
    }
    audit
}

/// 执行 `session_id` 对应的处理逻辑。
fn session_id(request_id: &RequestId, adapter_id: &AdapterId) -> String {
    format!(
        "session-{}-{}",
        safe_segment(adapter_id.as_str()),
        safe_segment(request_id.as_str())
    )
}

/// 执行 `provider_process_id` 对应的处理逻辑。
fn provider_process_id(request_id: &RequestId, adapter_id: &AdapterId) -> String {
    format!(
        "provider-{}-{}",
        safe_segment(adapter_id.as_str()),
        safe_segment(request_id.as_str())
    )
}

/// 执行 `start_command` 对应的受控流程。
fn start_command(handle: &AdapterHandle) -> String {
    match handle.transport {
        AdapterTransport::Stdio => command_with_args(handle.command.as_deref(), &handle.args),
        AdapterTransport::Http => handle
            .endpoint
            .as_ref()
            .map(|endpoint| {
                format!(
                    "{} {}",
                    handle.method.as_deref().unwrap_or("POST"),
                    endpoint
                )
            })
            .unwrap_or_else(|| "http:<missing-endpoint>".to_owned()),
        AdapterTransport::Mcp => command_with_args(handle.mcp_command.as_deref(), &handle.mcp_args),
        AdapterTransport::Skill => {
            if let Some(command) = handle.skill_runner_command.as_deref() {
                command_with_args(Some(command), &handle.skill_runner_args)
            } else if let Some(command) = handle.command.as_deref() {
                command_with_args(Some(command), &handle.args)
            } else {
                format!(
                    "skill:{}",
                    handle.skill_name().unwrap_or("<missing-skill-id>")
                )
            }
        }
        AdapterTransport::Builtin => "builtin".to_owned(),
        AdapterTransport::Hardware => "hardware-driver".to_owned(),
        AdapterTransport::LuaCapability => "lua-capability".to_owned(),
        AdapterTransport::Eventbus => "eventbus".to_owned(),
    }
}

/// 执行 `command_with_args` 对应的处理逻辑。
fn command_with_args(command: Option<&str>, args: &[String]) -> String {
    let command = command.unwrap_or("<missing-command>");
    if args.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

/// 执行 `manifest_digest` 对应的处理逻辑。
fn manifest_digest(handle: &AdapterHandle) -> String {
    let material = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        handle.id.as_str(),
        handle.version,
        handle.transport.as_str(),
        handle.source_path,
        handle.command.as_deref().unwrap_or(""),
        handle.endpoint.as_deref().unwrap_or(""),
        handle.mcp_command.as_deref().unwrap_or(""),
        handle.skill_name().unwrap_or(""),
        handle
            .capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    format!("fnv64:{:016x}", fnv1a64(material.as_bytes()))
}

/// 执行 `credential_token_digest` 对应的处理逻辑。
fn credential_token_digest(
    session_id: &str,
    adapter_id: &AdapterId,
    request_id: &RequestId,
    capability: &CapabilityName,
) -> String {
    let material = format!(
        "{session_id}|{}|{}|{}",
        adapter_id.as_str(),
        request_id.as_str(),
        capability.as_str()
    );
    format!("fnv64:{:016x}", fnv1a64(material.as_bytes()))
}

/// 执行 `fnv1a64` 对应的处理逻辑。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 执行 `safe_segment` 对应的处理逻辑。
fn safe_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// 执行 `now_ms` 对应的处理逻辑。
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AdapterHandle;
    use std::collections::BTreeMap;

    /// 执行 `handle` 对应的处理逻辑。
    fn handle() -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("stdio-test").unwrap(),
            name: "Stdio Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Stdio,
            capabilities: vec![CapabilityName::parse("repo.analyze").unwrap()],
            source_path: "test".to_owned(),
            command: Some("stdio-runner".to_owned()),
            args: vec!["--once".to_owned()],
            endpoint: None,
            method: None,
            credential_env: Vec::new(),
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

    /// 执行 `invocation` 对应的处理逻辑。
    fn invocation(request_id: &str) -> AdapterInvocation {
        AdapterInvocation::new(
            RequestId::parse(request_id).unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        )
    }

    /// 验证 `supervisor_records_acquire_and_release` 场景下的预期行为。
    #[test]
    fn supervisor_records_acquire_and_release() {
        let handle = handle();
        let invocation = invocation("req-supervisor-1");
        let mut supervisor = InMemoryProviderSupervisor::new();

        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(&handle, &invocation))
            .unwrap();
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 1);

        let snapshot = supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .unwrap();

        assert!(!snapshot.active);
        assert_eq!(snapshot.health, "completed");
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 0);
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.slot:released"));
    }

    /// 验证 `supervisor_rejects_concurrency_limit_without_new_slot` 场景下的预期行为。
    #[test]
    fn supervisor_rejects_concurrency_limit_without_new_slot() {
        let mut handle = handle();
        handle.max_concurrency = Some(1);
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-concurrency-a"),
            ))
            .unwrap();
        let error = supervisor
            .acquire(
                ProviderExecutionRequest::from_handle(
                    &handle,
                    &invocation("req-supervisor-concurrency-b"),
                )
                .with_retry_backoff_ms(Some(1000)),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert!(error.is_retryable());
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("provider_concurrency_limited")
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "retry_after_ms" && value == "1000"));
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 1);

        supervisor
            .complete(&first, ProviderExecutionOutcome::completed("completed"))
            .unwrap();
    }

    /// 验证 `supervisor_rejects_rate_limit_without_starting_new_process` 场景下的预期行为。
    #[test]
    fn supervisor_rejects_rate_limit_without_starting_new_process() {
        let mut handle = handle();
        handle.rate_limit = Some(AdapterRateLimit {
            max_requests: 1,
            window_ms: 60_000,
        });
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-rate-a"),
            ))
            .unwrap();
        supervisor
            .complete(&first, ProviderExecutionOutcome::completed("completed"))
            .unwrap();
        let error = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-rate-b"),
            ))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("provider_rate_limited")
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, _)| key == "retry_after_ms"));
        assert_eq!(supervisor.processes().unwrap().len(), 1);
    }

    /// 验证 `supervisor_opens_circuit_and_blocks_new_processes` 场景下的预期行为。
    #[test]
    fn supervisor_opens_circuit_and_blocks_new_processes() {
        let mut handle = handle();
        handle.circuit_breaker = Some(AdapterCircuitBreaker {
            failure_threshold: 1,
            recovery_window_ms: 60_000,
        });
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-circuit-a"),
            ))
            .unwrap();
        let snapshot = supervisor
            .complete(
                &first,
                ProviderExecutionOutcome::failed(&EvaError::unavailable("provider failed")),
            )
            .unwrap();
        let error = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-circuit-b"),
            ))
            .unwrap_err();

        assert_eq!(snapshot.health, "circuit_open");
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.circuit:opened"));
        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("provider_circuit_open")
        );
        assert_eq!(supervisor.processes().unwrap().len(), 1);
    }

    /// 验证 `supervisor_allows_half_open_probe_after_recovery_window` 场景下的预期行为。
    #[test]
    fn supervisor_allows_half_open_probe_after_recovery_window() {
        let mut handle = handle();
        handle.circuit_breaker = Some(AdapterCircuitBreaker {
            failure_threshold: 1,
            recovery_window_ms: 0,
        });
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-half-open-a"),
            ))
            .unwrap();
        supervisor
            .complete(
                &first,
                ProviderExecutionOutcome::failed(&EvaError::unavailable("provider failed")),
            )
            .unwrap();
        let probe = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-half-open-b"),
            ))
            .unwrap();
        let snapshot = supervisor
            .complete(&probe, ProviderExecutionOutcome::completed("completed"))
            .unwrap();

        assert!(probe.half_open_probe);
        assert_eq!(snapshot.health, "completed");
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.circuit:half_open_probe"));
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.circuit:closed"));
    }

    /// 验证 `skill_start_command_uses_matching_fallback_args` 场景下的预期行为。
    #[test]
    fn skill_start_command_uses_matching_fallback_args() {
        let mut handle = handle();
        handle.transport = AdapterTransport::Skill;
        handle.command = Some("skill-fallback".to_owned());
        handle.args = vec!["--fallback".to_owned()];
        handle.skill_runner_command = None;
        handle.skill_runner_args = vec!["--runner".to_owned()];

        let request = ProviderExecutionRequest::from_handle(
            &handle,
            &AdapterInvocation::new(
                RequestId::parse("req-supervisor-2").unwrap(),
                CapabilityName::parse("repo.analyze").unwrap(),
            ),
        );

        assert_eq!(request.start_command, "skill-fallback --fallback");
    }

    /// 验证 `credential_scope_rejects_cross_provider_reuse` 场景下的预期行为。
    #[test]
    fn credential_scope_rejects_cross_provider_reuse() {
        let scope = ProviderCredentialScope::new_for_session(
            "session-stdio-req",
            AdapterId::parse("stdio-test").unwrap(),
            RequestId::parse("req-supervisor-credentials").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );

        let error = scope
            .ensure_matches(
                &AdapterId::parse("other-provider").unwrap(),
                &RequestId::parse("req-supervisor-credentials").unwrap(),
                &CapabilityName::parse("repo.analyze").unwrap(),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
        assert!(!scope
            .audit_entries()
            .iter()
            .any(|entry| entry.contains("eva-provider-session:")));
    }

    /// 验证 `credential_scope_injects_token_without_exposing_it_in_audit` 场景下的预期行为。
    #[test]
    fn credential_scope_injects_token_without_exposing_it_in_audit() {
        let scope = ProviderCredentialScope::new_for_session(
            "session-stdio-req",
            AdapterId::parse("stdio-test").unwrap(),
            RequestId::parse("req-supervisor-credentials").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );
        let mut env = BTreeMap::new();
        let mut headers = BTreeMap::new();

        scope.apply_env(&mut env);
        scope.apply_headers(&mut headers);

        assert_eq!(
            env.get(PROVIDER_SESSION_ID_ENV).map(String::as_str),
            Some("session-stdio-req")
        );
        assert!(env
            .get(PROVIDER_SESSION_TOKEN_ENV)
            .unwrap()
            .starts_with("eva-provider-session:"));
        assert!(headers.contains_key(PROVIDER_SESSION_TOKEN_HEADER));
        assert!(scope
            .audit_entries()
            .contains(&"credential.session_token:redacted".to_owned()));
    }

    /// 验证 `provider_session_token_redaction_catches_prefixed_values` 场景下的预期行为。
    #[test]
    fn provider_session_token_redaction_catches_prefixed_values() {
        let redacted = redact_provider_session_tokens(
            "before eva-provider-session:session-1:fnv64:abc123 after",
        );

        assert_eq!(redacted, "before [REDACTED] after");
    }
}
