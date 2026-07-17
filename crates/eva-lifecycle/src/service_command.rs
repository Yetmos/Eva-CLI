//! Non-shell command execution and bounded evidence for host service managers.

use eva_core::EvaError;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Default timeout for one host service-manager command.
pub const DEFAULT_SERVICE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Default bytes retained independently for stdout and stderr.
pub const DEFAULT_SERVICE_COMMAND_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// Whether an argv value may appear in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceCommandArgVisibility {
    /// Non-sensitive command metadata.
    Public,
    /// A value that must be redacted from debug and audit output.
    Secret,
}

impl ServiceCommandArgVisibility {
    /// Returns the stable audit spelling for this visibility.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Secret => "secret",
        }
    }
}

/// One typed argv entry with explicit diagnostic visibility.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceCommandArg {
    value: OsString,
    visibility: ServiceCommandArgVisibility,
}

impl ServiceCommandArg {
    /// Creates a non-sensitive argv entry.
    pub fn public(value: impl Into<OsString>) -> Self {
        Self {
            value: value.into(),
            visibility: ServiceCommandArgVisibility::Public,
        }
    }

    /// Creates an argv entry that must never enter debug or audit output.
    pub fn secret(value: impl Into<OsString>) -> Self {
        Self {
            value: value.into(),
            visibility: ServiceCommandArgVisibility::Secret,
        }
    }

    /// Returns the exact platform-native argv value.
    pub fn value(&self) -> &OsStr {
        &self.value
    }

    /// Returns this entry's diagnostic visibility.
    pub const fn visibility(&self) -> ServiceCommandArgVisibility {
        self.visibility
    }
}

impl fmt::Debug for ServiceCommandArg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("ServiceCommandArg");
        match self.visibility {
            ServiceCommandArgVisibility::Public => debug.field("value", &self.value),
            ServiceCommandArgVisibility::Secret => debug.field("value", &"[REDACTED]"),
        };
        debug.field("visibility", &self.visibility).finish()
    }
}

/// One executable and its argv, kept separate so no shell parsing is involved.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceCommand {
    executable: PathBuf,
    arguments: Vec<ServiceCommandArg>,
}

impl ServiceCommand {
    /// Creates a command without converting paths or arguments through UTF-8.
    ///
    /// The executable is service-manager program identity and must be public;
    /// sensitive values belong in argv entries marked with `secret`.
    pub fn new(
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = ServiceCommandArg>,
    ) -> Result<Self, EvaError> {
        let executable = executable.into();
        if executable.as_os_str().is_empty() {
            return Err(EvaError::invalid_argument(
                "service command executable cannot be empty",
            ));
        }

        Ok(Self {
            executable,
            arguments: arguments.into_iter().collect(),
        })
    }

    /// Returns the executable passed directly to `Command::new`.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns the exact typed argv passed directly to `Command::args`.
    pub fn arguments(&self) -> &[ServiceCommandArg] {
        &self.arguments
    }

    /// Returns the number of argv entries, excluding the executable.
    pub fn argument_count(&self) -> usize {
        self.arguments.len()
    }
}

impl fmt::Debug for ServiceCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceCommand")
            .field("executable", &self.executable)
            .field("arguments", &self.arguments)
            .finish()
    }
}

/// Validated timeout and per-stream output retention limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceCommandLimits {
    timeout: Duration,
    output_limit_bytes: usize,
}

impl ServiceCommandLimits {
    /// Creates non-zero command limits.
    pub fn new(timeout: Duration, output_limit_bytes: usize) -> Result<Self, EvaError> {
        if timeout.is_zero() {
            return Err(EvaError::invalid_argument(
                "service command timeout must be greater than zero",
            ));
        }
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "service command output limit must be greater than zero",
            ));
        }
        Ok(Self {
            timeout,
            output_limit_bytes,
        })
    }

    /// Returns the wall-clock timeout for one child process.
    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    /// Returns the independent stdout/stderr retention limit.
    pub const fn output_limit_bytes(self) -> usize {
        self.output_limit_bytes
    }
}

impl Default for ServiceCommandLimits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_SERVICE_COMMAND_TIMEOUT,
            output_limit_bytes: DEFAULT_SERVICE_COMMAND_OUTPUT_LIMIT_BYTES,
        }
    }
}

