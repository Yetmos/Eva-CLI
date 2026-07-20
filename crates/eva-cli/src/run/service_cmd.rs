//! Service-manager lifecycle commands.
//!
//! The command surface is deliberately thin: configuration owns the service
//! definition, lifecycle owns platform execution, and this module only binds
//! the two while preserving the public CLI envelope.

use super::{
    json_array, json_string, option_json, parse_common_options, success_envelope, trace_for,
    write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ServiceManagerKind};
use eva_core::{sha256_digest, EvaError};
use eva_lifecycle::{
    FakeServiceManagerAdapter, LaunchdAdapter, ServiceHostPlatform, ServiceManagerAdapter,
    ServiceManagerDefinition, ServiceManagerHandoffRequest, ServiceManagerMutationReport,
    ServiceManagerMutationRequest, ServiceManagerOperation, ServiceManagerRollbackRequest,
    ServiceManagerState, ServiceManagerStatusReport, ServiceManagerStatusRequest, SystemdAdapter,
    WindowsServiceAdapter,
};
use eva_policy::{MutationDecision, MutationOperation};
use eva_storage::atomic_write;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEV_STATE_VERSION: &str = "eva.service-manager-dev.v1";
const DEV_STATE_DIR: &str = "service-manager";
const DEV_STATE_MAX_BYTES: u64 = 4096;

/// Service lifecycle commands exposed by `eva service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceCommand {
    Install(ServiceOptions),
    Status(ServiceOptions),
    Start(ServiceOptions),
    Stop(ServiceOptions),
    Restart(ServiceOptions),
    Uninstall(ServiceOptions),
}

/// Common service command options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceOptions {
    common: CommonOptions,
    /// Allows the explicitly configured Fake adapter for local smoke tests.
    dev: bool,
}

/// Parses `service install|status|start|stop|restart|uninstall`.
pub(super) fn parse_service_command(args: &[String]) -> Result<ServiceCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing service subcommand"))?;
    let options = parse_service_options(rest)?;
    match subcommand.as_str() {
        "install" => Ok(ServiceCommand::Install(options)),
        "status" => Ok(ServiceCommand::Status(options)),
        "start" => Ok(ServiceCommand::Start(options)),
        "stop" => Ok(ServiceCommand::Stop(options)),
        "restart" => Ok(ServiceCommand::Restart(options)),
        "uninstall" | "remove" => Ok(ServiceCommand::Uninstall(options)),
        value => {
            Err(EvaError::unsupported("unknown service subcommand")
                .with_context("subcommand", value))
        }
    }
}

fn parse_service_options(args: &[String]) -> Result<ServiceOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut dev = false;
    for argument in args {
        if argument == "--dev" {
            if dev {
                return Err(EvaError::invalid_argument("duplicate --dev option"));
            }
            dev = true;
        } else {
            passthrough.push(argument.clone());
        }
    }
    Ok(ServiceOptions {
        common: parse_common_options(&passthrough)?,
        dev,
    })
}

/// Executes one parsed service command and writes the standard envelope.
pub(super) fn execute_service<W, E>(
    command: ServiceCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        ServiceCommand::Status(options) => execute_status(options, stdout, stderr),
        ServiceCommand::Install(options) => execute_mutation(
            options,
            ServiceManagerOperation::Install,
            "service.install",
            stdout,
            stderr,
            |adapter, definition| adapter.install(ServiceManagerMutationRequest { definition }),
        ),
        ServiceCommand::Start(options) => execute_mutation(
            options,
            ServiceManagerOperation::Start,
            "service.start",
            stdout,
            stderr,
            |adapter, definition| adapter.start(ServiceManagerMutationRequest { definition }),
        ),
        ServiceCommand::Stop(options) => execute_mutation(
            options,
            ServiceManagerOperation::Stop,
            "service.stop",
            stdout,
            stderr,
            |adapter, definition| adapter.stop(ServiceManagerMutationRequest { definition }),
        ),
        ServiceCommand::Restart(options) => execute_mutation(
            options,
            ServiceManagerOperation::Restart,
            "service.restart",
            stdout,
            stderr,
            |adapter, definition| adapter.restart(ServiceManagerMutationRequest { definition }),
        ),
        ServiceCommand::Uninstall(options) => execute_mutation(
            options,
            ServiceManagerOperation::Uninstall,
            "service.uninstall",
            stdout,
            stderr,
            |adapter, definition| adapter.uninstall(ServiceManagerMutationRequest { definition }),
        ),
    }
}

