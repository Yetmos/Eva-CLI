//! 实现本地前台守护进程的独占锁、状态文件、控制邮箱和周期调度边界。
//!
//! 启动时先取得原子文件锁，再恢复持久化任务和提供者状态，验证策略与可观测性后才发布
//! running 状态和 PID。控制面采用请求/响应文件邮箱：单一守护循环按文件名顺序消费请求，
//! 先执行状态变更，再原子发布响应，最后删除请求；调用方超时不代表请求未被稍后执行。
//! Local daemon process-boundary and control-plane contracts for V1.12.

use crate::{
    run_scheduler_retry_tick, RuntimeBuilder, RuntimeRecoveryCoordinator, RuntimeRecoveryReport,
    SchedulerRetryTickOptions, SchedulerRetryTickReport, ShutdownReport,
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
    FileSystemKnowledgeStore, FileSystemMemoryStore, KnowledgeRebuildCheckpointReport,
    MemoryCompactionReport,
};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind,
    MetricLabels, MetricName, MetricPoint, MetricSink, ObservabilitySmokeReport, SpanId,
    TraceFields,
};
use eva_policy::PolicyDomainSet;
use eva_scheduler::GenerationRouteGate;
use eva_storage::{
    DurableBackend, DurableBackendOptions, DurableBackendReport, FileSystemDurableBackend,
    FileSystemProviderProcessTable, FileSystemTaskStateStore, TaskStateSnapshot, TaskStateStore,
};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "define the local daemon process and control boundary without starting providers";

/// 定义 `DAEMON_GENERATION` 常量。
const DAEMON_GENERATION: &str = "daemon-v1.12.4";
/// 定义通过 `create_new` 原子取得的单实例锁文件名。
const LOCK_FILE: &str = "daemon.lock";
/// 定义守护进程可用性探测所需的 PID 文件名。
const PID_FILE: &str = "daemon.pid";
/// 定义持久化守护生命周期状态的文件名。
const STATE_FILE: &str = "daemon.state";
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
/// 定义 `CONTROL_RESPONSE_EXT` 常量。
const CONTROL_RESPONSE_EXT: &str = "response";
/// 定义 `CONTROL_POLL_INTERVAL_MS` 常量。
const CONTROL_POLL_INTERVAL_MS: u64 = 50;

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
    /// 记录 `pid_file` 字段对应的值。
    pub pid_file: String,
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
    pub memory_maintenance: DaemonMemoryMaintenanceReport,
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
    /// 记录 `paths` 字段对应的值。
    pub paths: DaemonPathReport,
    /// 记录 `state` 字段对应的值。
    pub state: Option<DaemonStateRecord>,
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
    /// 在请求和响应目录中唯一标识本次投递；复用该值会覆盖其邮箱关联语义。
    pub request_id: RequestId,
    /// 记录 `trace_id` 字段对应的值。
    pub trace_id: String,
    /// 记录 `operation` 字段对应的值。
    pub operation: DaemonControlOperation,
    /// 记录 `task_id` 字段对应的值。
    pub task_id: Option<String>,
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

