//! macOS launchd adapter backed by structured `launchctl` argv and managed plists.

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
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_LAUNCHD_POLL_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_LAUNCHD_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SYSTEM_PLIST_ROOT: &str = "/Library/LaunchDaemons";
const PLIST_MAX_BYTES: u64 = 64 * 1024;
const MANAGED_PLIST_MARKER_PREFIX: &str = "<!-- eva-cli-managed-plist=v1; identity-sha256=";
const LAUNCHCTL_MISSING_EXIT_CODES: [i32; 2] = [3, 113];

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Immutable launchd bootstrap domain captured by an adapter instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchdDomain {
    /// System launch daemon domain.
    System,
    /// Aqua login domain for one captured non-root user.
    Gui {
        /// Effective user id captured when the adapter was constructed.
        uid: u32,
    },
}

impl LaunchdDomain {
    fn selector(self) -> LaunchdDomainSelector {
        match self {
            Self::System => LaunchdDomainSelector::System,
            Self::Gui { .. } => LaunchdDomainSelector::Gui,
        }
    }

    fn target(self) -> String {
        match self {
            Self::System => "system".to_owned(),
            Self::Gui { uid } => format!("gui/{uid}"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Gui { .. } => "gui",
        }
    }
}

/// Production launchd adapter bound to one domain and one configured label.
pub struct LaunchdAdapter<E> {
    factory: Mutex<ServiceManagerFactory<E>>,
    launchctl_executable: PathBuf,
    domain: LaunchdDomain,
    label: String,
    plist_root: PathBuf,
    expected_uid: u32,
    expected_gid: u32,
    allow_root_create: bool,
    enforce_native_permissions: bool,
    poll_timeout: Duration,
    poll_interval: Duration,
}

impl<E> LaunchdAdapter<E>
where
    E: ServiceCommandExecutor,
{
    /// Creates a native adapter from the explicit `system/<label>` or
    /// `gui/<label>` launchd unit name in the definition.
    pub fn new(executor: E, definition: &ServiceManagerDefinition) -> Result<Self, EvaError> {
        if ServiceHostPlatform::current() != ServiceHostPlatform::Macos {
            return Err(
                EvaError::unsupported("launchd adapter requires a macOS host")
                    .with_context("host_platform", ServiceHostPlatform::current().as_str()),
            );
        }
        let identity = validate_launchd_definition(definition, false)?;
        let launchctl_executable = launchctl_executable()?;
        match identity.selector {
            LaunchdDomainSelector::System => Ok(Self::with_paths(
                executor,
                launchctl_executable,
                LaunchdDomain::System,
                identity.label,
                PathBuf::from(SYSTEM_PLIST_ROOT),
                0,
                0,
                false,
                true,
            )),
            LaunchdDomainSelector::Gui => {
                let (uid, gid, home) = current_gui_user()?;
                if uid == 0 {
                    return Err(EvaError::permission_denied(
                        "root cannot be bound to a launchd GUI domain",
                    ));
                }
                let plist_root = home.join("Library").join("LaunchAgents");
                require_utf8_path(&plist_root, "launchd GUI plist root")?;
                Ok(Self::with_paths(
                    executor,
                    launchctl_executable,
                    LaunchdDomain::Gui { uid },
                    identity.label,
                    plist_root,
                    uid,
                    gid,
                    true,
                    true,
                ))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn with_paths(
        executor: E,
        launchctl_executable: PathBuf,
        domain: LaunchdDomain,
        label: String,
        plist_root: PathBuf,
        expected_uid: u32,
        expected_gid: u32,
        allow_root_create: bool,
        enforce_native_permissions: bool,
    ) -> Self {
        Self {
            factory: Mutex::new(ServiceManagerFactory::new(executor)),
            launchctl_executable,
            domain,
            label,
            plist_root,
            expected_uid,
            expected_gid,
            allow_root_create,
            enforce_native_permissions,
            poll_timeout: DEFAULT_LAUNCHD_POLL_TIMEOUT,
            poll_interval: DEFAULT_LAUNCHD_POLL_INTERVAL,
        }
    }

    #[cfg(test)]
    fn for_test(
        executor: E,
        domain: LaunchdDomain,
        label: impl Into<String>,
        plist_root: PathBuf,
    ) -> Self {
        Self {
            factory: Mutex::new(ServiceManagerFactory::for_host(
                ServiceHostPlatform::Macos,
                executor,
            )),
            launchctl_executable: PathBuf::from("launchctl"),
            domain,
            label: label.into(),
            plist_root,
            expected_uid: match domain {
                LaunchdDomain::System => 0,
                LaunchdDomain::Gui { uid } => uid,
            },
            expected_gid: 0,
            allow_root_create: false,
            enforce_native_permissions: false,
            poll_timeout: Duration::from_secs(1),
            poll_interval: Duration::ZERO,
        }
    }

    fn execute(&self, arguments: Vec<ServiceCommandArg>) -> Result<ServiceCommandReport, EvaError> {
        let command = ServiceCommand::new(self.launchctl_executable.clone(), arguments)?;
        let mut factory = self
            .factory
            .lock()
            .map_err(|_| EvaError::internal("launchd command factory lock is poisoned"))?;
        factory.execute_command(ServiceManagerKind::Launchd, &command)
    }

    fn validate_definition(
        &self,
        definition: &ServiceManagerDefinition,
        mutation: bool,
    ) -> Result<LaunchdIdentity, EvaError> {
        let identity = validate_launchd_definition(definition, mutation)?;
        if identity.selector != self.domain.selector() || identity.label != self.label {
            return Err(EvaError::conflict(
                "launchd definition does not match the adapter domain and label",
            ));
        }
        Ok(identity)
    }

    fn plist_path(&self) -> PathBuf {
        self.plist_root.join(format!("{}.plist", self.label))
    }

    fn domain_target(&self) -> String {
        self.domain.target()
    }

    fn service_target(&self) -> String {
        format!("{}/{}", self.domain.target(), self.label)
    }

    fn install_binary(&self, definition: &ServiceManagerDefinition) -> Result<PathBuf, EvaError> {
        let binary = definition
            .runtime_binary
            .as_ref()
            .ok_or_else(|| EvaError::invalid_argument("launchd install requires runtime_binary"))?;
        if !binary.is_absolute() {
            return Err(EvaError::invalid_argument(
                "launchd runtime_binary must be absolute",
            ));
        }
        let canonical = fs::canonicalize(binary).map_err(|error| {
            io_error(
                "failed to canonicalize launchd runtime_binary",
                "runtime_binary",
                error,
            )
        })?;
        let metadata = fs::metadata(&canonical).map_err(|error| {
            io_error(
                "failed to inspect canonical launchd runtime_binary",
                "runtime_binary",
                error,
            )
        })?;
        if !metadata.is_file() {
            return Err(EvaError::invalid_argument(
                "launchd runtime_binary must be a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(EvaError::permission_denied(
                    "launchd runtime_binary is not executable",
                ));
            }
            if self.enforce_native_permissions {
                let mut current = Some(canonical.as_path());
                while let Some(component) = current {
                    let component_metadata = fs::symlink_metadata(component).map_err(|error| {
                        io_error(
                            "failed to inspect launchd runtime path component",
                            "runtime_binary",
                            error,
                        )
                    })?;
                    if component_metadata.file_type().is_symlink() {
                        return Err(EvaError::conflict(
                            "canonical launchd runtime path contains a symlink",
                        ));
                    }
                    let owner_allowed = match self.domain {
                        LaunchdDomain::System => component_metadata.uid() == 0,
                        LaunchdDomain::Gui { uid } => {
                            component_metadata.uid() == 0 || component_metadata.uid() == uid
                        }
                    };
                    if !owner_allowed {
                        return Err(EvaError::permission_denied(
                            "launchd runtime path owner is not trusted for the domain",
                        ));
                    }
                    if component_metadata.mode() & 0o022 != 0 {
                        return Err(EvaError::conflict(
                            "launchd runtime path is writable by group or other users",
                        ));
                    }
                    current = component.parent();
                }
            }
        }
        require_utf8_path(&canonical, "launchd runtime_binary")?;
        Ok(canonical)
    }

    fn desired_plist(
        &self,
        definition: &ServiceManagerDefinition,
        binary: &Path,
    ) -> Result<Vec<u8>, EvaError> {
        let binary = require_utf8_path(binary, "launchd runtime_binary")?;
        validate_xml_text(&self.label, "launchd label")?;
        validate_xml_text(binary, "launchd runtime_binary")?;
        let marker = self.managed_marker(definition);
        let label = xml_escape(&self.label);
        let binary = xml_escape(binary);
        let run_at_load = xml_bool(definition.start_on_boot);
        let keep_alive = xml_bool(definition.restart_supervisor);
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{marker}\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{label}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{binary}</string>\n  </array>\n  <key>RunAtLoad</key>\n  {run_at_load}\n  <key>KeepAlive</key>\n  {keep_alive}\n  <key>ProcessType</key>\n  <string>Background</string>\n</dict>\n</plist>\n"
        )
        .into_bytes())
    }

    fn managed_marker(&self, definition: &ServiceManagerDefinition) -> String {
        let mut canonical = b"eva.launchd.plist.owner.v1\0".to_vec();
        append_digest_field(&mut canonical, self.domain.target().as_bytes());
        append_digest_field(&mut canonical, self.label.as_bytes());
        append_digest_field(&mut canonical, definition.service_name.as_bytes());
        format!(
            "{MANAGED_PLIST_MARKER_PREFIX}{} -->",
            digest_bytes(&canonical)
        )
    }

    fn managed_prefix(&self, definition: &ServiceManagerDefinition) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}\n",
            self.managed_marker(definition)
        )
        .into_bytes()
    }

    fn ensure_root(&self, for_write: bool) -> Result<bool, EvaError> {
        let metadata = match fs::symlink_metadata(&self.plist_root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !for_write {
                    return Ok(false);
                }
                if !self.allow_root_create {
                    return Err(EvaError::not_found("launchd plist root does not exist"));
                }
                self.create_gui_root()?;
                fs::symlink_metadata(&self.plist_root).map_err(|error| {
                    io_error("failed to inspect launchd plist root", "plist_root", error)
                })?
            }
            Err(error) => {
                return Err(io_error(
                    "failed to inspect launchd plist root",
                    "plist_root",
                    error,
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(EvaError::conflict(
                "launchd plist root must be a non-symlink directory",
            ));
        }
        if self.enforce_native_permissions {
            validate_native_owner(
                &metadata,
                self.expected_uid,
                self.expected_gid,
                "launchd plist root",
            )?;
        }
        Ok(true)
    }

    fn create_gui_root(&self) -> Result<(), EvaError> {
        let parent = self
            .plist_root
            .parent()
            .ok_or_else(|| EvaError::invalid_argument("launchd GUI plist root has no parent"))?;
        let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
            io_error(
                "launchd GUI Library directory is unavailable",
                "plist_root",
                error,
            )
        })?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(EvaError::conflict(
                "launchd GUI Library must be a non-symlink directory",
            ));
        }
        if self.enforce_native_permissions {
            validate_native_owner(
                &parent_metadata,
                self.expected_uid,
                self.expected_gid,
                "launchd GUI Library",
            )?;
        }
        fs::create_dir(&self.plist_root).map_err(|error| {
            io_error(
                "failed to create launchd GUI plist root",
                "plist_root",
                error,
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.plist_root, fs::Permissions::from_mode(0o755)).map_err(
                |error| {
                    io_error(
                        "failed to set launchd GUI plist root permissions",
                        "plist_root",
                        error,
                    )
                },
            )?;
        }
        sync_directory(parent)
    }

    fn read_plist(&self) -> Result<Option<Vec<u8>>, EvaError> {
        if !self.ensure_root(false)? {
            return Ok(None);
        }
        let path = self.plist_path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(io_error("failed to inspect launchd plist", "plist", error));
            }
        };
        if !metadata.file_type().is_file() {
            return Err(EvaError::conflict(
                "launchd plist path is not a regular file",
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
            .map_err(|error| io_error("failed to open launchd plist", "plist", error))?;
        let opened_metadata = file
            .metadata()
            .map_err(|error| io_error("failed to inspect opened launchd plist", "plist", error))?;
        if !opened_metadata.is_file() {
            return Err(EvaError::conflict(
                "opened launchd plist is not a regular file",
            ));
        }
        if self.enforce_native_permissions {
            validate_native_owner(
                &opened_metadata,
                self.expected_uid,
                self.expected_gid,
                "launchd plist",
            )?;
        }
        if opened_metadata.len() > PLIST_MAX_BYTES {
            return Err(EvaError::conflict("launchd plist exceeds the bounded size"));
        }
        let mut bytes = Vec::with_capacity((PLIST_MAX_BYTES as usize).saturating_add(1));
        let mut limited = (&mut file).take(PLIST_MAX_BYTES.saturating_add(1));
        limited
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("failed to read launchd plist", "plist", error))?;
        if bytes.len() as u64 > PLIST_MAX_BYTES {
            return Err(EvaError::conflict("launchd plist exceeds the bounded size"));
        }
        Ok(Some(bytes))
    }

    fn ensure_managed_plist(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<Vec<u8>, EvaError> {
        let bytes = self
            .read_plist()?
            .ok_or_else(|| EvaError::not_found("launchd plist is not installed"))?;
        self.validate_managed_plist_bytes(definition, &bytes)?;
        Ok(bytes)
    }

    fn validate_managed_plist_bytes(
        &self,
        definition: &ServiceManagerDefinition,
        bytes: &[u8],
    ) -> Result<(), EvaError> {
        if !bytes.starts_with(&self.managed_prefix(definition)) {
            return Err(EvaError::conflict(
                "launchd plist is not managed by this service definition",
            ));
        }
        if definition.runtime_binary.is_some() {
            let binary = self.install_binary(definition)?;
            let desired = self.desired_plist(definition, &binary)?;
            if bytes != desired {
                return Err(EvaError::conflict(
                    "managed launchd plist content does not match definition",
                ));
            }
        }
        Ok(())
    }

    fn ensure_desired_plist(
        &self,
        definition: &ServiceManagerDefinition,
        desired: &[u8],
    ) -> Result<bool, EvaError> {
        match self.read_plist()? {
            None => {
                self.ensure_root(true)?;
                atomic_create_plist(
                    &self.plist_root,
                    &self.plist_path(),
                    desired,
                    self.expected_uid,
                    self.expected_gid,
                    self.enforce_native_permissions,
                )?;
                Ok(true)
            }
            Some(existing) if existing == desired => Ok(false),
            Some(existing) if existing.starts_with(&self.managed_prefix(definition)) => Err(
                EvaError::conflict("existing launchd plist content does not match definition"),
            ),
            Some(_) => Err(EvaError::conflict(
                "existing launchd plist belongs to another owner",
            )),
        }
    }

    fn query_observed_state(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<(LaunchdObservedState, Vec<String>), EvaError> {
        let service_target = self.service_target();
        let report = self.execute(vec![
            ServiceCommandArg::public("print"),
            ServiceCommandArg::public(&service_target),
        ])?;
        let mut audit = adapter_command_audit(&report, "query_state");
        ensure_process_completion(&report, "query_state")?;
        if !report.success {
            let error = command_exit_error("query_state", &report);
            if error.kind() == eva_core::ErrorKind::NotFound {
                audit.push("launchd.state:not_loaded".to_owned());
                return Ok((LaunchdObservedState::NotLoaded, audit));
            }
            return Err(error);
        }
        let plist_path = self.plist_path();
        let plist_path = require_utf8_path(&plist_path, "launchd plist path")?;
        let program = match definition.runtime_binary.as_ref() {
            Some(_) => Some(self.install_binary(definition)?),
            None => None,
        };
        let program = program
            .as_deref()
            .map(|path| require_utf8_path(path, "launchd runtime_binary"))
            .transpose()?;
        let state = parse_launchctl_print(&report, &service_target, plist_path, program)?;
        audit.push(format!("launchd.state:{}", state.as_str()));
        Ok((state, audit))
    }

    fn wait_for_running(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<Vec<String>, EvaError> {
        self.wait_for_observed(definition, LaunchdWaitTarget::Running)
    }

    fn wait_for_not_loaded(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<Vec<String>, EvaError> {
        self.wait_for_observed(definition, LaunchdWaitTarget::NotLoaded)
    }

    fn wait_for_loaded_stopped(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<Vec<String>, EvaError> {
        self.wait_for_observed(definition, LaunchdWaitTarget::LoadedStopped)
    }

    fn wait_for_stable(
        &self,
        definition: &ServiceManagerDefinition,
    ) -> Result<(LaunchdObservedState, Vec<String>), EvaError> {
        let started_at = Instant::now();
        let mut audit = Vec::new();
        loop {
            let (state, current_audit) = self.query_observed_state(definition)?;
            audit.extend(current_audit);
            if state != LaunchdObservedState::Transitional {
                return Ok((state, audit));
            }
            if started_at.elapsed() >= self.poll_timeout {
                return Err(EvaError::timeout(
                    "launchd service did not reach a stable state",
                ));
            }
            sleep_poll_interval(self.poll_interval);
        }
    }

    fn wait_for_observed(
        &self,
        definition: &ServiceManagerDefinition,
        expected: LaunchdWaitTarget,
    ) -> Result<Vec<String>, EvaError> {
        let started_at = Instant::now();
        let mut audit = Vec::new();
        loop {
            let (state, current_audit) = self.query_observed_state(definition)?;
            audit.extend(current_audit);
            let matches = match expected {
                LaunchdWaitTarget::Running => state == LaunchdObservedState::Running,
                LaunchdWaitTarget::NotLoaded => state == LaunchdObservedState::NotLoaded,
                LaunchdWaitTarget::LoadedStopped => state == LaunchdObservedState::LoadedStopped,
            };
            if matches {
                return Ok(audit);
            }
            if started_at.elapsed() >= self.poll_timeout {
                return Err(EvaError::timeout("launchd state transition timed out")
                    .with_context("expected_state", expected.as_str()));
            }
            sleep_poll_interval(self.poll_interval);
        }
    }

    fn run_bootstrap(&self) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("bootstrap"),
            ServiceCommandArg::public(self.domain_target()),
            ServiceCommandArg::secret(self.plist_path().into_os_string()),
        ])?;
        let audit = adapter_command_audit(&report, "bootstrap");
        ensure_success_exit(&report, "bootstrap")?;
        Ok(audit)
    }

    fn run_bootout(&self) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("bootout"),
            ServiceCommandArg::public(self.service_target()),
        ])?;
        let audit = adapter_command_audit(&report, "bootout");
        ensure_success_exit(&report, "bootout")?;
        Ok(audit)
    }

    fn run_kickstart(&self, restart: bool) -> Result<Vec<String>, EvaError> {
        let mut arguments = vec![ServiceCommandArg::public("kickstart")];
        if restart {
            arguments.push(ServiceCommandArg::public("-k"));
        }
        arguments.push(ServiceCommandArg::public(self.service_target()));
        let report = self.execute(arguments)?;
        let audit = adapter_command_audit(&report, if restart { "restart" } else { "start" });
        ensure_success_exit(&report, if restart { "restart" } else { "start" })?;
        Ok(audit)
    }

    fn run_kill(&self) -> Result<Vec<String>, EvaError> {
        let report = self.execute(vec![
            ServiceCommandArg::public("kill"),
            ServiceCommandArg::public("SIGTERM"),
            ServiceCommandArg::public(self.service_target()),
        ])?;
        let audit = adapter_command_audit(&report, "kill");
        ensure_success_exit(&report, "kill")?;
        Ok(audit)
    }

    fn restore_failed_uninstall(
        &self,
        definition: &ServiceManagerDefinition,
        tombstone: &Path,
        previous: LaunchdObservedState,
    ) -> Result<(), EvaError> {
        restore_tombstone(&self.plist_root, tombstone, &self.plist_path())?;
        let (mut current, _) = self.query_observed_state(definition)?;
        if previous == LaunchdObservedState::NotLoaded {
            if current != LaunchdObservedState::NotLoaded {
                self.run_bootout()?;
                self.wait_for_not_loaded(definition)?;
            }
            return Ok(());
        }
        if current == LaunchdObservedState::NotLoaded {
            self.run_bootstrap()?;
            current = self.wait_for_stable(definition)?.0;
        } else if current == LaunchdObservedState::Transitional {
            current = self.wait_for_stable(definition)?.0;
        }
        match previous {
            LaunchdObservedState::Running if current != LaunchdObservedState::Running => {
                self.run_kickstart(false)?;
                self.wait_for_running(definition)?;
            }
            LaunchdObservedState::LoadedStopped if current == LaunchdObservedState::Running => {
                self.run_kill()?;
                self.wait_for_loaded_stopped(definition)?;
            }
            LaunchdObservedState::LoadedStopped
                if current != LaunchdObservedState::LoadedStopped =>
            {
                return Err(EvaError::unavailable(
                    "failed to restore stopped launchd state after uninstall failure",
                ));
            }
            LaunchdObservedState::Transitional => {
                return Err(EvaError::internal(
                    "uninstall recovery received a transitional previous state",
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn audit_identity(&self) -> Vec<String> {
        let mut audit = vec![
            format!("launchd.domain:{}", self.domain.as_str()),
            format!("launchd.label:{}", self.label),
        ];
        if let LaunchdDomain::Gui { uid } = self.domain {
            audit.push(format!("launchd.gui_uid:{uid}"));
        }
        audit
    }

    fn status_report(
        &self,
        definition: &ServiceManagerDefinition,
        state: ServiceManagerState,
        mut audit: Vec<String>,
    ) -> ServiceManagerStatusReport {
        audit.push(format!("launchd.typed_state:{}", state.as_str()));
        ServiceManagerStatusReport {
            kind: ServiceManagerKind::Launchd,
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
        &self,
        definition: &ServiceManagerDefinition,
        operation: ServiceManagerOperation,
        state: ServiceManagerState,
        mutation_executed: bool,
        mut audit: Vec<String>,
    ) -> ServiceManagerMutationReport {
        audit.extend([
            format!("launchd.operation:{}", operation.as_str()),
            format!("service_manager.mutation_executed:{mutation_executed}"),
            format!("service_manager.state:{}", state.as_str()),
        ]);
        ServiceManagerMutationReport {
            kind: ServiceManagerKind::Launchd,
            service_name: definition.service_name.clone(),
            operation,
            state,
            mutation_executed,
            production_adapter: true,
            audit,
        }
    }
}

impl LaunchdAdapter<ProcessServiceCommandExecutor> {
    /// Creates a native launchd adapter bound to the definition's explicit domain.
    pub fn native(definition: &ServiceManagerDefinition) -> Result<Self, EvaError> {
        Self::new(ProcessServiceCommandExecutor::default(), definition)
    }
}

impl<E> ServiceManagerAdapter for LaunchdAdapter<E>
where
    E: ServiceCommandExecutor,
{
    fn kind(&self) -> ServiceManagerKind {
        ServiceManagerKind::Launchd
    }

    fn install(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        self.validate_definition(definition, true)?;
        let binary = self.install_binary(definition)?;
        let desired = self.desired_plist(definition, &binary)?;
        let mut audit = self.audit_identity();
        audit.push(format!("launchd.plist.digest:{}", digest_bytes(&desired)));

        let existing = self.read_plist()?;
        let mut observed = if existing.is_none() {
            let (state, state_audit) = self.query_observed_state(definition)?;
            audit.extend(state_audit);
            if state != LaunchdObservedState::NotLoaded {
                return Err(EvaError::conflict(
                    "launchd service target is already registered without a managed plist",
                ));
            }
            state
        } else {
            LaunchdObservedState::NotLoaded
        };
        let plist_changed = self.ensure_desired_plist(definition, &desired)?;
        audit.push(format!("launchd.plist.changed:{plist_changed}"));
        let created_plist = existing.is_none() && plist_changed;
        let result = (|| {
            let mut mutation_executed = plist_changed;
            if existing.is_some() {
                let (state, state_audit) = self.query_observed_state(definition)?;
                audit.extend(state_audit);
                observed = state;
            }
            if observed == LaunchdObservedState::NotLoaded {
                audit.extend(self.run_bootstrap()?);
                mutation_executed = true;
            }
            let (stable, state_audit) = self.wait_for_stable(definition)?;
            audit.extend(state_audit);
            let state = match stable {
                LaunchdObservedState::Running => ServiceManagerState::Running,
                LaunchdObservedState::LoadedStopped if definition.start_on_boot => {
                    audit.extend(self.run_kickstart(false)?);
                    audit.extend(self.wait_for_running(definition)?);
                    mutation_executed = true;
                    ServiceManagerState::Running
                }
                LaunchdObservedState::LoadedStopped => ServiceManagerState::Stopped,
                LaunchdObservedState::NotLoaded => {
                    return Err(EvaError::conflict(
                        "launchd service did not remain bootstrapped after install",
                    ));
                }
                LaunchdObservedState::Transitional => unreachable!(),
            };
            Ok(self.mutation_report(
                definition,
                ServiceManagerOperation::Install,
                state,
                mutation_executed,
                audit,
            ))
        })();
        match result {
            Err(install_error) if created_plist => {
                let install_error_kind = format!("{:?}", install_error.kind());
                match self.uninstall(ServiceManagerMutationRequest { definition }) {
                    Ok(_) => Err(install_error),
                    Err(cleanup_error) => Err(cleanup_error
                        .with_context("rollback_operation", "launchd_install")
                        .with_context("install_error_kind", install_error_kind)),
                }
            }
            result => result,
        }
    }

    fn uninstall(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        self.validate_definition(definition, true)?;
        let mut audit = self.audit_identity();
        let local = self.read_plist()?;
        if let Some(bytes) = local.as_ref() {
            self.validate_managed_plist_bytes(definition, bytes)?;
            audit.push(format!("launchd.plist.digest:{}", digest_bytes(bytes)));
        }
        let (observed, state_audit) = self.wait_for_stable(definition)?;
        audit.extend(state_audit);
        if local.is_none() && observed == LaunchdObservedState::NotLoaded {
            return Ok(self.mutation_report(
                definition,
                ServiceManagerOperation::Uninstall,
                ServiceManagerState::NotInstalled,
                false,
                audit,
            ));
        }
        if local.is_none() {
            return Err(EvaError::conflict(
                "launchd service is registered without a managed plist",
            ));
        }
        if observed != LaunchdObservedState::NotLoaded {
            audit.extend(self.run_bootout()?);
            audit.extend(self.wait_for_not_loaded(definition)?);
        }
        let tombstone = move_plist_to_tombstone(&self.plist_root, &self.plist_path())?;
        let verify = self.query_observed_state(definition);
        let (after, verify_audit) = match verify {
            Ok(value) => value,
            Err(error) => {
                let error_kind = format!("{:?}", error.kind());
                return match self.restore_failed_uninstall(definition, &tombstone, observed) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error
                        .with_context("recovery_operation", "launchd_uninstall")
                        .with_context("uninstall_error_kind", error_kind)),
                };
            }
        };
        audit.extend(verify_audit);
        if after != LaunchdObservedState::NotLoaded {
            let error = EvaError::conflict("launchd service remained registered after uninstall");
            let error_kind = format!("{:?}", error.kind());
            return match self.restore_failed_uninstall(definition, &tombstone, observed) {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(recovery_error
                    .with_context("recovery_operation", "launchd_uninstall")
                    .with_context("uninstall_error_kind", error_kind)),
            };
        }
        if let Err(remove_error) = fs::remove_file(&tombstone) {
            let error = io_error(
                "failed to remove launchd plist tombstone",
                "plist",
                remove_error,
            );
            let error_kind = format!("{:?}", error.kind());
            return match self.restore_failed_uninstall(definition, &tombstone, observed) {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(recovery_error
                    .with_context("recovery_operation", "launchd_uninstall")
                    .with_context("uninstall_error_kind", error_kind)),
            };
        }
        sync_directory(&self.plist_root)?;
        audit.push("launchd.plist.removed:true".to_owned());
        Ok(self.mutation_report(
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
        self.validate_definition(definition, false)?;
        let mut audit = self.audit_identity();
        let local = self.read_plist()?;
        if let Some(bytes) = local.as_ref() {
            self.validate_managed_plist_bytes(definition, bytes)?;
            audit.push(format!("launchd.plist.digest:{}", digest_bytes(bytes)));
        }
        let (observed, state_audit) = self.query_observed_state(definition)?;
        audit.extend(state_audit);
        let state = match (local.is_some(), observed) {
            (false, LaunchdObservedState::NotLoaded) => ServiceManagerState::NotInstalled,
            (false, _) => {
                return Err(EvaError::conflict(
                    "launchd service is registered without a managed plist",
                ));
            }
            (true, LaunchdObservedState::Running) => ServiceManagerState::Running,
            (true, LaunchdObservedState::LoadedStopped | LaunchdObservedState::NotLoaded) => {
                ServiceManagerState::Stopped
            }
            (true, LaunchdObservedState::Transitional) => {
                return Err(EvaError::unavailable(
                    "launchd service is in a transitional state",
                ));
            }
        };
        Ok(self.status_report(definition, state, audit))
    }

    fn start(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        let definition = request.definition;
        self.validate_definition(definition, true)?;
        let mut audit = self.audit_identity();
        let plist = self.ensure_managed_plist(definition).map_err(|error| {
            if error.kind() == eva_core::ErrorKind::NotFound {
                EvaError::not_found("launchd service is not installed")
            } else {
                error
            }
        })?;
        audit.push(format!("launchd.plist.digest:{}", digest_bytes(&plist)));
        let (mut observed, state_audit) = self.wait_for_stable(definition)?;
        audit.extend(state_audit);
        if observed == LaunchdObservedState::Running {
            return Ok(self.mutation_report(
                definition,
                ServiceManagerOperation::Start,
                ServiceManagerState::Running,
                false,
                audit,
            ));
        }
        if observed == LaunchdObservedState::NotLoaded {
            audit.extend(self.run_bootstrap()?);
            let (stable, bootstrap_audit) = self.wait_for_stable(definition)?;
            audit.extend(bootstrap_audit);
            observed = stable;
            if observed == LaunchdObservedState::Running {
                return Ok(self.mutation_report(
                    definition,
                    ServiceManagerOperation::Start,
                    ServiceManagerState::Running,
                    true,
                    audit,
                ));
            }
        }
        audit.extend(self.run_kickstart(false)?);
        audit.extend(self.wait_for_running(definition)?);
        Ok(self.mutation_report(
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
        self.validate_definition(definition, true)?;
        let mut audit = self.audit_identity();
        let local = self.read_plist()?;
        if let Some(bytes) = local.as_ref() {
            self.validate_managed_plist_bytes(definition, bytes)?;
            audit.push(format!("launchd.plist.digest:{}", digest_bytes(bytes)));
        }
        let (observed, state_audit) = self.wait_for_stable(definition)?;
        audit.extend(state_audit);
        if local.is_none() && observed == LaunchdObservedState::NotLoaded {
            return Ok(self.mutation_report(
                definition,
                ServiceManagerOperation::Stop,
                ServiceManagerState::NotInstalled,
                false,
                audit,
            ));
        }
        if local.is_none() {
            return Err(EvaError::conflict(
                "launchd service is registered without a managed plist",
            ));
        }
        if observed == LaunchdObservedState::NotLoaded {
            return Ok(self.mutation_report(
                definition,
                ServiceManagerOperation::Stop,
                ServiceManagerState::Stopped,
                false,
                audit,
            ));
        }
        if observed == LaunchdObservedState::LoadedStopped {
            return Ok(self.mutation_report(
                definition,
                ServiceManagerOperation::Stop,
                ServiceManagerState::Stopped,
                false,
                audit,
            ));
        }
        audit.extend(self.run_bootout()?);
        audit.extend(self.wait_for_not_loaded(definition)?);
        Ok(self.mutation_report(
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
        self.validate_definition(definition, true)?;
        let mut audit = self.audit_identity();
        let plist = self.ensure_managed_plist(definition).map_err(|error| {
            if error.kind() == eva_core::ErrorKind::NotFound {
                EvaError::not_found("launchd service is not installed")
            } else {
                error
            }
        })?;
        audit.push(format!("launchd.plist.digest:{}", digest_bytes(&plist)));
        let (observed, state_audit) = self.wait_for_stable(definition)?;
        audit.extend(state_audit);
        if observed == LaunchdObservedState::NotLoaded {
            audit.extend(self.run_bootstrap()?);
            let (_, bootstrap_audit) = self.wait_for_stable(definition)?;
            audit.extend(bootstrap_audit);
        }
        audit.extend(self.run_kickstart(true)?);
        audit.extend(self.wait_for_running(definition)?);
        Ok(self.mutation_report(
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
        self.validate_definition(request.definition, true)?;
        Err(EvaError::unsupported(
            "launchd generation handoff is not implemented yet",
        ))
    }

    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError> {
        self.validate_definition(request.definition, true)?;
        Err(EvaError::unsupported(
            "launchd generation rollback is not implemented yet",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdDomainSelector {
    System,
    Gui,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchdIdentity {
    selector: LaunchdDomainSelector,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdObservedState {
    NotLoaded,
    LoadedStopped,
    Running,
    Transitional,
}

impl LaunchdObservedState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotLoaded => "not_loaded",
            Self::LoadedStopped => "loaded_stopped",
            Self::Running => "running",
            Self::Transitional => "transitional",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdWaitTarget {
    Running,
    NotLoaded,
    LoadedStopped,
}

impl LaunchdWaitTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NotLoaded => "not_loaded",
            Self::LoadedStopped => "loaded_stopped",
        }
    }
}

fn validate_launchd_definition(
    definition: &ServiceManagerDefinition,
    mutation: bool,
) -> Result<LaunchdIdentity, EvaError> {
    if definition.kind != ServiceManagerKind::Launchd {
        return Err(
            EvaError::unsupported("launchd adapter requires launchd kind")
                .with_context("requested_kind", definition.kind.as_str()),
        );
    }
    if mutation && !definition.enabled {
        return Err(EvaError::invalid_argument(
            "launchd service mutation requires an enabled definition",
        ));
    }
    if definition.service_name.trim().is_empty() {
        return Err(EvaError::invalid_argument(
            "launchd service_name cannot be empty",
        ));
    }
    if !definition.start_on_boot && definition.restart_supervisor {
        return Err(EvaError::invalid_argument(
            "launchd KeepAlive cannot be enabled when RunAtLoad is disabled",
        ));
    }
    let unit_name = definition.unit_name.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("launchd unit_name must use system/<label> or gui/<label>")
    })?;
    let (domain, label) = unit_name.split_once('/').ok_or_else(|| {
        EvaError::invalid_argument("launchd unit_name must use system/<label> or gui/<label>")
    })?;
    if label.contains('/') {
        return Err(EvaError::invalid_argument(
            "launchd unit_name contains too many domain separators",
        ));
    }
    validate_launchd_label(label)?;
    let selector = match domain {
        "system" => LaunchdDomainSelector::System,
        "gui" => LaunchdDomainSelector::Gui,
        _ => {
            return Err(EvaError::invalid_argument(
                "launchd unit_name has an unsupported domain",
            ));
        }
    };
    Ok(LaunchdIdentity {
        selector,
        label: label.to_owned(),
    })
}

fn validate_launchd_label(label: &str) -> Result<(), EvaError> {
    if label.is_empty()
        || label.len() > 240
        || !label
            .as_bytes()
            .first()
            .is_some_and(|value| value.is_ascii_alphanumeric())
        || label.contains("..")
        || label.chars().any(|character| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-')
        })
    {
        return Err(EvaError::invalid_argument("launchd label is invalid"));
    }
    Ok(())
}

fn parse_launchctl_print(
    report: &ServiceCommandReport,
    expected_target: &str,
    expected_plist_path: &str,
    expected_program: Option<&str>,
) -> Result<LaunchdObservedState, EvaError> {
    let output = String::from_utf8(report.stdout.bytes().to_vec())
        .map_err(|_| EvaError::conflict("launchctl print output is not valid UTF-8"))?;
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| EvaError::conflict("launchctl print output is empty"))?;
    if header.trim() != format!("{expected_target} = {{") {
        return Err(EvaError::conflict(
            "launchctl print target does not match the requested service",
        ));
    }
    let mut depth = 1_u32;
    let mut state = None;
    let mut pid = None;
    let mut path = None;
    let mut program = None;
    for line in lines {
        let trimmed = line.trim();
        if depth == 0 {
            return Err(EvaError::conflict(
                "launchctl print contains trailing content after the service object",
            ));
        }
        if trimmed.ends_with(" = {") || trimmed == "{" {
            depth = depth
                .checked_add(1)
                .ok_or_else(|| EvaError::conflict("launchctl print nesting overflow"))?;
            continue;
        }
        if trimmed == "}" {
            depth = depth
                .checked_sub(1)
                .ok_or_else(|| EvaError::conflict("launchctl print nesting underflow"))?;
            continue;
        }
        if depth != 1 {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(" = ") else {
            continue;
        };
        match key {
            "state" if state.is_none() => state = Some(value),
            "pid" if pid.is_none() => pid = Some(value),
            "path" if path.is_none() => path = Some(value),
            "program" if program.is_none() => program = Some(value),
            "state" | "pid" | "path" | "program" => {
                return Err(EvaError::conflict(
                    "launchctl print repeats a top-level field",
                ));
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(EvaError::conflict("launchctl print braces are unbalanced"));
    }
    let path = path
        .ok_or_else(|| EvaError::conflict("launchctl print is missing the top-level plist path"))?;
    if path != expected_plist_path {
        return Err(EvaError::conflict(
            "launchctl registered plist path does not match definition",
        ));
    }
    if let Some(expected_program) = expected_program {
        let program = program.ok_or_else(|| {
            EvaError::conflict("launchctl print is missing the top-level program")
        })?;
        if program != expected_program {
            return Err(EvaError::conflict(
                "launchctl registered program does not match definition",
            ));
        }
    }
    match state
        .ok_or_else(|| EvaError::conflict("launchctl print is missing the top-level state"))?
    {
        "running" => {
            let pid = pid
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid > 0)
                .ok_or_else(|| EvaError::conflict("running launchd service has no positive pid"))?;
            let _ = pid;
            Ok(LaunchdObservedState::Running)
        }
        "waiting" | "exited" | "stopped" | "not running" => Ok(LaunchdObservedState::LoadedStopped),
        "spawn scheduled" | "starting" => Ok(LaunchdObservedState::Transitional),
        value => Err(EvaError::conflict("launchctl returned an unknown state")
            .with_context("launchd_state", value.to_owned())),
    }
}

fn atomic_create_plist(
    root: &Path,
    target: &Path,
    bytes: &[u8],
    expected_uid: u32,
    expected_gid: u32,
    enforce_native_permissions: bool,
) -> Result<(), EvaError> {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .ok_or_else(|| EvaError::invalid_argument("launchd plist has no file name"))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".eva-tmp-{}-{counter}", std::process::id()));
    let temp_path = root.join(temp_name);
    let mut published = false;
    #[cfg(unix)]
    let mut published_identity = None;
    let result: Result<(), EvaError> = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut file = options.open(&temp_path).map_err(|error| {
            io_error("failed to create temporary launchd plist", "plist", error)
        })?;
        file.write_all(bytes)
            .map_err(|error| io_error("failed to write temporary launchd plist", "plist", error))?;
        file.flush()
            .map_err(|error| io_error("failed to flush temporary launchd plist", "plist", error))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644)).map_err(
                |error| io_error("failed to set launchd plist permissions", "plist", error),
            )?;
            if enforce_native_permissions {
                let result = unsafe {
                    libc::fchown(
                        file.as_raw_fd(),
                        expected_uid as libc::uid_t,
                        expected_gid as libc::gid_t,
                    )
                };
                if result != 0 {
                    return Err(io_error(
                        "failed to set launchd plist owner",
                        "plist",
                        std::io::Error::last_os_error(),
                    ));
                }
            }
        }
        #[cfg(not(unix))]
        let _ = (expected_uid, expected_gid, enforce_native_permissions);
        file.sync_all()
            .map_err(|error| io_error("failed to sync temporary launchd plist", "plist", error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata().map_err(|error| {
                io_error("failed to identify temporary launchd plist", "plist", error)
            })?;
            published_identity = Some((metadata.dev(), metadata.ino()));
        }
        fs::hard_link(&temp_path, target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                EvaError::conflict("launchd plist appeared during atomic publication")
            } else {
                io_error("failed to publish launchd plist", "plist", error)
            }
        })?;
        published = true;
        sync_directory(root)?;
        fs::remove_file(&temp_path).map_err(|error| {
            io_error("failed to remove temporary launchd plist", "plist", error)
        })?;
        sync_directory(root)?;
        Ok(())
    })();
    if let Err(error) = result {
        let error_kind = format!("{:?}", error.kind());
        let temp_cleanup_error = match fs::remove_file(&temp_path) {
            Ok(()) => None,
            Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => None,
            Err(cleanup_error) => Some(io_error(
                "failed to clean temporary launchd plist after publication error",
                "plist",
                cleanup_error,
            )),
        };
        let rollback_error = if published {
            #[cfg(unix)]
            {
                let (device, inode) = published_identity.ok_or_else(|| {
                    EvaError::internal("published launchd plist has no file identity")
                })?;
                rollback_published_plist(root, target, device, inode).err()
            }
            #[cfg(not(unix))]
            {
                rollback_published_plist(root, target, bytes).err()
            }
        } else {
            None
        };
        if let Some(rollback_error) = rollback_error {
            return Err(rollback_error
                .with_context("rollback_operation", "launchd_plist_publication")
                .with_context("publication_error_kind", error_kind));
        }
        if let Some(temp_cleanup_error) = temp_cleanup_error {
            return Err(temp_cleanup_error
                .with_context("rollback_operation", "launchd_plist_publication")
                .with_context("publication_error_kind", error_kind));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rollback_published_plist(
    root: &Path,
    target: &Path,
    expected_device: u64,
    expected_inode: u64,
) -> Result<(), EvaError> {
    use std::os::unix::fs::MetadataExt;

    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .ok_or_else(|| EvaError::invalid_argument("launchd plist has no file name"))?;
    let mut quarantine_name = OsString::from(".");
    quarantine_name.push(file_name);
    quarantine_name.push(format!(".eva-rollback-{}-{counter}", std::process::id()));
    let quarantine = root.join(quarantine_name);
    match rename_exclusive(
        target,
        &quarantine,
        "failed to quarantine published launchd plist",
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == eva_core::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(&quarantine).map_err(|error| {
        io_error(
            "failed to identify quarantined launchd plist for rollback",
            "plist",
            error,
        )
    })?;
    if metadata.dev() != expected_device || metadata.ino() != expected_inode {
        let conflict = EvaError::conflict("published launchd plist changed before rollback");
        return match rename_exclusive(
            &quarantine,
            target,
            "failed to restore changed launchd plist after rollback check",
        ) {
            Ok(()) => Err(conflict),
            Err(recovery_error) => Err(recovery_error
                .with_context("rollback_operation", "launchd_plist_quarantine")
                .with_context("rollback_error_kind", "Conflict")),
        };
    }
    fs::remove_file(&quarantine).map_err(|error| {
        io_error(
            "failed to roll back published launchd plist",
            "plist",
            error,
        )
    })?;
    sync_directory(root)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn rollback_published_plist(
    root: &Path,
    target: &Path,
    expected_device: u64,
    expected_inode: u64,
) -> Result<(), EvaError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let file = match options.open(target) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(io_error(
                "failed to open published launchd plist for rollback",
                "plist",
                error,
            ));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        io_error(
            "failed to identify published launchd plist for rollback",
            "plist",
            error,
        )
    })?;
    if metadata.dev() != expected_device || metadata.ino() != expected_inode {
        return Err(EvaError::conflict(
            "published launchd plist changed before rollback",
        ));
    }
    fs::remove_file(target).map_err(|error| {
        io_error(
            "failed to roll back published launchd plist",
            "plist",
            error,
        )
    })?;
    sync_directory(root)
}

#[cfg(not(unix))]
fn rollback_published_plist(root: &Path, target: &Path, expected: &[u8]) -> Result<(), EvaError> {
    let current = match fs::read(target) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(io_error(
                "failed to read published launchd plist for rollback",
                "plist",
                error,
            ));
        }
    };
    if current != expected {
        return Err(EvaError::conflict(
            "published launchd plist changed before rollback",
        ));
    }
    fs::remove_file(target).map_err(|error| {
        io_error(
            "failed to roll back published launchd plist",
            "plist",
            error,
        )
    })?;
    sync_directory(root)
}

fn move_plist_to_tombstone(root: &Path, target: &Path) -> Result<PathBuf, EvaError> {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .ok_or_else(|| EvaError::invalid_argument("launchd plist has no file name"))?;
    let mut tombstone_name = OsString::from(".");
    tombstone_name.push(file_name);
    tombstone_name.push(format!(".eva-delete-{}-{counter}", std::process::id()));
    let tombstone = root.join(tombstone_name);
    #[cfg(target_os = "macos")]
    rename_exclusive(target, &tombstone, "failed to stage launchd plist removal")?;
    #[cfg(not(target_os = "macos"))]
    {
        fs::hard_link(target, &tombstone)
            .map_err(|error| io_error("failed to stage launchd plist removal", "plist", error))?;
        if let Err(error) = fs::remove_file(target) {
            let _ = fs::remove_file(&tombstone);
            return Err(io_error("failed to detach launchd plist", "plist", error));
        }
    }
    if let Err(error) = sync_directory(root) {
        let error_kind = format!("{:?}", error.kind());
        return match restore_tombstone(root, &tombstone, target) {
            Ok(()) => Err(error),
            Err(recovery_error) => Err(recovery_error
                .with_context("rollback_operation", "launchd_plist_tombstone")
                .with_context("publication_error_kind", error_kind)),
        };
    }
    Ok(tombstone)
}

fn restore_tombstone(root: &Path, tombstone: &Path, target: &Path) -> Result<(), EvaError> {
    #[cfg(target_os = "macos")]
    rename_exclusive(
        tombstone,
        target,
        "failed to restore launchd plist without clobbering",
    )?;
    #[cfg(not(target_os = "macos"))]
    {
        fs::hard_link(tombstone, target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                EvaError::conflict("cannot restore launchd plist because the target reappeared")
            } else {
                io_error("failed to restore launchd plist", "plist", error)
            }
        })?;
        fs::remove_file(tombstone).map_err(|error| {
            io_error(
                "failed to remove restored launchd plist tombstone",
                "plist",
                error,
            )
        })?;
    }
    sync_directory(root)
}

#[cfg(target_os = "macos")]
fn rename_exclusive(from: &Path, to: &Path, message: &'static str) -> Result<(), EvaError> {
    use std::os::unix::ffi::OsStrExt;

    let from = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| EvaError::invalid_argument("launchd path contains NUL"))?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| EvaError::invalid_argument("launchd path contains NUL"))?;
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io_error(message, "plist", std::io::Error::last_os_error()))
    }
}

