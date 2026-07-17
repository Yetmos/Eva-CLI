//! Central provider process ownership and platform process-boundary contract.
//!
//! This module owns only the OS boundary. Transport registration, restart
//! policy, and durable process-table mutation are deliberately left to the
//! following W3 tasks.

use eva_core::EvaError;
use eva_storage::ProviderProcessSnapshot;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider OS process-group and Job Object ownership";

/// Stateless platform process backend. The caller configures executable,
/// argv, environment, cwd, and stdio on `Command`; this backend adds only the
/// ownership boundary and never invokes a shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OsProcessBackend;

/// Stable public name used by the adapter runtime and later spawn/register work.
pub type ProcessBackend = OsProcessBackend;

/// Abstracts the OS spawn boundary so a caller can attach durable registration
/// immediately after the child is created. Implementations must return an
/// owned handle; callers own cleanup if a later registration step fails.
pub trait ProviderProcessSpawner {
    /// Spawn a provider command inside the implementation's OS boundary.
    fn spawn_provider(&self, command: Command) -> Result<ProviderProcessHandle, EvaError>;
}

impl OsProcessBackend {
    /// Creates a backend with no mutable process-global state.
    pub const fn new() -> Self {
        Self
    }

    /// Spawns a direct command inside a platform-owned process boundary.
    pub fn spawn(&self, mut command: Command) -> Result<ProviderProcessHandle, EvaError> {
        let (child, boundary) = platform::spawn(&mut command).map_err(|error| {
            EvaError::unavailable("failed to spawn provider process boundary")
                .with_context("io_error", error.to_string())
        })?;
        let identity = match platform::identity(&child, &boundary) {
            Ok(identity) => identity,
            Err(error) => {
                let mut child = child;
                let mut boundary = boundary;
                let _ = platform::terminate(&mut child, &mut boundary);
                return Err(
                    EvaError::unavailable("failed to query provider process identity")
                        .with_context("io_error", error.to_string()),
                );
            }
        };
        Ok(ProviderProcessHandle {
            child,
            boundary,
            identity,
        })
    }

    /// Alias kept explicit for callers that want to emphasize the command boundary.
    pub fn spawn_command(&self, command: Command) -> Result<ProviderProcessHandle, EvaError> {
        self.spawn(command)
    }
}

impl ProviderProcessSpawner for OsProcessBackend {
    fn spawn_provider(&self, command: Command) -> Result<ProviderProcessHandle, EvaError> {
        self.spawn(command)
    }
}

/// Real process identity returned by the platform backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// OS process identifier.
    pub pid: u32,
    /// OS process-incarnation token paired with `pid`.
    pub process_start_token: String,
    /// Unix process group identifier, when applicable.
    pub process_group_id: Option<u32>,
    /// Windows Job Object identity, when applicable.
    pub job_id: Option<String>,
}

impl ProcessIdentity {
    /// Returns the process identity as a tuple suitable for storage APIs.
    pub fn storage_fields(&self) -> (u32, String, Option<u32>, Option<String>) {
        (
            self.pid,
            self.process_start_token.clone(),
            self.process_group_id,
            self.job_id.clone(),
        )
    }

    /// Stamps a durable provider snapshot with this OS identity and attempt.
    pub fn stamp_snapshot(
        &self,
        snapshot: &mut ProviderProcessSnapshot,
        attempt: u32,
    ) -> Result<(), EvaError> {
        snapshot.set_process_identity(
            self.pid,
            self.process_start_token.clone(),
            self.process_group_id,
            self.job_id.clone(),
            attempt,
        )
    }
}

/// Owned provider process handle. Dropping a live handle terminates its whole
/// process boundary, preventing a transport error from orphaning descendants.
pub struct ProviderProcessHandle {
    child: Child,
    boundary: ProcessBoundary,
    identity: ProcessIdentity,
}

