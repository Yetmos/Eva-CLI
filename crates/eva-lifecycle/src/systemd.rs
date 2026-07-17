//! Linux systemd adapter backed by structured `systemctl` argv and atomic unit publication.

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
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_SYSTEMD_POLL_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_SYSTEMD_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SYSTEMD_UNIT_ROOT: &str = "/etc/systemd/system";
const SYSTEMD_UNIT_MAX_BYTES: u64 = 64 * 1024;
const MANAGED_UNIT_MARKER_PREFIX: &str = "# eva-cli-managed-unit=v1; unit=";

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Production systemd adapter for the system service domain.
pub struct SystemdAdapter<E> {
    factory: Mutex<ServiceManagerFactory<E>>,
    systemctl_executable: PathBuf,
    unit_root: PathBuf,
    poll_timeout: Duration,
    poll_interval: Duration,
    enforce_native_permissions: bool,
}

impl<E> SystemdAdapter<E>
where
    E: ServiceCommandExecutor,
{
    /// Creates the native adapter only on Linux and resolves `systemctl` from
    /// fixed system locations rather than mutable PATH input.
    pub fn new(executor: E) -> Result<Self, EvaError> {
        if ServiceHostPlatform::current() != ServiceHostPlatform::Linux {
            return Err(
                EvaError::unsupported("systemd adapter requires a Linux host")
                    .with_context("host_platform", ServiceHostPlatform::current().as_str()),
            );
        }
        Ok(Self::with_paths(
            executor,
            systemctl_executable()?,
            PathBuf::from(SYSTEMD_UNIT_ROOT),
            true,
        ))
    }

    fn with_paths(
        executor: E,
        systemctl_executable: PathBuf,
        unit_root: PathBuf,
        enforce_native_permissions: bool,
    ) -> Self {
        Self {
            factory: Mutex::new(ServiceManagerFactory::new(executor)),
            systemctl_executable,
            unit_root,
            poll_timeout: DEFAULT_SYSTEMD_POLL_TIMEOUT,
            poll_interval: DEFAULT_SYSTEMD_POLL_INTERVAL,
            enforce_native_permissions,
        }
    }

    #[cfg(test)]
    fn for_test(executor: E, unit_root: PathBuf) -> Self {
        Self {
            factory: Mutex::new(ServiceManagerFactory::for_host(
                ServiceHostPlatform::Linux,
                executor,
            )),
            systemctl_executable: PathBuf::from("systemctl"),
            unit_root,
            poll_timeout: Duration::from_secs(1),
            poll_interval: Duration::ZERO,
            enforce_native_permissions: false,
        }
    }

    fn execute(&self, arguments: Vec<ServiceCommandArg>) -> Result<ServiceCommandReport, EvaError> {
        let command = ServiceCommand::new(self.systemctl_executable.clone(), arguments)?;
        let mut factory = self
            .factory
            .lock()
            .map_err(|_| EvaError::internal("systemd command factory lock is poisoned"))?;
        factory.execute_command(ServiceManagerKind::Systemd, &command)
    }

    fn validate_definition(
        definition: &ServiceManagerDefinition,
        mutation: bool,
    ) -> Result<String, EvaError> {
        if definition.kind != ServiceManagerKind::Systemd {
            return Err(
                EvaError::unsupported("systemd adapter requires systemd kind")
                    .with_context("requested_kind", definition.kind.as_str()),
            );
        }
        if mutation && !definition.enabled {
            return Err(EvaError::invalid_argument(
                "systemd service mutation requires an enabled definition",
            ));
        }
        validate_service_name(&definition.service_name)?;
        resolve_unit_name(definition)
    }

    fn unit_path(&self, unit_name: &str) -> PathBuf {
        self.unit_root.join(unit_name)
    }

    fn install_binary(definition: &ServiceManagerDefinition) -> Result<PathBuf, EvaError> {
        let binary = definition
            .runtime_binary
            .as_ref()
            .ok_or_else(|| EvaError::invalid_argument("systemd install requires runtime_binary"))?;
        if !binary.is_absolute() {
            return Err(EvaError::invalid_argument(
                "systemd runtime_binary must be absolute",
            ));
        }
        let metadata = fs::metadata(binary).map_err(|error| {
            io_error(
                "systemd runtime_binary does not exist",
                "runtime_binary",
                error,
            )
        })?;
        if !metadata.is_file() {
            return Err(EvaError::invalid_argument(
                "systemd runtime_binary must be a regular file",
            ));
        }
        fs::canonicalize(binary).map_err(|error| {
            io_error(
                "failed to canonicalize systemd runtime_binary",
                "runtime_binary",
                error,
            )
        })
    }

    fn desired_unit(
        _definition: &ServiceManagerDefinition,
        _unit_name: &str,
        binary: &Path,
    ) -> Result<Vec<u8>, EvaError> {
        let binary = systemd_exec_path(binary)?;
        let marker = managed_marker(_unit_name);
        let content = format!(
            "{marker}[Unit]\nDescription=Eva CLI Runtime Service\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={binary}\nRestart=on-failure\nRestartSec=5s\nKillMode=control-group\n\n[Install]\nWantedBy=multi-user.target\n",
            binary = binary,
        );
        Ok(content.into_bytes())
    }

    fn read_unit(&self, unit_name: &str) -> Result<Option<Vec<u8>>, EvaError> {
        validate_unit_root(&self.unit_root)?;
        let path = self.unit_path(unit_name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("failed to inspect systemd unit", "unit", error)),
        };
        if !metadata.file_type().is_file() {
            return Err(EvaError::conflict(
                "systemd unit path is not a regular file",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| io_error("failed to read systemd unit", "unit", error))?;
        let opened_metadata = file
            .metadata()
            .map_err(|error| io_error("failed to inspect opened systemd unit", "unit", error))?;
        if !opened_metadata.is_file() {
            return Err(EvaError::conflict(
                "opened systemd unit is not a regular file",
            ));
        }
        if self.enforce_native_permissions {
            validate_native_unit_permissions(&opened_metadata)?;
        }
        if opened_metadata.len() > SYSTEMD_UNIT_MAX_BYTES {
            return Err(EvaError::conflict(
                "systemd unit file exceeds the bounded size",
            ));
        }
        let mut bytes = Vec::with_capacity((SYSTEMD_UNIT_MAX_BYTES as usize).saturating_add(1));
        let mut limited = (&mut file).take(SYSTEMD_UNIT_MAX_BYTES.saturating_add(1));
        std::io::Read::read_to_end(&mut limited, &mut bytes)
            .map_err(|error| io_error("failed to read systemd unit", "unit", error))?;
        if bytes.len() as u64 > SYSTEMD_UNIT_MAX_BYTES {
            return Err(EvaError::conflict(
                "systemd unit file exceeds the bounded size",
            ));
        }
        Ok(Some(bytes))
    }

    fn ensure_managed_unit(
        &self,
        definition: &ServiceManagerDefinition,
        unit_name: &str,
    ) -> Result<Vec<u8>, EvaError> {
        let bytes = self
            .read_unit(unit_name)?
            .ok_or_else(|| EvaError::not_found("systemd unit is not installed"))?;
        let marker = managed_marker(unit_name);
        if !bytes.starts_with(marker.as_bytes()) {
            return Err(EvaError::conflict(
                "systemd unit is not managed by this service definition",
            ));
        }
        if definition.runtime_binary.is_some() {
            let canonical_binary = Self::install_binary(definition)?;
            let desired = Self::desired_unit(definition, unit_name, &canonical_binary)?;
            if bytes != desired {
                return Err(EvaError::conflict(
                    "managed systemd unit content does not match definition",
                ));
            }
        }
        Ok(bytes)
    }

    fn ensure_desired_unit(
        &self,
        _definition: &ServiceManagerDefinition,
        unit_name: &str,
        desired: &[u8],
    ) -> Result<bool, EvaError> {
        match self.read_unit(unit_name)? {
            None => {
                atomic_create_unit(&self.unit_root, unit_name, desired)?;
                Ok(true)
            }
            Some(existing) if existing == desired => Ok(false),
            Some(_) => Err(EvaError::conflict(
                "existing systemd unit content does not match definition",
            )),
        }
    }

    fn query_snapshot(&self, unit_name: &str) -> Result<(SystemdSnapshot, Vec<String>), EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("--system"),
            ServiceCommandArg::public("--no-ask-password"),
            ServiceCommandArg::public("--no-pager"),
            ServiceCommandArg::public("show"),
            ServiceCommandArg::public("--property=LoadState"),
            ServiceCommandArg::public("--property=ActiveState"),
            ServiceCommandArg::public("--property=SubState"),
            ServiceCommandArg::public("--property=UnitFileState"),
            ServiceCommandArg::public(unit_name),
        ])?;
        let mut audit = adapter_command_audit(&report, "query_snapshot");
        ensure_process_completion(&report, "query_snapshot")?;
        if !report.success && report.stdout.bytes().is_empty() {
            return Err(command_exit_error("query_snapshot", &report));
        }
        let snapshot = parse_systemd_show(&report)?;
        if !report.success && snapshot.load_state != SystemdLoadState::NotFound {
            return Err(command_exit_error("query_snapshot", &report));
        }
        audit.extend([
            format!("systemd.load_state:{}", snapshot.load_state.as_str()),
            format!("systemd.active_state:{}", snapshot.active_state.as_str()),
            format!("systemd.unit_file_state:{}", snapshot.enablement.as_str()),
        ]);
        Ok((snapshot, audit))
    }

    fn wait_for_state(
        &self,
        _definition: &ServiceManagerDefinition,
        unit_name: &str,
        expected: ServiceManagerState,
    ) -> Result<(ServiceManagerState, Vec<String>), EvaError> {
        let started_at = Instant::now();
        let mut audit = Vec::new();
        loop {
            let (snapshot, current_audit) = self.query_snapshot(unit_name)?;
            audit.extend(current_audit);
            if snapshot.active_state.service_state() == Some(expected) {
                return Ok((expected, audit));
            }
            if started_at.elapsed() >= self.poll_timeout {
                return Err(
                    EvaError::timeout("systemd service state transition timed out")
                        .with_context("expected_state", expected.as_str()),
                );
            }
            sleep_poll_interval(self.poll_interval);
        }
    }

    fn wait_for_stable_state(
        &self,
        _definition: &ServiceManagerDefinition,
        unit_name: &str,
    ) -> Result<(ServiceManagerState, Vec<String>), EvaError> {
        let started_at = Instant::now();
        let mut audit = Vec::new();
        loop {
            let (snapshot, current_audit) = self.query_snapshot(unit_name)?;
            audit.extend(current_audit);
            if let Some(state) = snapshot.active_state.service_state() {
                return Ok((state, audit));
            }
            if started_at.elapsed() >= self.poll_timeout {
                return Err(EvaError::timeout(
                    "systemd service did not reach a stable state",
                ));
            }
            sleep_poll_interval(self.poll_interval);
        }
    }

    fn run_systemctl(
        &self,
        arguments: Vec<ServiceCommandArg>,
        action: &'static str,
    ) -> Result<Vec<String>, EvaError> {
        let mut command_arguments = vec![
            ServiceCommandArg::public("--system"),
            ServiceCommandArg::public("--no-ask-password"),
        ];
        command_arguments.extend(arguments);
        let report = self.execute(command_arguments)?;
        let audit = adapter_command_audit(&report, action);
        ensure_success_exit(&report, action)?;
        Ok(audit)
    }

    fn run_daemon_reload(&self) -> Result<Vec<String>, EvaError> {
        self.run_systemctl(
            vec![ServiceCommandArg::public("daemon-reload")],
            "daemon_reload",
        )
    }

    fn run_enable(&self, unit_name: &str) -> Result<Vec<String>, EvaError> {
        self.run_systemctl(
            vec![
                ServiceCommandArg::public("enable"),
                ServiceCommandArg::public(unit_name),
            ],
            "enable",
        )
    }

    fn run_disable(&self, unit_name: &str) -> Result<Vec<String>, EvaError> {
        self.run_systemctl(
            vec![
                ServiceCommandArg::public("disable"),
                ServiceCommandArg::public(unit_name),
            ],
            "disable",
        )
    }

    fn run_start(&self, unit_name: &str) -> Result<Vec<String>, EvaError> {
        self.run_systemctl(
            vec![
                ServiceCommandArg::public("start"),
                ServiceCommandArg::public(unit_name),
            ],
            "start",
        )
    }

    fn run_stop(&self, unit_name: &str) -> Result<Vec<String>, EvaError> {
        self.run_systemctl(
            vec![
                ServiceCommandArg::public("stop"),
                ServiceCommandArg::public(unit_name),
            ],
            "stop",
        )
    }

    fn run_restart(&self, unit_name: &str) -> Result<Vec<String>, EvaError> {
        self.run_systemctl(
            vec![
                ServiceCommandArg::public("restart"),
                ServiceCommandArg::public(unit_name),
            ],
            "restart",
        )
    }

    fn status_report(
        definition: &ServiceManagerDefinition,
        state: ServiceManagerState,
        mut audit: Vec<String>,
    ) -> ServiceManagerStatusReport {
        audit.push(format!("systemd.state:{}", state.as_str()));
        ServiceManagerStatusReport {
            kind: ServiceManagerKind::Systemd,
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
            format!("systemd.operation:{}", operation.as_str()),
            format!("service_manager.mutation_executed:{mutation_executed}"),
            format!("service_manager.state:{}", state.as_str()),
        ]);
        ServiceManagerMutationReport {
            kind: ServiceManagerKind::Systemd,
            service_name: definition.service_name.clone(),
            operation,
            state,
            mutation_executed,
            production_adapter: true,
            audit,
        }
    }
}

