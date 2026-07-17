//! Windows Service Control Manager adapter backed by structured `sc.exe` argv.

use crate::service_command::{
    ProcessServiceCommandExecutor, ServiceCommand, ServiceCommandArg, ServiceCommandExecutor,
    ServiceCommandReport, ServiceCommandTermination,
};
use crate::service_factory::{ServiceHostPlatform, ServiceManagerFactory};
use crate::service_manager::{
    ServiceManagerAdapter, ServiceManagerDefinition, ServiceManagerHandoffReport,
    ServiceManagerHandoffRequest, ServiceManagerKind, ServiceManagerMutationReport,
    ServiceManagerMutationRequest, ServiceManagerOperation, ServiceManagerRollbackReport,
    ServiceManagerRollbackRequest, ServiceManagerState, ServiceManagerStatusReport,
    ServiceManagerStatusRequest,
};
use eva_core::EvaError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_WINDOWS_SERVICE_POLL_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_WINDOWS_SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WINDOWS_LOCAL_SERVICE_ACCOUNT: &str = r"NT AUTHORITY\LocalService";
const WINDOWS_SERVICE_NAME_MAX_UTF16: usize = 256;

const SC_ERROR_ACCESS_DENIED: i32 = 5;
const SC_ERROR_INVALID_PARAMETER: i32 = 87;
const SC_ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const SC_ERROR_SERVICE_DISABLED: i32 = 1058;
const SC_ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const SC_ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
const SC_ERROR_SERVICE_EXISTS: i32 = 1073;
const SC_ERROR_SERVICE_MARKED_FOR_DELETE: i32 = 1072;

const SC_START_AUTO: u32 = 2;
const SC_START_DEMAND: u32 = 3;

/// Production Windows service adapter. All SCM commands pass through a
/// host-bound factory and are serialized so status remains callable by `&self`.
pub struct WindowsServiceAdapter<E> {
    factory: Mutex<ServiceManagerFactory<E>>,
    sc_executable: PathBuf,
    poll_timeout: Duration,
    poll_interval: Duration,
}

