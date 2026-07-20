//! 管理外部提供者的准入、会话凭据和进程快照。
//!
//! `acquire` 在写入运行中快照前依次检查熔断、并发与速率限制，任一检查失败都不会占用
//! 执行槽；`complete` 负责释放槽并根据结果推进熔断状态。会话令牌只注入对应提供者的
//! 环境变量或请求头，审计和输出路径必须使用摘要或脱敏值。
//! Provider supervisor slots and process table integration.

use crate::manifest::{AdapterCircuitBreaker, AdapterHandle, AdapterRateLimit};
use crate::process_backend::{OsProcessBackend, ProcessIdentity, ProcessTerminationOutcome};
use crate::runtime::AdapterInvocation;
use eva_config::{AdapterTransport, ProviderConfig, ProviderRunAsIdentity};
use eva_core::{AdapterId, CapabilityName, ErrorKind, EvaError, RequestId};
use eva_storage::{
    FileSystemProviderAdmissionTable, FileSystemProviderProcessTable, InMemoryProviderProcessTable,
    ProviderProcessSnapshot, ProviderProcessTable,
};
use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
const PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX: &str = "provider.admission:reservation:";
const PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX: &str = "provider.admission:release_pending:";
const PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX: &str =
    "provider.admission:release_resolved:";

/// Bounded timing for closing provider admission and retiring active slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDrainOptions {
    total_timeout: Duration,
    poll_interval: Duration,
}

impl ProviderDrainOptions {
    /// Creates a provider drain budget. Both values must be non-zero so a
    /// caller cannot accidentally skip the admission-close observation step.
    pub fn new(total_timeout: Duration) -> Result<Self, EvaError> {
        if total_timeout.is_zero() {
            return Err(EvaError::invalid_argument(
                "provider drain timeout must be greater than zero",
            ));
        }
        Ok(Self {
            total_timeout,
            poll_interval: Duration::from_millis(10).min(total_timeout),
        })
    }

    /// Returns the absolute wall-clock budget for the drain operation.
    pub const fn total_timeout(self) -> Duration {
        self.total_timeout
    }

    /// Overrides the bounded polling interval, primarily for deterministic tests.
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Result<Self, EvaError> {
        if poll_interval.is_zero() {
            return Err(EvaError::invalid_argument(
                "provider drain poll interval must be greater than zero",
            ));
        }
        self.poll_interval = poll_interval.min(self.total_timeout);
        Ok(self)
    }

    const fn poll_interval(self) -> Duration {
        self.poll_interval
    }
}

impl Default for ProviderDrainOptions {
    fn default() -> Self {
        Self::new(Duration::from_secs(3)).expect("static provider drain budget is valid")
    }
}

/// Stable evidence emitted after a supervisor has closed admission and retired
/// every provider snapshot it could safely own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDrainReport {
    /// Whether this report is a cached result of an earlier successful drain.
    pub already_drained: bool,
    /// `drained` when no active provider remains, `timed_out` otherwise.
    pub phase: String,
    /// Number of active snapshots still present when the report was produced.
    pub active_provider_count: usize,
    /// Number of provider process boundaries terminated during this drain.
    pub terminated_provider_count: usize,
    /// Number of boundaries that required force termination.
    pub forced_provider_count: usize,
    /// Number of legacy snapshots without an OS identity.
    pub missing_identity_count: usize,
    /// Deterministic lifecycle evidence suitable for daemon audit output.
    pub audit: Vec<String>,
}

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
    /// Canonical provider restart, run-as, and vault-reference declaration.
    pub provider: ProviderConfig,
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
    /// Durable admission identity; absent for supervisors without a durable admission table.
    pub admission_reservation_id: Option<String>,
    /// 标记该槽是否是熔断恢复窗口后的唯一半开探测。
    pub half_open_probe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderAdmissionLease {
    table: FileSystemProviderAdmissionTable,
    adapter_id: AdapterId,
    reservation_id: String,
    session_id: String,
}

impl ProviderAdmissionLease {
    pub(crate) fn renew_at(&self, now_ms: u128) -> Result<(), EvaError> {
        self.table.renew(
            &self.adapter_id,
            &self.reservation_id,
            &self.session_id,
            now_ms,
            eva_storage::DEFAULT_RESERVATION_TTL_MS,
        )?;
        Ok(())
    }
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
    /// Extend a durable admission lease. Implementations fail closed when ownership is stale.
    fn renew_admission(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _now_ms: u128,
    ) -> Result<(), EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable admission renewal",
        ))
    }
    /// Attach the real OS identity immediately after a transport spawn.
    /// Implementations that do not persist process identities fail closed.
    fn register_process_identity(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _identity: &ProcessIdentity,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support OS process registration",
        ))
    }
    /// Persist a consumed restart attempt and its next due time.
    fn schedule_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _attempt: u32,
        _due_at_ms: u128,
        _reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable restart scheduling",
        ))
    }
    /// Move a due durable restart into its pre-spawn state.
    fn prepare_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _now_ms: u128,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable restart preparation",
        ))
    }
    /// Persist terminal budget exhaustion.
    fn exhaust_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable restart exhaustion",
        ))
    }
    /// Persist a terminal failure that must not consume restart budget.
    fn fail_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support terminal restart failure",
        ))
    }
    /// Read the authoritative durable snapshot for a slot.
    fn snapshot(&self, _slot: &ProviderExecutionSlot) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not expose durable restart state",
        ))
    }
    /// 执行 `processes` 对应的处理逻辑。
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;

    /// Close admission and retire active provider slots before daemon shutdown.
    /// Implementations that do not own process boundaries fail closed rather
    /// than pretending that a drain completed.
    fn drain(&mut self, _options: ProviderDrainOptions) -> Result<ProviderDrainReport, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support bounded drain",
        ))
    }
}

