//! 实现本地前台守护进程的独占锁、状态文件、控制邮箱和周期调度边界。
//!
//! 启动时先取得原子文件锁，再恢复持久化任务和提供者状态，验证策略与可观测性后才发布
//! running 状态和 PID。控制面采用请求/响应文件邮箱：单一守护循环按文件名顺序消费请求，
//! 先执行状态变更，再原子发布响应，最后删除请求；调用方超时不代表请求未被稍后执行。
//! Local daemon process-boundary and control-plane contracts for V1.12.

use crate::config_generation_store::ConfigGenerationStore;
use crate::memory_worker::{
    ensure_retrieval_schedule, run_scheduled_retrieval, DaemonRetrievalWorker,
};
use crate::{preflight_config_reload, ConfigReloadPreflightOutcome, ConfigWatcher};
use crate::{
    run_scheduler_retry_tick_with_handler, FileSystemTaskArtifactResolver, IdempotencyKey,
    RuntimeBuilder, RuntimeRecoveryCoordinator, RuntimeRecoveryReport, SchedulerRetryTickOptions,
    SchedulerRetryTickReport, ShutdownReport, TaskArtifactRef, TaskAttemptPolicy, TaskEnvelope,
    TaskHandlerRegistry, TaskInput, TaskKind, TaskWorkerDrainOptions, TaskWorkerDrainReport,
    TaskWorkerRuntime,
};
use eva_config::ProjectConfig;
use eva_core::{AgentId, EvaError, GenerationId, RequestId};
use eva_eventbus::DurableEventBus;
use eva_hardware::{
    discover_project_devices, parse_hotplug_subscriber_state, render_hotplug_subscriber_state,
    run_hotplug_subscriber_once, HardwareHotplugDeviceState, HardwareHotplugSubscriberReport,
};
use eva_lifecycle::{
    DrainCoordinator, DrainPlan, GenerationController, GenerationState, RuntimeGeneration,
};
use eva_memory::{
    FileSystemKnowledgeStore, FileSystemMemoryStore, FileSystemScheduleStore,
    KnowledgeRebuildCheckpointReport, MemoryCompactionReport,
};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind,
    MetricLabels, MetricName, MetricPoint, MetricSink, ObservabilitySmokeReport,
    RuntimeObservabilityLifecycle, SpanId, TraceFields,
};
use eva_policy::PolicyDomainSet;
use eva_scheduler::GenerationRouteGate;
use eva_storage::{
    artifact_store::sha256_digest, atomic_write as atomic_storage_write, probe_runtime_lease,
    DurableBackend, DurableBackendLayout, DurableBackendOptions, DurableBackendReport,
    DurableRuntimeLeaseGuard, DurableRuntimeLeaseIdentity, DurableRuntimeLeaseRecord,
    DurableRuntimeLeaseState, DurableWriterGuard, FileSystemDurableBackend, FileSystemEffectLedger,
    FileSystemProviderProcessTable, FileSystemTaskStateStore, TaskInputSnapshot, TaskStateSnapshot,
    TaskStateStore, WriterGeneration, DEFAULT_RUNTIME_LEASE_TTL_MS,
};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "define the local daemon process and control boundary without starting providers";

/// 定义 `DAEMON_GENERATION` 常量。
const DAEMON_GENERATION: &str = "daemon-v1.12.4";
/// 永久保留且从不替换的 daemon OS-lock anchor 文件名。
const LOCK_FILE: &str = "daemon.lock";
/// 原子替换的 daemon lease 记录；固定 OS-lock anchor 与此记录必须使用不同路径。
const LEASE_FILE: &str = "daemon.lease";
/// 定义守护进程可用性探测所需的 PID 文件名。
const PID_FILE: &str = "daemon.pid";
/// PID projection 的版本化磁盘格式；legacy 纯数字只读但不证明 owner identity。
const PID_PROJECTION_FORMAT: &str = "eva.daemon-pid.v1";
/// 定义持久化守护生命周期状态的文件名。
const STATE_FILE: &str = "daemon.state";
/// Durable proof that one exact lease generation completed worker drain and residual scanning.
const SHUTDOWN_DRAIN_EVIDENCE_FILE: &str = "daemon.shutdown-drain";
const SHUTDOWN_DRAIN_EVIDENCE_FORMAT: &str = "eva.daemon-shutdown-drain.v1";
/// 定义 `AGENT_CONTROL_STATE_FILE` 常量。
const AGENT_CONTROL_STATE_FILE: &str = "agent-control.state";
/// 定义 `HARDWARE_HOTPLUG_STATE_FILE` 常量。
const HARDWARE_HOTPLUG_STATE_FILE: &str = "hardware-hotplug.state";
/// 定义客户端原子投递控制请求的邮箱目录。
const CONTROL_REQUEST_DIR: &str = "control/requests";
/// 定义守护循环原子发布控制响应的邮箱目录。
const CONTROL_RESPONSE_DIR: &str = "control/responses";
/// 定义 `CONTROL_REQUEST_EXT` 常量。
const CONTROL_REQUEST_EXT: &str = "request";
/// 无法解析或语义校验失败的请求会改名隔离，避免持续毒化 daemon loop。
const CONTROL_REJECTED_EXT: &str = "rejected";
/// 定义 `CONTROL_RESPONSE_EXT` 常量。
const CONTROL_RESPONSE_EXT: &str = "response";
/// 定义 `CONTROL_POLL_INTERVAL_MS` 常量。
const CONTROL_POLL_INTERVAL_MS: u64 = 50;
const MEMORY_MAINTENANCE_SCHEDULE_ID: &str = "memory-maintenance";
const MEMORY_MAINTENANCE_INTERVAL_MS: u128 = 60_000;
const MEMORY_MAINTENANCE_LEASE_MS: u128 = 30_000;
/// Lease heartbeat 的单调调度间隔；墙上时钟仅写入持久记录。
pub const DAEMON_LEASE_HEARTBEAT_INTERVAL_MS: u64 = 10_000;
/// A daemon lease remains live for two missed scheduler ticks before status
/// becomes degraded, while the storage TTL remains the stale cutoff.
pub const DAEMON_LEASE_DEGRADED_AFTER_MS: u128 = 20_000;
pub const DAEMON_LEASE_STALE_AFTER_MS: u128 = DEFAULT_RUNTIME_LEASE_TTL_MS;
/// Shutdown drain stays below the degraded lease window so the live owner remains observable.
pub const MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS: u64 = 15_000;
const STARTUP_DIR: &str = "startup";
const STARTUP_FRAME_FORMAT: &str = "eva.daemon-startup.v1";
const STARTUP_ABORT_FORMAT: &str = "eva.daemon-startup-abort.v1";
const STARTUP_REPORT_SUFFIX: &str = "report.json";

/// 定义守护进程各持久化边界及前台运行模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStartOptions {
    /// 保存任务、事件和提供者进程证据的持久化根目录。
    pub durable_backend: PathBuf,
    /// 保存生命周期状态、控制邮箱和子系统检查点的目录。
    pub state_dir: PathBuf,
    /// 保存单实例互斥锁；必须与所有同项目守护进程共享。
    pub lock_dir: PathBuf,
    /// 保存可用性探测使用的 PID 文件。
    pub pid_dir: PathBuf,
    /// 记录 `observability_backend` 字段对应的值。
    pub observability_backend: PathBuf,
    /// 记录 `foreground` 字段对应的值。
    pub foreground: bool,
    /// 记录 `dev_mode` 字段对应的值。
    pub dev_mode: bool,
    /// 为真时完成启动边界检查后立即正常关闭，不进入无限控制循环。
    pub shutdown_after_smoke: bool,
}

/// 表示 `DaemonPathReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPathReport {
    /// 记录 `durable_backend_root` 字段对应的值。
    pub durable_backend_root: String,
    /// 记录 `observability_backend_root` 字段对应的值。
    pub observability_backend_root: String,
    /// 记录 `state_dir` 字段对应的值。
    pub state_dir: String,
    /// 记录 `lock_dir` 字段对应的值。
    pub lock_dir: String,
    /// 记录 `pid_dir` 字段对应的值。
    pub pid_dir: String,
    /// 记录 `control_request_dir` 字段对应的值。
    pub control_request_dir: String,
    /// 记录 `control_response_dir` 字段对应的值。
    pub control_response_dir: String,
    /// 记录 `state_file` 字段对应的值。
    pub state_file: String,
    /// 记录 `hardware_hotplug_state_file` 字段对应的值。
    pub hardware_hotplug_state_file: String,
    /// 记录 `lock_file` 字段对应的值。
    pub lock_file: String,
    /// 记录原子 daemon lease 文件路径。
    pub lease_file: String,
    /// 记录 `pid_file` 字段对应的值。
    pub pid_file: String,
}

/// Daemon lease 的稳定只读投影；PID/token/generation 共同标识一次进程 incarnation。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLeaseReport {
    /// `active` 或 `released`。
    pub state: String,
    /// Lease owner 的操作系统 PID。
    pub pid: u32,
    /// 复用 durable writer owner token 的进程启动身份。
    pub process_start_token: String,
    /// 复用 durable writer fencing generation 的单调代际。
    pub generation: u64,
    /// 最近一次成功 heartbeat 的 epoch 毫秒。
    pub heartbeat_at_ms: u128,
    /// Lease 最早可接管的 epoch 毫秒。
    pub expires_at_ms: u128,
    /// 固定 anchor 上的 OS lock 当前是否仍由 owner 持有。
    pub owner_live: bool,
    /// Probe 时 lease 是否已到期；live owner 即使到期也绝不被抢占。
    pub expired: bool,
}

/// Derived daemon lease liveness classification. It is observational and does
/// not itself reclaim or release an owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonFreshness {
    Live,
    Degraded,
    Stale,
}

impl DaemonFreshness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Degraded => "degraded",
            Self::Stale => "stale",
        }
    }
}

/// 表示 `DaemonPolicyReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPolicyReport {
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `source_count` 字段对应的值。
    pub source_count: usize,
    /// 记录 `effective_layers` 字段对应的值。
    pub effective_layers: Vec<String>,
}

/// 守护进程对外发布的生命周期记录；可用性仍需同时满足锁和 PID 存在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStateRecord {
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `mode` 字段对应的值。
    pub mode: String,
    /// 记录 `pid` 字段对应的值。
    pub pid: u32,
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: String,
    /// 记录 `project_root` 字段对应的值。
    pub project_root: String,
    /// 记录 `started_at_ms` 字段对应的值。
    pub started_at_ms: u128,
    /// 记录 `stopped_at_ms` 字段对应的值。
    pub stopped_at_ms: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonShutdownDrainEvidence {
    pid: u32,
    process_start_token: String,
    generation: u64,
    request_id: RequestId,
    completed_at_ms: u128,
    inflight_tasks: usize,
    cancellation_requests: usize,
    forced_terminal_tasks: usize,
    phase: String,
}

impl DaemonShutdownDrainEvidence {
    fn completed(
        request_id: RequestId,
        lease: &DurableRuntimeLeaseRecord,
        drain: &TaskWorkerDrainReport,
    ) -> Result<Self, EvaError> {
        if drain.already_drained || drain.phase != "drained" {
            return Err(EvaError::conflict(
                "daemon shutdown drain report is not a first successful completion",
            ));
        }
        Ok(Self {
            pid: lease.pid(),
            process_start_token: lease.process_start_token().to_owned(),
            generation: lease.generation().0,
            request_id,
            completed_at_ms: now_ms(),
            inflight_tasks: drain.inflight_tasks,
            cancellation_requests: drain.cancellation_requests,
            forced_terminal_tasks: drain.forced_terminal_tasks,
            phase: drain.phase.clone(),
        })
    }

    fn matches_status(&self, status: &DaemonStatusReport) -> bool {
        status
            .state
            .as_ref()
            .is_some_and(|state| state.status == "stopped" && state.pid == self.pid)
            && status.lease.as_ref().is_some_and(|lease| {
                lease.state == "released"
                    && !lease.owner_live
                    && !lease.expired
                    && lease.pid == self.pid
                    && lease.process_start_token == self.process_start_token
                    && lease.generation == self.generation
            })
            && self.phase == "drained"
    }

    fn to_storage(&self) -> String {
        format!(
            "format={SHUTDOWN_DRAIN_EVIDENCE_FORMAT}\npid={}\nprocess_start_token={}\ngeneration={}\nrequest_id={}\ncompleted_at_ms={}\ninflight_tasks={}\ncancellation_requests={}\nforced_terminal_tasks={}\nphase={}\n",
            self.pid,
            encode_field(&self.process_start_token),
            self.generation,
            self.request_id.as_str(),
            self.completed_at_ms,
            self.inflight_tasks,
            self.cancellation_requests,
            self.forced_terminal_tasks,
            self.phase,
        )
    }

    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut format = None;
        let mut pid = None;
        let mut process_start_token = None;
        let mut generation = None;
        let mut request_id = None;
        let mut completed_at_ms = None;
        let mut inflight_tasks = None;
        let mut cancellation_requests = None;
        let mut forced_terminal_tasks = None;
        let mut phase = None;

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict(
                    "daemon shutdown drain evidence is invalid",
                ));
            };
            match key {
                "format" => format = Some(value.to_owned()),
                "pid" => {
                    pid = Some(value.parse::<u32>().map_err(|_| {
                        EvaError::conflict("daemon shutdown drain evidence pid is invalid")
                    })?)
                }
                "process_start_token" => process_start_token = Some(decode_field(value)?),
                "generation" => {
                    generation = Some(value.parse::<u64>().map_err(|_| {
                        EvaError::conflict("daemon shutdown drain evidence generation is invalid")
                    })?)
                }
                "request_id" => request_id = Some(RequestId::parse(value)?),
                "completed_at_ms" => {
                    completed_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::conflict(
                            "daemon shutdown drain evidence completion time is invalid",
                        )
                    })?)
                }
                "inflight_tasks" => {
                    inflight_tasks = Some(value.parse::<usize>().map_err(|_| {
                        EvaError::conflict(
                            "daemon shutdown drain evidence inflight count is invalid",
                        )
                    })?)
                }
                "cancellation_requests" => {
                    cancellation_requests = Some(value.parse::<usize>().map_err(|_| {
                        EvaError::conflict(
                            "daemon shutdown drain evidence cancellation count is invalid",
                        )
                    })?)
                }
                "forced_terminal_tasks" => {
                    forced_terminal_tasks = Some(value.parse::<usize>().map_err(|_| {
                        EvaError::conflict(
                            "daemon shutdown drain evidence forced-terminal count is invalid",
                        )
                    })?)
                }
                "phase" => phase = Some(value.to_owned()),
                _ => {
                    return Err(EvaError::conflict(
                        "daemon shutdown drain evidence has an unknown field",
                    )
                    .with_context("field", key))
                }
            }
        }

        if format.as_deref() != Some(SHUTDOWN_DRAIN_EVIDENCE_FORMAT) {
            return Err(EvaError::conflict(
                "daemon shutdown drain evidence format mismatch",
            ));
        }
        let evidence = Self {
            pid: pid.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing pid")
            })?,
            process_start_token: process_start_token.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing process start token")
            })?,
            generation: generation.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing generation")
            })?,
            request_id: request_id.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing request id")
            })?,
            completed_at_ms: completed_at_ms.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing completion time")
            })?,
            inflight_tasks: inflight_tasks.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing inflight count")
            })?,
            cancellation_requests: cancellation_requests.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing cancellation count")
            })?,
            forced_terminal_tasks: forced_terminal_tasks.ok_or_else(|| {
                EvaError::conflict(
                    "daemon shutdown drain evidence is missing forced-terminal count",
                )
            })?,
            phase: phase.ok_or_else(|| {
                EvaError::conflict("daemon shutdown drain evidence is missing phase")
            })?,
        };
        if evidence.pid == 0
            || evidence.process_start_token.is_empty()
            || evidence.generation == 0
            || evidence.completed_at_ms == 0
            || evidence.phase != "drained"
        {
            return Err(EvaError::conflict(
                "daemon shutdown drain evidence violates its completion contract",
            ));
        }
        Ok(evidence)
    }
}

/// 表示 `DaemonStartReport` 数据结构。
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonStartReport {
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `mode` 字段对应的值。
    pub mode: String,
    /// 记录 `pid` 字段对应的值。
    pub pid: u32,
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: String,
    /// 记录 `project_root` 字段对应的值。
    pub project_root: String,
    /// 记录 `foreground` 字段对应的值。
    pub foreground: bool,
    /// 记录 `dev_mode` 字段对应的值。
    pub dev_mode: bool,
    /// 记录 `provider_processes_started` 字段对应的值。
    pub provider_processes_started: bool,
    /// 记录 `paths` 字段对应的值。
    pub paths: DaemonPathReport,
    /// 本次 daemon incarnation 最终发布的 lease 投影。
    pub lease: DaemonLeaseReport,
    /// 记录 `durable_backend` 字段对应的值。
    pub durable_backend: DurableBackendReport,
    /// 记录 `recovery` 字段对应的值。
    pub recovery: RuntimeRecoveryReport,
    /// 记录 `policy` 字段对应的值。
    pub policy: DaemonPolicyReport,
    /// 记录 `observability` 字段对应的值。
    pub observability: ObservabilitySmokeReport,
    /// 记录 `hardware_hotplug` 字段对应的值。
    pub hardware_hotplug: HardwareHotplugSubscriberReport,
    /// 记录 `memory_maintenance` 字段对应的值。
    pub memory_maintenance: Option<DaemonMemoryMaintenanceReport>,
    /// 记录 `shutdown` 字段对应的值。
    pub shutdown: Option<ShutdownReport>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `DaemonMemoryMaintenanceReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonMemoryMaintenanceReport {
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `memory_gc` 字段对应的值。
    pub memory_gc: MemoryCompactionReport,
    /// 记录 `knowledge_rebuild` 字段对应的值。
    pub knowledge_rebuild: KnowledgeRebuildCheckpointReport,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `DaemonStatusReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatusReport {
    /// 记录 `available` 字段对应的值。
    pub available: bool,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `lock_present` 字段对应的值。
    pub lock_present: bool,
    /// 记录 `pid_present` 字段对应的值。
    pub pid_present: bool,
    /// PID projection 是否与 state 和 lease 的 owner PID 一致。
    pub pid_matches_lease: bool,
    /// Derived freshness of the observed daemon lease.
    pub freshness: String,
    /// Age of the observed lease heartbeat at status read time.
    pub heartbeat_age_ms: Option<u128>,
    /// 当前 lease；从未 claim 时为 `None`。
    pub lease: Option<DaemonLeaseReport>,
    /// 记录 `paths` 字段对应的值。
    pub paths: DaemonPathReport,
    /// 记录 `state` 字段对应的值。
    pub state: Option<DaemonStateRecord>,
}

/// 表示 `DaemonStopReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStopReport {
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `mutation_executed` 字段对应的值。
    pub mutation_executed: bool,
    /// 记录 `lock_removed` 字段对应的值。
    pub lock_removed: bool,
    /// 记录 `pid_removed` 字段对应的值。
    pub pid_removed: bool,
    /// stop 返回时的最终 lease 投影；无历史且无需清理时为 `None`。
    pub lease: Option<DaemonLeaseReport>,
    /// 记录 `paths` 字段对应的值。
    pub paths: DaemonPathReport,
    /// 记录 `state` 字段对应的值。
    pub state: Option<DaemonStateRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStartupHandshake {
    nonce: String,
    launcher_pid: u32,
    child_start_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStartupPhase {
    Claimed,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStartupFrame {
    pub phase: DaemonStartupPhase,
    pub nonce: String,
    pub launcher_pid: u32,
    pub child_pid: u32,
    pub process_start_token: Option<String>,
    pub generation: Option<u64>,
    pub report_digest: Option<String>,
    pub observed_at_ms: u128,
    pub error_kind: Option<String>,
    pub cleanup_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStartupCleanupReport {
    pub child_pid: u32,
    pub identity_source: String,
    pub pid_removed: bool,
    pub state_stopped: bool,
    pub lease: Option<DaemonLeaseReport>,
    pub cleanup_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonPidProjection {
    Versioned {
        pid: u32,
        process_start_token: String,
        generation: u64,
    },
    Legacy {
        pid: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonRunMode {
    Foreground,
    BackgroundChild,
}

struct DaemonStartupHooks<'a> {
    handshake: &'a DaemonStartupHandshake,
    publish_report: &'a mut dyn FnMut(&DaemonStartReport) -> Result<String, EvaError>,
    ready_published: bool,
}

/// 定义文件邮箱可请求的只读查询与持久化状态变更。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonControlOperation {
    /// 只读取当前守护状态，不执行持久化变更。
    Status,
    /// 关闭运行时并发布 stopped 状态，再移除 PID。
    Shutdown,
    /// 在持久化任务存储中创建 queued 快照。
    SubmitTask,
    /// 在既有任务快照上记录取消请求，而非直接伪造终态。
    CancelTask,
    /// 持久化停止接收新工作的代际排空计划。
    Drain,
    /// 验证代际提升与排空顺序后持久化新的路由状态。
    ReloadPlan,
}

/// 以 `request_id` 为邮箱文件名和响应关联键的版本化控制请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonControlRequest {
    /// mailbox wire schema；新请求为 2，reader 继续接受旧版本 1。
    wire_version: u32,
    /// 在请求和响应目录中唯一标识本次投递；复用该值会覆盖其邮箱关联语义。
    pub request_id: RequestId,
    /// 记录 `trace_id` 字段对应的值。
    pub trace_id: String,
    /// 记录 `operation` 字段对应的值。
    pub operation: DaemonControlOperation,
    /// 记录 `task_id` 字段对应的值。
    pub task_id: Option<String>,
    /// submit_task 的完整不可变业务信封；其他 operation 必须为 None。
    pub task_envelope: Option<TaskEnvelope>,
    /// 记录 `reason` 字段对应的值。
    pub reason: Option<String>,
    /// 记录 `plan_id` 字段对应的值。
    pub plan_id: Option<String>,
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: Option<String>,
    /// 记录 `agent_id` 字段对应的值。
    pub agent_id: Option<String>,
    /// 记录 `from_generation_id` 字段对应的值。
    pub from_generation_id: Option<String>,
    /// 记录 `to_generation_id` 字段对应的值。
    pub to_generation_id: Option<String>,
    /// 记录 `from_release` 字段对应的值。
    pub from_release: Option<String>,
    /// 记录 `to_release` 字段对应的值。
    pub to_release: Option<String>,
    /// 记录 `inflight_tasks` 字段对应的值。
    pub inflight_tasks: Option<usize>,
    /// 记录 `timeout_ms` 字段对应的值。
    pub timeout_ms: Option<u64>,
    /// 记录 `created_at_ms` 字段对应的值。
    pub created_at_ms: u128,
}

/// 记录控制操作结果、是否执行变更及对应请求/响应文件证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonControlResponse {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `trace_id` 字段对应的值。
    pub trace_id: String,
    /// 记录 `operation` 字段对应的值。
    pub operation: DaemonControlOperation,
    /// 记录 `accepted` 字段对应的值。
    pub accepted: bool,
    /// 记录 `daemon_available` 字段对应的值。
    pub daemon_available: bool,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 区分请求被接受与是否实际完成持久化状态变更。
    pub mutation_executed: bool,
    /// 记录 `request_file` 字段对应的值。
    pub request_file: String,
    /// 记录 `response_file` 字段对应的值。
    pub response_file: String,
    /// 记录 `state` 字段对应的值。
    pub state: Option<DaemonStateRecord>,
    /// 响应生成时由持有中的 guard 提供的 lease 身份。
    pub lease: Option<DaemonLeaseReport>,
    /// 记录 `task_id` 字段对应的值。
    pub task_id: Option<String>,
    /// 记录 `plan_id` 字段对应的值。
    pub plan_id: Option<String>,
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: Option<String>,
    /// 记录 `message` 字段对应的值。
    pub message: String,
    /// 记录 `shutdown` 字段对应的值。
    pub shutdown: Option<ShutdownReport>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 排空或热重载操作成功规划后原子发布的代理控制检查点。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonAgentControlState {
    /// 记录 `agent_id` 字段对应的值。
    agent_id: String,
    /// 记录 `operation` 字段对应的值。
    operation: String,
    /// 记录 `lifecycle` 字段对应的值。
    lifecycle: String,
    /// 记录 `drain_generation_id` 字段对应的值。
    drain_generation_id: Option<String>,
    /// 记录 `drain_inflight_tasks` 字段对应的值。
    drain_inflight_tasks: Option<usize>,
    /// 记录 `drain_timeout_ms` 字段对应的值。
    drain_timeout_ms: Option<u64>,
    /// 记录 `drain_accepts_new_work` 字段对应的值。
    drain_accepts_new_work: Option<bool>,
    /// 记录 `drain_status` 字段对应的值。
    drain_status: Option<String>,
    /// 记录 `active_generation` 字段对应的值。
    active_generation: Option<String>,
    /// 记录 `previous_generation` 字段对应的值。
    previous_generation: Option<String>,
    /// 记录 `previous_generation_state` 字段对应的值。
    previous_generation_state: Option<String>,
    /// 记录 `selected_generation_for_new_work` 字段对应的值。
    selected_generation_for_new_work: Option<String>,
    /// 记录 `from_release` 字段对应的值。
    from_release: Option<String>,
    /// 记录 `to_release` 字段对应的值。
    to_release: Option<String>,
    /// 记录 `plan_id` 字段对应的值。
    plan_id: Option<String>,
    /// 记录 `mutation_executed` 字段对应的值。
    mutation_executed: bool,
    /// 记录 `updated_at_ms` 字段对应的值。
    updated_at_ms: u128,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonStartOptions {
    /// 执行 `defaults` 对应的处理逻辑。
    pub fn defaults(project: &ProjectConfig) -> Self {
        let data_dir = project
            .eva
            .runtime
            .data_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(".eva/data"));
        let data_root = resolve_project_path(&project.project_root, &data_dir);
        Self {
            durable_backend: data_root.join("durable"),
            state_dir: data_root.join("daemon").join("state"),
            lock_dir: data_root.join("daemon").join("locks"),
            pid_dir: data_root.join("daemon").join("pids"),
            observability_backend: data_root.join("observability"),
            foreground: true,
            dev_mode: false,
            shutdown_after_smoke: true,
        }
    }

    /// 执行 `resolve_against_project` 对应的处理逻辑。
    pub fn resolve_against_project(mut self, project_root: &Path) -> Self {
        self.durable_backend = resolve_project_path(project_root, &self.durable_backend);
        self.state_dir = resolve_project_path(project_root, &self.state_dir);
        self.lock_dir = resolve_project_path(project_root, &self.lock_dir);
        self.pid_dir = resolve_project_path(project_root, &self.pid_dir);
        self.observability_backend =
            resolve_project_path(project_root, &self.observability_backend);
        self
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonPathReport {
    /// 从持久化数据或输入构造 `from_options` 对应的值。
    fn from_options(options: &DaemonStartOptions) -> Self {
        Self {
            durable_backend_root: display_path(&options.durable_backend),
            observability_backend_root: display_path(&options.observability_backend),
            state_dir: display_path(&options.state_dir),
            lock_dir: display_path(&options.lock_dir),
            pid_dir: display_path(&options.pid_dir),
            control_request_dir: display_path(&control_request_dir(options)),
            control_response_dir: display_path(&control_response_dir(options)),
            state_file: display_path(&state_file(options)),
            hardware_hotplug_state_file: display_path(&hardware_hotplug_state_file(options)),
            lock_file: display_path(&lock_file(options)),
            lease_file: display_path(&lease_file(options)),
            pid_file: display_path(&pid_file(options)),
        }
    }
}

impl DaemonLeaseReport {
    fn from_record(record: &DurableRuntimeLeaseRecord, owner_live: bool, expired: bool) -> Self {
        let state = match record.state() {
            DurableRuntimeLeaseState::Active => "active",
            DurableRuntimeLeaseState::Released => "released",
        };
        Self {
            state: state.to_owned(),
            pid: record.pid(),
            process_start_token: record.process_start_token().to_owned(),
            generation: record.generation().0,
            heartbeat_at_ms: record.heartbeat_at_ms(),
            expires_at_ms: record.expires_at_ms(),
            owner_live,
            expired,
        }
    }

    fn from_guard(lease: &DurableRuntimeLeaseGuard, now_ms: u128) -> Self {
        let record = lease.record();
        Self::from_record(
            record,
            record.state() == DurableRuntimeLeaseState::Active,
            record.expires_at_ms() <= now_ms,
        )
    }

    /// Return the non-negative age of the last persisted daemon heartbeat.
    pub fn heartbeat_age_ms(&self, now_ms: u128) -> u128 {
        now_ms.saturating_sub(self.heartbeat_at_ms)
    }

    /// Classify the lease without treating a degraded owner as reclaimable.
    pub fn freshness_at(&self, now_ms: u128) -> DaemonFreshness {
        if self.state != "active"
            || !self.owner_live
            || self.expired
            || now_ms >= self.expires_at_ms
        {
            return DaemonFreshness::Stale;
        }
        let age_ms = self.heartbeat_age_ms(now_ms);
        if age_ms < DAEMON_LEASE_DEGRADED_AFTER_MS {
            DaemonFreshness::Live
        } else if age_ms < DAEMON_LEASE_STALE_AFTER_MS {
            DaemonFreshness::Degraded
        } else {
            DaemonFreshness::Stale
        }
    }
}

impl DaemonStartupHandshake {
    pub fn new(
        nonce: impl Into<String>,
        launcher_pid: u32,
        child_start_token: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let nonce = nonce.into();
        validate_startup_nonce(&nonce)?;
        if launcher_pid == 0 {
            return Err(EvaError::invalid_argument(
                "daemon startup launcher pid must be positive",
            ));
        }
        let child_start_token = child_start_token.into();
        validate_startup_child_token(&child_start_token)?;
        Ok(Self {
            nonce,
            launcher_pid,
            child_start_token,
        })
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub const fn launcher_pid(&self) -> u32 {
        self.launcher_pid
    }

    pub fn child_start_token(&self) -> &str {
        &self.child_start_token
    }
}

impl DaemonStartupPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "claimed" => Ok(Self::Claimed),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            _ => Err(EvaError::conflict("daemon startup frame phase is invalid")),
        }
    }
}

impl DaemonStartupFrame {
    fn claimed(handshake: &DaemonStartupHandshake, lease: &DurableRuntimeLeaseRecord) -> Self {
        Self {
            phase: DaemonStartupPhase::Claimed,
            nonce: handshake.nonce.clone(),
            launcher_pid: handshake.launcher_pid,
            child_pid: lease.pid(),
            process_start_token: Some(lease.process_start_token().to_owned()),
            generation: Some(lease.generation().0),
            report_digest: None,
            observed_at_ms: now_ms(),
            error_kind: None,
            cleanup_complete: false,
        }
    }

    fn ready(
        handshake: &DaemonStartupHandshake,
        lease: &DurableRuntimeLeaseRecord,
        report_digest: String,
    ) -> Self {
        Self {
            phase: DaemonStartupPhase::Ready,
            nonce: handshake.nonce.clone(),
            launcher_pid: handshake.launcher_pid,
            child_pid: lease.pid(),
            process_start_token: Some(lease.process_start_token().to_owned()),
            generation: Some(lease.generation().0),
            report_digest: Some(report_digest),
            observed_at_ms: now_ms(),
            error_kind: None,
            cleanup_complete: false,
        }
    }

    fn failed(
        handshake: &DaemonStartupHandshake,
        child_pid: u32,
        claimed: Option<&Self>,
        error: &EvaError,
        cleanup_complete: bool,
    ) -> Self {
        Self {
            phase: DaemonStartupPhase::Failed,
            nonce: handshake.nonce.clone(),
            launcher_pid: handshake.launcher_pid,
            child_pid: claimed.map(|frame| frame.child_pid).unwrap_or(child_pid),
            process_start_token: claimed.and_then(|frame| frame.process_start_token.clone()),
            generation: claimed.and_then(|frame| frame.generation),
            report_digest: None,
            observed_at_ms: now_ms(),
            error_kind: Some(error.kind().as_str().to_owned()),
            cleanup_complete,
        }
    }

    fn validate(&self) -> Result<(), EvaError> {
        validate_startup_nonce(&self.nonce)?;
        if self.launcher_pid == 0 || self.child_pid == 0 {
            return Err(EvaError::conflict(
                "daemon startup frame process identity is invalid",
            ));
        }
        let has_identity = self
            .process_start_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
            && self.generation.is_some_and(|generation| generation > 0);
        let has_report_digest = self
            .report_digest
            .as_deref()
            .is_some_and(is_canonical_sha256);
        match self.phase {
            DaemonStartupPhase::Claimed
                if has_identity
                    && self.report_digest.is_none()
                    && self.error_kind.is_none()
                    && !self.cleanup_complete =>
            {
                Ok(())
            }
            DaemonStartupPhase::Ready
                if has_identity
                    && has_report_digest
                    && self.error_kind.is_none()
                    && !self.cleanup_complete =>
            {
                Ok(())
            }
            DaemonStartupPhase::Failed
                if self.report_digest.is_none() && self.error_kind.is_some() =>
            {
                Ok(())
            }
            _ => Err(EvaError::conflict(
                "daemon startup frame fields do not match its phase",
            )),
        }
    }

    fn to_storage(&self) -> String {
        format!(
            "format={STARTUP_FRAME_FORMAT}\nphase={}\nnonce={}\nlauncher_pid={}\nchild_pid={}\nprocess_start_token={}\ngeneration={}\nreport_digest={}\nobserved_at_ms={}\nerror_kind={}\ncleanup_complete={}\n",
            self.phase.as_str(),
            self.nonce,
            self.launcher_pid,
            self.child_pid,
            self.process_start_token.as_deref().unwrap_or_default(),
            self.generation
                .map(|generation| generation.to_string())
                .unwrap_or_default(),
            self.report_digest.as_deref().unwrap_or_default(),
            self.observed_at_ms,
            self.error_kind.as_deref().unwrap_or_default(),
            self.cleanup_complete
        )
    }

    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut fields = std::collections::BTreeMap::new();
        for line in data.lines().filter(|line| !line.is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon startup frame is invalid"));
            };
            if key.is_empty() || fields.insert(key, value).is_some() {
                return Err(EvaError::conflict(
                    "daemon startup frame contains duplicate or empty fields",
                ));
            }
        }
        if fields.len() != 11 || fields.get("format").copied() != Some(STARTUP_FRAME_FORMAT) {
            return Err(EvaError::conflict(
                "daemon startup frame format is corrupt or unsupported",
            ));
        }
        let parse_u32 = |name: &str| {
            fields
                .get(name)
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| EvaError::conflict("daemon startup frame integer is invalid"))
        };
        let parse_optional_u64 = |name: &str| -> Result<Option<u64>, EvaError> {
            let value = fields.get(name).copied().unwrap_or_default();
            if value.is_empty() {
                Ok(None)
            } else {
                value
                    .parse::<u64>()
                    .map(Some)
                    .map_err(|_| EvaError::conflict("daemon startup frame generation is invalid"))
            }
        };
        let frame = Self {
            phase: DaemonStartupPhase::parse(fields.get("phase").copied().unwrap_or_default())?,
            nonce: fields.get("nonce").copied().unwrap_or_default().to_owned(),
            launcher_pid: parse_u32("launcher_pid")?,
            child_pid: parse_u32("child_pid")?,
            process_start_token: fields
                .get("process_start_token")
                .copied()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            generation: parse_optional_u64("generation")?,
            report_digest: fields
                .get("report_digest")
                .copied()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            observed_at_ms: fields
                .get("observed_at_ms")
                .and_then(|value| value.parse::<u128>().ok())
                .ok_or_else(|| EvaError::conflict("daemon startup frame timestamp is invalid"))?,
            error_kind: fields
                .get("error_kind")
                .copied()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            cleanup_complete: match fields.get("cleanup_complete").copied() {
                Some("true") => true,
                Some("false") => false,
                _ => {
                    return Err(EvaError::conflict(
                        "daemon startup frame cleanup flag is invalid",
                    ))
                }
            },
        };
        frame.validate()?;
        Ok(frame)
    }
}

impl DaemonRunMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground_dev",
            Self::BackgroundChild => "background",
        }
    }

    const fn foreground(self) -> bool {
        matches!(self, Self::Foreground)
    }
}