impl<E> WindowsServiceAdapter<E>
where
    E: ServiceCommandExecutor,
{
    /// Creates an adapter only on a Windows host and resolves trusted `sc.exe`
    /// from the native system directory rather than mutable PATH input.
    pub fn new(executor: E) -> Result<Self, EvaError> {
        if ServiceHostPlatform::current() != ServiceHostPlatform::Windows {
            return Err(
                EvaError::unsupported("Windows service adapter requires a Windows host")
                    .with_context("host_platform", ServiceHostPlatform::current().as_str()),
            );
        }
        Ok(Self {
            factory: Mutex::new(ServiceManagerFactory::new(executor)),
            sc_executable: windows_sc_executable()?,
            poll_timeout: DEFAULT_WINDOWS_SERVICE_POLL_TIMEOUT,
            poll_interval: DEFAULT_WINDOWS_SERVICE_POLL_INTERVAL,
        })
    }

    #[cfg(test)]
    fn for_test(executor: E) -> Self {
        Self {
            factory: Mutex::new(ServiceManagerFactory::for_host(
                ServiceHostPlatform::Windows,
                executor,
            )),
            sc_executable: PathBuf::from("manager-program"),
            poll_timeout: Duration::from_secs(1),
            poll_interval: Duration::ZERO,
        }
    }

    fn execute(&self, arguments: Vec<ServiceCommandArg>) -> Result<ServiceCommandReport, EvaError> {
        let command = ServiceCommand::new(self.sc_executable.clone(), arguments)?;
        let mut factory = self
            .factory
            .lock()
            .map_err(|_| EvaError::internal("Windows service command factory lock is poisoned"))?;
        factory.execute_command(ServiceManagerKind::WindowsService, &command)
    }

    fn validate_definition(
        definition: &ServiceManagerDefinition,
        mutation: bool,
    ) -> Result<(), EvaError> {
        if definition.kind != ServiceManagerKind::WindowsService {
            return Err(EvaError::unsupported(
                "Windows service adapter requires windows_service kind",
            )
            .with_context("requested_kind", definition.kind.as_str()));
        }
        if mutation && !definition.enabled {
            return Err(EvaError::invalid_argument(
                "Windows service mutation requires an enabled definition",
            ));
        }
        validate_windows_service_name(&definition.service_name)
    }

    fn install_binary(definition: &ServiceManagerDefinition) -> Result<PathBuf, EvaError> {
        let binary = definition.runtime_binary.as_ref().ok_or_else(|| {
            EvaError::invalid_argument("Windows service install requires runtime_binary")
        })?;
        if !binary.is_absolute() {
            return Err(EvaError::invalid_argument(
                "Windows service runtime_binary must be absolute",
            ));
        }
        let metadata = std::fs::metadata(binary).map_err(|error| {
            EvaError::not_found("Windows service runtime_binary does not exist")
                .with_context("io_error", error.to_string())
        })?;
        if !metadata.is_file() {
            return Err(EvaError::invalid_argument(
                "Windows service runtime_binary must be a regular file",
            ));
        }
        std::fs::canonicalize(binary).map_err(|error| {
            EvaError::unavailable("failed to canonicalize Windows service runtime_binary")
                .with_context("io_error", error.to_string())
        })
    }

    fn query_config(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<(Option<WindowsServiceConfigSnapshot>, Vec<String>), EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("qc"),
            ServiceCommandArg::public(&definition.service_name),
        ])?;
        let audit = adapter_command_audit(&report, "query_config");
        ensure_process_completion(&report, "query_config")?;
        if report.exit_code == Some(SC_ERROR_SERVICE_DOES_NOT_EXIST) {
            return Ok((None, audit));
        }
        ensure_success_exit(&report, "query_config")?;
        Ok((Some(parse_service_config(&report)?), audit))
    }

    fn query_observed_state(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<(WindowsObservedState, Vec<String>), EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("query"),
            ServiceCommandArg::public(&definition.service_name),
        ])?;
        let audit = adapter_command_audit(&report, "query_state");
        ensure_process_completion(&report, "query_state")?;
        if report.exit_code == Some(SC_ERROR_SERVICE_DOES_NOT_EXIST) {
            return Ok((WindowsObservedState::NotInstalled, audit));
        }
        if report.exit_code == Some(SC_ERROR_SERVICE_MARKED_FOR_DELETE) {
            return Ok((WindowsObservedState::PendingDelete, audit));
        }
        ensure_success_exit(&report, "query_state")?;
        Ok((parse_service_state(&report)?, audit))
    }

    fn wait_for_state(
        &self,
        definition: &ServiceManagerDefinition,
        expected: ServiceManagerState,
    ) -> Result<(ServiceManagerState, Vec<String>), EvaError> {
        let started_at = Instant::now();
        loop {
            let (observed, audit) = self.query_observed_state(definition)?;
            if observed.service_state() == Some(expected) {
                return Ok((expected, audit));
            }
            if started_at.elapsed() >= self.poll_timeout {
                return Err(
                    EvaError::timeout("Windows service state transition timed out")
                        .with_context("expected_state", expected.as_str()),
                );
            }
            sleep_poll_interval(self.poll_interval);
        }
    }

    fn wait_for_stable_state(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<(ServiceManagerState, Vec<String>), EvaError> {
        let started_at = Instant::now();
        loop {
            let (observed, audit) = self.query_observed_state(definition)?;
            if let Some(state) = observed.service_state() {
                return Ok((state, audit));
            }
            if started_at.elapsed() >= self.poll_timeout {
                return Err(EvaError::timeout(
                    "Windows service did not reach a stable state",
                ));
            }
            sleep_poll_interval(self.poll_interval);
        }
    }

    fn run_create(
        &self,
        definition: &ServiceManagerDefinition,
        binary: &Path,
    ) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("create"),
            ServiceCommandArg::public(&definition.service_name),
            ServiceCommandArg::public("binPath="),
            ServiceCommandArg::secret(quoted_windows_executable(binary)),
            ServiceCommandArg::public("start="),
            ServiceCommandArg::public(start_mode_arg(definition.start_on_boot)),
            ServiceCommandArg::public("obj="),
            ServiceCommandArg::public(WINDOWS_LOCAL_SERVICE_ACCOUNT),
        ])?;
        let audit = adapter_command_audit(&report, "create");
        ensure_success_exit(&report, "create")?;
        Ok(audit)
    }

    fn run_start(&self, definition: &ServiceManagerDefinition) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("start"),
            ServiceCommandArg::public(&definition.service_name),
        ])?;
        let audit = adapter_command_audit(&report, "start");
        ensure_process_completion(&report, "start")?;
        if report.exit_code != Some(SC_ERROR_SERVICE_ALREADY_RUNNING) {
            ensure_success_exit(&report, "start")?;
        }
        Ok(audit)
    }

    fn run_stop(&self, definition: &ServiceManagerDefinition) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("stop"),
            ServiceCommandArg::public(&definition.service_name),
        ])?;
        let audit = adapter_command_audit(&report, "stop");
        ensure_process_completion(&report, "stop")?;
        if report.exit_code != Some(SC_ERROR_SERVICE_NOT_ACTIVE) {
            ensure_success_exit(&report, "stop")?;
        }
        Ok(audit)
    }

    fn run_delete(&self, definition: &ServiceManagerDefinition) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("delete"),
            ServiceCommandArg::public(&definition.service_name),
        ])?;
        let audit = adapter_command_audit(&report, "delete");
        ensure_process_completion(&report, "delete")?;
        if !matches!(
            report.exit_code,
            Some(0 | SC_ERROR_SERVICE_DOES_NOT_EXIST | SC_ERROR_SERVICE_MARKED_FOR_DELETE)
        ) {
            return Err(command_exit_error("delete", &report));
        }
        Ok(audit)
    }

    fn run_start_mode_config(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("config"),
            ServiceCommandArg::public(&definition.service_name),
            ServiceCommandArg::public("start="),
            ServiceCommandArg::public(start_mode_arg(definition.start_on_boot)),
        ])?;
        let audit = adapter_command_audit(&report, "config_start_mode");
        ensure_success_exit(&report, "config_start_mode")?;
        Ok(audit)
    }

    fn status_report(
        definition: &ServiceManagerDefinition,
        state: ServiceManagerState,
        mut audit: Vec<String>,
    ) -> ServiceManagerStatusReport {
        audit.push(format!("windows_service.state:{}", state.as_str()));
        ServiceManagerStatusReport {
            kind: ServiceManagerKind::WindowsService,
            service_name: definition.service_name.clone(),
            configured: definition.enabled,
            production_adapter: true,
            state,
            active_generation: None,
            active_release: None,
            candidate_generation: None,
            audit,
        }
    }

    fn mutation_report(
        definition: &ServiceManagerDefinition,
        operation: ServiceManagerOperation,
        state: ServiceManagerState,
        mutation_executed: bool,
        mut audit: Vec<String>,
    ) -> ServiceManagerMutationReport {
        audit.extend([
            format!("windows_service.operation:{}", operation.as_str()),
            format!("service_manager.mutation_executed:{mutation_executed}"),
            format!("service_manager.state:{}", state.as_str()),
        ]);
        ServiceManagerMutationReport {
            kind: ServiceManagerKind::WindowsService,
            service_name: definition.service_name.clone(),
            operation,
            state,
            mutation_executed,
            production_adapter: true,
            audit,
        }
    }
}

