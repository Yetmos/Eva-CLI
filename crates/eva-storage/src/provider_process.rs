//! Provider 进程/会话快照、文件表和 daemon 重启恢复契约。
//! Provider process/session table contracts.

use crate::durable_backend::{
    acquire_record_write_lock, atomic_write, DurableWriterGuard, WriterGeneration,
};
use crate::state_store::StateVersion;
use crate::DurableBackendLayout;
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本模块的架构职责：跨进程保存 provider 会话身份、健康、重启策略与审计链。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider process/session table snapshots";

/// Supervisor 与重启恢复共享的可查询 provider 执行快照。
/// Queryable provider execution state shared by supervisors and future recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProcessSnapshot {
    /// Supervisor 分配的逻辑会话 ID，也是表的 upsert key。
    pub session_id: String,
    /// 实际 provider 进程/槽位 ID。
    pub provider_process_id: String,
    /// OS 分配的真实进程 ID；中央 process backend 接线前可以为空。
    pub pid: Option<u32>,
    /// 与 PID 配对的进程 incarnation/start token，防止 PID 复用误认。
    pub process_start_token: Option<String>,
    /// Unix process group ID；与 Windows job_id 互斥。
    pub process_group_id: Option<u32>,
    /// Windows Job Object stable identity；与 Unix process_group_id 互斥。
    pub job_id: Option<String>,
    /// 触发 provider 执行的请求 ID。
    pub request_id: RequestId,
    /// Provider 所属 Adapter ID。
    pub adapter_id: AdapterId,
    /// 本会话执行的 capability。
    pub capability: CapabilityName,
    /// stdio/http 等 transport 标识。
    pub transport: String,
    /// 启动时 manifest 内容摘要，用于恢复时判断配置漂移。
    pub manifest_digest: String,
    /// 诊断用启动命令文本；执行由 supervisor 负责。
    pub start_command: String,
    /// running/failed/interrupted 等健康状态。
    pub health: String,
    /// Supervisor 重启策略标识。
    pub restart_policy: String,
    /// 可选重试退避毫秒数。
    pub retry_backoff_ms: Option<u64>,
    /// Provider 自动重启 attempt，首次启动从 1 开始；未接中央 spawn 时为 0。
    pub attempt: u32,
    /// Provider manifest 允许消耗的自动重启次数；初始进程不计入预算。
    pub restart_max_attempts: u32,
    /// Provider manifest 声明的指数退避基准毫秒数。
    pub restart_backoff_ms: u64,
    /// 已经 durable 提交、且不可回收的自动重启次数。
    pub restart_attempts: u32,
    /// 下一次自动重启允许开始的 epoch 毫秒。
    pub restart_due_at_ms: Option<u128>,
    /// disabled、stable、pending 或 exhausted。
    pub restart_state: String,
    /// Provider record 的单调 CAS 版本；零只表示尚未持久化的候选或 legacy v1。
    pub record_version: StateVersion,
    /// 最后提交该记录的 durable writer generation；内存表使用零。
    pub owner_generation: WriterGeneration,
    /// 会话是否仍被视为活动；重启扫描会关闭遗留活动会话。
    pub active: bool,
    /// 最近一次 provider 错误；重启中断不会覆盖已有根因。
    pub last_error: Option<String>,
    /// 初次启动 epoch 毫秒。
    pub started_at_ms: u128,
    /// 最近状态变化 epoch 毫秒。
    pub updated_at_ms: u128,
    /// 按发生顺序保存的 supervisor/recovery 审计条目。
    pub audit: Vec<String>,
}

/// V1.13 supervisor 所需的 provider 会话表行为。
/// Provider process/session table behavior required by V1.13 supervision.
pub trait ProviderProcessTable {
    /// 按 session ID 以 candidate.record_version 执行 fenced compare-and-set。
    /// 成功返回带新 record_version/owner_generation 的权威记录。
    fn compare_and_set(
        &mut self,
        candidate: ProviderProcessSnapshot,
    ) -> Result<ProviderProcessSnapshot, EvaError>;
    /// Returns the writer generation fencing mutations, when this table is
    /// backed by a runtime writer. In-memory tables have no owner fence.
    fn writer_generation(&self) -> Option<WriterGeneration> {
        None
    }
    /// 兼容旧调用方的 CAS 别名；不会绕过版本检查或 writer ownership。
    fn upsert(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError> {
        self.compare_and_set(snapshot).map(|_| ())
    }
    /// 精确读取一个 session；缺失返回 NotFound。
    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError>;
    /// 返回所有可解析快照；文件实现遇到任一损坏文件会整体失败。
    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;
}

/// 首版 provider supervisor 使用的内存进程表。
/// In-memory process table used by the first provider supervisor baseline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryProviderProcessTable {
    /// 按 session ID 有序保存，保证 list 结果确定。
    snapshots: BTreeMap<String, ProviderProcessSnapshot>,
}

/// Daemon 重启恢复使用的文件系统进程表。
/// Filesystem-backed process table used by restart recovery.
#[derive(Debug)]
pub struct FileSystemProviderProcessTable {
    /// `.provider` 单快照文件目录。
    process_dir: PathBuf,
    /// Runtime writer ownership required for all mutations.
    writer: Option<DurableWriterGuard>,
}

impl Clone for FileSystemProviderProcessTable {
    fn clone(&self) -> Self {
        Self {
            process_dir: self.process_dir.clone(),
            writer: self.writer.clone(),
        }
    }
}

impl PartialEq for FileSystemProviderProcessTable {
    fn eq(&self, other: &Self) -> bool {
        self.process_dir == other.process_dir
    }
}

impl Eq for FileSystemProviderProcessTable {}

impl ProviderProcessSnapshot {
    #[allow(clippy::too_many_arguments)]
    /// 创建活动 provider 会话初始快照。
    /// 所有强类型 ID 由调用方提前校验；started/updated 使用同一时刻，审计链记录槽位、会话和进程。
    pub fn running(
        session_id: impl Into<String>,
        provider_process_id: impl Into<String>,
        request_id: RequestId,
        adapter_id: AdapterId,
        capability: CapabilityName,
        transport: impl Into<String>,
        manifest_digest: impl Into<String>,
        start_command: impl Into<String>,
        restart_policy: impl Into<String>,
    ) -> Self {
        let now = now_ms();
        let session_id = session_id.into();
        let provider_process_id = provider_process_id.into();
        let restart_policy = restart_policy.into();
        let restart_state = if restart_policy == "none" {
            "disabled"
        } else {
            "stable"
        };
        Self {
            audit: vec![
                "provider.supervisor.acquired".to_owned(),
                format!("provider.session:{session_id}"),
                format!("provider.process:{provider_process_id}"),
            ],
            session_id,
            provider_process_id,
            pid: None,
            process_start_token: None,
            process_group_id: None,
            job_id: None,
            request_id,
            adapter_id,
            capability,
            transport: transport.into(),
            manifest_digest: manifest_digest.into(),
            start_command: start_command.into(),
            health: "running".to_owned(),
            restart_policy,
            retry_backoff_ms: None,
            attempt: 0,
            restart_max_attempts: 0,
            restart_backoff_ms: 0,
            restart_attempts: 0,
            restart_due_at_ms: None,
            restart_state: restart_state.to_owned(),
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            active: true,
            last_error: None,
            started_at_ms: now,
            updated_at_ms: now,
        }
    }