fn validate_native_owner(
    metadata: &fs::Metadata,
    expected_uid: u32,
    expected_gid: u32,
    resource: &'static str,
) -> Result<(), EvaError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != expected_uid || metadata.gid() != expected_gid {
            return Err(EvaError::permission_denied(
                "launchd resource owner does not match the captured domain",
            )
            .with_context("resource", resource));
        }
        if metadata.mode() & 0o022 != 0 {
            return Err(
                EvaError::conflict("launchd resource is writable by group or other users")
                    .with_context("resource", resource),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, expected_uid, expected_gid, resource);
    Ok(())
}

fn launchctl_executable() -> Result<PathBuf, EvaError> {
    let candidate = Path::new("/bin/launchctl");
    let resolved = fs::canonicalize(candidate)
        .map_err(|error| io_error("trusted launchctl was not found", "launchctl", error))?;
    let metadata = fs::metadata(&resolved)
        .map_err(|error| io_error("failed to inspect launchctl", "launchctl", error))?;
    if !metadata.is_file() {
        return Err(EvaError::conflict(
            "trusted launchctl is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(EvaError::permission_denied(
                "trusted launchctl ownership or permissions are invalid",
            ));
        }
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(EvaError::permission_denied(
                "trusted launchctl is not executable",
            ));
        }
    }
    Ok(resolved)
}

