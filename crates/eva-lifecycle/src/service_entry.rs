//! Direct daemon service entrypoint and cooperative stop bridge.
//!
//! The module owns only the process boundary. Runtime code decides how to
//! drain work after observing [`ServiceStopToken::is_requested`]. No signal or
//! SCM callback performs blocking work, allocates, or calls runtime code.

use crate::{ServiceHostPlatform, ServiceManagerKind};
use eva_core::EvaError;
#[cfg(windows)]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(windows)]
use std::sync::Mutex;

const STATE_START_PENDING: u8 = 0;
const STATE_RUNNING: u8 = 1;
const STATE_STOP_PENDING: u8 = 2;
const STATE_STOPPED: u8 = 3;

/// Cooperative process-stop token shared with the daemon loop.
#[derive(Debug, Clone, Default)]
pub struct ServiceStopToken {
    requested: Arc<AtomicBool>,
    #[cfg(unix)]
    signal_generation: Option<usize>,
}

impl ServiceStopToken {
    /// Creates a clear stop token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests shutdown. Repeated requests are intentionally idempotent.
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }

    /// Returns whether a stop request has been observed.
    pub fn is_requested(&self) -> bool {
        if self.requested.load(Ordering::Acquire) {
            return true;
        }
        #[cfg(unix)]
        if let Some(generation) = self.signal_generation {
            return UNIX_SIGNAL_REQUESTED_GENERATION.load(Ordering::Acquire) == generation;
        }
        false
    }

    #[cfg(unix)]
    fn for_signal_generation(generation: usize) -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
            signal_generation: Some(generation),
        }
    }
}

/// Service lifecycle state exposed for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEntryState {
    /// The service callback has been entered but has not reported readiness.
    StartPending,
    /// The service callback reported readiness.
    Running,
    /// A stop request was delivered to the callback.
    StopPending,
    /// The callback returned and the process is leaving the service boundary.
    Stopped,
}

impl ServiceEntryState {
    const fn from_raw(value: u8) -> Self {
        match value {
            STATE_RUNNING => Self::Running,
            STATE_STOP_PENDING => Self::StopPending,
            STATE_STOPPED => Self::Stopped,
            _ => Self::StartPending,
        }
    }
}

type ReadyReporter = Arc<dyn Fn() -> Result<(), EvaError> + Send + Sync>;

#[cfg(windows)]
type ServiceEntryHandler = Box<dyn FnOnce(ServiceEntryContext) -> Result<(), EvaError> + Send>;

/// Context passed to a direct service-entrypoint closure.
#[derive(Clone)]
pub struct ServiceEntryContext {
    token: ServiceStopToken,
    state: Arc<AtomicU8>,
    ready_reporter: Option<ReadyReporter>,
}

impl std::fmt::Debug for ServiceEntryContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceEntryContext")
            .field("stop_requested", &self.token.is_requested())
            .field("state", &self.state())
            .finish()
    }
}

impl ServiceEntryContext {
    #[allow(dead_code)]
    fn new(token: ServiceStopToken, ready_reporter: Option<ReadyReporter>) -> Self {
        Self {
            token,
            state: Arc::new(AtomicU8::new(STATE_START_PENDING)),
            ready_reporter,
        }
    }

    /// Returns the cooperative stop token owned by this service invocation.
    pub fn stop_token(&self) -> &ServiceStopToken {
        &self.token
    }