impl DaemonPidProjection {
    fn pid(&self) -> u32 {
        match self {
            Self::Versioned { pid, .. } | Self::Legacy { pid } => *pid,
        }
    }

    fn matches_lease(&self, lease: &DurableRuntimeLeaseRecord) -> bool {
        self.matches_identity(&lease.identity())
    }

    fn matches_identity(&self, identity: &DurableRuntimeLeaseIdentity) -> bool {
        match self {
            Self::Versioned {
                pid,
                process_start_token,
                generation,
            } => {
                *pid == identity.pid()
                    && process_start_token == identity.process_start_token()
                    && *generation == identity.generation().0
            }
            Self::Legacy { .. } => false,
        }
    }

    fn from_lease(lease: &DurableRuntimeLeaseRecord) -> Self {
        Self::Versioned {
            pid: lease.pid(),
            process_start_token: lease.process_start_token().to_owned(),
            generation: lease.generation().0,
        }
    }

    fn to_storage(&self) -> String {
        match self {
            Self::Versioned {
                pid,
                process_start_token,
                generation,
            } => format!(
                "format={PID_PROJECTION_FORMAT}\npid={pid}\nprocess_start_token={process_start_token}\ngeneration={generation}\n"
            ),
            Self::Legacy { pid } => pid.to_string(),
        }
    }

    fn from_storage(data: &str) -> Result<Self, EvaError> {
        if let Ok(pid) = data.trim().parse::<u32>() {
            if pid == 0 {
                return Err(EvaError::conflict("daemon pid file contains zero"));
            }
            return Ok(Self::Legacy { pid });
        }

        let mut fields = std::collections::BTreeMap::new();
        for line in data.lines().filter(|line| !line.is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon pid projection is invalid"));
            };
            if key.is_empty() || fields.insert(key, value).is_some() {
                return Err(EvaError::conflict(
                    "daemon pid projection contains duplicate or empty fields",
                ));
            }
        }
        if fields.len() != 4 || fields.get("format").copied() != Some(PID_PROJECTION_FORMAT) {
            return Err(EvaError::conflict(
                "daemon pid projection format is corrupt or unsupported",
            ));
        }
        let pid = fields
            .get("pid")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid > 0)
            .ok_or_else(|| EvaError::conflict("daemon pid projection pid is invalid"))?;
        let process_start_token = fields
            .get("process_start_token")
            .filter(|value| !value.is_empty())
            .map(|value| (*value).to_owned())
            .ok_or_else(|| {
                EvaError::conflict("daemon pid projection process start token is invalid")
            })?;
        let generation = fields
            .get("generation")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|generation| *generation > 0)
            .ok_or_else(|| EvaError::conflict("daemon pid projection generation is invalid"))?;
        Ok(Self::Versioned {
            pid,
            process_start_token,
            generation,
        })
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonStateRecord {
    /// 执行 `running` 对应的受控流程。
    fn running(project: &ProjectConfig, mode: DaemonRunMode) -> Self {
        Self {
            status: "running".to_owned(),
            mode: mode.as_str().to_owned(),
            pid: std::process::id(),
            generation_id: DAEMON_GENERATION.to_owned(),
            project_root: display_path(&project.project_root),
            started_at_ms: now_ms(),
            stopped_at_ms: None,
        }
    }

    /// 停止、取消或释放 `stopped` 管理的状态。
    fn stopped(mut self) -> Self {
        self.status = "stopped".to_owned();
        self.stopped_at_ms = Some(now_ms());
        self
    }

    /// 按稳定存储格式编码 `to_storage` 对应的数据。
    fn to_storage(&self) -> String {
        let stopped_at_ms = self
            .stopped_at_ms
            .map(|value| value.to_string())
            .unwrap_or_default();
        format!(
            "status={}\nmode={}\npid={}\ngeneration_id={}\nproject_root={}\nstarted_at_ms={}\nstopped_at_ms={}\n",
            self.status,
            self.mode,
            self.pid,
            self.generation_id,
            self.project_root,
            self.started_at_ms,
            stopped_at_ms
        )
    }

    /// 从持久化数据或输入构造 `from_storage` 对应的值。
    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut status = None;
        let mut mode = None;
        let mut pid = None;
        let mut generation_id = None;
        let mut project_root = None;
        let mut started_at_ms = None;
        let mut stopped_at_ms = None;

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon state record is invalid"));
            };
            match key {
                "status" => status = Some(value.to_owned()),
                "mode" => mode = Some(value.to_owned()),
                "pid" => {
                    pid = Some(
                        value
                            .parse::<u32>()
                            .map_err(|_| EvaError::conflict("daemon state pid is invalid"))?,
                    )
                }
                "generation_id" => generation_id = Some(value.to_owned()),
                "project_root" => project_root = Some(value.to_owned()),
                "started_at_ms" => {
                    started_at_ms =
                        Some(value.parse::<u128>().map_err(|_| {
                            EvaError::conflict("daemon state started_at_ms is invalid")
                        })?)
                }
                "stopped_at_ms" => {
                    stopped_at_ms = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<u128>().map_err(|_| {
                            EvaError::conflict("daemon state stopped_at_ms is invalid")
                        })?)
                    }
                }
                _ => {
                    return Err(EvaError::conflict("daemon state has unknown field")
                        .with_context("field", key));
                }
            }
        }

        Ok(Self {
            status: status.ok_or_else(|| EvaError::conflict("daemon state missing status"))?,
            mode: mode.ok_or_else(|| EvaError::conflict("daemon state missing mode"))?,
            pid: pid.ok_or_else(|| EvaError::conflict("daemon state missing pid"))?,
            generation_id: generation_id
                .ok_or_else(|| EvaError::conflict("daemon state missing generation_id"))?,
            project_root: project_root
                .ok_or_else(|| EvaError::conflict("daemon state missing project_root"))?,
            started_at_ms: started_at_ms
                .ok_or_else(|| EvaError::conflict("daemon state missing started_at_ms"))?,
            stopped_at_ms,
        })
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonAgentControlState {
    /// 从持久化数据或输入构造 `from_drain` 对应的值。
    fn from_drain(
        agent_id: String,
        drain: &DrainPlan,
        plan_id: Option<String>,
        audit: Vec<String>,
    ) -> Self {
        Self {
            agent_id,
            operation: "drain".to_owned(),
            lifecycle: "draining".to_owned(),
            drain_generation_id: Some(drain.generation_id.as_str().to_owned()),
            drain_inflight_tasks: Some(drain.inflight_tasks),
            drain_timeout_ms: Some(drain.timeout_ms),
            drain_accepts_new_work: Some(drain.accepts_new_work),
            drain_status: Some(drain.status.as_str().to_owned()),
            active_generation: None,
            previous_generation: None,
            previous_generation_state: None,
            selected_generation_for_new_work: None,
            from_release: None,
            to_release: None,
            plan_id,
            mutation_executed: true,
            updated_at_ms: now_ms(),
            audit,
        }
    }

    /// 按稳定存储格式编码 `to_storage` 对应的数据。
    fn to_storage(&self) -> String {
        format!(
            "version=1\nagent_id={}\noperation={}\nlifecycle={}\ndrain_generation_id={}\ndrain_inflight_tasks={}\ndrain_timeout_ms={}\ndrain_accepts_new_work={}\ndrain_status={}\nactive_generation={}\nprevious_generation={}\nprevious_generation_state={}\nselected_generation_for_new_work={}\nfrom_release={}\nto_release={}\nplan_id={}\nmutation_executed={}\nupdated_at_ms={}\naudit={}\n",
            encode_field(&self.agent_id),
            encode_field(&self.operation),
            encode_field(&self.lifecycle),
            encode_optional_field(self.drain_generation_id.as_deref()),
            self.drain_inflight_tasks
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.drain_timeout_ms
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.drain_accepts_new_work
                .map(|value| value.to_string())
                .unwrap_or_default(),
            encode_optional_field(self.drain_status.as_deref()),
            encode_optional_field(self.active_generation.as_deref()),
            encode_optional_field(self.previous_generation.as_deref()),
            encode_optional_field(self.previous_generation_state.as_deref()),
            encode_optional_field(self.selected_generation_for_new_work.as_deref()),
            encode_optional_field(self.from_release.as_deref()),
            encode_optional_field(self.to_release.as_deref()),
            encode_optional_field(self.plan_id.as_deref()),
            self.mutation_executed,
            self.updated_at_ms,
            encode_audit(&self.audit)
        )
    }

    /// 从持久化数据或输入构造 `from_storage` 对应的值。
    #[cfg(test)]
    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut agent_id = None;
        let mut operation = None;
        let mut lifecycle = None;
        let mut drain_generation_id = None;
        let mut drain_inflight_tasks = None;
        let mut drain_timeout_ms = None;
        let mut drain_accepts_new_work = None;
        let mut drain_status = None;
        let mut active_generation = None;
        let mut previous_generation = None;
        let mut previous_generation_state = None;
        let mut selected_generation_for_new_work = None;
        let mut from_release = None;
        let mut to_release = None;
        let mut plan_id = None;
        let mut mutation_executed = None;
        let mut updated_at_ms = None;
        let mut audit = Vec::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon agent control state is invalid"));
            };
            match key {
                "version" => {
                    if value != "1" {
                        return Err(EvaError::conflict(
                            "daemon agent control state version mismatch",
                        )
                        .with_context("version", value));
                    }
                }
                "agent_id" => agent_id = Some(decode_field(value)?),
                "operation" => operation = Some(decode_field(value)?),
                "lifecycle" => lifecycle = Some(decode_field(value)?),
                "drain_generation_id" => drain_generation_id = decode_optional_field(value)?,
                "drain_inflight_tasks" => {
                    drain_inflight_tasks = parse_optional_usize(
                        value,
                        "daemon agent control state drain_inflight_tasks is invalid",
                    )?
                }
                "drain_timeout_ms" => {
                    drain_timeout_ms = parse_optional_u64(
                        value,
                        "daemon agent control state drain_timeout_ms is invalid",
                    )?
                }
                "drain_accepts_new_work" => {
                    drain_accepts_new_work = if value.is_empty() {
                        None
                    } else {
                        Some(parse_bool(value, "drain_accepts_new_work")?)
                    }
                }
                "drain_status" => drain_status = decode_optional_field(value)?,
                "active_generation" => active_generation = decode_optional_field(value)?,
                "previous_generation" => previous_generation = decode_optional_field(value)?,
                "previous_generation_state" => {
                    previous_generation_state = decode_optional_field(value)?
                }
                "selected_generation_for_new_work" => {
                    selected_generation_for_new_work = decode_optional_field(value)?
                }
                "from_release" => from_release = decode_optional_field(value)?,
                "to_release" => to_release = decode_optional_field(value)?,
                "plan_id" => plan_id = decode_optional_field(value)?,
                "mutation_executed" => {
                    mutation_executed = Some(parse_bool(value, "mutation_executed")?)
                }
                "updated_at_ms" => {
                    updated_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::conflict("daemon agent control state updated_at_ms is invalid")
                    })?)
                }
                "audit" => audit = decode_audit(value)?,
                _ => {
                    return Err(
                        EvaError::conflict("daemon agent control state has unknown field")
                            .with_context("field", key),
                    );
                }
            }
        }

        Ok(Self {
            agent_id: agent_id
                .ok_or_else(|| EvaError::conflict("daemon agent control state missing agent_id"))?,
            operation: operation.ok_or_else(|| {
                EvaError::conflict("daemon agent control state missing operation")
            })?,
            lifecycle: lifecycle.ok_or_else(|| {
                EvaError::conflict("daemon agent control state missing lifecycle")
            })?,
            drain_generation_id,
            drain_inflight_tasks,
            drain_timeout_ms,
            drain_accepts_new_work,
            drain_status,
            active_generation,
            previous_generation,
            previous_generation_state,
            selected_generation_for_new_work,
            from_release,
            to_release,
            plan_id,
            mutation_executed: mutation_executed.ok_or_else(|| {
                EvaError::conflict("daemon agent control state missing mutation_executed")
            })?,
            updated_at_ms: updated_at_ms.ok_or_else(|| {
                EvaError::conflict("daemon agent control state missing updated_at_ms")
            })?,
            audit,
        })
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonControlOperation {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Shutdown => "shutdown",
            Self::SubmitTask => "submit_task",
            Self::CancelTask => "cancel_task",
            Self::Drain => "drain",
            Self::ReloadPlan => "reload_plan",
        }
    }

    /// 解析 `parse` 对应的数据，并拒绝无效格式。
    fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "status" => Ok(Self::Status),
            "shutdown" => Ok(Self::Shutdown),
            "submit_task" => Ok(Self::SubmitTask),
            "cancel_task" => Ok(Self::CancelTask),
            "drain" => Ok(Self::Drain),
            "reload_plan" => Ok(Self::ReloadPlan),
            _ => Err(
                EvaError::invalid_argument("unknown daemon control operation")
                    .with_context("operation", value),
            ),
        }
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonControlRequest {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        request_id: RequestId,
        trace: &TraceFields,
        operation: DaemonControlOperation,
    ) -> Self {
        Self {
            wire_version: 2,
            request_id,
            trace_id: trace_id(trace),
            operation,
            task_id: None,
            task_envelope: None,
            reason: None,
            plan_id: None,
            generation_id: None,
            agent_id: None,
            from_generation_id: None,
            to_generation_id: None,
            from_release: None,
            to_release: None,
            inflight_tasks: None,
            timeout_ms: None,
            created_at_ms: now_ms(),
        }
    }

    /// 设置 `task_id` 并返回更新后的实例。
    pub fn with_task_id(mut self, value: impl Into<String>) -> Self {
        self.task_id = Some(value.into());
        self
    }

    /// 为 submit_task 绑定完整、已校验的任务信封。
    pub fn with_task_envelope(mut self, envelope: TaskEnvelope) -> Self {
        self.task_envelope = Some(envelope);
        self
    }

    /// 设置 `reason` 并返回更新后的实例。
    pub fn with_reason(mut self, value: impl Into<String>) -> Self {
        self.reason = Some(value.into());
        self
    }

    /// 设置 `plan_id` 并返回更新后的实例。
    pub fn with_plan_id(mut self, value: impl Into<String>) -> Self {
        self.plan_id = Some(value.into());
        self
    }

    /// 设置 `generation_id` 并返回更新后的实例。
    pub fn with_generation_id(mut self, value: impl Into<String>) -> Self {
        self.generation_id = Some(value.into());
        self
    }

    /// 设置 `agent_id` 并返回更新后的实例。
    pub fn with_agent_id(mut self, value: impl Into<String>) -> Self {
        self.agent_id = Some(value.into());
        self
    }

    /// 设置 `from_generation_id` 并返回更新后的实例。
    pub fn with_from_generation_id(mut self, value: impl Into<String>) -> Self {
        self.from_generation_id = Some(value.into());
        self
    }

    /// 设置 `to_generation_id` 并返回更新后的实例。
    pub fn with_to_generation_id(mut self, value: impl Into<String>) -> Self {
        self.to_generation_id = Some(value.into());
        self
    }

    /// 设置 `from_release` 并返回更新后的实例。
    pub fn with_from_release(mut self, value: impl Into<String>) -> Self {
        self.from_release = Some(value.into());
        self
    }

    /// 设置 `to_release` 并返回更新后的实例。
    pub fn with_to_release(mut self, value: impl Into<String>) -> Self {
        self.to_release = Some(value.into());
        self
    }

    /// 设置 `inflight_tasks` 并返回更新后的实例。
    pub fn with_inflight_tasks(mut self, value: usize) -> Self {
        self.inflight_tasks = Some(value);
        self
    }

    /// 设置 `timeout_ms` 并返回更新后的实例。
    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = Some(value);
        self
    }

    /// 按稳定存储格式编码 `to_storage` 对应的数据。
    fn to_storage(&self) -> String {
        if self.wire_version == 1 {
            return format!(
                "version=1\nrequest_id={}\ntrace_id={}\noperation={}\ntask_id={}\nreason={}\nplan_id={}\ngeneration_id={}\nagent_id={}\nfrom_generation_id={}\nto_generation_id={}\nfrom_release={}\nto_release={}\ninflight_tasks={}\ntimeout_ms={}\ncreated_at_ms={}\n",
                self.request_id.as_str(),
                encode_field(&self.trace_id),
                self.operation.as_str(),
                encode_optional_field(self.task_id.as_deref()),
                encode_optional_field(self.reason.as_deref()),
                encode_optional_field(self.plan_id.as_deref()),
                encode_optional_field(self.generation_id.as_deref()),
                encode_optional_field(self.agent_id.as_deref()),
                encode_optional_field(self.from_generation_id.as_deref()),
                encode_optional_field(self.to_generation_id.as_deref()),
                encode_optional_field(self.from_release.as_deref()),
                encode_optional_field(self.to_release.as_deref()),
                self.inflight_tasks
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                self.timeout_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                self.created_at_ms
            );
        }
        let envelope = self.task_envelope.as_ref().map(TaskEnvelope::to_snapshot);
        let (
            envelope_kind,
            envelope_agent_id,
            input_kind,
            inline_input_hex,
            artifact_ref,
            input_digest,
            idempotency_key,
            max_attempts,
            retry_backoff_ms,
            attempt_timeout_ms,
        ) = match envelope {
            Some(envelope) => {
                let (input_kind, inline_input_hex, artifact_ref, input_digest) =
                    match envelope.input {
                        TaskInputSnapshot::Inline { bytes, digest } => {
                            ("inline", encode_bytes(&bytes), String::new(), digest)
                        }
                        TaskInputSnapshot::Artifact {
                            artifact_ref,
                            digest,
                        } => (
                            "artifact",
                            String::new(),
                            encode_field(&artifact_ref),
                            digest,
                        ),
                    };
                (
                    encode_field(&envelope.kind),
                    encode_field(&envelope.agent_id),
                    input_kind,
                    inline_input_hex,
                    artifact_ref,
                    input_digest,
                    encode_field(&envelope.idempotency_key),
                    envelope.attempt_policy.max_attempts.to_string(),
                    envelope.attempt_policy.retry_backoff_ms.to_string(),
                    envelope
                        .attempt_policy
                        .attempt_timeout_ms
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
            }
            None => (
                String::new(),
                String::new(),
                "",
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        [
            format!("version={}", self.wire_version),
            format!("request_id={}", self.request_id.as_str()),
            format!("trace_id={}", encode_field(&self.trace_id)),
            format!("operation={}", self.operation.as_str()),
            format!("task_id={}", encode_optional_field(self.task_id.as_deref())),
            format!("task_envelope_kind={envelope_kind}"),
            format!("task_envelope_agent_id={envelope_agent_id}"),
            format!("task_input_kind={input_kind}"),
            format!("task_inline_input_hex={inline_input_hex}"),
            format!("task_artifact_ref={artifact_ref}"),
            format!("task_input_digest={input_digest}"),
            format!("task_idempotency_key={idempotency_key}"),
            format!("task_max_attempts={max_attempts}"),
            format!("task_retry_backoff_ms={retry_backoff_ms}"),
            format!("task_attempt_timeout_ms={attempt_timeout_ms}"),
            format!("reason={}", encode_optional_field(self.reason.as_deref())),
            format!("plan_id={}", encode_optional_field(self.plan_id.as_deref())),
            format!(
                "generation_id={}",
                encode_optional_field(self.generation_id.as_deref())
            ),
            format!(
                "agent_id={}",
                encode_optional_field(self.agent_id.as_deref())
            ),
            format!(
                "from_generation_id={}",
                encode_optional_field(self.from_generation_id.as_deref())
            ),
            format!(
                "to_generation_id={}",
                encode_optional_field(self.to_generation_id.as_deref())
            ),
            format!(
                "from_release={}",
                encode_optional_field(self.from_release.as_deref())
            ),
            format!(
                "to_release={}",
                encode_optional_field(self.to_release.as_deref())
            ),
            format!(
                "inflight_tasks={}",
                self.inflight_tasks
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "timeout_ms={}",
                self.timeout_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!("created_at_ms={}", self.created_at_ms),
            String::new(),
        ]
        .join("\n")
    }

    /// 从持久化数据或输入构造 `from_storage` 对应的值。
    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut wire_version = None;
        let mut request_id = None;
        let mut trace_id = None;
        let mut operation = None;
        let mut task_id = None;
        let mut task_envelope_kind = None;
        let mut task_envelope_agent_id = None;
        let mut task_input_kind = None;
        let mut task_inline_input_hex = None;
        let mut task_artifact_ref = None;
        let mut task_input_digest = None;
        let mut task_idempotency_key = None;
        let mut task_max_attempts = None;
        let mut task_retry_backoff_ms = None;
        let mut task_attempt_timeout_ms = None;
        let mut reason = None;
        let mut plan_id = None;
        let mut generation_id = None;
        let mut agent_id = None;
        let mut from_generation_id = None;
        let mut to_generation_id = None;
        let mut from_release = None;
        let mut to_release = None;
        let mut inflight_tasks = None;
        let mut timeout_ms = None;
        let mut created_at_ms = None;
        let mut seen = BTreeSet::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon control request is invalid"));
            };
            if !seen.insert(key.to_owned()) {
                return Err(
                    EvaError::conflict("daemon control request has duplicate field")
                        .with_context("field", key),
                );
            }
            match key {
                "version" => {
                    let parsed = value.parse::<u32>().map_err(|_| {
                        EvaError::conflict("daemon control request version is invalid")
                    })?;
                    if !matches!(parsed, 1 | 2) {
                        return Err(
                            EvaError::conflict("daemon control request version mismatch")
                                .with_context("version", value),
                        );
                    }
                    wire_version = Some(parsed);
                }
                "request_id" => request_id = Some(RequestId::parse(value)?),
                "trace_id" => trace_id = Some(decode_field(value)?),
                "operation" => operation = Some(DaemonControlOperation::parse(value)?),
                "task_id" => task_id = decode_optional_field(value)?,
                "task_envelope_kind" => task_envelope_kind = Some(decode_field(value)?),
                "task_envelope_agent_id" => task_envelope_agent_id = Some(decode_field(value)?),
                "task_input_kind" => task_input_kind = Some(value.to_owned()),
                "task_inline_input_hex" => task_inline_input_hex = Some(value.to_owned()),
                "task_artifact_ref" => task_artifact_ref = Some(decode_field(value)?),
                "task_input_digest" => task_input_digest = Some(value.to_owned()),
                "task_idempotency_key" => task_idempotency_key = Some(decode_field(value)?),
                "task_max_attempts" => task_max_attempts = Some(value.to_owned()),
                "task_retry_backoff_ms" => task_retry_backoff_ms = Some(value.to_owned()),
                "task_attempt_timeout_ms" => task_attempt_timeout_ms = Some(value.to_owned()),
                "reason" => reason = decode_optional_field(value)?,
                "plan_id" => plan_id = decode_optional_field(value)?,
                "generation_id" => generation_id = decode_optional_field(value)?,
                "agent_id" => agent_id = decode_optional_field(value)?,
                "from_generation_id" => from_generation_id = decode_optional_field(value)?,
                "to_generation_id" => to_generation_id = decode_optional_field(value)?,
                "from_release" => from_release = decode_optional_field(value)?,
                "to_release" => to_release = decode_optional_field(value)?,
                "inflight_tasks" => {
                    inflight_tasks = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<usize>().map_err(|_| {
                            EvaError::conflict("daemon control request inflight_tasks is invalid")
                        })?)
                    }
                }
                "timeout_ms" => {
                    timeout_ms = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<u64>().map_err(|_| {
                            EvaError::conflict("daemon control request timeout_ms is invalid")
                        })?)
                    }
                }
                "created_at_ms" => {
                    created_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::conflict("daemon control request created_at_ms is invalid")
                    })?)
                }
                _ => {
                    return Err(
                        EvaError::conflict("daemon control request has unknown field")
                            .with_context("field", key),
                    );
                }
            }
        }
        let wire_version = wire_version
            .ok_or_else(|| EvaError::conflict("daemon control request missing version"))?;
        let operation = operation
            .ok_or_else(|| EvaError::conflict("daemon control request missing operation"))?;
        let task_envelope = if wire_version == 1 {
            if task_envelope_kind.is_some()
                || task_envelope_agent_id.is_some()
                || task_input_kind.is_some()
                || task_inline_input_hex.is_some()
                || task_artifact_ref.is_some()
                || task_input_digest.is_some()
                || task_idempotency_key.is_some()
                || task_max_attempts.is_some()
                || task_retry_backoff_ms.is_some()
                || task_attempt_timeout_ms.is_some()
            {
                return Err(EvaError::conflict(
                    "daemon control request v1 cannot contain task envelope fields",
                ));
            }
            None
        } else {
            let kind = required_control_field(task_envelope_kind, "task_envelope_kind")?;
            let envelope_agent_id =
                required_control_field(task_envelope_agent_id, "task_envelope_agent_id")?;
            let input_kind = required_control_field(task_input_kind, "task_input_kind")?;
            let inline_input_hex =
                required_control_field(task_inline_input_hex, "task_inline_input_hex")?;
            let artifact_ref = required_control_field(task_artifact_ref, "task_artifact_ref")?;
            let input_digest = required_control_field(task_input_digest, "task_input_digest")?;
            let idempotency_key =
                required_control_field(task_idempotency_key, "task_idempotency_key")?;
            let max_attempts = required_control_field(task_max_attempts, "task_max_attempts")?;
            let retry_backoff_ms =
                required_control_field(task_retry_backoff_ms, "task_retry_backoff_ms")?;
            let attempt_timeout_ms =
                required_control_field(task_attempt_timeout_ms, "task_attempt_timeout_ms")?;
            let has_envelope = !kind.is_empty()
                || !envelope_agent_id.is_empty()
                || !input_kind.is_empty()
                || !inline_input_hex.is_empty()
                || !artifact_ref.is_empty()
                || !input_digest.is_empty()
                || !idempotency_key.is_empty()
                || !max_attempts.is_empty()
                || !retry_backoff_ms.is_empty()
                || !attempt_timeout_ms.is_empty();
            if !has_envelope {
                None
            } else {
                let input = match input_kind.as_str() {
                    "inline" if artifact_ref.is_empty() => {
                        let input = TaskInput::inline(decode_bytes(
                            &inline_input_hex,
                            "task_inline_input_hex",
                        )?)?;
                        if input.digest() != input_digest {
                            return Err(EvaError::invalid_argument(
                                "daemon task inline input digest mismatch",
                            ));
                        }
                        input
                    }
                    "artifact" if inline_input_hex.is_empty() => {
                        TaskInput::artifact(TaskArtifactRef::new(artifact_ref, input_digest)?)
                    }
                    "inline" | "artifact" => {
                        return Err(EvaError::invalid_argument(
                            "daemon task input fields conflict with discriminator",
                        ))
                    }
                    _ => {
                        return Err(EvaError::invalid_argument(
                            "daemon task input discriminator is unsupported",
                        )
                        .with_context("input_kind", input_kind))
                    }
                };
                let max_attempts = max_attempts.parse::<u32>().map_err(|_| {
                    EvaError::invalid_argument("daemon task max_attempts is invalid")
                })?;
                let retry_backoff_ms = retry_backoff_ms.parse::<u64>().map_err(|_| {
                    EvaError::invalid_argument("daemon task retry_backoff_ms is invalid")
                })?;
                let attempt_timeout_ms = if attempt_timeout_ms.is_empty() {
                    None
                } else {
                    Some(attempt_timeout_ms.parse::<u64>().map_err(|_| {
                        EvaError::invalid_argument("daemon task attempt_timeout_ms is invalid")
                    })?)
                };
                Some(TaskEnvelope::new(
                    TaskKind::parse(&kind)?,
                    AgentId::parse(&envelope_agent_id)?,
                    input,
                    IdempotencyKey::parse(&idempotency_key)?,
                    TaskAttemptPolicy::new(max_attempts, retry_backoff_ms, attempt_timeout_ms)?,
                )?)
            }
        };

        let request = Self {
            wire_version,
            request_id: request_id
                .ok_or_else(|| EvaError::conflict("daemon control request missing request_id"))?,
            trace_id: trace_id
                .ok_or_else(|| EvaError::conflict("daemon control request missing trace_id"))?,
            operation,
            task_id,
            task_envelope,
            reason,
            plan_id,
            generation_id,
            agent_id,
            from_generation_id,
            to_generation_id,
            from_release,
            to_release,
            inflight_tasks,
            timeout_ms,
            created_at_ms: created_at_ms.ok_or_else(|| {
                EvaError::conflict("daemon control request missing created_at_ms")
            })?,
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), EvaError> {
        if !matches!(self.wire_version, 1 | 2) {
            return Err(EvaError::invalid_argument(
                "daemon control request version is unsupported",
            ));
        }
        if let Some(task_id) = &self.task_id {
            RequestId::parse(task_id)?;
        }
        if self.operation == DaemonControlOperation::Shutdown
            && self.timeout_ms.is_some_and(|timeout_ms| {
                !(2..=MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS).contains(&timeout_ms)
            })
        {
            return Err(EvaError::invalid_argument(
                "daemon shutdown drain timeout is outside the live lease budget",
            )
            .with_context("minimum_ms", "2")
            .with_context(
                "maximum_ms",
                MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS.to_string(),
            ));
        }
        match self.operation {
            DaemonControlOperation::SubmitTask
                if self.wire_version == 1 && self.agent_id.is_some() =>
            {
                Err(EvaError::invalid_argument(
                    "daemon control request v1 submit cannot carry a generic agent identity",
                ))
            }
            DaemonControlOperation::SubmitTask
                if self.wire_version >= 2 && self.task_envelope.is_none() =>
            {
                Err(EvaError::invalid_argument(
                    "daemon submit request requires a complete task envelope",
                ))
            }
            DaemonControlOperation::SubmitTask => {
                if let Some(envelope) = &self.task_envelope {
                    envelope.to_snapshot().validate()?;
                    if self.wire_version >= 2 {
                        if let Some(control_agent_id) = self.agent_id.as_deref() {
                            let control_agent_id = AgentId::parse(control_agent_id)?;
                            if &control_agent_id != envelope.agent_id() {
                                return Err(EvaError::conflict(
                                    "daemon submit control agent does not match task envelope agent",
                                )
                                .with_context("control_agent_id", control_agent_id.as_str())
                                .with_context(
                                    "envelope_agent_id",
                                    envelope.agent_id().as_str(),
                                ));
                            }
                        }
                    }
                }
                Ok(())
            }
            _ if self.task_envelope.is_some() => Err(EvaError::invalid_argument(
                "task envelope is only valid for daemon submit",
            )),
            _ => Ok(()),
        }
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonControlResponse {
    /// 按稳定存储格式编码 `to_storage` 对应的数据。
    fn to_storage(&self) -> String {
        let state = self.state.as_ref();
        let lease = self.lease.as_ref();
        let shutdown = self.shutdown.as_ref();
        format!(
            "version=2\nrequest_id={}\ntrace_id={}\noperation={}\naccepted={}\ndaemon_available={}\nstatus={}\nmutation_executed={}\nrequest_file={}\nresponse_file={}\nstate_status={}\nstate_mode={}\nstate_pid={}\nstate_generation_id={}\nstate_project_root={}\nstate_started_at_ms={}\nstate_stopped_at_ms={}\nlease_state={}\nlease_pid={}\nlease_process_start_token={}\nlease_generation={}\nlease_heartbeat_at_ms={}\nlease_expires_at_ms={}\nlease_owner_live={}\nlease_expired={}\ntask_id={}\nplan_id={}\ngeneration_id={}\nmessage={}\nshutdown_already_shutdown={}\nshutdown_request_count={}\nshutdown_phase={}\naudit={}\n",
            self.request_id.as_str(),
            encode_field(&self.trace_id),
            self.operation.as_str(),
            self.accepted,
            self.daemon_available,
            encode_field(&self.status),
            self.mutation_executed,
            encode_field(&self.request_file),
            encode_field(&self.response_file),
            encode_optional_field(state.map(|value| value.status.as_str())),
            encode_optional_field(state.map(|value| value.mode.as_str())),
            state
                .map(|value| value.pid.to_string())
                .unwrap_or_default(),
            encode_optional_field(state.map(|value| value.generation_id.as_str())),
            encode_optional_field(state.map(|value| value.project_root.as_str())),
            state
                .map(|value| value.started_at_ms.to_string())
                .unwrap_or_default(),
            state
                .and_then(|value| value.stopped_at_ms)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            encode_optional_field(lease.map(|value| value.state.as_str())),
            lease.map(|value| value.pid.to_string()).unwrap_or_default(),
            encode_optional_field(lease.map(|value| value.process_start_token.as_str())),
            lease
                .map(|value| value.generation.to_string())
                .unwrap_or_default(),
            lease
                .map(|value| value.heartbeat_at_ms.to_string())
                .unwrap_or_default(),
            lease
                .map(|value| value.expires_at_ms.to_string())
                .unwrap_or_default(),
            lease
                .map(|value| value.owner_live.to_string())
                .unwrap_or_default(),
            lease
                .map(|value| value.expired.to_string())
                .unwrap_or_default(),
            encode_optional_field(self.task_id.as_deref()),
            encode_optional_field(self.plan_id.as_deref()),
            encode_optional_field(self.generation_id.as_deref()),
            encode_field(&self.message),
            shutdown
                .map(|value| value.already_shutdown.to_string())
                .unwrap_or_default(),
            shutdown
                .map(|value| value.request_count.to_string())
                .unwrap_or_default(),
            encode_optional_field(shutdown.map(|value| value.phase.as_str())),
            encode_audit(&self.audit)
        )
    }

    /// 从持久化数据或输入构造 `from_storage` 对应的值。
    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut wire_version = None;
        let mut request_id = None;
        let mut trace_id = None;
        let mut operation = None;
        let mut accepted = None;
        let mut daemon_available = None;
        let mut status = None;
        let mut mutation_executed = None;
        let mut request_file = None;
        let mut response_file = None;
        let mut state_status = None;
        let mut state_mode = None;
        let mut state_pid = None;
        let mut state_generation_id = None;
        let mut state_project_root = None;
        let mut state_started_at_ms = None;
        let mut state_stopped_at_ms = None;
        let mut lease_state = None;
        let mut lease_pid = None;
        let mut lease_process_start_token = None;
        let mut lease_generation = None;
        let mut lease_heartbeat_at_ms = None;
        let mut lease_expires_at_ms = None;
        let mut lease_owner_live = None;
        let mut lease_expired = None;
        let mut task_id = None;
        let mut plan_id = None;
        let mut generation_id = None;
        let mut message = None;
        let mut shutdown_already_shutdown = None;
        let mut shutdown_request_count = None;
        let mut shutdown_phase = None;
        let mut audit = Vec::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon control response is invalid"));
            };
            match key {
                "version" => {
                    if !matches!(value, "1" | "2") {
                        return Err(
                            EvaError::conflict("daemon control response version mismatch")
                                .with_context("version", value),
                        );
                    }
                    wire_version = Some(value.parse::<u32>().expect("validated literal"));
                }
                "request_id" => request_id = Some(RequestId::parse(value)?),
                "trace_id" => trace_id = Some(decode_field(value)?),
                "operation" => operation = Some(DaemonControlOperation::parse(value)?),
                "accepted" => accepted = Some(parse_bool(value, "accepted")?),
                "daemon_available" => {
                    daemon_available = Some(parse_bool(value, "daemon_available")?)
                }
                "status" => status = Some(decode_field(value)?),
                "mutation_executed" => {
                    mutation_executed = Some(parse_bool(value, "mutation_executed")?)
                }
                "request_file" => request_file = Some(decode_field(value)?),
                "response_file" => response_file = Some(decode_field(value)?),
                "state_status" => state_status = decode_optional_field(value)?,
                "state_mode" => state_mode = decode_optional_field(value)?,
                "state_pid" => {
                    state_pid = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<u32>().map_err(|_| {
                            EvaError::conflict("daemon control response state_pid is invalid")
                        })?)
                    }
                }
                "state_generation_id" => state_generation_id = decode_optional_field(value)?,
                "state_project_root" => state_project_root = decode_optional_field(value)?,
                "state_started_at_ms" => {
                    state_started_at_ms = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<u128>().map_err(|_| {
                            EvaError::conflict(
                                "daemon control response state_started_at_ms is invalid",
                            )
                        })?)
                    }
                }
                "state_stopped_at_ms" => {
                    state_stopped_at_ms = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<u128>().map_err(|_| {
                            EvaError::conflict(
                                "daemon control response state_stopped_at_ms is invalid",
                            )
                        })?)
                    }
                }
                "lease_state" => lease_state = decode_optional_field(value)?,
                "lease_pid" => lease_pid = parse_optional_u32(value, "lease_pid")?,
                "lease_process_start_token" => {
                    lease_process_start_token = decode_optional_field(value)?
                }
                "lease_generation" => {
                    lease_generation = parse_optional_u64(value, "lease_generation")?
                }
                "lease_heartbeat_at_ms" => {
                    lease_heartbeat_at_ms = parse_optional_u128(value, "lease_heartbeat_at_ms")?
                }
                "lease_expires_at_ms" => {
                    lease_expires_at_ms = parse_optional_u128(value, "lease_expires_at_ms")?
                }
                "lease_owner_live" => {
                    lease_owner_live = if value.is_empty() {
                        None
                    } else {
                        Some(parse_bool(value, "lease_owner_live")?)
                    }
                }
                "lease_expired" => {
                    lease_expired = if value.is_empty() {
                        None
                    } else {
                        Some(parse_bool(value, "lease_expired")?)
                    }
                }
                "task_id" => task_id = decode_optional_field(value)?,
                "plan_id" => plan_id = decode_optional_field(value)?,
                "generation_id" => generation_id = decode_optional_field(value)?,
                "message" => message = Some(decode_field(value)?),
                "shutdown_already_shutdown" => {
                    shutdown_already_shutdown = if value.is_empty() {
                        None
                    } else {
                        Some(parse_bool(value, "shutdown_already_shutdown")?)
                    }
                }
                "shutdown_request_count" => {
                    shutdown_request_count = if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<u64>().map_err(|_| {
                            EvaError::conflict(
                                "daemon control response shutdown_request_count is invalid",
                            )
                        })?)
                    }
                }
                "shutdown_phase" => shutdown_phase = decode_optional_field(value)?,
                "audit" => audit = decode_audit(value)?,
                _ => {
                    return Err(
                        EvaError::conflict("daemon control response has unknown field")
                            .with_context("field", key),
                    );
                }
            }
        }

        let state = match (
            state_status,
            state_mode,
            state_pid,
            state_generation_id,
            state_project_root,
            state_started_at_ms,
        ) {
            (
                Some(status),
                Some(mode),
                Some(pid),
                Some(generation_id),
                Some(project_root),
                Some(started_at_ms),
            ) => Some(DaemonStateRecord {
                status,
                mode,
                pid,
                generation_id,
                project_root,
                started_at_ms,
                stopped_at_ms: state_stopped_at_ms,
            }),
            _ => None,
        };
        let shutdown = match (
            shutdown_already_shutdown,
            shutdown_request_count,
            shutdown_phase,
        ) {
            (Some(already_shutdown), Some(request_count), Some(phase)) => Some(ShutdownReport {
                already_shutdown,
                request_count,
                phase,
            }),
            _ => None,
        };
        let lease = match (
            lease_state,
            lease_pid,
            lease_process_start_token,
            lease_generation,
            lease_heartbeat_at_ms,
            lease_expires_at_ms,
            lease_owner_live,
            lease_expired,
        ) {
            (
                Some(state),
                Some(pid),
                Some(process_start_token),
                Some(generation),
                Some(heartbeat_at_ms),
                Some(expires_at_ms),
                Some(owner_live),
                Some(expired),
            ) => Some(DaemonLeaseReport {
                state,
                pid,
                process_start_token,
                generation,
                heartbeat_at_ms,
                expires_at_ms,
                owner_live,
                expired,
            }),
            (None, None, None, None, None, None, None, None) => None,
            _ => {
                return Err(EvaError::conflict(
                    "daemon control response lease projection is incomplete",
                ))
            }
        };

        let wire_version = wire_version
            .ok_or_else(|| EvaError::conflict("daemon control response missing version"))?;
        if wire_version == 1 && lease.is_some() {
            return Err(EvaError::conflict(
                "daemon control response v1 cannot contain a lease projection",
            ));
        }
        if wire_version == 2 && lease.is_none() {
            return Err(EvaError::conflict(
                "daemon control response v2 requires a lease projection",
            ));
        }
        if let Some(lease) = lease.as_ref() {
            validate_daemon_lease_report(lease)?;
        }

        Ok(Self {
            request_id: request_id
                .ok_or_else(|| EvaError::conflict("daemon control response missing request_id"))?,
            trace_id: trace_id
                .ok_or_else(|| EvaError::conflict("daemon control response missing trace_id"))?,
            operation: operation
                .ok_or_else(|| EvaError::conflict("daemon control response missing operation"))?,
            accepted: accepted
                .ok_or_else(|| EvaError::conflict("daemon control response missing accepted"))?,
            daemon_available: daemon_available.ok_or_else(|| {
                EvaError::conflict("daemon control response missing daemon_available")
            })?,
            status: status
                .ok_or_else(|| EvaError::conflict("daemon control response missing status"))?,
            mutation_executed: mutation_executed.ok_or_else(|| {
                EvaError::conflict("daemon control response missing mutation_executed")
            })?,
            request_file: request_file.ok_or_else(|| {
                EvaError::conflict("daemon control response missing request_file")
            })?,
            response_file: response_file.ok_or_else(|| {
                EvaError::conflict("daemon control response missing response_file")
            })?,
            state,
            lease,
            task_id,
            plan_id,
            generation_id,
            message: message
                .ok_or_else(|| EvaError::conflict("daemon control response missing message"))?,
            shutdown,
            audit,
        })
    }
}