    /// Attaches a real OS process identity supplied by a later process backend.
    pub fn set_process_identity(
        &mut self,
        pid: u32,
        process_start_token: impl Into<String>,
        process_group_id: Option<u32>,
        job_id: Option<String>,
        attempt: u32,
    ) -> Result<(), EvaError> {
        let mut candidate = self.clone();
        candidate.pid = Some(pid);
        candidate.process_start_token = Some(process_start_token.into());
        candidate.process_group_id = process_group_id;
        candidate.job_id = job_id;
        candidate.attempt = attempt;
        candidate.validate_identity()?;
        *self = candidate;
        Ok(())
    }

    /// Returns whether this snapshot carries a real OS process identity.
    pub fn has_process_identity(&self) -> bool {
        self.pid.is_some()
    }

    /// Configure the immutable restart budget on a newly admitted session.
    pub fn configure_restart_budget(
        &mut self,
        max_attempts: u32,
        backoff_ms: u64,
    ) -> Result<(), EvaError> {
        let mut candidate = self.clone();
        candidate.restart_max_attempts = max_attempts;
        candidate.restart_backoff_ms = backoff_ms;
        candidate.restart_attempts = 0;
        candidate.restart_due_at_ms = None;
        candidate.restart_state = if max_attempts == 0 {
            "disabled".to_owned()
        } else {
            "running".to_owned()
        };
        candidate.validate_restart_state()?;
        *self = candidate;
        Ok(())
    }

    /// Returns the process incarnation number for the next registered spawn.
    pub fn next_process_attempt(&self) -> u32 {
        self.restart_attempts.saturating_add(1).max(1)
    }

    /// Move a due inactive restart into the pre-spawn state. The record stays
    /// inactive until a real OS identity is registered atomically.
    pub fn prepare_for_restart(&mut self, now_ms: u128) -> Result<(), EvaError> {
        if self.active || self.restart_state != "pending" {
            return Err(EvaError::conflict(
                "provider restart preparation requires an inactive pending session",
            )
            .with_context("session_id", &self.session_id)
            .with_context("restart_state", &self.restart_state));
        }
        let due_at_ms = self.restart_due_at_ms.ok_or_else(|| {
            EvaError::conflict("pending provider restart has no durable due time")
                .with_context("session_id", &self.session_id)
        })?;
        if now_ms < due_at_ms {
            return Err(
                EvaError::unavailable("provider restart backoff has not elapsed")
                    .with_retryable(true)
                    .with_context("session_id", &self.session_id)
                    .with_context("restart_due_at_ms", due_at_ms.to_string())
                    .with_context("retry_after_ms", (due_at_ms - now_ms).to_string()),
            );
        }
        self.pid = None;
        self.process_start_token = None;
        self.process_group_id = None;
        self.job_id = None;
        self.attempt = 0;
        self.restart_due_at_ms = None;
        self.restart_state = "starting".to_owned();
        self.health = "restart_starting".to_owned();
        self.updated_at_ms = now_ms;
        self.audit
            .push("provider.restart:spawn_requested".to_owned());
        self.validate_record(false)
    }

    /// Promote a registered restart process to the active running state.
    pub fn mark_restart_running(&mut self) -> Result<(), EvaError> {
        if self.restart_state != "starting" || !self.has_process_identity() {
            return Err(EvaError::conflict(
                "provider restart requires a starting record and real process identity",
            )
            .with_context("session_id", &self.session_id));
        }
        self.active = true;
        self.health = "running".to_owned();
        self.last_error = None;
        self.updated_at_ms = now_ms();
        self.restart_state = "running".to_owned();
        self.audit
            .push("provider.restart:process_registered".to_owned());
        self.validate_record(false)
    }

    /// Persist one consumed automatic restart and its deterministic due time.
    pub fn mark_restart_pending(
        &mut self,
        attempt: u32,
        due_at_ms: u128,
        reason: impl Into<String>,
    ) -> Result<(), EvaError> {
        if attempt == 0 || attempt <= self.restart_attempts || attempt > self.restart_max_attempts {
            return Err(
                EvaError::conflict("provider restart attempt is outside budget")
                    .with_context("attempt", attempt.to_string())
                    .with_context("max_attempts", self.restart_max_attempts.to_string()),
            );
        }
        self.restart_attempts = attempt;
        self.pid = None;
        self.process_start_token = None;
        self.process_group_id = None;
        self.job_id = None;
        self.attempt = 0;
        self.restart_due_at_ms = Some(due_at_ms);
        self.restart_state = "pending".to_owned();
        self.active = false;
        self.health = "restart_pending".to_owned();
        self.updated_at_ms = now_ms();
        self.audit
            .push(format!("provider.restart:attempt:{attempt}"));
        self.audit
            .push(format!("provider.restart:due_at_ms:{due_at_ms}"));
        self.audit.push(format!(
            "provider.restart:pending_reason:{}",
            sanitize_audit_value(&reason.into())
        ));
        self.validate_record(false)
    }

    /// Recover a daemon crash that happened after restart budget commit but
    /// before the new process identity was registered.
    pub fn recover_starting_restart(&mut self, now_ms: u128) -> Result<(), EvaError> {
        if self.active || self.restart_state != "starting" {
            return Err(EvaError::conflict(
                "starting provider restart is not recoverable from its current state",
            )
            .with_context("session_id", &self.session_id));
        }
        self.pid = None;
        self.process_start_token = None;
        self.process_group_id = None;
        self.job_id = None;
        self.attempt = 0;
        self.restart_due_at_ms = Some(now_ms);
        self.restart_state = "pending".to_owned();
        self.health = "restart_pending".to_owned();
        self.updated_at_ms = now_ms;
        self.audit
            .push("provider.restart:starting_recovered".to_owned());
        self.validate_record(false)
    }

    /// Persist terminal budget exhaustion after a failed attempt.
    pub fn mark_restart_exhausted(&mut self, reason: impl Into<String>) -> Result<(), EvaError> {
        self.restart_state = "exhausted".to_owned();
        self.restart_due_at_ms = None;
        self.pid = None;
        self.process_start_token = None;
        self.process_group_id = None;
        self.job_id = None;
        self.attempt = 0;
        self.active = false;
        self.health = "restart_exhausted".to_owned();
        self.updated_at_ms = now_ms();
        self.audit
            .push("provider.restart:budget_exhausted".to_owned());
        self.audit.push(format!(
            "provider.restart:exhausted_reason:{}",
            sanitize_audit_value(&reason.into())
        ));
        self.validate_record(false)
    }

    /// Persist a non-retryable terminal failure without pretending that the
    /// configured restart budget was consumed.
    pub fn mark_restart_failed(&mut self, reason: impl Into<String>) -> Result<(), EvaError> {
        self.restart_state = "failed".to_owned();
        self.restart_due_at_ms = None;
        self.pid = None;
        self.process_start_token = None;
        self.process_group_id = None;
        self.job_id = None;
        self.attempt = 0;
        self.active = false;
        self.health = "failed".to_owned();
        self.updated_at_ms = now_ms();
        self.audit.push("provider.restart:non_retryable".to_owned());
        self.audit.push(format!(
            "provider.restart:failure_reason:{}",
            sanitize_audit_value(&reason.into())
        ));
        self.validate_record(false)
    }

