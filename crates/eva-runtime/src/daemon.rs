//! Local daemon process-boundary contracts for V1.12.1.

use crate::{RuntimeBuilder, ShutdownReport};
use eva_config::ProjectConfig;
use eva_core::EvaError;
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind,
    MetricLabels, MetricName, MetricPoint, MetricSink, ObservabilitySmokeReport, SpanId,
    TraceFields,
};
use eva_policy::PolicyDomainSet;
use eva_storage::{
    DurableBackend, DurableBackendOptions, DurableBackendReport, FileSystemDurableBackend,
};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "define the local daemon process boundary without starting providers";

const DAEMON_GENERATION: &str = "daemon-v1.12.1";
const LOCK_FILE: &str = "daemon.lock";
const PID_FILE: &str = "daemon.pid";
const STATE_FILE: &str = "daemon.state";

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

    let mut runtime = RuntimeBuilder::new().build(project)?;
    let mut shutdown = None;
    let mut status = "running".to_owned();
    if options.shutdown_after_smoke {
        let shutdown_report = runtime.shutdown();
        let stopped = running_state.clone().stopped();
        write_state(&options, &stopped)?;
        remove_if_exists(&pid_file(&options))?;
        status = "stopped".to_owned();
        shutdown = Some(shutdown_report);
    }

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
        policy,
        observability,
        shutdown,
        audit: vec![
            "daemon:v1.12.1:lock_acquired".to_owned(),
            "daemon:v1.12.1:durable_backend_verified".to_owned(),
            "daemon:v1.12.1:policy_verified".to_owned(),
            "daemon:v1.12.1:observability_verified".to_owned(),
            "daemon:v1.12.1:provider_processes_not_started".to_owned(),
        ],
    })
}

pub fn daemon_status(options: &DaemonStartOptions) -> Result<DaemonStatusReport, EvaError> {
    let paths = DaemonPathReport::from_options(options);
    let lock_present = lock_file(options).exists();
    let state = read_state(options)?;
    let status = state
        .as_ref()
        .map(|record| record.status.clone())
        .unwrap_or_else(|| "unavailable".to_owned());
    Ok(DaemonStatusReport {
        available: state.is_some() || lock_present,
        status,
        lock_present,
        paths,
        state,
    })
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

fn lock_file(options: &DaemonStartOptions) -> PathBuf {
    options.lock_dir.join(LOCK_FILE)
}

fn pid_file(options: &DaemonStartOptions) -> PathBuf {
    options.pid_dir.join(PID_FILE)
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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
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

    #[test]
    fn daemon_start_smoke_verifies_boundaries_and_stops() {
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("start");
        let options = DaemonStartOptions {
            durable_backend: root.join("durable"),
            state_dir: root.join("state"),
            lock_dir: root.join("locks"),
            pid_dir: root.join("pids"),
            observability_backend: root.join("observability"),
            foreground: true,
            dev_mode: true,
            shutdown_after_smoke: true,
        };

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
    fn daemon_lock_conflict_blocks_start_before_state_write() {
        let project = load_project_config(workspace_root()).unwrap();
        let root = temp_root("lock");
        let options = DaemonStartOptions {
            durable_backend: root.join("durable"),
            state_dir: root.join("state"),
            lock_dir: root.join("locks"),
            pid_dir: root.join("pids"),
            observability_backend: root.join("observability"),
            foreground: true,
            dev_mode: true,
            shutdown_after_smoke: true,
        };
        fs::create_dir_all(&options.lock_dir).unwrap();
        fs::write(lock_file(&options), "pid=1\n").unwrap();

        let error = start_daemon(&project, options.clone(), &TraceFields::default()).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(!state_file(&options).exists());

        fs::remove_dir_all(root).ok();
    }
}