fn validate_daemon_lease_report(lease: &DaemonLeaseReport) -> Result<(), EvaError> {
    if lease.pid == 0
        || lease.generation == 0
        || lease.process_start_token.is_empty()
        || !matches!(lease.state.as_str(), "active" | "released")
    {
        return Err(EvaError::conflict(
            "daemon control response lease identity is invalid",
        ));
    }
    match lease.state.as_str() {
        "active" if lease.expires_at_ms > lease.heartbeat_at_ms => Ok(()),
        "released"
            if lease.expires_at_ms == lease.heartbeat_at_ms
                && !lease.owner_live
                && !lease.expired =>
        {
            Ok(())
        }
        _ => Err(EvaError::conflict(
            "daemon control response lease lifecycle is invalid",
        )),
    }
}

/// 按锁、恢复、验证、状态发布、子系统启动的顺序启动前台守护进程。
///
/// 锁在任何状态文件写入前取得，冲突时不会触碰 PID 或 running 状态。锁之后发生错误会通过
/// RAII 尝试释放锁；若 PID 或状态已经写入，可用性检查仍会因锁缺失而报告不可用。持久化
/// 恢复完成前绝不发布 running 状态。
pub fn start_daemon(
    project: &ProjectConfig,
    options: DaemonStartOptions,
    trace: &TraceFields,
) -> Result<DaemonStartReport, EvaError> {
    if !options.foreground {
        return Err(EvaError::unsupported(
            "background daemon spawning must use the CLI parent/child entrypoint",
        ));
    }
    start_daemon_inner(project, options, trace, DaemonRunMode::Foreground, None)
}

pub fn start_daemon_background_child(
    project: &ProjectConfig,
    mut options: DaemonStartOptions,
    trace: &TraceFields,
    handshake: &DaemonStartupHandshake,
    publish_report: &mut dyn FnMut(&DaemonStartReport) -> Result<String, EvaError>,
) -> Result<DaemonStartReport, EvaError> {
    options.foreground = true;
    options.shutdown_after_smoke = false;
    let mut hooks = DaemonStartupHooks {
        handshake,
        publish_report,
        ready_published: false,
    };
    let result = start_daemon_inner(
        project,
        options.clone(),
        trace,
        DaemonRunMode::BackgroundChild,
        Some(&mut hooks),
    );
    if let Err(error) = &result {
        if !hooks.ready_published {
            let claimed =
                read_daemon_startup_frame(&options, handshake, DaemonStartupPhase::Claimed)
                    .ok()
                    .flatten();
            let _ = remove_if_exists(&daemon_startup_report_path(&options, handshake));
            let cleanup_complete = daemon_startup_cleanup_complete(&options).unwrap_or(false);
            let failed = DaemonStartupFrame::failed(
                handshake,
                std::process::id(),
                claimed.as_ref(),
                error,
                cleanup_complete,
            );
            if let Err(frame_error) =
                write_daemon_startup_failure_frame(&options, handshake, &failed)
            {
                return Err(error
                    .clone()
                    .with_context("startup_failure_frame_error", frame_error.to_string()));
            }
        }
    }
    result
}

