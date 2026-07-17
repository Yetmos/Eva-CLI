//! Central provider process ownership and platform process-boundary contract.
//!
//! This module owns only the OS boundary. Transport registration, restart
//! policy, and durable process-table mutation are deliberately left to the
//! following W3 tasks.

use eva_core::EvaError;
use eva_storage::ProviderProcessSnapshot;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::time::Duration;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider OS process-group and Job Object ownership";

/// Upper bound used after a force request while the OS retires the process
/// boundary. This is deliberately separate from the caller-selected graceful
/// period: a successful cleanup call must not return while a known group or
/// Job is still live.
const FORCE_TERMINATION_WAIT: Duration = Duration::from_secs(5);

/// Stable result of an owned-handle termination or daemon orphan scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTerminationOutcome {
    /// The recorded process incarnation no longer exists.
    AlreadyExited,
    /// The complete process boundary exited during the graceful period.
    Graceful,
    /// The graceful period elapsed and the complete boundary was force-killed.
    Forced,
    /// The PID exists, but its start token or group/Job boundary no longer
    /// matches the durable snapshot. No signal was sent.
    IdentityMismatch,
    /// A legacy active snapshot has no real OS identity. No signal was sent.
    MissingIdentity,
}

impl ProcessTerminationOutcome {
    /// Returns a stable value suitable for durable audit evidence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyExited => "already_exited",
            Self::Graceful => "graceful",
            Self::Forced => "forced",
            Self::IdentityMismatch => "identity_mismatch",
            Self::MissingIdentity => "missing_identity",
        }
    }
}

/// Auditable cleanup result shared by live handles and restart recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTerminationReport {
    /// PID from the live handle or durable snapshot.
    pub pid: Option<u32>,
    /// Process group/Job cleanup decision.
    pub outcome: ProcessTerminationOutcome,
    /// `unix_group`, `windows_job`, or `none`.
    pub boundary: String,
    /// Caller-provided graceful period.
    pub graceful_timeout_ms: u128,
    /// Whether a platform graceful request was successfully issued.
    pub graceful_requested: bool,
    /// Whether this caller owned and reaped the direct child.
    pub reaped: bool,
}

impl ProcessTerminationReport {
    fn new(
        pid: Option<u32>,
        outcome: ProcessTerminationOutcome,
        boundary: impl Into<String>,
        graceful_timeout: Duration,
        graceful_requested: bool,
        reaped: bool,
    ) -> Self {
        Self {
            pid,
            outcome,
            boundary: boundary.into(),
            graceful_timeout_ms: graceful_timeout.as_millis(),
            graceful_requested,
            reaped,
        }
    }

