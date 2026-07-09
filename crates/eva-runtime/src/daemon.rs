//! Local daemon process-boundary and control-plane contracts for V1.12.

use crate::{
    run_scheduler_retry_tick, RuntimeBuilder, RuntimeRecoveryCoordinator, RuntimeRecoveryReport,
    SchedulerRetryTickOptions, SchedulerRetryTickReport, ShutdownReport,
};
use eva_config::ProjectConfig;
use eva_core::{AgentId, EvaError, GenerationId, RequestId};
use eva_eventbus::DurableEventBus;
use eva_lifecycle::{
    DrainCoordinator, DrainPlan, GenerationController, GenerationState, RuntimeGeneration,
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

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "define the local daemon process and control boundary without starting providers";

const DAEMON_GENERATION: &str = "daemon-v1.12.4";
const LOCK_FILE: &str = "daemon.lock";
const PID_FILE: &str = "daemon.pid";
const STATE_FILE: &str = "daemon.state";
const AGENT_CONTROL_STATE_FILE: &str = "agent-control.state";
const CONTROL_REQUEST_DIR: &str = "control/requests";
const CONTROL_RESPONSE_DIR: &str = "control/responses";
const CONTROL_REQUEST_EXT: &str = "request";
const CONTROL_RESPONSE_EXT: &str = "response";
const CONTROL_POLL_INTERVAL_MS: u64 = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStartOptions {
    pub durable_backend: PathBuf,
    pub state_dir: PathBuf,
    pub lock_dir: PathBuf,
    pub pid_dir: PathBuf,
    pub observability_backend: PathBuf,
    pub foreground: bool,
    pub dev_mode: bool,
    pub shutdown_after_smoke: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPathReport {
    pub durable_backend_root: String,
    pub observability_backend_root: String,
    pub state_dir: String,
    pub lock_dir: String,
    pub pid_dir: String,
    pub control_request_dir: String,
    pub control_response_dir: String,
    pub state_file: String,
    pub lock_file: String,
    pub pid_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPolicyReport {
    pub status: String,
    pub source_count: usize,
    pub effective_layers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStateRecord {
    pub status: String,
    pub mode: String,
    pub pid: u32,
    pub generation_id: String,
    pub project_root: String,
    pub started_at_ms: u128,
    pub stopped_at_ms: Option<u128>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DaemonStartReport {
    pub status: String,
    pub mode: String,
    pub pid: u32,
    pub generation_id: String,
    pub project_root: String,
    pub foreground: bool,
    pub dev_mode: bool,
    pub provider_processes_started: bool,
    pub paths: DaemonPathReport,
    pub durable_backend: DurableBackendReport,
    pub recovery: RuntimeRecoveryReport,
    pub policy: DaemonPolicyReport,
    pub observability: ObservabilitySmokeReport,
    pub shutdown: Option<ShutdownReport>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatusReport {
    pub available: bool,
    pub status: String,
    pub lock_present: bool,
    pub pid_present: bool,
    pub paths: DaemonPathReport,
    pub state: Option<DaemonStateRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStopReport {
    pub status: String,
    pub mutation_executed: bool,
    pub lock_removed: bool,
    pub pid_removed: bool,
    pub paths: DaemonPathReport,
    pub state: Option<DaemonStateRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonControlOperation {
    Status,
    Shutdown,
    SubmitTask,
    CancelTask,
    Drain,
    ReloadPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonControlRequest {
    pub request_id: RequestId,
    pub trace_id: String,
    pub operation: DaemonControlOperation,
    pub task_id: Option<String>,
    pub reason: Option<String>,
    pub plan_id: Option<String>,
    pub generation_id: Option<String>,
    pub agent_id: Option<String>,
    pub from_generation_id: Option<String>,
    pub to_generation_id: Option<String>,
    pub from_release: Option<String>,
    pub to_release: Option<String>,
    pub inflight_tasks: Option<usize>,
    pub timeout_ms: Option<u64>,
    pub created_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonControlResponse {
    pub request_id: RequestId,
    pub trace_id: String,
    pub operation: DaemonControlOperation,
    pub accepted: bool,
    pub daemon_available: bool,
    pub status: String,
    pub mutation_executed: bool,
    pub request_file: String,
    pub response_file: String,
    pub state: Option<DaemonStateRecord>,
    pub task_id: Option<String>,
    pub plan_id: Option<String>,
    pub generation_id: Option<String>,
    pub message: String,
    pub shutdown: Option<ShutdownReport>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonAgentControlState {
    agent_id: String,
    operation: String,
    lifecycle: String,
    drain_generation_id: Option<String>,
    drain_inflight_tasks: Option<usize>,
    drain_timeout_ms: Option<u64>,
    drain_accepts_new_work: Option<bool>,
    drain_status: Option<String>,
    active_generation: Option<String>,
    previous_generation: Option<String>,
    previous_generation_state: Option<String>,
    selected_generation_for_new_work: Option<String>,
    from_release: Option<String>,
    to_release: Option<String>,
    plan_id: Option<String>,
    mutation_executed: bool,
    updated_at_ms: u128,
    audit: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct DaemonLockGuard {
    path: PathBuf,
    release_on_drop: bool,
}

impl DaemonStartOptions {
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

impl DaemonPathReport {
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
            lock_file: display_path(&lock_file(options)),
            pid_file: display_path(&pid_file(options)),
        }
    }
}

impl DaemonStateRecord {
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

    fn stopped(mut self) -> Self {
        self.status = "stopped".to_owned();
        self.stopped_at_ms = Some(now_ms());
        self
    }

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

impl DaemonAgentControlState {
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

impl DaemonControlOperation {
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

impl DaemonControlRequest {
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

    pub fn with_task_id(mut self, value: impl Into<String>) -> Self {
        self.task_id = Some(value.into());
        self
    }

    pub fn with_reason(mut self, value: impl Into<String>) -> Self {
        self.reason = Some(value.into());
        self
    }

    pub fn with_plan_id(mut self, value: impl Into<String>) -> Self {
        self.plan_id = Some(value.into());
        self
    }

    pub fn with_generation_id(mut self, value: impl Into<String>) -> Self {
        self.generation_id = Some(value.into());
        self
    }

    pub fn with_agent_id(mut self, value: impl Into<String>) -> Self {
        self.agent_id = Some(value.into());
        self
    }

    pub fn with_from_generation_id(mut self, value: impl Into<String>) -> Self {
        self.from_generation_id = Some(value.into());
        self
    }

    pub fn with_to_generation_id(mut self, value: impl Into<String>) -> Self {
        self.to_generation_id = Some(value.into());
        self
    }

    pub fn with_from_release(mut self, value: impl Into<String>) -> Self {
        self.from_release = Some(value.into());
        self
    }

    pub fn with_to_release(mut self, value: impl Into<String>) -> Self {
        self.to_release = Some(value.into());
        self
    }

    pub fn with_inflight_tasks(mut self, value: usize) -> Self {
        self.inflight_tasks = Some(value);
        self
    }

    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = Some(value);
        self
    }

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

impl DaemonControlResponse {
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
    let lock = DaemonLockGuard::acquire(lock_file(&options))?;

    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let durable_report = durable_backend.verify()?;
    let mut task_store = FileSystemTaskStateStore::from_durable_layout(durable_backend.layout());
    let mut provider_process_table =
        FileSystemProviderProcessTable::from_durable_layout(durable_backend.layout());
    let recovery = RuntimeRecoveryCoordinator
        .recover_task_store_with_provider_processes(&mut task_store, &mut provider_process_table)?;
    drop(durable_backend);
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

    let running_state = DaemonStateRecord::running(project);
    write_state(&options, &running_state)?;
    fs::write(pid_file(&options), running_state.pid.to_string()).map_err(|error| {
        EvaError::internal("failed to write daemon pid file")
            .with_context("path", pid_file(&options).display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ensure_control_dirs(&options)?;

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
        ],
    })
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonControlLoopReport {
    status: String,
    shutdown: Option<ShutdownReport>,
}

fn run_control_loop(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
    runtime: &mut crate::Runtime,
    running_state: DaemonStateRecord,
) -> Result<DaemonControlLoopReport, EvaError> {
    loop {
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

fn run_daemon_scheduler_tick(
    project: &ProjectConfig,
    options: &DaemonStartOptions,
) -> Result<SchedulerRetryTickReport, EvaError> {
    let durable_backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    let mut bus = DurableEventBus::open(durable_backend.layout())?;
    run_scheduler_retry_tick(
        project,
        &mut bus,
        SchedulerRetryTickOptions {
            redrive_ready_at_ms: now_ms() as u64,
            ..SchedulerRetryTickOptions::default()
        },
    )
}

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

    Ok(DaemonControlResponse {
        request_id: request.request_id,
        trace_id: request.trace_id,
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
    })
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedAgentDrainControl {
    agent_id: String,
    generation_id: String,
    plan_id: Option<String>,
    audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedAgentReloadControl {
    agent_id: String,
    plan_id: String,
    active_generation: String,
    audit: Vec<String>,
}

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

fn control_agent_id(request: &DaemonControlRequest) -> Result<String, EvaError> {
    let agent_id = request
        .agent_id
        .as_deref()
        .or(request.task_id.as_deref())
        .unwrap_or("daemon-agent");
    AgentId::parse(agent_id)?;
    Ok(agent_id.to_owned())
}

fn open_durable_task_store(
    options: &DaemonStartOptions,
) -> Result<FileSystemTaskStateStore, EvaError> {
    let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(
        &options.durable_backend,
    ))?;
    Ok(FileSystemTaskStateStore::from_durable_layout(
        backend.layout(),
    ))
}

impl DaemonLockGuard {
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

impl Drop for DaemonLockGuard {
    fn drop(&mut self) {
        if self.release_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn verify_policy(project: &ProjectConfig) -> Result<DaemonPolicyReport, EvaError> {
    let domains = PolicyDomainSet::from_project(project)?;
    let effective = domains.effective_policy()?;
    Ok(DaemonPolicyReport {
        status: "verified".to_owned(),
        source_count: domains.source_count,
        effective_layers: effective.layer_names,
    })
}

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

fn read_control_request(path: &Path) -> Result<DaemonControlRequest, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::internal("failed to read daemon control request")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DaemonControlRequest::from_storage(&data)
}

fn write_control_request(path: &Path, request: &DaemonControlRequest) -> Result<(), EvaError> {
    write_atomic(
        path,
        &request.to_storage(),
        "failed to write daemon control request",
    )
}

fn write_control_response(path: &Path, response: &DaemonControlResponse) -> Result<(), EvaError> {
    write_atomic(
        path,
        &response.to_storage(),
        "failed to write daemon control response",
    )
}

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

fn remove_if_exists(path: &Path) -> Result<bool, EvaError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(EvaError::internal("failed to remove daemon file")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())),
    }
}

fn state_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(STATE_FILE)
}

fn agent_control_state_file(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(AGENT_CONTROL_STATE_FILE)
}

fn lock_file(options: &DaemonStartOptions) -> PathBuf {
    options.lock_dir.join(LOCK_FILE)
}

fn pid_file(options: &DaemonStartOptions) -> PathBuf {
    options.pid_dir.join(PID_FILE)
}

fn control_request_dir(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(CONTROL_REQUEST_DIR)
}

fn control_response_dir(options: &DaemonStartOptions) -> PathBuf {
    options.state_dir.join(CONTROL_RESPONSE_DIR)
}

fn control_request_file(options: &DaemonStartOptions, request_id: &RequestId) -> PathBuf {
    control_request_dir(options).join(format!("{}.{}", request_id.as_str(), CONTROL_REQUEST_EXT))
}

fn control_response_file(options: &DaemonStartOptions, request_id: &RequestId) -> PathBuf {
    control_response_dir(options).join(format!("{}.{}", request_id.as_str(), CONTROL_RESPONSE_EXT))
}

fn resolve_project_path(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn trace_id(trace: &TraceFields) -> String {
    trace.continuity_key().unwrap_or_else(|| {
        trace
            .span_id
            .as_ref()
            .map(|value| format!("span_id:{}", value.as_str()))
            .unwrap_or_else(|| "span_id:daemon.control".to_owned())
    })
}

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

fn encode_optional_field(value: Option<&str>) -> String {
    value.map(encode_field).unwrap_or_default()
}

fn decode_optional_field(value: &str) -> Result<Option<String>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        decode_field(value).map(Some)
    }
}

fn encode_audit(values: &[String]) -> String {
    values
        .iter()
        .map(|value| encode_field(value))
        .collect::<Vec<_>>()
        .join(",")
}

fn decode_audit(value: &str) -> Result<Vec<String>, EvaError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value.split(',').map(decode_field).collect()
}

fn encode_field(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

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

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

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

    fn durable_event(event_id: &str, topic: &str) -> Event {
        Event::new(
            EventId::parse(event_id).unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
    }

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

    #[test]
    fn daemon_start_smoke_verifies_boundaries_and_stops() {
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("start");
        let options = daemon_options(&root, true);

        let report = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap();

        assert_eq!(report.status, "stopped");
        assert!(!report.provider_processes_started);
        assert!(report.shutdown.is_some());
        assert!(state_file(&options).is_file());
        assert!(!lock_file(&options).exists());
        assert!(!pid_file(&options).exists());

        fs::remove_dir_all(root).ok();
    }

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
            let mut task_store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
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
        assert_eq!(bus.log().records().len(), 2);
        assert_eq!(
            bus.event_log_status(&EventId::parse("evt-daemon-retry:replay-1").unwrap()),
            Some(eva_storage::EventLogStatus::Acked)
        );

        fs::remove_dir_all(root).ok();
    }

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