/// How command execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceCommandTermination {
    /// The process exited on its own.
    Exited,
    /// The process exceeded its wall-clock budget and was terminated.
    TimedOut,
    /// At least one output stream exceeded its retention bound.
    OutputLimitExceeded,
}

impl ServiceCommandTermination {
    /// Returns the stable audit spelling for this termination.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::TimedOut => "timed_out",
            Self::OutputLimitExceeded => "output_limit_exceeded",
        }
    }
}

/// Bounded bytes and digest evidence for one child output stream.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceCommandStream {
    /// Retained bytes; this is a prefix when `truncated` is true.
    pub(crate) bytes: Vec<u8>,
    /// Bytes observed before normal exit or enforced termination.
    pub(crate) observed_size_bytes: u64,
    /// SHA-256 of the retained bytes, never falsely presented as a full digest.
    pub(crate) digest: String,
    /// Whether the retained bytes are only a prefix of the command stream.
    pub(crate) truncated: bool,
}

impl ServiceCommandStream {
    fn complete(bytes: Vec<u8>) -> Self {
        let observed_size_bytes = bytes.len() as u64;
        Self::from_capture(bytes, observed_size_bytes, false)
    }

    fn from_capture(bytes: Vec<u8>, observed_size_bytes: u64, truncated: bool) -> Self {
        let digest = sha256_digest(&bytes);
        Self {
            bytes,
            observed_size_bytes,
            digest,
            truncated,
        }
    }

    /// Returns the number of bytes retained in memory.
    pub fn captured_size_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// Returns the retained bytes, which are a prefix when truncated.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the bytes observed before exit or enforced termination.
    pub const fn observed_size_bytes(&self) -> u64 {
        self.observed_size_bytes
    }

    /// Returns the canonical SHA-256 digest of the retained bytes.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns whether the retained bytes are only a stream prefix.
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl fmt::Debug for ServiceCommandStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceCommandStream")
            .field("captured_size_bytes", &self.bytes.len())
            .field("observed_size_bytes", &self.observed_size_bytes)
            .field("digest", &self.digest)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Raw executor result used by host-bound factories and deterministic mocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCommandExecution {
    /// How execution ended.
    pub(crate) termination: ServiceCommandTermination,
    /// Exit code only for a process that exited on its own.
    pub(crate) exit_code: Option<i32>,
    /// Bounded stdout capture.
    pub(crate) stdout: ServiceCommandStream,
    /// Bounded stderr capture.
    pub(crate) stderr: ServiceCommandStream,
}

impl ServiceCommandExecution {
    /// Creates a completed execution for mocks and adapter-level tests.
    pub fn exited(
        exit_code: Option<i32>,
        stdout: impl Into<Vec<u8>>,
        stderr: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            termination: ServiceCommandTermination::Exited,
            exit_code,
            stdout: ServiceCommandStream::complete(stdout.into()),
            stderr: ServiceCommandStream::complete(stderr.into()),
        }
    }

    /// Returns true only for a normal zero-code exit.
    pub fn success(&self) -> bool {
        self.termination == ServiceCommandTermination::Exited && self.exit_code == Some(0)
    }

    /// Returns how execution ended.
    pub const fn termination(&self) -> ServiceCommandTermination {
        self.termination
    }

    /// Returns the exit code for a process that exited on its own.
    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Returns bounded stdout evidence.
    pub const fn stdout(&self) -> &ServiceCommandStream {
        &self.stdout
    }

    /// Returns bounded stderr evidence.
    pub const fn stderr(&self) -> &ServiceCommandStream {
        &self.stderr
    }