    /// Publishes readiness. On Windows this transitions SCM to RUNNING; on
    /// Unix it only records the local state because no service controller API
    /// exists at this boundary.
    pub fn report_ready(&self) -> Result<(), EvaError> {
        if self.token.is_requested() {
            return Err(EvaError::conflict(
                "service entrypoint cannot report ready after a stop request",
            ));
        }
        match self.state.compare_exchange(
            STATE_START_PENDING,
            STATE_RUNNING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(STATE_RUNNING) => return Ok(()),
            Err(_) => {
                return Err(EvaError::conflict(
                    "service entrypoint cannot report ready outside START_PENDING",
                ));
            }
        }
        if let Some(reporter) = self.ready_reporter.as_ref() {
            if let Err(error) = reporter() {
                let _ = self.state.compare_exchange(
                    STATE_RUNNING,
                    STATE_START_PENDING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                return Err(error);
            }
        }
        if self.token.is_requested() {
            let _ = self.state.compare_exchange(
                STATE_RUNNING,
                STATE_STOP_PENDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(EvaError::conflict(
                "service entrypoint received a stop request while reporting ready",
            ));
        }
        Ok(())
    }

    /// Returns the latest service boundary state.
    pub fn state(&self) -> ServiceEntryState {
        ServiceEntryState::from_raw(self.state.load(Ordering::Acquire))
    }

    #[cfg(test)]
    fn request_stop(&self) {
        self.token.request();
        self.state.store(STATE_STOP_PENDING, Ordering::Release);
    }

    fn mark_stopped(&self) {
        self.state.store(STATE_STOPPED, Ordering::Release);
    }
}

/// Runs one direct service entrypoint.
///
/// On Unix the closure is invoked directly after installing temporary
/// SIGTERM/SIGINT handlers. On Windows the process enters
/// `StartServiceCtrlDispatcherW`; SCM controls only set the stop token and
/// publish STOP_PENDING, leaving cooperative draining to the closure.
pub fn run_service_entrypoint<F>(
    kind: ServiceManagerKind,
    service_name: impl AsRef<str>,
    handler: F,
) -> Result<(), EvaError>
where
    F: FnOnce(ServiceEntryContext) -> Result<(), EvaError> + Send + 'static,
{
    validate_entrypoint_identity(kind, service_name.as_ref())?;
    #[cfg(unix)]
    {
        let (_signals, token) = UnixSignalGuard::install()?;
        let context = ServiceEntryContext::new(token, None);
        let result = handler(context.clone());
        context.mark_stopped();
        result
    }
    #[cfg(windows)]
    {
        run_windows_service_entrypoint(kind, service_name.as_ref(), handler)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let context = ServiceEntryContext::new(ServiceStopToken::new(), None);
        let result = handler(context.clone());
        context.mark_stopped();
        result
    }
}

fn validate_entrypoint_identity(
    kind: ServiceManagerKind,
    service_name: &str,
) -> Result<(), EvaError> {
    if service_name.trim().is_empty() || service_name.trim() != service_name {
        return Err(EvaError::invalid_argument(
            "service entrypoint service_name cannot be empty or padded",
        ));
    }
    if service_name
        .chars()
        .any(|value| value == '\0' || value.is_control())
    {
        return Err(EvaError::invalid_argument(
            "service entrypoint service_name contains a control character",
        ));
    }
    let expected = ServiceHostPlatform::current().service_manager_kind();
    if kind == ServiceManagerKind::Fake || expected != Some(kind) {
        return Err(EvaError::unsupported(
            "service entrypoint kind does not match the current host",
        )
        .with_context("host_platform", ServiceHostPlatform::current().as_str())
        .with_context("requested_kind", kind.as_str())
        .with_context(
            "expected_kind",
            expected
                .map(ServiceManagerKind::as_str)
                .unwrap_or("unsupported"),
        ));
    }
    Ok(())
}

#[cfg(unix)]
struct UnixSignalGuard {
    previous_term: libc::sigaction,
    previous_int: libc::sigaction,
    generation: usize,
}

#[cfg(unix)]
static NEXT_UNIX_SIGNAL_GENERATION: AtomicUsize = AtomicUsize::new(1);
#[cfg(unix)]
static ACTIVE_UNIX_SIGNAL_GENERATION: AtomicUsize = AtomicUsize::new(0);
#[cfg(unix)]
static UNIX_SIGNAL_REQUESTED_GENERATION: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
impl UnixSignalGuard {
    fn install() -> Result<(Self, ServiceStopToken), EvaError> {
        let generation = loop {
            let candidate = NEXT_UNIX_SIGNAL_GENERATION.fetch_add(1, Ordering::Relaxed);
            if candidate != 0 {
                break candidate;
            }
        };
        if ACTIVE_UNIX_SIGNAL_GENERATION
            .compare_exchange(0, generation, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(EvaError::conflict(
                "another service entrypoint already owns the process signal bridge",
            ));
        }
        UNIX_SIGNAL_REQUESTED_GENERATION.store(0, Ordering::Release);
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = unix_signal_handler as *const () as usize;
        action.sa_flags = 0;
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        let mut previous_term: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut previous_int: libc::sigaction = unsafe { std::mem::zeroed() };
        let term_result = unsafe { libc::sigaction(libc::SIGTERM, &action, &mut previous_term) };
        if term_result != 0 {
            let error = std::io::Error::last_os_error();
            let _ = ACTIVE_UNIX_SIGNAL_GENERATION.compare_exchange(
                generation,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(EvaError::unavailable(
                "failed to install service entrypoint SIGTERM handler",
            )
            .with_context("io_error", error.to_string()));
        }
        let int_result = unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous_int) };
        if int_result != 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::sigaction(libc::SIGTERM, &previous_term, std::ptr::null_mut());
            }
            let _ = ACTIVE_UNIX_SIGNAL_GENERATION.compare_exchange(
                generation,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(EvaError::unavailable(
                "failed to install service entrypoint SIGINT handler",
            )
            .with_context("io_error", error.to_string()));
        }
        Ok((
            Self {
                previous_term,
                previous_int,
                generation,
            },
            ServiceStopToken::for_signal_generation(generation),
        ))
    }
}

