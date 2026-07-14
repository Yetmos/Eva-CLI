//! 中文：运行时代际的幂等关闭请求状态。
//! Shutdown state for runtime generations.

/// 中文：本模块负责记录协调关闭和排空流程的最小状态。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "coordinated shutdown and draining";

/// 中文：可重复请求的关闭标记和调用计数。
/// Idempotent shutdown marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShutdownState {
    /// 中文：是否已经收到至少一次关闭请求。
    requested: bool,
    /// 中文：累计关闭请求次数，用于诊断重复调用。
    request_count: u64,
}

/// 中文：一次关闭请求的幂等结果摘要。
/// Result of requesting shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// 中文：发起本次请求前是否已经处于关闭状态。
    pub already_shutdown: bool,
    /// 中文：包括本次在内的累计请求次数。
    pub request_count: u64,
    /// 中文：供 CLI 显示的稳定关闭阶段名称。
    pub phase: String,
}

impl ShutdownState {
    /// 中文：记录一次关闭请求；首次和重复调用返回不同阶段，但最终状态始终为已请求。
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

    /// 中文：判断运行时是否已经收到关闭请求。
    pub fn is_requested(&self) -> bool {
        self.requested
    }
}