fn execute_status<W, E>(
    options: ServiceOptions,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    let trace = trace_for("cli.service.status");
    let result = prepare(&options).and_then(|(definition, adapter)| {
        adapter.status(ServiceManagerStatusRequest {
            definition: &definition,
        })
    });
    match result {
        Ok(report) => {
            write_status(stdout, options.common.output, &report, &trace)?;
            Ok(EXIT_OK)
        }
        Err(error) => write_command_error(
            stderr,
            options.common.output,
            "service.status",
            &error,
            &trace,
        ),
    }
}

fn execute_mutation<W, E, F>(
    options: ServiceOptions,
    operation: ServiceManagerOperation,
    command_name: &str,
    stdout: &mut W,
    stderr: &mut E,
    action: F,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
    F: FnOnce(
        &mut dyn ServiceManagerAdapter,
        &ServiceManagerDefinition,
    ) -> Result<ServiceManagerMutationReport, EvaError>,
{
    let trace = trace_for(match operation {
        ServiceManagerOperation::Install => "cli.service.install",
        ServiceManagerOperation::Start => "cli.service.start",
        ServiceManagerOperation::Stop => "cli.service.stop",
        ServiceManagerOperation::Restart => "cli.service.restart",
        ServiceManagerOperation::Uninstall => "cli.service.uninstall",
    });
    let result = prepare(&options).and_then(|(definition, mut adapter)| {
        let decision = MutationDecision::service_manager(
            mutation_operation(operation),
            definition.enabled,
            true,
        );
        decision.ensure_allowed()?;
        let mut report = action(adapter.as_mut(), &definition)?;
        report.audit.splice(0..0, decision.audit);
        Ok(report)
    });
    match result {
        Ok(report) => {
            write_mutation(stdout, options.common.output, command_name, &report, &trace)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            write_command_error(stderr, options.common.output, command_name, &error, &trace)
        }
    }
}

/// Loads the typed definition and creates a host-bound adapter.
///
/// Fake is intentionally unavailable unless `--dev` is explicit. Production
/// kinds are checked against the compilation host before any command can run.
fn prepare(
    options: &ServiceOptions,
) -> Result<(ServiceManagerDefinition, Box<dyn ServiceManagerAdapter>), EvaError> {
    let project = load_project_config(&options.common.project_root)?;
    let config = project.eva.service_manager.as_ref().ok_or_else(|| {
        EvaError::not_found("project does not configure a service manager")
            .with_context("project", project.project_root.display().to_string())
    })?;
    let mut definition = ServiceManagerDefinition::from(config);
    bind_production_daemon_entrypoint(&mut definition, &project.project_root)?;
    let adapter = create_adapter(&definition, options.dev, &project.project_root)?;
    Ok((definition, adapter))
}

fn bind_production_daemon_entrypoint(
    definition: &mut ServiceManagerDefinition,
    project_root: &Path,
) -> Result<(), EvaError> {
    if definition.production_adapter_enabled() {
        let executable = match definition.runtime_binary.as_ref() {
            Some(configured) if configured.is_absolute() => configured.clone(),
            Some(configured) => project_root.join(configured),
            None => std::env::current_exe().map_err(|error| {
                EvaError::internal("failed to resolve the Eva service executable")
                    .with_context("io_error", error.to_string())
            })?,
        };
        let executable = fs::canonicalize(&executable).map_err(|error| {
            EvaError::internal("failed to canonicalize the Eva service executable")
                .with_context("path", executable.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        definition.runtime_binary = Some(executable.clone());
        definition.set_daemon_entrypoint(executable, project_root.to_path_buf())?;
    }
    Ok(())
}

fn create_adapter(
    definition: &ServiceManagerDefinition,
    dev: bool,
    project_root: &Path,
) -> Result<Box<dyn ServiceManagerAdapter>, EvaError> {
    match definition.kind {
        ServiceManagerKind::Fake => {
            if !dev {
                return Err(EvaError::permission_denied(
                    "fake service manager requires explicit --dev",
                )
                .with_context("kind", definition.kind.as_str()));
            }
            Ok(Box::new(PersistentFakeServiceManagerAdapter::new(
                project_root,
                definition,
            )?))
        }
        kind => {
            if dev {
                return Err(EvaError::invalid_argument(
                    "--dev is only valid with the fake service manager",
                )
                .with_context("kind", kind.as_str()));
            }
            let host = ServiceHostPlatform::current();
            if host.service_manager_kind() != Some(kind) {
                return Err(EvaError::unsupported(
                    "configured service manager does not match the host platform",
                )
                .with_context("host_platform", host.as_str())
                .with_context("requested_kind", kind.as_str())
                .with_context(
                    "expected_kind",
                    host.service_manager_kind()
                        .map_or("none", ServiceManagerKind::as_str),
                ));
            }
            match kind {
                ServiceManagerKind::WindowsService => {
                    Ok(Box::new(WindowsServiceAdapter::native()?))
                }
                ServiceManagerKind::Systemd => Ok(Box::new(SystemdAdapter::native()?)),
                ServiceManagerKind::Launchd => Ok(Box::new(LaunchdAdapter::native(definition)?)),
                ServiceManagerKind::Fake => unreachable!("fake handled above"),
            }
        }
    }
}

fn mutation_operation(operation: ServiceManagerOperation) -> MutationOperation {
    match operation {
        ServiceManagerOperation::Install => MutationOperation::ServiceInstall,
        ServiceManagerOperation::Start => MutationOperation::ServiceStart,
        ServiceManagerOperation::Stop => MutationOperation::ServiceStop,
        ServiceManagerOperation::Restart => MutationOperation::ServiceRestart,
        ServiceManagerOperation::Uninstall => MutationOperation::ServiceUninstall,
    }
}

fn write_mutation<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    command: &str,
    report: &ServiceManagerMutationReport,
    trace: &eva_observability::TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(command, EXIT_OK, &mutation_json(report), trace)
        )
        .map_err(write_error_kind),
        OutputFormat::Text => {
            writeln!(writer, "service_manager: {}", report.kind.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "service_name: {}", report.service_name).map_err(write_error_kind)?;
            writeln!(writer, "operation: {}", report.operation.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "state: {}", report.state.as_str()).map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: {}", report.mutation_executed)
                .map_err(write_error_kind)?;
            writeln!(writer, "production_adapter: {}", report.production_adapter)
                .map_err(write_error_kind)?;
            for (index, audit) in report.audit.iter().enumerate() {
                writeln!(writer, "audit[{index}]: {audit}").map_err(write_error_kind)?;
            }
            Ok(())
        }
    }
}

fn write_status<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &ServiceManagerStatusReport,
    trace: &eva_observability::TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("service.status", EXIT_OK, &status_json(report), trace)
        )
        .map_err(write_error_kind),
        OutputFormat::Text => {
            writeln!(writer, "service_manager: {}", report.kind.as_str())
                .map_err(write_error_kind)?;
            writeln!(writer, "service_name: {}", report.service_name).map_err(write_error_kind)?;
            writeln!(writer, "configured: {}", report.configured).map_err(write_error_kind)?;
            writeln!(writer, "state: {}", report.state.as_str()).map_err(write_error_kind)?;
            writeln!(writer, "production_adapter: {}", report.production_adapter)
                .map_err(write_error_kind)?;
            for (index, audit) in report.audit.iter().enumerate() {
                writeln!(writer, "audit[{index}]: {audit}").map_err(write_error_kind)?;
            }
            Ok(())
        }
    }
}