    /// Reset the crash-loop budget after a successful stable invocation.
    pub fn mark_stable_success(&mut self) -> Result<(), EvaError> {
        self.restart_attempts = 0;
        self.restart_due_at_ms = None;
        self.restart_state = if self.restart_max_attempts == 0 {
            "disabled".to_owned()
        } else {
            "stable".to_owned()
        };
        self.audit.push("provider.restart:stable_reset".to_owned());
        self.validate_record(false)
    }

    fn validate_restart_state(&self) -> Result<(), EvaError> {
        if !matches!(
            self.restart_state.as_str(),
            "disabled"
                | "unconfigured"
                | "stable"
                | "running"
                | "pending"
                | "starting"
                | "exhausted"
                | "failed"
        ) {
            return Err(
                EvaError::invalid_argument("provider restart state is invalid")
                    .with_context("restart_state", &self.restart_state),
            );
        }
        if self.restart_attempts > self.restart_max_attempts {
            return Err(EvaError::conflict(
                "provider restart attempts exceed configured budget",
            ));
        }
        if self.restart_state == "pending" && self.restart_due_at_ms.is_none() {
            return Err(EvaError::invalid_argument(
                "pending provider restart requires a due timestamp",
            ));
        }
        if self.restart_state != "pending" && self.restart_due_at_ms.is_some() {
            return Err(EvaError::invalid_argument(
                "non-pending provider restart cannot carry a due timestamp",
            ));
        }
        if self.restart_max_attempts == 0 && self.restart_backoff_ms != 0 {
            return Err(EvaError::invalid_argument(
                "disabled provider restart requires zero backoff",
            ));
        }
        if self.restart_max_attempts > 0 && self.restart_backoff_ms == 0 {
            return Err(EvaError::invalid_argument(
                "automatic provider restart requires positive backoff",
            ));
        }
        Ok(())
    }

    fn validate_identity(&self) -> Result<(), EvaError> {
        if let Some(pid) = self.pid {
            if pid == 0 || self.attempt == 0 {
                return Err(EvaError::invalid_argument(
                    "provider process identity requires positive pid and attempt",
                ));
            }
            let token = self.process_start_token.as_deref().ok_or_else(|| {
                EvaError::invalid_argument("provider process identity requires start token")
            })?;
            if token.trim().is_empty() || token.len() > 256 || token.chars().any(char::is_control) {
                return Err(EvaError::invalid_argument(
                    "provider process start token is empty, oversized, or contains controls",
                ));
            }
            if self.process_group_id.is_none() && self.job_id.is_none() {
                return Err(EvaError::invalid_argument(
                    "provider process identity requires a process group or Windows job id",
                ));
            }
        } else if self.process_start_token.is_some()
            || self.process_group_id.is_some()
            || self.job_id.is_some()
            || self.attempt != 0
        {
            return Err(EvaError::invalid_argument(
                "provider process identity fields require a pid",
            ));
        }
        if self.process_group_id.is_some() && self.job_id.is_some() {
            return Err(EvaError::invalid_argument(
                "provider process group and Windows job identities are mutually exclusive",
            ));
        }
        if self.process_group_id.is_some_and(|group| group == 0) {
            return Err(EvaError::invalid_argument(
                "provider process group id must be positive",
            ));
        }
        if self.job_id.as_deref().is_some_and(|job| {
            job.trim().is_empty() || job.len() > 256 || job.chars().any(char::is_control)
        }) {
            return Err(EvaError::invalid_argument(
                "provider Windows job id is empty, oversized, or contains controls",
            ));
        }
        Ok(())
    }