#[cfg(target_os = "macos")]
fn current_gui_user() -> Result<(u32, u32, PathBuf), EvaError> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStringExt;

    let uid = unsafe { libc::geteuid() };
    let mut buffer_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    if buffer_size < 1024 {
        buffer_size = 16 * 1024;
    }
    loop {
        let mut record = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; buffer_size as usize];
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                &mut record,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE {
            buffer_size = buffer_size.saturating_mul(2);
            if buffer_size > 1024 * 1024 {
                return Err(EvaError::conflict(
                    "launchd passwd record exceeds its bound",
                ));
            }
            continue;
        }
        if code != 0 {
            return Err(io_error(
                "failed to resolve launchd GUI user",
                "passwd",
                std::io::Error::from_raw_os_error(code),
            ));
        }
        if result.is_null() || record.pw_dir.is_null() {
            return Err(EvaError::not_found(
                "launchd GUI user passwd record was not found",
            ));
        }
        let home = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes().to_vec();
        if home.is_empty() {
            return Err(EvaError::conflict(
                "launchd GUI user home directory is empty",
            ));
        }
        let home = PathBuf::from(OsString::from_vec(home));
        if !home.is_absolute() {
            return Err(EvaError::conflict(
                "launchd GUI user home directory must be absolute",
            ));
        }
        return Ok((uid, record.pw_gid, home));
    }
}