fn mutation_json(report: &ServiceManagerMutationReport) -> String {
    format!(
        "{{\"kind\":{},\"service_name\":{},\"operation\":{},\"state\":{},\"mutation_executed\":{},\"production_adapter\":{},\"audit\":{}}}",
        json_string(report.kind.as_str()),
        json_string(&report.service_name),
        json_string(report.operation.as_str()),
        json_string(report.state.as_str()),
        report.mutation_executed,
        report.production_adapter,
        json_array(report.audit.iter().map(|entry| json_string(entry))),
    )
}

fn status_json(report: &ServiceManagerStatusReport) -> String {
    format!(
        "{{\"kind\":{},\"service_name\":{},\"configured\":{},\"production_adapter\":{},\"state\":{},\"mutation_executed\":false,\"active_generation\":{},\"active_release\":{},\"candidate_generation\":{},\"audit\":{}}}",
        json_string(report.kind.as_str()),
        json_string(&report.service_name),
        report.configured,
        report.production_adapter,
        json_string(report.state.as_str()),
        option_json(report.active_generation.as_deref()),
        option_json(report.active_release.as_deref()),
        option_json(report.candidate_generation.as_deref()),
        json_array(report.audit.iter().map(|entry| json_string(entry))),
    )
}

/// Persistent state used only by the explicitly selected development Fake.
///
/// Production adapters read their state from the host service manager. The
/// Fake otherwise loses its state when each CLI invocation exits, so this
/// small adapter keeps the same typed lifecycle contract across commands while
/// remaining visibly scoped to `--dev` and to the canonical project/service
/// identity. The lock is intentionally conservative: a crashed development
/// process leaves a lock that must be inspected/removed by the developer.
struct PersistentFakeServiceManagerAdapter {
    inner: FakeServiceManagerAdapter,
    state: DevFakeStateStore,
}