    fn validate_record(&self, persisted: bool) -> Result<(), EvaError> {
        if self.session_id.trim().is_empty() || self.provider_process_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process identity fields cannot be empty",
            ));
        }
        if self.transport.trim().is_empty() || self.health.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process transport and health cannot be empty",
            ));
        }
        self.validate_identity()?;
        self.validate_restart_state()?;
        if persisted && self.record_version == StateVersion::ZERO {
            return Err(EvaError::conflict(
                "persisted provider process record must have a positive version",
            ));
        }
        Ok(())
    }

    /// 释放 provider 槽位并转换为非活动终态。
    /// health 不允许空值；有 last_error 记为 supervisor.failed，否则记为 completed。
    pub fn release(
        &mut self,
        health: impl Into<String>,
        last_error: Option<String>,
    ) -> Result<(), EvaError> {
        let health = health.into();
        if health.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process health cannot be empty",
            ));
        }
        self.active = false;
        self.health = health.clone();
        self.last_error = last_error;
        self.updated_at_ms = now_ms();
        self.audit.push("provider.slot:released".to_owned());
        self.audit.push(format!("provider.health:{health}"));
        if self.last_error.is_some() {
            self.audit.push("provider.supervisor.failed".to_owned());
        } else {
            self.audit.push("provider.supervisor.completed".to_owned());
        }
        Ok(())
    }

    /// Daemon 重启恢复时把遗留活动会话标为 interrupted。
    /// 已有 last_error 代表更接近根因的 provider 失败，必须保留；restart reason 仅追加到去换行审计链。
    pub fn mark_interrupted_after_restart(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        let previous_health = self.health.clone();
        self.active = false;
        self.health = "interrupted".to_owned();
        if self.last_error.is_none() {
            self.last_error = Some(reason.clone());
        } else {
            self.audit
                .push("provider.recovery:last_error_preserved".to_owned());
        }
        self.updated_at_ms = now_ms();
        self.audit.push("provider.recovery:restart_scan".to_owned());
        self.audit.push(format!(
            "provider.recovery:previous_health:{previous_health}"
        ));
        self.audit.push(format!(
            "provider.recovery:interrupted_reason:{}",
            sanitize_audit_value(&reason)
        ));
        self.audit.push("provider.health:interrupted".to_owned());
    }

    /// 序列化为 version=3 逐行格式；可选值为空串，重复 audit 行保留顺序。
    pub fn to_storage(&self) -> String {
        let mut lines = vec![
            "version=3".to_owned(),
            format!("record_version={}", self.record_version.0),
            format!("owner_generation={}", self.owner_generation.0),
            format!("attempt={}", self.attempt),
            format!("restart_max_attempts={}", self.restart_max_attempts),
            format!("restart_backoff_ms={}", self.restart_backoff_ms),
            format!("restart_attempts={}", self.restart_attempts),
            format!(
                "restart_due_at_ms={}",
                self.restart_due_at_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!("restart_state={}", encode_field(&self.restart_state)),
            format!("session_id={}", encode_field(&self.session_id)),
            format!(
                "provider_process_id={}",
                encode_field(&self.provider_process_id)
            ),
            format!(
                "pid={}",
                self.pid.map(|value| value.to_string()).unwrap_or_default()
            ),
            format!(
                "process_start_token={}",
                self.process_start_token
                    .as_deref()
                    .map(encode_field)
                    .unwrap_or_default()
            ),
            format!(
                "process_group_id={}",
                self.process_group_id
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "job_id={}",
                self.job_id.as_deref().map(encode_field).unwrap_or_default()
            ),
            format!("request_id={}", encode_field(self.request_id.as_str())),
            format!("adapter_id={}", encode_field(self.adapter_id.as_str())),
            format!("capability={}", encode_field(self.capability.as_str())),
            format!("transport={}", encode_field(&self.transport)),
            format!("manifest_digest={}", encode_field(&self.manifest_digest)),
            format!("start_command={}", encode_field(&self.start_command)),
            format!("health={}", encode_field(&self.health)),
            format!("restart_policy={}", encode_field(&self.restart_policy)),
            format!(
                "retry_backoff_ms={}",
                self.retry_backoff_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!("active={}", self.active),
            format!(
                "last_error={}",
                self.last_error
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!("started_at_ms={}", self.started_at_ms),
            format!("updated_at_ms={}", self.updated_at_ms),
        ];
        lines.extend(
            self.audit
                .iter()
                .map(|entry| format!("audit={}", encode_field(entry))),
        );
        lines.push(String::new());
        lines.join("\n")
    }

    /// 严格解析 version=1/2/3 快照并重新验证 RequestId、AdapterId 和 CapabilityName。
    /// 旧版本缺少的 restart controller 字段按未配置状态兼容读取；首次 CAS 会升级它。
    pub fn from_storage(data: &str) -> Result<Self, EvaError> {
        use std::collections::BTreeSet;

        let mut version = None;
        let mut record_version = None;
        let mut owner_generation = None;
        let mut attempt = None;
        let mut restart_max_attempts = None;
        let mut restart_backoff_ms = None;
        let mut restart_attempts = None;
        let mut restart_due_at_ms = None;
        let mut restart_state = None;
        let mut session_id = None;
        let mut provider_process_id = None;
        let mut pid = None;
        let mut process_start_token = None;
        let mut process_group_id = None;
        let mut job_id = None;
        let mut request_id = None;
        let mut adapter_id = None;
        let mut capability = None;
        let mut transport = None;
        let mut manifest_digest = None;
        let mut start_command = None;
        let mut health = None;
        let mut restart_policy = None;
        let mut retry_backoff_ms = None;
        let mut active = None;
        let mut last_error = None;
        let mut started_at_ms = None;
        let mut updated_at_ms = None;
        let mut audit = Vec::new();
        let mut seen_scalars = BTreeSet::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::invalid_argument(
                    "provider process snapshot is invalid",
                ));
            };
            if key != "audit" && !seen_scalars.insert(key.to_owned()) {
                return Err(EvaError::invalid_argument(
                    "provider process snapshot contains a duplicate scalar field",
                )
                .with_context("field", key));
            }
            match key {
                "version" => {
                    version = Some(value.parse::<u32>().map_err(|_| {
                        EvaError::invalid_argument("provider process version is invalid")
                    })?);
                }
                "record_version" => {
                    record_version = Some(value.parse::<u64>().map_err(|_| {
                        EvaError::invalid_argument("provider process record_version is invalid")
                    })?);
                }
                "owner_generation" => {
                    owner_generation = Some(value.parse::<u64>().map_err(|_| {
                        EvaError::invalid_argument("provider process owner_generation is invalid")
                    })?);
                }
                "attempt" => {
                    attempt = Some(value.parse::<u32>().map_err(|_| {
                        EvaError::invalid_argument("provider process attempt is invalid")
                    })?);
                }
                "restart_max_attempts" => {
                    restart_max_attempts = Some(value.parse::<u32>().map_err(|_| {
                        EvaError::invalid_argument(
                            "provider process restart_max_attempts is invalid",
                        )
                    })?);
                }
                "restart_backoff_ms" => {
                    restart_backoff_ms = Some(value.parse::<u64>().map_err(|_| {
                        EvaError::invalid_argument("provider process restart_backoff_ms is invalid")
                    })?);
                }
                "restart_attempts" => {
                    restart_attempts = Some(value.parse::<u32>().map_err(|_| {
                        EvaError::invalid_argument("provider process restart_attempts is invalid")
                    })?);
                }
                "restart_due_at_ms" => {
                    restart_due_at_ms =
                        parse_optional_u128(value, "provider process restart_due_at_ms is invalid")?
                }
                "restart_state" => restart_state = Some(decode_field(value)),
                "session_id" => session_id = Some(decode_field(value)),
                "provider_process_id" => provider_process_id = Some(decode_field(value)),
                "pid" => pid = parse_optional_u32(value, "provider process pid is invalid")?,
                "process_start_token" => process_start_token = decode_optional_field(value),
                "process_group_id" => {
                    process_group_id =
                        parse_optional_u32(value, "provider process group id is invalid")?
                }
                "job_id" => job_id = decode_optional_field(value),
                "request_id" => request_id = Some(RequestId::parse(&decode_field(value))?),
                "adapter_id" => adapter_id = Some(AdapterId::parse(&decode_field(value))?),
                "capability" => capability = Some(CapabilityName::parse(&decode_field(value))?),
                "transport" => transport = Some(decode_field(value)),
                "manifest_digest" => manifest_digest = Some(decode_field(value)),
                "start_command" => start_command = Some(decode_field(value)),
                "health" => health = Some(decode_field(value)),
                "restart_policy" => restart_policy = Some(decode_field(value)),
                "retry_backoff_ms" => {
                    retry_backoff_ms =
                        parse_optional_u64(value, "provider process retry_backoff_ms is invalid")?
                }
                "active" => active = Some(parse_bool(value, "active")?),
                "last_error" => last_error = decode_optional_field(value),
                "started_at_ms" => {
                    started_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::invalid_argument("provider process started_at_ms is invalid")
                    })?)
                }
                "updated_at_ms" => {
                    updated_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::invalid_argument("provider process updated_at_ms is invalid")
                    })?)
                }
                "audit" => audit.push(decode_field(value)),
                _ => {
                    return Err(EvaError::invalid_argument(
                        "provider process snapshot has unknown field",
                    )
                    .with_context("field", key));
                }
            }
        }

        let version = version.ok_or_else(|| {
            EvaError::invalid_argument("provider process snapshot is missing version")
        })?;
        if !matches!(version, 1..=3) {
            return Err(
                EvaError::invalid_argument("provider process version mismatch")
                    .with_context("version", version.to_string()),
            );
        }
        if version >= 2
            && (!seen_scalars.contains("record_version")
                || !seen_scalars.contains("owner_generation")
                || !seen_scalars.contains("attempt")
                || !seen_scalars.contains("pid")
                || !seen_scalars.contains("process_start_token")
                || !seen_scalars.contains("process_group_id")
                || !seen_scalars.contains("job_id"))
        {
            return Err(EvaError::invalid_argument(
                "provider process v2 snapshot is missing identity or CAS fields",
            ));
        }
        if version == 3
            && (!seen_scalars.contains("restart_max_attempts")
                || !seen_scalars.contains("restart_backoff_ms")
                || !seen_scalars.contains("restart_attempts")
                || !seen_scalars.contains("restart_due_at_ms")
                || !seen_scalars.contains("restart_state"))
        {
            return Err(EvaError::invalid_argument(
                "provider process v3 snapshot is missing restart controller fields",
            ));
        }
        if version >= 2 && owner_generation.unwrap_or(0) == 0 {
            return Err(EvaError::conflict(
                "provider process v2 snapshot requires a positive owner generation",
            ));
        }
        let snapshot = Self {
            record_version: StateVersion(record_version.unwrap_or(0)),
            owner_generation: WriterGeneration(owner_generation.unwrap_or(0)),
            attempt: attempt.unwrap_or(0),
            restart_max_attempts: restart_max_attempts.unwrap_or(0),
            restart_backoff_ms: restart_backoff_ms.unwrap_or(0),
            restart_attempts: restart_attempts.unwrap_or(0),
            restart_due_at_ms,
            restart_state: restart_state.unwrap_or_else(|| {
                if version < 3 {
                    "unconfigured".to_owned()
                } else {
                    String::new()
                }
            }),
            session_id: session_id
                .ok_or_else(|| EvaError::invalid_argument("provider process missing session_id"))?,
            provider_process_id: provider_process_id.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing provider_process_id")
            })?,
            pid,
            process_start_token,
            process_group_id,
            job_id,
            request_id: request_id
                .ok_or_else(|| EvaError::invalid_argument("provider process missing request_id"))?,
            adapter_id: adapter_id
                .ok_or_else(|| EvaError::invalid_argument("provider process missing adapter_id"))?,
            capability: capability
                .ok_or_else(|| EvaError::invalid_argument("provider process missing capability"))?,
            transport: transport
                .ok_or_else(|| EvaError::invalid_argument("provider process missing transport"))?,
            manifest_digest: manifest_digest.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing manifest_digest")
            })?,
            start_command: start_command.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing start_command")
            })?,
            health: health
                .ok_or_else(|| EvaError::invalid_argument("provider process missing health"))?,
            restart_policy: restart_policy.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing restart_policy")
            })?,
            retry_backoff_ms,
            active: active
                .ok_or_else(|| EvaError::invalid_argument("provider process missing active"))?,
            last_error,
            started_at_ms: started_at_ms.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing started_at_ms")
            })?,
            updated_at_ms: updated_at_ms.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing updated_at_ms")
            })?,
            audit,
        };
        snapshot.validate_record(version >= 2)?;
        Ok(snapshot)
    }
}