/// 在调用线程内维护准入状态，并可镜像快照到持久化进程表的监督器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryProviderSupervisor {
    /// 作为本实例判断活动槽的权威内存进程表。
    table: InMemoryProviderProcessTable,
    /// 可选的持久化镜像；写入失败会阻止报告准入或完成成功。
    durable_table: Option<FileSystemProviderProcessTable>,
    admission_table: Option<FileSystemProviderAdmissionTable>,
    /// 按适配器隔离的固定窗口计数器。
    rate_windows: BTreeMap<AdapterId, ProviderRateWindow>,
    /// 按适配器隔离的连续失败、开启时间和半开探测状态。
    circuit_states: BTreeMap<AdapterId, ProviderCircuitState>,
    /// One-way admission gate. Once set, no new provider execution may start.
    draining: bool,
    /// Cached successful report makes repeated shutdown requests idempotent.
    drain_report: Option<ProviderDrainReport>,
    /// A failed drain is terminal for this supervisor generation. Retrying
    /// after a partial cleanup could mistake a released snapshot for a fully
    /// released admission reservation.
    drain_error: Option<EvaError>,
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
            provider: handle.provider.clone(),
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
            admission_table: None,
            rate_windows: BTreeMap::new(),
            circuit_states: BTreeMap::new(),
            draining: false,
            drain_report: None,
            drain_error: None,
        }
    }

    /// 设置 `process_table` 并返回更新后的实例。
    pub fn with_process_table(durable_table: FileSystemProviderProcessTable) -> Self {
        let admission_root = durable_table.root_path().join("admission");
        let admission_table = FileSystemProviderAdmissionTable::new(admission_root).ok();
        Self {
            table: InMemoryProviderProcessTable::new(),
            durable_table: Some(durable_table),
            admission_table,
            rate_windows: BTreeMap::new(),
            circuit_states: BTreeMap::new(),
            draining: false,
            drain_report: None,
            drain_error: None,
        }
    }

    pub(crate) fn admission_lease(
        &self,
        slot: &ProviderExecutionSlot,
    ) -> Result<Option<ProviderAdmissionLease>, EvaError> {
        let Some(table) = self.admission_table.clone() else {
            return Ok(None);
        };
        let reservation_id = slot.admission_reservation_id.clone().ok_or_else(|| {
            EvaError::conflict("provider execution slot lacks admission identity")
        })?;
        Ok(Some(ProviderAdmissionLease {
            table,
            adapter_id: slot.adapter_id.clone(),
            reservation_id,
            session_id: slot.session_id.clone(),
        }))
    }

    /// 执行 `active_for_adapter` 对应的处理逻辑。
    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        if let Some(table) = &self.durable_table {
            return Ok(table
                .list()?
                .into_iter()
                .filter(|snapshot| snapshot.active && &snapshot.adapter_id == adapter_id)
                .collect());
        }
        self.table.active_for_adapter(adapter_id)
    }

    /// Returns whether this supervisor has closed provider admission.
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// Close admission, wait for in-flight providers, and force-clean any
    /// boundary that remains at the absolute deadline. The gate is one-way;
    /// a successful report is cached so repeated daemon shutdowns cannot
    /// accidentally reopen admission or release a successor reservation.
    pub fn drain(
        &mut self,
        options: ProviderDrainOptions,
    ) -> Result<ProviderDrainReport, EvaError> {
        if let Some(previous) = &self.drain_report {
            let mut repeated = previous.clone();
            repeated.already_drained = true;
            return Ok(repeated);
        }
        if let Some(previous) = &self.drain_error {
            return Err(previous.clone());
        }
        self.draining = true;
        let result = self
            .reconcile_pending_admission_releases()
            .and_then(|_| self.drain_once(options));
        match result {
            Ok(report) => {
                self.drain_report = Some(report.clone());
                Ok(report)
            }
            Err(error) => {
                self.drain_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn drain_once(
        &mut self,
        options: ProviderDrainOptions,
    ) -> Result<ProviderDrainReport, EvaError> {
        let deadline = Instant::now()
            .checked_add(options.total_timeout())
            .ok_or_else(|| {
                EvaError::invalid_argument("provider drain deadline is outside the clock range")
            })?;
        let backend = OsProcessBackend::new();
        let mut terminated_provider_count = 0usize;
        let mut forced_provider_count = 0usize;
        let mut missing_identity_count = 0usize;
        let mut audit = vec![
            "provider.lifecycle:admission_closed".to_owned(),
            format!(
                "provider.lifecycle:drain_timeout_ms:{}",
                options.total_timeout().as_millis()
            ),
        ];

        loop {
            let active = self
                .processes()?
                .into_iter()
                .filter(|snapshot| snapshot.active)
                .collect::<Vec<_>>();
            if active.is_empty() {
                let report = ProviderDrainReport {
                    already_drained: false,
                    phase: "drained".to_owned(),
                    active_provider_count: 0,
                    terminated_provider_count,
                    forced_provider_count,
                    missing_identity_count,
                    audit: {
                        audit.push("provider.lifecycle:drained".to_owned());
                        audit
                    },
                };
                return Ok(report);
            }

            // The supervisor is single-threaded behind AdapterRuntime's
            // RefCell. Only a committed v3 slot that is still in the initial
            // or restart-starting state can be the known pre-spawn window.
            // Legacy v1/v2 records retain `restart_state=unconfigured` and
            // must remain fail-closed when their OS identity is missing.
            let mut retired_unregistered = false;
            for snapshot in &active {
                let unregistered = !snapshot.has_process_identity()
                    && snapshot.attempt == 0
                    && snapshot.record_version.0 > 0
                    && snapshot.restart_state != "unconfigured"
                    && !snapshot
                        .audit
                        .iter()
                        .any(|entry| entry == "provider.process:registered");
                if unregistered
                    && self.retire_snapshot_after_drain(
                        snapshot,
                        "provider supervisor drain closed an unregistered slot",
                    )?
                {
                    retired_unregistered = true;
                    missing_identity_count += 1;
                    audit.push(format!(
                        "provider.lifecycle:retired_unregistered:{}",
                        snapshot.session_id
                    ));
                }
            }
            if retired_unregistered {
                continue;
            }

            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            if remaining_budget.is_zero() {
                let remaining = active.len();
                return Err(EvaError::timeout(
                    "provider supervisor drain left active providers behind",
                )
                .with_context("active_provider_count", remaining.to_string())
                .with_context(
                    "terminated_provider_count",
                    terminated_provider_count.to_string(),
                )
                .with_context("cleanup_blocked", "true"));
            }

            // Start graceful termination while there is still budget. The
            // previous implementation waited until the deadline and passed
            // zero, which skipped the cooperative window entirely. Divide the
            // remaining budget across active boundaries so one provider cannot
            // consume the entire shutdown window before its siblings are
            // observed.
            let per_provider_budget = remaining_budget
                .checked_div(active.len() as u32)
                .unwrap_or(remaining_budget);
            let graceful_timeout = per_provider_budget / 2;
            let force_timeout = per_provider_budget.saturating_sub(graceful_timeout);
            let mut cleanup_blocked = false;
            for snapshot in &active {
                if !snapshot.has_process_identity() {
                    missing_identity_count += 1;
                    cleanup_blocked = true;
                    audit.push(format!(
                        "provider.lifecycle:missing_identity:{}",
                        snapshot.session_id
                    ));
                    continue;
                }
                match backend.terminate_snapshot_with_force_timeout(
                    snapshot,
                    graceful_timeout,
                    force_timeout,
                ) {
                    Ok(termination)
                        if matches!(
                            termination.outcome,
                            ProcessTerminationOutcome::AlreadyExited
                                | ProcessTerminationOutcome::Graceful
                                | ProcessTerminationOutcome::Forced
                        ) =>
                    {
                        terminated_provider_count += 1;
                        if termination.outcome == ProcessTerminationOutcome::Forced {
                            forced_provider_count += 1;
                        }
                        audit.extend(termination.audit_entries());
                        if self.retire_snapshot_after_drain(
                            snapshot,
                            "provider supervisor graceful drain",
                        )? {
                            audit.push(format!(
                                "provider.lifecycle:retired:{}",
                                snapshot.session_id
                            ));
                        }
                    }
                    Ok(termination) => {
                        cleanup_blocked = true;
                        audit.extend(termination.audit_entries());
                        audit.push(format!(
                            "provider.lifecycle:cleanup_blocked:{}",
                            snapshot.session_id
                        ));
                    }
                    Err(error) => {
                        cleanup_blocked = true;
                        audit.push(format!(
                            "provider.lifecycle:cleanup_error:{}",
                            sanitize_drain_value(error.message())
                        ));
                    }
                }
            }

            let remaining = self
                .processes()?
                .into_iter()
                .filter(|snapshot| snapshot.active)
                .count();
            if remaining == 0 {
                let report = ProviderDrainReport {
                    already_drained: false,
                    phase: "drained".to_owned(),
                    active_provider_count: 0,
                    terminated_provider_count,
                    forced_provider_count,
                    missing_identity_count,
                    audit: {
                        audit.push("provider.lifecycle:drained_after_graceful".to_owned());
                        audit
                    },
                };
                return Ok(report);
            }
            if Instant::now() >= deadline {
                return Err(EvaError::timeout(
                    "provider supervisor drain left active providers behind",
                )
                .with_context("active_provider_count", remaining.to_string())
                .with_context(
                    "terminated_provider_count",
                    terminated_provider_count.to_string(),
                )
                .with_context("cleanup_blocked", cleanup_blocked.to_string()));
            }
            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            thread::sleep(options.poll_interval().min(remaining_budget));
        }
    }

    fn retire_snapshot_after_drain(
        &mut self,
        snapshot: &ProviderProcessSnapshot,
        reason: &str,
    ) -> Result<bool, EvaError> {
        let mut current = match self.read_process(&snapshot.session_id) {
            Ok(current) => current,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !current.active {
            return Ok(false);
        }
        let admission_release = self.admission_release_for_snapshot(&current)?;
        if let Some((_, reservation_id)) = &admission_release {
            current.audit.push(format!(
                "{PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX}{reservation_id}"
            ));
        }
        // A completion racing with drain is allowed to win. The process-table
        // CAS below then either commits this terminal state or reports a
        // version conflict, which the next list pass re-evaluates.
        current.release("interrupted", Some(format!("provider shutdown: {reason}")))?;
        current
            .audit
            .push("provider.lifecycle:shutdown_retired".to_owned());
        let committed = match self.upsert_process(current) {
            Ok(committed) => committed,
            Err(error) if error.kind() == ErrorKind::Conflict => return Ok(false),
            Err(error) => return Err(error),
        };
        // Release only the reservation identity persisted with this provider
        // incarnation. A successor may reuse the session ID, but cannot reuse
        // the old reservation ID without violating the admission fence.
        if let Some((table, reservation_id)) = admission_release {
            match table.release_owned(
                &committed.adapter_id,
                &reservation_id,
                &committed.session_id,
            ) {
                Ok(()) => {
                    self.mark_admission_release_resolved(&committed, &reservation_id)?;
                }
                Err(error) if error.kind() == ErrorKind::Conflict => {
                    // The reservation may have expired between ownership
                    // proof and release. Reconcile immediately; a successor
                    // with the same session but a different reservation ID is
                    // preserved by the reconciliation fence.
                    self.reconcile_pending_admission_releases()?;
                    let latest = self.read_process(&committed.session_id)?;
                    if pending_release_id(&latest).is_some_and(|id| id == reservation_id) {
                        return Err(error
                            .with_context("session_id", &committed.session_id)
                            .with_context("reservation_id", reservation_id));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    fn admission_release_for_snapshot(
        &self,
        snapshot: &ProviderProcessSnapshot,
    ) -> Result<Option<(FileSystemProviderAdmissionTable, String)>, EvaError> {
        let Some(table) = self.admission_table.clone() else {
            return Ok(None);
        };
        let expected_reservation_id = reservation_id_from_audit(snapshot).ok_or_else(|| {
            EvaError::conflict("provider drain lacks durable admission reservation identity")
                .with_context("session_id", &snapshot.session_id)
        })?;
        let state = table.snapshot(&snapshot.adapter_id, now_ms())?;
        let same_session = state
            .reservations
            .iter()
            .filter(|reservation| reservation.session_id == snapshot.session_id)
            .collect::<Vec<_>>();
        match same_session.as_slice() {
            [reservation] if reservation.reservation_id == expected_reservation_id => {
                Ok(Some((table, expected_reservation_id)))
            }
            // An expired reservation is already absent after `snapshot`'
            // cleanup. There is nothing left to release; a successor would
            // have appeared in this same list and is handled as a conflict.
            [] => Ok(None),
            _ => Err(
                EvaError::conflict("provider drain found ambiguous admission reservations")
                    .with_context("session_id", &snapshot.session_id),
            ),
        }
    }

    fn mark_admission_release_resolved(
        &mut self,
        snapshot: &ProviderProcessSnapshot,
        reservation_id: &str,
    ) -> Result<(), EvaError> {
        let mut current = self.read_process(&snapshot.session_id)?;
        if !current.audit.iter().any(|entry| {
            entry == &format!("{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{reservation_id}")
        }) {
            current.audit.push(format!(
                "{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{reservation_id}"
            ));
            match self.upsert_process(current) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::Conflict => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Replays durable release intents left by a crash or an admission-table
    /// I/O failure. The exact reservation ID is the fence; a successor using
    /// the same session ID is never removed.
    fn reconcile_pending_admission_releases(&mut self) -> Result<(), EvaError> {
        let Some(table) = self.admission_table.clone() else {
            return Ok(());
        };
        let snapshots = self.processes()?;
        for snapshot in snapshots {
            for reservation_id in pending_release_ids(&snapshot) {
                let state = table.snapshot(&snapshot.adapter_id, now_ms())?;
                let exact = state.reservations.iter().any(|reservation| {
                    reservation.reservation_id == reservation_id
                        && reservation.session_id == snapshot.session_id
                });
                let successor = state.reservations.iter().any(|reservation| {
                    reservation.session_id == snapshot.session_id
                        && reservation.reservation_id != reservation_id
                });
                if exact {
                    table.release_owned(
                        &snapshot.adapter_id,
                        &reservation_id,
                        &snapshot.session_id,
                    )?;
                }
                // A successor with the same session ID is intentionally
                // preserved: the old reservation is already absent, so
                // resolving this intent is safe and avoids retrying forever
                // after an expiry/restart.
                let mut current = self.read_process(&snapshot.session_id)?;
                if successor
                    && !current.audit.iter().any(|entry| {
                        entry == &format!("provider.admission:successor_preserved:{reservation_id}")
                    })
                {
                    current.audit.push(format!(
                        "provider.admission:successor_preserved:{reservation_id}"
                    ));
                    self.upsert_process(current)?;
                }
                self.mark_admission_release_resolved(&snapshot, &reservation_id)?;
            }
        }
        Ok(())
    }
}

impl ProviderSupervisor for InMemoryProviderSupervisor {
    /// 按熔断、并发、速率的顺序完成准入，全部通过后才写入运行中快照。
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError> {
        self.reconcile_pending_admission_releases()?;
        if self.draining {
            return Err(admission_error(
                &request,
                "provider supervisor is draining",
                "provider_draining",
                None,
            ));
        }
        let now = now_ms();
        let session_id = session_id(&request.request_id, &request.adapter_id);
        let provider_process_id = provider_process_id(&request.request_id, &request.adapter_id);
        match self.read_process(&session_id) {
            Ok(mut existing) => {
                ensure_request_matches_snapshot(&request, &existing)?;
                existing.prepare_for_restart(now)?;
                let admission_reservation_id = if let Some(table) = &self.admission_table {
                    let matches = table
                        .snapshot(&existing.adapter_id, now)?
                        .reservations
                        .into_iter()
                        .filter(|reservation| reservation.session_id == existing.session_id)
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [reservation] => Some(reservation.reservation_id.clone()),
                        [] => {
                            return Err(EvaError::conflict(
                                "provider restart lacks an active admission reservation",
                            ))
                        }
                        _ => {
                            return Err(EvaError::conflict(
                                "provider restart has conflicting admission reservations",
                            ))
                        }
                    }
                } else {
                    None
                };
                if let Some(reservation_id) = &admission_reservation_id {
                    existing.audit.push(format!(
                        "{PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX}{reservation_id}"
                    ));
                }
                let committed = self.upsert_process(existing)?;
                return Ok(ProviderExecutionSlot {
                    session_id: committed.session_id,
                    provider_process_id: committed.provider_process_id,
                    request_id: committed.request_id,
                    adapter_id: committed.adapter_id,
                    admission_reservation_id,
                    half_open_probe: false,
                });
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // 熔断检查会占用半开探测权，因此其后任何检查失败都不能产生进程快照。
        let half_open_probe = self.admit_circuit(&request, now)?;
        self.admit_concurrency(&request)?;
        self.admit_rate_limit(&request, now)?;
        let limit_audit = limit_audit_entries(&request, half_open_probe);
        let restart_policy = request.provider.restart.mode.as_str().to_owned();
        let mut snapshot = ProviderProcessSnapshot::running(
            session_id.clone(),
            provider_process_id.clone(),
            request.request_id.clone(),
            request.adapter_id.clone(),
            request.capability,
            request.transport.as_str(),
            request.manifest_digest,
            request.start_command,
            restart_policy,
        );
        snapshot.audit.extend(limit_audit);
        snapshot.retry_backoff_ms = request.retry_backoff_ms;
        snapshot.configure_restart_budget(
            request.provider.restart.max_attempts,
            request.provider.restart.backoff_ms,
        )?;
        let admission_reservation = if let Some(table) = &self.admission_table {
            Some(table.reserve(
                &request.adapter_id,
                request.max_concurrency.unwrap_or(usize::MAX),
                &session_id,
                now,
                eva_storage::DEFAULT_RESERVATION_TTL_MS,
            )?)
        } else {
            None
        };
        let admission_reservation_id = admission_reservation
            .as_ref()
            .map(|reservation| reservation.reservation_id.clone());
        if let Some(reservation_id) = &admission_reservation_id {
            snapshot.audit.push(format!(
                "{PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX}{reservation_id}"
            ));
        }
        if let Err(error) = self.upsert_process(snapshot) {
            if let (Some(table), Some(reservation_id)) =
                (&self.admission_table, admission_reservation_id.as_deref())
            {
                let _ = table.release_owned(&request.adapter_id, reservation_id, &session_id);
            }
            return Err(error);
        }
        Ok(ProviderExecutionSlot {
            session_id,
            provider_process_id,
            request_id: request.request_id,
            adapter_id: request.adapter_id,
            admission_reservation_id,
            half_open_probe,
        })
    }

    /// 将运行中快照释放为终态，再用同一结果更新熔断状态并同步持久化镜像。
    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = if let Some(table) = &self.durable_table {
            table.read(&slot.session_id)?
        } else {
            self.table.read(&slot.session_id)?
        };
        if !snapshot.active {
            return Err(EvaError::conflict(
                "provider execution slot has already reached a terminal state",
            )
            .with_context("session_id", &slot.session_id)
            .with_context("health", &snapshot.health));
        }
        snapshot.release(outcome.health, outcome.last_error)?;
        if snapshot.last_error.is_none() && snapshot.health == "completed" {
            snapshot.mark_stable_success()?;
        }
        self.record_circuit_outcome(slot, &mut snapshot);
        let result = self.upsert_process(snapshot);
        if result.is_ok() {
            if let Some(table) = &self.admission_table {
                let reservation_id = slot.admission_reservation_id.as_deref().ok_or_else(|| {
                    EvaError::conflict("provider execution slot lacks admission identity")
                })?;
                table.release_owned(&slot.adapter_id, reservation_id, &slot.session_id)?;
            }
        }
        result
    }

    fn renew_admission(
        &mut self,
        slot: &ProviderExecutionSlot,
        now_ms: u128,
    ) -> Result<(), EvaError> {
        let table = self.admission_table.as_ref().ok_or_else(|| {
            EvaError::unsupported("provider supervisor has no durable admission table")
        })?;
        let reservation_id = slot.admission_reservation_id.as_deref().ok_or_else(|| {
            EvaError::conflict("provider execution slot lacks admission identity")
        })?;
        table.renew(
            &slot.adapter_id,
            reservation_id,
            &slot.session_id,
            now_ms,
            eva_storage::DEFAULT_RESERVATION_TTL_MS,
        )?;
        Ok(())
    }

    fn register_process_identity(
        &mut self,
        slot: &ProviderExecutionSlot,
        identity: &ProcessIdentity,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        InMemoryProviderSupervisor::register_process_identity(self, slot, identity)
    }

    fn schedule_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        attempt: u32,
        due_at_ms: u128,
        reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if self.draining {
            return Err(EvaError::unavailable(
                "provider supervisor is draining; restart scheduling is closed",
            )
            .with_provider_code("provider_draining")
            .with_context("session_id", &slot.session_id));
        }
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.mark_restart_pending(attempt, due_at_ms, reason)?;
        self.upsert_process(snapshot)
    }

    fn prepare_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        now_ms: u128,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if self.draining {
            return Err(EvaError::unavailable(
                "provider supervisor is draining; restart admission is closed",
            )
            .with_provider_code("provider_draining")
            .with_context("session_id", &slot.session_id));
        }
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.prepare_for_restart(now_ms)?;
        self.upsert_process(snapshot)
    }

    fn exhaust_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.mark_restart_exhausted(reason)?;
        self.upsert_process(snapshot)
    }

    fn fail_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.mark_restart_failed(reason)?;
        self.upsert_process(snapshot)
    }

    fn snapshot(&self, slot: &ProviderExecutionSlot) -> Result<ProviderProcessSnapshot, EvaError> {
        let snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        Ok(snapshot)
    }

    /// 执行 `processes` 对应的处理逻辑。
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        if let Some(table) = &self.durable_table {
            return table.list();
        }
        self.table.list()
    }

    fn drain(&mut self, options: ProviderDrainOptions) -> Result<ProviderDrainReport, EvaError> {
        InMemoryProviderSupervisor::drain(self, options)
    }
}

impl InMemoryProviderSupervisor {
    /// Attach a real OS identity to an already admitted provider slot.
    ///
    /// The slot is created before transport spawn so admission limits remain
    /// authoritative. This method performs the second, fenced CAS immediately
    /// after spawn; callers must terminate the returned handle if this method
    /// fails, ensuring a failed registration cannot leave an orphan.
    pub(crate) fn register_process_identity(
        &mut self,
        slot: &ProviderExecutionSlot,
        identity: &ProcessIdentity,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if self.draining {
            return Err(EvaError::unavailable(
                "provider supervisor is draining; process registration is closed",
            )
            .with_provider_code("provider_draining")
            .with_context("session_id", &slot.session_id));
        }
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        let restarting = snapshot.restart_state == "starting";
        if (!snapshot.active || snapshot.health != "running") && !restarting {
            return Err(EvaError::conflict(
                "provider process registration requires an active running slot",
            )
            .with_context("session_id", &slot.session_id));
        }
        if snapshot.has_process_identity() {
            return Err(
                EvaError::conflict("provider process slot already has an OS identity")
                    .with_context("session_id", &slot.session_id),
            );
        }
        let process_attempt = snapshot.next_process_attempt();
        identity.stamp_snapshot(&mut snapshot, process_attempt)?;
        if restarting {
            snapshot.mark_restart_running()?;
        }
        snapshot
            .audit
            .push("provider.process:registered".to_owned());
        snapshot
            .audit
            .push(format!("provider.pid:{}", identity.pid));
        snapshot.audit.push(format!(
            "provider.process_boundary:{}",
            if identity.process_group_id.is_some() {
                "unix_group"
            } else {
                "windows_job"
            }
        ));
        self.upsert_process(snapshot)
    }

    fn read_process(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        if let Some(table) = &self.durable_table {
            table.read(session_id)
        } else {
            self.table.read(session_id)
        }
    }

    /// 执行 `upsert_process` 对应的处理逻辑。
    fn upsert_process(
        &mut self,
        snapshot: ProviderProcessSnapshot,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if let Some(table) = &mut self.durable_table {
            // The durable table is authoritative whenever configured; all
            // reads and completion paths use it, so no second mirror CAS can
            // create a partial-commit failure after this write succeeds.
            table.compare_and_set(snapshot)
        } else {
            self.table.compare_and_set(snapshot)
        }
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

fn sanitize_drain_value(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn reservation_id_from_audit(snapshot: &ProviderProcessSnapshot) -> Option<String> {
    snapshot.audit.iter().rev().find_map(|entry| {
        entry
            .strip_prefix(PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn pending_release_id(snapshot: &ProviderProcessSnapshot) -> Option<String> {
    pending_release_ids(snapshot).pop()
}

fn pending_release_ids(snapshot: &ProviderProcessSnapshot) -> Vec<String> {
    let mut pending = Vec::new();
    for entry in &snapshot.audit {
        let Some(reservation_id) = entry
            .strip_prefix(PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let resolved =
            format!("{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{reservation_id}");
        if !snapshot.audit.iter().any(|item| item == &resolved)
            && !pending.iter().any(|item| item == reservation_id)
        {
            pending.push(reservation_id.to_owned());
        }
    }
    pending
}

fn ensure_slot_matches_snapshot(
    slot: &ProviderExecutionSlot,
    snapshot: &ProviderProcessSnapshot,
) -> Result<(), EvaError> {
    if snapshot.provider_process_id == slot.provider_process_id
        && snapshot.request_id == slot.request_id
        && snapshot.adapter_id == slot.adapter_id
    {
        return Ok(());
    }
    Err(
        EvaError::conflict("provider execution slot identity does not match durable snapshot")
            .with_context("session_id", &slot.session_id),
    )
}

fn ensure_request_matches_snapshot(
    request: &ProviderExecutionRequest,
    snapshot: &ProviderProcessSnapshot,
) -> Result<(), EvaError> {
    let matches = snapshot.request_id == request.request_id
        && snapshot.adapter_id == request.adapter_id
        && snapshot.capability == request.capability
        && snapshot.transport == request.transport.as_str()
        && snapshot.manifest_digest == request.manifest_digest
        && snapshot.start_command == request.start_command
        && snapshot.restart_policy == request.provider.restart.mode.as_str()
        && snapshot.restart_max_attempts == request.provider.restart.max_attempts
        && snapshot.restart_backoff_ms == request.provider.restart.backoff_ms;
    if matches {
        return Ok(());
    }
    Err(
        EvaError::conflict("provider restart request does not match its durable session identity")
            .with_context("session_id", &snapshot.session_id)
            .with_context("adapter_id", request.adapter_id.as_str())
            .with_context("request_id", request.request_id.as_str()),
    )
}

/// 执行 `limit_audit_entries` 对应的处理逻辑。
fn limit_audit_entries(request: &ProviderExecutionRequest, half_open_probe: bool) -> Vec<String> {
    let mut audit = vec![
        format!(
            "provider.restart.mode:{}",
            request.provider.restart.mode.as_str()
        ),
        format!(
            "provider.restart.max_attempts:{}",
            request.provider.restart.max_attempts
        ),
        format!(
            "provider.restart.backoff_ms:{}",
            request.provider.restart.backoff_ms
        ),
        format!("provider.run_as.kind:{}", request.provider.run_as.kind()),
        format!(
            "provider.vault_secret_refs:{}",
            request.provider.vault_secrets.len()
        ),
    ];
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
    let mut material = Vec::new();
    push_digest_field(&mut material, "format", "eva.adapter.manifest.v3");
    push_digest_field(&mut material, "id", handle.id.as_str());
    push_digest_field(&mut material, "version", &handle.version);
    push_digest_field(&mut material, "transport", handle.transport.as_str());
    push_digest_field(&mut material, "source_path", &handle.source_path);
    push_digest_field(
        &mut material,
        "command",
        handle.command.as_deref().unwrap_or(""),
    );
    push_digest_collection(&mut material, "arg", handle.args.iter().map(String::as_str));
    push_digest_field(
        &mut material,
        "endpoint",
        handle.endpoint.as_deref().unwrap_or(""),
    );
    push_digest_field(
        &mut material,
        "mcp_command",
        handle.mcp_command.as_deref().unwrap_or(""),
    );
    push_digest_collection(
        &mut material,
        "mcp_arg",
        handle.mcp_args.iter().map(String::as_str),
    );
    push_digest_field(
        &mut material,
        "skill_runner_command",
        handle.skill_runner_command.as_deref().unwrap_or(""),
    );
    push_digest_collection(
        &mut material,
        "skill_runner_arg",
        handle.skill_runner_args.iter().map(String::as_str),
    );
    push_digest_field(
        &mut material,
        "skill_name",
        handle.skill_name().unwrap_or(""),
    );

    let mut capabilities = handle
        .capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>();
    capabilities.sort_unstable();
    push_digest_collection(&mut material, "capability", capabilities);

    let mut credential_env = handle
        .credential_env
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    credential_env.sort_unstable();
    push_digest_collection(&mut material, "credential_env", credential_env);

    push_digest_field(
        &mut material,
        "restart_mode",
        handle.provider.restart.mode.as_str(),
    );
    push_digest_field(
        &mut material,
        "restart_max_attempts",
        &handle.provider.restart.max_attempts.to_string(),
    );
    push_digest_field(
        &mut material,
        "restart_backoff_ms",
        &handle.provider.restart.backoff_ms.to_string(),
    );
    match &handle.provider.run_as {
        ProviderRunAsIdentity::Current => {
            push_digest_field(&mut material, "run_as_kind", "current");
        }
        ProviderRunAsIdentity::Unix { uid, gid } => {
            push_digest_field(&mut material, "run_as_kind", "unix");
            push_digest_field(&mut material, "run_as_uid", &uid.to_string());
            push_digest_field(&mut material, "run_as_gid", &gid.to_string());
        }
        ProviderRunAsIdentity::Windows { account } => {
            push_digest_field(&mut material, "run_as_kind", "windows");
            push_digest_field(&mut material, "run_as_account", account);
        }
    }

    let mut vault_secrets = handle.provider.vault_secrets.iter().collect::<Vec<_>>();
    vault_secrets.sort_unstable();
    push_digest_field(
        &mut material,
        "vault_secret_count",
        &vault_secrets.len().to_string(),
    );
    for secret in vault_secrets {
        push_digest_field(&mut material, "vault_env", &secret.env);
        push_digest_field(&mut material, "vault_ref", &secret.secret_ref);
    }

    format!("fnv64:{:016x}", fnv1a64(&material))
}

/// Appends one labeled, length-prefixed field to canonical digest material.
fn push_digest_field(material: &mut Vec<u8>, label: &str, value: &str) {
    push_digest_bytes(material, label.as_bytes());
    push_digest_bytes(material, value.as_bytes());
}

/// Appends an ordered collection with an explicit count and repeated field label.
fn push_digest_collection<'a>(
    material: &mut Vec<u8>,
    label: &str,
    values: impl IntoIterator<Item = &'a str>,
) {
    let values = values.into_iter().collect::<Vec<_>>();
    push_digest_field(
        material,
        &format!("{label}_count"),
        &values.len().to_string(),
    );
    for value in values {
        push_digest_field(material, label, value);
    }
}

/// Uses a platform-independent u64 byte length before every digest component.
fn push_digest_bytes(material: &mut Vec<u8>, value: &[u8]) {
    material.extend_from_slice(&(value.len() as u64).to_be_bytes());
    material.extend_from_slice(value);
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
            provider: ProviderConfig::default(),
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

    #[test]
    fn supervisor_drain_closes_admission_and_is_idempotent() {
        let handle = handle();
        let initial_invocation = invocation("req-supervisor-drain");
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &initial_invocation,
            ))
            .unwrap();

        let options = ProviderDrainOptions::new(Duration::from_millis(50))
            .unwrap()
            .with_poll_interval(Duration::from_millis(1))
            .unwrap();
        let report = supervisor.drain(options).unwrap();
        assert_eq!(report.phase, "drained");
        assert_eq!(report.active_provider_count, 0);
        assert!(!report.already_drained);
        assert!(supervisor.is_draining());
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "provider.lifecycle:admission_closed"));

        let rejected = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-drain-after"),
            ))
            .unwrap_err();
        assert_eq!(
            rejected.provider_code().map(|code| code.as_str()),
            Some("provider_draining")
        );

        let repeated = supervisor.drain(options).unwrap();
        assert!(repeated.already_drained);
        assert_eq!(repeated.phase, "drained");
        assert!(supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .is_err());

        let registration_error = supervisor
            .register_process_identity(
                &slot,
                &ProcessIdentity {
                    pid: 1,
                    process_start_token: "drain-test".to_owned(),
                    process_group_id: Some(1),
                    job_id: None,
                },
            )
            .unwrap_err();
        assert_eq!(
            registration_error.provider_code().map(|code| code.as_str()),
            Some("provider_draining")
        );
    }

    #[test]
    fn supervisor_drain_keeps_legacy_identityless_snapshot_fail_closed() {
        let handle = handle();
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-legacy-drain"),
            ))
            .unwrap();

        // A v1/v2-compatible active record has a positive CAS version after
        // persistence but retains the legacy `unconfigured` restart state.
        // It must not be mistaken for the known pre-spawn window.
        let mut legacy = supervisor.table.read(&slot.session_id).unwrap();
        legacy.restart_state = "unconfigured".to_owned();
        supervisor.table.compare_and_set(legacy).unwrap();

        let options = ProviderDrainOptions::new(Duration::from_millis(20))
            .unwrap()
            .with_poll_interval(Duration::from_millis(1))
            .unwrap();
        let first = supervisor.drain(options).unwrap_err();
        assert_eq!(first.kind(), eva_core::ErrorKind::Timeout);
        assert!(first
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "active_provider_count" && value == "1"));
        assert!(supervisor.table.read(&slot.session_id).unwrap().active);

        // A failed drain is terminal for this supervisor generation. The
        // cached error prevents a later call from claiming success after a
        // partial cleanup.
        let repeated = supervisor.drain(options).unwrap_err();
        assert_eq!(repeated, first);
        assert!(supervisor.is_draining());
    }

    #[test]
    fn supervisor_drain_never_releases_a_successor_reservation() {
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-drain-successor-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let mut handle = handle();
        handle.max_concurrency = Some(1);
        let mut supervisor = InMemoryProviderSupervisor::new();
        supervisor.admission_table = Some(table.clone());
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-successor-drain"),
            ))
            .unwrap();
        let original_reservation_id = slot.admission_reservation_id.clone().unwrap();
        let original = table
            .snapshot(&handle.id, now_ms())
            .unwrap()
            .reservations
            .into_iter()
            .find(|reservation| reservation.reservation_id == original_reservation_id)
            .unwrap();
        let successor = table
            .reserve(
                &handle.id,
                1,
                &slot.session_id,
                original.expires_at_ms,
                eva_storage::DEFAULT_RESERVATION_TTL_MS,
            )
            .unwrap();
        assert_ne!(successor.reservation_id, original_reservation_id);

        let options = ProviderDrainOptions::new(Duration::from_millis(20))
            .unwrap()
            .with_poll_interval(Duration::from_millis(1))
            .unwrap();
        let first = supervisor.drain(options).unwrap_err();
        assert_eq!(first.kind(), eva_core::ErrorKind::Conflict);
        assert!(supervisor.table.read(&slot.session_id).unwrap().active);
        assert_eq!(
            table.snapshot(&handle.id, now_ms()).unwrap().reservations,
            vec![successor.clone()]
        );

        // The failed result is cached, and a retry cannot consume the
        // successor reservation under the old session ID.
        assert_eq!(supervisor.drain(options).unwrap_err(), first);
        assert_eq!(
            table.snapshot(&handle.id, now_ms()).unwrap().reservations,
            vec![successor]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn supervisor_reconciles_expired_release_intent_without_touching_successor() {
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-drain-pending-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let mut handle = handle();
        handle.max_concurrency = Some(1);
        let mut supervisor = InMemoryProviderSupervisor::new();
        supervisor.admission_table = Some(table.clone());
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-pending-release"),
            ))
            .unwrap();
        let old_reservation_id = slot.admission_reservation_id.clone().unwrap();
        let old_reservation = table
            .snapshot(&handle.id, now_ms())
            .unwrap()
            .reservations
            .into_iter()
            .find(|reservation| reservation.reservation_id == old_reservation_id)
            .unwrap();

        // Simulate a crash after the provider snapshot CAS but before the
        // admission release. Include an older unresolved intent to prove one
        // reconciliation pass drains the complete durable backlog rather than
        // only the newest marker.
        let mut retired = supervisor.table.read(&slot.session_id).unwrap();
        let older_reservation_id = "expired-before-current";
        retired.audit.push(format!(
            "{PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX}{older_reservation_id}"
        ));
        retired.audit.push(format!(
            "{PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX}{old_reservation_id}"
        ));
        retired
            .release("interrupted", Some("simulated drain crash".to_owned()))
            .unwrap();
        supervisor.table.compare_and_set(retired).unwrap();

        // Expiry allows a successor reservation with the same session ID to
        // appear. Reconciliation must preserve it and only resolve the old
        // release intent.
        let successor = table
            .reserve(
                &handle.id,
                1,
                &slot.session_id,
                old_reservation.expires_at_ms,
                eva_storage::DEFAULT_RESERVATION_TTL_MS,
            )
            .unwrap();
        assert_ne!(successor.reservation_id, old_reservation_id);

        let report = supervisor
            .drain(
                ProviderDrainOptions::new(Duration::from_millis(20))
                    .unwrap()
                    .with_poll_interval(Duration::from_millis(1))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(report.phase, "drained");
        let reconciled = supervisor.table.read(&slot.session_id).unwrap();
        assert!(pending_release_id(&reconciled).is_none());
        assert!(reconciled.audit.iter().any(|entry| {
            entry == &format!("provider.admission:successor_preserved:{old_reservation_id}")
        }));
        assert!(reconciled.audit.iter().any(|entry| {
            entry
                == &format!(
                    "{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{old_reservation_id}"
                )
        }));
        assert!(reconciled.audit.iter().any(|entry| {
            entry
                == &format!(
                    "{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{older_reservation_id}"
                )
        }));
        assert_eq!(
            table.snapshot(&handle.id, now_ms()).unwrap().reservations,
            vec![successor]
        );
        let _ = std::fs::remove_dir_all(root);
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

    #[test]
    fn manifest_digest_binds_provider_restart_identity_env_and_vault_refs() {
        let mut baseline = handle();
        baseline.credential_env = vec!["API_TOKEN".to_owned()];
        let baseline_digest =
            ProviderExecutionRequest::from_handle(&baseline, &invocation("req-provider-digest"))
                .manifest_digest;

        let mut restart_changed = baseline.clone();
        restart_changed.provider.restart = eva_config::ProviderRestartConfig {
            mode: eva_config::ProviderRestartMode::OnFailure,
            max_attempts: 2,
            backoff_ms: 100,
        };
        assert_ne!(
            baseline_digest,
            ProviderExecutionRequest::from_handle(
                &restart_changed,
                &invocation("req-provider-digest"),
            )
            .manifest_digest
        );

        let mut identity_changed = baseline.clone();
        identity_changed.provider.run_as = eva_config::ProviderRunAsIdentity::Unix {
            uid: 1000,
            gid: 1001,
        };
        assert_ne!(
            baseline_digest,
            ProviderExecutionRequest::from_handle(
                &identity_changed,
                &invocation("req-provider-digest"),
            )
            .manifest_digest
        );

        let mut env_changed = baseline.clone();
        env_changed.credential_env.push("SECOND_TOKEN".to_owned());
        assert_ne!(
            baseline_digest,
            ProviderExecutionRequest::from_handle(
                &env_changed,
                &invocation("req-provider-digest"),
            )
            .manifest_digest
        );

        let mut vault_changed = baseline;
        vault_changed
            .provider
            .vault_secrets
            .push(eva_config::ProviderVaultSecretRef {
                env: "API_TOKEN".to_owned(),
                secret_ref: "vault://providers/digest/token#value".to_owned(),
            });
        let vault_digest = ProviderExecutionRequest::from_handle(
            &vault_changed,
            &invocation("req-provider-digest"),
        )
        .manifest_digest;
        assert_ne!(baseline_digest, vault_digest);
    }

    #[test]
    fn provider_audit_contains_only_identity_kind_and_vault_count() {
        let mut configured = handle();
        configured.provider.run_as = eva_config::ProviderRunAsIdentity::Windows {
            account: "SecretAccountName".to_owned(),
        };
        configured.provider.vault_secrets = vec![eva_config::ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://providers/audit/token#value".to_owned(),
        }];
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &configured,
                &invocation("req-provider-audit"),
            ))
            .unwrap();
        let snapshot = supervisor.processes().unwrap().pop().unwrap();

        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.run_as.kind:windows"));
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.vault_secret_refs:1"));
        assert!(!snapshot
            .audit
            .iter()
            .any(|entry| entry.contains("SecretAccountName") || entry.contains("vault://")));
        assert!(!snapshot.manifest_digest.contains("SecretAccountName"));
        assert!(!snapshot
            .manifest_digest
            .contains("vault://providers/audit/token"));

        supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .unwrap();
    }

    #[test]
    fn manifest_digest_canonicalizes_unordered_provider_sets() {
        let mut first = handle();
        first.credential_env = vec!["SECOND_TOKEN".to_owned(), "API_TOKEN".to_owned()];
        first.provider.vault_secrets = vec![
            eva_config::ProviderVaultSecretRef {
                env: "SECOND_TOKEN".to_owned(),
                secret_ref: "vault://providers/z/token".to_owned(),
            },
            eva_config::ProviderVaultSecretRef {
                env: "API_TOKEN".to_owned(),
                secret_ref: "vault://providers/a/token".to_owned(),
            },
        ];
        let mut second = first.clone();
        second.credential_env.reverse();
        second.provider.vault_secrets.reverse();

        let first_digest =
            ProviderExecutionRequest::from_handle(&first, &invocation("req-provider-canonical-a"))
                .manifest_digest;
        let second_digest =
            ProviderExecutionRequest::from_handle(&second, &invocation("req-provider-canonical-b"))
                .manifest_digest;

        assert_eq!(first_digest, second_digest);
    }

    #[test]
    fn manifest_digest_preserves_native_argv_boundaries() {
        let digest = |handle: &AdapterHandle, request_id: &str| {
            ProviderExecutionRequest::from_handle(handle, &invocation(request_id)).manifest_digest
        };

        let mut stdio_single = handle();
        stdio_single.args = vec!["--scope workspace".to_owned()];
        let mut stdio_split = stdio_single.clone();
        stdio_split.args = vec!["--scope".to_owned(), "workspace".to_owned()];
        assert_ne!(
            digest(&stdio_single, "req-provider-stdio-single"),
            digest(&stdio_split, "req-provider-stdio-split")
        );

        let mut mcp_single = handle();
        mcp_single.transport = AdapterTransport::Mcp;
        mcp_single.mcp_command = Some("provider".to_owned());
        mcp_single.mcp_args = vec!["--scope workspace".to_owned()];
        let mut mcp_split = mcp_single.clone();
        mcp_split.mcp_args = vec!["--scope".to_owned(), "workspace".to_owned()];
        assert_ne!(
            digest(&mcp_single, "req-provider-mcp-single"),
            digest(&mcp_split, "req-provider-mcp-split")
        );

        let mut skill_single = handle();
        skill_single.transport = AdapterTransport::Skill;
        skill_single.skill_runner_command = Some("provider".to_owned());
        skill_single.skill_runner_args = vec!["--scope workspace".to_owned()];
        let mut skill_split = skill_single.clone();
        skill_split.skill_runner_args = vec!["--scope".to_owned(), "workspace".to_owned()];
        assert_ne!(
            digest(&skill_single, "req-provider-skill-single"),
            digest(&skill_split, "req-provider-skill-split")
        );
    }
}