    /// Returns deterministic evidence entries for recovery and transport audit.
    pub fn audit_entries(&self) -> Vec<String> {
        vec![
            format!(
                "provider.cleanup:pid:{}",
                self.pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "none".to_owned())
            ),
            format!("provider.cleanup:outcome:{}", self.outcome.as_str()),
            format!("provider.cleanup:boundary:{}", self.boundary),
            format!(
                "provider.cleanup:graceful_timeout_ms:{}",
                self.graceful_timeout_ms
            ),
            format!(
                "provider.cleanup:graceful_requested:{}",
                self.graceful_requested
            ),
            format!(
                "provider.cleanup:forced:{}",
                self.outcome == ProcessTerminationOutcome::Forced
            ),
            format!("provider.cleanup:reaped:{}", self.reaped),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformTerminationResult {
    outcome: ProcessTerminationOutcome,
    graceful_requested: bool,
    reaped: bool,
}

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
                let _ = platform::force_terminate(&mut child, &mut boundary);
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

    /// Cleans up one active process recorded by a previous daemon incarnation.
    ///
    /// The platform implementation re-queries the PID start token and its
    /// process group or named Job before issuing any signal. A missing legacy
    /// identity and a reused PID are reported as non-mutating outcomes so
    /// restart recovery can persist an explicit audit decision without risking
    /// an unrelated process.
    pub fn terminate_snapshot(
        &self,
        snapshot: &ProviderProcessSnapshot,
        graceful_timeout: Duration,
    ) -> Result<ProcessTerminationReport, EvaError> {
        let Some(identity) = ProcessIdentity::from_snapshot(snapshot)? else {
            return Ok(ProcessTerminationReport::new(
                None,
                ProcessTerminationOutcome::MissingIdentity,
                "none",
                graceful_timeout,
                false,
                false,
            ));
        };
        let boundary = identity.boundary_kind();
        let result =
            platform::terminate_snapshot(&identity, graceful_timeout).map_err(|error| {
                EvaError::unavailable("failed to clean up durable provider process boundary")
                    .with_context("session_id", &snapshot.session_id)
                    .with_context("pid", identity.pid.to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(ProcessTerminationReport::new(
            Some(identity.pid),
            result.outcome,
            boundary,
            graceful_timeout,
            result.graceful_requested,
            result.reaped,
        ))
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

    /// Reconstructs an OS identity from a durable snapshot. A fully legacy
    /// snapshot returns `None`; partially populated or ambiguous identities
    /// fail validation instead of weakening the cleanup fence.
    pub fn from_snapshot(snapshot: &ProviderProcessSnapshot) -> Result<Option<Self>, EvaError> {
        let Some(pid) = snapshot.pid else {
            if snapshot.process_start_token.is_some()
                || snapshot.process_group_id.is_some()
                || snapshot.job_id.is_some()
                || snapshot.attempt != 0
            {
                return Err(EvaError::invalid_argument(
                    "provider snapshot has partial OS process identity",
                )
                .with_context("session_id", &snapshot.session_id));
            }
            return Ok(None);
        };
        let process_start_token = snapshot
            .process_start_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                EvaError::invalid_argument("provider snapshot is missing process start token")
                    .with_context("session_id", &snapshot.session_id)
                    .with_context("pid", pid.to_string())
            })?;
        if pid == 0 || snapshot.attempt == 0 {
            return Err(EvaError::invalid_argument(
                "provider snapshot process identity must have positive pid and attempt",
            )
            .with_context("session_id", &snapshot.session_id));
        }
        if snapshot.process_group_id.is_some() == snapshot.job_id.is_some() {
            return Err(EvaError::invalid_argument(
                "provider snapshot must identify exactly one process group or Job",
            )
            .with_context("session_id", &snapshot.session_id)
            .with_context("pid", pid.to_string()));
        }
        Ok(Some(Self {
            pid,
            process_start_token,
            process_group_id: snapshot.process_group_id,
            job_id: snapshot.job_id.clone(),
        }))
    }

    /// Returns the stable OS boundary name used in cleanup audit evidence.
    pub const fn boundary_kind(&self) -> &'static str {
        if self.process_group_id.is_some() {
            "unix_group"
        } else if self.job_id.is_some() {
            "windows_job"
        } else {
            "none"
        }
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

    /// Requests graceful shutdown for the complete process boundary, waits up
    /// to `graceful_timeout`, then force-kills the boundary and reaps the direct
    /// child when the timeout elapses.
    pub fn terminate_gracefully(
        &mut self,
        graceful_timeout: Duration,
    ) -> Result<ProcessTerminationReport, EvaError> {
        let boundary = self.identity.boundary_kind();
        let result = platform::terminate_gracefully(
            &mut self.child,
            &mut self.boundary,
            &self.identity,
            graceful_timeout,
        )
        .map_err(|error| {
            EvaError::unavailable("failed to terminate provider process boundary")
                .with_context("pid", self.pid().to_string())
                .with_context("io_error", error.to_string())
        })?;
        Ok(ProcessTerminationReport::new(
            Some(self.pid()),
            result.outcome,
            boundary,
            graceful_timeout,
            result.graceful_requested,
            result.reaped,
        ))
    }

    /// Immediately force-kills the complete process boundary and reaps the
    /// direct child. This remains available for registration failures and Drop
    /// paths where blocking for a graceful period would be unsafe.
    pub fn force_terminate(&mut self) -> Result<ProcessTerminationReport, EvaError> {
        let boundary = self.identity.boundary_kind();
        let result =
            platform::force_terminate(&mut self.child, &mut self.boundary).map_err(|error| {
                EvaError::unavailable("failed to terminate provider process boundary")
                    .with_context("pid", self.pid().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(ProcessTerminationReport::new(
            Some(self.pid()),
            result.outcome,
            boundary,
            Duration::ZERO,
            false,
            result.reaped,
        ))
    }

    /// Force-terminates the complete process group or Job Object. Repeated
    /// calls are idempotent after the first successful boundary close.
    pub fn terminate(&mut self) -> Result<(), EvaError> {
        self.force_terminate().map(|_| ())
    }
}

impl Drop for ProviderProcessHandle {
    fn drop(&mut self) {
        let _ = platform::force_terminate(&mut self.child, &mut self.boundary);
    }
}

#[cfg(unix)]
struct ProcessBoundary {
    process_group_id: libc::pid_t,
    terminated: bool,
}

#[cfg(unix)]
mod platform {
    use super::{
        PlatformTerminationResult, ProcessBoundary, ProcessIdentity, ProcessTerminationOutcome,
        FORCE_TERMINATION_WAIT,
    };
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant};

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

    pub(super) fn terminate_gracefully(
        child: &mut Child,
        boundary: &mut ProcessBoundary,
        identity: &ProcessIdentity,
        graceful_timeout: Duration,
    ) -> io::Result<PlatformTerminationResult> {
        if boundary.terminated {
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                true,
            ));
        }
        // A live leader must still be the exact incarnation captured at spawn.
        // If it has already exited, this owned boundary remains authoritative
        // for descendants in the original process group.
        if child.try_wait()?.is_none() {
            verify_identity(child, boundary, identity)?;
        }
        let graceful_requested = signal_group(boundary.process_group_id, libc::SIGTERM)?;
        if !graceful_requested {
            boundary.terminated = true;
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                true,
            ));
        }
        if wait_for_owned_group_exit(child, boundary.process_group_id, graceful_timeout)? {
            boundary.terminated = true;
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::Graceful,
                true,
                true,
            ));
        }