fn start_daemon_inner(
    project: &ProjectConfig,
    options: DaemonStartOptions,
    trace: &TraceFields,
    run_mode: DaemonRunMode,
    mut startup_hooks: Option<&mut DaemonStartupHooks<'_>>,
) -> Result<DaemonStartReport, EvaError> {
    fs::create_dir_all(&options.lock_dir).map_err(|error| {
        EvaError::internal("failed to create daemon lock directory")
            .with_context("path", options.lock_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let observed_probe = probe_runtime_lease(lock_file(&options), lease_file(&options), now_ms())?;
    if observed_probe.owner_live() {
        return Err(EvaError::conflict(
            "daemon start refused because the lease owner is still live",
        )
        .with_context("anchor_path", lock_file(&options).display().to_string())
        .with_context(
            "generation",
            observed_probe
                .record()
                .map(|record| record.generation().0.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
        ));
    }
    if let Some(record) = observed_probe.record().filter(|record| {
        record.state() == DurableRuntimeLeaseState::Active && !observed_probe.expired()
    }) {
        return Err(EvaError::conflict(
            "daemon start refused until the dead owner's lease expires",
        )
        .with_context("pid", record.pid().to_string())
        .with_context("generation", record.generation().0.to_string())
        .with_context("expires_at_ms", record.expires_at_ms().to_string()));
    }
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let durable_report = durable_backend.verify()?;
    let mut observability_lifecycle =
        RuntimeObservabilityLifecycle::start(&options.observability_backend);
    // 固定 anchor 必须先于 durable writer 取得；guard 同时持有两层 ownership 到 daemon 退出。
    let lease_ttl_ms = daemon_runtime_lease_ttl_ms(&options)?;
    let mut lease = if let Some(handshake) = startup_hooks.as_deref().map(|hooks| hooks.handshake) {
        DurableRuntimeLeaseGuard::acquire_with_process_start_token(
            &durable_backend,
            lock_file(&options),
            lease_file(&options),
            handshake.child_start_token(),
            now_ms(),
            lease_ttl_ms,
        )?
    } else {
        DurableRuntimeLeaseGuard::acquire(
            &durable_backend,
            lock_file(&options),
            lease_file(&options),
            now_ms(),
            lease_ttl_ms,
        )?
    };
    delay_after_startup_lease_for_test()?;
    if let Some(hooks) = startup_hooks.as_deref_mut() {
        write_daemon_startup_frame(
            &options,
            hooks.handshake,
            &DaemonStartupFrame::claimed(hooks.handshake, lease.record()),
        )?;
    }
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;
    let mut task_handlers = TaskHandlerRegistry::with_runtime_defaults()?;
    register_daemon_process_harness_handlers(&options, &mut task_handlers)?;
    let task_handlers = Arc::new(task_handlers);
    let mut task_store =
        FileSystemTaskStateStore::from_runtime_writer(durable_backend.layout(), lease.writer())?;
    let mut effect_ledger =
        FileSystemEffectLedger::open_with_writer(durable_backend.layout(), lease.writer())?;
    let mut provider_process_table = FileSystemProviderProcessTable::from_runtime_writer(
        durable_backend.layout(),
        lease.writer(),
    )?;
    // Handler/effect facts must classify abandoned tasks before provider recovery or worker startup.
    let recovery = RuntimeRecoveryCoordinator
        .recover_task_store_with_effects_and_provider_processes(
            &mut task_store,
            task_handlers.as_ref(),
            &mut effect_ledger,
            &mut provider_process_table,
        )?;
    drop(task_store);
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;
    record_daemon_recovery_observability(observability_lifecycle.pipeline_mut(), trace, &recovery);
    let policy = verify_policy(project)?;
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;
    let observability = verify_observability(&mut observability_lifecycle, trace)?;
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;

    fs::create_dir_all(&options.state_dir).map_err(|error| {
        EvaError::internal("failed to create daemon state directory")
            .with_context("path", options.state_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    fs::create_dir_all(&options.pid_dir).map_err(|error| {
        EvaError::internal("failed to create daemon pid directory")
            .with_context("path", options.pid_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ensure_control_dirs(&options)?;
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;
    let hardware_hotplug = start_hardware_hotplug_subscriber(
        project,
        &options,
        observability_lifecycle.pipeline_mut(),
    )?;
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;
    let memory_schedule = FileSystemScheduleStore::new(options.state_dir.join("schedules"))?;
    if memory_schedule
        .read(MEMORY_MAINTENANCE_SCHEDULE_ID)
        .is_err()
    {
        memory_schedule.upsert(MEMORY_MAINTENANCE_SCHEDULE_ID, 0)?;
    }
    let memory_schedule_owner = schedule_owner(&lease);
    let retrieval_worker =
        DaemonRetrievalWorker::from_project(project, provider_process_table.clone())?;
    ensure_retrieval_schedule(&memory_schedule, retrieval_worker.as_ref())?;
    let memory_maintenance = run_scheduled_memory_maintenance(
        &memory_schedule,
        &memory_schedule_owner,
        &options,
        observability_lifecycle.pipeline_mut(),
        trace,
    )?;
    let _retrieval = run_scheduled_retrieval(
        &memory_schedule,
        &memory_schedule_owner,
        retrieval_worker.as_ref(),
        now_ms(),
    )?;
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;
    let mut runtime = RuntimeBuilder::new().build(project)?;
    let config_generation_store = ConfigGenerationStore::new(durable_backend.layout());
    let recovered_generation =
        config_generation_store.initialize(&runtime.generation().identity)?;
    if recovered_generation.active.digest != runtime.generation().identity.digest {
        return Err(EvaError::conflict(
            "durable active config digest does not match loaded project",
        )
        .with_context("durable_digest", recovered_generation.active.digest)
        .with_context("loaded_digest", &runtime.generation().identity.digest));
    }
    let mut config_watcher = if project.eva.runtime.hot_reload && !options.shutdown_after_smoke {
        Some(ConfigWatcher::start(
            &project.project_root,
            Duration::from_millis(100),
            Duration::from_millis(250),
        )?)
    } else {
        None
    };
    let task_artifacts = Arc::new(FileSystemTaskArtifactResolver::with_default_limit(
        &durable_backend.layout().artifact_dir,
    ));
    let mut task_worker = if options.shutdown_after_smoke {
        None
    } else {
        let task_store = FileSystemTaskStateStore::from_runtime_writer(
            durable_backend.layout(),
            lease.writer(),
        )?;
        let task_failure_bus =
            DurableEventBus::open_with_writer(durable_backend.layout(), lease.writer())?;
        Some(TaskWorkerRuntime::start_paused_with_durable_services(
            task_store,
            Arc::clone(&task_handlers),
            task_artifacts.clone(),
            task_worker_execution_owner(lease.record()),
            task_failure_bus,
            effect_ledger,
        )?)
    };
    if let Some(worker) = task_worker.as_ref() {
        worker.check_health()?;
    }
    // All startup work must finish before publishing ready state, and the lease is renewed at that boundary.
    lease.renew_at(now_ms())?;
    ensure_startup_not_aborted(
        &options,
        startup_hooks.as_deref().map(|hooks| hooks.handshake),
    )?;

    // 状态和 PID 均成功写入后，`daemon_status` 才可能将进程判为可用。
    let running_state = DaemonStateRecord::running(project, run_mode);
    write_state(&options, &running_state)?;
    if let Err(error) = write_pid_projection(&options, lease.record()) {
        if let Some(watcher) = config_watcher.as_mut() {
            let _ = watcher.stop_and_join();
        }
        if let Some(worker) = task_worker.as_mut() {
            let _ = worker.stop_and_join();
        }
        let _ = write_state(&options, &running_state.clone().stopped());
        let _ = lease.release_at(now_ms());
        return Err(error);
    }

    let lifecycle = (|| {
        if let Some(hooks) = startup_hooks.as_deref_mut() {
            let ready_report = DaemonStartReport {
                status: "running".to_owned(),
                mode: run_mode.as_str().to_owned(),
                pid: running_state.pid,
                generation_id: DAEMON_GENERATION.to_owned(),
                project_root: display_path(&project.project_root),
                foreground: run_mode.foreground(),
                dev_mode: options.dev_mode,
                provider_processes_started: false,
                paths: DaemonPathReport::from_options(&options),
                lease: DaemonLeaseReport::from_guard(&lease, now_ms()),
                durable_backend: durable_report.clone(),
                recovery: recovery.clone(),
                policy: policy.clone(),
                observability: observability.clone(),
                hardware_hotplug: hardware_hotplug.clone(),
                memory_maintenance: memory_maintenance.clone(),
                shutdown: None,
                audit: daemon_start_audit(
                    task_handlers.as_ref(),
                    task_artifacts.as_ref(),
                    task_worker.is_some(),
                ),
            };
            let report_digest = (hooks.publish_report)(&ready_report)?;
            ensure_startup_not_aborted(&options, Some(hooks.handshake))?;
            write_daemon_startup_frame(
                &options,
                hooks.handshake,
                &DaemonStartupFrame::ready(hooks.handshake, lease.record(), report_digest),
            )?;
            hooks.ready_published = true;
        }
        if let Some(worker) = task_worker.as_ref() {
            worker.activate();
        }
        let (status, shutdown) = if options.shutdown_after_smoke {
            let shutdown_report = runtime.shutdown();
            let stopped = running_state.clone().stopped();
            write_state(&options, &stopped)?;
            remove_matching_pid(&options, lease.record())?;
            ("stopped".to_owned(), Some(shutdown_report))
        } else {
            let worker = task_worker
                .as_mut()
                .ok_or_else(|| EvaError::internal("daemon task worker was not created"))?;
            let loop_result = run_control_loop(DaemonControlLoopContext {
                options: &options,
                runtime: &mut runtime,
                running_state: running_state.clone(),
                durable_layout: durable_backend.layout(),
                lease: &mut lease,
                task_worker: worker,
                observability: observability_lifecycle.pipeline_mut(),
                memory_schedule: &memory_schedule,
                memory_schedule_owner: &memory_schedule_owner,
                retrieval_worker: retrieval_worker.as_ref(),
                config_watcher: config_watcher.as_ref(),
                config_generation_store: &config_generation_store,
            });
            let join_result = worker.stop_and_join();
            let loop_report = match (loop_result, join_result) {
                (Ok(report), Ok(())) => report,
                (Err(error), Ok(())) => return Err(error),
                (Ok(_), Err(error)) => return Err(error),
                (Err(error), Err(join_error)) => {
                    return Err(error.with_context("task_worker_join_error", join_error.to_string()))
                }
            };
            (loop_report.status, loop_report.shutdown)
        };
        Ok::<_, EvaError>((status, shutdown))
    })();

    let (status, shutdown) = match lifecycle {
        Ok(report) => report,
        Err(mut error) => {
            if let Some(worker) = task_worker.as_mut() {
                if let Err(join_error) = worker.stop_and_join() {
                    error = error.with_context("task_worker_join_error", join_error.to_string());
                }
            }
            if let Some(watcher) = config_watcher.as_mut() {
                if let Err(join_error) = watcher.stop_and_join() {
                    error = error.with_context("config_watcher_join_error", join_error.to_string());
                }
            }
            let _ = write_state(&options, &running_state.clone().stopped());
            let _ = remove_matching_pid(&options, lease.record());
            if let Err(shutdown_error) = observability_lifecycle.shutdown(trace) {
                error =
                    error.with_context("observability_shutdown_error", shutdown_error.to_string());
            }
            let _ = lease.release_at(now_ms());
            return Err(error);
        }
    };
    if let Some(watcher) = config_watcher.as_mut() {
        watcher.stop_and_join()?;
    }
    observability_lifecycle.shutdown(trace)?;
    let released = lease.release_at(now_ms())?.clone();
    let lease_report = DaemonLeaseReport::from_record(&released, false, false);

    Ok(DaemonStartReport {
        status,
        mode: run_mode.as_str().to_owned(),
        pid: running_state.pid,
        generation_id: DAEMON_GENERATION.to_owned(),
        project_root: display_path(&project.project_root),
        foreground: run_mode.foreground(),
        dev_mode: options.dev_mode,
        provider_processes_started: false,
        paths: DaemonPathReport::from_options(&options),
        lease: lease_report,
        durable_backend: durable_report,
        recovery,
        policy,
        observability,
        hardware_hotplug,
        memory_maintenance,
        shutdown,
        audit: daemon_start_audit(
            task_handlers.as_ref(),
            task_artifacts.as_ref(),
            task_worker.is_some(),
        ),
    })
}

fn daemon_start_audit(
    task_handlers: &TaskHandlerRegistry,
    task_artifacts: &FileSystemTaskArtifactResolver,
    task_worker_enabled: bool,
) -> Vec<String> {
    let mut audit = vec![
        "daemon:v1.12.1:lock_acquired".to_owned(),
        "daemon:v1.12.1:durable_backend_verified".to_owned(),
        "daemon:v1.12.1:policy_verified".to_owned(),
        "daemon:v1.12.1:observability_verified".to_owned(),
        "daemon:w8-l07:observability_lifecycle_owned_until_shutdown".to_owned(),
        "daemon:v1.12.1:provider_processes_not_started".to_owned(),
        "daemon:v1.12.2:control_mailbox_ready".to_owned(),
        "daemon:v1.12.4:scheduler_retry_tick_ready".to_owned(),
        "daemon:v1.13.5:provider_recovery_scanned".to_owned(),
        "daemon:v1.13.5:provider_orphan_scan_completed".to_owned(),
        "daemon:v1.15.4:hardware_hotplug_subscriber_ready".to_owned(),
        "daemon:v1.15.6:memory_maintenance_ready".to_owned(),
        "daemon:w1-l10:effect_aware_recovery_ready".to_owned(),
        "daemon:w1-l11:bounded_shutdown_drain_ready".to_owned(),
        format!(
            "daemon:w1-l05:task_handler_registry_ready:{}",
            task_handlers.registered_kinds().join(",")
        ),
        format!(
            "daemon:w1-l05:task_artifact_input_limit_bytes:{}",
            task_artifacts.max_size_bytes()
        ),
    ];
    if task_worker_enabled {
        audit.push("daemon:w1-l06:task_worker_claim_gate_ready".to_owned());
        audit.push("daemon:w1-l08:owned_replay_delivery_ready".to_owned());
        audit.push("daemon:w1-l09:durable_effect_ledger_ready".to_owned());
    }
    audit
}

fn task_worker_execution_owner(lease: &DurableRuntimeLeaseRecord) -> String {
    let identity = format!(
        "{}:{}:{}",
        lease.pid(),
        lease.process_start_token(),
        lease.generation().0
    );
    format!(
        "daemon:g{}:worker-0:{}",
        lease.generation().0,
        sha256_digest(identity.as_bytes())
    )
}

/// 只有 active/fresh lease、live OS-lock owner、PID projection 与 running state 完全一致时可用。
pub fn daemon_status(options: &DaemonStartOptions) -> Result<DaemonStatusReport, EvaError> {
    let paths = DaemonPathReport::from_options(options);
    let lock_present = lock_file(options).exists();
    let pid = read_pid_projection(options)?;
    let pid_present = pid.is_some();
    let state = read_state(options)?;
    let observed_now_ms = now_ms();
    let probe = probe_runtime_lease(lock_file(options), lease_file(options), observed_now_ms)?;
    let lease = probe
        .record()
        .map(|record| DaemonLeaseReport::from_record(record, probe.owner_live(), probe.expired()));
    let status = state
        .as_ref()
        .map(|record| record.status.clone())
        .unwrap_or_else(|| "unavailable".to_owned());
    let running = state
        .as_ref()
        .map(|record| record.status == "running")
        .unwrap_or(false);
    let pid_matches_lease = match (pid.as_ref(), state.as_ref(), probe.record()) {
        (Some(pid), Some(state), Some(lease)) => pid.pid() == state.pid && pid.matches_lease(lease),
        _ => false,
    };
    let lease_available = lease
        .as_ref()
        .map(|lease| lease.state == "active" && lease.owner_live && !lease.expired)
        .unwrap_or(false);
    let freshness = lease
        .as_ref()
        .map(|lease| lease.freshness_at(observed_now_ms).as_str().to_owned())
        .unwrap_or_else(|| "stale".to_owned());
    let heartbeat_age_ms = lease
        .as_ref()
        .map(|lease| lease.heartbeat_age_ms(observed_now_ms));
    Ok(DaemonStatusReport {
        available: running && lease_available && pid_matches_lease,
        status,
        lock_present,
        pid_present,
        pid_matches_lease,
        freshness,
        heartbeat_age_ms,
        lease,
        paths,
        state,
    })
}

/// 原子投递请求并轮询关联响应。
///
/// 发送前会删除同请求标识的旧响应，因此复用 `request_id` 表示一次新的执行，而不是读取旧
/// 结果。等待超时不会删除请求文件，守护循环可能稍后执行该操作；调用方不得把超时解释为
/// “未发生变更”，应使用状态查询或原请求标识核对结果。
pub fn send_daemon_control_request(
    options: &DaemonStartOptions,
    request: DaemonControlRequest,
    timeout_ms: u64,
) -> Result<DaemonControlResponse, EvaError> {
    request.validate()?;
    let status = daemon_status(options)?;
    if !status.available {
        if let Some(response) = repeated_shutdown_response(options, &request, &status)? {
            return Ok(response);
        }
        return Err(EvaError::unavailable("daemon control API is unavailable")
            .with_context("operation", request.operation.as_str())
            .with_context("request_id", request.request_id.as_str())
            .with_context("trace_id", &request.trace_id)
            .with_context("state_status", &status.status)
            .with_context("lock_present", status.lock_present.to_string())
            .with_context("pid_present", status.pid_present.to_string())
            .with_context("pid_matches_lease", status.pid_matches_lease.to_string())
            .with_context(
                "lease_owner_live",
                status
                    .lease
                    .as_ref()
                    .map(|lease| lease.owner_live)
                    .unwrap_or(false)
                    .to_string(),
            )
            .with_context(
                "lease_expired",
                status
                    .lease
                    .as_ref()
                    .map(|lease| lease.expired)
                    .unwrap_or(false)
                    .to_string(),
            )
            .with_context("lease_freshness", &status.freshness)
            .with_context(
                "lease_heartbeat_age_ms",
                status
                    .heartbeat_age_ms
                    .map(|age| age.to_string())
                    .unwrap_or_else(|| "none".to_owned()),
            )
            .with_context(
                "suggestion",
                "start a foreground daemon with --no-shutdown-after-smoke, then retry the control command",
            ));
    }

    ensure_control_dirs(options)?;
    let request_path = control_request_file(options, &request.request_id);
    let response_path = control_response_file(options, &request.request_id);
    // 清除旧响应后再发布请求，使同一标识的显式重发不会误读历史结果。
    remove_if_exists(&response_path)?;
    write_control_request(&request_path, &request)?;

    let started_at = now_ms();
    loop {
        if response_path.exists() {
            let data = fs::read_to_string(&response_path).map_err(|error| {
                EvaError::internal("failed to read daemon control response")
                    .with_context("path", response_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            let mut response = DaemonControlResponse::from_storage(&data)?;
            if response.operation != DaemonControlOperation::Shutdown {
                return Ok(response);
            }
            let released_status = daemon_status(options)?;
            if shutdown_is_fully_released(options, &released_status)? {
                response.lease = released_status.lease;
                return Ok(response);
            }
        }
        if now_ms().saturating_sub(started_at) >= timeout_ms as u128 {
            return Err(EvaError::timeout("daemon control response timed out")
                .with_context("operation", request.operation.as_str())
                .with_context("request_id", request.request_id.as_str())
                .with_context("trace_id", &request.trace_id)
                .with_context("response_file", response_path.display().to_string()));
        }
        thread::sleep(Duration::from_millis(CONTROL_POLL_INTERVAL_MS));
    }
}

fn shutdown_is_fully_released(
    options: &DaemonStartOptions,
    status: &DaemonStatusReport,
) -> Result<bool, EvaError> {
    let projections_released = status.status == "stopped"
        && !status.pid_present
        && status
            .lease
            .as_ref()
            .is_some_and(|lease| lease.state == "released" && !lease.owner_live && !lease.expired);
    if !projections_released {
        return Ok(false);
    }
    Ok(read_shutdown_drain_evidence(options)?
        .is_some_and(|evidence| evidence.matches_status(status)))
}

fn repeated_shutdown_response(
    options: &DaemonStartOptions,
    request: &DaemonControlRequest,
    status: &DaemonStatusReport,
) -> Result<Option<DaemonControlResponse>, EvaError> {
    if request.operation != DaemonControlOperation::Shutdown
        || !shutdown_is_fully_released(options, status)?
    {
        return Ok(None);
    }
    Ok(Some(DaemonControlResponse {
        request_id: request.request_id.clone(),
        trace_id: request.trace_id.clone(),
        operation: request.operation,
        accepted: true,
        daemon_available: false,
        status: "stopped".to_owned(),
        mutation_executed: false,
        request_file: display_path(&control_request_file(options, &request.request_id)),
        response_file: display_path(&control_response_file(options, &request.request_id)),
        state: status.state.clone(),
        lease: status.lease.clone(),
        task_id: None,
        plan_id: None,
        generation_id: None,
        message: "daemon shutdown was already fully drained and released".to_owned(),
        shutdown: Some(ShutdownReport {
            already_shutdown: true,
            request_count: 2,
            phase: "already_shutdown".to_owned(),
        }),
        audit: vec!["daemon:w1-l11:shutdown_idempotent_noop".to_owned()],
    }))
}

/// 仅在能够安全 claim ownership 时清理守护投影；活动或 dead-but-unexpired owner 均拒绝。
pub fn stop_daemon(options: &DaemonStartOptions) -> Result<DaemonStopReport, EvaError> {
    let paths = DaemonPathReport::from_options(options);
    let observed_state = read_state(options)?;
    let observed_pid = read_pid_projection(options)?;
    let observed_probe = probe_runtime_lease(lock_file(options), lease_file(options), now_ms())?;
    let observed_record = observed_probe.record().cloned();
    if let Some(pid) = observed_pid.as_ref() {
        let previous = observed_record.as_ref().ok_or_else(|| {
            EvaError::conflict("daemon pid projection has no matching historical lease")
                .with_context("pid", pid.pid().to_string())
        })?;
        if !pid.matches_lease(previous) {
            return Err(EvaError::conflict(
                "daemon pid projection belongs to another historical lease",
            )
            .with_context("pid", pid.pid().to_string())
            .with_context("expected_generation", previous.generation().0.to_string()));
        }
        if observed_state
            .as_ref()
            .is_some_and(|state| state.pid != pid.pid())
        {
            return Err(EvaError::conflict(
                "daemon state and pid projection belong to different owners",
            )
            .with_context("pid_file_pid", pid.pid().to_string()));
        }
    }
    let observed_lease = observed_probe.record().map(|record| {
        DaemonLeaseReport::from_record(
            record,
            observed_probe.owner_live(),
            observed_probe.expired(),
        )
    });
    if observed_probe.owner_live() {
        return Err(EvaError::conflict(
            "daemon stop refused because the lease owner is still live",
        )
        .with_context("anchor_path", lock_file(options).display().to_string())
        .with_context(
            "generation",
            observed_record
                .as_ref()
                .map(|record| record.generation().0.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
        ));
    }
    if let Some(record) = observed_record.as_ref().filter(|record| {
        record.state() == DurableRuntimeLeaseState::Active && !observed_probe.expired()
    }) {
        return Err(
            EvaError::conflict("daemon stop refused until the dead owner's lease expires")
                .with_context("pid", record.pid().to_string())
                .with_context("generation", record.generation().0.to_string())
                .with_context("expires_at_ms", record.expires_at_ms().to_string()),
        );
    }
    let already_stopped = observed_state
        .as_ref()
        .map(|state| state.status == "stopped")
        .unwrap_or(true);
    let lease_inactive = observed_lease
        .as_ref()
        .map(|lease| lease.state == "released")
        .unwrap_or(true);
    if already_stopped && observed_pid.is_none() && lease_inactive && !observed_probe.owner_live() {
        return Ok(DaemonStopReport {
            status: observed_state
                .as_ref()
                .map(|state| state.status.clone())
                .unwrap_or_else(|| "unavailable".to_owned()),
            mutation_executed: false,
            lock_removed: false,
            pid_removed: false,
            lease: observed_lease,
            paths,
            state: observed_state,
        });
    }

    let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut lease = DurableRuntimeLeaseGuard::acquire(
        &backend,
        lock_file(options),
        lease_file(options),
        now_ms(),
        DEFAULT_RUNTIME_LEASE_TTL_MS,
    )?;
    let pid_projection = read_pid_projection(options)?;
    match (pid_projection.as_ref(), observed_record.as_ref()) {
        (Some(pid), Some(previous)) if !pid.matches_lease(previous) => {
            return Err(EvaError::conflict(
                "daemon pid projection belongs to another historical lease",
            )
            .with_context("pid", pid.pid().to_string())
            .with_context("expected_generation", previous.generation().0.to_string()))
        }
        (Some(pid), None) => {
            return Err(EvaError::conflict(
                "daemon pid projection has no matching historical lease",
            )
            .with_context("pid", pid.pid().to_string()))
        }
        _ => {}
    }
    let state = match read_state(options)? {
        Some(record) => {
            if let Some(pid) = pid_projection.as_ref() {
                if pid.pid() != record.pid {
                    return Err(EvaError::conflict(
                        "daemon state and pid projection belong to different owners",
                    )
                    .with_context("state_pid", record.pid.to_string())
                    .with_context("pid_file_pid", pid.pid().to_string()));
                }
            }
            let stopped = record.stopped();
            write_state(options, &stopped)?;
            Some(stopped)
        }
        None => None,
    };
    let pid_removed = match (pid_projection, observed_record.as_ref()) {
        (Some(_), Some(previous)) => remove_matching_pid(options, previous)?,
        (None, _) => false,
        (Some(_), None) => unreachable!("validated above"),
    };
    let released = lease.release_at(now_ms())?.clone();
    let lease = Some(DaemonLeaseReport::from_record(&released, false, false));

    Ok(DaemonStopReport {
        status: state
            .as_ref()
            .map(|record| record.status.clone())
            .unwrap_or_else(|| "unavailable".to_owned()),
        // 进入此分支即已安全 claim 并发布 released lease，即使没有旧 PID/state 也发生了变更。
        mutation_executed: true,
        lock_removed: false,
        pid_removed,
        lease,
        paths,
        state,
    })
}

/// 表示 `DaemonControlLoopReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonControlLoopReport {
    /// 记录 `status` 字段对应的值。
    status: String,
    /// 记录 `shutdown` 字段对应的值。
    shutdown: Option<ShutdownReport>,
}

/// 单个 mailbox mutation 所需的 daemon-owned runtime、writer 与配置引用。
struct DaemonControlContext<'a> {
    project: &'a ProjectConfig,
    options: &'a DaemonStartOptions,
    runtime: &'a mut crate::Runtime,
    running_state: &'a DaemonStateRecord,
    durable_layout: &'a DurableBackendLayout,
    lease: &'a mut DurableRuntimeLeaseGuard,
    task_worker: &'a mut TaskWorkerRuntime,
    observability: &'a mut BestEffortObservabilityPipeline,
}

struct DaemonControlLoopContext<'a> {
    options: &'a DaemonStartOptions,
    runtime: &'a mut crate::Runtime,
    running_state: DaemonStateRecord,
    durable_layout: &'a DurableBackendLayout,
    lease: &'a mut DurableRuntimeLeaseGuard,
    task_worker: &'a mut TaskWorkerRuntime,
    observability: &'a mut BestEffortObservabilityPipeline,
    memory_schedule: &'a FileSystemScheduleStore,
    memory_schedule_owner: &'a str,
    retrieval_worker: Option<&'a DaemonRetrievalWorker>,
    config_watcher: Option<&'a ConfigWatcher>,
    config_generation_store: &'a ConfigGenerationStore,
}

/// 串行执行调度重试 tick 和按文件名排序的控制请求，直到处理到关闭操作。
///
/// 每个请求先执行状态变更，再原子写响应，最后删除请求。响应写入失败时请求会保留以便诊断
/// 或重试，但变更可能已经发生，因此调用方必须依据响应或状态确认结果，不能只观察请求文件。
fn run_control_loop(
    context: DaemonControlLoopContext<'_>,
) -> Result<DaemonControlLoopReport, EvaError> {
    let heartbeat_interval = daemon_runtime_heartbeat_interval(context.options)?;
    let mut next_heartbeat = Instant::now() + heartbeat_interval;
    loop {
        let loop_generation = context.runtime.generation();
        let loop_project = loop_generation.project.as_ref();
        context.task_worker.check_health()?;
        renew_daemon_lease_if_due(context.lease, &mut next_heartbeat, heartbeat_interval)?;
        // 每轮先推进到期的调度重试，保证控制流量不会无限饿死恢复任务。
        let _tick = run_daemon_scheduler_tick(
            loop_project,
            context.options,
            context.lease.writer(),
            context.task_worker,
            context.observability,
        )?;
        let trace = TraceFields::default();
        let _maintenance = run_scheduled_memory_maintenance(
            context.memory_schedule,
            context.memory_schedule_owner,
            context.options,
            context.observability,
            &trace,
        )?;
        let _retrieval = run_scheduled_retrieval(
            context.memory_schedule,
            context.memory_schedule_owner,
            context.retrieval_worker,
            now_ms(),
        )?;
        if let Some(watcher) = context.config_watcher {
            if let Some(changes) = watcher.recv_timeout(Duration::ZERO)? {
                let changed_paths = changes
                    .paths
                    .iter()
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .collect();
                let mut report = preflight_config_reload(
                    &context.runtime.generation(),
                    &loop_project.project_root,
                    changed_paths,
                );
                if let Some(candidate) = report.candidate.take() {
                    context
                        .config_generation_store
                        .prepare(&candidate.identity)?;
                    let _previous = context.runtime.promote_generation(candidate)?;
                    context.config_generation_store.promote()?;
                    context.config_generation_store.retire()?;
                }
                record_config_reload_preflight(context.observability, &report);
            }
        }
        context.task_worker.check_health()?;
        renew_daemon_lease_if_due(context.lease, &mut next_heartbeat, heartbeat_interval)?;
        for request_path in pending_control_requests(context.options)? {
            let request_generation = context.runtime.generation();
            renew_daemon_lease_if_due(context.lease, &mut next_heartbeat, heartbeat_interval)?;
            let request = match read_control_request(&request_path) {
                Ok(request) => request,
                Err(error) => {
                    let _ = quarantine_control_request(&request_path, &error);
                    continue;
                }
            };
            let response_path = control_response_file(context.options, &request.request_id);
            let operation = request.operation;
            let mut context = DaemonControlContext {
                project: request_generation.project.as_ref(),
                options: context.options,
                runtime: context.runtime,
                running_state: &context.running_state,
                durable_layout: context.durable_layout,
                lease: context.lease,
                task_worker: context.task_worker,
                observability: context.observability,
            };
            let mut response = match handle_control_request(
                &mut context,
                request,
                &request_path,
                &response_path,
            ) {
                Ok(response) => response,
                Err(error) if operation == DaemonControlOperation::Shutdown => return Err(error),
                Err(error) => {
                    let _ = quarantine_control_request(&request_path, &error);
                    continue;
                }
            };
            renew_daemon_lease_if_due(context.lease, &mut next_heartbeat, heartbeat_interval)?;
            response.lease = Some(DaemonLeaseReport::from_guard(context.lease, now_ms()));
            let shutdown = response.shutdown.clone();
            let is_shutdown = response.operation == DaemonControlOperation::Shutdown;
            // 响应必须先原子发布；仅发布成功后才能删除请求这一恢复证据。
            write_control_response(&response_path, &response)?;
            remove_if_exists(&request_path)?;
            if is_shutdown {
                return Ok(DaemonControlLoopReport {
                    status: "stopped".to_owned(),
                    shutdown,
                });
            }
        }
        context.task_worker.check_health()?;
        thread::sleep(Duration::from_millis(CONTROL_POLL_INTERVAL_MS));
    }
}

fn record_config_reload_preflight(
    pipeline: &mut BestEffortObservabilityPipeline,
    report: &crate::ConfigReloadPreflight,
) {
    let _ = AuditSink::record(pipeline, config_reload_preflight_audit(report));
}

fn config_reload_preflight_audit(report: &crate::ConfigReloadPreflight) -> AuditEvent {
    let (outcome, message) = match &report.outcome {
        ConfigReloadPreflightOutcome::Ready => {
            (AuditOutcome::Ok, "config reload candidate passed preflight")
        }
        ConfigReloadPreflightOutcome::Rejected { .. } => (
            AuditOutcome::Blocked,
            "config reload candidate rejected; active generation retained",
        ),
    };
    let mut event = AuditEvent::new(
        AuditAction::ConfigValidated,
        outcome,
        TraceFields::default(),
    )
    .with_message(message)
    .with_field("old_digest", &report.old_digest)
    .with_field(
        "candidate_digest",
        report.candidate_digest.as_deref().unwrap_or("unavailable"),
    )
    .with_field("changed_paths", report.changed_paths.join(","))
    .with_field("active_generation_changed", "false");
    if let ConfigReloadPreflightOutcome::Rejected {
        error_kind,
        error_field,
        error_message,
        remediation,
    } = &report.outcome
    {
        event = event
            .with_field("error_kind", error_kind)
            .with_field("error_field", error_field)
            .with_field("error_message", error_message)
            .with_field("remediation", remediation);
    }
    event
}

/// 以单调时钟节流 heartbeat，持久记录只使用 epoch 时间；续租失败会终止 control loop。
fn renew_daemon_lease_if_due(
    lease: &mut DurableRuntimeLeaseGuard,
    next_heartbeat: &mut Instant,
    heartbeat_interval: Duration,
) -> Result<bool, EvaError> {
    let observed_at = Instant::now();
    if observed_at < *next_heartbeat {
        return Ok(false);
    }
    lease.renew_at(now_ms())?;
    *next_heartbeat = observed_at + heartbeat_interval;
    Ok(true)
}

/// 以当前逻辑时间重驱已到期的调度重试；每个循环最多调用一次该边界。
fn run_daemon_scheduler_tick(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    writer: DurableWriterGuard,
    task_worker: &TaskWorkerRuntime,
    observability: &mut BestEffortObservabilityPipeline,
) -> Result<SchedulerRetryTickReport, EvaError> {
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut bus = DurableEventBus::open_with_writer(durable_backend.layout(), writer)?;
    let report = run_scheduler_retry_tick_with_handler(
        project,
        &mut bus,
        task_worker,
        SchedulerRetryTickOptions {
            redrive_ready_at_ms: now_ms() as u64,
            ..SchedulerRetryTickOptions::default()
        },
    )?;
    record_scheduler_retry_observability(observability, &report);
    Ok(report)
}

/// 执行 `start_hardware_hotplug_subscriber` 对应的受控流程。
fn start_hardware_hotplug_subscriber(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    observability: &mut BestEffortObservabilityPipeline,
) -> Result<HardwareHotplugSubscriberReport, EvaError> {
    let previous_state = read_hardware_hotplug_state(options)?;
    let discovery = discover_project_devices(project)?;
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut bus = DurableEventBus::open(durable_backend.layout())?;
    let request_id_prefix = format!("req-daemon-hotplug-{}", now_ms());
    let report = run_hotplug_subscriber_once(
        &discovery.candidates,
        &previous_state,
        &mut bus,
        &request_id_prefix,
        observability,
    )?;
    write_hardware_hotplug_state(options, &report.state)?;
    Ok(report)
}

/// 执行 `run_memory_maintenance` 对应的受控流程。
fn run_memory_maintenance(
    options: &DaemonStartOptions,
    observability: &mut BestEffortObservabilityPipeline,
    trace: &TraceFields,
) -> Result<DaemonMemoryMaintenanceReport, EvaError> {
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut memory_store = FileSystemMemoryStore::from_durable_layout(durable_backend.layout());
    let mut knowledge_store =
        FileSystemKnowledgeStore::from_durable_layout(durable_backend.layout());
    let memory_gc = memory_store.compact_expired_at(now_ms(), observability, trace)?;
    let knowledge_rebuild = knowledge_store.rebuild_checkpoint(observability, trace)?;
    Ok(DaemonMemoryMaintenanceReport {
        status: "ready".to_owned(),
        audit: vec![
            "memory.maintenance:ttl_gc_completed".to_owned(),
            "memory.maintenance:knowledge_rebuild_checkpoint_completed".to_owned(),
        ],
        memory_gc,
        knowledge_rebuild,
    })
}

fn schedule_owner(lease: &DurableRuntimeLeaseGuard) -> String {
    format!(
        "daemon:{}:{}",
        lease.record().pid(),
        lease.record().generation().0
    )
}

fn run_scheduled_memory_maintenance(
    schedule: &FileSystemScheduleStore,
    owner: &str,
    options: &DaemonStartOptions,
    observability: &mut BestEffortObservabilityPipeline,
    trace: &TraceFields,
) -> Result<Option<DaemonMemoryMaintenanceReport>, EvaError> {
    let observed_at = now_ms();
    let claim = match schedule.claim(
        MEMORY_MAINTENANCE_SCHEDULE_ID,
        owner,
        observed_at,
        MEMORY_MAINTENANCE_LEASE_MS,
    ) {
        Ok(claim) => claim,
        Err(error) if matches!(error.kind(), eva_core::ErrorKind::Unavailable) => return Ok(None),
        Err(error) => return Err(error),
    };
    let report = run_memory_maintenance(options, observability, trace)?;
    schedule.complete(
        MEMORY_MAINTENANCE_SCHEDULE_ID,
        owner,
        claim.generation,
        observed_at.saturating_add(MEMORY_MAINTENANCE_INTERVAL_MS),
    )?;
    Ok(Some(report))
}

/// 登记 `record_daemon_recovery_observability` 对应的数据或状态。
fn record_daemon_recovery_observability(
    pipeline: &mut BestEffortObservabilityPipeline,
    trace: &TraceFields,
    report: &RuntimeRecoveryReport,
) {
    let Ok(span_id) = SpanId::parse("runtime.daemon.recovery") else {
        return;
    };
    let recovery_trace = trace.child_span(span_id);
    let _ =
        RuntimeRecoveryCoordinator.record_recovery_audit(pipeline, recovery_trace.clone(), report);
    if let Ok(name) = MetricName::parse("runtime.daemon.recovery") {
        let _ = MetricSink::record(
            pipeline,
            MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                MetricLabels::runtime("daemon_v1.16.1", DAEMON_GENERATION)
                    .with("recovered_tasks", report.recovered_tasks.len().to_string())
                    .with(
                        "recovered_provider_processes",
                        report.recovered_provider_processes.len().to_string(),
                    )
                    .with(
                        "provider_orphan_cleanup_outcomes",
                        report
                            .audit
                            .iter()
                            .filter(|entry| entry.starts_with("runtime.recovery:provider_orphan:"))
                            .count()
                            .to_string(),
                    ),
            ),
        );
    }
    let recovered_tasks = report.recovered_tasks.len().to_string();
    let recovered_provider_processes = report.recovered_provider_processes.len().to_string();
    let provider_orphan_cleanup_outcomes = report
        .audit
        .iter()
        .filter(|entry| entry.starts_with("runtime.recovery:provider_orphan:"))
        .count()
        .to_string();
    let _ = pipeline.export_span(
        "runtime.daemon.recovery",
        &recovery_trace,
        &[
            ("component", "runtime"),
            ("recovered_tasks", recovered_tasks.as_str()),
            (
                "recovered_provider_processes",
                recovered_provider_processes.as_str(),
            ),
            (
                "provider_orphan_cleanup_outcomes",
                provider_orphan_cleanup_outcomes.as_str(),
            ),
        ],
    );
}

/// 登记 `record_scheduler_retry_observability` 对应的数据或状态。
fn record_scheduler_retry_observability(
    pipeline: &mut BestEffortObservabilityPipeline,
    report: &SchedulerRetryTickReport,
) {
    if report.dispatched_events.is_empty() && report.failed_events.is_empty() {
        return;
    }
    let Ok(span_id) = SpanId::parse("runtime.scheduler.retry") else {
        return;
    };
    let trace = TraceFields::default().with_span_id(span_id);
    let outcome = if report.failed_events.is_empty() {
        AuditOutcome::Ok
    } else {
        AuditOutcome::Failed
    };
    let _ = AuditSink::record(
        pipeline,
        AuditEvent::new(AuditAction::SchedulerRetry, outcome, trace.clone())
            .with_message("daemon scheduler retry tick observed")
            .with_field(
                "scanned_dead_letters",
                report.scanned_dead_letters.to_string(),
            )
            .with_field("due_dead_letters", report.due_dead_letters.to_string())
            .with_field(
                "dispatched_events",
                report.dispatched_events.len().to_string(),
            )
            .with_field("failed_events", report.failed_events.len().to_string())
            .with_field("skipped_events", report.skipped_events.len().to_string()),
    );
    if let Ok(name) = MetricName::parse("runtime.scheduler.retry") {
        let _ = MetricSink::record(
            pipeline,
            MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                MetricLabels::runtime("daemon_v1.16.1", DAEMON_GENERATION)
                    .with(
                        "dispatched_events",
                        report.dispatched_events.len().to_string(),
                    )
                    .with("failed_events", report.failed_events.len().to_string()),
            ),
        );
    }
    let dispatched_events = report.dispatched_events.len().to_string();
    let failed_events = report.failed_events.len().to_string();
    let _ = pipeline.export_span(
        "runtime.scheduler.retry",
        &trace,
        &[
            ("component", "runtime"),
            ("dispatched_events", dispatched_events.as_str()),
            ("failed_events", failed_events.as_str()),
        ],
    );
}

/// 登记 `record_daemon_control_observability` 对应的数据或状态。
fn record_daemon_control_observability(
    pipeline: &mut BestEffortObservabilityPipeline,
    request: &DaemonControlRequest,
    response: &DaemonControlResponse,
    task_lifecycle_status: Option<&str>,
) {
    let Ok(span_id) = SpanId::parse(&format!(
        "runtime.daemon.control.{}",
        request.operation.as_str()
    )) else {
        return;
    };
    let mut trace = TraceFields::default()
        .with_request_id(request.request_id.clone())
        .with_span_id(span_id);
    if let Some(agent_id) = request
        .task_envelope
        .as_ref()
        .map(|envelope| envelope.agent_id().as_str())
        .or(request.agent_id.as_deref())
        .and_then(|value| AgentId::parse(value).ok())
    {
        trace = trace.with_agent_id(agent_id);
    }
    if let Some(generation_id) = response
        .generation_id
        .as_deref()
        .and_then(|value| GenerationId::parse(value).ok())
    {
        trace.generation_id = Some(generation_id);
    }

    let outcome = if response.accepted {
        AuditOutcome::Ok
    } else {
        AuditOutcome::Blocked
    };
    let mut event = AuditEvent::new(AuditAction::RuntimeControl, outcome, trace.clone())
        .with_message("daemon control request observed")
        .with_field("operation", request.operation.as_str())
        .with_field("status", response.status.as_str())
        .with_field("mutation_executed", response.mutation_executed.to_string())
        .with_field("trace_id", response.trace_id.as_str());
    if let Some(task_id) = response.task_id.as_deref() {
        event = event.with_field("task_id", task_id);
    }
    if let Some(plan_id) = response.plan_id.as_deref() {
        event = event.with_field("plan_id", plan_id);
    }
    let _ = AuditSink::record(pipeline, event);

    if let Ok(name) = MetricName::parse("runtime.daemon.control") {
        let _ = MetricSink::record(
            pipeline,
            MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                MetricLabels::runtime("daemon_v1.16.1", DAEMON_GENERATION)
                    .with("operation", request.operation.as_str())
                    .with("status", response.status.as_str())
                    .with("mutation_executed", response.mutation_executed.to_string()),
            ),
        );
    }
    let _ = pipeline.export_span(
        "runtime.daemon.control",
        &trace,
        &[
            ("component", "runtime"),
            ("operation", request.operation.as_str()),
            ("status", response.status.as_str()),
        ],
    );

    record_task_lifecycle_observability(pipeline, request, response, &trace, task_lifecycle_status);
}

/// 登记 `record_task_lifecycle_observability` 对应的数据或状态。
fn record_task_lifecycle_observability(
    pipeline: &mut BestEffortObservabilityPipeline,
    request: &DaemonControlRequest,
    response: &DaemonControlResponse,
    parent_trace: &TraceFields,
    task_lifecycle_status: Option<&str>,
) {
    let lifecycle_status = match request.operation {
        DaemonControlOperation::SubmitTask if response.mutation_executed => {
            task_lifecycle_status.unwrap_or("queued")
        }
        DaemonControlOperation::CancelTask if response.mutation_executed => {
            task_lifecycle_status.unwrap_or("cancelling")
        }
        _ => return,
    };
    let Some(task_id) = response.task_id.as_deref() else {
        return;
    };
    let Ok(span_id) = SpanId::parse("runtime.task.lifecycle") else {
        return;
    };
    let trace = parent_trace.child_span(span_id);
    let agent_id = request
        .task_envelope
        .as_ref()
        .map(|envelope| envelope.agent_id().as_str())
        .or(request.agent_id.as_deref())
        .unwrap_or("daemon-control");
    let _ = AuditSink::record(
        pipeline,
        AuditEvent::new(AuditAction::TaskLifecycle, AuditOutcome::Ok, trace.clone())
            .with_message("daemon task lifecycle mutation observed")
            .with_field("operation", request.operation.as_str())
            .with_field("task_id", task_id)
            .with_field("task_status", lifecycle_status)
            .with_field("agent_id", agent_id),
    );
    if let Ok(name) = MetricName::parse("runtime.task.lifecycle") {
        let _ = MetricSink::record(
            pipeline,
            MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                MetricLabels::task(lifecycle_status, agent_id)
                    .with("operation", request.operation.as_str())
                    .with("task_id", task_id),
            ),
        );
    }
    let _ = pipeline.export_span(
        "runtime.task.lifecycle",
        &trace,
        &[
            ("component", "runtime"),
            ("operation", request.operation.as_str()),
            ("task_status", lifecycle_status),
        ],
    );
}

fn shutdown_drain_options(timeout_ms: Option<u64>) -> Result<TaskWorkerDrainOptions, EvaError> {
    let Some(timeout_ms) = timeout_ms else {
        return Ok(TaskWorkerDrainOptions::default());
    };
    if timeout_ms < 2 {
        return Err(EvaError::invalid_argument(
            "daemon shutdown drain timeout must be at least two milliseconds",
        ));
    }
    let cancellation_ms = (timeout_ms / 3).max(1);
    let graceful_ms = timeout_ms.saturating_sub(cancellation_ms).max(1);
    TaskWorkerDrainOptions::new(
        Duration::from_millis(graceful_ms),
        Duration::from_millis(cancellation_ms),
    )
}

/// 执行单个控制操作并构造响应；可观测性采用尽力而为，不改变已完成的业务结果。
fn handle_control_request(
    context: &mut DaemonControlContext<'_>,
    request: DaemonControlRequest,
    request_path: &Path,
    response_path: &Path,
) -> Result<DaemonControlResponse, EvaError> {
    let mut request = request;
    if request.operation == DaemonControlOperation::SubmitTask
        && request.wire_version == 1
        && request.task_envelope.is_none()
    {
        let task_id = request
            .task_id
            .as_deref()
            .unwrap_or_else(|| request.request_id.as_str());
        request.task_envelope = Some(legacy_submit_envelope(context.project, task_id)?);
    }
    let mut state = read_state(context.options)?.unwrap_or_else(|| context.running_state.clone());
    let accepted = true;
    let mut mutation_executed = false;
    let mut task_id = request.task_id.clone();
    let mut plan_id = request.plan_id.clone();
    let mut generation_id = request.generation_id.clone();
    let message;
    let mut shutdown = None;
    let mut audit = vec![format!(
        "daemon:v1.12.2:control:{}",
        request.operation.as_str()
    )];
    let mut task_lifecycle_status = None;

    // 所有持久化变更均在响应构造前完成，因此 `mutation_executed` 只描述已成功分支。
    match request.operation {
        DaemonControlOperation::Status => {
            message = "daemon status returned through local control mailbox".to_owned();
        }
        DaemonControlOperation::Shutdown => {
            context.lease.renew_at(now_ms())?;
            let drain_options = shutdown_drain_options(request.timeout_ms)?;
            let drain = context.task_worker.drain_and_stop(drain_options)?;
            context.lease.renew_at(now_ms())?;
            let shutdown_report = context.runtime.shutdown();
            let drain_evidence = DaemonShutdownDrainEvidence::completed(
                request.request_id.clone(),
                context.lease.record(),
                &drain,
            )?;
            write_shutdown_drain_evidence(context.options, &drain_evidence)?;
            state = state.stopped();
            write_state(context.options, &state)?;
            remove_matching_pid(context.options, context.lease.record())?;
            mutation_executed = true;
            message =
                "daemon shutdown drained task ownership through local control mailbox".to_owned();
            audit.push("daemon:v1.12.2:shutdown_recorded".to_owned());
            audit.push("daemon:w1-l11:claim_gate_closed".to_owned());
            audit.push(format!(
                "daemon:w1-l11:drained:inflight={}:cancelled={}:forced={}",
                drain.inflight_tasks, drain.cancellation_requests, drain.forced_terminal_tasks
            ));
            audit.push("daemon:w1-l11:stable_task_state_flushed".to_owned());
            shutdown = Some(shutdown_report);
        }
        DaemonControlOperation::SubmitTask => {
            let submitted_task_id = submit_control_task(
                context.durable_layout,
                context.lease.writer(),
                context.project,
                &request,
            )?;
            context.task_worker.notify_new_work();
            task_id = Some(submitted_task_id);
            task_lifecycle_status = Some("queued".to_owned());
            mutation_executed = true;
            message =
                "task submitted to durable task store through daemon control mailbox".to_owned();
            audit.push("daemon:v1.12.2:task_submitted".to_owned());
        }
        DaemonControlOperation::CancelTask => {
            let cancelled =
                cancel_control_task(context.durable_layout, context.lease.writer(), &request)?;
            context
                .task_worker
                .signal_cancellation(&cancelled.task_id, cancelled.cancel_token.as_deref());
            task_lifecycle_status = Some(cancelled.status.clone());
            task_id = Some(cancelled.task_id);
            mutation_executed = true;
            message = "task cancellation recorded through daemon control mailbox".to_owned();
            audit.push("daemon:v1.12.2:task_cancel_requested".to_owned());
        }
        DaemonControlOperation::Drain => {
            let applied = apply_agent_drain_control(context.options, &request)?;
            task_id = Some(applied.agent_id.clone());
            generation_id = Some(applied.generation_id);
            plan_id = applied.plan_id;
            mutation_executed = true;
            message =
                "agent drain mutation recorded through daemon scheduler gate state".to_owned();
            audit.extend(applied.audit);
        }
        DaemonControlOperation::ReloadPlan => {
            let applied = apply_agent_reload_control(context.options, &request)?;
            task_id = Some(applied.agent_id.clone());
            plan_id = Some(applied.plan_id);
            generation_id = Some(applied.active_generation);
            mutation_executed = true;
            message =
                "agent reload mutation recorded through daemon generation route gate".to_owned();
            audit.extend(applied.audit);
        }
    }

    let response = DaemonControlResponse {
        request_id: request.request_id.clone(),
        trace_id: request.trace_id.clone(),
        operation: request.operation,
        accepted,
        daemon_available: true,
        status: state.status.clone(),
        mutation_executed,
        request_file: display_path(request_path),
        response_file: display_path(response_path),
        state: Some(state),
        lease: Some(DaemonLeaseReport::from_guard(context.lease, now_ms())),
        task_id,
        plan_id,
        generation_id,
        message,
        shutdown,
        audit,
    };
    record_daemon_control_observability(
        context.observability,
        &request,
        &response,
        task_lifecycle_status.as_deref(),
    );
    Ok(response)
}

/// 创建 queued 任务快照；任务标识默认沿用请求标识，作为持久化关联键。
fn submit_control_task(
    durable_layout: &DurableBackendLayout,
    writer: DurableWriterGuard,
    project: &ProjectConfig,
    request: &DaemonControlRequest,
) -> Result<String, EvaError> {
    let task_id = request
        .task_id
        .clone()
        .unwrap_or_else(|| request.request_id.as_str().to_owned());
    RequestId::parse(&task_id)?;
    if task_id.starts_with("replay-delivery-") {
        return Err(EvaError::invalid_argument(
            "daemon submit task id uses the reserved replay-delivery namespace",
        )
        .with_context("task_id", &task_id));
    }
    let envelope = match &request.task_envelope {
        Some(envelope) => envelope.clone(),
        None if request.wire_version == 1 => legacy_submit_envelope(project, &task_id)?,
        None => {
            return Err(EvaError::invalid_argument(
                "daemon submit request requires a complete task envelope",
            ))
        }
    };
    let agent = project
        .agents
        .iter()
        .find(|agent| &agent.id == envelope.agent_id())
        .ok_or_else(|| {
            EvaError::not_found("daemon task envelope references an unknown agent")
                .with_context("agent_id", envelope.agent_id().as_str())
        })?;
    if !agent.enabled {
        return Err(
            EvaError::conflict("daemon task envelope references a disabled agent")
                .with_context("agent_id", envelope.agent_id().as_str()),
        );
    }
    let mut store = open_durable_task_store(durable_layout, writer)?;
    let mut snapshot =
        TaskStateSnapshot::queued_with_envelope(task_id.clone(), envelope.to_snapshot())?;
    snapshot.push_log(
        "info",
        format!(
            "submitted through daemon control mailbox for project {}",
            display_path(&project.project_root)
        ),
    );
    store.write(&snapshot)?;
    Ok(task_id)
}

/// 仅供 v1 mailbox/旧 CLI 兼容：显式标为 `legacy.submit`，不会冒充已注册 handler。
fn legacy_submit_envelope(
    project: &ProjectConfig,
    task_id: &str,
) -> Result<TaskEnvelope, EvaError> {
    let agent_id = project
        .agents
        .iter()
        .find(|agent| agent.enabled)
        .map(|agent| agent.id.clone())
        .ok_or_else(|| EvaError::not_found("daemon submit requires an enabled agent"))?;
    TaskEnvelope::new(
        TaskKind::parse("legacy.submit")?,
        agent_id,
        TaskInput::inline(Vec::new())?,
        IdempotencyKey::parse(task_id)?,
        TaskAttemptPolicy::new(1, 0, None)?,
    )
}

/// 在既有快照上请求取消；缺少任务或状态转换非法时不创建替代快照。
fn cancel_control_task(
    durable_layout: &DurableBackendLayout,
    writer: DurableWriterGuard,
    request: &DaemonControlRequest,
) -> Result<TaskStateSnapshot, EvaError> {
    let task_id = request.task_id.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("daemon cancel task request requires a task id")
    })?;
    RequestId::parse(task_id)?;
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| "cancel requested by daemon control API".to_owned());
    let mut store = open_durable_task_store(durable_layout, writer)?;
    store.request_cancellation(task_id, reason)
}

/// 表示 `AppliedAgentDrainControl` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedAgentDrainControl {
    /// 记录 `agent_id` 字段对应的值。
    agent_id: String,
    /// 记录 `generation_id` 字段对应的值。
    generation_id: String,
    /// 记录 `plan_id` 字段对应的值。
    plan_id: Option<String>,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
}

/// 表示 `AppliedAgentReloadControl` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedAgentReloadControl {
    /// 记录 `agent_id` 字段对应的值。
    agent_id: String,
    /// 记录 `plan_id` 字段对应的值。
    plan_id: String,
    /// 记录 `active_generation` 字段对应的值。
    active_generation: String,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
}

/// 先完整验证排空计划，再原子写代理控制检查点；规划失败不会留下部分状态。
fn apply_agent_drain_control(
    options: &DaemonStartOptions,
    request: &DaemonControlRequest,
) -> Result<AppliedAgentDrainControl, EvaError> {
    let agent_id = control_agent_id(request)?;
    let generation_id = request
        .generation_id
        .clone()
        .or_else(|| request.from_generation_id.clone())
        .unwrap_or_else(|| DAEMON_GENERATION.to_owned());
    let drain = DrainCoordinator.plan(
        GenerationId::parse(&generation_id)?,
        request.inflight_tasks.unwrap_or(0),
        request.timeout_ms.unwrap_or(30_000),
    )?;
    let mut audit = vec!["daemon:v1.12.5:agent_drain_mutation".to_owned()];
    audit.extend(drain.audit.iter().cloned());
    audit.push(format!(
        "scheduler:generation:{}:accepts_new_work:false",
        drain.generation_id.as_str()
    ));

    let persisted = DaemonAgentControlState::from_drain(
        agent_id.clone(),
        &drain,
        request.plan_id.clone(),
        audit.clone(),
    );
    write_agent_control_state(options, &persisted)?;

    Ok(AppliedAgentDrainControl {
        agent_id,
        generation_id,
        plan_id: request.plan_id.clone(),
        audit,
    })
}

/// 按候选健康、路由选择、代际提升、旧代排空的顺序验证重载，再发布单一检查点。
fn apply_agent_reload_control(
    options: &DaemonStartOptions,
    request: &DaemonControlRequest,
) -> Result<AppliedAgentReloadControl, EvaError> {
    let agent_id = control_agent_id(request)?;
    let from_generation = request
        .from_generation_id
        .clone()
        .unwrap_or_else(|| DAEMON_GENERATION.to_owned());
    let mut to_generation = request
        .to_generation_id
        .clone()
        .or_else(|| request.generation_id.clone())
        .unwrap_or_else(|| format!("{}:candidate", request.request_id.as_str()));
    if to_generation == from_generation {
        to_generation = format!("{to_generation}:next");
    }
    let from_release = request
        .from_release
        .clone()
        .unwrap_or_else(|| "current".to_owned());
    let to_release = request
        .to_release
        .clone()
        .unwrap_or_else(|| "next".to_owned());
    let plan_id = request
        .plan_id
        .clone()
        .unwrap_or_else(|| format!("reload:{}", request.request_id.as_str()));

    let from_generation_id = GenerationId::parse(&from_generation)?;
    let to_generation_id = GenerationId::parse(&to_generation)?;
    let mut route_gate = GenerationRouteGate::new(from_generation_id.clone());
    // 新代际必须先完成影子健康标记，路由门禁才允许为新工作选择它。
    route_gate.start_candidate(to_generation_id.clone())?;
    route_gate.mark_candidate_shadow_healthy(&to_generation_id)?;
    let selected_generation_for_new_work = route_gate
        .selected_generation_for_new_work()
        .as_str()
        .to_owned();

    let active = RuntimeGeneration::new(
        from_generation_id.clone(),
        from_release.clone(),
        GenerationState::Active,
    )?;
    let candidate = RuntimeGeneration::new(
        to_generation_id.clone(),
        to_release.clone(),
        GenerationState::Pending,
    )?;
    let mut controller = GenerationController::new(active)?;
    controller.start_candidate(candidate)?;
    // 提升成功后才为旧代际生成排空计划，避免新工作没有活动代际可选。
    controller.promote_candidate()?;
    let drain = DrainCoordinator.plan_generation_swap_drain(
        from_generation_id,
        to_generation_id,
        request.inflight_tasks.unwrap_or(0),
        request.timeout_ms.unwrap_or(30_000),
    )?;
    let previous = controller
        .retired
        .first()
        .ok_or_else(|| EvaError::internal("generation promotion did not retain previous active"))?;

    let mut audit = vec!["daemon:v1.12.5:agent_reload_mutation".to_owned()];
    audit.extend(route_gate.audit().iter().cloned());
    audit.extend(controller.audit.iter().cloned());
    audit.extend(drain.audit.iter().cloned());
    audit.push(format!(
        "scheduler:new_work_generation:{}",
        selected_generation_for_new_work
    ));

    let active_generation = controller.active.id.as_str().to_owned();
    let persisted = DaemonAgentControlState {
        agent_id: agent_id.clone(),
        operation: "reload_plan".to_owned(),
        lifecycle: "running".to_owned(),
        drain_generation_id: Some(drain.plan.generation_id.as_str().to_owned()),
        drain_inflight_tasks: Some(drain.plan.inflight_tasks),
        drain_timeout_ms: Some(drain.plan.timeout_ms),
        drain_accepts_new_work: Some(drain.plan.accepts_new_work),
        drain_status: Some(drain.plan.status.as_str().to_owned()),
        active_generation: Some(active_generation.clone()),
        previous_generation: Some(previous.id.as_str().to_owned()),
        previous_generation_state: Some(previous.state.as_str().to_owned()),
        selected_generation_for_new_work: Some(selected_generation_for_new_work),
        from_release: Some(from_release),
        to_release: Some(to_release),
        plan_id: Some(plan_id.clone()),
        mutation_executed: true,
        updated_at_ms: now_ms(),
        audit: audit.clone(),
    };
    write_agent_control_state(options, &persisted)?;

    Ok(AppliedAgentReloadControl {
        agent_id,
        plan_id,
        active_generation,
        audit,
    })
}

/// 执行 `control_agent_id` 对应的处理逻辑。
fn control_agent_id(request: &DaemonControlRequest) -> Result<String, EvaError> {
    let agent_id = request
        .agent_id
        .as_deref()
        .or(request.task_id.as_deref())
        .unwrap_or("daemon-agent");
    AgentId::parse(agent_id)?;
    Ok(agent_id.to_owned())
}

/// 读取 `open_durable_task_store` 所需的持久化数据，失败时保留错误上下文。
fn open_durable_task_store(
    durable_layout: &DurableBackendLayout,
    writer: DurableWriterGuard,
) -> Result<FileSystemTaskStateStore, EvaError> {
    FileSystemTaskStateStore::from_runtime_writer(durable_layout, writer)
}

/// 校验 `verify_policy` 对应的约束，不满足时返回明确错误。
fn verify_policy(project: &ProjectConfig) -> Result<DaemonPolicyReport, EvaError> {
    let domains = PolicyDomainSet::from_project(project)?;
    let effective = domains.effective_policy()?;
    Ok(DaemonPolicyReport {
        status: "verified".to_owned(),
        source_count: domains.source_count,
        effective_layers: effective.layer_names,
    })
}

/// 校验 `verify_observability` 对应的约束，不满足时返回明确错误。
fn verify_observability(
    lifecycle: &mut RuntimeObservabilityLifecycle,
    trace: &TraceFields,
) -> Result<ObservabilitySmokeReport, EvaError> {
    let runtime_trace = trace.child_span(SpanId::parse("runtime.daemon.start")?);
    AuditSink::record(
        lifecycle.pipeline_mut(),
        AuditEvent::new(
            AuditAction::RuntimeStarted,
            AuditOutcome::Planned,
            runtime_trace.clone(),
        )
        .with_message("daemon foreground smoke boundary verified")
        .with_field("generation_id", DAEMON_GENERATION),
    )?;
    MetricSink::record(
        lifecycle.pipeline_mut(),
        MetricPoint::new(
            MetricName::parse("runtime.daemon.start")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::runtime("daemon_v1.12.1", DAEMON_GENERATION)),
    )?;
    lifecycle.pipeline_mut().export_span(
        "runtime.daemon.start",
        &runtime_trace,
        &[("component", "runtime"), ("mode", "foreground_dev")],
    )?;
    lifecycle.flush(trace)
}

/// 校验 `ensure_control_dirs` 对应的约束，不满足时返回明确错误。
fn ensure_control_dirs(options: &DaemonStartOptions) -> Result<(), EvaError> {
    for path in [control_request_dir(options), control_response_dir(options)] {
        fs::create_dir_all(&path).map_err(|error| {
            EvaError::internal("failed to create daemon control directory")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    Ok(())
}

/// 返回 `pending_control_requests` 对应的数据视图。
fn pending_control_requests(options: &DaemonStartOptions) -> Result<Vec<PathBuf>, EvaError> {
    let dir = control_request_dir(options);
    fs::create_dir_all(&dir).map_err(|error| {
        EvaError::internal("failed to create daemon control request directory")
            .with_context("path", dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| {
        EvaError::internal("failed to read daemon control request directory")
            .with_context("path", dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read daemon control request entry")
                .with_context("path", dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value == CONTROL_REQUEST_EXT)
            .unwrap_or(false)
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// 读取 `read_control_request` 所需的持久化数据，失败时保留错误上下文。
fn read_control_request(path: &Path) -> Result<DaemonControlRequest, EvaError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        EvaError::internal("failed to inspect daemon control request")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    if !metadata.file_type().is_file() {
        return Err(EvaError::invalid_argument(
            "daemon control request entry must be a regular file",
        ));
    }
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::internal("failed to read daemon control request")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DaemonControlRequest::from_storage(&data)
}

/// 先把目录项移出 pending 集合，再安全删除原项并发布不含 payload 的独立摘要。
fn quarantine_control_request(path: &Path, error: &EvaError) -> Result<(), EvaError> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown-request");
    let quarantined_at_ms = now_ms();
    let rejected = path.with_file_name(format!(
        "{stem}.{quarantined_at_ms}.{}",
        CONTROL_REJECTED_EXT
    ));
    let isolated = path.with_file_name(format!("{stem}.{quarantined_at_ms}.quarantine-entry"));
    match fs::rename(path, &isolated) {
        Ok(()) => {}
        Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(io_error) => {
            return Err(
                EvaError::internal("failed to isolate invalid daemon control request")
                    .with_context("path", path.display().to_string())
                    .with_context("isolated_path", isolated.display().to_string())
                    .with_context("io_error", io_error.to_string()),
            )
        }
    }
    let metadata = fs::symlink_metadata(&isolated).map_err(|io_error| {
        EvaError::internal("failed to inspect isolated daemon control request")
            .with_context("path", isolated.display().to_string())
            .with_context("io_error", io_error.to_string())
    })?;
    let file_type = metadata.file_type();
    let entry_kind = if file_type.is_file() {
        "regular_file"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "directory"
    } else {
        "other"
    };
    let request_bytes = if file_type.is_file() {
        fs::read(&isolated).ok()
    } else {
        None
    };
    let request_size_bytes = request_bytes
        .as_ref()
        .map(Vec::len)
        .unwrap_or_else(|| metadata.len().try_into().unwrap_or(usize::MAX));
    let request_digest = request_bytes
        .as_deref()
        .map(sha256_digest)
        .unwrap_or_default();
    let quarantine_record = format!(
        "version=1\nrequest_file={}\nentry_kind={}\nrequest_size_bytes={}\nrequest_sha256={}\nerror_kind={}\nerror_message={}\n",
        encode_field(stem),
        entry_kind,
        request_size_bytes,
        request_digest,
        error.kind().as_str(),
        encode_field(error.message())
    );
    let summary_temp = rejected.with_extension("summary.tmp");
    let mut summary_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&summary_temp)
        .map_err(|io_error| {
            EvaError::internal("failed to create daemon control quarantine summary")
                .with_context("path", summary_temp.display().to_string())
                .with_context("io_error", io_error.to_string())
        })?;
    summary_file
        .write_all(quarantine_record.as_bytes())
        .and_then(|()| summary_file.flush())
        .and_then(|()| summary_file.sync_all())
        .map_err(|io_error| {
            EvaError::internal("failed to persist daemon control quarantine summary")
                .with_context("path", summary_temp.display().to_string())
                .with_context("io_error", io_error.to_string())
        })?;
    let removal = if file_type.is_dir() {
        fs::remove_dir(&isolated)
    } else {
        fs::remove_file(&isolated)
    };
    let entry_removed = match removal {
        Ok(()) => true,
        Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };
    let disposition = format!(
        "entry_removed={entry_removed}\npayload_retained={}\n",
        !entry_removed
    );
    summary_file
        .write_all(disposition.as_bytes())
        .and_then(|()| summary_file.flush())
        .and_then(|()| summary_file.sync_all())
        .map_err(|io_error| {
            EvaError::internal("failed to finalize daemon control quarantine summary")
                .with_context("path", summary_temp.display().to_string())
                .with_context("io_error", io_error.to_string())
        })?;
    drop(summary_file);
    fs::rename(&summary_temp, &rejected).map_err(|io_error| {
        EvaError::internal("failed to publish daemon control quarantine summary")
            .with_context("path", summary_temp.display().to_string())
            .with_context("rejected_path", rejected.display().to_string())
            .with_context("io_error", io_error.to_string())
    })?;
    let reason_path = rejected.with_extension("reason");
    let reason = format!(
        "error_kind={}\nerror_message={}\nrequest_sha256={}\npayload_retained={}\n",
        error.kind().as_str(),
        encode_field(error.message()),
        request_digest,
        !entry_removed
    );
    let _ = write_atomic(
        &reason_path,
        &reason,
        "failed to write rejected daemon control request reason",
    );
    Ok(())
}

/// 持久化 `write_control_request` 对应的数据，写入失败时返回错误。
fn write_control_request(path: &Path, request: &DaemonControlRequest) -> Result<(), EvaError> {
    write_atomic(
        path,
        &request.to_storage(),
        "failed to write daemon control request",
    )
}

/// 持久化 `write_control_response` 对应的数据，写入失败时返回错误。
fn write_control_response(path: &Path, response: &DaemonControlResponse) -> Result<(), EvaError> {
    write_atomic(
        path,
        &response.to_storage(),
        "failed to write daemon control response",
    )
}

fn write_startup_atomic(path: &Path, data: &str, message: &'static str) -> Result<(), EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            EvaError::internal("failed to create daemon startup directory")
                .with_context("path", parent.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    let result = atomic_storage_write(path, data.as_bytes()).map_err(|error| {
        EvaError::internal(message)
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    });
    confirm_startup_atomic_result(path, data, result)
}

fn confirm_startup_atomic_result(
    path: &Path,
    data: &str,
    result: Result<(), EvaError>,
) -> Result<(), EvaError> {
    let Err(error) = result else {
        return Ok(());
    };
    match fs::read(path) {
        Ok(actual) if actual == data.as_bytes() => Ok(()),
        Ok(_) => Err(error.with_context("publication_readback", "mismatch")),
        Err(read_error) => Err(error
            .with_context("publication_readback", "unavailable")
            .with_context("publication_readback_error", read_error.to_string())),
    }
}

/// 持久化 `write_atomic` 对应的数据，写入失败时返回错误。
fn write_atomic(path: &Path, data: &str, message: &'static str) -> Result<(), EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            EvaError::internal("failed to create daemon control file directory")
                .with_context("path", parent.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).map_err(|error| {
        EvaError::internal(message)
            .with_context("path", tmp_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    fs::rename(&tmp_path, path).map_err(|error| {
        EvaError::internal(message)
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 读取 `read_state` 所需的持久化数据，失败时保留错误上下文。
fn read_state(options: &DaemonStartOptions) -> Result<Option<DaemonStateRecord>, EvaError> {
    let path = state_file(options);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path).map_err(|error| {
        EvaError::internal("failed to read daemon state")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DaemonStateRecord::from_storage(&data).map(Some)
}

fn read_shutdown_drain_evidence(
    options: &DaemonStartOptions,
) -> Result<Option<DaemonShutdownDrainEvidence>, EvaError> {
    let path = shutdown_drain_evidence_file(options);
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                EvaError::internal("failed to read daemon shutdown drain evidence")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string()),
            )
        }
    };
    DaemonShutdownDrainEvidence::from_storage(&data)
        .map(Some)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

/// 严格读取 PID projection；损坏内容不能被当作“不存在”后继续接管。
fn read_pid_projection(
    options: &DaemonStartOptions,
) -> Result<Option<DaemonPidProjection>, EvaError> {
    let path = pid_file(options);
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EvaError::internal("failed to read daemon pid file")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string()))
        }
    };
    DaemonPidProjection::from_storage(&data)
        .map(Some)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

fn write_pid_projection(
    options: &DaemonStartOptions,
    lease: &DurableRuntimeLeaseRecord,
) -> Result<(), EvaError> {
    let path = pid_file(options);
    fs::write(&path, DaemonPidProjection::from_lease(lease).to_storage()).map_err(|error| {
        EvaError::internal("failed to write daemon pid projection")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 只删除仍投影到预期完整 lease identity 的 PID，防止 PID reuse 或迟到 shutdown 清掉 successor。
fn remove_matching_pid(
    options: &DaemonStartOptions,
    expected: &DurableRuntimeLeaseRecord,
) -> Result<bool, EvaError> {
    let Some(actual) = read_pid_projection(options)? else {
        return Ok(false);
    };
    if !actual.matches_lease(expected) {
        return Err(
            EvaError::conflict("daemon pid projection belongs to another owner")
                .with_context("path", pid_file(options).display().to_string())
                .with_context("expected_pid", expected.pid().to_string())
                .with_context("actual_pid", actual.pid().to_string())
                .with_context("expected_generation", expected.generation().0.to_string()),
        );
    }
    remove_if_exists(&pid_file(options))
}

/// 读取 `read_agent_control_state` 所需的持久化数据，失败时保留错误上下文。
#[cfg(test)]
fn read_agent_control_state(
    options: &DaemonStartOptions,
) -> Result<Option<DaemonAgentControlState>, EvaError> {
    let path = agent_control_state_file(options);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path).map_err(|error| {
        EvaError::internal("failed to read daemon agent control state")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DaemonAgentControlState::from_storage(&data).map(Some)
}

/// 读取 `read_hardware_hotplug_state` 所需的持久化数据，失败时保留错误上下文。
fn read_hardware_hotplug_state(
    options: &DaemonStartOptions,
) -> Result<Vec<HardwareHotplugDeviceState>, EvaError> {
    let path = hardware_hotplug_state_file(options);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path).map_err(|error| {
        EvaError::internal("failed to read hardware hotplug state")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    parse_hotplug_subscriber_state(&data)
}

/// 持久化 `write_state` 对应的数据，写入失败时返回错误。
fn write_state(options: &DaemonStartOptions, state: &DaemonStateRecord) -> Result<(), EvaError> {
    fs::create_dir_all(&options.state_dir).map_err(|error| {
        EvaError::internal("failed to create daemon state directory")
            .with_context("path", options.state_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let path = state_file(options);
    fs::write(&path, state.to_storage()).map_err(|error| {
        EvaError::internal("failed to write daemon state")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

fn write_shutdown_drain_evidence(
    options: &DaemonStartOptions,
    evidence: &DaemonShutdownDrainEvidence,
) -> Result<(), EvaError> {
    let path = shutdown_drain_evidence_file(options);
    atomic_storage_write(&path, evidence.to_storage().as_bytes()).map_err(|error| {
        EvaError::internal("failed to persist daemon shutdown drain evidence")
            .with_context("path", path.display().to_string())
            .with_context("storage_error", error.to_string())
    })
}

/// 持久化 `write_agent_control_state` 对应的数据，写入失败时返回错误。
fn write_agent_control_state(
    options: &DaemonStartOptions,
    state: &DaemonAgentControlState,
) -> Result<(), EvaError> {
    write_atomic(
        &agent_control_state_file(options),
        &state.to_storage(),
        "failed to write daemon agent control state",
    )
}

/// 持久化 `write_hardware_hotplug_state` 对应的数据，写入失败时返回错误。
fn write_hardware_hotplug_state(
    options: &DaemonStartOptions,
    states: &[HardwareHotplugDeviceState],
) -> Result<(), EvaError> {
    write_atomic(
        &hardware_hotplug_state_file(options),
        &render_hotplug_subscriber_state(states),
        "failed to write hardware hotplug state",
    )
}

/// 停止、取消或释放 `remove_if_exists` 管理的状态。
fn remove_if_exists(path: &Path) -> Result<bool, EvaError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(EvaError::internal("failed to remove daemon file")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())),
    }
}

pub fn daemon_startup_report_path(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
) -> PathBuf {
    startup_dir(options).join(format!("{}.{}", handshake.nonce, STARTUP_REPORT_SUFFIX))
}

pub fn write_daemon_startup_report(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    report_json: &str,
) -> Result<String, EvaError> {
    if report_json.trim() != report_json
        || !report_json.starts_with('{')
        || !report_json.ends_with('}')
    {
        return Err(EvaError::invalid_argument(
            "daemon startup report must be one canonical JSON object",
        ));
    }
    let digest = sha256_digest(report_json.as_bytes());
    write_startup_atomic(
        &daemon_startup_report_path(options, handshake),
        report_json,
        "failed to publish daemon startup report",
    )?;
    Ok(digest)
}

pub fn read_daemon_startup_report(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    expected_digest: &str,
) -> Result<Option<String>, EvaError> {
    if !is_canonical_sha256(expected_digest) {
        return Err(EvaError::conflict(
            "daemon startup report digest is not canonical SHA-256",
        ));
    }
    let path = daemon_startup_report_path(options, handshake);
    let report = match fs::read_to_string(&path) {
        Ok(report) => report,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EvaError::internal("failed to read daemon startup report")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string()))
        }
    };
    let actual_digest = sha256_digest(report.as_bytes());
    if actual_digest != expected_digest {
        return Err(EvaError::conflict("daemon startup report digest mismatch")
            .with_context("path", path.display().to_string())
            .with_context("expected_digest", expected_digest)
            .with_context("actual_digest", actual_digest));
    }
    Ok(Some(report))
}

pub fn read_daemon_startup_frame(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    phase: DaemonStartupPhase,
) -> Result<Option<DaemonStartupFrame>, EvaError> {
    let path = startup_frame_file(options, handshake, phase);
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EvaError::internal("failed to read daemon startup frame")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string()))
        }
    };
    let frame = DaemonStartupFrame::from_storage(&data)?;
    if frame.phase != phase
        || frame.nonce != handshake.nonce
        || frame.launcher_pid != handshake.launcher_pid
        || frame
            .process_start_token
            .as_deref()
            .is_some_and(|token| token != handshake.child_start_token())
    {
        return Err(EvaError::conflict(
            "daemon startup frame does not match the launcher handshake",
        ));
    }
    Ok(Some(frame))
}

pub fn request_daemon_startup_abort(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
) -> Result<(), EvaError> {
    write_startup_atomic(
        &startup_abort_file(options, handshake),
        &format!(
            "format={STARTUP_ABORT_FORMAT}\nnonce={}\nlauncher_pid={}\n",
            handshake.nonce, handshake.launcher_pid
        ),
        "failed to publish daemon startup abort request",
    )
}

pub fn cleanup_failed_daemon_start(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    expected_child_pid: u32,
    claimed: Option<&DaemonStartupFrame>,
    failure: &EvaError,
) -> Result<DaemonStartupCleanupReport, EvaError> {
    if expected_child_pid == 0 {
        return Err(EvaError::invalid_argument(
            "failed daemon startup cleanup requires a positive child pid",
        ));
    }
    if let Some(frame) = claimed {
        frame.validate()?;
        if frame.phase != DaemonStartupPhase::Claimed
            || frame.nonce != handshake.nonce
            || frame.launcher_pid != handshake.launcher_pid
            || frame.child_pid != expected_child_pid
        {
            return Err(EvaError::conflict(
                "failed daemon startup cleanup claimed frame is mismatched",
            ));
        }
    }

    let anchor_path = lock_file(options);
    let lease_path = lease_file(options);
    let probe = probe_runtime_lease(&anchor_path, &lease_path, now_ms())?;
    let identity_from_claimed = claimed
        .map(|frame| {
            DurableRuntimeLeaseIdentity::new(
                expected_child_pid,
                frame.process_start_token.clone().ok_or_else(|| {
                    EvaError::conflict("claimed startup frame is missing process identity")
                })?,
                WriterGeneration(frame.generation.ok_or_else(|| {
                    EvaError::conflict("claimed startup frame is missing writer generation")
                })?),
            )
        })
        .transpose()?;
    let (identity, identity_source) = match (identity_from_claimed, probe.record()) {
        (Some(identity), Some(record)) => {
            if record.identity() != identity {
                return Err(EvaError::conflict(
                    "failed daemon startup cleanup lease identity changed",
                ));
            }
            (Some(identity), "claimed_frame")
        }
        (Some(_), None) => {
            return Err(EvaError::conflict(
                "failed daemon startup cleanup lost the claimed lease record",
            ))
        }
        (None, Some(record))
            if record.pid() == expected_child_pid
                && record.process_start_token() == handshake.child_start_token() =>
        {
            (Some(record.identity()), "lease_probe")
        }
        (None, Some(_record)) => {
            write_daemon_startup_failure_frame(
                options,
                handshake,
                &DaemonStartupFrame::failed(handshake, expected_child_pid, None, failure, true),
            )?;
            return Ok(DaemonStartupCleanupReport {
                child_pid: expected_child_pid,
                identity_source: "other_owner".to_owned(),
                pid_removed: false,
                state_stopped: false,
                lease: None,
                cleanup_complete: true,
            });
        }
        (None, None) => (None, "no_lease"),
    };

    if probe.owner_live() {
        return Err(
            EvaError::conflict("failed daemon startup cleanup refused a live lease owner")
                .with_context("child_pid", expected_child_pid.to_string()),
        );
    }

    let Some(identity) = identity else {
        let cleanup_complete = daemon_startup_cleanup_complete(options)?;
        if !cleanup_complete {
            return Err(EvaError::conflict(
                "failed daemon startup has residue without a reclaimable lease identity",
            ));
        }
        write_daemon_startup_failure_frame(
            options,
            handshake,
            &DaemonStartupFrame::failed(handshake, expected_child_pid, claimed, failure, true),
        )?;
        return Ok(DaemonStartupCleanupReport {
            child_pid: expected_child_pid,
            identity_source: identity_source.to_owned(),
            pid_removed: false,
            state_stopped: false,
            lease: None,
            cleanup_complete: true,
        });
    };

    let previous_state = probe
        .record()
        .map(|record| record.state())
        .ok_or_else(|| EvaError::conflict("failed daemon startup lease record disappeared"))?;
    if previous_state == DurableRuntimeLeaseState::Released {
        let projection = read_pid_projection(options)?;
        let state_running = read_state(options)?.is_some_and(|state| state.status == "running");
        if projection.is_none() && !state_running {
            let lease_report = probe
                .record()
                .map(|record| DaemonLeaseReport::from_record(record, false, false));
            write_daemon_startup_failure_frame(
                options,
                handshake,
                &DaemonStartupFrame::failed(handshake, expected_child_pid, claimed, failure, true),
            )?;
            return Ok(DaemonStartupCleanupReport {
                child_pid: expected_child_pid,
                identity_source: identity_source.to_owned(),
                pid_removed: false,
                state_stopped: false,
                lease: lease_report,
                cleanup_complete: true,
            });
        }
    }
    let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut lease = match previous_state {
        DurableRuntimeLeaseState::Active => DurableRuntimeLeaseGuard::reclaim_failed_start(
            &backend,
            &anchor_path,
            &lease_path,
            &identity,
            now_ms(),
            DEFAULT_RUNTIME_LEASE_TTL_MS,
        )?,
        DurableRuntimeLeaseState::Released => DurableRuntimeLeaseGuard::acquire(
            &backend,
            &anchor_path,
            &lease_path,
            now_ms(),
            DEFAULT_RUNTIME_LEASE_TTL_MS,
        )?,
    };

    let projection = read_pid_projection(options)?;
    let projection_matches = projection
        .as_ref()
        .is_some_and(|projection| projection.matches_identity(&identity));
    if projection.is_some() && !projection_matches {
        return Err(EvaError::conflict(
            "failed daemon startup pid projection belongs to another identity",
        ));
    }

    let mut state_stopped = false;
    if let Some(state) = read_state(options)?.filter(|state| state.status == "running") {
        if state.pid != expected_child_pid
            || (!projection_matches && previous_state == DurableRuntimeLeaseState::Released)
        {
            return Err(EvaError::conflict(
                "failed daemon startup running state is not bound to the reclaimed identity",
            ));
        }
        write_state(options, &state.stopped())?;
        state_stopped = true;
    }
    let pid_removed = if projection_matches {
        remove_if_exists(&pid_file(options))?
    } else {
        false
    };
    let released = lease.release_at(now_ms())?.clone();
    let lease_report = DaemonLeaseReport::from_record(&released, false, false);
    drop(lease);
    let cleanup_complete = daemon_startup_cleanup_complete(options)?;
    if !cleanup_complete {
        return Err(EvaError::conflict(
            "failed daemon startup cleanup did not reach a stable inactive state",
        ));
    }
    write_daemon_startup_failure_frame(
        options,
        handshake,
        &DaemonStartupFrame::failed(handshake, expected_child_pid, claimed, failure, true),
    )?;
    Ok(DaemonStartupCleanupReport {
        child_pid: expected_child_pid,
        identity_source: identity_source.to_owned(),
        pid_removed,
        state_stopped,
        lease: Some(lease_report),
        cleanup_complete: true,
    })
}

pub fn clear_daemon_startup_handshake(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
) -> Result<(), EvaError> {
    for path in [
        startup_frame_file(options, handshake, DaemonStartupPhase::Claimed),
        startup_frame_file(options, handshake, DaemonStartupPhase::Ready),
        startup_frame_file(options, handshake, DaemonStartupPhase::Failed),
        startup_abort_file(options, handshake),
        daemon_startup_report_path(options, handshake),
    ] {
        remove_if_exists(&path)?;
    }
    Ok(())
}

fn write_daemon_startup_frame(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    frame: &DaemonStartupFrame,
) -> Result<(), EvaError> {
    frame.validate()?;
    if frame.nonce != handshake.nonce
        || frame.launcher_pid != handshake.launcher_pid
        || frame
            .process_start_token
            .as_deref()
            .is_some_and(|token| token != handshake.child_start_token())
    {
        return Err(EvaError::conflict(
            "daemon startup frame cannot be published for another launcher",
        ));
    }
    write_startup_atomic(
        &startup_frame_file(options, handshake, frame.phase),
        &frame.to_storage(),
        "failed to publish daemon startup frame",
    )
}

fn write_daemon_startup_failure_frame(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    frame: &DaemonStartupFrame,
) -> Result<(), EvaError> {
    if frame.phase != DaemonStartupPhase::Failed {
        return Err(EvaError::invalid_argument(
            "daemon startup failure publisher requires a failed frame",
        ));
    }
    let ready_path = startup_frame_file(options, handshake, DaemonStartupPhase::Ready);
    match fs::symlink_metadata(&ready_path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_daemon_startup_frame(options, handshake, frame)
        }
        Err(error) => Err(EvaError::internal(
            "failed to inspect daemon ready frame before failure publication",
        )
        .with_context("path", ready_path.display().to_string())
        .with_context("io_error", error.to_string())),
    }
}

fn daemon_startup_abort_requested(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
) -> Result<bool, EvaError> {
    let path = startup_abort_file(options, handshake);
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(
                EvaError::internal("failed to read daemon startup abort request")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string()),
            )
        }
    };
    let expected = format!(
        "format={STARTUP_ABORT_FORMAT}\nnonce={}\nlauncher_pid={}\n",
        handshake.nonce, handshake.launcher_pid
    );
    if data != expected {
        return Err(EvaError::conflict(
            "daemon startup abort request is corrupt or belongs to another launcher",
        ));
    }
    Ok(true)
}

fn ensure_startup_not_aborted(
    options: &DaemonStartOptions,
    handshake: Option<&DaemonStartupHandshake>,
) -> Result<(), EvaError> {
    if let Some(handshake) = handshake {
        if daemon_startup_abort_requested(options, handshake)? {
            return Err(EvaError::conflict(
                "background daemon startup was aborted by its launcher",
            ));
        }
    }
    Ok(())
}

fn daemon_startup_cleanup_complete(options: &DaemonStartOptions) -> Result<bool, EvaError> {
    let probe = probe_runtime_lease(lock_file(options), lease_file(options), now_ms())?;
    let lease_inactive = probe
        .record()
        .map(|record| record.state() == DurableRuntimeLeaseState::Released)
        .unwrap_or(true);
    let state_inactive = read_state(options)?
        .map(|state| state.status != "running")
        .unwrap_or(true);
    Ok(!probe.owner_live()
        && lease_inactive
        && read_pid_projection(options)?.is_none()
        && state_inactive)
}

fn startup_dir(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(STARTUP_DIR)
}

fn startup_frame_file(
    options: &DaemonStartOptions,
    handshake: &DaemonStartupHandshake,
    phase: DaemonStartupPhase,
) -> PathBuf {
    startup_dir(options).join(format!("{}.{}", handshake.nonce, phase.as_str()))
}

fn startup_abort_file(options: &DaemonStartOptions, handshake: &DaemonStartupHandshake) -> PathBuf {
    startup_dir(options).join(format!("{}.abort", handshake.nonce))
}

fn validate_startup_nonce(nonce: &str) -> Result<(), EvaError> {
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(EvaError::invalid_argument(
            "daemon startup nonce must use 1..=128 ASCII alphanumeric, '-' or '_' bytes",
        ));
    }
    Ok(())
}

fn validate_startup_child_token(token: &str) -> Result<(), EvaError> {
    if token.is_empty()
        || token.len() > 128
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(EvaError::invalid_argument(
            "daemon startup child token must use 1..=128 ASCII alphanumeric, '-' or '_' bytes",
        ));
    }
    Ok(())
}

fn is_canonical_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

#[cfg(debug_assertions)]
fn daemon_runtime_lease_ttl_ms(options: &DaemonStartOptions) -> Result<u128, EvaError> {
    let Some(value) = std::env::var_os("EVA_DAEMON_TEST_LEASE_TTL_MS") else {
        return Ok(DEFAULT_RUNTIME_LEASE_TTL_MS);
    };
    if !options.dev_mode {
        return Err(EvaError::invalid_argument(
            "daemon test lease ttl requires explicit dev mode",
        ));
    }
    let ttl_ms = value
        .to_str()
        .ok_or_else(|| EvaError::invalid_argument("daemon test lease ttl is not utf-8"))?
        .parse::<u128>()
        .map_err(|_| EvaError::invalid_argument("daemon test lease ttl is invalid"))?;
    if !(1_000..=DEFAULT_RUNTIME_LEASE_TTL_MS).contains(&ttl_ms) {
        return Err(EvaError::invalid_argument(
            "daemon test lease ttl is outside the supported range",
        )
        .with_context("minimum_ms", "1000")
        .with_context("maximum_ms", DEFAULT_RUNTIME_LEASE_TTL_MS.to_string()));
    }
    Ok(ttl_ms)
}

#[cfg(debug_assertions)]
fn daemon_runtime_heartbeat_interval(options: &DaemonStartOptions) -> Result<Duration, EvaError> {
    let ttl_ms = daemon_runtime_lease_ttl_ms(options)?;
    let interval_ms = if ttl_ms == DEFAULT_RUNTIME_LEASE_TTL_MS {
        DAEMON_LEASE_HEARTBEAT_INTERVAL_MS
    } else {
        u64::try_from((ttl_ms / 3).max(1))
            .map_err(|_| EvaError::internal("daemon test heartbeat interval overflowed"))?
    };
    Ok(Duration::from_millis(interval_ms))
}

#[cfg(not(debug_assertions))]
fn daemon_runtime_lease_ttl_ms(_options: &DaemonStartOptions) -> Result<u128, EvaError> {
    Ok(DEFAULT_RUNTIME_LEASE_TTL_MS)
}

#[cfg(not(debug_assertions))]
fn daemon_runtime_heartbeat_interval(_options: &DaemonStartOptions) -> Result<Duration, EvaError> {
    Ok(Duration::from_millis(DAEMON_LEASE_HEARTBEAT_INTERVAL_MS))
}

#[cfg(debug_assertions)]
fn register_daemon_process_harness_handlers(
    options: &DaemonStartOptions,
    registry: &mut TaskHandlerRegistry,
) -> Result<(), EvaError> {
    let Some(root) = std::env::var_os("EVA_DAEMON_TEST_PROCESS_HARNESS_DIR") else {
        return Ok(());
    };
    if !options.dev_mode {
        return Err(EvaError::invalid_argument(
            "daemon process harness handlers require explicit dev mode",
        ));
    }
    let root = PathBuf::from(root);
    let metadata = fs::symlink_metadata(&root).map_err(|error| {
        EvaError::invalid_argument("daemon process harness directory is unavailable")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(EvaError::invalid_argument(
            "daemon process harness path must be a real directory",
        )
        .with_context("path", root.display().to_string()));
    }
    let root = fs::canonicalize(&root).map_err(|error| {
        EvaError::invalid_argument("daemon process harness directory cannot be canonicalized")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })?;

    let restart_root = root.clone();
    registry.register(
        TaskKind::parse("runtime.process-restart")?,
        move |invocation: &crate::TaskHandlerInvocation<'_>| {
            let started = restart_root.join(format!("restart.started.{}", invocation.attempt()));
            write_process_harness_marker(
                &started,
                format!(
                    "task_id={}\nattempt={}\n",
                    invocation.task_id().as_str(),
                    invocation.attempt()
                )
                .as_bytes(),
                false,
            )?;
            wait_for_process_harness_release(&restart_root.join("restart.release"), invocation)?;
            Ok(crate::TaskHandlerResult::new(invocation.payload()))
        },
    )?;

    let effect_root = root;
    registry.register_non_idempotent(
        TaskKind::parse("runtime.process-effect")?,
        "runtime-process-effect-v1",
        move |invocation: &crate::TaskHandlerInvocation<'_>| {
            let task_id = invocation.task_id().as_str();
            let applied = effect_root.join(format!("effect.applied.{task_id}"));
            if let Err(error) = write_process_harness_marker(
                &applied,
                format!(
                    "task_id={}\nattempt={}\n",
                    invocation.task_id().as_str(),
                    invocation.attempt()
                )
                .as_bytes(),
                true,
            ) {
                write_process_harness_marker(
                    &effect_root.join(format!("effect.duplicate.{task_id}")),
                    error.to_string().as_bytes(),
                    false,
                )?;
                return Err(EvaError::conflict(
                    "daemon process harness effect was invoked more than once",
                )
                .with_retryable(false));
            }
            write_process_harness_marker(
                &effect_root.join(format!("effect.started.{task_id}")),
                format!("task_id={}\n", invocation.task_id().as_str()).as_bytes(),
                false,
            )?;
            wait_for_process_harness_release(
                &effect_root.join(format!("effect.release.{task_id}")),
                invocation,
            )?;
            Ok(crate::TaskHandlerResult::new(invocation.payload()))
        },
    )?;
    Ok(())
}

#[cfg(debug_assertions)]
fn write_process_harness_marker(
    path: &Path,
    bytes: &[u8],
    create_new: bool,
) -> Result<(), EvaError> {
    let mut options = OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    let mut file = options.open(path).map_err(|error| {
        EvaError::internal("failed to create daemon process harness marker")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    file.write_all(bytes).map_err(|error| {
        EvaError::internal("failed to write daemon process harness marker")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        EvaError::internal("failed to sync daemon process harness marker")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

#[cfg(debug_assertions)]
fn wait_for_process_harness_release(
    release: &Path,
    invocation: &crate::TaskHandlerInvocation<'_>,
) -> Result<(), EvaError> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if release.is_file() {
            return Ok(());
        }
        if invocation.cancellation().is_requested() {
            return Err(
                EvaError::unavailable("daemon process harness handler was cancelled")
                    .with_retryable(false),
            );
        }
        if Instant::now() >= deadline {
            return Err(
                EvaError::timeout("daemon process harness handler release timed out")
                    .with_retryable(false),
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(debug_assertions))]
fn register_daemon_process_harness_handlers(
    _options: &DaemonStartOptions,
    _registry: &mut TaskHandlerRegistry,
) -> Result<(), EvaError> {
    Ok(())
}

#[cfg(debug_assertions)]
fn delay_after_startup_lease_for_test() -> Result<(), EvaError> {
    let Some(value) = std::env::var_os("EVA_DAEMON_TEST_LEASE_CLAIM_DELAY_MS") else {
        return Ok(());
    };
    let delay_ms = value
        .to_str()
        .ok_or_else(|| EvaError::invalid_argument("daemon test lease delay is not utf-8"))?
        .parse::<u64>()
        .map_err(|_| EvaError::invalid_argument("daemon test lease delay is invalid"))?;
    thread::sleep(Duration::from_millis(delay_ms));
    Ok(())
}

#[cfg(not(debug_assertions))]
fn delay_after_startup_lease_for_test() -> Result<(), EvaError> {
    Ok(())
}

/// 执行 `state_file` 对应的处理逻辑。
fn state_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(STATE_FILE)
}

fn shutdown_drain_evidence_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(SHUTDOWN_DRAIN_EVIDENCE_FILE)
}

/// 执行 `agent_control_state_file` 对应的处理逻辑。
fn agent_control_state_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(AGENT_CONTROL_STATE_FILE)
}

/// 执行 `hardware_hotplug_state_file` 对应的处理逻辑。
fn hardware_hotplug_state_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(HARDWARE_HOTPLUG_STATE_FILE)
}

/// 执行 `lock_file` 对应的处理逻辑。
fn lock_file(options: &DaemonStartOptions) -> PathBuf {
    options.lock_dir.join(LOCK_FILE)
}

/// 返回与固定 lock anchor 分离、可原子替换的 lease record 路径。
fn lease_file(options: &DaemonStartOptions) -> PathBuf {
    options.lock_dir.join(LEASE_FILE)
}

/// 执行 `pid_file` 对应的处理逻辑。
fn pid_file(options: &DaemonStartOptions) -> PathBuf {
    options.pid_dir.join(PID_FILE)
}

/// 执行 `control_request_dir` 对应的处理逻辑。
fn control_request_dir(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(CONTROL_REQUEST_DIR)
}

/// 执行 `control_response_dir` 对应的处理逻辑。
fn control_response_dir(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(CONTROL_RESPONSE_DIR)
}

/// 执行 `control_request_file` 对应的处理逻辑。
fn control_request_file(options: &DaemonStartOptions, request_id: &RequestId) -> PathBuf {
    control_request_dir(options).join(format!("{}.{}", request_id.as_str(), CONTROL_REQUEST_EXT))
}

/// 执行 `control_response_file` 对应的处理逻辑。
fn control_response_file(options: &DaemonStartOptions, request_id: &RequestId) -> PathBuf {
    control_response_dir(options).join(format!("{}.{}", request_id.as_str(), CONTROL_RESPONSE_EXT))
}

/// 执行 `resolve_project_path` 对应的处理逻辑。
fn resolve_project_path(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

/// 执行 `display_path` 对应的处理逻辑。
fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// 执行 `now_ms` 对应的处理逻辑。
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// 执行 `trace_id` 对应的处理逻辑。
fn trace_id(trace: &TraceFields) -> String {
    trace.continuity_key().unwrap_or_else(|| {
        trace
            .span_id
            .as_ref()
            .map(|value| format!("span_id:{}", value.as_str()))
            .unwrap_or_else(|| "span_id:daemon.control".to_owned())
    })
}

/// 解析 `parse_bool` 对应的数据，并拒绝无效格式。
fn parse_bool(value: &str, field: &str) -> Result<bool, EvaError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(
            EvaError::conflict("daemon control boolean field is invalid")
                .with_context("field", field)
                .with_context("value", value),
        ),
    }
}

/// 解析 `parse_optional_usize` 对应的数据，并拒绝无效格式。
#[cfg(test)]
fn parse_optional_usize(value: &str, message: &'static str) -> Result<Option<usize>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<usize>()
            .map(Some)
            .map_err(|_| EvaError::conflict(message))
    }
}

/// 解析 `parse_optional_u64` 对应的数据，并拒绝无效格式。
fn parse_optional_u64(value: &str, message: &'static str) -> Result<Option<u64>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| EvaError::conflict(message))
    }
}

/// 解析可选 u32 字段，并拒绝无效格式。
fn parse_optional_u32(value: &str, message: &'static str) -> Result<Option<u32>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u32>()
            .map(Some)
            .map_err(|_| EvaError::conflict(message))
    }
}

/// 解析可选 u128 字段，并拒绝无效格式。
fn parse_optional_u128(value: &str, message: &'static str) -> Result<Option<u128>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u128>()
            .map(Some)
            .map_err(|_| EvaError::conflict(message))
    }
}