impl InMemoryProviderProcessTable {
    /// 创建空内存会话表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回指定 Adapter 当前 active 的快照，保持 BTreeMap 的确定顺序。
    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|snapshot| snapshot.active && &snapshot.adapter_id == adapter_id)
            .collect())
    }
}

impl FileSystemProviderProcessTable {
    /// 使用传统项目布局 `<root>/.eva/provider-processes` 创建表句柄。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            process_dir: root.as_ref().join(".eva").join("provider-processes"),
            writer: None,
        }
    }

    /// 使用 durable backend state 子树创建只读表句柄；所有 mutation 都会 fail closed。
    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self {
            process_dir: layout.state_dir.join("provider-processes"),
            writer: None,
        }
    }

    /// Creates a writable table fenced by the supplied durable runtime writer.
    pub fn from_runtime_writer(
        layout: &DurableBackendLayout,
        writer: DurableWriterGuard,
    ) -> Result<Self, EvaError> {
        if writer.root() != layout.root {
            return Err(EvaError::conflict(
                "provider process writer belongs to a different durable backend",
            )
            .with_context("layout_root", layout.root.display().to_string())
            .with_context("writer_root", writer.root().display().to_string()));
        }
        writer.verify_current()?;
        Ok(Self {
            process_dir: layout.state_dir.join("provider-processes"),
            writer: Some(writer),
        })
    }

    /// 返回 provider 快照目录。
    pub fn process_dir(&self) -> &Path {
        &self.process_dir
    }

    /// 将 session ID 映射为 collision-free `.provider` 文件名。
    fn snapshot_path(&self, session_id: &str) -> Result<PathBuf, EvaError> {
        if session_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process session id cannot be empty",
            ));
        }
        let digest = Sha256::digest(session_id.as_bytes());
        Ok(self
            .process_dir
            .join(format!("{}.provider", bytes_to_hex(digest.as_slice()))))
    }

    fn legacy_snapshot_path(&self, session_id: &str) -> Result<PathBuf, EvaError> {
        if session_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process session id cannot be empty",
            ));
        }
        Ok(self
            .process_dir
            .join(format!("{}.provider", legacy_safe_file_segment(session_id))))
    }

    fn record_lock_path(&self, session_id: &str) -> Result<PathBuf, EvaError> {
        Ok(self
            .snapshot_path(session_id)?
            .with_extension("provider.lock"))
    }
}

fn read_provider_snapshot(path: &Path) -> Result<Option<ProviderProcessSnapshot>, EvaError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                EvaError::internal("failed to read provider process snapshot")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string()),
            )
        }
    };
    ProviderProcessSnapshot::from_storage(&data)
        .map(Some)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

fn ensure_immutable_identity(
    current: &ProviderProcessSnapshot,
    candidate: &ProviderProcessSnapshot,
) -> Result<(), EvaError> {
    let same = current.provider_process_id == candidate.provider_process_id
        && current.request_id == candidate.request_id
        && current.adapter_id == candidate.adapter_id
        && current.capability == candidate.capability
        && current.transport == candidate.transport
        && current.manifest_digest == candidate.manifest_digest
        && current.start_command == candidate.start_command
        && current.restart_policy == candidate.restart_policy
        && current.retry_backoff_ms == candidate.retry_backoff_ms
        && current.restart_max_attempts == candidate.restart_max_attempts
        && current.restart_backoff_ms == candidate.restart_backoff_ms
        && current.started_at_ms == candidate.started_at_ms;
    if same {
        return Ok(());
    }
    Err(
        EvaError::conflict("provider process immutable identity changed")
            .with_context("session_id", &candidate.session_id),
    )
}

impl ProviderProcessTable for InMemoryProviderProcessTable {
    /// 按 session ID 执行进程内 CAS，并返回递增版本的权威快照。
    fn compare_and_set(
        &mut self,
        candidate: ProviderProcessSnapshot,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        candidate.validate_record(false)?;
        if let Some(current) = self.snapshots.get(&candidate.session_id) {
            ensure_immutable_identity(current, &candidate)?;
        }
        let actual = self
            .snapshots
            .get(&candidate.session_id)
            .map(|snapshot| snapshot.record_version)
            .unwrap_or(StateVersion::ZERO);
        if actual != candidate.record_version {
            return Err(
                EvaError::conflict("provider process record version conflict")
                    .with_context("session_id", &candidate.session_id)
                    .with_context("expected", candidate.record_version.0.to_string())
                    .with_context("actual", actual.0.to_string()),
            );
        }
        let mut committed = candidate;
        committed.record_version = actual.checked_next()?;
        committed.validate_record(true)?;
        self.snapshots
            .insert(committed.session_id.clone(), committed.clone());
        Ok(committed)
    }