impl SystemdAdapter<ProcessServiceCommandExecutor> {
    /// Creates the native systemd adapter with default command limits.
    pub fn native() -> Result<Self, EvaError> {
        Self::new(ProcessServiceCommandExecutor::default())
    }
}

impl<E> ServiceManagerAdapter for SystemdAdapter<E>
where
    E: ServiceCommandExecutor,
{
    fn kind(&self) -> ServiceManagerKind {
        ServiceManagerKind::Systemd
    }

    fn install(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        let unit_name = Self::validate_definition(definition, true)?;
        let binary = Self::install_binary(definition)?;
        let desired = Self::desired_unit(definition, &unit_name, &binary)?;
        let digest = digest_bytes(&desired);
        let mut audit = vec![format!("systemd.unit.name:{unit_name}")];
        if self.read_unit(&unit_name)?.is_none() {
            let (snapshot, preflight_audit) = self.query_snapshot(&unit_name)?;
            audit.extend(preflight_audit);
            if snapshot.load_state != SystemdLoadState::NotFound {
                return Err(EvaError::conflict(
                    "systemd unit name is already provided by another source",
                ));
            }
        }
        let unit_changed = self.ensure_desired_unit(definition, &unit_name, &desired)?;
        audit.push(format!("systemd.unit.changed:{unit_changed}"));
        audit.push(format!("systemd.unit.digest:{digest}"));
        let mut mutation_executed = unit_changed;
        if unit_changed {
            audit.extend(self.run_daemon_reload()?);
        }

        let (mut snapshot, snapshot_audit) = self.query_snapshot(&unit_name)?;
        audit.extend(snapshot_audit);
        if snapshot.load_state == SystemdLoadState::NotFound {
            return Err(EvaError::conflict(
                "systemd unit was not loaded after publication",
            ));
        }
        match (definition.start_on_boot, snapshot.enablement) {
            (true, SystemdEnablement::Enabled) | (false, SystemdEnablement::Disabled) => {}
            (true, SystemdEnablement::EnabledRuntime | SystemdEnablement::Disabled) => {
                audit.extend(self.run_enable(&unit_name)?);
                mutation_executed = true;
            }
            (false, SystemdEnablement::Enabled | SystemdEnablement::EnabledRuntime) => {
                audit.extend(self.run_disable(&unit_name)?);
                mutation_executed = true;
            }
            (_, SystemdEnablement::Masked | SystemdEnablement::Other) => {
                return Err(EvaError::conflict(
                    "systemd unit file state is not safely managed",
                ));
            }
            (_, SystemdEnablement::NotInstalled) => {
                return Err(EvaError::conflict(
                    "systemd unit was not visible after publication",
                ));
            }
        }
        if mutation_executed && snapshot.enablement != SystemdEnablement::NotInstalled {
            let (verified, verify_audit) = self.query_snapshot(&unit_name)?;
            audit.extend(verify_audit);
            snapshot = verified;
            let enablement_ok = if definition.start_on_boot {
                snapshot.enablement == SystemdEnablement::Enabled
            } else {
                snapshot.enablement == SystemdEnablement::Disabled
            };
            if snapshot.load_state != SystemdLoadState::Loaded || !enablement_ok {
                return Err(EvaError::conflict(
                    "systemd unit enablement did not converge",
                ));
            }
        }

        let (state, state_audit) = self.wait_for_stable_state(definition, &unit_name)?;
        audit.extend(state_audit);
        if state == ServiceManagerState::NotInstalled {
            return Err(EvaError::conflict(
                "systemd unit was not active after install",
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
        let unit_name = Self::validate_definition(definition, true)?;
        let mut audit = vec![format!("systemd.unit.name:{unit_name}")];
        let local_unit = self.read_unit(&unit_name)?;
        if let Some(unit) = local_unit.as_ref() {
            if !unit.starts_with(managed_marker(&unit_name).as_bytes()) {
                return Err(EvaError::conflict(
                    "systemd unit is not managed by this service definition",
                ));
            }
            audit.push(format!("systemd.unit.digest:{}", digest_bytes(unit)));
        }
        let (snapshot, state_audit) = self.query_snapshot(&unit_name)?;
        audit.extend(state_audit);
        if local_unit.is_none() && snapshot.load_state == SystemdLoadState::NotFound {
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Uninstall,
                ServiceManagerState::NotInstalled,
                false,
                audit,
            ));
        }
        if local_unit.is_none() {
            return Err(EvaError::conflict(
                "systemd unit is provided by a foreign source",
            ));
        }
        if snapshot.load_state == SystemdLoadState::NotFound {
            return Err(EvaError::unavailable("managed systemd unit is not loaded"));
        }
        let (state, stable_audit) = self.wait_for_stable_state(definition, &unit_name)?;
        audit.extend(stable_audit);
        if state == ServiceManagerState::Running {
            audit.extend(self.run_stop(&unit_name)?);
            let (_, stop_audit) =
                self.wait_for_state(definition, &unit_name, ServiceManagerState::Stopped)?;
            audit.extend(stop_audit);
        }
        if matches!(
            snapshot.enablement,
            SystemdEnablement::Masked | SystemdEnablement::Other
        ) {
            return Err(EvaError::conflict(
                "systemd unit file state is not safely managed",
            ));
        }
        if matches!(
            snapshot.enablement,
            SystemdEnablement::Enabled | SystemdEnablement::EnabledRuntime
        ) {
            audit.extend(self.run_disable(&unit_name)?);
        }
        let tombstone = move_unit_to_tombstone(&self.unit_root, &unit_name)?;
        let reload = self.run_daemon_reload();
        if let Err(error) = reload {
            restore_tombstone(&self.unit_root, &tombstone, &unit_name);
            return Err(error);
        }
        let after_result = self.query_snapshot(&unit_name);
        let (after, verify_audit) = match after_result {
            Ok(value) => value,
            Err(error) => {
                restore_tombstone(&self.unit_root, &tombstone, &unit_name);
                let _ = self.run_daemon_reload();
                return Err(error);
            }
        };
        audit.extend(verify_audit);
        if after.load_state != SystemdLoadState::NotFound {
            restore_tombstone(&self.unit_root, &tombstone, &unit_name);
            let _ = self.run_daemon_reload();
            return Err(EvaError::conflict(
                "systemd unit remained loaded after uninstall",
            ));
        }
        fs::remove_file(&tombstone)
            .map_err(|error| io_error("failed to remove systemd unit tombstone", "unit", error))?;
        sync_directory(&self.unit_root)?;
        audit.push("systemd.unit.removed:true".to_owned());
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
        let unit_name = Self::validate_definition(definition, false)?;
        let mut audit = vec![format!("systemd.unit.name:{unit_name}")];
        let local_unit = self.read_unit(&unit_name)?;
        if let Some(unit) = local_unit.as_ref() {
            if !unit.starts_with(managed_marker(&unit_name).as_bytes()) {
                return Err(EvaError::conflict(
                    "systemd unit is not managed by this service definition",
                ));
            }
            audit.push(format!("systemd.unit.digest:{}", digest_bytes(unit)));
        }
        let (snapshot, state_audit) = self.query_snapshot(&unit_name)?;
        audit.extend(state_audit);
        if local_unit.is_none() && snapshot.load_state == SystemdLoadState::NotFound {
            audit.push("systemd.unit.present:false".to_owned());
            return Ok(Self::status_report(
                definition,
                ServiceManagerState::NotInstalled,
                audit,
            ));
        }
        if local_unit.is_none() {
            return Err(EvaError::conflict(
                "systemd unit is provided by a foreign source",
            ));
        }
        if snapshot.load_state == SystemdLoadState::NotFound {
            return Err(EvaError::unavailable("managed systemd unit is not loaded"));
        }
        if matches!(
            snapshot.enablement,
            SystemdEnablement::Masked | SystemdEnablement::Other
        ) {
            return Err(EvaError::conflict(
                "systemd unit file state is not safely managed",
            ));
        }
        let state = snapshot.active_state.service_state().ok_or_else(|| {
            EvaError::unavailable("systemd service is in a transitional state")
                .with_context("systemd_state", snapshot.active_state.as_str())
        })?;
        Ok(Self::status_report(definition, state, audit))
    }

    fn start(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        let unit_name = Self::validate_definition(definition, true)?;
        let mut audit = vec![format!("systemd.unit.name:{unit_name}")];
        let unit = self
            .ensure_managed_unit(definition, &unit_name)
            .map_err(|error| {
                if error.kind() == eva_core::ErrorKind::NotFound {
                    EvaError::not_found("systemd service is not installed")
                } else {
                    error
                }
            })?;
        audit.push(format!("systemd.unit.digest:{}", digest_bytes(&unit)));
        let (state, state_audit) = self.wait_for_stable_state(definition, &unit_name)?;
        audit.extend(state_audit);
        if state == ServiceManagerState::NotInstalled {
            return Err(EvaError::not_found("systemd service is not installed"));
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
        audit.extend(self.run_start(&unit_name)?);
        let (_, final_audit) =
            self.wait_for_state(definition, &unit_name, ServiceManagerState::Running)?;
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
        let unit_name = Self::validate_definition(definition, true)?;
        let mut audit = vec![format!("systemd.unit.name:{unit_name}")];
        let local_unit = self.read_unit(&unit_name)?;
        if local_unit.is_none() {
            let (snapshot, state_audit) = self.query_snapshot(&unit_name)?;
            audit.extend(state_audit);
            if snapshot.load_state != SystemdLoadState::NotFound {
                return Err(EvaError::conflict(
                    "systemd unit is provided by a foreign source",
                ));
            }
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Stop,
                ServiceManagerState::NotInstalled,
                false,
                audit,
            ));
        }
        let unit = self.ensure_managed_unit(definition, &unit_name)?;
        audit.push(format!("systemd.unit.digest:{}", digest_bytes(&unit)));
        let (state, state_audit) = self.wait_for_stable_state(definition, &unit_name)?;
        audit.extend(state_audit);
        if state != ServiceManagerState::Running {
            return Ok(Self::mutation_report(
                definition,
                ServiceManagerOperation::Stop,
                state,
                false,
                audit,
            ));
        }
        audit.extend(self.run_stop(&unit_name)?);
        let (_, final_audit) =
            self.wait_for_state(definition, &unit_name, ServiceManagerState::Stopped)?;
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
        let unit_name = Self::validate_definition(definition, true)?;
        let mut audit = vec![format!("systemd.unit.name:{unit_name}")];
        let unit = self
            .ensure_managed_unit(definition, &unit_name)
            .map_err(|error| {
                if error.kind() == eva_core::ErrorKind::NotFound {
                    EvaError::not_found("systemd service is not installed")
                } else {
                    error
                }
            })?;
        audit.push(format!("systemd.unit.digest:{}", digest_bytes(&unit)));
        let (state, state_audit) = self.wait_for_stable_state(definition, &unit_name)?;
        audit.extend(state_audit);
        if state == ServiceManagerState::NotInstalled {
            return Err(EvaError::not_found("systemd service is not installed"));
        }
        audit.extend(self.run_restart(&unit_name)?);
        let (_, final_audit) =
            self.wait_for_state(definition, &unit_name, ServiceManagerState::Running)?;
        audit.extend(final_audit);
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
            "systemd generation handoff is not implemented yet",
        ))
    }

    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError> {
        Self::validate_definition(request.definition, true)?;
        Err(EvaError::unsupported(
            "systemd generation rollback is not implemented yet",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdObservedState {
    NotInstalled,
    Inactive,
    Active,
    Failed,
    Activating,
    Deactivating,
}

impl SystemdObservedState {
    const fn service_state(self) -> Option<ServiceManagerState> {
        match self {
            Self::NotInstalled => Some(ServiceManagerState::NotInstalled),
            Self::Inactive | Self::Failed => Some(ServiceManagerState::Stopped),
            Self::Active => Some(ServiceManagerState::Running),
            Self::Activating | Self::Deactivating => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Activating => "activating",
            Self::Deactivating => "deactivating",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdEnablement {
    Enabled,
    EnabledRuntime,
    Disabled,
    Masked,
    Other,
    NotInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdLoadState {
    Loaded,
    NotFound,
}

impl SystemdLoadState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::NotFound => "not_found",
        }
    }
}

impl SystemdEnablement {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::EnabledRuntime => "enabled_runtime",
            Self::Disabled => "disabled",
            Self::Masked => "masked",
            Self::Other => "other",
            Self::NotInstalled => "not_installed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SystemdSnapshot {
    load_state: SystemdLoadState,
    active_state: SystemdObservedState,
    enablement: SystemdEnablement,
}

fn validate_service_name(service_name: &str) -> Result<(), EvaError> {
    if service_name.is_empty()
        || service_name.len() > 240
        || !service_name
            .as_bytes()
            .first()
            .is_some_and(|value| value.is_ascii_alphanumeric())
    {
        return Err(EvaError::invalid_argument(
            "systemd service name length is invalid",
        ));
    }
    if service_name.chars().any(|character| {
        !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-')
    }) {
        return Err(EvaError::invalid_argument(
            "systemd service name contains an unsupported character",
        ));
    }
    Ok(())
}

fn resolve_unit_name(definition: &ServiceManagerDefinition) -> Result<String, EvaError> {
    let raw = definition
        .unit_name
        .as_deref()
        .unwrap_or(&definition.service_name);
    let unit_name = if raw.ends_with(".service") {
        raw.to_owned()
    } else {
        format!("{raw}.service")
    };
    if unit_name.len() > 255
        || !unit_name.ends_with(".service")
        || unit_name == ".service"
        || !unit_name
            .as_bytes()
            .first()
            .is_some_and(|value| value.is_ascii_alphanumeric())
        || unit_name.contains('/')
        || unit_name.contains('\\')
        || unit_name.contains("..")
        || unit_name.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-')
        })
    {
        return Err(EvaError::invalid_argument("systemd unit name is invalid"));
    }
    Ok(unit_name)
}

fn managed_marker(unit_name: &str) -> String {
    format!("{MANAGED_UNIT_MARKER_PREFIX}{unit_name}\n")
}

fn systemd_exec_path(binary: &Path) -> Result<String, EvaError> {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[cfg(unix)]
    let bytes = binary.as_os_str().as_bytes();
    #[cfg(not(unix))]
    let owned = binary.to_string_lossy().into_owned();
    #[cfg(not(unix))]
    let bytes = owned.as_bytes();

    let mut escaped = String::from("\"");
    for &byte in bytes {
        match byte {
            b'$' => {
                return Err(EvaError::invalid_argument(
                    "systemd runtime_binary contains an unsupported character",
                ));
            }
            b'\\' => escaped.push_str("\\\\"),
            b'"' => escaped.push_str("\\\""),
            b'%' => escaped.push_str("%%"),
            b'\t' => escaped.push_str("\\t"),
            0x20..=0x7e => escaped.push(byte as char),
            _ => escaped.push_str(&format!("\\x{byte:02X}")),
        }
    }
    escaped.push('"');
    Ok(escaped)
}

fn atomic_create_unit(root: &Path, unit_name: &str, bytes: &[u8]) -> Result<(), EvaError> {
    validate_unit_root(root)?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".{unit_name}.eva-tmp-{}-{counter}", std::process::id());
    let temp_path = root.join(temp_name);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut file = options
            .open(&temp_path)
            .map_err(|error| io_error("failed to create temporary systemd unit", "unit", error))?;
        file.write_all(bytes)
            .map_err(|error| io_error("failed to write temporary systemd unit", "unit", error))?;
        file.flush()
            .map_err(|error| io_error("failed to flush temporary systemd unit", "unit", error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644)).map_err(
                |error| io_error("failed to set systemd unit permissions", "unit", error),
            )?;
        }
        file.sync_all()
            .map_err(|error| io_error("failed to sync temporary systemd unit", "unit", error))?;
        let target = root.join(unit_name);
        fs::hard_link(&temp_path, &target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                EvaError::conflict("systemd unit appeared during atomic publication")
            } else {
                io_error("failed to publish systemd unit", "unit", error)
            }
        })?;
        fs::remove_file(&temp_path)
            .map_err(|error| io_error("failed to remove temporary systemd unit", "unit", error))?;
        sync_directory(root)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn move_unit_to_tombstone(root: &Path, unit_name: &str) -> Result<PathBuf, EvaError> {
    validate_unit_root(root)?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tombstone = root.join(format!(
        ".{unit_name}.eva-delete-{}-{counter}",
        std::process::id()
    ));
    let target = root.join(unit_name);
    fs::hard_link(&target, &tombstone)
        .map_err(|error| io_error("failed to stage systemd unit removal", "unit", error))?;
    if let Err(error) = fs::remove_file(&target) {
        let _ = fs::remove_file(&tombstone);
        return Err(io_error("failed to detach systemd unit", "unit", error));
    }
    if let Err(error) = sync_directory(root) {
        let _ = fs::rename(&tombstone, &target);
        return Err(error);
    }
    Ok(tombstone)
}