#[cfg(unix)]
impl Drop for UnixSignalGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGTERM, &self.previous_term, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &self.previous_int, std::ptr::null_mut());
        }
        let _ = ACTIVE_UNIX_SIGNAL_GENERATION.compare_exchange(
            self.generation,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

#[cfg(unix)]
extern "C" fn unix_signal_handler(_signal: libc::c_int) {
    let generation = ACTIVE_UNIX_SIGNAL_GENERATION.load(Ordering::Acquire);
    if generation != 0 {
        UNIX_SIGNAL_REQUESTED_GENERATION.store(generation, Ordering::Release);
    }
}

#[cfg(windows)]
struct WindowsInvocation {
    service_name: Vec<u16>,
    token: ServiceStopToken,
    state: Arc<AtomicU8>,
    handler: Mutex<Option<ServiceEntryHandler>>,
    result: Mutex<Option<Result<(), EvaError>>>,
    status_handle: AtomicUsize,
    status_done: Arc<AtomicBool>,
    checkpoint: Arc<AtomicU32>,
}

#[cfg(windows)]
static ACTIVE_WINDOWS_INVOCATION: std::sync::atomic::AtomicPtr<WindowsInvocation> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(windows)]
fn run_windows_service_entrypoint<F>(
    _kind: ServiceManagerKind,
    service_name: &str,
    handler: F,
) -> Result<(), EvaError>
where
    F: FnOnce(ServiceEntryContext) -> Result<(), EvaError> + Send + 'static,
{
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT};
    use windows_sys::Win32::System::Services::{StartServiceCtrlDispatcherW, SERVICE_TABLE_ENTRYW};

    let mut service_name_wide = std::ffi::OsStr::new(service_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let invocation = Box::new(WindowsInvocation {
        service_name: service_name_wide.clone(),
        token: ServiceStopToken::new(),
        state: Arc::new(AtomicU8::new(STATE_START_PENDING)),
        handler: Mutex::new(Some(Box::new(handler))),
        result: Mutex::new(None),
        status_handle: AtomicUsize::new(0),
        status_done: Arc::new(AtomicBool::new(false)),
        checkpoint: Arc::new(AtomicU32::new(1)),
    });
    let pointer = Box::into_raw(invocation);
    if ACTIVE_WINDOWS_INVOCATION
        .compare_exchange(
            std::ptr::null_mut(),
            pointer,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        unsafe { drop(Box::from_raw(pointer)) };
        return Err(EvaError::conflict(
            "another Windows service entrypoint is already active",
        ));
    }

    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: service_name_wide.as_mut_ptr(),
            lpServiceProc: Some(windows_service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    let dispatch_ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } != 0;
    let dispatch_error = if dispatch_ok {
        0
    } else {
        unsafe { GetLastError() }
    };
    let invocation = unsafe { Box::from_raw(pointer) };
    ACTIVE_WINDOWS_INVOCATION.store(std::ptr::null_mut(), Ordering::Release);
    let result = invocation
        .result
        .into_inner()
        .map_err(|_| EvaError::internal("Windows service result lock is poisoned"))?
        .unwrap_or_else(|| {
            if dispatch_error == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
                Err(EvaError::unavailable(
                    "process was not started by the Windows Service Control Manager",
                ))
            } else {
                Err(EvaError::unavailable(
                    "Windows service dispatcher did not invoke the service callback",
                )
                .with_context("win32_error", dispatch_error.to_string()))
            }
        });
    if !dispatch_ok && dispatch_error != ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
        return Err(EvaError::unavailable("StartServiceCtrlDispatcherW failed")
            .with_context("win32_error", dispatch_error.to_string()));
    }
    result
}