/// 按稳定存储格式编码 `encode_optional_field` 对应的数据。
fn encode_optional_field(value: Option<&str>) -> String {
    value.map(encode_field).unwrap_or_default()
}

/// 解析 `decode_optional_field` 对应的数据，并拒绝无效格式。
fn decode_optional_field(value: &str) -> Result<Option<String>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        decode_field(value).map(Some)
    }
}

/// 按稳定存储格式编码 `encode_audit` 对应的数据。
fn encode_audit(values: &[String]) -> String {
    values
        .iter()
        .map(|value| encode_field(value))
        .collect::<Vec<_>>()
        .join(",")
}

/// 解析 `decode_audit` 对应的数据，并拒绝无效格式。
fn decode_audit(value: &str) -> Result<Vec<String>, EvaError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value.split(',').map(decode_field).collect()
}

fn required_control_field<T>(value: Option<T>, field: &'static str) -> Result<T, EvaError> {
    value.ok_or_else(|| {
        EvaError::conflict("daemon control request v2 is missing a field")
            .with_context("field", field)
    })
}

fn encode_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_bytes(value: &str, field: &'static str) -> Result<Vec<u8>, EvaError> {
    if !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(EvaError::conflict(
            "daemon control binary field is not canonical lowercase hex",
        )
        .with_context("field", field));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = if pair[0].is_ascii_digit() {
            pair[0] - b'0'
        } else {
            pair[0] - b'a' + 10
        };
        let low = if pair[1].is_ascii_digit() {
            pair[1] - b'0'
        } else {
            pair[1] - b'a' + 10
        };
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// 按稳定存储格式编码 `encode_field` 对应的数据。
fn encode_field(value: &str) -> String {
    /// 定义 `HEX` 常量。
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// 解析 `decode_field` 对应的数据，并拒绝无效格式。
fn decode_field(value: &str) -> Result<String, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict(
            "daemon control encoded field has odd length",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for index in (0..value.len()).step_by(2) {
        let byte = u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| {
            EvaError::conflict("daemon control encoded field is not hex")
                .with_context("offset", index.to_string())
        })?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).map_err(|error| {
        EvaError::conflict("daemon control encoded field is not utf-8")
            .with_context("utf8_error", error.to_string())
    })
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_adapter::OsProcessBackend;
    use eva_config::load_project_config;
    use eva_core::{Event, EventId, EventPayload, Topic};
    use eva_eventbus::{EventBus, ReplayHandlerBinding};
    use eva_storage::{
        FileSystemProviderProcessTable, ProviderProcessSnapshot, ProviderProcessTable,
    };
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn daemon_test_guard() -> MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn rejected_config_preflight_audit_contains_digest_field_and_remediation() {
        let report = crate::ConfigReloadPreflight {
            old_digest: "sha256:old".to_owned(),
            candidate_digest: None,
            changed_paths: vec!["config/routes/topics.yaml".to_owned()],
            outcome: ConfigReloadPreflightOutcome::Rejected {
                error_kind: "invalid_argument".to_owned(),
                error_field: "routes".to_owned(),
                error_message: "route references an unknown agent".to_owned(),
                remediation: "correct the reported configuration field".to_owned(),
            },
            candidate: None,
        };

        let event = config_reload_preflight_audit(&report);
        assert_eq!(event.action, AuditAction::ConfigValidated);
        assert_eq!(event.outcome, AuditOutcome::Blocked);
        assert!(event
            .fields
            .contains(&("old_digest".to_owned(), "sha256:old".to_owned())));
        assert!(event
            .fields
            .contains(&("candidate_digest".to_owned(), "unavailable".to_owned())));
        assert!(event
            .fields
            .contains(&("error_field".to_owned(), "routes".to_owned())));
        assert!(event
            .fields
            .iter()
            .any(|(key, value)| { key == "remediation" && value.contains("correct") }));
        assert!(event
            .fields
            .contains(&("active_generation_changed".to_owned(), "false".to_owned())));
    }

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 执行 `temp_root` 对应的处理逻辑。
    fn temp_root(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "eva-runtime-daemon-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn read_tree_text(root: &Path) -> String {
        let mut pending = vec![root.to_path_buf()];
        let mut text = String::new();
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(
                    fs::read_dir(path)
                        .unwrap()
                        .filter_map(Result::ok)
                        .map(|entry| entry.path()),
                );
            } else if let Ok(bytes) = fs::read(path) {
                text.push_str(&String::from_utf8_lossy(&bytes));
                text.push('\n');
            }
        }
        text
    }

    /// 执行 `daemon_options` 对应的处理逻辑。
    fn daemon_options(root: &Path, shutdown_after_smoke: bool) -> DaemonStartOptions {
        DaemonStartOptions {
            durable_backend: root.join("durable"),
            state_dir: root.join("state"),
            lock_dir: root.join("locks"),
            pid_dir: root.join("pids"),
            observability_backend: root.join("observability"),
            foreground: true,
            dev_mode: true,
            shutdown_after_smoke,
        }
    }

    fn sample_task_envelope(input: Vec<u8>) -> crate::TaskEnvelope {
        crate::TaskEnvelope::new(
            crate::TaskKind::parse("runtime.echo").unwrap(),
            eva_core::AgentId::parse("root-agent").unwrap(),
            crate::TaskInput::inline(input).unwrap(),
            crate::IdempotencyKey::parse("idem-daemon-envelope").unwrap(),
            crate::TaskAttemptPolicy::new(3, 250, Some(5_000)).unwrap(),
        )
        .unwrap()
    }

    #[test]
    /// v2 control mailbox 对完整 TaskEnvelope 做无损往返，v1 字段布局不再承载新 payload。
    fn daemon_control_request_v2_round_trips_task_envelope() {
        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-envelope-wire").unwrap());
        let input = b"daemon-debug-secret".to_vec();
        let leaked_bytes = format!("{input:?}");
        let request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-envelope-wire").unwrap(),
            &trace,
            DaemonControlOperation::SubmitTask,
        )
        .with_task_id("req-daemon-envelope-task")
        .with_task_envelope(sample_task_envelope(input));

        let stored = request.to_storage();
        let reopened = DaemonControlRequest::from_storage(&stored).unwrap();
        let request_debug = format!("{request:?}");
        let mismatched = request.clone().with_agent_id("spoof-agent").to_storage();
        let mismatch_error = DaemonControlRequest::from_storage(&mismatched).unwrap_err();

        assert!(stored.starts_with("version=2\n"));
        assert!(!request_debug.contains(&leaked_bytes));
        assert!(request_debug.contains("bytes: \"<redacted>\""));
        assert!(request_debug.contains("size_bytes: 19"));
        assert_eq!(mismatch_error.kind(), eva_core::ErrorKind::Conflict);
        assert!(mismatch_error.message().contains("does not match"));
        assert_eq!(reopened, request);
        assert_eq!(
            reopened.task_envelope.as_ref().unwrap().kind().as_str(),
            "runtime.echo"
        );
    }

    #[test]
    /// 旧 v1 submit mailbox 仍可读取，但服务端明确补成不可执行的 legacy.submit 信封。
    fn daemon_control_request_v1_submit_uses_explicit_legacy_envelope() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let expected_agent_id = project
            .agents
            .iter()
            .find(|agent| agent.enabled)
            .unwrap()
            .id
            .as_str()
            .to_owned();
        let expected_agent_json = format!("\"agent_id\":\"{expected_agent_id}\"");
        let root = temp_root("legacy-submit-wire");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-v1-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);
        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-v1-submit").unwrap());
        let mut request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-v1-submit").unwrap(),
            &trace,
            DaemonControlOperation::SubmitTask,
        )
        .with_task_id("req-daemon-v1-task");
        request.wire_version = 1;
        assert!(request.to_storage().starts_with("version=1\n"));

        let spoof_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-v1-spoof-submit").unwrap());
        let mut spoof_request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-v1-spoof-submit").unwrap(),
            &spoof_trace,
            DaemonControlOperation::SubmitTask,
        )
        .with_task_id("req-daemon-v1-spoof-task")
        .with_agent_id("spoof-agent");
        spoof_request.wire_version = 1;
        assert!(DaemonControlRequest::from_storage(&spoof_request.to_storage()).is_err());
        fs::write(
            control_request_file(&options, &spoof_request.request_id),
            spoof_request.to_storage(),
        )
        .unwrap();
        let mut spoof_rejected = false;
        for _ in 0..100 {
            spoof_rejected = fs::read_dir(control_request_dir(&options))
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.path().extension().and_then(|value| value.to_str())
                        == Some(CONTROL_REJECTED_EXT)
                });
            if spoof_rejected {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(spoof_rejected);
        assert!(!options
            .durable_backend
            .join("tasks")
            .join("req-daemon-v1-spoof-task.task")
            .exists());

        send_daemon_control_request(&options, request, 2_000).unwrap();
        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-v1-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-v1-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();

        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let reopened = FileSystemTaskStateStore::from_durable_layout(backend.layout())
            .read(Some("req-daemon-v1-task"))
            .unwrap();
        let envelope = reopened.envelope.unwrap();
        assert_eq!(envelope.kind, "legacy.submit".to_owned());
        assert_eq!(envelope.agent_id, expected_agent_id);
        let audit = fs::read_to_string(options.observability_backend.join("audit.jsonl")).unwrap();
        let metrics =
            fs::read_to_string(options.observability_backend.join("metrics.jsonl")).unwrap();
        let spans =
            fs::read_to_string(options.observability_backend.join("otel-spans.jsonl")).unwrap();
        assert!(!audit.contains("req-daemon-v1-spoof-submit"));
        let submit_audit = audit
            .lines()
            .filter(|line| line.contains("req-daemon-v1-submit"))
            .collect::<Vec<_>>();
        assert!(!submit_audit.is_empty());
        assert!(submit_audit
            .iter()
            .all(|line| line.contains(&expected_agent_json)));
        let task_metrics = metrics
            .lines()
            .filter(|line| line.contains("req-daemon-v1-task"))
            .collect::<Vec<_>>();
        assert!(!task_metrics.is_empty());
        assert!(task_metrics
            .iter()
            .all(|line| line.contains(&expected_agent_json)));
        let submit_spans = spans
            .lines()
            .filter(|line| line.contains("req-daemon-v1-submit"))
            .collect::<Vec<_>>();
        assert!(!submit_spans.is_empty());
        assert!(submit_spans
            .iter()
            .all(|line| line.contains(&expected_agent_json)));

        fs::remove_dir_all(root).ok();
    }

    /// 执行 `wait_for_daemon_available` 对应的处理逻辑。
    fn wait_for_daemon_available(options: &DaemonStartOptions) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let last_observation = match daemon_status(options) {
                Ok(report) if report.available => return,
                Ok(report) => format!("{report:?}"),
                Err(error) => format!("{error:?}"),
            };
            if Instant::now() >= deadline {
                panic!("daemon did not become available: {last_observation}");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// 执行 `wait_for_scheduler_retry_ack` 对应的处理逻辑。
    fn wait_for_scheduler_retry_ack(options: &DaemonStartOptions, event_id: &str) {
        let replay_id = EventId::parse(&format!("{event_id}:replay-1")).unwrap();
        for _ in 0..100 {
            let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
                &options.durable_backend,
            ))
            .unwrap();
            let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();
            if bus.event_log_status(&replay_id) == Some(eva_storage::EventLogStatus::Acked) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("scheduler retry did not ack replay event");
    }

    /// 执行 `durable_event` 对应的处理逻辑。
    fn durable_event(event_id: &str, topic: &str) -> Event {
        Event::new(
            EventId::parse(event_id).unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
        .with_request_id(RequestId::parse("req-daemon-retry-handler").unwrap())
    }

    /// 执行 `daemon_provider_process` 对应的处理逻辑。
    fn daemon_provider_process(session_id: &str, request_id: &str) -> ProviderProcessSnapshot {
        let mut snapshot = ProviderProcessSnapshot::running(
            session_id,
            format!("proc-{session_id}"),
            RequestId::parse(request_id).unwrap(),
            eva_core::AdapterId::parse("stdio-test").unwrap(),
            eva_core::CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "fnv64:0123456789abcdef",
            "stdio-runner --once",
            "none",
        );
        let mut command = daemon_provider_sleep_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut handle = OsProcessBackend::new().spawn(command).unwrap();
        handle.identity().stamp_snapshot(&mut snapshot, 1).unwrap();
        handle.force_terminate().unwrap();
        snapshot
    }

    #[cfg(unix)]
    fn daemon_provider_sleep_command() -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command
    }

    #[cfg(windows)]
    fn daemon_provider_sleep_command() -> Command {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "ping", "127.0.0.1", "-n", "31"]);
        command
    }

    #[cfg(not(any(unix, windows)))]
    fn daemon_provider_sleep_command() -> Command {
        Command::new("unsupported")
    }

    /// 验证 `daemon_start_smoke_verifies_boundaries_and_stops` 场景下的预期行为。
    #[test]
    fn daemon_start_smoke_verifies_boundaries_and_stops() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("start");
        let options = daemon_options(&root, true);

        let report = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();

        assert_eq!(report.status, "stopped");
        assert!(!report.provider_processes_started);
        assert_eq!(report.hardware_hotplug.status, "ready");
        assert!(!report.hardware_hotplug.raw_handles_exposed);
        assert_eq!(report.hardware_hotplug.devices_seen, 1);
        assert_eq!(report.hardware_hotplug.events_published.len(), 1);
        let maintenance = report.memory_maintenance.as_ref().unwrap();
        assert_eq!(maintenance.status, "ready");
        assert_eq!(maintenance.memory_gc.expired_removed, 0);
        assert_eq!(maintenance.knowledge_rebuild.items_indexed, 0);
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "daemon:v1.15.6:memory_maintenance_ready"));
        assert!(report
            .audit
            .iter()
            .any(|entry| { entry == "daemon:w1-l05:task_handler_registry_ready:runtime.echo" }));
        assert!(report.audit.iter().any(|entry| {
            entry
                == &format!(
                    "daemon:w1-l05:task_artifact_input_limit_bytes:{}",
                    crate::DEFAULT_TASK_ARTIFACT_INPUT_LIMIT_BYTES
                )
        }));
        assert!(hardware_hotplug_state_file(&options).is_file());
        assert!(options
            .durable_backend
            .join("state")
            .join("memory")
            .join("memory-gc.checkpoint")
            .is_file());
        assert!(options
            .durable_backend
            .join("state")
            .join("knowledge")
            .join("knowledge-rebuild.checkpoint")
            .is_file());
        assert!(report.shutdown.is_some());
        assert!(state_file(&options).is_file());
        assert!(lock_file(&options).is_file());
        let lease =
            probe_runtime_lease(lock_file(&options), lease_file(&options), now_ms()).unwrap();
        assert!(!lease.owner_live());
        assert_eq!(
            lease.record().map(DurableRuntimeLeaseRecord::state),
            Some(DurableRuntimeLeaseState::Released)
        );
        assert!(!pid_file(&options).exists());

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_hotplug_subscriber_persists_state_across_restart` 场景下的预期行为。
    #[test]
    fn daemon_hotplug_subscriber_persists_state_across_restart() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("hotplug-subscriber");
        let options = daemon_options(&root, true);

        let first = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();
        assert_eq!(first.hardware_hotplug.events_published.len(), 1);
        assert!(first.memory_maintenance.is_some());
        let schedule_store =
            FileSystemScheduleStore::new(options.state_dir.join("schedules")).unwrap();
        let scheduled = schedule_store.read(MEMORY_MAINTENANCE_SCHEDULE_ID).unwrap();
        let state = read_hardware_hotplug_state(&options).unwrap();
        assert_eq!(state.len(), 1);

        let second = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();
        assert!(second.hardware_hotplug.events_published.is_empty());
        assert!(second.memory_maintenance.is_none());
        assert_eq!(
            schedule_store.read(MEMORY_MAINTENANCE_SCHEDULE_ID).unwrap(),
            scheduled
        );
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();

        assert_eq!(bus.log().records().len(), 1);
        assert_eq!(
            bus.log().records()[0].event.topic().as_str(),
            "/hardware/disconnected"
        );
        assert_eq!(
            bus.log().records()[0].event.payload().as_text(),
            Some("{\"device_id\":\"scale-main:main-scale\",\"action\":\"remove\",\"previous\":\"disconnected\",\"next\":\"disconnected\",\"reason\":\"manifest snapshot disconnected -> disconnected\"}")
        );

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_start_recovers_interrupted_provider_process_state` 场景下的预期行为。
    #[test]
    fn daemon_start_recovers_interrupted_provider_process_state() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("provider-recovery");
        let options = daemon_options(&root, true);
        {
            let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
                &options.durable_backend,
            ))
            .unwrap();
            let mut task_store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut process_table = FileSystemProviderProcessTable::from_runtime_writer(
                backend.layout(),
                task_store.runtime_writer().unwrap(),
            )
            .unwrap();
            let mut task = TaskStateSnapshot::queued("req-daemon-provider-recovery").unwrap();
            task.mark_running(100, None, "cancel-token-provider-recovery");
            task_store.write(&task).unwrap();
            process_table
                .upsert(daemon_provider_process(
                    "session-daemon-provider-recovery",
                    "req-daemon-provider-recovery",
                ))
                .unwrap();
        }

        let report = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let task_store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let process_table = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let task = task_store
            .read(Some("req-daemon-provider-recovery"))
            .unwrap();
        let process = process_table
            .read("session-daemon-provider-recovery")
            .unwrap();

        assert_eq!(report.recovery.scanned_provider_processes, 1);
        assert_eq!(report.recovery.recovered_provider_processes.len(), 1);
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "daemon:v1.13.5:provider_recovery_scanned"));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "daemon:v1.13.5:provider_orphan_scan_completed"));
        assert!(report.recovery.audit.iter().any(|entry| {
            entry
                == "runtime.recovery:provider_orphan:session-daemon-provider-recovery:already_exited"
        }));
        assert_eq!(task.status, "interrupted");
        assert!(!process.active);
        assert_eq!(process.health, "interrupted");

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_lock_conflict_blocks_start_before_state_write` 场景下的预期行为。
    #[test]
    fn daemon_lock_conflict_blocks_start_before_state_write() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("lock");
        let options = daemon_options(&root, true);
        fs::create_dir_all(&options.lock_dir).unwrap();
        fs::write(lock_file(&options), "pid=1\n").unwrap();

        let error = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(!state_file(&options).exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn daemon_status_binds_pid_to_live_lease_and_stop_refuses_live_owner() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("status-lease-identity");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-lease-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);

        let live = daemon_status(&options).unwrap();
        let live_lease = live.lease.as_ref().unwrap();
        assert!(live.available);
        assert!(live.pid_matches_lease);
        assert_eq!(live_lease.state, "active");
        assert!(live_lease.owner_live);
        assert!(!live_lease.expired);
        assert_eq!(live_lease.pid, std::process::id());
        assert!(live_lease.generation > 0);

        let pid_projection_bytes = fs::read(pid_file(&options)).unwrap();
        fs::write(
            pid_file(&options),
            format!(
                "format={PID_PROJECTION_FORMAT}\npid={}\nprocess_start_token=old-incarnation\ngeneration={}\n",
                std::process::id(),
                live_lease.generation
            ),
        )
        .unwrap();
        let mismatched = daemon_status(&options).unwrap();
        assert!(!mismatched.available);
        assert!(!mismatched.pid_matches_lease);
        fs::write(pid_file(&options), std::process::id().to_string()).unwrap();
        let legacy = daemon_status(&options).unwrap();
        assert!(!legacy.available);
        assert!(!legacy.pid_matches_lease);
        fs::write(pid_file(&options), pid_projection_bytes).unwrap();
        assert!(daemon_status(&options).unwrap().available);

        let competing_start = start_daemon(
            &project,
            options.clone(),
            &TraceFields::default()
                .with_request_id(RequestId::parse("req-daemon-competing-start").unwrap()),
        )
        .unwrap_err();
        assert_eq!(competing_start.kind(), eva_core::ErrorKind::Conflict);
        assert!(daemon_status(&options).unwrap().available);

        let stop_error = stop_daemon(&options).unwrap_err();
        assert_eq!(stop_error.kind(), eva_core::ErrorKind::Conflict);
        let after_stop_attempt = daemon_status(&options).unwrap();
        assert!(after_stop_attempt.available, "{after_stop_attempt:?}");

        let status_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-lease-status").unwrap());
        let status = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-lease-status").unwrap(),
                &status_trace,
                DaemonControlOperation::Status,
            ),
            2_000,
        )
        .unwrap();
        assert_eq!(
            status.lease.as_ref().unwrap().generation,
            live_lease.generation
        );
        assert!(status.lease.as_ref().unwrap().owner_live);
        let response_wire = status.to_storage();
        assert!(response_wire.starts_with("version=2\n"));
        assert_eq!(
            DaemonControlResponse::from_storage(&response_wire).unwrap(),
            status
        );
        let legacy_wire = response_wire
            .lines()
            .filter(|line| !line.starts_with("lease_"))
            .collect::<Vec<_>>()
            .join("\n")
            .replacen("version=2", "version=1", 1)
            + "\n";
        assert!(DaemonControlResponse::from_storage(&legacy_wire)
            .unwrap()
            .lease
            .is_none());

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-lease-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-lease-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();

        let released = daemon_status(&options).unwrap();
        assert!(!released.available);
        assert_eq!(released.lease.as_ref().unwrap().state, "released");
        assert!(!released.lease.as_ref().unwrap().owner_live);
        assert!(lock_file(&options).is_file());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn shutdown_pid_identity_failure_exits_and_releases_lease() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("shutdown-pid-mismatch");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-pid-mismatch-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);
        let lease = daemon_status(&options).unwrap().lease.unwrap();
        fs::write(
            pid_file(&options),
            format!(
                "format={PID_PROJECTION_FORMAT}\npid={}\nprocess_start_token=stale-owner\ngeneration={}\n",
                lease.pid, lease.generation
            ),
        )
        .unwrap();
        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-pid-mismatch-shutdown").unwrap());
        let request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-pid-mismatch-shutdown").unwrap(),
            &trace,
            DaemonControlOperation::Shutdown,
        );
        write_control_request(
            &control_request_file(&options, &request.request_id),
            &request,
        )
        .unwrap();

        let mut released = false;
        for _ in 0..100 {
            let probe =
                probe_runtime_lease(lock_file(&options), lease_file(&options), now_ms()).unwrap();
            if !probe.owner_live()
                && probe.record().map(DurableRuntimeLeaseRecord::state)
                    == Some(DurableRuntimeLeaseState::Released)
            {
                released = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(released, "shutdown failure did not release daemon lease");
        let error = daemon.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(read_state(&options).unwrap().unwrap().status, "stopped");
        assert!(pid_file(&options).is_file());
        assert!(!control_response_file(&options, &request.request_id).exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn stop_daemon_waits_for_dead_fresh_lease_and_reclaims_dead_expired_lease() {
        let _daemon_test_guard = daemon_test_guard();
        let root = temp_root("stop-stale-lease");
        let options = daemon_options(&root, true);
        fs::create_dir_all(&options.lock_dir).unwrap();
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
            &options.durable_backend,
        ))
        .unwrap();
        let claimed_at = now_ms();
        let mut owner = DurableRuntimeLeaseGuard::acquire(
            &backend,
            lock_file(&options),
            lease_file(&options),
            claimed_at,
            DEFAULT_RUNTIME_LEASE_TTL_MS,
        )
        .unwrap();
        let owner_record = owner.record().clone();
        owner.release_at(claimed_at).unwrap();
        drop(owner);
        let generation_before = fs::read(&backend.layout().writer_generation_path).unwrap();
        let fresh_expiry = claimed_at + DEFAULT_RUNTIME_LEASE_TTL_MS;
        fs::write(
            lease_file(&options),
            format!(
                "format=eva.daemon-lease.v1\nstate=active\npid={}\nprocess_start_token={}\ngeneration={}\nheartbeat_at_ms={}\nexpires_at_ms={}\n",
                owner_record.pid(),
                owner_record.process_start_token(),
                owner_record.generation().0,
                claimed_at,
                fresh_expiry
            ),
        )
        .unwrap();

        let fresh_error = stop_daemon(&options).unwrap_err();
        assert_eq!(fresh_error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            fs::read(&backend.layout().writer_generation_path).unwrap(),
            generation_before
        );

        fs::write(
            lease_file(&options),
            format!(
                "format=eva.daemon-lease.v1\nstate=active\npid={}\nprocess_start_token={}\ngeneration={}\nheartbeat_at_ms=1\nexpires_at_ms=2\n",
                owner_record.pid(),
                owner_record.process_start_token(),
                owner_record.generation().0
            ),
        )
        .unwrap();
        let stopped = stop_daemon(&options).unwrap();
        assert!(stopped.mutation_executed);
        assert!(!stopped.lock_removed);
        assert_eq!(stopped.lease.as_ref().unwrap().state, "released");
        assert_eq!(
            stopped.lease.as_ref().unwrap().generation,
            owner_record.generation().0 + 1
        );
        assert!(lock_file(&options).is_file());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn daemon_heartbeat_scheduler_renews_only_after_monotonic_deadline() {
        let root = temp_root("heartbeat-scheduler");
        let options = daemon_options(&root, true);
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
            &options.durable_backend,
        ))
        .unwrap();
        let mut lease = DurableRuntimeLeaseGuard::acquire(
            &backend,
            lock_file(&options),
            lease_file(&options),
            now_ms(),
            DEFAULT_RUNTIME_LEASE_TTL_MS,
        )
        .unwrap();
        let initial = lease.record().clone();
        let interval = Duration::from_millis(10);
        let mut next_heartbeat = Instant::now() + interval;

        assert!(!renew_daemon_lease_if_due(&mut lease, &mut next_heartbeat, interval).unwrap());
        assert_eq!(lease.record(), &initial);

        thread::sleep(interval);
        assert!(renew_daemon_lease_if_due(&mut lease, &mut next_heartbeat, interval).unwrap());
        assert!(lease.record().heartbeat_at_ms() >= initial.heartbeat_at_ms());
        assert!(lease.record().expires_at_ms() >= initial.expires_at_ms());

        lease.release_at(now_ms()).unwrap();
        drop(lease);
        drop(backend);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn daemon_freshness_transitions_after_heartbeat_stops() {
        let heartbeat_at_ms = 1_000;
        let lease = DaemonLeaseReport {
            state: "active".to_owned(),
            pid: 42,
            process_start_token: "daemon-freshness".to_owned(),
            generation: 7,
            heartbeat_at_ms,
            expires_at_ms: heartbeat_at_ms + DAEMON_LEASE_STALE_AFTER_MS,
            owner_live: true,
            expired: false,
        };

        assert_eq!(
            lease.freshness_at(heartbeat_at_ms + DAEMON_LEASE_DEGRADED_AFTER_MS - 1),
            DaemonFreshness::Live
        );
        assert_eq!(
            lease.freshness_at(heartbeat_at_ms + DAEMON_LEASE_DEGRADED_AFTER_MS),
            DaemonFreshness::Degraded
        );
        assert_eq!(
            lease.freshness_at(heartbeat_at_ms + DAEMON_LEASE_STALE_AFTER_MS - 1),
            DaemonFreshness::Degraded
        );
        assert_eq!(
            lease.freshness_at(heartbeat_at_ms + DAEMON_LEASE_STALE_AFTER_MS),
            DaemonFreshness::Stale
        );

        let mut dead_owner = lease;
        dead_owner.owner_live = false;
        assert_eq!(
            dead_owner.freshness_at(heartbeat_at_ms),
            DaemonFreshness::Stale
        );

        let mut released = dead_owner;
        released.state = "released".to_owned();
        assert_eq!(
            released.freshness_at(heartbeat_at_ms),
            DaemonFreshness::Stale
        );
        let mut expired = released;
        expired.state = "active".to_owned();
        expired.owner_live = true;
        expired.expired = true;
        assert_eq!(
            expired.freshness_at(heartbeat_at_ms),
            DaemonFreshness::Stale
        );
    }

    /// 验证 `daemon_control_status_and_shutdown_round_trip_has_trace_id` 场景下的预期行为。
    #[test]
    fn daemon_control_status_and_shutdown_round_trip_has_trace_id() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("control");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-loop").unwrap()),
            )
        });

        wait_for_daemon_available(&options);

        let status_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-control-status").unwrap());
        let status = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-control-status").unwrap(),
                &status_trace,
                DaemonControlOperation::Status,
            ),
            2_000,
        )
        .unwrap();

        assert!(status.accepted);
        assert!(status.daemon_available);
        assert_eq!(status.operation, DaemonControlOperation::Status);
        assert_eq!(status.trace_id, "request_id:req-daemon-control-status");
        assert_eq!(status.status, "running");
        assert!(Path::new(&status.response_file).is_file());

        let invalid_shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-invalid-shutdown").unwrap());
        let invalid_shutdown = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-invalid-shutdown").unwrap(),
                &invalid_shutdown_trace,
                DaemonControlOperation::Shutdown,
            )
            .with_timeout_ms(1),
            2_000,
        )
        .unwrap_err();
        assert_eq!(
            invalid_shutdown.kind(),
            eva_core::ErrorKind::InvalidArgument
        );
        assert!(daemon_status(&options).unwrap().available);

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-control-shutdown").unwrap());
        let shutdown = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-control-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();

        assert_eq!(shutdown.operation, DaemonControlOperation::Shutdown);
        assert_eq!(shutdown.status, "stopped");
        assert!(shutdown.mutation_executed);
        assert!(shutdown.shutdown.is_some());

        let report = daemon.join().unwrap().unwrap();
        assert_eq!(report.status, "stopped");
        assert!(report.shutdown.is_some());
        assert!(lock_file(&options).is_file());
        let lease =
            probe_runtime_lease(lock_file(&options), lease_file(&options), now_ms()).unwrap();
        assert!(!lease.owner_live());
        assert_eq!(
            lease.record().map(DurableRuntimeLeaseRecord::state),
            Some(DurableRuntimeLeaseState::Released)
        );
        assert!(!pid_file(&options).exists());

        let repeated_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-control-shutdown-again").unwrap());
        let repeated = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-control-shutdown-again").unwrap(),
                &repeated_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        assert!(repeated.accepted);
        assert!(!repeated.mutation_executed);
        assert!(!repeated.daemon_available);
        assert_eq!(repeated.status, "stopped");
        assert_eq!(
            repeated
                .shutdown
                .as_ref()
                .map(|shutdown| shutdown.phase.as_str()),
            Some("already_shutdown")
        );
        assert_eq!(
            repeated.lease.as_ref().map(|lease| lease.generation),
            Some(report.lease.generation)
        );
        let evidence = read_shutdown_drain_evidence(&options).unwrap().unwrap();
        assert_eq!(evidence.generation, report.lease.generation);
        assert_eq!(evidence.phase, "drained");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn repeated_shutdown_requires_generation_bound_drain_evidence() {
        let root = temp_root("shutdown-drain-evidence");
        let options = daemon_options(&root, false);
        fs::create_dir_all(&options.state_dir).unwrap();
        let state = DaemonStateRecord {
            status: "stopped".to_owned(),
            mode: "foreground".to_owned(),
            pid: 4242,
            generation_id: DAEMON_GENERATION.to_owned(),
            project_root: root.display().to_string(),
            started_at_ms: 10,
            stopped_at_ms: Some(20),
        };
        let status = DaemonStatusReport {
            available: false,
            status: "stopped".to_owned(),
            lock_present: true,
            pid_present: false,
            pid_matches_lease: false,
            freshness: "stale".to_owned(),
            heartbeat_age_ms: Some(0),
            lease: Some(DaemonLeaseReport {
                state: "released".to_owned(),
                pid: 4242,
                process_start_token: "shutdown-evidence-owner".to_owned(),
                generation: 7,
                heartbeat_at_ms: 20,
                expires_at_ms: 30,
                owner_live: false,
                expired: false,
            }),
            paths: DaemonPathReport::from_options(&options),
            state: Some(state),
        };
        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-evidence-repeat").unwrap());
        let request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-evidence-repeat").unwrap(),
            &trace,
            DaemonControlOperation::Shutdown,
        );

        assert!(repeated_shutdown_response(&options, &request, &status)
            .unwrap()
            .is_none());

        let mut evidence = DaemonShutdownDrainEvidence {
            pid: 4242,
            process_start_token: "shutdown-evidence-owner".to_owned(),
            generation: 6,
            request_id: RequestId::parse("req-daemon-evidence-original").unwrap(),
            completed_at_ms: 20,
            inflight_tasks: 1,
            cancellation_requests: 1,
            forced_terminal_tasks: 1,
            phase: "drained".to_owned(),
        };
        write_shutdown_drain_evidence(&options, &evidence).unwrap();
        assert!(repeated_shutdown_response(&options, &request, &status)
            .unwrap()
            .is_none());

        evidence.generation = 7;
        evidence.pid = 4243;
        write_shutdown_drain_evidence(&options, &evidence).unwrap();
        assert!(repeated_shutdown_response(&options, &request, &status)
            .unwrap()
            .is_none());

        evidence.pid = 4242;
        evidence.process_start_token = "another-shutdown-owner".to_owned();
        write_shutdown_drain_evidence(&options, &evidence).unwrap();
        assert!(repeated_shutdown_response(&options, &request, &status)
            .unwrap()
            .is_none());

        evidence.process_start_token = "shutdown-evidence-owner".to_owned();
        write_shutdown_drain_evidence(&options, &evidence).unwrap();
        let repeated = repeated_shutdown_response(&options, &request, &status)
            .unwrap()
            .unwrap();
        assert!(!repeated.mutation_executed);
        assert!(repeated.shutdown.unwrap().already_shutdown);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// daemon 提交后释放 writer，再以只读 backend 重开仍恢复完整 TaskEnvelope。
    fn daemon_submit_task_envelope_survives_shutdown_and_store_reopen() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("submit-envelope-reopen");
        let options = daemon_options(&root, false);
        let recovery_input = b"daemon-start-report-secret".to_vec();
        let leaked_recovery_bytes = format!("{recovery_input:?}");
        {
            let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
                &options.durable_backend,
            ))
            .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let recovery_snapshot = TaskStateSnapshot::queued_with_envelope(
                "req-daemon-debug-recovery",
                sample_task_envelope(recovery_input).to_snapshot(),
            )
            .unwrap();
            store.create(&recovery_snapshot).unwrap();
        }
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-envelope-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);
        let envelope = sample_task_envelope(vec![0, b'\n', b'%', 0xff]);
        let expected = envelope.to_snapshot();
        let submit_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-envelope-submit").unwrap());

        let submitted = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-envelope-submit").unwrap(),
                &submit_trace,
                DaemonControlOperation::SubmitTask,
            )
            .with_task_id("req-daemon-envelope-persisted")
            .with_task_envelope(envelope),
            2_000,
        )
        .unwrap();
        assert!(submitted.mutation_executed);

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-envelope-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-envelope-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        let report = daemon.join().unwrap().unwrap();
        let report_debug = format!("{report:?}");
        assert!(!report_debug.contains(&leaked_recovery_bytes));
        assert!(report
            .recovery
            .unchanged_tasks
            .iter()
            .any(|task_id| task_id == "req-daemon-debug-recovery"));

        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let reopened = FileSystemTaskStateStore::from_durable_layout(backend.layout())
            .read(Some("req-daemon-envelope-persisted"))
            .unwrap();
        let queued_recovery = FileSystemTaskStateStore::from_durable_layout(backend.layout())
            .read(Some("req-daemon-debug-recovery"))
            .unwrap();
        assert_eq!(reopened.envelope, Some(expected));
        assert_eq!(reopened.retry_max_attempts, 3);
        assert_eq!(reopened.owner_generation.0, report.lease.generation);
        assert_eq!(queued_recovery.status, "completed");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn daemon_ready_worker_executes_echo_and_joins_before_lease_release() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("ready-worker-executes");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-worker-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);

        let payload = b"daemon-worker-result".to_vec();
        let submit_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-worker-submit").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-worker-submit").unwrap(),
                &submit_trace,
                DaemonControlOperation::SubmitTask,
            )
            .with_task_id("req-daemon-worker-task")
            .with_task_envelope(sample_task_envelope(payload.clone())),
            2_000,
        )
        .unwrap();

        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let task_store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let started_at = Instant::now();
        let completed = loop {
            let snapshot = task_store.read(Some("req-daemon-worker-task")).unwrap();
            if snapshot.status == "completed" {
                break snapshot;
            }
            assert!(
                started_at.elapsed() < Duration::from_secs(3),
                "daemon worker task remained in status {}",
                snapshot.status
            );
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(completed.attempts, 1);
        assert_eq!(completed.result_size_bytes, Some(payload.len()));
        assert_eq!(
            completed.result_digest.as_deref(),
            Some(sha256_digest(&payload).as_str())
        );
        assert!(completed.execution_owner.is_some());
        assert!(completed.cancel_token.is_some());
        let completed_debug = format!("{completed:?}");
        assert!(completed_debug.contains("<redacted>"));
        assert!(!completed_debug.contains("task-cancel:"));

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-worker-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-worker-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        let report = daemon.join().unwrap().unwrap();
        assert_eq!(report.status, "stopped");
        assert_eq!(report.lease.state, "released");
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "daemon:w1-l06:task_worker_claim_gate_ready"));
        assert!(!pid_file(&options).exists());
        let released =
            probe_runtime_lease(lock_file(&options), lease_file(&options), now_ms()).unwrap();
        assert!(!released.owner_live());
        assert_eq!(
            released.record().map(DurableRuntimeLeaseRecord::state),
            Some(DurableRuntimeLeaseState::Released)
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn daemon_restart_requeues_abandoned_echo_before_ready_and_completes_it() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("restart-abandoned-echo");
        let options = daemon_options(&root, false);
        let task_id = "req-daemon-restart-abandoned-echo";
        let payload = b"restart-abandoned-echo".to_vec();
        {
            let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
                &options.durable_backend,
            ))
            .unwrap();
            let writer = backend.acquire_runtime_writer().unwrap();
            let mut store =
                FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer).unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        task_id,
                        sample_task_envelope(payload.clone()).to_snapshot(),
                    )
                    .unwrap(),
                )
                .unwrap();
            store
                .try_claim_queued(
                    task_id,
                    "daemon.crashed.worker",
                    "cancel.crashed.worker",
                    now_ms(),
                )
                .unwrap()
                .unwrap();
        }

        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default().with_request_id(
                    RequestId::parse("req-daemon-restart-abandoned-loop").unwrap(),
                ),
            )
        });
        wait_for_daemon_available(&options);

        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let started_at = Instant::now();
        let completed = loop {
            let snapshot = store.read(Some(task_id)).unwrap();
            if snapshot.status == "completed" {
                break snapshot;
            }
            assert!(
                started_at.elapsed() < Duration::from_secs(3),
                "recovered daemon task remained in status {}",
                snapshot.status
            );
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(completed.attempts, 2);
        assert_eq!(completed.result_size_bytes, Some(payload.len()));

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-restart-abandoned-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-restart-abandoned-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        let report = daemon.join().unwrap().unwrap();
        assert!(report.recovery.recovered_tasks.iter().any(|task| {
            task.task_id == task_id && task.previous_status == "running" && task.status == "queued"
        }));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "daemon:w1-l10:effect_aware_recovery_ready"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// poison request 仅保留不可逆摘要，Agent 身份分叉在 mutation 前被隔离且 daemon 继续服务。
    fn invalid_task_envelope_request_is_quarantined_without_stopping_daemon() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("invalid-envelope-quarantine");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-invalid-envelope-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);
        let invalid_digest_payload = b"invalid-digest-secret".to_vec();
        let unknown_agent_payload = b"unknown-agent-secret".to_vec();
        let mismatched_agent_payload = b"agent-mismatch-secret".to_vec();
        let envelope = sample_task_envelope(invalid_digest_payload.clone());
        let digest = envelope.input().digest().to_owned();
        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-invalid-envelope-submit").unwrap());
        let request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-invalid-envelope-submit").unwrap(),
            &trace,
            DaemonControlOperation::SubmitTask,
        )
        .with_task_id("req-daemon-invalid-envelope-task")
        .with_task_envelope(envelope);
        let tampered = request.to_storage().replace(
            &format!("task_input_digest={digest}"),
            "task_input_digest=sha256:BAD",
        );
        fs::write(
            control_request_file(&options, &request.request_id),
            tampered,
        )
        .unwrap();

        let mut quarantined = false;
        for _ in 0..100 {
            quarantined = fs::read_dir(control_request_dir(&options))
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.path().extension().and_then(|value| value.to_str())
                        == Some(CONTROL_REJECTED_EXT)
                });
            if quarantined {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(quarantined);
        assert!(!options
            .durable_backend
            .join("tasks")
            .join("req-daemon-invalid-envelope-task.task")
            .exists());

        let unknown_agent_envelope = crate::TaskEnvelope::new(
            crate::TaskKind::parse("runtime.echo").unwrap(),
            eva_core::AgentId::parse("missing-agent").unwrap(),
            crate::TaskInput::inline(unknown_agent_payload.clone()).unwrap(),
            crate::IdempotencyKey::parse("idem-unknown-agent").unwrap(),
            crate::TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap();
        let unknown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-unknown-agent-submit").unwrap());
        let unknown_request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-unknown-agent-submit").unwrap(),
            &unknown_trace,
            DaemonControlOperation::SubmitTask,
        )
        .with_task_id("req-daemon-unknown-agent-task")
        .with_task_envelope(unknown_agent_envelope);
        fs::write(
            control_request_file(&options, &unknown_request.request_id),
            unknown_request.to_storage(),
        )
        .unwrap();

        let mismatch_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-agent-mismatch-submit").unwrap());
        let mismatch_request = DaemonControlRequest::new(
            RequestId::parse("req-daemon-agent-mismatch-submit").unwrap(),
            &mismatch_trace,
            DaemonControlOperation::SubmitTask,
        )
        .with_task_id("req-daemon-agent-mismatch-task")
        .with_task_envelope(sample_task_envelope(mismatched_agent_payload.clone()))
        .with_agent_id("spoof-agent");
        fs::write(
            control_request_file(&options, &mismatch_request.request_id),
            mismatch_request.to_storage(),
        )
        .unwrap();
        let directory_request = control_request_dir(&options).join("invalid-entry.request");
        fs::create_dir_all(&directory_request).unwrap();
        let mut rejected_count = 0;
        for _ in 0..100 {
            rejected_count = fs::read_dir(control_request_dir(&options))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str())
                        == Some(CONTROL_REJECTED_EXT)
                })
                .count();
            if rejected_count >= 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(rejected_count, 4);
        assert!(!directory_request.exists());
        assert!(!options
            .durable_backend
            .join("tasks")
            .join("req-daemon-unknown-agent-task.task")
            .exists());
        assert!(!options
            .durable_backend
            .join("tasks")
            .join("req-daemon-agent-mismatch-task.task")
            .exists());

        let status_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-after-invalid-status").unwrap());
        let status = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-after-invalid-status").unwrap(),
                &status_trace,
                DaemonControlOperation::Status,
            ),
            2_000,
        )
        .unwrap();
        assert_eq!(status.status, "running");

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-after-invalid-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-after-invalid-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();

        let retained_text = read_tree_text(&root);
        for payload in [
            invalid_digest_payload,
            unknown_agent_payload,
            mismatched_agent_payload,
        ] {
            let plaintext = String::from_utf8(payload.clone()).unwrap();
            assert!(!retained_text.contains(&plaintext));
            assert!(!retained_text.contains(&encode_bytes(&payload)));
        }
        assert!(retained_text.matches("payload_retained=false").count() >= 4);
        assert!(retained_text.contains("request_sha256=sha256:"));
        assert!(retained_text.contains("request_size_bytes="));

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_control_request_is_removed_without_reading_or_mutating_its_target() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("symlink-request-quarantine");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-symlink-loop").unwrap()),
            )
        });
        wait_for_daemon_available(&options);

        let target_payload = b"external-symlink-target-secret";
        let target = root.join("external-target.txt");
        fs::write(&target, target_payload).unwrap();
        let request_path = control_request_dir(&options).join("symlink.request");
        std::os::unix::fs::symlink(&target, &request_path).unwrap();

        let mut rejected = false;
        for _ in 0..100 {
            rejected = fs::read_dir(control_request_dir(&options))
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.path().extension().and_then(|value| value.to_str())
                        == Some(CONTROL_REJECTED_EXT)
                });
            if rejected {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(rejected);
        assert!(!request_path.exists());
        assert_eq!(fs::read(&target).unwrap(), target_payload);
        let quarantine_text = read_tree_text(&control_request_dir(&options));
        assert!(!quarantine_text.contains(String::from_utf8_lossy(target_payload).as_ref()));
        assert!(!quarantine_text.contains(&encode_bytes(target_payload)));
        assert!(quarantine_text.contains("entry_kind=symlink"));
        assert!(quarantine_text.contains("request_sha256=\n"));

        let status_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-after-symlink-status").unwrap());
        let status = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-after-symlink-status").unwrap(),
                &status_trace,
                DaemonControlOperation::Status,
            ),
            2_000,
        )
        .unwrap();
        assert_eq!(status.status, "running");

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-after-symlink-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-after-symlink-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_control_submit_cancel_writes_observability_pipeline` 场景下的预期行为。
    #[test]
    fn daemon_control_submit_cancel_writes_observability_pipeline() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("control-observability");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-observed-loop").unwrap()),
            )
        });

        wait_for_daemon_available(&options);

        let submit_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-observed-submit").unwrap());
        let submit = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-observed-submit").unwrap(),
                &submit_trace,
                DaemonControlOperation::SubmitTask,
            )
            .with_task_id("req-daemon-observed-task")
            .with_task_envelope(sample_task_envelope(b"observed".to_vec())),
            2_000,
        )
        .unwrap();
        assert!(submit.mutation_executed);
        assert_eq!(submit.task_id.as_deref(), Some("req-daemon-observed-task"));

        let cancel_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-observed-cancel").unwrap());
        let cancel = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-observed-cancel").unwrap(),
                &cancel_trace,
                DaemonControlOperation::CancelTask,
            )
            .with_task_id("req-daemon-observed-task")
            .with_agent_id("root-agent")
            .with_reason("observability test cancel"),
            2_000,
        )
        .unwrap();
        assert!(cancel.mutation_executed);

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-observed-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-observed-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();

        let audit = fs::read_to_string(options.observability_backend.join("audit.jsonl")).unwrap();
        let metrics =
            fs::read_to_string(options.observability_backend.join("metrics.jsonl")).unwrap();
        let spans =
            fs::read_to_string(options.observability_backend.join("otel-spans.jsonl")).unwrap();
        assert!(audit.contains("\"action\":\"runtime.control\""));
        assert!(audit.contains("\"action\":\"task.lifecycle\""));
        assert!(audit.contains("\"request_id\":\"req-daemon-observed-submit\""));
        assert!(audit.contains("\"task_id\":\"req-daemon-observed-task\""));
        assert!(metrics.contains("\"name\":\"runtime.daemon.control\""));
        assert!(metrics.contains("\"name\":\"runtime.task.lifecycle\""));
        assert!(metrics.contains("\"surface\":\"task\""));
        assert!(spans.contains("\"name\":\"runtime.daemon.control\""));
        assert!(spans.contains("\"name\":\"runtime.task.lifecycle\""));
        let submit_audit = audit
            .lines()
            .filter(|line| line.contains("req-daemon-observed-submit"))
            .collect::<Vec<_>>();
        assert!(!submit_audit.is_empty());
        assert!(submit_audit
            .iter()
            .all(|line| line.contains("\"agent_id\":\"root-agent\"")));
        let task_metrics = metrics
            .lines()
            .filter(|line| line.contains("req-daemon-observed-task"))
            .collect::<Vec<_>>();
        assert!(!task_metrics.is_empty());
        assert!(task_metrics
            .iter()
            .all(|line| line.contains("\"agent_id\":\"root-agent\"")));
        let submit_spans = spans
            .lines()
            .filter(|line| line.contains("req-daemon-observed-submit"))
            .collect::<Vec<_>>();
        assert!(!submit_spans.is_empty());
        assert!(submit_spans
            .iter()
            .all(|line| line.contains("\"agent_id\":\"root-agent\"")));

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_control_observability_degrades_without_blocking_control_flow` 场景下的预期行为。
    #[test]
    fn daemon_control_observability_degrades_without_blocking_control_flow() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("control-observability-degraded");
        let options = daemon_options(&root, false);
        fs::create_dir_all(&root).unwrap();
        fs::write(&options.observability_backend, "not a directory").unwrap();
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-degraded-loop").unwrap()),
            )
        });

        wait_for_daemon_available(&options);
        let status_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-degraded-status").unwrap());
        let status = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-degraded-status").unwrap(),
                &status_trace,
                DaemonControlOperation::Status,
            ),
            2_000,
        )
        .unwrap();
        assert!(status.accepted);
        assert_eq!(status.status, "running");

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-degraded-shutdown").unwrap());
        let shutdown = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-degraded-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        assert_eq!(shutdown.status, "stopped");
        daemon.join().unwrap().unwrap();

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_control_loop_ticks_scheduler_retry_once` 场景下的预期行为。
    #[test]
    fn daemon_control_loop_ticks_scheduler_retry_once() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("scheduler-retry");
        let options = daemon_options(&root, false);
        {
            let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
                &options.durable_backend,
            ))
            .unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = durable_event("evt-daemon-retry", "/input/user");
            bus.publish(event.clone()).unwrap();
            bus.dead_letter_for_handlers(
                event,
                EvaError::timeout("handler timeout"),
                vec![ReplayHandlerBinding::new(
                    "runtime.echo",
                    AgentId::parse("root-agent").unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
        }
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-retry-loop").unwrap()),
            )
        });

        wait_for_daemon_available(&options);
        wait_for_scheduler_retry_ack(&options, "evt-daemon-retry");

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-retry-shutdown").unwrap());
        let shutdown = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-retry-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        assert_eq!(shutdown.status, "stopped");
        let report = daemon.join().unwrap().unwrap();
        assert_eq!(report.status, "stopped");

        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(
            &options.durable_backend,
        ))
        .unwrap();
        let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();
        assert_eq!(bus.dead_letters()[0].replay_count, 1);
        assert_eq!(
            bus.log()
                .records()
                .iter()
                .filter(|record| record.event.topic().as_str() != "/hardware/disconnected")
                .count(),
            2
        );
        assert_eq!(
            bus.event_log_status(&EventId::parse("evt-daemon-retry:replay-1").unwrap()),
            Some(eva_storage::EventLogStatus::Acked)
        );
        let audit = fs::read_to_string(options.observability_backend.join("audit.jsonl")).unwrap();
        let metrics =
            fs::read_to_string(options.observability_backend.join("metrics.jsonl")).unwrap();
        let spans =
            fs::read_to_string(options.observability_backend.join("otel-spans.jsonl")).unwrap();
        assert!(audit.contains("\"action\":\"scheduler.retry\""));
        assert!(audit.contains("\"dispatched_events\":\"1\""));
        assert!(metrics.contains("\"name\":\"runtime.scheduler.retry\""));
        assert!(spans.contains("\"name\":\"runtime.scheduler.retry\""));

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_control_returns_unavailable_without_running_daemon` 场景下的预期行为。
    #[test]
    fn daemon_control_returns_unavailable_without_running_daemon() {
        let root = temp_root("control-unavailable");
        let options = daemon_options(&root, true);
        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-control-missing").unwrap());

        let error = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-control-missing").unwrap(),
                &trace,
                DaemonControlOperation::Status,
            ),
            100,
        )
        .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "trace_id"
                && value == "request_id:req-daemon-control-missing"));

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_drain_mutates_agent_control_state` 场景下的预期行为。
    #[test]
    fn daemon_drain_mutates_agent_control_state() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("agent-drain");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-agent-drain-loop").unwrap()),
            )
        });

        wait_for_daemon_available(&options);

        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-agent-drain").unwrap());
        let response = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-agent-drain").unwrap(),
                &trace,
                DaemonControlOperation::Drain,
            )
            .with_agent_id("root-agent")
            .with_generation_id("gen-agent-old")
            .with_inflight_tasks(2)
            .with_timeout_ms(30_000),
            2_000,
        )
        .unwrap();

        assert_eq!(response.operation, DaemonControlOperation::Drain);
        assert!(response.mutation_executed);
        assert_eq!(response.task_id.as_deref(), Some("root-agent"));
        assert_eq!(response.generation_id.as_deref(), Some("gen-agent-old"));
        assert!(response
            .audit
            .iter()
            .any(|item| item == "daemon:v1.12.5:agent_drain_mutation"));

        let state = read_agent_control_state(&options).unwrap().unwrap();
        assert_eq!(state.agent_id, "root-agent");
        assert_eq!(state.operation, "drain");
        assert_eq!(state.drain_generation_id.as_deref(), Some("gen-agent-old"));
        assert_eq!(state.drain_accepts_new_work, Some(false));
        assert_eq!(state.drain_status.as_deref(), Some("planned"));
        assert!(state.mutation_executed);

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-agent-drain-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-agent-drain-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();
        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_reload_mutates_generation_route_state` 场景下的预期行为。
    #[test]
    fn daemon_reload_mutates_generation_route_state() {
        let _daemon_test_guard = daemon_test_guard();
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("agent-reload");
        let options = daemon_options(&root, false);
        let daemon_project = project.clone();
        let daemon_options = options.clone();
        let daemon = std::thread::spawn(move || {
            start_daemon(
                &daemon_project,
                daemon_options,
                &TraceFields::default()
                    .with_request_id(RequestId::parse("req-daemon-agent-reload-loop").unwrap()),
            )
        });

        wait_for_daemon_available(&options);

        let trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-agent-reload").unwrap());
        let response = send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-agent-reload").unwrap(),
                &trace,
                DaemonControlOperation::ReloadPlan,
            )
            .with_agent_id("root-agent")
            .with_from_generation_id("gen-old")
            .with_to_generation_id("gen-new")
            .with_from_release("1.11.4-alpha")
            .with_to_release("1.11.5-alpha")
            .with_inflight_tasks(0)
            .with_timeout_ms(30_000),
            2_000,
        )
        .unwrap();

        assert_eq!(response.operation, DaemonControlOperation::ReloadPlan);
        assert!(response.mutation_executed);
        assert_eq!(response.task_id.as_deref(), Some("root-agent"));
        assert_eq!(response.generation_id.as_deref(), Some("gen-new"));
        assert!(response
            .audit
            .iter()
            .any(|item| item == "scheduler:new_work_generation:gen-new"));

        let state = read_agent_control_state(&options).unwrap().unwrap();
        assert_eq!(state.agent_id, "root-agent");
        assert_eq!(state.operation, "reload_plan");
        assert_eq!(state.active_generation.as_deref(), Some("gen-new"));
        assert_eq!(state.previous_generation.as_deref(), Some("gen-old"));
        assert_eq!(state.previous_generation_state.as_deref(), Some("draining"));
        assert_eq!(
            state.selected_generation_for_new_work.as_deref(),
            Some("gen-new")
        );
        assert_eq!(state.drain_accepts_new_work, Some(false));
        assert!(state
            .audit
            .iter()
            .any(|item| item == "generation_route:gen-new:shadow_healthy"));

        let shutdown_trace = TraceFields::default()
            .with_request_id(RequestId::parse("req-daemon-agent-reload-shutdown").unwrap());
        send_daemon_control_request(
            &options,
            DaemonControlRequest::new(
                RequestId::parse("req-daemon-agent-reload-shutdown").unwrap(),
                &shutdown_trace,
                DaemonControlOperation::Shutdown,
            ),
            2_000,
        )
        .unwrap();
        daemon.join().unwrap().unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn startup_report_digest_and_frame_round_trip_are_strict() {
        let root = temp_root("startup-report-digest");
        let options = daemon_options(&root, false);
        let handshake =
            DaemonStartupHandshake::new("nonce-1", std::process::id(), "child-token-1").unwrap();
        let report = "{\"status\":\"running\"}";
        let digest = write_daemon_startup_report(&options, &handshake, report).unwrap();
        assert!(is_canonical_sha256(&digest));
        assert_eq!(
            read_daemon_startup_report(&options, &handshake, &digest)
                .unwrap()
                .as_deref(),
            Some(report)
        );

        let frame = DaemonStartupFrame {
            phase: DaemonStartupPhase::Ready,
            nonce: handshake.nonce().to_owned(),
            launcher_pid: handshake.launcher_pid(),
            child_pid: std::process::id(),
            process_start_token: Some("child-token-1".to_owned()),
            generation: Some(7),
            report_digest: Some(digest.clone()),
            observed_at_ms: 10,
            error_kind: None,
            cleanup_complete: false,
        };
        frame.validate().unwrap();
        assert_eq!(
            DaemonStartupFrame::from_storage(&frame.to_storage()).unwrap(),
            frame
        );

        let mut missing_digest = frame.clone();
        missing_digest.report_digest = None;
        assert_eq!(
            missing_digest.validate().unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
        fs::write(
            daemon_startup_report_path(&options, &handshake),
            "{\"status\":\"tampered\"}",
        )
        .unwrap();
        assert_eq!(
            read_daemon_startup_report(&options, &handshake, &digest)
                .unwrap_err()
                .kind(),
            eva_core::ErrorKind::Conflict
        );

        let ready_path = startup_frame_file(&options, &handshake, DaemonStartupPhase::Ready);
        fs::write(&ready_path, frame.to_storage()).unwrap();
        confirm_startup_atomic_result(
            &ready_path,
            &frame.to_storage(),
            Err(EvaError::internal("injected parent directory sync failure")),
        )
        .unwrap();
        let failed = DaemonStartupFrame::failed(
            &handshake,
            std::process::id(),
            None,
            &EvaError::internal("late startup error"),
            true,
        );
        write_daemon_startup_failure_frame(&options, &handshake, &failed).unwrap();
        assert!(!startup_frame_file(&options, &handshake, DaemonStartupPhase::Failed).exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_claimed_cleanup_rejects_same_pid_with_another_start_token() {
        let root = temp_root("startup-successor-token");
        let options = daemon_options(&root, false);
        fs::create_dir_all(&options.lock_dir).unwrap();
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
            &options.durable_backend,
        ))
        .unwrap();
        let successor_token = "successor-child-token";
        let successor = DurableRuntimeLeaseGuard::acquire_with_process_start_token(
            &backend,
            lock_file(&options),
            lease_file(&options),
            successor_token,
            now_ms(),
            DEFAULT_RUNTIME_LEASE_TTL_MS,
        )
        .unwrap();
        let handshake = DaemonStartupHandshake::new(
            "successor-nonce",
            std::process::id(),
            "expected-dead-child-token",
        )
        .unwrap();

        let report = cleanup_failed_daemon_start(
            &options,
            &handshake,
            std::process::id(),
            None,
            &EvaError::timeout("injected launcher timeout"),
        )
        .unwrap();
        assert_eq!(report.identity_source, "other_owner");
        assert!(report.cleanup_complete);
        let probe =
            probe_runtime_lease(lock_file(&options), lease_file(&options), now_ms()).unwrap();
        assert!(probe.owner_live());
        assert_eq!(
            probe.record().unwrap().process_start_token(),
            successor_token
        );
        assert_eq!(probe.record().unwrap(), successor.record());
        drop(successor);
        fs::remove_dir_all(root).ok();
    }
}