    fn validate_integrity(&self) -> Result<(), EvaError> {
        validate_stream_integrity("stdout", &self.stdout)?;
        validate_stream_integrity("stderr", &self.stderr)?;

        match self.termination {
            ServiceCommandTermination::Exited => {
                if self.stdout.truncated || self.stderr.truncated {
                    return Err(EvaError::conflict(
                        "exited service command cannot contain truncated streams",
                    ));
                }
            }
            ServiceCommandTermination::TimedOut => {
                if self.exit_code.is_some() || !self.stdout.truncated || !self.stderr.truncated {
                    return Err(EvaError::conflict(
                        "timed-out service command evidence is inconsistent",
                    ));
                }
            }
            ServiceCommandTermination::OutputLimitExceeded => {
                if self.exit_code.is_some() || !(self.stdout.truncated || self.stderr.truncated) {
                    return Err(EvaError::conflict(
                        "output-limited service command evidence is inconsistent",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Unforgeable proof that a factory matched the manager kind to its host.
///
/// The type is public so external test doubles can implement the executor
/// trait, but only this crate can construct a value.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedServiceCommandTarget {
    kind: crate::service_manager::ServiceManagerKind,
}

impl ValidatedServiceCommandTarget {
    pub(crate) const fn new(kind: crate::service_manager::ServiceManagerKind) -> Self {
        Self { kind }
    }

    /// Returns the production manager kind validated by the factory.
    pub const fn kind(&self) -> crate::service_manager::ServiceManagerKind {
        self.kind
    }
}

/// Secret-free evidence returned by a host-bound command runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCommandReport {
    /// Service-manager kind validated before any executor call.
    pub kind: crate::service_manager::ServiceManagerKind,
    /// How execution ended.
    pub termination: ServiceCommandTermination,
    /// Exit code only for a process that exited on its own.
    pub exit_code: Option<i32>,
    /// Whether execution ended with exit code zero.
    pub success: bool,
    /// Bounded stdout bytes and digest.
    pub stdout: ServiceCommandStream,
    /// Bounded stderr bytes and digest.
    pub stderr: ServiceCommandStream,
    /// SHA-256 binding termination, exit code, and both stream captures.
    pub result_digest: String,
    /// Ordered metadata without executable, argv values, or output bytes.
    pub audit: Vec<String>,
}

impl ServiceCommandReport {
    pub(crate) fn from_execution(
        kind: crate::service_manager::ServiceManagerKind,
        command: &ServiceCommand,
        execution: ServiceCommandExecution,
    ) -> Result<Self, EvaError> {
        execution.validate_integrity()?;
        let success = execution.success();
        let result_digest = command_result_digest(&execution);
        let mut audit = vec![
            "service_command.shell:false".to_owned(),
            format!("service_command.kind:{}", kind.as_str()),
            format!(
                "service_command.argv_count:{}",
                command.argument_count().saturating_add(1)
            ),
        ];
        for (index, argument) in command.arguments().iter().enumerate() {
            let value = match argument.visibility() {
                ServiceCommandArgVisibility::Public => "public",
                ServiceCommandArgVisibility::Secret => "[REDACTED]",
            };
            audit.push(format!("service_command.argv.{index}:{value}"));
        }
        audit.extend([
            format!(
                "service_command.termination:{}",
                execution.termination.as_str()
            ),
            format!(
                "service_command.exit_code:{}",
                execution
                    .exit_code
                    .map_or_else(|| "none".to_owned(), |code| code.to_string())
            ),
            format!("service_command.success:{success}"),
            format!(
                "service_command.stdout_captured_size_bytes:{}",
                execution.stdout.captured_size_bytes()
            ),
            format!(
                "service_command.stdout_observed_size_bytes:{}",
                execution.stdout.observed_size_bytes
            ),
            format!(
                "service_command.stdout_truncated:{}",
                execution.stdout.truncated
            ),
            format!(
                "service_command.stderr_captured_size_bytes:{}",
                execution.stderr.captured_size_bytes()
            ),
            format!(
                "service_command.stderr_observed_size_bytes:{}",
                execution.stderr.observed_size_bytes
            ),
            format!(
                "service_command.stderr_truncated:{}",
                execution.stderr.truncated
            ),
        ]);

        Ok(Self {
            kind,
            termination: execution.termination,
            exit_code: execution.exit_code,
            success,
            stdout: execution.stdout,
            stderr: execution.stderr,
            result_digest,
            audit,
        })
    }
}

fn validate_stream_integrity(
    stream_name: &'static str,
    stream: &ServiceCommandStream,
) -> Result<(), EvaError> {
    let captured_size_bytes = stream.bytes.len() as u64;
    if captured_size_bytes > stream.observed_size_bytes
        || (!stream.truncated && captured_size_bytes != stream.observed_size_bytes)
    {
        return Err(
            EvaError::conflict("service command stream size evidence is inconsistent")
                .with_context("stream", stream_name),
        );
    }
    if stream.digest != sha256_digest(&stream.bytes) {
        return Err(
            EvaError::conflict("service command stream digest evidence is inconsistent")
                .with_context("stream", stream_name),
        );
    }
    Ok(())
}

/// Object-safe backend boundary used by host-bound factories and test doubles.
pub trait ServiceCommandExecutor: Send {
    /// Executes one already separated executable/argv pair without a shell.
    fn execute(
        &mut self,
        target: &ValidatedServiceCommandTarget,
        command: &ServiceCommand,
    ) -> Result<ServiceCommandExecution, EvaError>;
}

impl<T> ServiceCommandExecutor for Box<T>
where
    T: ServiceCommandExecutor + ?Sized,
{
    fn execute(
        &mut self,
        target: &ValidatedServiceCommandTarget,
        command: &ServiceCommand,
    ) -> Result<ServiceCommandExecution, EvaError> {
        (**self).execute(target, command)
    }
}

/// `std::process::Command` backend with timeout and bounded concurrent capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessServiceCommandExecutor {
    limits: ServiceCommandLimits,
}

impl ProcessServiceCommandExecutor {
    /// Creates an executor with already validated limits.
    pub const fn new(limits: ServiceCommandLimits) -> Self {
        Self { limits }
    }

    /// Returns this executor's immutable limits.
    pub const fn limits(self) -> ServiceCommandLimits {
        self.limits
    }
}

impl ServiceCommandExecutor for ProcessServiceCommandExecutor {
    fn execute(
        &mut self,
        _target: &ValidatedServiceCommandTarget,
        command: &ServiceCommand,
    ) -> Result<ServiceCommandExecution, EvaError> {
        let mut native_command = Command::new(command.executable());
        native_command
            .args(command.arguments().iter().map(ServiceCommandArg::value))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, mut process_tree) =
            spawn_process_tree(&mut native_command).map_err(|error| {
                EvaError::unavailable("failed to start owned service-manager command tree")
                    .with_context("io_error", error.to_string())
            })?;

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = process_tree.terminate(&mut child);
                return Err(EvaError::internal(
                    "service-manager command stdout was not available",
                ));
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = process_tree.terminate(&mut child);
                return Err(EvaError::internal(
                    "service-manager command stderr was not available",
                ));
            }
        };

        let (limit_sender, limit_receiver) = mpsc::channel();
        let stdout_reader = spawn_capture(
            stdout,
            self.limits.output_limit_bytes(),
            limit_sender.clone(),
        );
        let stderr_reader = spawn_capture(stderr, self.limits.output_limit_bytes(), limit_sender);

        let started_at = Instant::now();
        let (mut termination, mut exit_code) = loop {
            if limit_receiver.try_recv().is_ok() {
                break (ServiceCommandTermination::OutputLimitExceeded, None);
            }

            match child.try_wait() {
                Ok(Some(status)) => break (ServiceCommandTermination::Exited, status.code()),
                Ok(None) => {}
                Err(error) => {
                    let cleanup = process_tree.terminate(&mut child);
                    if cleanup.is_ok() {
                        let _ = stdout_reader.join();
                        let _ = stderr_reader.join();
                    }
                    return Err(
                        EvaError::unavailable("failed to poll service-manager command")
                            .with_context("io_error", error.to_string()),
                    );
                }
            }

            let elapsed = started_at.elapsed();
            if elapsed >= self.limits.timeout() {
                break (ServiceCommandTermination::TimedOut, None);
            }
            thread::sleep(
                self.limits
                    .timeout()
                    .saturating_sub(elapsed)
                    .min(Duration::from_millis(5)),
            );
        };

        // End the owned tree even after the direct child exits. Descendants may
        // otherwise keep inherited stdout/stderr handles open forever.
        process_tree.terminate(&mut child).map_err(|error| {
            EvaError::unavailable("failed to terminate service-manager command tree")
                .with_context("io_error", error.to_string())
        })?;
        let stdout = join_capture(stdout_reader, "stdout");
        let stderr = join_capture(stderr_reader, "stderr");
        let stdout = stdout?;
        let stderr = stderr?;
        if stdout.truncated || stderr.truncated {
            termination = ServiceCommandTermination::OutputLimitExceeded;
            exit_code = None;
        }
        let force_truncated = termination != ServiceCommandTermination::Exited;

        Ok(ServiceCommandExecution {
            termination,
            exit_code,
            stdout: ServiceCommandStream::from_capture(
                stdout.bytes,
                stdout.observed_size_bytes,
                stdout.truncated || force_truncated,
            ),
            stderr: ServiceCommandStream::from_capture(
                stderr.bytes,
                stderr.observed_size_bytes,
                stderr.truncated || force_truncated,
            ),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CapturedStream {
    bytes: Vec<u8>,
    observed_size_bytes: u64,
    truncated: bool,
}

fn spawn_capture(
    reader: impl Read + Send + 'static,
    output_limit_bytes: usize,
    limit_sender: mpsc::Sender<()>,
) -> thread::JoinHandle<io::Result<CapturedStream>> {
    thread::spawn(move || capture_stream(reader, output_limit_bytes, limit_sender))
}

fn capture_stream(
    mut reader: impl Read,
    output_limit_bytes: usize,
    limit_sender: mpsc::Sender<()>,
) -> io::Result<CapturedStream> {
    let mut bytes = Vec::with_capacity(output_limit_bytes.min(8 * 1024));
    let mut observed_size_bytes = 0_u64;
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed_size_bytes = observed_size_bytes.saturating_add(read as u64);
        let retained = output_limit_bytes.saturating_sub(bytes.len()).min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        if retained < read && !truncated {
            truncated = true;
            let _ = limit_sender.send(());
        }
    }

    Ok(CapturedStream {
        bytes,
        observed_size_bytes,
        truncated,
    })
}

fn join_capture(
    handle: thread::JoinHandle<io::Result<CapturedStream>>,
    stream: &'static str,
) -> Result<CapturedStream, EvaError> {
    handle
        .join()
        .map_err(|_| {
            EvaError::internal("service-manager output reader panicked")
                .with_context("stream", stream)
        })?
        .map_err(|error| {
            EvaError::unavailable("failed to read service-manager output")
                .with_context("stream", stream)
                .with_context("io_error", error.to_string())
        })
}

#[cfg(unix)]
struct ProcessTreeGuard {
    process_group_id: libc::pid_t,
    terminated: bool,
}

#[cfg(unix)]
fn spawn_process_tree(
    command: &mut Command,
) -> io::Result<(std::process::Child, ProcessTreeGuard)> {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
    let mut child = command.spawn()?;
    let process_group_id = match libc::pid_t::try_from(child.id()) {
        Ok(process_group_id) => process_group_id,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "child process id does not fit a Unix process group id",
            ));
        }
    };
    Ok((
        child,
        ProcessTreeGuard {
            process_group_id,
            terminated: false,
        },
    ))
}

#[cfg(unix)]
impl ProcessTreeGuard {
    fn terminate(&mut self, child: &mut std::process::Child) -> io::Result<()> {
        if self.terminated {
            return Ok(());
        }
        let result = unsafe { libc::kill(-self.process_group_id, libc::SIGKILL) };
        let group_error = if result == 0 {
            None
        } else {
            let error = io::Error::last_os_error();
            (error.raw_os_error() != Some(libc::ESRCH)).then_some(error)
        };
        self.terminated = true;
        let _ = child.kill();
        let wait_result = child.wait().map(|_| ());
        if let Some(error) = group_error {
            if error.raw_os_error() == Some(libc::EPERM) && wait_result.is_ok() {
                let probe = unsafe { libc::kill(-self.process_group_id, 0) };
                if probe < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }
            }
            Err(error)
        } else {
            wait_result
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        if !self.terminated {
            let _ = unsafe { libc::kill(-self.process_group_id, libc::SIGKILL) };
        }
    }
}

#[cfg(windows)]
struct ProcessTreeGuard {
    job_handle: windows_sys::Win32::Foundation::HANDLE,
    terminated: bool,
}

#[cfg(windows)]
fn spawn_process_tree(
    command: &mut Command,
) -> io::Result<(std::process::Child, ProcessTreeGuard)> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    let mut process_tree = ProcessTreeGuard::new()?;
    command.creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn()?;
    if let Err(error) = process_tree.assign_and_resume(&child) {
        let _ = process_tree.terminate(&mut child);
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok((child, process_tree))
}

#[cfg(windows)]
impl ProcessTreeGuard {
    fn new() -> io::Result<Self> {
        use std::mem::size_of;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let job_handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job_handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut guard = Self {
            job_handle,
            terminated: false,
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                guard.job_handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            guard.terminated = true;
            return Err(error);
        }
        Ok(guard)
    }