        force_group_and_reap(child, boundary)?;
        Ok(termination_result(
            ProcessTerminationOutcome::Forced,
            true,
            true,
        ))
    }

    pub(super) fn force_terminate(
        child: &mut Child,
        boundary: &mut ProcessBoundary,
    ) -> io::Result<PlatformTerminationResult> {
        if boundary.terminated {
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                true,
            ));
        }
        let existed = process_group_exists(boundary.process_group_id)?;
        force_group_and_reap(child, boundary)?;
        Ok(termination_result(
            if existed {
                ProcessTerminationOutcome::Forced
            } else {
                ProcessTerminationOutcome::AlreadyExited
            },
            false,
            true,
        ))
    }

    pub(super) fn terminate_snapshot(
        identity: &ProcessIdentity,
        graceful_timeout: Duration,
    ) -> io::Result<PlatformTerminationResult> {
        let Some(group_id) = identity.process_group_id else {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        };
        if identity.job_id.is_some() || group_id != identity.pid {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        }
        let group_id = libc::pid_t::try_from(group_id)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Unix group ID"))?;
        if group_id <= 1 || group_id == unsafe { libc::getpgrp() } {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        }
        match snapshot_identity_matches(identity, group_id)? {
            SnapshotIdentity::Missing => {
                return Ok(termination_result(
                    ProcessTerminationOutcome::AlreadyExited,
                    false,
                    false,
                ))
            }
            SnapshotIdentity::Mismatch => {
                return Ok(termination_result(
                    ProcessTerminationOutcome::IdentityMismatch,
                    false,
                    false,
                ))
            }
            SnapshotIdentity::Matches => {}
        }

        let graceful_requested = signal_group(group_id, libc::SIGTERM)?;
        if !graceful_requested {
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                false,
            ));
        }
        if wait_for_group_exit(group_id, graceful_timeout)? {
            return Ok(termination_result(
                ProcessTerminationOutcome::Graceful,
                true,
                false,
            ));
        }

        // Re-fence immediately before the destructive fallback. POSIX keeps a
        // process-group identifier allocated until the last member leaves, so
        // a surviving group remains the recorded boundary even after its
        // leader exits.
        if snapshot_identity_matches(identity, group_id)? != SnapshotIdentity::Matches {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                true,
                false,
            ));
        }
        if !signal_group(group_id, libc::SIGKILL)? {
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                true,
                false,
            ));
        }
        if !wait_for_group_exit(group_id, FORCE_TERMINATION_WAIT)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Unix provider process group survived SIGKILL",
            ));
        }
        Ok(termination_result(
            ProcessTerminationOutcome::Forced,
            true,
            false,
        ))
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SnapshotIdentity {
        Missing,
        Matches,
        Mismatch,
    }

    fn snapshot_identity_matches(
        identity: &ProcessIdentity,
        expected_group: libc::pid_t,
    ) -> io::Result<SnapshotIdentity> {
        let pid = libc::pid_t::try_from(identity.pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Unix PID"))?;
        let observed_group = unsafe { libc::getpgid(pid) };
        if observed_group < 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) {
                if process_group_exists(expected_group)? {
                    // A process-group ID cannot be reused while any member of
                    // that group remains. The leader token fenced the group at
                    // spawn; continued group existence is therefore sufficient
                    // evidence after the leader exits.
                    Ok(SnapshotIdentity::Matches)
                } else {
                    Ok(SnapshotIdentity::Missing)
                }
            } else {
                Err(error)
            };
        }
        if observed_group != expected_group {
            return Ok(SnapshotIdentity::Mismatch);
        }
        match process_start_token(identity.pid) {
            Ok(token) if token == identity.process_start_token => Ok(SnapshotIdentity::Matches),
            Ok(_) => Ok(SnapshotIdentity::Mismatch),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                if process_group_exists(expected_group)? {
                    Ok(SnapshotIdentity::Matches)
                } else {
                    Ok(SnapshotIdentity::Missing)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn force_group_and_reap(child: &mut Child, boundary: &mut ProcessBoundary) -> io::Result<()> {
        let group_result = signal_group(boundary.process_group_id, libc::SIGKILL);
        let _ = child.kill();
        let reap_result = reap_child(child);
        let group_result = group_result?;
        if group_result && !wait_for_group_exit(boundary.process_group_id, FORCE_TERMINATION_WAIT)?
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Unix provider process group survived SIGKILL",
            ));
        }
        reap_result?;
        boundary.terminated = true;
        Ok(())
    }

    fn signal_group(process_group_id: libc::pid_t, signal: libc::c_int) -> io::Result<bool> {
        if unsafe { libc::kill(-process_group_id, signal) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    fn wait_for_group_exit(process_group_id: libc::pid_t, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !process_group_exists(process_group_id)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_owned_group_exit(
        child: &mut Child,
        process_group_id: libc::pid_t,
        timeout: Duration,
    ) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            let _ = child.try_wait()?;
            if !process_group_exists(process_group_id)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn process_group_exists(process_group_id: libc::pid_t) -> io::Result<bool> {
        if unsafe { libc::kill(-process_group_id, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(error),
        }
    }

    fn reap_child(child: &mut Child) -> io::Result<()> {
        match child.wait() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn termination_result(
        outcome: ProcessTerminationOutcome,
        graceful_requested: bool,
        reaped: bool,
    ) -> PlatformTerminationResult {
        PlatformTerminationResult {
            outcome,
            graceful_requested,
            reaped,
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
    fn snapshot_cleanup_reopens_boundary_and_terminates_descendants() {
        let identity_path = std::env::temp_dir().join(format!(
            "eva-process-backend-snapshot-identity-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));
        let descendant_path = std::env::temp_dir().join(format!(
            "eva-process-backend-snapshot-descendant-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));
        let mut owner_command = Command::new(std::env::current_exe().unwrap());
        owner_command
            .args([
                "--exact",
                "process_backend::tests::snapshot_owner_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("EVA_PROCESS_BACKEND_SNAPSHOT_IDENTITY", &identity_path)
            .env("EVA_PROCESS_BACKEND_DESCENDANT_READY", &descendant_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut owner = owner_command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !identity_path.is_file() || !descendant_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "snapshot cleanup descendant did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let mut snapshot = provider_snapshot("snapshot-cleanup");
        apply_identity_file(&mut snapshot, &fs::read_to_string(&identity_path).unwrap());
        let descendant_pid = fs::read_to_string(&descendant_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(owner.wait().unwrap().success());
        let report = ProcessBackend::new()
            .terminate_snapshot(&snapshot, Duration::from_secs(1))
            .unwrap();

        assert!(matches!(
            report.outcome,
            ProcessTerminationOutcome::AlreadyExited
                | ProcessTerminationOutcome::Graceful
                | ProcessTerminationOutcome::Forced
        ));
        assert!(!report.reaped);
        wait_until_process_is_gone(descendant_pid);
        let _ = fs::remove_file(identity_path);
        let _ = fs::remove_file(descendant_path);
    }

    #[test]
    fn snapshot_cleanup_rejects_reused_pid_start_token_without_signalling() {
        let mut command = helper_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let backend = ProcessBackend::new();
        let mut handle = backend.spawn(command).unwrap();
        let mut snapshot = provider_snapshot("snapshot-reused-pid");
        handle.identity().stamp_snapshot(&mut snapshot, 1).unwrap();
        snapshot.process_start_token = Some("different-process-incarnation".to_owned());

        let report = backend
            .terminate_snapshot(&snapshot, Duration::from_millis(25))
            .unwrap();

        assert_eq!(report.outcome, ProcessTerminationOutcome::IdentityMismatch);
        assert!(!report.graceful_requested);
        assert!(handle.is_running().unwrap());
        handle.force_terminate().unwrap();
    }

    #[test]
    fn snapshot_cleanup_reports_legacy_identity_without_side_effects() {
        let snapshot = provider_snapshot("snapshot-legacy");
        let report = ProcessBackend::new()
            .terminate_snapshot(&snapshot, Duration::from_millis(25))
            .unwrap();

        assert_eq!(report.outcome, ProcessTerminationOutcome::MissingIdentity);
        assert_eq!(report.pid, None);
        assert_eq!(report.boundary, "none");
    }

    #[cfg(unix)]
    #[test]
    fn graceful_timeout_force_kills_an_uncooperative_group() {
        let ready_path = std::env::temp_dir().join(format!(
            "eva-process-backend-force-descendant-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_backend::tests::term_ignoring_descendant_helper",
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
                "TERM-ignoring descendant did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let descendant_pid = fs::read_to_string(&ready_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();

        let report = handle
            .terminate_gracefully(Duration::from_millis(50))
            .unwrap();

        assert_eq!(report.outcome, ProcessTerminationOutcome::Forced);
        assert!(report.graceful_requested);
        assert!(report.reaped);
        wait_until_process_is_gone(descendant_pid);
        let _ = fs::remove_file(ready_path);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_cleanup_force_kills_descendant_after_group_leader_exits() {
        let ready_path = std::env::temp_dir().join(format!(
            "eva-process-backend-exited-leader-descendant-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_backend::tests::exiting_group_leader_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("EVA_PROCESS_BACKEND_DESCENDANT_READY", &ready_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let backend = ProcessBackend::new();
        let mut handle = backend.spawn(command).unwrap();
        let mut snapshot = provider_snapshot("snapshot-exited-leader");
        handle.identity().stamp_snapshot(&mut snapshot, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "exiting group leader did not publish its descendant"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let descendant_pid = fs::read_to_string(&ready_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(handle.wait().unwrap().success());
        assert!(process_is_alive(descendant_pid));

        let report = backend
            .terminate_snapshot(&snapshot, Duration::from_millis(50))
            .unwrap();

        assert_eq!(report.outcome, ProcessTerminationOutcome::Forced);
        assert!(report.graceful_requested);
        assert!(!report.reaped);
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
    #[test]
    #[ignore = "spawned by graceful_timeout_force_kills_an_uncooperative_group"]
    fn term_ignoring_descendant_helper() {
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
        descendant_helper();
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "spawned by snapshot_cleanup_force_kills_descendant_after_group_leader_exits"]
    fn exiting_group_leader_helper() {
        let ready_path = std::env::var_os("EVA_PROCESS_BACKEND_DESCENDANT_READY")
            .map(PathBuf::from)
            .expect("descendant ready path");
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
        let child = descendant_command().spawn().unwrap();
        fs::write(ready_path, child.id().to_string()).unwrap();
        drop(child);
    }

    #[test]
    #[ignore = "spawned by snapshot_cleanup_reopens_boundary_and_terminates_descendants"]
    fn snapshot_owner_helper() {
        let identity_path = std::env::var_os("EVA_PROCESS_BACKEND_SNAPSHOT_IDENTITY")
            .map(PathBuf::from)
            .expect("snapshot identity path");
        let ready_path = std::env::var_os("EVA_PROCESS_BACKEND_DESCENDANT_READY")
            .map(PathBuf::from)
            .expect("descendant ready path");
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
        let backend = ProcessBackend::new();
        let handle = backend.spawn(command).unwrap();
        let identity = handle.identity().clone();
        fs::write(
            identity_path,
            format!(
                "pid={}\ntoken={}\ngroup={}\njob={}\n",
                identity.pid,
                identity.process_start_token,
                identity
                    .process_group_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                identity.job_id.as_deref().unwrap_or_default(),
            ),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "snapshot owner descendant did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // Simulate a daemon crash: process teardown closes the Windows Job
        // handle automatically, while Unix descendants remain in their
        // dedicated group for the successor daemon's snapshot scan.
        std::mem::forget(handle);
        std::process::exit(0);
    }

    fn provider_snapshot(name: &str) -> ProviderProcessSnapshot {
        let request_id = format!("req-backend-{name}");
        ProviderProcessSnapshot::running(
            format!("backend-{name}-session"),
            format!("backend-{name}-process"),
            eva_core::RequestId::parse(&request_id).unwrap(),
            eva_core::AdapterId::parse("stdio-test").unwrap(),
            eva_core::CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "digest",
            "helper",
            "none",
        )
    }

    fn apply_identity_file(snapshot: &mut ProviderProcessSnapshot, data: &str) {
        let mut pid = None;
        let mut token = None;
        let mut group = None;
        let mut job = None;
        for line in data.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "pid" => pid = value.parse::<u32>().ok(),
                "token" => token = Some(value.to_owned()),
                "group" if !value.is_empty() => group = value.parse::<u32>().ok(),
                "job" if !value.is_empty() => job = Some(value.to_owned()),
                _ => {}
            }
        }
        snapshot
            .set_process_identity(pid.unwrap(), token.unwrap(), group, job, 1)
            .unwrap();
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
    use super::{
        PlatformTerminationResult, ProcessBoundary, ProcessIdentity, ProcessTerminationOutcome,
        FORCE_TERMINATION_WAIT,
    };
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND,
        ERROR_INVALID_PARAMETER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, OpenJobObjectW,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, OpenThread, ResumeThread, WaitForSingleObject,
        CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, PROCESS_QUERY_LIMITED_INFORMATION,
        THREAD_SUSPEND_RESUME,
    };

    static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessBoundary)> {
        let mut boundary = new_boundary()?;
        command.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);
        let mut child = command.spawn()?;
        let assigned =
            unsafe { AssignProcessToJobObject(boundary.job_handle, child.as_raw_handle().cast()) };
        if assigned == 0 {
            let error = io::Error::last_os_error();
            let _ = force_terminate(&mut child, &mut boundary);
            return Err(error);
        }
        if let Err(error) = resume_suspended_process(child.id()) {
            let _ = force_terminate(&mut child, &mut boundary);
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

    pub(super) fn terminate_gracefully(
        child: &mut Child,
        boundary: &mut ProcessBoundary,
        identity: &ProcessIdentity,
        graceful_timeout: Duration,
    ) -> io::Result<PlatformTerminationResult> {
        if boundary.terminated {
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                true,
            ));
        }
        verify_identity(child, boundary, identity)?;
        if job_is_empty(boundary.job_handle)? {
            boundary.terminated = true;
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                true,
            ));
        }
        // Closing an owned stdio pipe is the only process-local cooperative
        // shutdown signal that cannot escape to unrelated console groups.
        // `GenerateConsoleCtrlEvent` is intentionally avoided here because a
        // service daemon may share a console with its launcher.
        let graceful_requested = child.stdin.take().is_some();
        if wait_for_job_empty(boundary.job_handle, graceful_timeout)? {
            boundary.terminated = true;
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::Graceful,
                graceful_requested,
                true,
            ));
        }
        force_job_and_reap(child, boundary)?;
        Ok(termination_result(
            ProcessTerminationOutcome::Forced,
            graceful_requested,
            true,
        ))
    }

    pub(super) fn force_terminate(
        child: &mut Child,
        boundary: &mut ProcessBoundary,
    ) -> io::Result<PlatformTerminationResult> {
        if boundary.terminated {
            reap_child(child)?;
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                true,
            ));
        }
        let was_empty = job_is_empty(boundary.job_handle).unwrap_or(false);
        force_job_and_reap(child, boundary)?;
        Ok(termination_result(
            if was_empty {
                ProcessTerminationOutcome::AlreadyExited
            } else {
                ProcessTerminationOutcome::Forced
            },
            false,
            true,
        ))
    }

    pub(super) fn terminate_snapshot(
        identity: &ProcessIdentity,
        graceful_timeout: Duration,
    ) -> io::Result<PlatformTerminationResult> {
        let Some(job_id) = identity.job_id.as_deref() else {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        };
        if identity.process_group_id.is_some() || job_id.trim().is_empty() {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        }

        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
                0,
                identity.pid,
            )
        };
        if process.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                Ok(termination_result(
                    ProcessTerminationOutcome::AlreadyExited,
                    false,
                    false,
                ))
            } else {
                Err(error)
            };
        }
        let process = OwnedHandle(process);
        if process_start_token_from_handle(process.0)? != identity.process_start_token {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        }

        let job_name = wide_string(job_id);
        let job = unsafe {
            OpenJobObjectW(
                JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE,
                0,
                job_name.as_ptr(),
            )
        };
        if job.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
                // Closing the last KILL_ON_JOB_CLOSE handle destroys the named
                // Job before every exited process object necessarily becomes
                // unopenable. The immutable process handle tells these apart:
                // a signaled process is already cleaned, while a live process
                // with a missing Job remains a fenced mismatch.
                let exited = wait_for_process_exit(process.0, graceful_timeout)?;
                Ok(termination_result(
                    if exited {
                        ProcessTerminationOutcome::AlreadyExited
                    } else {
                        ProcessTerminationOutcome::IdentityMismatch
                    },
                    false,
                    false,
                ))
            } else {
                Err(error)
            };
        }
        let job = OwnedHandle(job);
        if !process_is_in_job(process.0, job.0)? {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                false,
                false,
            ));
        }
        if job_is_empty(job.0)? {
            return Ok(termination_result(
                ProcessTerminationOutcome::AlreadyExited,
                false,
                false,
            ));
        }

        // Recovery has no inherited stdin handle to close. Preserve the
        // graceful wait window for a provider already exiting on its own,
        // then terminate the named Job as the bounded fallback.
        let graceful_requested = false;
        if wait_for_job_empty(job.0, graceful_timeout)? {
            return Ok(termination_result(
                ProcessTerminationOutcome::Graceful,
                graceful_requested,
                false,
            ));
        }

        // Both handles refer to immutable kernel objects, so this final check
        // remains safe even if the numeric PID is concurrently reused.
        if process_start_token_from_handle(process.0)? != identity.process_start_token
            || !process_is_in_job(process.0, job.0)?
        {
            return Ok(termination_result(
                ProcessTerminationOutcome::IdentityMismatch,
                graceful_requested,
                false,
            ));
        }
        terminate_job(job.0)?;
        if !wait_for_job_empty(job.0, FORCE_TERMINATION_WAIT)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows provider Job survived forced termination",
            ));
        }
        Ok(termination_result(
            ProcessTerminationOutcome::Forced,
            graceful_requested,
            false,
        ))
    }

    fn new_boundary() -> io::Result<ProcessBoundary> {
        for _ in 0..8 {
            let job_id = next_job_id();
            let job_name = wide_string(&job_id);
            let job_handle = unsafe { CreateJobObjectW(std::ptr::null(), job_name.as_ptr()) };
            if job_handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already_exists {
                unsafe { CloseHandle(job_handle) };
                continue;
            }
            configure_job(job_handle)?;
            return Ok(ProcessBoundary {
                job_handle,
                job_id,
                terminated: false,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to allocate a unique provider Job name",
        ))
    }

    fn configure_job(job_handle: HANDLE) -> io::Result<()> {
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
        Ok(())
    }

    fn next_job_id() -> String {
        let counter = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!(
            "Local\\EvaProviderJob-{}-{now}-{counter}",
            std::process::id()
        )
    }

    fn wide_string(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn process_start_token(child: &Child) -> io::Result<String> {
        process_start_token_from_handle(child.as_raw_handle().cast())
    }

    fn process_start_token_from_handle(process: HANDLE) -> io::Result<String> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result =
            unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Ok(format!("windows:{ticks}"))
    }

    fn process_is_in_job(process: HANDLE, job: HANDLE) -> io::Result<bool> {
        let mut in_job = 0;
        if unsafe { IsProcessInJob(process, job, &mut in_job) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(in_job != 0)
    }

    fn job_is_empty(job: HANDLE) -> io::Result<bool> {
        let mut information = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        if unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicAccountingInformation,
                (&raw mut information).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(information.ActiveProcesses == 0)
    }

    fn wait_for_job_empty(job: HANDLE, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if job_is_empty(job)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_process_exit(process: HANDLE, timeout: Duration) -> io::Result<bool> {
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        match unsafe { WaitForSingleObject(process, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            status => Err(io::Error::other(format!(
                "unexpected Windows process wait status {status}"
            ))),
        }
    }

    fn force_job_and_reap(child: &mut Child, boundary: &mut ProcessBoundary) -> io::Result<()> {
        let terminate_result = terminate_job(boundary.job_handle);
        let _ = child.kill();
        let reap_result = reap_child(child);
        terminate_result?;
        if !wait_for_job_empty(boundary.job_handle, FORCE_TERMINATION_WAIT)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows provider Job survived forced termination",
            ));
        }
        reap_result?;
        boundary.terminated = true;
        Ok(())
    }

    fn terminate_job(job: HANDLE) -> io::Result<()> {
        if unsafe { TerminateJobObject(job, 1) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(5 | 87 | 128)) && job_is_empty(job)? {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn reap_child(child: &mut Child) -> io::Result<()> {
        match child.wait() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn termination_result(
        outcome: ProcessTerminationOutcome,
        graceful_requested: bool,
        reaped: bool,
    ) -> PlatformTerminationResult {
        PlatformTerminationResult {
            outcome,
            graceful_requested,
            reaped,
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
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
    use super::{
        PlatformTerminationResult, ProcessBoundary, ProcessIdentity, ProcessTerminationOutcome,
    };
    use std::io;
    use std::process::{Child, Command};
    use std::time::Duration;

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

    pub(super) fn terminate_gracefully(
        _child: &mut Child,
        _boundary: &mut ProcessBoundary,
        _identity: &ProcessIdentity,
        _graceful_timeout: Duration,
    ) -> io::Result<PlatformTerminationResult> {
        Ok(unsupported_result())
    }

    pub(super) fn force_terminate(
        _child: &mut Child,
        _boundary: &mut ProcessBoundary,
    ) -> io::Result<PlatformTerminationResult> {
        Ok(unsupported_result())
    }

    pub(super) fn terminate_snapshot(
        _identity: &ProcessIdentity,
        _graceful_timeout: Duration,
    ) -> io::Result<PlatformTerminationResult> {
        Ok(unsupported_result())
    }

    fn unsupported_result() -> PlatformTerminationResult {
        PlatformTerminationResult {
            outcome: ProcessTerminationOutcome::IdentityMismatch,
            graceful_requested: false,
            reaped: false,
        }
    }
}
