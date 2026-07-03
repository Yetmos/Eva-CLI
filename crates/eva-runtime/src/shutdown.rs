//! Shutdown state for runtime generations.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "coordinated shutdown and draining";

/// Idempotent shutdown marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShutdownState {
    requested: bool,
    request_count: u64,
}

/// Result of requesting shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub already_shutdown: bool,
    pub request_count: u64,
    pub phase: String,
}

impl ShutdownState {
    pub fn request(&mut self) -> ShutdownReport {
        let already_shutdown = self.requested;
        self.requested = true;
        self.request_count += 1;

        ShutdownReport {
            already_shutdown,
            request_count: self.request_count,
            phase: if already_shutdown {
                "already_shutdown".to_owned()
            } else {
                "noop_shutdown_recorded".to_owned()
            },
        }
    }

    pub fn is_requested(&self) -> bool {
        self.requested
    }
}