/// 持有单实例锁文件所有权，并在正常作用域退出时尽力释放。
#[derive(Debug, PartialEq, Eq)]
struct DaemonLockGuard {
    /// 记录 `path` 字段对应的值。
    path: PathBuf,
    /// 控制析构是否删除锁文件；删除失败不会覆盖正在返回的原始错误。
    release_on_drop: bool,
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
            pid_file: display_path(&pid_file(options)),
        }
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonStateRecord {
    /// 执行 `running` 对应的受控流程。
    fn running(project: &ProjectConfig) -> Self {
        Self {
            status: "running".to_owned(),
            mode: "foreground_dev".to_owned(),
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
            request_id,
            trace_id: trace_id(trace),
            operation,
            task_id: None,
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
        format!(
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
        )
    }

    /// 从持久化数据或输入构造 `from_storage` 对应的值。
    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut request_id = None;
        let mut trace_id = None;
        let mut operation = None;
        let mut task_id = None;
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

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("daemon control request is invalid"));
            };
            match key {
                "version" => {
                    if value != "1" {
                        return Err(
                            EvaError::conflict("daemon control request version mismatch")
                                .with_context("version", value),
                        );
                    }
                }
                "request_id" => request_id = Some(RequestId::parse(value)?),
                "trace_id" => trace_id = Some(decode_field(value)?),
                "operation" => operation = Some(DaemonControlOperation::parse(value)?),
                "task_id" => task_id = decode_optional_field(value)?,
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

        Ok(Self {
            request_id: request_id
                .ok_or_else(|| EvaError::conflict("daemon control request missing request_id"))?,
            trace_id: trace_id
                .ok_or_else(|| EvaError::conflict("daemon control request missing trace_id"))?,
            operation: operation
                .ok_or_else(|| EvaError::conflict("daemon control request missing operation"))?,
            task_id,
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
        })
    }
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonControlResponse {
    /// 按稳定存储格式编码 `to_storage` 对应的数据。
    fn to_storage(&self) -> String {
        let state = self.state.as_ref();
        let shutdown = self.shutdown.as_ref();
        format!(
            "version=1\nrequest_id={}\ntrace_id={}\noperation={}\naccepted={}\ndaemon_available={}\nstatus={}\nmutation_executed={}\nrequest_file={}\nresponse_file={}\nstate_status={}\nstate_mode={}\nstate_pid={}\nstate_generation_id={}\nstate_project_root={}\nstate_started_at_ms={}\nstate_stopped_at_ms={}\ntask_id={}\nplan_id={}\ngeneration_id={}\nmessage={}\nshutdown_already_shutdown={}\nshutdown_request_count={}\nshutdown_phase={}\naudit={}\n",
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
                    if value != "1" {
                        return Err(
                            EvaError::conflict("daemon control response version mismatch")
                                .with_context("version", value),
                        );
                    }
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
        return Err(
            EvaError::unsupported("background daemon spawn is not implemented in V1.12.1")
                .with_context("suggestion", "use foreground/dev smoke mode"),
        );
    }

    fs::create_dir_all(&options.lock_dir).map_err(|error| {
        EvaError::internal("failed to create daemon lock directory")
            .with_context("path", options.lock_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    // `create_new` 锁是多个守护进程之间唯一的原子互斥点，必须先于恢复和状态写入。
    let lock = DaemonLockGuard::acquire(lock_file(&options))?;

    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let durable_report = durable_backend.verify()?;
    let mut task_store = FileSystemTaskStateStore::from_writable_backend(&durable_backend)?;
    let mut provider_process_table =
        FileSystemProviderProcessTable::from_durable_layout(durable_backend.layout());
    // 先消除崩溃遗留的运行中任务/提供者快照，再允许新控制请求观察到 running。
    let recovery = RuntimeRecoveryCoordinator
        .recover_task_store_with_provider_processes(&mut task_store, &mut provider_process_table)?;
    drop(task_store);
    drop(durable_backend);
    record_daemon_recovery_observability(&options, trace, &recovery);
    let policy = verify_policy(project)?;
    let observability = verify_observability(&options, trace)?;

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

    // 状态和 PID 均成功写入后，`daemon_status` 才可能将进程判为可用。
    let running_state = DaemonStateRecord::running(project);
    write_state(&options, &running_state)?;
    fs::write(pid_file(&options), running_state.pid.to_string()).map_err(|error| {
        EvaError::internal("failed to write daemon pid file")
            .with_context("path", pid_file(&options).display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ensure_control_dirs(&options)?;
    let hardware_hotplug = start_hardware_hotplug_subscriber(project, &options)?;
    let memory_maintenance = run_memory_maintenance(&options, trace)?;

    let mut runtime = RuntimeBuilder::new().build(project)?;
    let (status, shutdown) = if options.shutdown_after_smoke {
        let shutdown_report = runtime.shutdown();
        let stopped = running_state.clone().stopped();
        write_state(&options, &stopped)?;
        remove_if_exists(&pid_file(&options))?;
        ("stopped".to_owned(), Some(shutdown_report))
    } else {
        let loop_report = run_control_loop(project, &options, &mut runtime, running_state.clone())?;
        (loop_report.status, loop_report.shutdown)
    };

    drop(lock);

    Ok(DaemonStartReport {
        status,
        mode: "foreground_dev".to_owned(),
        pid: running_state.pid,
        generation_id: DAEMON_GENERATION.to_owned(),
        project_root: display_path(&project.project_root),
        foreground: options.foreground,
        dev_mode: options.dev_mode,
        provider_processes_started: false,
        paths: DaemonPathReport::from_options(&options),
        durable_backend: durable_report,
        recovery,
        policy,
        observability,
        hardware_hotplug,
        memory_maintenance,
        shutdown,
        audit: vec![
            "daemon:v1.12.1:lock_acquired".to_owned(),
            "daemon:v1.12.1:durable_backend_verified".to_owned(),
            "daemon:v1.12.1:policy_verified".to_owned(),
            "daemon:v1.12.1:observability_verified".to_owned(),
            "daemon:v1.12.1:provider_processes_not_started".to_owned(),
            "daemon:v1.12.2:control_mailbox_ready".to_owned(),
            "daemon:v1.12.4:scheduler_retry_tick_ready".to_owned(),
            "daemon:v1.13.5:provider_recovery_scanned".to_owned(),
            "daemon:v1.15.4:hardware_hotplug_subscriber_ready".to_owned(),
            "daemon:v1.15.6:memory_maintenance_ready".to_owned(),
        ],
    })
}

/// 只有状态为 running 且锁、PID 同时存在时才报告控制面可用，避免信任单个陈旧文件。
pub fn daemon_status(options: &DaemonStartOptions) -> Result<DaemonStatusReport, EvaError> {
    let paths = DaemonPathReport::from_options(options);
    let lock_present = lock_file(options).exists();
    let pid_present = pid_file(options).exists();
    let state = read_state(options)?;
    let status = state
        .as_ref()
        .map(|record| record.status.clone())
        .unwrap_or_else(|| "unavailable".to_owned());
    let running = state
        .as_ref()
        .map(|record| record.status == "running")
        .unwrap_or(false);
    Ok(DaemonStatusReport {
        available: running && lock_present && pid_present,
        status,
        lock_present,
        pid_present,
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
    let status = daemon_status(options)?;
    if !status.available {
        return Err(EvaError::unavailable("daemon control API is unavailable")
            .with_context("operation", request.operation.as_str())
            .with_context("request_id", request.request_id.as_str())
            .with_context("trace_id", &request.trace_id)
            .with_context("state_status", &status.status)
            .with_context("lock_present", status.lock_present.to_string())
            .with_context("pid_present", status.pid_present.to_string())
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
            return DaemonControlResponse::from_storage(&data);
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

/// 清理守护文件并把状态标为 stopped；活动运行时应优先通过 `Shutdown` 控制请求正常关闭。
pub fn stop_daemon(options: &DaemonStartOptions) -> Result<DaemonStopReport, EvaError> {
    let paths = DaemonPathReport::from_options(options);
    let lock_removed = remove_if_exists(&lock_file(options))?;
    let pid_removed = remove_if_exists(&pid_file(options))?;
    let state = match read_state(options)? {
        Some(record) => {
            let stopped = record.stopped();
            write_state(options, &stopped)?;
            Some(stopped)
        }
        None => None,
    };

    Ok(DaemonStopReport {
        status: state
            .as_ref()
            .map(|record| record.status.clone())
            .unwrap_or_else(|| "unavailable".to_owned()),
        mutation_executed: lock_removed || pid_removed || state.is_some(),
        lock_removed,
        pid_removed,
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

/// 串行执行调度重试 tick 和按文件名排序的控制请求，直到处理到关闭操作。
///
/// 每个请求先执行状态变更，再原子写响应，最后删除请求。响应写入失败时请求会保留以便诊断
/// 或重试，但变更可能已经发生，因此调用方必须依据响应或状态确认结果，不能只观察请求文件。
fn run_control_loop(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    runtime: &mut crate::Runtime,
    running_state: DaemonStateRecord,
) -> Result<DaemonControlLoopReport, EvaError> {
    loop {
        // 每轮先推进到期的调度重试，保证控制流量不会无限饿死恢复任务。
        let _tick = run_daemon_scheduler_tick(project, options)?;
        for request_path in pending_control_requests(options)? {
            let request = read_control_request(&request_path)?;
            let response_path = control_response_file(options, &request.request_id);
            let response = handle_control_request(
                project,
                options,
                runtime,
                &running_state,
                request,
                &request_path,
                &response_path,
            )?;
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
        thread::sleep(Duration::from_millis(CONTROL_POLL_INTERVAL_MS));
    }
}

/// 以当前逻辑时间重驱已到期的调度重试；每个循环最多调用一次该边界。
fn run_daemon_scheduler_tick(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
) -> Result<SchedulerRetryTickReport, EvaError> {
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut bus = DurableEventBus::open(durable_backend.layout())?;
    let report = run_scheduler_retry_tick(
        project,
        &mut bus,
        SchedulerRetryTickOptions {
            redrive_ready_at_ms: now_ms() as u64,
            ..SchedulerRetryTickOptions::default()
        },
    )?;
    record_scheduler_retry_observability(options, &report);
    Ok(report)
}

/// 执行 `start_hardware_hotplug_subscriber` 对应的受控流程。
fn start_hardware_hotplug_subscriber(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
) -> Result<HardwareHotplugSubscriberReport, EvaError> {
    let previous_state = read_hardware_hotplug_state(options)?;
    let discovery = discover_project_devices(project)?;
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut bus = DurableEventBus::open(durable_backend.layout())?;
    let mut audit_sink = BestEffortObservabilityPipeline::open(&options.observability_backend);
    let request_id_prefix = format!("req-daemon-hotplug-{}", now_ms());
    let report = run_hotplug_subscriber_once(
        &discovery.candidates,
        &previous_state,
        &mut bus,
        &request_id_prefix,
        &mut audit_sink,
    )?;
    write_hardware_hotplug_state(options, &report.state)?;
    Ok(report)
}

/// 执行 `run_memory_maintenance` 对应的受控流程。
fn run_memory_maintenance(
    options: &DaemonStartOptions,
    trace: &TraceFields,
) -> Result<DaemonMemoryMaintenanceReport, EvaError> {
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut audit_sink = BestEffortObservabilityPipeline::open(&options.observability_backend);
    let mut memory_store = FileSystemMemoryStore::from_durable_layout(durable_backend.layout());
    let mut knowledge_store =
        FileSystemKnowledgeStore::from_durable_layout(durable_backend.layout());
    let memory_gc = memory_store.compact_expired_at(now_ms(), &mut audit_sink, trace)?;
    let knowledge_rebuild = knowledge_store.rebuild_checkpoint(&mut audit_sink, trace)?;
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

/// 登记 `record_daemon_recovery_observability` 对应的数据或状态。
fn record_daemon_recovery_observability(
    options: &DaemonStartOptions,
    trace: &TraceFields,
    report: &RuntimeRecoveryReport,
) {
    let Ok(span_id) = SpanId::parse("runtime.daemon.recovery") else {
        return;
    };
    let mut pipeline = BestEffortObservabilityPipeline::open(&options.observability_backend);
    let recovery_trace = trace.child_span(span_id);
    let _ = RuntimeRecoveryCoordinator.record_recovery_audit(
        &mut pipeline,
        recovery_trace.clone(),
        report,
    );
    if let Ok(name) = MetricName::parse("runtime.daemon.recovery") {
        let _ = MetricSink::record(
            &mut pipeline,
            MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                MetricLabels::runtime("daemon_v1.16.1", DAEMON_GENERATION)
                    .with("recovered_tasks", report.recovered_tasks.len().to_string())
                    .with(
                        "recovered_provider_processes",
                        report.recovered_provider_processes.len().to_string(),
                    ),
            ),
        );
    }
    let recovered_tasks = report.recovered_tasks.len().to_string();
    let recovered_provider_processes = report.recovered_provider_processes.len().to_string();
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
        ],
    );
}

/// 登记 `record_scheduler_retry_observability` 对应的数据或状态。
fn record_scheduler_retry_observability(
    options: &DaemonStartOptions,
    report: &SchedulerRetryTickReport,
) {
    if report.dispatched_events.is_empty() && report.failed_events.is_empty() {
        return;
    }
    let Ok(span_id) = SpanId::parse("runtime.scheduler.retry") else {
        return;
    };
    let trace = TraceFields::default().with_span_id(span_id);
    let mut pipeline = BestEffortObservabilityPipeline::open(&options.observability_backend);
    let outcome = if report.failed_events.is_empty() {
        AuditOutcome::Ok
    } else {
        AuditOutcome::Failed
    };
    let _ = AuditSink::record(
        &mut pipeline,
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
            &mut pipeline,
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
    options: &DaemonStartOptions,
    request: &DaemonControlRequest,
    response: &DaemonControlResponse,
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
        .agent_id
        .as_deref()
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

    let mut pipeline = BestEffortObservabilityPipeline::open(&options.observability_backend);
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
    let _ = AuditSink::record(&mut pipeline, event);

    if let Ok(name) = MetricName::parse("runtime.daemon.control") {
        let _ = MetricSink::record(
            &mut pipeline,
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

    record_task_lifecycle_observability(&mut pipeline, request, response, &trace);
}

/// 登记 `record_task_lifecycle_observability` 对应的数据或状态。
fn record_task_lifecycle_observability(
    pipeline: &mut BestEffortObservabilityPipeline,
    request: &DaemonControlRequest,
    response: &DaemonControlResponse,
    parent_trace: &TraceFields,
) {
    let lifecycle_status = match request.operation {
        DaemonControlOperation::SubmitTask if response.mutation_executed => "queued",
        DaemonControlOperation::CancelTask if response.mutation_executed => "cancelling",
        _ => return,
    };
    let Some(task_id) = response.task_id.as_deref() else {
        return;
    };
    let Ok(span_id) = SpanId::parse("runtime.task.lifecycle") else {
        return;
    };
    let trace = parent_trace.child_span(span_id);
    let agent_id = request.agent_id.as_deref().unwrap_or("daemon-control");
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

/// 执行单个控制操作并构造响应；可观测性采用尽力而为，不改变已完成的业务结果。
fn handle_control_request(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    runtime: &mut crate::Runtime,
    running_state: &DaemonStateRecord,
    request: DaemonControlRequest,
    request_path: &Path,
    response_path: &Path,
) -> Result<DaemonControlResponse, EvaError> {
    let mut state = read_state(options)?.unwrap_or_else(|| running_state.clone());
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

    // 所有持久化变更均在响应构造前完成，因此 `mutation_executed` 只描述已成功分支。
    match request.operation {
        DaemonControlOperation::Status => {
            message = "daemon status returned through local control mailbox".to_owned();
        }
        DaemonControlOperation::Shutdown => {
            let shutdown_report = runtime.shutdown();
            state = state.stopped();
            write_state(options, &state)?;
            remove_if_exists(&pid_file(options))?;
            mutation_executed = true;
            message = "daemon shutdown recorded through local control mailbox".to_owned();
            audit.push("daemon:v1.12.2:shutdown_recorded".to_owned());
            shutdown = Some(shutdown_report);
        }
        DaemonControlOperation::SubmitTask => {
            let submitted_task_id = submit_control_task(options, project, &request)?;
            task_id = Some(submitted_task_id);
            mutation_executed = true;
            message =
                "task submitted to durable task store through daemon control mailbox".to_owned();
            audit.push("daemon:v1.12.2:task_submitted".to_owned());
        }
        DaemonControlOperation::CancelTask => {
            let cancelled_task_id = cancel_control_task(options, &request)?;
            task_id = Some(cancelled_task_id);
            mutation_executed = true;
            message = "task cancellation recorded through daemon control mailbox".to_owned();
            audit.push("daemon:v1.12.2:task_cancel_requested".to_owned());
        }
        DaemonControlOperation::Drain => {
            let applied = apply_agent_drain_control(options, &request)?;
            task_id = Some(applied.agent_id.clone());
            generation_id = Some(applied.generation_id);
            plan_id = applied.plan_id;
            mutation_executed = true;
            message =
                "agent drain mutation recorded through daemon scheduler gate state".to_owned();
            audit.extend(applied.audit);
        }
        DaemonControlOperation::ReloadPlan => {
            let applied = apply_agent_reload_control(options, &request)?;
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
        task_id,
        plan_id,
        generation_id,
        message,
        shutdown,
        audit,
    };
    record_daemon_control_observability(options, &request, &response);
    Ok(response)
}

/// 创建 queued 任务快照；任务标识默认沿用请求标识，作为持久化关联键。
fn submit_control_task(
    options: &DaemonStartOptions,
    project: &ProjectConfig,
    request: &DaemonControlRequest,
) -> Result<String, EvaError> {
    let task_id = request
        .task_id
        .clone()
        .unwrap_or_else(|| request.request_id.as_str().to_owned());
    RequestId::parse(&task_id)?;
    let mut store = open_durable_task_store(options)?;
    let mut snapshot = TaskStateSnapshot::queued(task_id.clone())?;
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

/// 在既有快照上请求取消；缺少任务或状态转换非法时不创建替代快照。
fn cancel_control_task(
    options: &DaemonStartOptions,
    request: &DaemonControlRequest,
) -> Result<String, EvaError> {
    let task_id = request.task_id.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("daemon cancel task request requires a task id")
    })?;
    RequestId::parse(task_id)?;
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| "cancel requested by daemon control API".to_owned());
    let mut store = open_durable_task_store(options)?;
    store.update_snapshot(task_id, |snapshot| {
        snapshot.request_cancel(reason);
        Ok(())
    })?;
    Ok(task_id.to_owned())
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
    options: &DaemonStartOptions,
) -> Result<FileSystemTaskStateStore, EvaError> {
    let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    FileSystemTaskStateStore::from_writable_backend(&backend)
}

/// 为相关类型实现其约定的行为与方法。
impl DaemonLockGuard {
    /// 执行 `acquire` 对应的处理逻辑。
    fn acquire(path: PathBuf) -> Result<Self, EvaError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                EvaError::internal("failed to create daemon lock directory")
                    .with_context("path", parent.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    EvaError::conflict("daemon lock already exists")
                        .with_context("path", path.display().to_string())
                } else {
                    EvaError::internal("failed to create daemon lock")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                }
            })?;
        writeln!(
            file,
            "pid={}\ngeneration_id={DAEMON_GENERATION}\n",
            std::process::id()
        )
        .map_err(|error| {
            EvaError::internal("failed to write daemon lock")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        Ok(Self {
            path,
            release_on_drop: true,
        })
    }
}

/// 为相关类型实现其约定的行为与方法。
impl Drop for DaemonLockGuard {
    /// 停止、取消或释放 `drop` 管理的状态。
    fn drop(&mut self) {
        if self.release_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
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
    options: &DaemonStartOptions,
    trace: &TraceFields,
) -> Result<ObservabilitySmokeReport, EvaError> {
    let backend_root = options.observability_backend.display().to_string();
    let mut pipeline = BestEffortObservabilityPipeline::open(&options.observability_backend);
    let runtime_trace = trace.child_span(SpanId::parse("runtime.daemon.start")?);
    AuditSink::record(
        &mut pipeline,
        AuditEvent::new(
            AuditAction::RuntimeStarted,
            AuditOutcome::Planned,
            runtime_trace.clone(),
        )
        .with_message("daemon foreground smoke boundary verified")
        .with_field("generation_id", DAEMON_GENERATION),
    )?;
    MetricSink::record(
        &mut pipeline,
        MetricPoint::new(
            MetricName::parse("runtime.daemon.start")?,
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::runtime("daemon_v1.12.1", DAEMON_GENERATION)),
    )?;
    pipeline.export_span(
        "runtime.daemon.start",
        &runtime_trace,
        &[("component", "runtime"), ("mode", "foreground_dev")],
    )?;
    Ok(pipeline.smoke_report(backend_root, trace.continuity_key()))
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
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::internal("failed to read daemon control request")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DaemonControlRequest::from_storage(&data)
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

/// 执行 `state_file` 对应的处理逻辑。
fn state_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(STATE_FILE)
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
#[cfg(test)]
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
    use eva_config::load_project_config;
    use eva_core::{Event, EventId, EventPayload, Topic};
    use eva_eventbus::EventBus;
    use eva_storage::{
        FileSystemProviderProcessTable, ProviderProcessSnapshot, ProviderProcessTable,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

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

    /// 执行 `wait_for_daemon_available` 对应的处理逻辑。
    fn wait_for_daemon_available(options: &DaemonStartOptions) {
        for _ in 0..100 {
            if daemon_status(options)
                .map(|report| report.available)
                .unwrap_or(false)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon did not become available");
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
    }

    /// 执行 `daemon_provider_process` 对应的处理逻辑。
    fn daemon_provider_process(session_id: &str, request_id: &str) -> ProviderProcessSnapshot {
        ProviderProcessSnapshot::running(
            session_id,
            format!("proc-{session_id}"),
            RequestId::parse(request_id).unwrap(),
            eva_core::AdapterId::parse("stdio-test").unwrap(),
            eva_core::CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "fnv64:0123456789abcdef",
            "stdio-runner --once",
            "none",
        )
    }

    /// 验证 `daemon_start_smoke_verifies_boundaries_and_stops` 场景下的预期行为。
    #[test]
    fn daemon_start_smoke_verifies_boundaries_and_stops() {
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
        assert_eq!(report.memory_maintenance.status, "ready");
        assert_eq!(report.memory_maintenance.memory_gc.expired_removed, 0);
        assert_eq!(report.memory_maintenance.knowledge_rebuild.items_indexed, 0);
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "daemon:v1.15.6:memory_maintenance_ready"));
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
        assert!(!lock_file(&options).exists());
        assert!(!pid_file(&options).exists());

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_hotplug_subscriber_persists_state_across_restart` 场景下的预期行为。
    #[test]
    fn daemon_hotplug_subscriber_persists_state_across_restart() {
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("hotplug-subscriber");
        let options = daemon_options(&root, true);

        let first = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();
        assert_eq!(first.hardware_hotplug.events_published.len(), 1);
        let state = read_hardware_hotplug_state(&options).unwrap();
        assert_eq!(state.len(), 1);

        let second = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();
        assert!(second.hardware_hotplug.events_published.is_empty());
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
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("provider-recovery");
        let options = daemon_options(&root, true);
        {
            let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
                &options.durable_backend,
            ))
            .unwrap();
            let mut task_store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut process_table =
                FileSystemProviderProcessTable::from_durable_layout(backend.layout());
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
        assert_eq!(task.status, "interrupted");
        assert!(!process.active);
        assert_eq!(process.health, "interrupted");

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_lock_conflict_blocks_start_before_state_write` 场景下的预期行为。
    #[test]
    fn daemon_lock_conflict_blocks_start_before_state_write() {
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

    /// 验证 `daemon_control_status_and_shutdown_round_trip_has_trace_id` 场景下的预期行为。
    #[test]
    fn daemon_control_status_and_shutdown_round_trip_has_trace_id() {
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
        assert!(!lock_file(&options).exists());
        assert!(!pid_file(&options).exists());

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_control_submit_cancel_writes_observability_pipeline` 场景下的预期行为。
    #[test]
    fn daemon_control_submit_cancel_writes_observability_pipeline() {
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
            .with_agent_id("root-agent"),
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

        fs::remove_dir_all(root).ok();
    }

    /// 验证 `daemon_control_observability_degrades_without_blocking_control_flow` 场景下的预期行为。
    #[test]
    fn daemon_control_observability_degrades_without_blocking_control_flow() {
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
            bus.dead_letter(event, EvaError::timeout("handler timeout"))
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
}