impl std::fmt::Debug for ProviderProcessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderProcessHandle")
            .field("identity", &self.identity)
            .field("child_id", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl ProviderProcessHandle {
    /// Returns the immutable identity captured immediately after spawn.
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    /// Returns the direct child PID.
    pub const fn pid(&self) -> u32 {
        self.identity.pid
    }

    /// Takes the configured child stdin pipe, if one was requested.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Takes the configured child stdout pipe, if one was requested.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Takes the configured child stderr pipe, if one was requested.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Polls the direct child without releasing the process boundary.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, EvaError> {
        self.child.try_wait().map_err(|error| {
            EvaError::unavailable("failed to query provider process status")
                .with_context("pid", self.pid().to_string())
                .with_context("io_error", error.to_string())
        })
    }

    /// Waits for the direct child. Descendants remain owned until termination
    /// or handle drop, so a normal parent exit cannot silently detach them.
    pub fn wait(&mut self) -> Result<ExitStatus, EvaError> {
        self.child.wait().map_err(|error| {
            EvaError::unavailable("failed to wait for provider process")
                .with_context("pid", self.pid().to_string())
                .with_context("io_error", error.to_string())
        })
    }

    /// Returns whether the direct child is still running.
    pub fn is_running(&mut self) -> Result<bool, EvaError> {
        Ok(self.try_wait()?.is_none())
    }

    /// Re-queries the OS identity and rejects PID reuse or boundary drift.
    pub fn verify_identity(&self) -> Result<(), EvaError> {
        platform::verify_identity(&self.child, &self.boundary, &self.identity).map_err(|error| {
            EvaError::conflict("provider process identity is no longer current")
                .with_context("pid", self.pid().to_string())
                .with_context("io_error", error.to_string())
        })
    }

    /// Force-terminates the complete process group or Job Object. Repeated
    /// calls are idempotent after the first successful boundary close.
    pub fn terminate(&mut self) -> Result<(), EvaError> {
        platform::terminate(&mut self.child, &mut self.boundary).map_err(|error| {
            EvaError::unavailable("failed to terminate provider process boundary")
                .with_context("pid", self.pid().to_string())
                .with_context("io_error", error.to_string())
        })
    }
}

impl Drop for ProviderProcessHandle {
    fn drop(&mut self) {
        let _ = platform::terminate(&mut self.child, &mut self.boundary);
    }
}

#[cfg(unix)]
struct ProcessBoundary {
    process_group_id: libc::pid_t,
    terminated: bool,
}