    fn assign_and_resume(&mut self, child: &std::process::Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let assigned =
            unsafe { AssignProcessToJobObject(self.job_handle, child.as_raw_handle().cast()) };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        resume_suspended_process(child.id())
    }

    fn terminate(&mut self, child: &mut std::process::Child) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if self.terminated {
            return Ok(());
        }
        let terminated = unsafe { TerminateJobObject(self.job_handle, 1) };
        let job_error = (terminated == 0).then(io::Error::last_os_error);
        self.terminated = true;
        let _ = child.kill();
        let wait_result = child.wait().map(|_| ());
        if let Some(error) = job_error {
            Err(error)
        } else {
            wait_result
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        unsafe {
            CloseHandle(self.job_handle);
        }
    }
}

#[cfg(windows)]
fn resume_suspended_process(process_id: u32) -> io::Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut present = unsafe { Thread32First(snapshot, &raw mut entry) };
    while present != 0 {
        if entry.th32OwnerProcessID == process_id {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                let error = io::Error::last_os_error();
                unsafe {
                    CloseHandle(snapshot);
                }
                return Err(error);
            }
            let resumed = unsafe { ResumeThread(thread) };
            unsafe {
                CloseHandle(thread);
                CloseHandle(snapshot);
            }
            if resumed == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        present = unsafe { Thread32Next(snapshot, &raw mut entry) };
    }
    unsafe {
        CloseHandle(snapshot);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "suspended child main thread was not found",
    ))
}