impl PersistentFakeServiceManagerAdapter {
    fn new(project_root: &Path, definition: &ServiceManagerDefinition) -> Result<Self, EvaError> {
        let state = DevFakeStateStore::acquire(project_root, &definition.service_name)?;
        let persisted = state.read_state()?;
        let mut inner = FakeServiceManagerAdapter::new();
        match persisted {
            ServiceManagerState::NotInstalled => {}
            ServiceManagerState::Stopped => {
                inner.install(ServiceManagerMutationRequest { definition })?;
            }
            ServiceManagerState::Running => {
                inner.install(ServiceManagerMutationRequest { definition })?;
                inner.start(ServiceManagerMutationRequest { definition })?;
            }
        }
        Ok(Self { inner, state })
    }

    fn persist(&self, state: ServiceManagerState) -> Result<(), EvaError> {
        self.state.write_state(state)
    }

    fn persist_current(&self, definition: &ServiceManagerDefinition) -> Result<(), EvaError> {
        let report = self
            .inner
            .status(ServiceManagerStatusRequest { definition })?;
        self.persist(report.state)
    }
}

impl ServiceManagerAdapter for PersistentFakeServiceManagerAdapter {
    fn kind(&self) -> ServiceManagerKind {
        self.inner.kind()
    }

    fn install(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let report = self.inner.install(request)?;
        self.persist(report.state)?;
        Ok(report)
    }

    fn uninstall(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let report = self.inner.uninstall(request)?;
        self.persist(report.state)?;
        Ok(report)
    }