#[cfg(unix)]
mod platform {
    use super::{ProcessBoundary, ProcessIdentity};
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    #[cfg(target_os = "linux")]
    use std::fs;

    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessBoundary)> {
        // `process_group(0)` asks the child to become the leader of a fresh
        // group before exec, so helpers and descendants share one kill target.
        command.process_group(0);
        let mut child = command.spawn()?;
        let pid = libc::pid_t::try_from(child.id()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "provider PID does not fit a Unix process-group identifier",
            )
        })?;
        let observed_group = unsafe { libc::getpgid(pid) };
        if observed_group < 0 || observed_group != pid {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "child did not become its own Unix process group",
            ));
        }
        Ok((
            child,
            ProcessBoundary {
                process_group_id: pid,
                terminated: false,
            },
        ))
    }

    pub(super) fn identity(
        child: &Child,
        boundary: &ProcessBoundary,
    ) -> io::Result<ProcessIdentity> {
        let pid = child.id();
        let process_group_id = u32::try_from(boundary.process_group_id).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Unix process group ID is negative",
            )
        })?;
        Ok(ProcessIdentity {
            pid,
            process_start_token: process_start_token(pid)?,
            process_group_id: Some(process_group_id),
            job_id: None,
        })
    }

    pub(super) fn verify_identity(
        child: &Child,
        boundary: &ProcessBoundary,
        identity: &ProcessIdentity,
    ) -> io::Result<()> {
        if child.id() != identity.pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "direct child PID changed",
            ));
        }
        let pid = libc::pid_t::try_from(identity.pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Unix PID"))?;
        let observed_group = unsafe { libc::getpgid(pid) };
        if observed_group != boundary.process_group_id
            || observed_group < 0
            || identity.process_group_id != u32::try_from(observed_group).ok()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unix process group identity changed",
            ));
        }
        let observed_token = process_start_token(identity.pid)?;
        if observed_token != identity.process_start_token {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unix process start token changed",
            ));
        }
        Ok(())
    }

    pub(super) fn terminate(child: &mut Child, boundary: &mut ProcessBoundary) -> io::Result<()> {
        if boundary.terminated {
            return Ok(());
        }
        let result = unsafe { libc::kill(-boundary.process_group_id, libc::SIGKILL) };
        let group_error = if result == 0 {
            None
        } else {
            let error = io::Error::last_os_error();
            (error.raw_os_error() != Some(libc::ESRCH)).then_some(error)
        };
        boundary.terminated = true;
        let _ = child.kill();
        let wait_result = match child.wait() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        };
        if let Some(error) = group_error {
            Err(error)
        } else {
            wait_result
        }
    }

    #[cfg(target_os = "linux")]
    fn process_start_token(pid: u32) -> io::Result<String> {
        let data = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let (_, rest) = data.rsplit_once(") ").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Linux process stat is malformed",
            )
        })?;
        let start_ticks = rest.split_whitespace().nth(19).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Linux process stat has no start-time field",
            )
        })?;
        if start_ticks.is_empty() || !start_ticks.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Linux process start-time field is invalid",
            ));
        }
        Ok(format!("linux:{start_ticks}"))
    }

    #[cfg(target_os = "macos")]
    fn process_start_token(pid: u32) -> io::Result<String> {
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let result = unsafe {
            libc::proc_pidinfo(
                libc::pid_t::try_from(pid).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "macOS PID is invalid")
                })?,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if result < size {
            return Err(io::Error::last_os_error());
        }
        Ok(format!(
            "macos:{}:{}",
            info.pbi_start_tvsec, info.pbi_start_tvusec
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn process_start_token(_pid: u32) -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process start-token query is unsupported on this Unix target",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn backend_spawns_real_identity_and_stamps_provider_snapshot() {
        let mut command = helper_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let backend = ProcessBackend::new();
        let mut handle = backend.spawn(command).unwrap();
        assert!(handle.pid() > 0);
        assert!(!handle.identity().process_start_token.is_empty());
        assert!(handle.identity().process_group_id.is_some() || handle.identity().job_id.is_some());
        handle.verify_identity().unwrap();

        let mut snapshot = ProviderProcessSnapshot::running(
            "backend-test-session",
            "backend-test-process",
            eva_core::RequestId::parse("req-backend-test").unwrap(),
            eva_core::AdapterId::parse("stdio-test").unwrap(),
            eva_core::CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "digest",
            "helper",
            "none",
        );
        handle.identity().stamp_snapshot(&mut snapshot, 1).unwrap();
        assert_eq!(snapshot.pid, Some(handle.pid()));
        assert_eq!(snapshot.attempt, 1);
        handle.terminate().unwrap();
        assert!(!handle.is_running().unwrap());
    }

    #[test]
    fn backend_termination_is_idempotent_and_waitable() {
        let mut command = helper_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut handle = ProcessBackend::new().spawn(command).unwrap();
        handle.terminate().unwrap();
        handle.terminate().unwrap();
        let status = handle.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn backend_termination_reaps_a_spawned_descendant() {
        let ready_path = std::env::temp_dir().join(format!(
            "eva-process-backend-descendant-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_backend::tests::descendant_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("EVA_PROCESS_BACKEND_DESCENDANT_READY", &ready_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut handle = ProcessBackend::new().spawn(command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "descendant helper did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let descendant_pid = fs::read_to_string(&ready_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(process_is_alive(descendant_pid));
        handle.terminate().unwrap();
        wait_until_process_is_gone(descendant_pid);
        let _ = fs::remove_file(ready_path);
    }

    #[test]
    #[ignore = "spawned by backend_termination_reaps_a_spawned_descendant"]
    fn descendant_helper() {
        let ready_path = std::env::var_os("EVA_PROCESS_BACKEND_DESCENDANT_READY")
            .map(PathBuf::from)
            .expect("descendant ready path");
        let mut child = descendant_command().spawn().unwrap();
        fs::write(ready_path, child.id().to_string()).unwrap();
        let _ = child.wait();
    }

    #[cfg(unix)]
    fn helper_command() -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command
    }

    #[cfg(windows)]
    fn helper_command() -> Command {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "ping", "127.0.0.1", "-n", "31"]);
        command
    }

    #[cfg(not(any(unix, windows)))]
    fn helper_command() -> Command {
        Command::new("unsupported")
    }

    #[cfg(unix)]
    fn descendant_command() -> Command {
        let mut command = Command::new("sleep");
        command.arg("30");
        command
    }

    #[cfg(windows)]
    fn descendant_command() -> Command {
        let mut command = Command::new("ping");
        command.args(["127.0.0.1", "-n", "31"]);
        command
    }

    #[cfg(not(any(unix, windows)))]
    fn descendant_command() -> Command {
        Command::new("unsupported")
    }

    fn unique_test_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn wait_until_process_is_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while process_is_alive(pid) {
            assert!(
                Instant::now() < deadline,
                "descendant process {pid} survived cleanup"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    fn process_is_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return false;
        }
        unsafe { CloseHandle(process) };
        true
    }

    #[cfg(not(any(unix, windows)))]
    fn process_is_alive(_pid: u32) -> bool {
        false
    }
}

#[cfg(windows)]
struct ProcessBoundary {
    job_handle: windows_sys::Win32::Foundation::HANDLE,
    job_id: String,
    terminated: bool,
}

#[cfg(windows)]
impl Drop for ProcessBoundary {
    fn drop(&mut self) {
        if !self.job_handle.is_null() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job_handle) };
            self.job_handle = std::ptr::null_mut();
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{ProcessBoundary, ProcessIdentity};
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenThread, ResumeThread, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED,
        THREAD_SUSPEND_RESUME,
    };

    static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);

    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessBoundary)> {
        let mut boundary = new_boundary()?;
        command.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe { CloseHandle(boundary.job_handle) };
                return Err(error);
            }
        };
        let assigned =
            unsafe { AssignProcessToJobObject(boundary.job_handle, child.as_raw_handle().cast()) };
        if assigned == 0 {
            let error = io::Error::last_os_error();
            let _ = terminate(&mut child, &mut boundary);
            return Err(error);
        }
        if let Err(error) = resume_suspended_process(child.id()) {
            let _ = terminate(&mut child, &mut boundary);
            return Err(error);
        }
        Ok((child, boundary))
    }

    pub(super) fn identity(
        child: &Child,
        boundary: &ProcessBoundary,
    ) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity {
            pid: child.id(),
            process_start_token: process_start_token(child)?,
            process_group_id: None,
            job_id: Some(boundary.job_id.clone()),
        })
    }

    pub(super) fn verify_identity(
        child: &Child,
        boundary: &ProcessBoundary,
        identity: &ProcessIdentity,
    ) -> io::Result<()> {
        if child.id() != identity.pid || identity.job_id.as_deref() != Some(&boundary.job_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows process identity changed",
            ));
        }
        if process_start_token(child)? != identity.process_start_token {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows process start token changed",
            ));
        }
        let mut in_job = 0;
        let result = unsafe {
            IsProcessInJob(
                child.as_raw_handle().cast(),
                boundary.job_handle,
                &mut in_job,
            )
        };
        if result == 0 || in_job == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows process is no longer in its Job Object",
            ));
        }
        Ok(())
    }

    pub(super) fn terminate(child: &mut Child, boundary: &mut ProcessBoundary) -> io::Result<()> {
        if boundary.terminated {
            return Ok(());
        }
        let result = unsafe { TerminateJobObject(boundary.job_handle, 1) };
        let job_error = (result == 0).then(io::Error::last_os_error);
        boundary.terminated = true;
        let _ = child.kill();
        let wait_result = match child.wait() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        };
        if let Some(error) = job_error {
            if matches!(error.raw_os_error(), Some(5 | 87 | 128)) {
                return wait_result;
            }
            return Err(error);
        }
        wait_result
    }

    fn new_boundary() -> io::Result<ProcessBoundary> {
        let job_handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job_handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job_handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            unsafe { CloseHandle(job_handle) };
            return Err(error);
        }
        let counter = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(ProcessBoundary {
            job_handle,
            job_id: format!("job-{}-{counter}", std::process::id()),
            terminated: false,
        })
    }

    fn process_start_token(child: &Child) -> io::Result<String> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result = unsafe {
            GetProcessTimes(
                child.as_raw_handle().cast(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Ok(format!("windows:{ticks}"))
    }

    fn resume_suspended_process(process_id: u32) -> io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        let mut present = unsafe { Thread32First(snapshot, &raw mut entry) };
        while present != 0 {
            if entry.th32OwnerProcessID == process_id {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    let error = io::Error::last_os_error();
                    unsafe { CloseHandle(snapshot) };
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
        unsafe { CloseHandle(snapshot) };
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "suspended provider main thread was not found",
        ))
    }
}

#[cfg(not(any(unix, windows)))]
struct ProcessBoundary;

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::{ProcessBoundary, ProcessIdentity};
    use std::io;
    use std::process::{Child, Command};

    pub(super) fn spawn(_command: &mut Command) -> io::Result<(Child, ProcessBoundary)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "provider process boundaries are unsupported on this host",
        ))
    }

    pub(super) fn identity(
        _child: &Child,
        _boundary: &ProcessBoundary,
    ) -> io::Result<ProcessIdentity> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "provider process identities are unsupported on this host",
        ))
    }

    pub(super) fn verify_identity(
        _child: &Child,
        _boundary: &ProcessBoundary,
        _identity: &ProcessIdentity,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "provider process identities are unsupported on this host",
        ))
    }

    pub(super) fn terminate(_child: &mut Child, _boundary: &mut ProcessBoundary) -> io::Result<()> {
        Ok(())
    }
}