#[cfg(not(any(unix, windows)))]
struct ProcessTreeGuard;

#[cfg(not(any(unix, windows)))]
fn spawn_process_tree(
    _command: &mut Command,
) -> io::Result<(std::process::Child, ProcessTreeGuard)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owned process trees are unsupported on this host",
    ))
}

#[cfg(not(any(unix, windows)))]
impl ProcessTreeGuard {
    fn terminate(&mut self, _child: &mut std::process::Child) -> io::Result<()> {
        Ok(())
    }
}

fn command_result_digest(execution: &ServiceCommandExecution) -> String {
    let mut canonical = Vec::with_capacity(
        64_usize
            .saturating_add(execution.stdout.bytes.len())
            .saturating_add(execution.stderr.bytes.len()),
    );
    canonical.extend_from_slice(b"eva.service-command.result.v1\0");
    canonical.push(match execution.termination {
        ServiceCommandTermination::Exited => 0,
        ServiceCommandTermination::TimedOut => 1,
        ServiceCommandTermination::OutputLimitExceeded => 2,
    });
    match execution.exit_code {
        Some(exit_code) => {
            canonical.push(1);
            canonical.extend_from_slice(&exit_code.to_be_bytes());
        }
        None => canonical.push(0),
    }
    append_stream_digest_input(&mut canonical, &execution.stdout);
    append_stream_digest_input(&mut canonical, &execution.stderr);
    sha256_digest(&canonical)
}