#[cfg(windows)]
unsafe extern "system" fn windows_service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Services::{
        RegisterServiceCtrlHandlerExW, SetServiceStatus, SERVICE_ACCEPT_PRESHUTDOWN,
        SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
        SERVICE_STATUS, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_WIN32_OWN_PROCESS,
    };

    let pointer = ACTIVE_WINDOWS_INVOCATION.load(Ordering::Acquire);
    if pointer.is_null() {
        return;
    }
    let invocation = &*pointer;
    let status_handle = RegisterServiceCtrlHandlerExW(
        invocation.service_name.as_ptr(),
        Some(windows_service_control_handler),
        pointer.cast(),
    );
    if status_handle.is_null() {
        let error = std::io::Error::last_os_error();
        *invocation
            .result
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Err(EvaError::permission_denied(
            "RegisterServiceCtrlHandlerExW failed",
        )
        .with_context("io_error", error.to_string())));
        return;
    }
    invocation
        .status_handle
        .store(status_handle as usize, Ordering::Release);
    let start_status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_START_PENDING,
        dwControlsAccepted: 0,
        dwWin32ExitCode: ERROR_SUCCESS,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 1,
        dwWaitHint: 20_000,
    };
    if SetServiceStatus(status_handle, &start_status) == 0 {
        let error = std::io::Error::last_os_error();
        *invocation
            .result
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Err(EvaError::unavailable(
            "SetServiceStatus START_PENDING failed",
        )
        .with_context("io_error", error.to_string())));
        return;
    }

    // Raw Windows handles are represented as pointers and are intentionally
    // captured as an integer so the ready hook remains Send + Sync.
    let status_handle_for_ready = status_handle as usize;
    let token_for_ready = invocation.token.clone();
    let state_for_ready = invocation.state.clone();
    let ready_reporter: ReadyReporter = Arc::new(move || {
        if token_for_ready.is_requested()
            || state_for_ready.load(Ordering::Acquire) != STATE_RUNNING
        {
            return Err(EvaError::conflict(
                "service received a stop request before reporting RUNNING",
            ));
        }
        let running_status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: SERVICE_RUNNING,
            dwControlsAccepted: SERVICE_ACCEPT_STOP
                | SERVICE_ACCEPT_SHUTDOWN
                | SERVICE_ACCEPT_PRESHUTDOWN,
            dwWin32ExitCode: ERROR_SUCCESS,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: 0,
            dwWaitHint: 0,
        };
        if SetServiceStatus(
            status_handle_for_ready as windows_sys::Win32::System::Services::SERVICE_STATUS_HANDLE,
            &running_status,
        ) == 0
        {
            let error = std::io::Error::last_os_error();
            return Err(EvaError::unavailable("SetServiceStatus RUNNING failed")
                .with_context("io_error", error.to_string()));
        }
        if token_for_ready.is_requested()
            || state_for_ready.load(Ordering::Acquire) != STATE_RUNNING
        {
            state_for_ready.store(STATE_STOP_PENDING, Ordering::Release);
            let pending_status = SERVICE_STATUS {
                dwServiceType: SERVICE_WIN32_OWN_PROCESS,
                dwCurrentState: SERVICE_STOP_PENDING,
                dwControlsAccepted: 0,
                dwWin32ExitCode: ERROR_SUCCESS,
                dwServiceSpecificExitCode: 0,
                dwCheckPoint: 1,
                dwWaitHint: 20_000,
            };
            if SetServiceStatus(
                status_handle_for_ready
                    as windows_sys::Win32::System::Services::SERVICE_STATUS_HANDLE,
                &pending_status,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                return Err(EvaError::unavailable(
                    "SetServiceStatus STOP_PENDING after readiness race failed",
                )
                .with_context("io_error", error.to_string()));
            }
            return Err(EvaError::conflict(
                "service received a stop request while reporting RUNNING",
            ));
        }
        Ok(())
    });
    let context = ServiceEntryContext {
        token: invocation.token.clone(),
        state: invocation.state.clone(),
        ready_reporter: Some(ready_reporter),
    };
    let watchdog_done = invocation.status_done.clone();
    let watchdog_state = invocation.state.clone();
    let watchdog_handle = invocation.status_handle.load(Ordering::Acquire);
    let watchdog_checkpoint = invocation.checkpoint.clone();
    let watchdog = std::thread::spawn(move || {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Services::{
            SetServiceStatus, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STOP_PENDING,
            SERVICE_WIN32_OWN_PROCESS,
        };
        while !watchdog_done.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if watchdog_done.load(Ordering::Acquire) {
                break;
            }
            let state = watchdog_state.load(Ordering::Acquire);
            if state != STATE_START_PENDING && state != STATE_STOP_PENDING {
                continue;
            }
            let checkpoint = watchdog_checkpoint.fetch_add(1, Ordering::AcqRel) + 1;
            let status = SERVICE_STATUS {
                dwServiceType: SERVICE_WIN32_OWN_PROCESS,
                dwCurrentState: if state == STATE_STOP_PENDING {
                    SERVICE_STOP_PENDING
                } else {
                    SERVICE_START_PENDING
                },
                dwControlsAccepted: 0,
                dwWin32ExitCode: ERROR_SUCCESS,
                dwServiceSpecificExitCode: 0,
                dwCheckPoint: checkpoint,
                dwWaitHint: 20_000,
            };
            unsafe {
                SetServiceStatus(
                    watchdog_handle as windows_sys::Win32::System::Services::SERVICE_STATUS_HANDLE,
                    &status,
                );
            }
        }
    });
    let mut handler_result = invocation
        .handler
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .map(|handler| handler(context.clone()))
        .unwrap_or_else(|| {
            Err(EvaError::internal(
                "Windows service handler was already consumed",
            ))
        });
    context.mark_stopped();
    invocation.status_done.store(true, Ordering::Release);
    let _ = watchdog.join();
    let stopped_status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_STOPPED,
        dwControlsAccepted: 0,
        dwWin32ExitCode: if handler_result.is_ok() {
            ERROR_SUCCESS
        } else {
            windows_sys::Win32::Foundation::ERROR_SERVICE_SPECIFIC_ERROR
        },
        dwServiceSpecificExitCode: if handler_result.is_ok() { 0 } else { 1 },
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    if SetServiceStatus(status_handle, &stopped_status) == 0 {
        let status_error = std::io::Error::last_os_error();
        handler_result = match handler_result {
            Ok(()) => Err(EvaError::unavailable("SetServiceStatus STOPPED failed")
                .with_context("io_error", status_error.to_string())),
            Err(handler_error) => Err(handler_error
                .with_context("set_service_status_stopped_error", status_error.to_string())),
        };
    }
    *invocation
        .result
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(handler_result);
}