    fn status(
        &self,
        request: ServiceManagerStatusRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError> {
        self.inner.status(request)
    }

    fn start(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let report = self.inner.start(request)?;
        self.persist(report.state)?;
        Ok(report)
    }

    fn stop(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let report = self.inner.stop(request)?;
        self.persist(report.state)?;
        Ok(report)
    }

    fn restart(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let report = self.inner.restart(request)?;
        self.persist(report.state)?;
        Ok(report)
    }

    fn handoff(
        &mut self,
        request: ServiceManagerHandoffRequest<'_>,
    ) -> Result<eva_lifecycle::ServiceManagerHandoffReport, EvaError> {
        let definition = request.definition;
        let report = self.inner.handoff(request)?;
        self.persist_current(definition)?;
        Ok(report)
    }

    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<eva_lifecycle::ServiceManagerRollbackReport, EvaError> {
        let definition = request.definition;
        let report = self.inner.rollback(request)?;
        self.persist_current(definition)?;
        Ok(report)
    }
}

/// A project/service-scoped lock and atomic state file for the development Fake.
struct DevFakeStateStore {
    state_path: PathBuf,
    lock_path: PathBuf,
    service_name: String,
    lock_token: String,
    _lock_file: File,
}

impl DevFakeStateStore {
    fn acquire(project_root: &Path, service_name: &str) -> Result<Self, EvaError> {
        let eva_dir = project_root.join(".eva");
        ensure_directory(&eva_dir)?;
        let root = eva_dir.join(DEV_STATE_DIR);
        ensure_directory(&root)?;

        let (state_path, lock_path) = dev_state_paths(project_root, service_name, &root);
        reject_non_regular_entry(&state_path, "development service state")?;
        reject_non_regular_entry(&lock_path, "development service lock")?;

        let mut lock_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    EvaError::conflict("development service manager state is locked")
                        .with_context("service_name", service_name)
                } else {
                    EvaError::internal("failed to create development service manager lock")
                        .with_context("service_name", service_name)
                }
            })?;
        let payload = format!(
            "version=1\npid={}\nstarted_unix_ms={}\n",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default()
        );
        if let Err(error) = lock_file
            .write_all(payload.as_bytes())
            .and_then(|_| lock_file.sync_all())
        {
            let _ = fs::remove_file(&lock_path);
            return Err(
                EvaError::internal("failed to initialize development service lock")
                    .with_context("service_name", service_name)
                    .with_context("io_error", error.to_string()),
            );
        }

        Ok(Self {
            state_path,
            lock_path,
            service_name: service_name.to_owned(),
            lock_token: payload,
            _lock_file: lock_file,
        })
    }

    fn read_state(&self) -> Result<ServiceManagerState, EvaError> {
        let metadata = match fs::symlink_metadata(&self.state_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ServiceManagerState::NotInstalled)
            }
            Err(_) => {
                return Err(
                    EvaError::internal("failed to inspect development service state")
                        .with_context("service_name", &self.service_name),
                )
            }
        };
        if !metadata.file_type().is_file() || metadata.len() > DEV_STATE_MAX_BYTES {
            return Err(
                EvaError::conflict("development service state is corrupt or unsupported")
                    .with_context("service_name", &self.service_name),
            );
        }
        let mut file = File::open(&self.state_path).map_err(|_| {
            EvaError::internal("failed to open development service state")
                .with_context("service_name", &self.service_name)
        })?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes).map_err(|_| {
            EvaError::internal("failed to read development service state")
                .with_context("service_name", &self.service_name)
        })?;
        if bytes.len() as u64 > DEV_STATE_MAX_BYTES {
            return Err(
                EvaError::conflict("development service state exceeds the read limit")
                    .with_context("service_name", &self.service_name),
            );
        }
        parse_dev_state(&bytes).map_err(|message| {
            EvaError::conflict(message).with_context("service_name", &self.service_name)
        })
    }

    fn write_state(&self, state: ServiceManagerState) -> Result<(), EvaError> {
        let content = format!("version={DEV_STATE_VERSION}\nstate={}\n", state.as_str());
        atomic_write(&self.state_path, content.as_bytes()).map_err(|error| {
            EvaError::internal("failed to persist development service state")
                .with_context("service_name", &self.service_name)
                .with_context("io_error", error.to_string())
        })
    }
}

pub(super) fn dev_state_paths(
    project_root: &Path,
    service_name: &str,
    root: &Path,
) -> (PathBuf, PathBuf) {
    let mut identity = project_root.to_string_lossy().into_owned();
    identity.push('\0');
    identity.push_str(service_name);
    let digest = sha256_digest(identity.as_bytes());
    let stem = digest.strip_prefix("sha256:").unwrap_or(&digest);
    (
        root.join(format!("{stem}.state")),
        root.join(format!("{stem}.lock")),
    )
}