impl WindowsServiceAdapter<ProcessServiceCommandExecutor> {
    /// Creates the native production adapter with default command limits.
    pub fn native() -> Result<Self, EvaError> {
        Self::new(ProcessServiceCommandExecutor::default())
    }
}

impl<E> ServiceManagerAdapter for WindowsServiceAdapter<E>
where
    E: ServiceCommandExecutor,
{
    fn kind(&self) -> ServiceManagerKind {
        ServiceManagerKind::WindowsService
    }

    fn install(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        Self::validate_definition(definition, true)?;
        let binary = Self::install_binary(definition)?;
        let (existing, mut audit) = self.query_config(definition)?;
        let mut mutation_executed = false;

        if let Some(existing) = existing {
            if !binary_path_matches(&existing.binary_path, &binary) {
                return Err(EvaError::conflict(
                    "existing Windows service binary path does not match definition",
                ));
            }
            if !existing
                .service_account
                .eq_ignore_ascii_case(WINDOWS_LOCAL_SERVICE_ACCOUNT)
            {
                return Err(EvaError::conflict(
                    "existing Windows service account is not LocalService",
                ));
            }
            if existing.start_type != desired_start_type(definition.start_on_boot) {
                audit.extend(self.run_start_mode_config(definition)?);
                let (verified, verify_audit) = self.query_config(definition)?;
                audit.extend(verify_audit);
                let verified = verified.ok_or_else(|| {
                    EvaError::conflict("Windows service disappeared after start-mode config")
                })?;
                if verified.start_type != desired_start_type(definition.start_on_boot) {
                    return Err(EvaError::conflict(
                        "Windows service start mode did not converge",
                    ));
                }
                mutation_executed = true;
            }
        } else {
            audit.extend(self.run_create(definition, &binary)?);
            mutation_executed = true;
        }

        let (state, state_audit) = self.wait_for_stable_state(definition)?;
        audit.extend(state_audit);
        if state == ServiceManagerState::NotInstalled {
            return Err(EvaError::conflict(
                "Windows service was not installed after create or config",
            ));
        }
        Ok(Self::mutation_report(
            definition,
            ServiceManagerOperation::Install,
            state,
            mutation_executed,
            audit,
        ))
    }

    fn uninstall(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        Self::validate_definition(definition, true)?;
        let (observed, mut audit) = self.query_observed_state(definition)?;
        if observed == WindowsObservedState::NotInstalled {
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Uninstall,
                ServiceManagerState::NotInstalled,
                false,
                audit,
            ));
        }

        let (state, state_audit) = if observed.service_state().is_some() {
            (observed.service_state().unwrap(), Vec::new())
        } else {
            self.wait_for_stable_state(definition)?
        };
        audit.extend(state_audit);
        if state == ServiceManagerState::NotInstalled {
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Uninstall,
                ServiceManagerState::NotInstalled,
                false,
                audit,
            ));
        }
        if state == ServiceManagerState::Running {
            audit.extend(self.run_stop(definition)?);
            let (_, stop_audit) = self.wait_for_state(definition, ServiceManagerState::Stopped)?;
            audit.extend(stop_audit);
        }
        audit.extend(self.run_delete(definition)?);
        let (_, delete_audit) =
            self.wait_for_state(definition, ServiceManagerState::NotInstalled)?;
        audit.extend(delete_audit);
        Ok(Self::mutation_report(
            definition,
            ServiceManagerOperation::Uninstall,
            ServiceManagerState::NotInstalled,
            true,
            audit,
        ))
    }

    fn status(
        &self,
        request: ServiceManagerStatusRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError> {
        let definition = request.definition;
        Self::validate_definition(definition, false)?;
        let (observed, audit) = self.query_observed_state(definition)?;
        let state = observed.service_state().ok_or_else(|| {
            EvaError::unavailable("Windows service is in a transitional state")
                .with_context("scm_state", observed.as_str())
        })?;
        Ok(Self::status_report(definition, state, audit))
    }

    fn start(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        Self::validate_definition(definition, true)?;
        let (state, mut audit) = self.wait_for_stable_state(definition)?;
        if state == ServiceManagerState::NotInstalled {
            return Err(EvaError::not_found("Windows service is not installed"));
        }
        if state == ServiceManagerState::Running {
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Start,
                state,
                false,
                audit,
            ));
        }
        audit.extend(self.run_start(definition)?);
        let (_, final_audit) = self.wait_for_state(definition, ServiceManagerState::Running)?;
        audit.extend(final_audit);
        Ok(Self::mutation_report(
            definition,
            ServiceManagerOperation::Start,
            ServiceManagerState::Running,
            true,
            audit,
        ))
    }

    fn stop(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        Self::validate_definition(definition, true)?;
        let (state, mut audit) = self.wait_for_stable_state(definition)?;
        if state != ServiceManagerState::Running {
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Stop,
                state,
                false,
                audit,
            ));
        }
        audit.extend(self.run_stop(definition)?);
        let (_, final_audit) = self.wait_for_state(definition, ServiceManagerState::Stopped)?;
        audit.extend(final_audit);
        Ok(Self::mutation_report(
            definition,
            ServiceManagerOperation::Stop,
            ServiceManagerState::Stopped,
            true,
            audit,
        ))
    }

    fn restart(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        Self::validate_definition(definition, true)?;
        let (state, mut audit) = self.wait_for_stable_state(definition)?;
        if state == ServiceManagerState::NotInstalled {
            return Err(EvaError::not_found("Windows service is not installed"));
        }
        if state == ServiceManagerState::Running {
            audit.extend(self.run_stop(definition)?);
            let (_, stop_audit) = self.wait_for_state(definition, ServiceManagerState::Stopped)?;
            audit.extend(stop_audit);
        }
        audit.extend(self.run_start(definition)?);
        let (_, start_audit) = self.wait_for_state(definition, ServiceManagerState::Running)?;
        audit.extend(start_audit);
        Ok(Self::mutation_report(
            definition,
            ServiceManagerOperation::Restart,
            ServiceManagerState::Running,
            true,
            audit,
        ))
    }

    fn handoff(
        &mut self,
        request: ServiceManagerHandoffRequest<'_>,
    ) -> Result<ServiceManagerHandoffReport, EvaError> {
        Self::validate_definition(request.definition, true)?;
        Err(EvaError::unsupported(
            "Windows service generation handoff is not implemented yet",
        ))
    }

    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError> {
        Self::validate_definition(request.definition, true)?;
        Err(EvaError::unsupported(
            "Windows service generation rollback is not implemented yet",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsServiceConfigSnapshot {
    binary_path: String,
    start_type: u32,
    service_account: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsObservedState {
    NotInstalled,
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    PendingDelete,
}

impl WindowsObservedState {
    const fn service_state(self) -> Option<ServiceManagerState> {
        match self {
            Self::NotInstalled => Some(ServiceManagerState::NotInstalled),
            Self::Stopped => Some(ServiceManagerState::Stopped),
            // SCM keeps a paused service process active. Treat it as running at
            // this three-state boundary so stop/uninstall still terminate it.
            Self::Running | Self::Paused => Some(ServiceManagerState::Running),
            Self::StartPending
            | Self::StopPending
            | Self::ContinuePending
            | Self::PausePending
            | Self::PendingDelete => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::Stopped => "stopped",
            Self::StartPending => "start_pending",
            Self::StopPending => "stop_pending",
            Self::Running => "running",
            Self::ContinuePending => "continue_pending",
            Self::PausePending => "pause_pending",
            Self::Paused => "paused",
            Self::PendingDelete => "pending_delete",
        }
    }
}

fn validate_windows_service_name(service_name: &str) -> Result<(), EvaError> {
    let utf16_len = service_name.encode_utf16().count();
    if utf16_len == 0 || utf16_len > WINDOWS_SERVICE_NAME_MAX_UTF16 {
        return Err(EvaError::invalid_argument(
            "Windows service name length is invalid",
        ));
    }
    if service_name.chars().any(|character| {
        character == '/' || character == '\\' || character == '"' || character.is_control()
    }) {
        return Err(EvaError::invalid_argument(
            "Windows service name contains an unsupported character",
        ));
    }
    Ok(())
}

fn parse_service_config(
    report: &ServiceCommandReport,
) -> Result<WindowsServiceConfigSnapshot, EvaError> {
    let output = String::from_utf8_lossy(report.stdout.bytes());
    let binary_path = sc_field(&output, "BINARY_PATH_NAME")?.to_owned();
    let start_type = sc_numeric_field(&output, "START_TYPE")?;
    let service_account = sc_field(&output, "SERVICE_START_NAME")?.to_owned();
    Ok(WindowsServiceConfigSnapshot {
        binary_path,
        start_type,
        service_account,
    })
}

fn parse_service_state(report: &ServiceCommandReport) -> Result<WindowsObservedState, EvaError> {
    let output = String::from_utf8_lossy(report.stdout.bytes());
    let numeric_fields = output
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter_map(|(_, value)| value.trim().split_ascii_whitespace().next())
        .filter_map(|value| value.parse::<u32>().ok())
        .collect::<Vec<_>>();
    let state = numeric_fields.get(1).copied().ok_or_else(|| {
        EvaError::conflict("Windows service query output is missing numeric state")
    })?;
    match state {
        1 => Ok(WindowsObservedState::Stopped),
        2 => Ok(WindowsObservedState::StartPending),
        3 => Ok(WindowsObservedState::StopPending),
        4 => Ok(WindowsObservedState::Running),
        5 => Ok(WindowsObservedState::ContinuePending),
        6 => Ok(WindowsObservedState::PausePending),
        7 => Ok(WindowsObservedState::Paused),
        _ => Err(
            EvaError::conflict("Windows service query returned an unknown state")
                .with_context("scm_state", state.to_string()),
        ),
    }
}

fn sc_field<'a>(output: &'a str, field: &'static str) -> Result<&'a str, EvaError> {
    output
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| {
            name.trim()
                .eq_ignore_ascii_case(field)
                .then_some(value.trim())
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            EvaError::conflict("Windows service config output is missing a field")
                .with_context("field", field)
        })
}

fn sc_numeric_field(output: &str, field: &'static str) -> Result<u32, EvaError> {
    sc_field(output, field)?
        .split_ascii_whitespace()
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| {
            EvaError::conflict("Windows service config field is not numeric")
                .with_context("field", field)
        })
}

fn ensure_process_completion(
    report: &ServiceCommandReport,
    action: &'static str,
) -> Result<(), EvaError> {
    match report.termination {
        ServiceCommandTermination::Exited => Ok(()),
        ServiceCommandTermination::TimedOut => {
            Err(EvaError::timeout("Windows service command timed out")
                .with_context("action", action))
        }
        ServiceCommandTermination::OutputLimitExceeded => Err(EvaError::conflict(
            "Windows service command output exceeded its limit",
        )
        .with_context("action", action)),
    }
}

fn ensure_success_exit(
    report: &ServiceCommandReport,
    action: &'static str,
) -> Result<(), EvaError> {
    ensure_process_completion(report, action)?;
    if report.success {
        Ok(())
    } else {
        Err(command_exit_error(action, report))
    }
}

fn command_exit_error(action: &'static str, report: &ServiceCommandReport) -> EvaError {
    let exit_code = report.exit_code.unwrap_or(-1);
    let error = match exit_code {
        SC_ERROR_ACCESS_DENIED => {
            EvaError::permission_denied("Windows SCM denied the requested operation")
        }
        SC_ERROR_INVALID_PARAMETER => {
            EvaError::invalid_argument("Windows SCM rejected command parameters")
        }
        SC_ERROR_SERVICE_DOES_NOT_EXIST => EvaError::not_found("Windows service is not installed"),
        SC_ERROR_SERVICE_EXISTS => EvaError::conflict("Windows service already exists"),
        SC_ERROR_SERVICE_DISABLED => EvaError::conflict("Windows service is disabled"),
        SC_ERROR_SERVICE_MARKED_FOR_DELETE => {
            EvaError::conflict("Windows service is pending deletion")
        }
        _ => EvaError::unavailable("Windows SCM command failed"),
    };
    error
        .with_context("action", action)
        .with_context("exit_code", exit_code.to_string())
}

fn adapter_command_audit(report: &ServiceCommandReport, action: &'static str) -> Vec<String> {
    let mut audit = report.audit.clone();
    audit.push(format!("windows_service.command:{action}"));
    audit
}

fn quoted_windows_executable(path: &Path) -> OsString {
    let mut value = OsString::from("\"");
    value.push(path.as_os_str());
    value.push("\"");
    value
}

fn binary_path_matches(observed: &str, expected: &Path) -> bool {
    let expected = expected.to_string_lossy();
    let observed = observed.trim();
    observed.eq_ignore_ascii_case(&expected)
        || (observed.len() >= 2
            && observed.starts_with('"')
            && observed.ends_with('"')
            && observed[1..observed.len() - 1].eq_ignore_ascii_case(&expected))
}

const fn desired_start_type(start_on_boot: bool) -> u32 {
    if start_on_boot {
        SC_START_AUTO
    } else {
        SC_START_DEMAND
    }
}

const fn start_mode_arg(start_on_boot: bool) -> &'static str {
    if start_on_boot {
        "auto"
    } else {
        "demand"
    }
}

fn sleep_poll_interval(interval: Duration) {
    if interval.is_zero() {
        std::thread::yield_now();
    } else {
        std::thread::sleep(interval);
    }
}

#[cfg(windows)]
fn windows_sc_executable() -> Result<PathBuf, EvaError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0_u16; 260];
    loop {
        let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if length == 0 {
            return Err(
                EvaError::unavailable("failed to resolve the Windows system directory")
                    .with_context("io_error", std::io::Error::last_os_error().to_string()),
            );
        }
        if (length as usize) < buffer.len() {
            buffer.truncate(length as usize);
            let mut path = PathBuf::from(OsString::from_wide(&buffer));
            path.push("sc.exe");
            if !path.is_file() {
                return Err(EvaError::not_found("trusted Windows sc.exe was not found"));
            }
            return Ok(path);
        }
        buffer.resize(length as usize + 1, 0);
    }
}