fn restore_tombstone(root: &Path, tombstone: &Path, unit_name: &str) {
    if !root.join(unit_name).exists() {
        let _ = fs::rename(tombstone, root.join(unit_name));
        let _ = sync_directory(root);
    }
}

fn validate_unit_root(root: &Path) -> Result<(), EvaError> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| io_error("systemd unit root is unavailable", "unit_root", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(EvaError::conflict(
            "systemd unit root must be a non-symlink directory",
        ));
    }
    Ok(())
}

fn validate_native_unit_permissions(metadata: &fs::Metadata) -> Result<(), EvaError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o022 != 0 {
            return Err(EvaError::conflict(
                "systemd unit is writable by group or other users",
            ));
        }
        #[cfg(target_os = "linux")]
        if metadata.uid() != 0 {
            return Err(EvaError::permission_denied(
                "systemd unit is not owned by root",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), EvaError> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                io_error("failed to sync systemd unit directory", "unit_root", error)
            })?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn parse_systemd_show(report: &ServiceCommandReport) -> Result<SystemdSnapshot, EvaError> {
    let text = String::from_utf8(report.stdout.bytes().to_vec())
        .map_err(|_| EvaError::conflict("systemd show output is not valid UTF-8"))?;
    let mut load_state = None;
    let mut active_state = None;
    let mut sub_state = None;
    let mut unit_file_state = None;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| EvaError::conflict("systemd show output contains a malformed field"))?;
        match key {
            "LoadState" if load_state.is_none() => load_state = Some(value),
            "ActiveState" if active_state.is_none() => active_state = Some(value),
            "SubState" if sub_state.is_none() => sub_state = Some(value),
            "UnitFileState" if unit_file_state.is_none() => unit_file_state = Some(value),
            "LoadState" | "ActiveState" | "SubState" | "UnitFileState" => {
                return Err(EvaError::conflict("systemd show output repeats a field"));
            }
            _ => {
                return Err(EvaError::conflict(
                    "systemd show output contains an unexpected field",
                ));
            }
        }
    }
    let load_state = match load_state {
        Some("loaded") => SystemdLoadState::Loaded,
        Some("not-found") => SystemdLoadState::NotFound,
        Some("masked") | Some("error") | Some("bad-setting") => {
            return Err(EvaError::conflict("systemd unit load state is not usable"));
        }
        Some(value) => {
            return Err(EvaError::conflict("systemd returned an unknown load state")
                .with_context("systemd_load_state", value.to_owned()));
        }
        None => {
            return Err(EvaError::conflict(
                "systemd show output is missing LoadState",
            ))
        }
    };
    let active_state_value = active_state
        .ok_or_else(|| EvaError::conflict("systemd show output is missing ActiveState"))?;
    let sub_state_value =
        sub_state.ok_or_else(|| EvaError::conflict("systemd show output is missing SubState"))?;
    let unit_file_state_value = unit_file_state
        .ok_or_else(|| EvaError::conflict("systemd show output is missing UnitFileState"))?;
    if load_state == SystemdLoadState::NotFound {
        return Ok(SystemdSnapshot {
            load_state,
            active_state: SystemdObservedState::NotInstalled,
            enablement: SystemdEnablement::NotInstalled,
        });
    }
    if active_state_value.is_empty()
        || sub_state_value.is_empty()
        || unit_file_state_value.is_empty()
    {
        return Err(EvaError::conflict(
            "systemd show output contains an empty field",
        ));
    }
    let active_state = parse_active_state_value(active_state_value)?;
    let enablement = parse_enablement_value(unit_file_state_value);
    Ok(SystemdSnapshot {
        load_state,
        active_state,
        enablement,
    })
}