#[cfg(windows)]
unsafe extern "system" fn windows_service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut std::ffi::c_void,
    context: *mut std::ffi::c_void,
) -> u32 {
    use windows_sys::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, ERROR_SUCCESS};
    use windows_sys::Win32::System::Services::{
        SetServiceStatus, SERVICE_CONTROL_PRESHUTDOWN, SERVICE_CONTROL_SHUTDOWN,
        SERVICE_CONTROL_STOP, SERVICE_STATUS, SERVICE_STOP_PENDING, SERVICE_WIN32_OWN_PROCESS,
    };
    if context.is_null() {
        return ERROR_CALL_NOT_IMPLEMENTED;
    }
    let invocation = &*(context as *const WindowsInvocation);
    if matches!(
        control,
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN | SERVICE_CONTROL_PRESHUTDOWN
    ) {
        invocation.token.requested.store(true, Ordering::Release);
        invocation
            .state
            .store(STATE_STOP_PENDING, Ordering::Release);
        let status_handle = invocation.status_handle.load(Ordering::Acquire);
        if status_handle != 0 {
            let status = SERVICE_STATUS {
                dwServiceType: SERVICE_WIN32_OWN_PROCESS,
                dwCurrentState: SERVICE_STOP_PENDING,
                dwControlsAccepted: 0,
                dwWin32ExitCode: ERROR_SUCCESS,
                dwServiceSpecificExitCode: 0,
                dwCheckPoint: 1,
                dwWaitHint: 20_000,
            };
            SetServiceStatus(
                status_handle as windows_sys::Win32::System::Services::SERVICE_STATUS_HANDLE,
                &status,
            );
        }
        return ERROR_SUCCESS;
    }
    ERROR_CALL_NOT_IMPLEMENTED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_token_is_cloneable_and_idempotent() {
        let token = ServiceStopToken::new();
        let clone = token.clone();
        assert!(!token.is_requested());
        clone.request();
        assert!(token.is_requested());
        token.request();
        assert!(clone.is_requested());
    }

    #[test]
    fn context_ready_and_stop_states_are_explicit() {
        let context = ServiceEntryContext::new(ServiceStopToken::new(), None);
        assert_eq!(context.state(), ServiceEntryState::StartPending);
        context.report_ready().expect("ready transition");
        assert_eq!(context.state(), ServiceEntryState::Running);
        context.request_stop();
        assert!(context.stop_token().is_requested());
        assert_eq!(context.state(), ServiceEntryState::StopPending);
        context.mark_stopped();
        assert_eq!(context.state(), ServiceEntryState::Stopped);
    }

    #[test]
    fn readiness_stop_race_remains_stop_pending() {
        let token = ServiceStopToken::new();
        let reporter_token = token.clone();
        let reporter: ReadyReporter = Arc::new(move || {
            reporter_token.request();
            Ok(())
        });
        let context = ServiceEntryContext::new(token, Some(reporter));

        assert_eq!(
            context
                .report_ready()
                .expect_err("concurrent stop must reject readiness")
                .kind(),
            eva_core::ErrorKind::Conflict
        );
        assert!(context.stop_token().is_requested());
        assert_eq!(context.state(), ServiceEntryState::StopPending);
    }

    #[cfg(unix)]
    #[test]
    fn unix_signal_guard_rejects_nested_owner_and_restores() {
        let (first, first_token) = UnixSignalGuard::install().expect("first owner");
        let second_error = match UnixSignalGuard::install() {
            Ok(_) => panic!("nested owner must fail"),
            Err(error) => error,
        };
        assert_eq!(second_error.kind(), eva_core::ErrorKind::Conflict);
        unix_signal_handler(libc::SIGTERM);
        assert!(first_token.is_requested());
        let first_generation = first.generation;
        drop(first);
        let (third, third_token) = UnixSignalGuard::install().expect("restored owner");
        UNIX_SIGNAL_REQUESTED_GENERATION.store(first_generation, Ordering::Release);
        assert!(
            !third_token.is_requested(),
            "a delayed signal from the previous generation must be ignored"
        );
        drop(third);
    }

    #[test]
    fn service_identity_rejects_fake_and_padded_names() {
        assert!(validate_entrypoint_identity(ServiceManagerKind::Fake, "eva").is_err());
        assert!(validate_entrypoint_identity(ServiceManagerKind::Systemd, " eva ").is_err());
    }
}