#[cfg(not(windows))]
fn windows_sc_executable() -> Result<PathBuf, EvaError> {
    Err(EvaError::unsupported(
        "Windows sc.exe is unavailable on this host",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_command::{
        ServiceCommandArgVisibility, ServiceCommandExecution, ValidatedServiceCommandTarget,
    };
    use eva_core::ErrorKind;
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::sync::Arc;

    const TEST_SERVICE_NAME: &str = "EvaCliServiceTest";

    #[derive(Debug)]
    struct ExpectedArgument {
        value: OsString,
        visibility: ServiceCommandArgVisibility,
    }

    struct ScriptStep {
        arguments: Vec<ExpectedArgument>,
        execution: ServiceCommandExecution,
    }

    #[derive(Default)]
    struct ScriptState {
        remaining: VecDeque<ScriptStep>,
        command_debug: Vec<String>,
        call_count: usize,
    }

    #[derive(Clone)]
    struct ScriptedExecutor {
        state: Arc<Mutex<ScriptState>>,
    }

    impl ScriptedExecutor {
        fn new(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
            Self {
                state: Arc::new(Mutex::new(ScriptState {
                    remaining: steps.into_iter().collect(),
                    ..ScriptState::default()
                })),
            }
        }

        fn assert_drained(&self) {
            let state = self.state.lock().expect("script lock");
            assert_eq!(state.remaining.len(), 0, "unconsumed scripted commands");
        }

        fn call_count(&self) -> usize {
            self.state.lock().expect("script lock").call_count
        }

        fn command_debug(&self) -> String {
            self.state
                .lock()
                .expect("script lock")
                .command_debug
                .join("\n")
        }
    }

    impl ServiceCommandExecutor for ScriptedExecutor {
        fn execute(
            &mut self,
            target: &ValidatedServiceCommandTarget,
            command: &ServiceCommand,
        ) -> Result<ServiceCommandExecution, EvaError> {
            let mut state = self.state.lock().expect("script lock");
            state.call_count += 1;
            let step = state
                .remaining
                .pop_front()
                .expect("unexpected Windows service command");

            assert_eq!(target.kind(), ServiceManagerKind::WindowsService);
            assert_eq!(command.executable(), Path::new("manager-program"));
            assert_eq!(command.arguments().len(), step.arguments.len());
            for (actual, expected) in command.arguments().iter().zip(&step.arguments) {
                assert_eq!(actual.value(), expected.value.as_os_str());
                assert_eq!(actual.visibility(), expected.visibility);
            }
            state.command_debug.push(format!("{command:?}"));
            Ok(step.execution)
        }
    }

    fn public(value: impl Into<OsString>) -> ExpectedArgument {
        ExpectedArgument {
            value: value.into(),
            visibility: ServiceCommandArgVisibility::Public,
        }
    }

    fn secret(value: impl Into<OsString>) -> ExpectedArgument {
        ExpectedArgument {
            value: value.into(),
            visibility: ServiceCommandArgVisibility::Secret,
        }
    }

    fn step(
        arguments: Vec<ExpectedArgument>,
        exit_code: i32,
        stdout: impl Into<Vec<u8>>,
    ) -> ScriptStep {
        ScriptStep {
            arguments,
            execution: ServiceCommandExecution::exited(Some(exit_code), stdout, Vec::new()),
        }
    }

    fn qc_arguments() -> Vec<ExpectedArgument> {
        vec![public("qc"), public(TEST_SERVICE_NAME)]
    }

    fn query_arguments() -> Vec<ExpectedArgument> {
        vec![public("query"), public(TEST_SERVICE_NAME)]
    }

    fn query_step(state: u32, label: &str) -> ScriptStep {
        step(query_arguments(), 0, query_output(state, label))
    }

    fn missing_qc_step() -> ScriptStep {
        step(qc_arguments(), SC_ERROR_SERVICE_DOES_NOT_EXIST, Vec::new())
    }

    fn missing_query_step() -> ScriptStep {
        step(
            query_arguments(),
            SC_ERROR_SERVICE_DOES_NOT_EXIST,
            Vec::new(),
        )
    }

    fn query_output(state: u32, label: &str) -> Vec<u8> {
        format!(
            "SERVICE_NAME: {TEST_SERVICE_NAME}\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : {state}  {label}\r\n"
        )
        .into_bytes()
    }

    fn config_output(binary: &Path, start_type: u32, account: &str) -> Vec<u8> {
        format!(
            "SERVICE_NAME: {TEST_SERVICE_NAME}\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        START_TYPE         : {start_type}  TEST_LABEL\r\n        BINARY_PATH_NAME   : \"{}\"\r\n        SERVICE_START_NAME : {account}\r\n",
            binary.display()
        )
        .into_bytes()
    }

    fn test_definition() -> ServiceManagerDefinition {
        ServiceManagerDefinition {
            enabled: true,
            kind: ServiceManagerKind::WindowsService,
            service_name: TEST_SERVICE_NAME.to_owned(),
            unit_name: None,
            runtime_binary: Some(
                std::fs::canonicalize(std::env::current_exe().expect("current test executable"))
                    .expect("canonical test executable"),
            ),
            candidate_runtime_binary: None,
            start_on_boot: true,
            restart_supervisor: false,
        }
    }

    fn mutation_request(
        definition: &ServiceManagerDefinition,
    ) -> ServiceManagerMutationRequest<'_> {
        ServiceManagerMutationRequest { definition }
    }

    fn status_request(definition: &ServiceManagerDefinition) -> ServiceManagerStatusRequest<'_> {
        ServiceManagerStatusRequest { definition }
    }

    #[test]
    fn install_uses_exact_create_argv_and_is_idempotent() {
        let definition = test_definition();
        let binary = definition
            .runtime_binary
            .as_deref()
            .expect("runtime binary");
        let executor = ScriptedExecutor::new([
            missing_qc_step(),
            step(
                vec![
                    public("create"),
                    public(TEST_SERVICE_NAME),
                    public("binPath="),
                    secret(quoted_windows_executable(binary)),
                    public("start="),
                    public("auto"),
                    public("obj="),
                    public(WINDOWS_LOCAL_SERVICE_ACCOUNT),
                ],
                0,
                Vec::new(),
            ),
            query_step(1, "STOPPED"),
            step(
                qc_arguments(),
                0,
                config_output(binary, SC_START_AUTO, WINDOWS_LOCAL_SERVICE_ACCOUNT),
            ),
            query_step(1, "STOPPED"),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let installed = adapter
            .install(mutation_request(&definition))
            .expect("first install");
        assert_eq!(installed.state, ServiceManagerState::Stopped);
        assert!(installed.mutation_executed);
        assert!(installed.production_adapter);
        let binary_text = binary.to_string_lossy();
        assert!(!installed.audit.join("\n").contains(binary_text.as_ref()));

        let repeated = adapter
            .install(mutation_request(&definition))
            .expect("repeated install");
        assert_eq!(repeated.state, ServiceManagerState::Stopped);
        assert!(!repeated.mutation_executed);
        assert!(!probe.command_debug().contains(binary_text.as_ref()));
        probe.assert_drained();
    }

    #[test]
    fn install_converges_start_mode_and_rechecks_config() {
        let definition = test_definition();
        let binary = definition
            .runtime_binary
            .as_deref()
            .expect("runtime binary");
        let executor = ScriptedExecutor::new([
            step(
                qc_arguments(),
                0,
                config_output(binary, SC_START_DEMAND, WINDOWS_LOCAL_SERVICE_ACCOUNT),
            ),
            step(
                vec![
                    public("config"),
                    public(TEST_SERVICE_NAME),
                    public("start="),
                    public("auto"),
                ],
                0,
                Vec::new(),
            ),
            step(
                qc_arguments(),
                0,
                config_output(binary, SC_START_AUTO, WINDOWS_LOCAL_SERVICE_ACCOUNT),
            ),
            query_step(1, "STOPPED"),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .install(mutation_request(&definition))
            .expect("start mode convergence");

        assert!(report.mutation_executed);
        assert_eq!(report.state, ServiceManagerState::Stopped);
        probe.assert_drained();
    }

    #[test]
    fn install_rejects_binary_and_account_drift() {
        let definition = test_definition();
        let binary = definition
            .runtime_binary
            .as_deref()
            .expect("runtime binary");

        let binary_executor = ScriptedExecutor::new([step(
            qc_arguments(),
            0,
            config_output(
                Path::new(r"C:\different\eva.exe"),
                SC_START_AUTO,
                WINDOWS_LOCAL_SERVICE_ACCOUNT,
            ),
        )]);
        let binary_probe = binary_executor.clone();
        let mut binary_adapter = WindowsServiceAdapter::for_test(binary_executor);
        let binary_error = binary_adapter
            .install(mutation_request(&definition))
            .expect_err("binary drift must fail closed");
        assert_eq!(binary_error.kind(), ErrorKind::Conflict);
        binary_probe.assert_drained();

        let account_executor = ScriptedExecutor::new([step(
            qc_arguments(),
            0,
            config_output(binary, SC_START_AUTO, r"LocalSystem"),
        )]);
        let account_probe = account_executor.clone();
        let mut account_adapter = WindowsServiceAdapter::for_test(account_executor);
        let account_error = account_adapter
            .install(mutation_request(&definition))
            .expect_err("account drift must fail closed");
        assert_eq!(account_error.kind(), ErrorKind::Conflict);
        account_probe.assert_drained();
    }

    #[test]
    fn install_rejects_a_service_that_disappears_after_create() {
        let definition = test_definition();
        let binary = definition
            .runtime_binary
            .as_deref()
            .expect("runtime binary");
        let executor = ScriptedExecutor::new([
            missing_qc_step(),
            step(
                vec![
                    public("create"),
                    public(TEST_SERVICE_NAME),
                    public("binPath="),
                    secret(quoted_windows_executable(binary)),
                    public("start="),
                    public("auto"),
                    public("obj="),
                    public(WINDOWS_LOCAL_SERVICE_ACCOUNT),
                ],
                0,
                Vec::new(),
            ),
            missing_query_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let error = adapter
            .install(mutation_request(&definition))
            .expect_err("disappearing service must not report installed");

        assert_eq!(error.kind(), ErrorKind::Conflict);
        probe.assert_drained();
    }

    #[test]
    fn start_is_idempotent_when_already_running() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([query_step(4, "RUNNING")]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .start(mutation_request(&definition))
            .expect("idempotent start");

        assert_eq!(report.state, ServiceManagerState::Running);
        assert!(!report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn restart_stops_then_starts_a_running_service() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([
            query_step(4, "RUNNING"),
            step(
                vec![public("stop"), public(TEST_SERVICE_NAME)],
                0,
                Vec::new(),
            ),
            query_step(1, "STOPPED"),
            step(
                vec![public("start"), public(TEST_SERVICE_NAME)],
                0,
                Vec::new(),
            ),
            query_step(4, "RUNNING"),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .restart(mutation_request(&definition))
            .expect("restart running service");

        assert_eq!(report.state, ServiceManagerState::Running);
        assert!(report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn uninstall_stops_running_service_then_deletes_it() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([
            query_step(4, "RUNNING"),
            step(
                vec![public("stop"), public(TEST_SERVICE_NAME)],
                0,
                Vec::new(),
            ),
            query_step(1, "STOPPED"),
            step(
                vec![public("delete"), public(TEST_SERVICE_NAME)],
                0,
                Vec::new(),
            ),
            missing_query_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .uninstall(mutation_request(&definition))
            .expect("uninstall running service");

        assert_eq!(report.state, ServiceManagerState::NotInstalled);
        assert!(report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn uninstall_stops_a_paused_service_before_deleting_it() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([
            query_step(7, "PAUSED"),
            step(
                vec![public("stop"), public(TEST_SERVICE_NAME)],
                0,
                Vec::new(),
            ),
            query_step(1, "STOPPED"),
            step(
                vec![public("delete"), public(TEST_SERVICE_NAME)],
                0,
                Vec::new(),
            ),
            missing_query_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .uninstall(mutation_request(&definition))
            .expect("uninstall paused service");

        assert_eq!(report.state, ServiceManagerState::NotInstalled);
        assert!(report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn pending_delete_waits_for_missing_without_deleting_again() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([
            step(
                query_arguments(),
                SC_ERROR_SERVICE_MARKED_FOR_DELETE,
                Vec::new(),
            ),
            missing_query_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .uninstall(mutation_request(&definition))
            .expect("pending delete convergence");

        assert_eq!(report.state, ServiceManagerState::NotInstalled);
        assert!(!report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn missing_stop_and_uninstall_are_noops_but_restart_is_not_found() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([
            missing_query_step(),
            missing_query_step(),
            missing_query_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let stopped = adapter
            .stop(mutation_request(&definition))
            .expect("missing stop is idempotent");
        assert_eq!(stopped.state, ServiceManagerState::NotInstalled);
        assert!(!stopped.mutation_executed);

        let uninstalled = adapter
            .uninstall(mutation_request(&definition))
            .expect("missing uninstall is idempotent");
        assert_eq!(uninstalled.state, ServiceManagerState::NotInstalled);
        assert!(!uninstalled.mutation_executed);

        let restart_error = adapter
            .restart(mutation_request(&definition))
            .expect_err("missing restart must fail");
        assert_eq!(restart_error.kind(), ErrorKind::NotFound);
        probe.assert_drained();
    }

    #[test]
    fn numeric_state_parsing_does_not_depend_on_the_label() {
        let definition = test_definition();
        let executor = ScriptedExecutor::new([query_step(4, "NOT_THE_ENGLISH_STATE_NAME")]);
        let probe = executor.clone();
        let adapter = WindowsServiceAdapter::for_test(executor);

        let report = adapter
            .status(status_request(&definition))
            .expect("numeric state");

        assert_eq!(report.state, ServiceManagerState::Running);
        assert!(report.production_adapter);
        probe.assert_drained();
    }

    #[test]
    fn scm_access_denied_maps_to_permission_denied() {
        let definition = test_definition();
        let executor =
            ScriptedExecutor::new([step(query_arguments(), SC_ERROR_ACCESS_DENIED, Vec::new())]);
        let probe = executor.clone();
        let adapter = WindowsServiceAdapter::for_test(executor);

        let error = adapter
            .status(status_request(&definition))
            .expect_err("SCM access denial");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        probe.assert_drained();
    }

    #[test]
    fn invalid_kind_and_service_name_fail_before_executor_call() {
        let mut wrong_kind = test_definition();
        wrong_kind.kind = ServiceManagerKind::Systemd;
        let mut invalid_name = test_definition();
        invalid_name.service_name = "bad/service".to_owned();
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let kind_error = adapter
            .install(mutation_request(&wrong_kind))
            .expect_err("wrong kind");
        assert_eq!(kind_error.kind(), ErrorKind::Unsupported);
        let name_error = adapter
            .install(mutation_request(&invalid_name))
            .expect_err("invalid service name");
        assert_eq!(name_error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(probe.call_count(), 0);
    }

    #[test]
    fn secret_binary_path_does_not_enter_debug_or_error_output() {
        let definition = test_definition();
        let binary = definition
            .runtime_binary
            .as_deref()
            .expect("runtime binary");
        let executor = ScriptedExecutor::new([
            missing_qc_step(),
            step(
                vec![
                    public("create"),
                    public(TEST_SERVICE_NAME),
                    public("binPath="),
                    secret(quoted_windows_executable(binary)),
                    public("start="),
                    public("auto"),
                    public("obj="),
                    public(WINDOWS_LOCAL_SERVICE_ACCOUNT),
                ],
                SC_ERROR_ACCESS_DENIED,
                Vec::new(),
            ),
        ]);
        let probe = executor.clone();
        let mut adapter = WindowsServiceAdapter::for_test(executor);

        let error = adapter
            .install(mutation_request(&definition))
            .expect_err("create should be denied");
        let binary_text = binary.to_string_lossy();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(!format!("{error:?}").contains(binary_text.as_ref()));
        assert!(!probe.command_debug().contains(binary_text.as_ref()));
        probe.assert_drained();
    }

    #[test]
    fn quoted_binary_comparison_handles_degenerate_quotes() {
        let expected = Path::new(r"C:\eva\eva.exe");
        assert!(!binary_path_matches("\"", expected));
        assert!(!binary_path_matches("", expected));
        assert!(binary_path_matches(r#""C:\eva\eva.exe""#, expected));
    }

    #[cfg(windows)]
    #[test]
    fn native_read_only_query_reports_event_log_running() {
        let adapter = WindowsServiceAdapter::native().expect("native Windows adapter");
        let definition = ServiceManagerDefinition {
            enabled: true,
            kind: ServiceManagerKind::WindowsService,
            service_name: "EventLog".to_owned(),
            unit_name: None,
            runtime_binary: None,
            candidate_runtime_binary: None,
            start_on_boot: true,
            restart_supervisor: false,
        };

        let report = adapter
            .status(status_request(&definition))
            .expect("query EventLog");

        assert_eq!(report.state, ServiceManagerState::Running);
        assert!(report.production_adapter);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "mutates the Windows SCM using a unique service name"]
    fn native_unique_service_create_delete_or_permission_probe() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let definition = ServiceManagerDefinition {
            enabled: true,
            kind: ServiceManagerKind::WindowsService,
            service_name: format!("EvaCliProbe{}{}", std::process::id(), nonce),
            unit_name: None,
            runtime_binary: Some(std::env::current_exe().expect("current test executable")),
            candidate_runtime_binary: None,
            start_on_boot: false,
            restart_supervisor: false,
        };
        let mut adapter = WindowsServiceAdapter::native().expect("native Windows adapter");

        let install = adapter.install(mutation_request(&definition));
        let cleanup = adapter.uninstall(mutation_request(&definition));

        match install {
            Ok(report) => {
                assert!(report.mutation_executed);
                assert_eq!(report.state, ServiceManagerState::Stopped);
            }
            Err(error) => assert_eq!(error.kind(), ErrorKind::PermissionDenied),
        }
        let cleanup = cleanup.expect("unique service cleanup");
        assert_eq!(cleanup.state, ServiceManagerState::NotInstalled);
    }
}