#[cfg(not(target_os = "macos"))]
fn current_gui_user() -> Result<(u32, u32, PathBuf), EvaError> {
    Err(EvaError::unsupported(
        "launchd GUI identity is unavailable on this host",
    ))
}

fn ensure_process_completion(
    report: &ServiceCommandReport,
    action: &'static str,
) -> Result<(), EvaError> {
    match report.termination {
        ServiceCommandTermination::Exited => Ok(()),
        ServiceCommandTermination::TimedOut => {
            Err(EvaError::timeout("launchctl command timed out").with_context("action", action))
        }
        ServiceCommandTermination::OutputLimitExceeded => Err(EvaError::conflict(
            "launchctl command output exceeded its limit",
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
    let errno = launchctl_errno(report);
    let text = String::from_utf8_lossy(report.stderr.bytes()).to_ascii_lowercase();
    let mut error = if matches!(errno, Some(1 | 13))
        || exit_code == 13
        || text.contains("operation not permitted")
        || text.contains("permission denied")
        || text.contains("not privileged")
    {
        EvaError::permission_denied("launchd denied the requested operation")
    } else if matches!(errno, Some(3))
        || (errno.is_none() && LAUNCHCTL_MISSING_EXIT_CODES.contains(&exit_code))
    {
        EvaError::not_found("launchd service is not loaded")
    } else if matches!(errno, Some(17)) || exit_code == 17 {
        EvaError::conflict("launchd service already exists")
    } else {
        EvaError::unavailable("launchctl command failed")
    };
    error = error.with_context("action", action);
    error = error.with_context("exit_code", exit_code.to_string());
    if let Some(errno) = errno {
        error = error.with_context("launchd_errno", errno.to_string());
    }
    error
}

fn launchctl_errno(report: &ServiceCommandReport) -> Option<i32> {
    let stderr = String::from_utf8_lossy(report.stderr.bytes());
    stderr.lines().find_map(|line| {
        let (_, suffix) = line.split_once("failed:")?;
        let digits = suffix
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        (!digits.is_empty())
            .then(|| digits.parse::<i32>().ok())
            .flatten()
    })
}

fn adapter_command_audit(report: &ServiceCommandReport, action: &'static str) -> Vec<String> {
    let mut audit = report.audit.clone();
    audit.push(format!("launchd.command:{action}"));
    audit
}

fn require_utf8_path<'a>(path: &'a Path, name: &'static str) -> Result<&'a str, EvaError> {
    path.to_str()
        .ok_or_else(|| EvaError::invalid_argument(format!("{name} must be valid UTF-8")))
}

fn validate_xml_text(value: &str, name: &'static str) -> Result<(), EvaError> {
    if value.chars().any(|character| {
        !matches!(character, '\u{9}' | '\u{A}' | '\u{D}')
            && (character < '\u{20}' || character == '\u{FFFE}' || character == '\u{FFFF}')
    }) {
        return Err(EvaError::invalid_argument(format!(
            "{name} contains a character forbidden by XML 1.0"
        )));
    }
    Ok(())
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

const fn xml_bool(value: bool) -> &'static str {
    if value {
        "<true/>"
    } else {
        "<false/>"
    }
}

fn append_digest_field(canonical: &mut Vec<u8>, value: &[u8]) {
    canonical.extend_from_slice(&(value.len() as u64).to_be_bytes());
    canonical.extend_from_slice(value);
}

fn digest_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn io_error(message: &'static str, context_key: &'static str, error: std::io::Error) -> EvaError {
    let value = match error.kind() {
        std::io::ErrorKind::PermissionDenied => EvaError::permission_denied(message),
        std::io::ErrorKind::NotFound => EvaError::not_found(message),
        std::io::ErrorKind::AlreadyExists => EvaError::conflict(message),
        _ => EvaError::unavailable(message),
    };
    value.with_context(context_key, error.kind().to_string())
}

fn sync_directory(path: &Path) -> Result<(), EvaError> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                io_error(
                    "failed to sync launchd plist directory",
                    "plist_root",
                    error,
                )
            })?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn sleep_poll_interval(interval: Duration) {
    if interval.is_zero() {
        std::thread::yield_now();
    } else {
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_command::{
        ServiceCommandArgVisibility, ServiceCommandExecution, ValidatedServiceCommandTarget,
    };
    use eva_core::ErrorKind;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_SERVICE_NAME: &str = "EvaCliLaunchdTest";
    const TEST_LABEL: &str = "com.eva-cli.launchd-test";
    const TEST_GUI_UID: u32 = 501;

    #[derive(Clone)]
    struct ExpectedArg {
        value: OsString,
        visibility: ServiceCommandArgVisibility,
    }

    #[derive(Clone)]
    struct ScriptStep {
        arguments: Vec<ExpectedArg>,
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
            assert_eq!(state.remaining.len(), 0, "unconsumed launchctl commands");
        }

        fn call_count(&self) -> usize {
            self.state.lock().expect("script lock").call_count
        }

        fn command_debug(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("script lock")
                .command_debug
                .clone()
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
                .expect("unexpected launchctl command");
            assert_eq!(target.kind(), ServiceManagerKind::Launchd);
            assert_eq!(command.executable(), Path::new("launchctl"));
            assert_eq!(command.arguments().len(), step.arguments.len());
            for (actual, expected) in command.arguments().iter().zip(&step.arguments) {
                assert_eq!(actual.value(), expected.value.as_os_str());
                assert_eq!(actual.visibility(), expected.visibility);
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
                .join(format!("eva-launchd-test-{}-{nonce}", std::process::id()));
            fs::create_dir(&path).expect("temporary launchd plist root");
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

    fn public(value: impl Into<OsString>) -> ExpectedArg {
        ExpectedArg {
            value: value.into(),
            visibility: ServiceCommandArgVisibility::Public,
        }
    }

    fn secret(value: impl Into<OsString>) -> ExpectedArg {
        ExpectedArg {
            value: value.into(),
            visibility: ServiceCommandArgVisibility::Secret,
        }
    }

    fn scripted_step(
        arguments: Vec<ExpectedArg>,
        exit_code: i32,
        stdout: impl Into<Vec<u8>>,
        stderr: impl Into<Vec<u8>>,
    ) -> ScriptStep {
        ScriptStep {
            arguments,
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn domain_target(domain: LaunchdDomain) -> String {
        domain.target()
    }

    fn service_target(domain: LaunchdDomain) -> String {
        format!("{}/{}", domain.target(), TEST_LABEL)
    }

    fn plist_path(root: &Path) -> PathBuf {
        root.join(format!("{TEST_LABEL}.plist"))
    }

    fn missing_print_step(domain: LaunchdDomain) -> ScriptStep {
        scripted_step(
            vec![public("print"), public(service_target(domain))],
            113,
            Vec::new(),
            b"Could not find service\n".to_vec(),
        )
    }

    fn print_step(root: &Path, domain: LaunchdDomain, state: &str, pid: Option<u32>) -> ScriptStep {
        registered_print_step(root, domain, state, pid, &plist_path(root), &test_binary())
    }

    fn registered_print_step(
        root: &Path,
        domain: LaunchdDomain,
        state: &str,
        pid: Option<u32>,
        registered_path: &Path,
        registered_program: &Path,
    ) -> ScriptStep {
        let target = service_target(domain);
        let mut stdout = format!(
            "{target} = {{\n\tpath = {}\n\tstate = {state}\n\tprogram = {}\n",
            registered_path.to_string_lossy(),
            registered_program.to_string_lossy(),
        );
        if let Some(pid) = pid {
            stdout.push_str(&format!("\tpid = {pid}\n"));
        }
        stdout.push_str("}\n");
        let _ = root;
        scripted_step(
            vec![public("print"), public(target)],
            0,
            stdout.into_bytes(),
            Vec::new(),
        )
    }

    fn bootstrap_step(root: &Path, domain: LaunchdDomain) -> ScriptStep {
        scripted_step(
            vec![
                public("bootstrap"),
                public(domain_target(domain)),
                secret(plist_path(root).into_os_string()),
            ],
            0,
            Vec::new(),
            Vec::new(),
        )
    }

    fn bootout_step(domain: LaunchdDomain) -> ScriptStep {
        scripted_step(
            vec![public("bootout"), public(service_target(domain))],
            0,
            Vec::new(),
            Vec::new(),
        )
    }

    fn kickstart_step(domain: LaunchdDomain, restart: bool) -> ScriptStep {
        let mut arguments = vec![public("kickstart")];
        if restart {
            arguments.push(public("-k"));
        }
        arguments.push(public(service_target(domain)));
        scripted_step(arguments, 0, Vec::new(), Vec::new())
    }

    fn kill_step(domain: LaunchdDomain) -> ScriptStep {
        scripted_step(
            vec![
                public("kill"),
                public("SIGTERM"),
                public(service_target(domain)),
            ],
            0,
            Vec::new(),
            Vec::new(),
        )
    }

    fn test_binary() -> PathBuf {
        fs::canonicalize(std::env::current_exe().expect("current test executable"))
            .expect("canonical test executable")
    }

    fn definition(selector: &str) -> ServiceManagerDefinition {
        ServiceManagerDefinition {
            enabled: true,
            kind: ServiceManagerKind::Launchd,
            service_name: TEST_SERVICE_NAME.to_owned(),
            unit_name: Some(format!("{selector}/{TEST_LABEL}")),
            runtime_binary: Some(test_binary()),
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

    fn write_desired_plist(
        adapter: &LaunchdAdapter<ScriptedExecutor>,
        definition: &ServiceManagerDefinition,
    ) -> Vec<u8> {
        let binary = adapter
            .install_binary(definition)
            .expect("test runtime binary");
        let desired = adapter
            .desired_plist(definition, &binary)
            .expect("desired launchd plist");
        atomic_create_plist(
            &adapter.plist_root,
            &adapter.plist_path(),
            &desired,
            adapter.expected_uid,
            adapter.expected_gid,
            false,
        )
        .expect("launchd plist fixture");
        desired
    }

    fn report(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> ServiceCommandReport {
        let command = ServiceCommand::new("launchctl", [ServiceCommandArg::public("print")])
            .expect("launchctl command");
        ServiceCommandReport::from_execution(
            ServiceManagerKind::Launchd,
            &command,
            ServiceCommandExecution::exited(Some(exit_code), stdout, stderr),
        )
        .expect("launchctl report")
    }

    #[test]
    fn system_install_is_idempotent_and_redacts_the_secret_plist_path() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([
            missing_print_step(domain),
            bootstrap_step(&root.path(), domain),
            print_step(&root.path(), domain, "running", Some(4101)),
            print_step(&root.path(), domain, "running", Some(4101)),
            print_step(&root.path(), domain, "running", Some(4101)),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");

        let first = adapter
            .install(mutation_request(&definition))
            .expect("first system install");
        let repeated = adapter
            .install(mutation_request(&definition))
            .expect("repeated system install");

        assert_eq!(first.state, ServiceManagerState::Running);
        assert!(first.mutation_executed);
        assert!(!repeated.mutation_executed);
        assert_eq!(repeated.state, ServiceManagerState::Running);
        let installed = fs::read(adapter.plist_path()).expect("installed plist");
        let expected = adapter
            .desired_plist(
                &definition,
                definition
                    .runtime_binary
                    .as_deref()
                    .expect("runtime binary"),
            )
            .expect("desired plist");
        assert_eq!(installed, expected);
        assert_eq!(
            fs::read_dir(root.path())
                .expect("plist root entries")
                .count(),
            1,
            "temporary plist files must not remain"
        );
        let secret_path = adapter.plist_path().to_string_lossy().into_owned();
        let runtime_path = definition
            .runtime_binary
            .as_ref()
            .expect("runtime binary")
            .to_string_lossy()
            .into_owned();
        let audit = format!("{}\n{}", first.audit.join("\n"), repeated.audit.join("\n"));
        assert!(!audit.contains(&secret_path));
        assert!(!audit.contains(&runtime_path));
        let debug = probe.command_debug().join("\n");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&secret_path));
        probe.assert_drained();
    }

    #[test]
    fn gui_install_uses_the_captured_uid_and_exact_domain_argv() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::Gui { uid: TEST_GUI_UID };
        let executor = ScriptedExecutor::new([
            missing_print_step(domain),
            bootstrap_step(&root.path(), domain),
            print_step(&root.path(), domain, "waiting", None),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let mut definition = definition("gui");
        definition.start_on_boot = false;

        let installed = adapter
            .install(mutation_request(&definition))
            .expect("GUI install");

        assert_eq!(installed.state, ServiceManagerState::Stopped);
        assert!(installed.mutation_executed);
        assert!(installed
            .audit
            .iter()
            .any(|entry| entry == "launchd.domain:gui"));
        assert!(installed
            .audit
            .iter()
            .any(|entry| entry == "launchd.gui_uid:501"));
        let plist = String::from_utf8(fs::read(adapter.plist_path()).expect("GUI plist"))
            .expect("UTF-8 plist");
        assert!(plist.contains("<key>RunAtLoad</key>\n  <false/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
        probe.assert_drained();
    }

    #[test]
    fn plist_xml_is_deterministic_for_every_valid_boolean_combination() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let adapter =
            LaunchdAdapter::for_test(executor, LaunchdDomain::System, TEST_LABEL, root.path());
        let binary = test_binary();
        let escaped_binary = xml_escape(
            binary
                .to_str()
                .expect("test runtime binary path must be UTF-8"),
        );

        for (start_on_boot, restart_supervisor) in [(false, false), (true, false), (true, true)] {
            let mut definition = definition("system");
            definition.start_on_boot = start_on_boot;
            definition.restart_supervisor = restart_supervisor;
            adapter
                .validate_definition(&definition, true)
                .expect("valid launchd boolean combination");
            let marker = adapter.managed_marker(&definition);
            let expected = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{marker}\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{TEST_LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{escaped_binary}</string>\n  </array>\n  <key>RunAtLoad</key>\n  {}\n  <key>KeepAlive</key>\n  {}\n  <key>ProcessType</key>\n  <string>Background</string>\n</dict>\n</plist>\n",
                xml_bool(start_on_boot),
                xml_bool(restart_supervisor),
            );
            assert_eq!(
                adapter
                    .desired_plist(&definition, &binary)
                    .expect("deterministic plist"),
                expected.into_bytes()
            );
        }

        let mut invalid = definition("system");
        invalid.start_on_boot = false;
        invalid.restart_supervisor = true;
        let error = adapter
            .validate_definition(&invalid, true)
            .expect_err("KeepAlive without RunAtLoad must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(probe.call_count(), 0);
    }

    #[test]
    fn plist_xml_escaping_and_character_validation_fail_closed() {
        assert_eq!(xml_escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
        for invalid in ["nul\0", "control\u{1}", "noncharacter\u{fffe}"] {
            assert_eq!(
                validate_xml_text(invalid, "test value")
                    .expect_err("invalid XML scalar")
                    .kind(),
                ErrorKind::InvalidArgument
            );
        }
        for valid in ["tab\tline\nreturn\r", "scalar\u{10000}"] {
            validate_xml_text(valid, "test value").expect("valid XML scalar");
        }
    }

    #[test]
    fn start_restart_and_stop_use_exact_idempotent_transitions() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([
            print_step(&root.path(), domain, "waiting", None),
            kickstart_step(domain, false),
            print_step(&root.path(), domain, "running", Some(4201)),
            print_step(&root.path(), domain, "running", Some(4201)),
            kickstart_step(domain, true),
            print_step(&root.path(), domain, "running", Some(4202)),
            print_step(&root.path(), domain, "running", Some(4202)),
            bootout_step(domain),
            missing_print_step(domain),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        let desired = write_desired_plist(&adapter, &definition);

        let started = adapter
            .start(mutation_request(&definition))
            .expect("start loaded service");
        assert_eq!(started.state, ServiceManagerState::Running);
        assert!(started.mutation_executed);
        let restarted = adapter
            .restart(mutation_request(&definition))
            .expect("restart loaded service");
        assert_eq!(restarted.state, ServiceManagerState::Running);
        assert!(restarted.mutation_executed);
        let stopped = adapter
            .stop(mutation_request(&definition))
            .expect("stop running service");
        assert_eq!(stopped.state, ServiceManagerState::Stopped);
        assert!(stopped.mutation_executed);
        assert_eq!(
            fs::read(adapter.plist_path()).expect("retained plist"),
            desired
        );
        probe.assert_drained();
    }

    #[test]
    fn unloaded_start_bootstraps_before_kickstart() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::Gui { uid: TEST_GUI_UID };
        let executor = ScriptedExecutor::new([
            missing_print_step(domain),
            bootstrap_step(&root.path(), domain),
            print_step(&root.path(), domain, "waiting", None),
            kickstart_step(domain, false),
            print_step(&root.path(), domain, "running", Some(4301)),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let mut definition = definition("gui");
        definition.start_on_boot = false;
        write_desired_plist(&adapter, &definition);

        let report = adapter
            .start(mutation_request(&definition))
            .expect("start unloaded GUI service");

        assert_eq!(report.state, ServiceManagerState::Running);
        assert!(report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn stop_is_a_noop_for_an_already_loaded_stopped_job() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([print_step(&root.path(), domain, "waiting", None)]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        let desired = write_desired_plist(&adapter, &definition);

        let report = adapter
            .stop(mutation_request(&definition))
            .expect("idempotent stop");

        assert_eq!(report.state, ServiceManagerState::Stopped);
        assert!(!report.mutation_executed);
        assert_eq!(
            fs::read(adapter.plist_path()).expect("retained plist"),
            desired
        );
        probe.assert_drained();
    }

    #[test]
    fn repeated_install_kickstarts_a_loaded_stopped_run_at_load_job() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([
            print_step(&root.path(), domain, "waiting", None),
            print_step(&root.path(), domain, "waiting", None),
            kickstart_step(domain, false),
            print_step(&root.path(), domain, "running", Some(4401)),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        write_desired_plist(&adapter, &definition);

        let report = adapter
            .install(mutation_request(&definition))
            .expect("converge repeated install");

        assert_eq!(report.state, ServiceManagerState::Running);
        assert!(report.mutation_executed);
        probe.assert_drained();
    }

    #[test]
    fn running_uninstall_boots_out_and_removes_only_the_managed_plist() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([
            print_step(&root.path(), domain, "running", Some(4501)),
            bootout_step(domain),
            missing_print_step(domain),
            missing_print_step(domain),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        write_desired_plist(&adapter, &definition);

        let report = adapter
            .uninstall(mutation_request(&definition))
            .expect("running uninstall");

        assert_eq!(report.state, ServiceManagerState::NotInstalled);
        assert!(report.mutation_executed);
        assert!(!adapter.plist_path().exists());
        assert_eq!(
            fs::read_dir(root.path())
                .expect("plist root entries")
                .count(),
            0,
            "uninstall must leave no tombstone"
        );
        probe.assert_drained();
    }

    #[test]
    fn missing_operation_matrix_is_stable_and_does_not_invent_mutations() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([
            missing_print_step(domain),
            missing_print_step(domain),
            missing_print_step(domain),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");

        let status = adapter
            .status(status_request(&definition))
            .expect("missing status");
        assert_eq!(status.state, ServiceManagerState::NotInstalled);
        let stopped = adapter
            .stop(mutation_request(&definition))
            .expect("missing stop");
        assert_eq!(stopped.state, ServiceManagerState::NotInstalled);
        assert!(!stopped.mutation_executed);
        let uninstalled = adapter
            .uninstall(mutation_request(&definition))
            .expect("missing uninstall");
        assert_eq!(uninstalled.state, ServiceManagerState::NotInstalled);
        assert!(!uninstalled.mutation_executed);
        for operation in [
            ServiceManagerOperation::Start,
            ServiceManagerOperation::Restart,
        ] {
            let error = match operation {
                ServiceManagerOperation::Start => adapter
                    .start(mutation_request(&definition))
                    .expect_err("missing start"),
                ServiceManagerOperation::Restart => adapter
                    .restart(mutation_request(&definition))
                    .expect_err("missing restart"),
                _ => unreachable!(),
            };
            assert_eq!(error.kind(), ErrorKind::NotFound);
        }
        assert_eq!(probe.call_count(), 3);
        probe.assert_drained();
    }

    #[test]
    fn failed_first_install_removes_the_new_plist_and_registration() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let failed_bootstrap = scripted_step(
            vec![
                public("bootstrap"),
                public(domain_target(domain)),
                secret(plist_path(&root.path()).into_os_string()),
            ],
            13,
            Vec::new(),
            b"Not privileged\n".to_vec(),
        );
        let executor = ScriptedExecutor::new([
            missing_print_step(domain),
            failed_bootstrap,
            missing_print_step(domain),
            missing_print_step(domain),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");

        let error = adapter
            .install(mutation_request(&definition))
            .expect_err("bootstrap denial");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(!adapter.plist_path().exists());
        assert_eq!(fs::read_dir(root.path()).expect("plist root").count(), 0);
        probe.assert_drained();
    }

    #[test]
    fn failed_uninstall_verification_restores_the_plist_and_running_registration() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let denied_verify = scripted_step(
            vec![public("print"), public(service_target(domain))],
            3,
            Vec::new(),
            b"Not privileged\n".to_vec(),
        );
        let executor = ScriptedExecutor::new([
            print_step(&root.path(), domain, "running", Some(4601)),
            bootout_step(domain),
            missing_print_step(domain),
            denied_verify,
            missing_print_step(domain),
            bootstrap_step(&root.path(), domain),
            print_step(&root.path(), domain, "running", Some(4602)),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        let desired = write_desired_plist(&adapter, &definition);

        let error = adapter
            .uninstall(mutation_request(&definition))
            .expect_err("verification denial");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read(adapter.plist_path()).expect("restored plist"),
            desired
        );
        assert_eq!(fs::read_dir(root.path()).expect("plist root").count(), 1);
        probe.assert_drained();
    }

    #[test]
    fn failed_uninstall_restores_a_loaded_stopped_registration_exactly() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let denied_verify = scripted_step(
            vec![public("print"), public(service_target(domain))],
            3,
            Vec::new(),
            b"Not privileged\n".to_vec(),
        );
        let executor = ScriptedExecutor::new([
            print_step(&root.path(), domain, "waiting", None),
            bootout_step(domain),
            missing_print_step(domain),
            denied_verify,
            missing_print_step(domain),
            bootstrap_step(&root.path(), domain),
            print_step(&root.path(), domain, "running", Some(4651)),
            kill_step(domain),
            print_step(&root.path(), domain, "waiting", None),
        ]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        let desired = write_desired_plist(&adapter, &definition);

        let error = adapter
            .uninstall(mutation_request(&definition))
            .expect_err("verification denial");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read(adapter.plist_path()).expect("restored plist"),
            desired
        );
        assert_eq!(fs::read_dir(root.path()).expect("plist root").count(), 1);
        probe.assert_drained();
    }

    #[test]
    fn status_maps_loaded_states_and_rejects_transitional_state() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([
            print_step(&root.path(), domain, "running", Some(4701)),
            print_step(&root.path(), domain, "waiting", None),
            missing_print_step(domain),
            print_step(&root.path(), domain, "starting", None),
        ]);
        let probe = executor.clone();
        let adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        write_desired_plist(&adapter, &definition);

        assert_eq!(
            adapter
                .status(status_request(&definition))
                .expect("running status")
                .state,
            ServiceManagerState::Running
        );
        assert_eq!(
            adapter
                .status(status_request(&definition))
                .expect("loaded stopped status")
                .state,
            ServiceManagerState::Stopped
        );
        assert_eq!(
            adapter
                .status(status_request(&definition))
                .expect("unloaded installed status")
                .state,
            ServiceManagerState::Stopped
        );
        assert_eq!(
            adapter
                .status(status_request(&definition))
                .expect_err("transitional status")
                .kind(),
            ErrorKind::Unavailable
        );
        probe.assert_drained();
    }

    #[test]
    fn local_plist_and_registration_drift_fail_closed() {
        let local_root = TempRoot::new();
        let no_commands = ScriptedExecutor::new([]);
        let local_probe = no_commands.clone();
        let local_adapter = LaunchdAdapter::for_test(
            no_commands,
            LaunchdDomain::System,
            TEST_LABEL,
            local_root.path(),
        );
        let definition = definition("system");
        fs::write(
            local_adapter.plist_path(),
            b"<?xml version=\"1.0\"?><plist/>",
        )
        .expect("foreign plist");
        assert_eq!(
            local_adapter
                .status(status_request(&definition))
                .expect_err("foreign plist")
                .kind(),
            ErrorKind::Conflict
        );
        let mut drifted = local_adapter
            .desired_plist(
                &definition,
                definition
                    .runtime_binary
                    .as_deref()
                    .expect("runtime binary"),
            )
            .expect("desired plist");
        drifted.extend_from_slice(b"<!-- drift -->\n");
        fs::write(local_adapter.plist_path(), &drifted).expect("drifted managed plist");
        for operation in ["status", "stop", "uninstall"] {
            let error = match operation {
                "status" => local_adapter
                    .status(status_request(&definition))
                    .expect_err("status drift"),
                "stop" => {
                    let executor = ScriptedExecutor::new([]);
                    let mut adapter = LaunchdAdapter::for_test(
                        executor,
                        LaunchdDomain::System,
                        TEST_LABEL,
                        local_root.path(),
                    );
                    adapter
                        .stop(mutation_request(&definition))
                        .expect_err("stop drift")
                }
                "uninstall" => {
                    let executor = ScriptedExecutor::new([]);
                    let mut adapter = LaunchdAdapter::for_test(
                        executor,
                        LaunchdDomain::System,
                        TEST_LABEL,
                        local_root.path(),
                    );
                    adapter
                        .uninstall(mutation_request(&definition))
                        .expect_err("uninstall drift")
                }
                _ => unreachable!(),
            };
            assert_eq!(error.kind(), ErrorKind::Conflict);
        }
        assert_eq!(local_probe.call_count(), 0);

        let registered_root = TempRoot::new();
        let wrong_path = registered_root.path().join("foreign.plist");
        let wrong_program = registered_root.path().join("foreign-runtime");
        let executor = ScriptedExecutor::new([
            registered_print_step(
                &registered_root.path(),
                LaunchdDomain::System,
                "running",
                Some(4801),
                &wrong_path,
                &test_binary(),
            ),
            registered_print_step(
                &registered_root.path(),
                LaunchdDomain::System,
                "running",
                Some(4801),
                &plist_path(&registered_root.path()),
                &wrong_program,
            ),
        ]);
        let probe = executor.clone();
        let adapter = LaunchdAdapter::for_test(
            executor,
            LaunchdDomain::System,
            TEST_LABEL,
            registered_root.path(),
        );
        write_desired_plist(&adapter, &definition);
        assert_eq!(
            adapter
                .status(status_request(&definition))
                .expect_err("registered path drift")
                .kind(),
            ErrorKind::Conflict
        );
        assert_eq!(
            adapter
                .status(status_request(&definition))
                .expect_err("registered program drift")
                .kind(),
            ErrorKind::Conflict
        );
        probe.assert_drained();
    }

    #[test]
    fn launchctl_print_parser_uses_only_set_once_top_level_fields() {
        let target = "system/com.eva-cli.launchd-test";
        let plist = "/Library/LaunchDaemons/com.eva-cli.launchd-test.plist";
        let program = "/opt/eva/eva";
        let valid = format!(
            "{target} = {{\n\tproperties = {{\n\t\tstate = stopped\n\t\tpid = 0\n\t\tpath = /foreign\n\t\tprogram = /foreign\n\t}}\n\tprogram = {program}\n\tpid = 991\n\tpath = {plist}\n\tstate = running\n}}\n"
        );
        assert_eq!(
            parse_launchctl_print(
                &report(0, valid.as_bytes(), b""),
                target,
                plist,
                Some(program)
            )
            .expect("valid nested launchctl output"),
            LaunchdObservedState::Running
        );

        for malformed in [
            format!("{target} = {{\n\tpath = {plist}\n\tprogram = {program}\n}}\n"),
            format!("{target} = {{\n\tstate = waiting\n\tstate = running\n\tpath = {plist}\n\tprogram = {program}\n\tpid = 1\n}}\n"),
            format!("{target} = {{\n\tstate = unknown\n\tpath = {plist}\n\tprogram = {program}\n}}\n"),
            format!("{target} = {{\n\tstate = running\n\tpid = 0\n\tpath = {plist}\n\tprogram = {program}\n}}\n"),
            format!("{target} = {{\n\tstate = running\n\tpid = 1\n\tpath = {plist}\n\tprogram = {program}\n"),
            format!("system/other = {{\n\tstate = waiting\n\tpath = {plist}\n\tprogram = {program}\n}}\n"),
            format!("{target} = {{\n\tstate = waiting\n\tpath = {plist}\n\tprogram = {program}\n}}\nother = {{\n}}\n"),
        ] {
            assert_eq!(
                parse_launchctl_print(
                    &report(0, malformed.as_bytes(), b""),
                    target,
                    plist,
                    Some(program),
                )
                .expect_err("malformed launchctl output")
                .kind(),
                ErrorKind::Conflict
            );
        }
        assert_eq!(
            parse_launchctl_print(&report(0, &[0xff], b""), target, plist, Some(program))
                .expect_err("non-UTF-8 launchctl output")
                .kind(),
            ErrorKind::Conflict
        );
    }

    #[test]
    fn permission_and_errno_mapping_precedes_missing_exit_codes() {
        let root = TempRoot::new();
        let domain = LaunchdDomain::System;
        let executor = ScriptedExecutor::new([scripted_step(
            vec![public("print"), public(service_target(domain))],
            3,
            Vec::new(),
            b"Not privileged: secret diagnostic\n".to_vec(),
        )]);
        let probe = executor.clone();
        let adapter = LaunchdAdapter::for_test(executor, domain, TEST_LABEL, root.path());
        let definition = definition("system");
        let query_error = adapter
            .status(status_request(&definition))
            .expect_err("permission must not look missing");
        assert_eq!(query_error.kind(), ErrorKind::PermissionDenied);
        assert!(!format!("{query_error:?}").contains("secret diagnostic"));
        probe.assert_drained();

        for (exit_code, stderr, expected) in [
            (13, "", ErrorKind::PermissionDenied),
            (
                1,
                "Bootstrap failed: 13: Permission denied",
                ErrorKind::PermissionDenied,
            ),
            (1, "Bootstrap failed: 17: File exists", ErrorKind::Conflict),
            (3, "Bootstrap failed: 17: File exists", ErrorKind::Conflict),
            (113, "Could not find service", ErrorKind::NotFound),
            (1, "Input/output error", ErrorKind::Unavailable),
        ] {
            assert_eq!(
                command_exit_error("test", &report(exit_code, b"", stderr.as_bytes())).kind(),
                expected
            );
        }
    }

    #[test]
    fn invalid_dsl_kind_and_domain_mismatch_fail_before_executor_calls() {
        let root = TempRoot::new();
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let adapter =
            LaunchdAdapter::for_test(executor, LaunchdDomain::System, TEST_LABEL, root.path());
        for unit_name in [
            None,
            Some(TEST_LABEL.to_owned()),
            Some(format!("user/{TEST_LABEL}")),
            Some(format!("system/{TEST_LABEL}/extra")),
            Some("system/.invalid".to_owned()),
        ] {
            let mut invalid = definition("system");
            invalid.unit_name = unit_name;
            assert_eq!(
                adapter
                    .status(status_request(&invalid))
                    .expect_err("invalid domain DSL")
                    .kind(),
                ErrorKind::InvalidArgument
            );
        }
        let mut wrong_kind = definition("system");
        wrong_kind.kind = ServiceManagerKind::Systemd;
        assert_eq!(
            adapter
                .status(status_request(&wrong_kind))
                .expect_err("wrong manager kind")
                .kind(),
            ErrorKind::Unsupported
        );
        let domain_mismatch = definition("gui");
        assert_eq!(
            adapter
                .status(status_request(&domain_mismatch))
                .expect_err("domain mismatch")
                .kind(),
            ErrorKind::Conflict
        );
        assert_eq!(probe.call_count(), 0);
    }

    #[test]
    fn atomic_publication_never_clobbers_an_existing_plist_or_leaves_temp_files() {
        let root = TempRoot::new();
        let target = plist_path(&root.path());
        fs::write(&target, b"foreign").expect("foreign target");

        let error = atomic_create_plist(&root.path(), &target, b"managed", 0, 0, false)
            .expect_err("no-clobber publication");

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(fs::read(&target).expect("unchanged target"), b"foreign");
        assert_eq!(fs::read_dir(root.path()).expect("plist root").count(), 1);
    }

    #[test]
    fn tombstone_restore_never_clobbers_a_reappearing_foreign_plist() {
        let root = TempRoot::new();
        let target = plist_path(&root.path());
        fs::write(&target, b"managed").expect("managed target");
        let tombstone =
            move_plist_to_tombstone(&root.path(), &target).expect("stage managed target");
        fs::write(&target, b"foreign").expect("reappearing foreign target");

        let error = restore_tombstone(&root.path(), &tombstone, &target)
            .expect_err("restore must not clobber");

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(fs::read(&target).expect("foreign target"), b"foreign");
        assert_eq!(
            fs::read(&tombstone).expect("retained tombstone"),
            b"managed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_rejects_a_non_executable_runtime_before_commands() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempRoot::new();
        let binary = root.path().join("not-executable");
        fs::write(&binary, b"fixture").expect("runtime fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o644))
            .expect("runtime fixture permissions");
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let mut adapter =
            LaunchdAdapter::for_test(executor, LaunchdDomain::System, TEST_LABEL, root.path());
        let mut definition = definition("system");
        definition.runtime_binary = Some(binary);

        assert_eq!(
            adapter
                .install(mutation_request(&definition))
                .expect_err("non-executable runtime")
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(probe.call_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn native_permission_policy_rejects_group_writable_gui_runtime_paths() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempRoot::new();
        let binary = root.path().join("group-writable-runtime");
        fs::write(&binary, b"fixture").expect("runtime fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o775))
            .expect("runtime fixture permissions");
        let uid = unsafe { libc::geteuid() };
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let mut adapter = LaunchdAdapter::for_test(
            executor,
            LaunchdDomain::Gui { uid },
            TEST_LABEL,
            root.path(),
        );
        adapter.enforce_native_permissions = true;
        adapter.expected_uid = uid;
        adapter.expected_gid = unsafe { libc::getegid() };
        let mut definition = definition("gui");
        definition.runtime_binary = Some(binary);

        assert_eq!(
            adapter
                .install(mutation_request(&definition))
                .expect_err("group-writable runtime path")
                .kind(),
            ErrorKind::Conflict
        );
        assert_eq!(probe.call_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn system_domain_rejects_a_non_root_owned_runtime_before_commands() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;

        let root = TempRoot::new();
        let binary = root.path().join("untrusted-system-runtime");
        fs::write(&binary, b"fixture").expect("runtime fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
            .expect("runtime fixture permissions");
        if unsafe { libc::geteuid() } == 0 {
            let path = std::ffi::CString::new(binary.as_os_str().as_bytes())
                .expect("runtime fixture path without NUL");
            assert_eq!(
                unsafe { libc::chown(path.as_ptr(), 501, libc::gid_t::MAX) },
                0,
                "make root test fixture non-root owned"
            );
        }
        let executor = ScriptedExecutor::new([]);
        let probe = executor.clone();
        let mut adapter =
            LaunchdAdapter::for_test(executor, LaunchdDomain::System, TEST_LABEL, root.path());
        adapter.enforce_native_permissions = true;
        let mut definition = definition("system");
        definition.runtime_binary = Some(binary);

        assert_eq!(
            adapter
                .install(mutation_request(&definition))
                .expect_err("non-root system runtime")
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(probe.call_count(), 0);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn native_constructor_rejects_non_macos_hosts() {
        let definition = definition("system");
        let error = LaunchdAdapter::native(&definition)
            .err()
            .expect("launchd is macOS-only");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[cfg(target_os = "macos")]
    fn force_native_cleanup(
        adapter: &LaunchdAdapter<ProcessServiceCommandExecutor>,
        definition: &ServiceManagerDefinition,
        remove_root_when_empty: bool,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        match adapter.run_bootout() {
            Ok(_) => {
                if let Err(error) = adapter.wait_for_not_loaded(definition) {
                    errors.push(format!("wait after forced bootout: {error:?}"));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => errors.push(format!("forced bootout: {error:?}")),
        }

        let target_name = adapter
            .plist_path()
            .file_name()
            .expect("native plist file name")
            .to_string_lossy()
            .into_owned();
        match fs::read_dir(&adapter.plist_root) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            let file_name = entry.file_name().to_string_lossy().into_owned();
                            if file_name == target_name
                                || file_name.contains(&format!(".{target_name}.eva-"))
                            {
                                if let Err(error) = fs::remove_file(entry.path()) {
                                    errors.push(format!("remove forced plist residue: {error}"));
                                }
                            }
                        }
                        Err(error) => errors.push(format!("read plist residue entry: {error}")),
                    }
                }
                if let Err(error) = sync_directory(&adapter.plist_root) {
                    errors.push(format!("sync plist root after forced cleanup: {error:?}"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => errors.push(format!("read plist root for forced cleanup: {error}")),
        }
        if remove_root_when_empty && adapter.plist_root.exists() {
            if let Err(error) = fs::remove_dir(&adapter.plist_root) {
                errors.push(format!("remove test-created GUI plist root: {error}"));
            }
        }
        match adapter.query_observed_state(definition) {
            Ok((LaunchdObservedState::NotLoaded, _)) => {}
            Ok((state, _)) => errors.push(format!("registration residue: {state:?}")),
            Err(error) => errors.push(format!("verify forced launchd cleanup: {error:?}")),
        }
        errors
    }

    #[cfg(target_os = "macos")]
    fn run_native_unique_label_probe(selector: &str, restart_supervisor: bool) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let unique = format!("{}-{nonce}", std::process::id());
        let label = format!("com.eva-cli.launchd-probe-{unique}");
        let definition = ServiceManagerDefinition {
            enabled: true,
            kind: ServiceManagerKind::Launchd,
            service_name: format!("EvaCliLaunchdProbe{}", unique.replace('-', "")),
            unit_name: Some(format!("{selector}/{label}")),
            runtime_binary: Some(PathBuf::from("/usr/bin/yes")),
            candidate_runtime_binary: None,
            start_on_boot: true,
            restart_supervisor,
        };
        let mut adapter = LaunchdAdapter::native(&definition).expect("native launchd adapter");
        let remove_root_when_empty = selector == "gui" && !adapter.plist_root.exists();

        let lifecycle = (|| -> Result<(), EvaError> {
            let install = adapter.install(mutation_request(&definition))?;
            if install.state != ServiceManagerState::Running {
                return Err(EvaError::conflict(
                    "native launchd install did not reach Running",
                ));
            }
            let status = adapter.status(status_request(&definition))?;
            if status.state != ServiceManagerState::Running {
                return Err(EvaError::conflict(
                    "native launchd status did not report Running",
                ));
            }
            adapter.restart(mutation_request(&definition))?;
            let stop = adapter.stop(mutation_request(&definition))?;
            if stop.state != ServiceManagerState::Stopped {
                return Err(EvaError::conflict(
                    "native launchd stop did not report Stopped",
                ));
            }
            Ok(())
        })();
        let adapter_cleanup = adapter.uninstall(mutation_request(&definition));
        let forced_cleanup = force_native_cleanup(&adapter, &definition, remove_root_when_empty);

        assert!(
            forced_cleanup.is_empty(),
            "forced launchd cleanup failed: {forced_cleanup:?}"
        );
        adapter_cleanup.expect("adapter launchd cleanup");
        assert!(!adapter.plist_path().exists(), "launchd plist residue");
        lifecycle.expect("native launchd lifecycle");
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a controlled elevated macOS host with launchd system-domain access"]
    fn native_unique_system_label_lifecycle_probe() {
        assert_eq!(unsafe { libc::geteuid() }, 0, "system probe requires root");
        run_native_unique_label_probe("system", true);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a controlled logged-in non-root macOS Aqua session"]
    fn native_unique_gui_label_lifecycle_probe() {
        assert_ne!(unsafe { libc::geteuid() }, 0, "GUI probe rejects root");
        run_native_unique_label_probe("gui", false);
    }
}