fn parse_active_state_value(value: &str) -> Result<SystemdObservedState, EvaError> {
    match value {
        "active" => Ok(SystemdObservedState::Active),
        "inactive" => Ok(SystemdObservedState::Inactive),
        "failed" => Ok(SystemdObservedState::Failed),
        "activating" => Ok(SystemdObservedState::Activating),
        "deactivating" | "reloading" => Ok(SystemdObservedState::Deactivating),
        _ => Err(
            EvaError::conflict("systemd returned an unknown active state")
                .with_context("systemd_state", value.to_owned()),
        ),
    }
}

fn parse_enablement_value(value: &str) -> SystemdEnablement {
    match value {
        "enabled" => SystemdEnablement::Enabled,
        "enabled-runtime" => SystemdEnablement::EnabledRuntime,
        "disabled" => SystemdEnablement::Disabled,
        "masked" => SystemdEnablement::Masked,
        "not-found" => SystemdEnablement::NotInstalled,
        _ => SystemdEnablement::Other,
    }
}

fn report_text(report: &ServiceCommandReport) -> String {
    let stdout = String::from_utf8_lossy(report.stdout.bytes());
    if !stdout.trim().is_empty() {
        stdout.into_owned()
    } else {
        String::from_utf8_lossy(report.stderr.bytes()).into_owned()
    }
}