impl Drop for DevFakeStateStore {
    fn drop(&mut self) {
        let owns_lock = fs::symlink_metadata(&self.lock_path)
            .map(|metadata| metadata.file_type().is_file())
            .ok()
            .and_then(|is_file| is_file.then(|| fs::read_to_string(&self.lock_path).ok()))
            .flatten()
            .is_some_and(|token| token == self.lock_token);
        if owns_lock {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn ensure_directory(path: &Path) -> Result<(), EvaError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(EvaError::conflict(
                "development service state directory is not a regular directory",
            ));
        }
        return Ok(());
    }
    fs::create_dir_all(path)
        .map_err(|_| EvaError::internal("failed to create development service state directory"))
}

fn reject_non_regular_entry(path: &Path, field: &'static str) -> Result<(), EvaError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(
            EvaError::conflict("development service state entry is not regular")
                .with_context("field", field),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(
            EvaError::internal("failed to inspect development service state entry")
                .with_context("field", field),
        ),
    }
}

fn parse_dev_state(bytes: &[u8]) -> Result<ServiceManagerState, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "development service state is not UTF-8")?;
    let mut version = None;
    let mut state = None;
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or("development service state has an invalid field")?;
        match key {
            "state" => {
                if state.replace(value).is_some() {
                    return Err("development service state has duplicate state");
                }
            }
            "version" => {
                if version.replace(value).is_some() {
                    return Err("development service state has duplicate version");
                }
                if value != DEV_STATE_VERSION {
                    return Err("development service state has an unsupported version");
                }
            }
            _ => return Err("development service state contains an unknown field"),
        }
    }
    if version.is_none() || state.is_none() {
        return Err("development service state is incomplete");
    }
    match state.unwrap() {
        "not_installed" => Ok(ServiceManagerState::NotInstalled),
        "stopped" => Ok(ServiceManagerState::Stopped),
        "running" => Ok(ServiceManagerState::Running),
        _ => Err("development service state has an unsupported service state"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> ServiceManagerDefinition {
        ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-test")
            .expect("valid fake definition")
    }

    #[test]
    fn parser_requires_a_known_subcommand_and_preserves_common_options() {
        let parsed = parse_service_command(&[
            "status".to_owned(),
            "--dev".to_owned(),
            "--project".to_owned(),
            "workspace".to_owned(),
            "--output".to_owned(),
            "json".to_owned(),
        ])
        .expect("service status should parse");
        match parsed {
            ServiceCommand::Status(options) => {
                assert!(options.dev);
                assert_eq!(
                    options.common.project_root,
                    std::path::PathBuf::from("workspace")
                );
                assert_eq!(options.common.output, OutputFormat::Json);
            }
            _ => panic!("unexpected service command"),
        }
        assert!(parse_service_command(&["unknown".to_owned()]).is_err());
        assert!(parse_service_command(&[
            "status".to_owned(),
            "--dev".to_owned(),
            "--dev".to_owned()
        ])
        .is_err());
    }

    #[test]
    fn fake_adapter_lifecycle_reports_idempotent_mutations() {
        let definition = definition();
        let mut adapter = FakeServiceManagerAdapter::new();
        let first = adapter
            .install(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .expect("first install");
        let second = adapter
            .install(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .expect("idempotent install");
        assert!(first.mutation_executed);
        assert!(!second.mutation_executed);
        assert_eq!(second.state, eva_lifecycle::ServiceManagerState::Stopped);

        let started = adapter
            .start(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .expect("start");
        let stopped = adapter
            .stop(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .expect("stop");
        assert!(started.mutation_executed);
        assert!(stopped.mutation_executed);
        assert_eq!(stopped.state, eva_lifecycle::ServiceManagerState::Stopped);
    }

    #[test]
    fn dev_fake_state_survives_separate_adapter_instances_and_full_lifecycle() {
        let root = std::env::temp_dir().join(format!(
            "eva-service-cli-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let definition = definition();

        {
            let mut adapter = PersistentFakeServiceManagerAdapter::new(&root, &definition)
                .expect("initial fake adapter");
            let installed = adapter
                .install(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .expect("install fake service");
            assert_eq!(installed.state, ServiceManagerState::Stopped);
            assert!(installed.mutation_executed);
        }
        {
            let mut adapter = PersistentFakeServiceManagerAdapter::new(&root, &definition)
                .expect("reopened fake adapter");
            let status = adapter
                .status(ServiceManagerStatusRequest {
                    definition: &definition,
                })
                .expect("status after reopen");
            assert_eq!(status.state, ServiceManagerState::Stopped);
            let started = adapter
                .start(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .expect("start after reopen");
            assert_eq!(started.state, ServiceManagerState::Running);
        }
        {
            let mut adapter = PersistentFakeServiceManagerAdapter::new(&root, &definition)
                .expect("reopened running fake adapter");
            let stopped = adapter
                .stop(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .expect("stop after reopen");
            assert_eq!(stopped.state, ServiceManagerState::Stopped);
            let restarted = adapter
                .restart(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .expect("restart after reopen");
            assert_eq!(restarted.state, ServiceManagerState::Running);
            let uninstalled = adapter
                .uninstall(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .expect("uninstall after reopen");
            assert_eq!(uninstalled.state, ServiceManagerState::NotInstalled);
        }
        {
            let adapter = PersistentFakeServiceManagerAdapter::new(&root, &definition)
                .expect("reopened uninstalled fake adapter");
            let status = adapter
                .status(ServiceManagerStatusRequest {
                    definition: &definition,
                })
                .expect("final status");
            assert_eq!(status.state, ServiceManagerState::NotInstalled);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fake_requires_explicit_development_mode() {
        let definition = definition();
        let options = ServiceOptions {
            common: CommonOptions {
                project_root: std::path::PathBuf::from("."),
                output: OutputFormat::Text,
            },
            dev: false,
        };
        let error = match create_adapter(&definition, options.dev, Path::new(".")) {
            Ok(_) => panic!("fake must be gated"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }

    #[test]
    fn production_adapter_rejects_dev_and_wrong_host_before_execution() {
        let host = ServiceHostPlatform::current();
        let wrong_kind = match host.service_manager_kind() {
            Some(ServiceManagerKind::WindowsService) => ServiceManagerKind::Systemd,
            Some(ServiceManagerKind::Systemd) => ServiceManagerKind::Launchd,
            Some(ServiceManagerKind::Launchd) | None => ServiceManagerKind::Systemd,
            Some(ServiceManagerKind::Fake) => unreachable!("hosts never map to fake"),
        };
        let definition = ServiceManagerDefinition::new(true, wrong_kind, "eva-test")
            .expect("valid production definition");

        let dev_error = match create_adapter(&definition, true, Path::new(".")) {
            Ok(_) => panic!("production adapter must reject --dev"),
            Err(error) => error,
        };
        assert_eq!(dev_error.kind(), eva_core::ErrorKind::InvalidArgument);

        let host_error = match create_adapter(&definition, false, Path::new(".")) {
            Ok(_) => panic!("wrong-host adapter must fail before execution"),
            Err(error) => error,
        };
        assert_eq!(host_error.kind(), eva_core::ErrorKind::Unsupported);
    }

    #[test]
    fn production_definition_binds_direct_daemon_entrypoint_but_fake_does_not() {
        let Some(kind) = ServiceHostPlatform::current().service_manager_kind() else {
            return;
        };
        let project_root =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")).unwrap();
        let executable = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let mut production = ServiceManagerDefinition::new(true, kind, "eva-test").unwrap();
        production.runtime_binary = Some(executable.clone());
        bind_production_daemon_entrypoint(&mut production, &project_root).unwrap();
        let entrypoint = production.service_entrypoint().unwrap();
        assert_eq!(entrypoint.executable, executable);
        assert_eq!(entrypoint.working_directory, project_root);
        assert!(entrypoint.is_daemon_entrypoint());
        entrypoint.validate().unwrap();

        let mut fake = definition();
        bind_production_daemon_entrypoint(&mut fake, &project_root).unwrap();
        assert!(fake.service_entrypoint().is_none());
    }
}