    /// 克隆读取指定会话，缺失返回带 session ID 的 NotFound。
    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        self.snapshots.get(session_id).cloned().ok_or_else(|| {
            EvaError::not_found("provider process session does not exist")
                .with_context("session_id", session_id)
        })
    }

    /// 按 session ID 排序返回全部快照。
    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        Ok(self.snapshots.values().cloned().collect())
    }
}

impl ProviderProcessTable for FileSystemProviderProcessTable {
    /// 在 durable writer/process/record 三重边界内执行 provider 快照 CAS。
    fn compare_and_set(
        &mut self,
        candidate: ProviderProcessSnapshot,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        candidate.validate_record(false)?;
        let writer = self.writer.clone().ok_or_else(|| {
            EvaError::conflict("provider process mutation requires runtime writer ownership")
                .with_context("path", self.process_dir.display().to_string())
        })?;
        fs::create_dir_all(&self.process_dir).map_err(|error| {
            EvaError::internal("failed to create provider process directory")
                .with_context("path", self.process_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = self.snapshot_path(&candidate.session_id)?;
        let legacy_path = self.legacy_snapshot_path(&candidate.session_id)?;
        let lock_path = self.record_lock_path(&candidate.session_id)?;
        writer.with_write_lock(|generation| {
            let _record_lock = acquire_record_write_lock(&lock_path)?;
            writer.verify_current()?;
            let current_path = if path.exists() {
                path.clone()
            } else if legacy_path.exists() {
                legacy_path.clone()
            } else {
                path.clone()
            };
            let current = read_provider_snapshot(&current_path)?;
            let actual = current
                .as_ref()
                .map(|snapshot| snapshot.record_version)
                .unwrap_or(StateVersion::ZERO);
            if let Some(current) = &current {
                if current.session_id != candidate.session_id {
                    return Err(
                        EvaError::conflict("provider process snapshot key collision")
                            .with_context("path", current_path.display().to_string())
                            .with_context("expected_session_id", &candidate.session_id)
                            .with_context("actual_session_id", &current.session_id),
                    );
                }
                if candidate.owner_generation != current.owner_generation {
                    return Err(
                        EvaError::conflict("provider process owner generation conflict")
                            .with_context("session_id", &candidate.session_id)
                            .with_context(
                                "expected_generation",
                                candidate.owner_generation.0.to_string(),
                            )
                            .with_context(
                                "actual_generation",
                                current.owner_generation.0.to_string(),
                            ),
                    );
                }
                ensure_immutable_identity(current, &candidate)?;
            } else if candidate.owner_generation != WriterGeneration::ZERO {
                return Err(EvaError::conflict(
                    "new provider process record cannot claim an existing owner generation",
                )
                .with_context("session_id", &candidate.session_id)
                .with_context("generation", candidate.owner_generation.0.to_string()));
            }
            if actual != candidate.record_version {
                return Err(
                    EvaError::conflict("provider process record version conflict")
                        .with_context("session_id", &candidate.session_id)
                        .with_context("expected", candidate.record_version.0.to_string())
                        .with_context("actual", actual.0.to_string()),
                );
            }
            let mut committed = candidate.clone();
            committed.record_version = actual.checked_next()?;
            committed.owner_generation = generation;
            committed.validate_record(true)?;
            atomic_write(&path, committed.to_storage().as_bytes()).map_err(|error| {
                EvaError::internal("failed to atomically write provider process snapshot")
                    .with_context("session_id", &committed.session_id)
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            if legacy_path != path && legacy_path.exists() {
                fs::remove_file(&legacy_path).map_err(|error| {
                    EvaError::internal("failed to remove migrated provider process snapshot")
                        .with_context("path", legacy_path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
            }
            Ok(committed)
        })
    }

    fn writer_generation(&self) -> Option<WriterGeneration> {
        self.writer.as_ref().map(DurableWriterGuard::generation)
    }

    /// 读取并严格解析指定 session 文件；任何 I/O 缺失映射为 NotFound，格式错误附加路径。
    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        let path = self.snapshot_path(session_id)?;
        let legacy_path = self.legacy_snapshot_path(session_id)?;
        let read_path = if path.exists() { path } else { legacy_path };
        let snapshot = read_provider_snapshot(&read_path)?.ok_or_else(|| {
            EvaError::not_found("provider process session does not exist")
                .with_context("path", read_path.display().to_string())
        })?;
        if snapshot.session_id != session_id {
            return Err(
                EvaError::conflict("provider process snapshot key collision")
                    .with_context("path", read_path.display().to_string())
                    .with_context("expected_session_id", session_id)
                    .with_context("actual_session_id", snapshot.session_id),
            );
        }
        Ok(snapshot)
    }

    /// 按文件路径排序加载所有 `.provider` 快照。
    /// 目录缺失表示空表；任一目标文件损坏使整个列表失败，避免恢复遗漏活跃进程。
    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        let entries = match fs::read_dir(&self.process_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(
                    EvaError::internal("failed to read provider process directory")
                        .with_context("path", self.process_dir.display().to_string())
                        .with_context("io_error", error.to_string()),
                );
            }
        };

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                EvaError::internal("failed to read provider process directory entry")
                    .with_context("path", self.process_dir.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("provider") {
                paths.push(path);
            }
        }
        paths.sort();

        let mut snapshots: BTreeMap<String, (bool, PathBuf, ProviderProcessSnapshot)> =
            BTreeMap::new();
        for path in paths {
            let snapshot = read_provider_snapshot(&path)?.ok_or_else(|| {
                EvaError::internal("provider process snapshot disappeared while listing")
                    .with_context("path", path.display().to_string())
            })?;
            let canonical_path = self.snapshot_path(&snapshot.session_id)?;
            let is_canonical = path == canonical_path;
            if let Some((existing_canonical, existing_path, existing)) =
                snapshots.get(&snapshot.session_id)
            {
                if *existing_canonical && !is_canonical {
                    // A crash after the hashed rename but before legacy cleanup
                    // leaves both files. The hashed record is authoritative.
                    continue;
                }
                if !*existing_canonical && is_canonical {
                    snapshots.insert(snapshot.session_id.clone(), (true, path, snapshot));
                    continue;
                }
                return Err(EvaError::conflict(
                    "duplicate provider process snapshots share a session id",
                )
                .with_context("session_id", &snapshot.session_id)
                .with_context("first_path", existing_path.display().to_string())
                .with_context("second_path", path.display().to_string())
                .with_context("first_version", existing.record_version.0.to_string())
                .with_context("second_version", snapshot.record_version.0.to_string()));
            }
            snapshots.insert(snapshot.session_id.clone(), (is_canonical, path, snapshot));
        }

        let mut values: Vec<_> = snapshots
            .into_values()
            .map(|(_, path, snapshot)| (path, snapshot))
            .collect();
        values.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(values.into_iter().map(|(_, snapshot)| snapshot).collect())
    }
}

/// 返回当前 epoch 毫秒；系统时钟早于 epoch 时回退 0。
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 严格解析小写布尔磁盘字段，并附加字段和值上下文。
fn parse_bool(value: &str, field: &'static str) -> Result<bool, EvaError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(
            EvaError::invalid_argument("provider process boolean field is invalid")
                .with_context("field", field)
                .with_context("value", value),
        ),
    }
}

/// 将空串解析为 None，否则严格解析 u64 退避值。
fn parse_optional_u64(value: &str, message: &'static str) -> Result<Option<u64>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| EvaError::invalid_argument(message).with_context("value", value))
    }
}

fn parse_optional_u128(value: &str, message: &'static str) -> Result<Option<u128>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u128>()
            .map(Some)
            .map_err(|_| EvaError::invalid_argument(message).with_context("value", value))
    }
}

fn parse_optional_u32(value: &str, message: &'static str) -> Result<Option<u32>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u32>()
            .map(Some)
            .map_err(|_| EvaError::invalid_argument(message).with_context("value", value))
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Reproduces the pre-CAS v1 filename mapping for backward-compatible reads.
fn legacy_safe_file_segment(value: &str) -> String {
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

/// 将磁盘空串恢复为 None，非空值百分号解码。
fn decode_optional_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(decode_field(value))
    }
}

/// 百分号编码逐行格式中的换行、分隔符和 `%` 本身。
fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
        .replace('|', "%7C")
        .replace('=', "%3D")
}