fn ensure_process_completion(
    report: &ServiceCommandReport,
    action: &'static str,
) -> Result<(), EvaError> {
    match report.termination {
        ServiceCommandTermination::Exited => Ok(()),
        ServiceCommandTermination::TimedOut => {
            Err(EvaError::timeout("systemd command timed out").with_context("action", action))
        }
        ServiceCommandTermination::OutputLimitExceeded => Err(EvaError::conflict(
            "systemd command output exceeded its limit",
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
    let text = report_text(report).to_ascii_lowercase();
    let exit_code = report.exit_code.unwrap_or(-1);
    let mut error = if text.contains("permission denied")
        || text.contains("access denied")
        || text.contains("authentication is required")
        || text.contains("not authorized")
    {
        EvaError::permission_denied("systemd denied the requested operation")
    } else if exit_code == 4 {
        EvaError::not_found("systemd unit is not installed")
    } else {
        EvaError::unavailable("systemd command failed")
    };
    error = error.with_context("action", action);
    error.with_context("exit_code", exit_code.to_string())
}

fn adapter_command_audit(report: &ServiceCommandReport, action: &'static str) -> Vec<String> {
    let mut audit = report.audit.clone();
    audit.push(format!("systemd.command:{action}"));
    audit
}

fn digest_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn io_error(message: &'static str, context_key: &'static str, error: std::io::Error) -> EvaError {
    let kind = match error.kind() {
        std::io::ErrorKind::PermissionDenied => EvaError::permission_denied(message),
        std::io::ErrorKind::NotFound => EvaError::not_found(message),
        std::io::ErrorKind::AlreadyExists => EvaError::conflict(message),
        _ => EvaError::unavailable(message),
    };
    kind.with_context(context_key, error.kind().to_string())
}

fn sleep_poll_interval(interval: Duration) {
    if interval.is_zero() {
        std::thread::yield_now();
    } else {
        std::thread::sleep(interval);
    }
}

fn systemctl_executable() -> Result<PathBuf, EvaError> {
    for candidate in [Path::new("/usr/bin/systemctl"), Path::new("/bin/systemctl")] {
        if candidate.is_file() {
            let resolved = fs::canonicalize(candidate)
                .map_err(|error| io_error("failed to resolve systemctl", "systemctl", error))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = fs::metadata(&resolved)
                    .map_err(|error| io_error("failed to inspect systemctl", "systemctl", error))?;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Ok(resolved);
        }
    }
    Err(EvaError::not_found("trusted systemctl was not found"))
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
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_SERVICE_NAME: &str = "EvaCliSystemdTest";
    const TEST_UNIT_NAME: &str = "eva-cli-systemd-test.service";

    #[derive(Clone)]
    struct ScriptStep {
        arguments: Vec<OsString>,
        exit_code: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
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
                .expect("unexpected systemd command");
            assert_eq!(target.kind(), ServiceManagerKind::Systemd);
            assert_eq!(command.executable(), Path::new("systemctl"));
            assert_eq!(command.arguments().len(), step.arguments.len());
            for (actual, expected) in command.arguments().iter().zip(&step.arguments) {
                assert_eq!(actual.value(), expected.as_os_str());
                assert_eq!(actual.visibility(), ServiceCommandArgVisibility::Public);
            }
            state.command_debug.push(format!("{command:?}"));
            Ok(ServiceCommandExecution::exited(
                Some(step.exit_code),
                step.stdout,
                step.stderr,
            ))
        }
    }

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("eva-systemd-test-{}-{nonce}", std::process::id()));
            fs::create_dir(&path).expect("temporary unit root");
            Self { path }
        }

        fn path(&self) -> PathBuf {
            self.path.clone()
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn public_args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn step(values: &[&str], exit_code: i32, stdout: &str) -> ScriptStep {
        ScriptStep {
            arguments: public_args(values),
            exit_code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn stderr_step(values: &[&str], exit_code: i32, stderr: &str) -> ScriptStep {
        ScriptStep {
            arguments: public_args(values),
            exit_code,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn show_step(
        load_state: &str,
        active_state: &str,
        unit_file_state: &str,
        exit_code: i32,
    ) -> ScriptStep {
        let output = format!(
            "UnitFileState={unit_file_state}\nSubState={sub_state}\nLoadState={load_state}\nActiveState={active_state}\n",
            sub_state = if active_state == "active" { "running" } else { "dead" },
        );
        step(
            &[
                "--system",
                "--no-ask-password",
                "--no-pager",
                "show",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=SubState",
                "--property=UnitFileState",
                TEST_UNIT_NAME,
            ],
            exit_code,
            &output,
        )
    }

    fn missing_show_step() -> ScriptStep {
        step(
            &[
                "--system",
                "--no-ask-password",
                "--no-pager",
                "show",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=SubState",
                "--property=UnitFileState",
                TEST_UNIT_NAME,
            ],
            1,
            "LoadState=not-found\nActiveState=\nSubState=\nUnitFileState=\n",
        )
    }

    fn definition() -> ServiceManagerDefinition {
        ServiceManagerDefinition {
            enabled: true,
            kind: ServiceManagerKind::Systemd,
            service_name: TEST_SERVICE_NAME.to_owned(),
            unit_name: Some(TEST_UNIT_NAME.to_owned()),
            runtime_binary: Some(
                fs::canonicalize(std::env::current_exe().expect("current test executable"))
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

    fn install_steps() -> Vec<ScriptStep> {
        vec![
            missing_show_step(),
            step(&["--system", "--no-ask-password", "daemon-reload"], 0, ""),
            show_step("loaded", "inactive", "disabled", 0),
            step(
                &["--system", "--no-ask-password", "enable", TEST_UNIT_NAME],
                0,
                "",
            ),
            show_step("loaded", "inactive", "enabled", 0),
            show_step("loaded", "inactive", "enabled", 0),
        ]
    }

    #[test]
    fn install_publishes_unit_atomically_and_uses_exact_systemctl_argv() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new(install_steps());
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();

        let report = adapter
            .install(mutation_request(&definition))
            .expect("systemd install");

        assert_eq!(report.state, ServiceManagerState::Stopped);
        assert!(report.mutation_executed);
        assert!(report.production_adapter);
        let unit = fs::read(root.path().join(TEST_UNIT_NAME)).expect("published unit");
        let unit_text = String::from_utf8(unit).expect("utf8 unit");
        assert!(unit_text.starts_with(&managed_marker(TEST_UNIT_NAME)));
        assert!(unit_text.contains("ExecStart=\""));
        assert!(unit_text.contains("Restart=on-failure"));
        assert_eq!(
            fs::read_dir(root.path())
                .expect("unit root entries")
                .count(),
            1,
            "temporary unit must not remain"
        );
        assert!(!report.audit.join("\n").contains(
            definition
                .runtime_binary
                .as_ref()
                .expect("runtime binary")
                .to_string_lossy()
                .as_ref()
        ));
        probe.assert_drained();
    }

    #[test]
    fn repeated_install_is_a_noop_when_unit_and_enablement_match() {
        let root = TempRoot::new();
        let mut steps = install_steps();
        steps.extend([
            show_step("loaded", "inactive", "enabled", 0),
            show_step("loaded", "inactive", "enabled", 0),
        ]);
        let executor = ScriptedExecutor::new(steps);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();

        let first = adapter
            .install(mutation_request(&definition))
            .expect("first install");
        let second = adapter
            .install(mutation_request(&definition))
            .expect("repeated install");

        assert!(first.mutation_executed);
        assert!(!second.mutation_executed);
        assert_eq!(second.state, ServiceManagerState::Stopped);
        probe.assert_drained();
    }

    #[test]
    fn start_stop_restart_follow_idempotent_state_transitions() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            show_step("loaded", "inactive", "disabled", 0),
            step(
                &["--system", "--no-ask-password", "start", TEST_UNIT_NAME],
                0,
                "",
            ),
            show_step("loaded", "active", "disabled", 0),
            show_step("loaded", "active", "disabled", 0),
            step(
                &["--system", "--no-ask-password", "stop", TEST_UNIT_NAME],
                0,
                "",
            ),
            show_step("loaded", "inactive", "disabled", 0),
            show_step("loaded", "inactive", "disabled", 0),
            step(
                &["--system", "--no-ask-password", "restart", TEST_UNIT_NAME],
                0,
                "",
            ),
            show_step("loaded", "active", "disabled", 0),
        ]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();
        fs::create_dir_all(root.path()).expect("unit root");
        let desired = SystemdAdapter::<ScriptedExecutor>::desired_unit(
            &definition,
            TEST_UNIT_NAME,
            definition.runtime_binary.as_deref().expect("binary"),
        )
        .expect("desired unit");
        atomic_create_unit(&root.path(), TEST_UNIT_NAME, &desired).expect("unit fixture");

        let started = adapter.start(mutation_request(&definition)).expect("start");
        assert!(started.mutation_executed);
        let stopped = adapter.stop(mutation_request(&definition)).expect("stop");
        assert!(stopped.mutation_executed);
        let restarted = adapter
            .restart(mutation_request(&definition))
            .expect("restart");
        assert!(restarted.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn running_uninstall_stops_disables_removes_and_reloads() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            show_step("loaded", "active", "enabled", 0),
            show_step("loaded", "active", "enabled", 0),
            step(
                &["--system", "--no-ask-password", "stop", TEST_UNIT_NAME],
                0,
                "",
            ),
            show_step("loaded", "inactive", "enabled", 0),
            step(
                &["--system", "--no-ask-password", "disable", TEST_UNIT_NAME],
                0,
                "",
            ),
            step(&["--system", "--no-ask-password", "daemon-reload"], 0, ""),
            missing_show_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();
        let desired = SystemdAdapter::<ScriptedExecutor>::desired_unit(
            &definition,
            TEST_UNIT_NAME,
            definition.runtime_binary.as_deref().expect("binary"),
        )
        .expect("desired unit");
        atomic_create_unit(&root.path(), TEST_UNIT_NAME, &desired).expect("unit fixture");

        let report = adapter
            .uninstall(mutation_request(&definition))
            .expect("uninstall");

        assert_eq!(report.state, ServiceManagerState::NotInstalled);
        assert!(report.mutation_executed);
        assert!(!root.path().join(TEST_UNIT_NAME).exists());
        probe.assert_drained();
    }

    #[test]
    fn missing_operations_are_idempotent_and_restart_is_not_found() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            missing_show_step(),
            missing_show_step(),
            missing_show_step(),
        ]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();

        let status = adapter
            .status(status_request(&definition))
            .expect("missing status");
        assert_eq!(status.state, ServiceManagerState::NotInstalled);
        let stop = adapter
            .stop(mutation_request(&definition))
            .expect("missing stop");
        assert!(!stop.mutation_executed);
        assert_eq!(stop.state, ServiceManagerState::NotInstalled);
        let uninstall = adapter
            .uninstall(mutation_request(&definition))
            .expect("missing uninstall");
        assert!(!uninstall.mutation_executed);
        let restart = adapter
            .restart(mutation_request(&definition))
            .expect_err("missing restart");
        assert_eq!(restart.kind(), ErrorKind::NotFound);
        assert_eq!(probe.call_count(), 3);
    }

    #[test]
    fn drift_foreign_units_and_invalid_names_fail_closed_before_commands() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();
        fs::write(
            root.path().join(TEST_UNIT_NAME),
            b"[Unit]\nDescription=foreign\n",
        )
        .expect("foreign unit");
        let drift = adapter
            .install(mutation_request(&definition))
            .expect_err("unit drift");
        assert_eq!(drift.kind(), ErrorKind::Conflict);

        let mut invalid = definition.clone();
        invalid.service_name = "bad/name".to_owned();
        let invalid_error = adapter
            .status(status_request(&invalid))
            .expect_err("invalid service name");
        assert_eq!(invalid_error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(probe.call_count(), 0);
    }

    #[test]
    fn failed_and_transitional_states_have_stable_typed_mapping() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            show_step("loaded", "failed", "disabled", 0),
            show_step("loaded", "activating", "disabled", 0),
        ]);
        let probe = executor.clone();
        let adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();
        let desired = SystemdAdapter::<ScriptedExecutor>::desired_unit(
            &definition,
            TEST_UNIT_NAME,
            definition.runtime_binary.as_deref().expect("binary"),
        )
        .expect("desired unit");
        atomic_create_unit(&root.path(), TEST_UNIT_NAME, &desired).expect("unit fixture");

        let failed = adapter
            .status(status_request(&definition))
            .expect("failed state");
        assert_eq!(failed.state, ServiceManagerState::Stopped);
        let transitional = adapter
            .status(status_request(&definition))
            .expect_err("activating state");
        assert_eq!(transitional.kind(), ErrorKind::Unavailable);
        probe.assert_drained();
    }

    #[test]
    fn systemd_show_parser_is_ordered_set_once_and_fail_closed() {
        let command =
            ServiceCommand::new("systemctl", [ServiceCommandArg::public("show")]).expect("command");
        let report = |stdout: &str| {
            ServiceCommandReport::from_execution(
                ServiceManagerKind::Systemd,
                &command,
                ServiceCommandExecution::exited(Some(0), stdout.as_bytes(), Vec::new()),
            )
            .expect("report")
        };

        let valid = parse_systemd_show(&report(
            "UnitFileState=enabled\nSubState=running\nActiveState=active\nLoadState=loaded\n",
        ))
        .expect("valid shuffled fields");
        assert_eq!(valid.load_state, SystemdLoadState::Loaded);
        assert_eq!(valid.active_state, SystemdObservedState::Active);
        assert_eq!(valid.enablement, SystemdEnablement::Enabled);

        let missing = parse_systemd_show(&report(
            "LoadState=not-found\nActiveState=\nSubState=\nUnitFileState=\n",
        ))
        .expect("not-found snapshot");
        assert_eq!(missing.load_state, SystemdLoadState::NotFound);
        assert_eq!(missing.active_state, SystemdObservedState::NotInstalled);

        for malformed in [
            "LoadState=loaded\nActiveState=active\nSubState=running\n",
            "LoadState=loaded\nLoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\n",
            "LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\nUnexpected=value\n",
            "LoadState=loaded\nActiveState=unknown\nSubState=running\nUnitFileState=enabled\n",
        ] {
            assert_eq!(parse_systemd_show(&report(malformed)).unwrap_err().kind(), ErrorKind::Conflict);
        }
    }

    #[test]
    fn permission_denied_from_systemctl_is_mapped_without_raw_output() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            missing_show_step(),
            stderr_step(
                &["--system", "--no-ask-password", "daemon-reload"],
                1,
                "Access denied\n",
            ),
        ]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();

        let error = adapter
            .install(mutation_request(&definition))
            .expect_err("daemon reload denial");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(!format!("{error:?}").contains("Access denied"));
        probe.assert_drained();
    }

    #[test]
    fn query_failure_without_stdout_preserves_permission_classification() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([stderr_step(
            &[
                "--system",
                "--no-ask-password",
                "--no-pager",
                "show",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=SubState",
                "--property=UnitFileState",
                TEST_UNIT_NAME,
            ],
            1,
            "Access denied\n",
        )]);
        let probe = executor.clone();
        let adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();

        let error = adapter
            .status(status_request(&definition))
            .expect_err("query denial");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(!format!("{error:?}").contains("Access denied"));
        probe.assert_drained();
    }

    #[test]
    fn managed_marker_does_not_allow_binary_drift_to_start() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();
        let mut desired = SystemdAdapter::<ScriptedExecutor>::desired_unit(
            &definition,
            TEST_UNIT_NAME,
            definition.runtime_binary.as_deref().expect("binary"),
        )
        .expect("desired unit");
        desired.extend_from_slice(b"# retained marker with drift\n");
        atomic_create_unit(&root.path(), TEST_UNIT_NAME, &desired).expect("drifted unit");

        let error = adapter
            .start(mutation_request(&definition))
            .expect_err("managed marker cannot hide drift");

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(probe.call_count(), 0);
    }

    #[test]
    fn uninstall_reload_failure_restores_the_managed_unit() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            show_step("loaded", "inactive", "disabled", 0),
            show_step("loaded", "inactive", "disabled", 0),
            stderr_step(
                &["--system", "--no-ask-password", "daemon-reload"],
                1,
                "Access denied\n",
            ),
        ]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let definition = definition();
        let desired = SystemdAdapter::<ScriptedExecutor>::desired_unit(
            &definition,
            TEST_UNIT_NAME,
            definition.runtime_binary.as_deref().expect("binary"),
        )
        .expect("desired unit");
        atomic_create_unit(&root.path(), TEST_UNIT_NAME, &desired).expect("unit fixture");

        let error = adapter
            .uninstall(mutation_request(&definition))
            .expect_err("reload failure");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(fs::read(root.path().join(TEST_UNIT_NAME)).unwrap(), desired);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        probe.assert_drained();
    }

    #[test]
    fn install_disables_existing_boot_enablement_when_requested() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([
            show_step("loaded", "inactive", "enabled", 0),
            step(
                &["--system", "--no-ask-password", "disable", TEST_UNIT_NAME],
                0,
                "",
            ),
            show_step("loaded", "inactive", "disabled", 0),
            show_step("loaded", "inactive", "disabled", 0),
        ]);
        let probe = executor.clone();
        let mut adapter = SystemdAdapter::for_test(executor, root.path());
        let mut definition = definition();
        definition.start_on_boot = false;
        let desired = SystemdAdapter::<ScriptedExecutor>::desired_unit(
            &definition,
            TEST_UNIT_NAME,
            definition.runtime_binary.as_deref().expect("binary"),
        )
        .expect("desired unit");
        atomic_create_unit(&root.path(), TEST_UNIT_NAME, &desired).expect("unit fixture");

        let report = adapter
            .install(mutation_request(&definition))
            .expect("disable boot enablement");

        assert!(report.mutation_executed);
        assert_eq!(report.state, ServiceManagerState::Stopped);
        probe.assert_drained();
    }

    #[test]
    fn executable_path_escaping_is_bounded_and_does_not_allow_environment_expansion() {
        assert_eq!(
            systemd_exec_path(Path::new("/opt/eva 100%.bin")).expect("escaped path"),
            r#""/opt/eva 100%%.bin""#
        );
        assert!(systemd_exec_path(Path::new("/opt/$HOME/eva")).is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn native_constructor_rejects_non_linux_hosts() {
        let error = SystemdAdapter::native()
            .err()
            .expect("systemd is Linux-only");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a controlled Linux host with systemd and elevated unit access"]
    fn native_unique_unit_lifecycle_probe() {
        let root = PathBuf::from(SYSTEMD_UNIT_ROOT);
        let mut adapter = SystemdAdapter::native().expect("native systemd adapter");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let unique = format!("{}-{nonce}", std::process::id());
        let mut definition = definition();
        definition.service_name = format!("EvaCliProbe{}", unique.replace('-', ""));
        definition.unit_name = Some(format!("eva-cli-probe-{unique}.service"));
        definition.runtime_binary = Some(PathBuf::from("/bin/true"));
        let install = adapter.install(mutation_request(&definition));
        let cleanup = adapter.uninstall(mutation_request(&definition));

        let unit_name = definition.unit_name.as_deref().expect("unit name");
        let mut forced_cleanup_errors = Vec::new();
        let mut forced_cleanup_executed = false;
        for entry in fs::read_dir(&root).expect("read systemd unit root") {
            let entry = entry.expect("read systemd unit entry");
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name == unit_name || file_name.contains(&format!(".{unit_name}.eva-")) {
                forced_cleanup_executed = true;
                if let Err(error) = fs::remove_file(entry.path()) {
                    forced_cleanup_errors.push(error.to_string());
                }
            }
        }
        if forced_cleanup_executed {
            if let Err(error) = adapter.run_daemon_reload() {
                forced_cleanup_errors.push(error.to_string());
            }
        }

        assert!(
            forced_cleanup_errors.is_empty(),
            "forced systemd cleanup failed: {forced_cleanup_errors:?}"
        );
        cleanup.expect("adapter systemd cleanup");
        assert!(!root.join(unit_name).exists(), "systemd unit residue");
        match install {
            Ok(report) => assert!(report.mutation_executed),
            Err(error) => assert_eq!(error.kind(), ErrorKind::PermissionDenied),
        }
    }
}