fn append_stream_digest_input(canonical: &mut Vec<u8>, stream: &ServiceCommandStream) {
    canonical.extend_from_slice(&stream.observed_size_bytes.to_be_bytes());
    canonical.push(u8::from(stream.truncated));
    canonical.extend_from_slice(&(stream.bytes.len() as u64).to_be_bytes());
    canonical.extend_from_slice(&stream.bytes);
}

fn sha256_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    const DESCENDANT_READY_FILE_ENV: &str = "EVA_SERVICE_COMMAND_DESCENDANT_READY_FILE";

    fn child_test_command(filter: &str) -> ServiceCommand {
        ServiceCommand::new(
            std::env::current_exe().unwrap(),
            [
                ServiceCommandArg::public(filter),
                ServiceCommandArg::public("--ignored"),
                ServiceCommandArg::public("--nocapture"),
            ],
        )
        .unwrap()
    }

    fn validated_target() -> ValidatedServiceCommandTarget {
        ValidatedServiceCommandTarget::new(
            crate::service_manager::ServiceManagerKind::WindowsService,
        )
    }

    #[test]
    fn command_keeps_exact_os_argv_and_redacts_secret_debug_output() {
        let command = ServiceCommand::new(
            PathBuf::from("manager-program"),
            [
                ServiceCommandArg::public("query"),
                ServiceCommandArg::secret("api-key=top-secret"),
            ],
        )
        .unwrap();

        assert_eq!(command.executable(), Path::new("manager-program"));
        assert_eq!(command.arguments()[0].value(), OsStr::new("query"));
        assert_eq!(
            command.arguments()[1].value(),
            OsStr::new("api-key=top-secret")
        );
        let debug = format!("{command:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("top-secret"));
    }

    #[test]
    fn execution_and_result_digests_bind_exit_code_and_both_streams() {
        let command =
            ServiceCommand::new("manager-program", [ServiceCommandArg::public("status")]).unwrap();
        let execution = ServiceCommandExecution::exited(Some(0), b"active\n".to_vec(), Vec::new());
        let report = ServiceCommandReport::from_execution(
            crate::service_manager::ServiceManagerKind::Systemd,
            &command,
            execution.clone(),
        )
        .unwrap();
        let failed = ServiceCommandReport::from_execution(
            crate::service_manager::ServiceManagerKind::Systemd,
            &command,
            ServiceCommandExecution::exited(
                Some(1),
                execution.stdout.bytes,
                execution.stderr.bytes,
            ),
        )
        .unwrap();

        assert_eq!(report.stdout.digest, sha256_digest(b"active\n"));
        assert_eq!(report.stderr.digest, sha256_digest(b""));
        assert!(report.result_digest.starts_with("sha256:"));
        assert_eq!(report.result_digest.len(), 71);
        assert_ne!(report.result_digest, failed.result_digest);
        assert_eq!(failed.termination, ServiceCommandTermination::Exited);
        assert_eq!(failed.exit_code, Some(1));
        assert!(!failed.success);
        assert_eq!(failed.stdout.digest, sha256_digest(b"active\n"));
        assert_eq!(failed.stderr.digest, sha256_digest(b""));
    }

    #[cfg(unix)]
    #[test]
    fn command_preserves_non_utf8_argv() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let raw = OsString::from_vec(b"eva-\xff.service".to_vec());
        let command =
            ServiceCommand::new("manager-program", [ServiceCommandArg::public(raw.clone())])
                .unwrap();

        assert_eq!(command.arguments()[0].value().as_bytes(), raw.as_bytes());
    }

    #[test]
    fn capture_retains_only_the_configured_bound_while_draining_bytes() {
        let (sender, _receiver) = mpsc::channel();
        let capture = capture_stream(Cursor::new(b"0123456789"), 4, sender).unwrap();

        assert_eq!(capture.bytes, b"0123");
        assert_eq!(capture.observed_size_bytes, 10);
        assert!(capture.truncated);
    }

    #[test]
    fn process_executor_captures_stdout_and_stderr_without_a_shell() {
        let command = child_test_command("process_service_command_helper_emits_output");
        let mut executor = ProcessServiceCommandExecutor::default();
        let execution = executor.execute(&validated_target(), &command).unwrap();

        assert_eq!(execution.termination, ServiceCommandTermination::Exited);
        assert_eq!(execution.exit_code, Some(0));
        assert!(execution.success());
        assert!(execution
            .stdout
            .bytes
            .windows(b"service-command-stdout".len())
            .any(|window| window == b"service-command-stdout"));
        assert!(execution
            .stderr
            .bytes
            .windows(b"service-command-stderr".len())
            .any(|window| window == b"service-command-stderr"));
        assert!(!execution.stdout.truncated);
        assert!(!execution.stderr.truncated);
    }

    #[test]
    fn process_executor_returns_output_limit_evidence() {
        let command = child_test_command("process_service_command_helper_exceeds_output_limit");
        let limits = ServiceCommandLimits::new(Duration::from_secs(5), 512).unwrap();
        let mut executor = ProcessServiceCommandExecutor::new(limits);
        let execution = executor.execute(&validated_target(), &command).unwrap();

        assert_eq!(
            execution.termination,
            ServiceCommandTermination::OutputLimitExceeded
        );
        assert_eq!(execution.exit_code, None);
        assert_eq!(execution.stdout.captured_size_bytes(), 512);
        assert!(execution.stdout.observed_size_bytes > 512);
        assert!(execution.stdout.truncated);
        assert!(!execution.success());
    }

    #[test]
    fn process_executor_returns_timeout_evidence() {
        let command = child_test_command("process_service_command_helper_times_out");
        let limits = ServiceCommandLimits::new(Duration::from_millis(50), 64 * 1024).unwrap();
        let mut executor = ProcessServiceCommandExecutor::new(limits);
        let execution = executor.execute(&validated_target(), &command).unwrap();

        assert_eq!(execution.termination, ServiceCommandTermination::TimedOut);
        assert_eq!(execution.exit_code, None);
        assert!(execution.stdout.truncated);
        assert!(execution.stderr.truncated);
        assert!(!execution.success());
    }

    #[test]
    fn process_executor_reaps_descendant_that_inherits_output_pipes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ready_file = std::env::temp_dir().join(format!(
            "eva-service-command-descendant-{}-{unique}.ready",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&ready_file);
        std::env::set_var(DESCENDANT_READY_FILE_ENV, &ready_file);
        let command = child_test_command("process_service_command_helper_spawns_pipe_descendant");
        let limits = ServiceCommandLimits::new(Duration::from_secs(5), 64 * 1024).unwrap();
        let mut executor = ProcessServiceCommandExecutor::new(limits);

        let started_at = Instant::now();
        let execution = executor.execute(&validated_target(), &command).unwrap();
        let elapsed = started_at.elapsed();
        std::env::remove_var(DESCENDANT_READY_FILE_ENV);
        let descendant_id: u32 = std::fs::read_to_string(&ready_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let _ = std::fs::remove_file(&ready_file);

        assert_eq!(execution.termination(), ServiceCommandTermination::Exited);
        assert!(
            elapsed < Duration::from_secs(3),
            "owned command tree exceeded bound: {elapsed:?}"
        );
        wait_until_process_is_gone(descendant_id, Duration::from_secs(2));
    }

    #[test]
    #[ignore = "spawned by process executor coverage"]
    fn process_service_command_helper_emits_output() {
        println!("service-command-stdout");
        eprintln!("service-command-stderr");
    }

    #[test]
    #[ignore = "spawned by output-limit coverage"]
    fn process_service_command_helper_exceeds_output_limit() {
        println!("{}", "x".repeat(32 * 1024));
    }

    #[test]
    #[ignore = "spawned by timeout coverage"]
    fn process_service_command_helper_times_out() {
        thread::sleep(Duration::from_secs(5));
    }

    #[test]
    #[ignore = "spawned by descendant process-tree coverage"]
    #[allow(clippy::zombie_processes)]
    fn process_service_command_helper_spawns_pipe_descendant() {
        let ready_file = std::env::var_os(DESCENDANT_READY_FILE_ENV).unwrap();
        Command::new(std::env::current_exe().unwrap())
            .args([
                "process_service_command_pipe_descendant_sleeps",
                "--ignored",
                "--nocapture",
            ])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !Path::new(&ready_file).is_file() {
            assert!(Instant::now() < deadline, "descendant did not become ready");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "spawned by descendant process-tree coverage"]
    fn process_service_command_pipe_descendant_sleeps() {
        let ready_file = std::env::var_os(DESCENDANT_READY_FILE_ENV).unwrap();
        std::fs::write(&ready_file, std::process::id().to_string()).unwrap();
        println!("service-command-descendant-ready");
        std::io::stdout().flush().unwrap();
        thread::sleep(Duration::from_secs(10));
    }

    #[test]
    fn limits_reject_zero_timeout_and_output_bound() {
        for error in [
            ServiceCommandLimits::new(Duration::ZERO, 1).unwrap_err(),
            ServiceCommandLimits::new(Duration::from_secs(1), 0).unwrap_err(),
        ] {
            assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        }
    }

    #[test]
    fn executor_trait_is_object_safe() {
        struct Mock;

        impl ServiceCommandExecutor for Mock {
            fn execute(
                &mut self,
                _target: &ValidatedServiceCommandTarget,
                _command: &ServiceCommand,
            ) -> Result<ServiceCommandExecution, EvaError> {
                Ok(ServiceCommandExecution::exited(
                    Some(0),
                    Vec::new(),
                    Vec::new(),
                ))
            }
        }

        let mut mock = Mock;
        let object: &mut dyn ServiceCommandExecutor = &mut mock;
        let command = ServiceCommand::new("manager-program", std::iter::empty()).unwrap();

        assert!(object
            .execute(&validated_target(), &command)
            .unwrap()
            .success());
    }

    #[test]
    fn empty_executable_is_rejected() {
        let error = ServiceCommand::new(PathBuf::new(), std::iter::empty()).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    fn wait_until_process_is_gone(process_id: u32, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while process_is_alive(process_id) {
            assert!(
                Instant::now() < deadline,
                "descendant process {process_id} survived tree cleanup"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(windows)]
    fn process_is_alive(process_id: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return false;
        }
        unsafe {
            CloseHandle(process);
        }
        true
    }

    #[cfg(unix)]
    fn process_is_alive(process_id: u32) -> bool {
        let Ok(process_id) = libc::pid_t::try_from(process_id) else {
            return false;
        };
        let result = unsafe { libc::kill(process_id, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