/// 以固定逆序恢复编码字符，最后处理 `%25` 避免二次解码。
fn decode_field(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%09", "\t")
        .replace("%7C", "|")
        .replace("%3D", "=")
        .replace("%25", "%")
}

/// 将审计原因中的 CR/LF 替换为空格，保证一条原因不会伪造多条日志。
fn sanitize_audit_value(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

#[cfg(test)]
/// Provider 会话 upsert、释放、文件重开、重启中断和损坏快照回归测试。
mod tests {
    use super::*;
    use crate::{DurableBackendOptions, FileSystemDurableBackend};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 创建 running provider 快照 fixture。
    fn snapshot(session: &str) -> ProviderProcessSnapshot {
        ProviderProcessSnapshot::running(
            session,
            format!("proc-{session}"),
            RequestId::parse("req-provider-table").unwrap(),
            AdapterId::parse("stdio-test").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "fnv64:0123456789abcdef",
            "stdio-test --run",
            "none",
        )
    }

    #[test]
    /// 验证内存表 upsert/list 及按 Adapter active 查询。
    fn process_table_upserts_and_lists_active_sessions() {
        let mut table = InMemoryProviderProcessTable::new();
        let adapter_id = AdapterId::parse("stdio-test").unwrap();

        table.upsert(snapshot("session-1")).unwrap();

        let sessions = table.list().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(table.active_for_adapter(&adapter_id).unwrap().len(), 1);
    }

    #[test]
    /// 验证 release 保存 last_error 并追加失败审计链。
    fn process_table_release_records_last_error() {
        let mut table = InMemoryProviderProcessTable::new();
        let mut snapshot = snapshot("session-2");
        snapshot
            .release("failed", Some("provider exited before ready".to_owned()))
            .unwrap();

        table.upsert(snapshot).unwrap();
        let stored = table.read("session-2").unwrap();

        assert!(!stored.active);
        assert_eq!(stored.health, "failed");
        assert_eq!(
            stored.last_error.as_deref(),
            Some("provider exited before ready")
        );
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.slot:released"));
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.supervisor.failed"));
    }

    #[test]
    /// 验证文件快照重开后保持全部字段与 retry backoff。
    fn filesystem_process_table_survives_reopen() {
        let root = test_root("filesystem-round-trip");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut writer = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            backend.acquire_runtime_writer().unwrap(),
        )
        .unwrap();
        let mut stored = snapshot("session-filesystem-1");
        stored.retry_backoff_ms = Some(1500);

        let committed = writer.compare_and_set(stored).unwrap();
        let reader = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let by_id = reader.read("session-filesystem-1").unwrap();
        let listed = reader.list().unwrap();

        assert_eq!(by_id, committed);
        assert_eq!(listed, vec![committed.clone()]);
        assert!(reader
            .snapshot_path("session-filesystem-1")
            .unwrap()
            .is_file());
    }

    #[test]
    /// 验证重启中断保留既有 provider 根因并追加 recovery 审计。
    fn interrupted_provider_process_preserves_last_error_and_audit_chain() {
        let mut stored = snapshot("session-interrupted");
        stored.last_error = Some("provider stderr: safe error".to_owned());
        let original_audit_len = stored.audit.len();

        stored.mark_interrupted_after_restart("daemon restart interrupted active provider session");

        assert!(!stored.active);
        assert_eq!(stored.health, "interrupted");
        assert_eq!(
            stored.last_error.as_deref(),
            Some("provider stderr: safe error")
        );
        assert!(stored.audit.len() > original_audit_len);
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.recovery:last_error_preserved"));
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.health:interrupted"));
    }

    #[test]
    /// 验证 list 遇到缺核心字段的快照整体失败。
    fn filesystem_process_table_reports_corrupt_snapshot() {
        let root = test_root("filesystem-corrupt");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let table = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        fs::create_dir_all(table.process_dir()).unwrap();
        fs::write(
            table.process_dir().join("corrupt.provider"),
            "version=1\nsession_id=session-corrupt\n",
        )
        .unwrap();

        let error = table.list().unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    #[test]
    fn process_identity_round_trips_for_unix_and_windows_shapes() {
        let mut unix = snapshot("identity-unix");
        unix.set_process_identity(4242, "unix-start-1", Some(4242), None, 1)
            .unwrap();
        unix.record_version = StateVersion(1);
        unix.owner_generation = WriterGeneration(7);
        let unix_reopened = ProviderProcessSnapshot::from_storage(&unix.to_storage()).unwrap();
        assert_eq!(unix_reopened.pid, Some(4242));
        assert_eq!(
            unix_reopened.process_start_token.as_deref(),
            Some("unix-start-1")
        );
        assert_eq!(unix_reopened.process_group_id, Some(4242));
        assert_eq!(unix_reopened.job_id, None);
        assert_eq!(unix_reopened.attempt, 1);
        assert_eq!(unix_reopened.record_version, StateVersion(1));
        assert_eq!(unix_reopened.owner_generation, WriterGeneration(7));

        let mut windows = snapshot("identity-windows");
        windows
            .set_process_identity(
                4343,
                "windows-start-1",
                None,
                Some("job-4343".to_owned()),
                2,
            )
            .unwrap();
        windows.record_version = StateVersion(3);
        windows.owner_generation = WriterGeneration(8);
        let windows_reopened =
            ProviderProcessSnapshot::from_storage(&windows.to_storage()).unwrap();
        assert_eq!(windows_reopened.pid, Some(4343));
        assert_eq!(
            windows_reopened.process_start_token.as_deref(),
            Some("windows-start-1")
        );
        assert_eq!(windows_reopened.process_group_id, None);
        assert_eq!(windows_reopened.job_id.as_deref(), Some("job-4343"));
        assert_eq!(windows_reopened.attempt, 2);
    }

    #[test]
    fn restart_budget_and_pending_due_time_survive_writer_generation_change() {
        let root = test_root("restart-budget-round-trip");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut writer = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            backend.acquire_runtime_writer().unwrap(),
        )
        .unwrap();
        let mut pending = snapshot("restart-budget");
        pending.restart_policy = "on_failure".to_owned();
        pending.configure_restart_budget(3, 25).unwrap();
        pending
            .mark_restart_pending(1, 9_999_999, "first crash")
            .unwrap();
        let committed = writer.compare_and_set(pending).unwrap();
        assert_eq!(committed.restart_attempts, 1);
        assert_eq!(committed.restart_state, "pending");
        assert_eq!(committed.restart_due_at_ms, Some(9_999_999));

        let reopened = FileSystemProviderProcessTable::from_durable_layout(backend.layout())
            .read("restart-budget")
            .unwrap();
        assert_eq!(reopened.restart_max_attempts, 3);
        assert_eq!(reopened.restart_backoff_ms, 25);
        assert_eq!(reopened.restart_attempts, 1);
        assert_eq!(reopened.restart_state, "pending");

        drop(writer);
        let mut next_writer = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            backend.acquire_runtime_writer().unwrap(),
        )
        .unwrap();
        let mut stable = reopened.clone();
        stable.active = false;
        stable.mark_stable_success().unwrap();
        let reset = next_writer.compare_and_set(stable).unwrap();
        assert_eq!(reset.restart_attempts, 0);
        assert_eq!(reset.restart_state, "stable");
        assert_eq!(reset.restart_due_at_ms, None);
    }

    #[test]
    fn invalid_process_identity_does_not_partially_update_snapshot() {
        let mut snapshot = snapshot("identity-invalid");
        let original = snapshot.clone();

        assert!(snapshot
            .set_process_identity(0, "bad", Some(1), None, 1)
            .is_err());
        assert_eq!(snapshot, original);

        assert!(snapshot
            .set_process_identity(42, "bad", Some(1), Some("job".to_owned()), 1)
            .is_err());
        assert_eq!(snapshot, original);

        assert!(snapshot
            .set_process_identity(42, "bad", Some(1), None, 0)
            .is_err());
        assert_eq!(snapshot, original);
    }

    #[test]
    fn legacy_provider_record_is_readable_and_upgraded_once() {
        let root = test_root("legacy-upgrade");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut stored = snapshot("legacy-session");
        stored.retry_backoff_ms = Some(250);
        let legacy_bytes = legacy_storage(&stored);
        let legacy_table = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        fs::create_dir_all(legacy_table.process_dir()).unwrap();
        let legacy_path = legacy_table
            .legacy_snapshot_path(&stored.session_id)
            .unwrap();
        fs::write(&legacy_path, &legacy_bytes).unwrap();

        let legacy = legacy_table.read(&stored.session_id).unwrap();
        assert_eq!(legacy.record_version, StateVersion::ZERO);
        assert_eq!(legacy.owner_generation, WriterGeneration::ZERO);
        assert_eq!(legacy.attempt, 0);
        assert!(!legacy.has_process_identity());

        let mut writer = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            backend.acquire_runtime_writer().unwrap(),
        )
        .unwrap();
        let committed = writer.compare_and_set(legacy.clone()).unwrap();
        assert_eq!(committed.record_version, StateVersion(1));
        assert!(committed.owner_generation > WriterGeneration::ZERO);
        assert!(!legacy_path.exists());
        let hashed_path = writer.snapshot_path(&stored.session_id).unwrap();
        assert!(hashed_path.is_file());
        let bytes_after_upgrade = fs::read(&hashed_path).unwrap();

        // A stale v1 candidate cannot overwrite the upgraded record.
        let error = writer.compare_and_set(legacy).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(fs::read(&hashed_path).unwrap(), bytes_after_upgrade);

        // Simulate a crash between hashed rename and legacy cleanup: list keeps
        // the canonical hashed record and does not return a duplicate.
        fs::write(&legacy_path, &legacy_bytes).unwrap();
        let listed = FileSystemProviderProcessTable::from_durable_layout(backend.layout())
            .list()
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].record_version, StateVersion(1));
    }

    #[test]
    fn filesystem_process_table_fences_versions_owner_and_writer_root() {
        let root = test_root("cas-fences");
        let other_root = test_root("cas-fences-other");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let other_backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(other_root.path()))
                .unwrap();
        let writer_guard = backend.acquire_runtime_writer().unwrap();
        let wrong_root = FileSystemProviderProcessTable::from_runtime_writer(
            other_backend.layout(),
            writer_guard.clone(),
        )
        .unwrap_err();
        assert_eq!(wrong_root.kind(), eva_core::ErrorKind::Conflict);

        let mut first = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            writer_guard.clone(),
        )
        .unwrap();
        let mut second = first.clone();
        let candidate = snapshot("cas-session");
        let committed = first.compare_and_set(candidate.clone()).unwrap();
        assert_eq!(committed.record_version, StateVersion(1));
        let stale_version = second.compare_and_set(candidate).unwrap_err();
        assert_eq!(stale_version.kind(), eva_core::ErrorKind::Conflict);

        let mut stale_owner = committed.clone();
        stale_owner.owner_generation = WriterGeneration(999);
        let owner_error = first.compare_and_set(stale_owner).unwrap_err();
        assert_eq!(owner_error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(first.read("cas-session").unwrap(), committed);

        let readonly = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let readonly_error = readonly
            .clone()
            .compare_and_set(snapshot("readonly-session"))
            .unwrap_err();
        assert_eq!(readonly_error.kind(), eva_core::ErrorKind::Conflict);
    }

    fn legacy_storage(snapshot: &ProviderProcessSnapshot) -> String {
        let mut lines = vec![
            "version=1".to_owned(),
            format!("session_id={}", encode_field(&snapshot.session_id)),
            format!(
                "provider_process_id={}",
                encode_field(&snapshot.provider_process_id)
            ),
            format!("request_id={}", encode_field(snapshot.request_id.as_str())),
            format!("adapter_id={}", encode_field(snapshot.adapter_id.as_str())),
            format!("capability={}", encode_field(snapshot.capability.as_str())),
            format!("transport={}", encode_field(&snapshot.transport)),
            format!(
                "manifest_digest={}",
                encode_field(&snapshot.manifest_digest)
            ),
            format!("start_command={}", encode_field(&snapshot.start_command)),
            format!("health={}", encode_field(&snapshot.health)),
            format!("restart_policy={}", encode_field(&snapshot.restart_policy)),
            format!(
                "retry_backoff_ms={}",
                snapshot
                    .retry_backoff_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!("active={}", snapshot.active),
            format!(
                "last_error={}",
                snapshot
                    .last_error
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!("started_at_ms={}", snapshot.started_at_ms),
            format!("updated_at_ms={}", snapshot.updated_at_ms),
        ];
        lines.extend(
            snapshot
                .audit
                .iter()
                .map(|entry| format!("audit={}", encode_field(entry))),
        );
        lines.push(String::new());
        lines.join("\n")
    }

    /// 测试临时 durable root 所有者。
    struct TestRoot {
        /// 唯一临时路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 返回临时路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 测试结束时尽力递归清理。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 用测试名、进程和时间构造并行安全路径。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-storage-provider-process-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
