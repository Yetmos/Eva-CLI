//! Durable provider restart decisions.
//!
//! This module is intentionally free of process and storage I/O. It owns the
//! bounded policy calculation so the supervisor can persist the decision with
//! one CAS and both in-process and daemon-restart paths use identical rules.

use eva_config::{ProviderRestartConfig, ProviderRestartMode};

/// Stable window after a successful invocation before a crash-loop budget is
/// considered healthy again. A successful one-shot invocation records the
/// reset immediately; long-lived callers may use the window for observability.
pub const DEFAULT_STABLE_RUN_WINDOW_MS: u64 = 30_000;

/// Maximum delay accepted from a manifest after exponential growth and jitter.
pub const MAX_RESTART_BACKOFF_MS: u64 = 24 * 60 * 60 * 1_000;

/// Outcome used by the controller; transport-specific errors are deliberately
/// reduced to a stable success/failure bit at this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartOutcome {
    /// The provider completed a stable invocation.
    StableSuccess,
    /// The provider exited unsuccessfully or could not be started.
    Failure,
}

/// Durable decision made from the current persisted restart budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// No automatic restart is configured for this outcome.
    NoRestart,
    /// Restart is allowed after the supplied delay.
    Restart {
        /// Monotonic number of automatic restarts consumed after initial start.
        attempt: u32,
        /// Jittered delay in milliseconds.
        delay_ms: u64,
    },
    /// The configured budget is exhausted.
    BudgetExhausted,
}

/// Compute one deterministic restart decision.
///
/// The seed must be the stable provider/session identity, not a wall-clock
/// value. This keeps retries reproducible across daemon generations while
/// still spreading simultaneous crash loops.
pub fn decide_restart(
    config: ProviderRestartConfig,
    consumed_attempts: u32,
    outcome: RestartOutcome,
    seed: &str,
) -> RestartDecision {
    if matches!(outcome, RestartOutcome::StableSuccess) {
        return RestartDecision::NoRestart;
    }

    let enabled = matches!(
        config.mode,
        ProviderRestartMode::OnFailure | ProviderRestartMode::Always
    );
    if !enabled {
        return RestartDecision::NoRestart;
    }
    if consumed_attempts >= config.max_attempts {
        return RestartDecision::BudgetExhausted;
    }

    let attempt = consumed_attempts.saturating_add(1);
    let exponent = attempt.saturating_sub(1).min(63);
    let exponential = config
        .backoff_ms
        .saturating_mul(1_u64.checked_shl(exponent).unwrap_or(u64::MAX));
    let capped = exponential.min(MAX_RESTART_BACKOFF_MS);
    let jitter_window = (capped / 4).max(1);
    let span = jitter_window.saturating_mul(2).saturating_add(1);
    let jitter = fnv1a64(format!("{seed}:{attempt}").as_bytes()) % span;
    let jitter = jitter as i128 - i128::from(jitter_window);
    let delay = (i128::from(capped) + jitter).clamp(0, i128::from(MAX_RESTART_BACKOFF_MS));

    RestartDecision::Restart {
        attempt,
        delay_ms: delay as u64,
    }
}

/// Returns the deterministic due time without allowing clock arithmetic to
/// wrap. The caller persists the result together with the decision.
pub fn due_at_ms(now_ms: u128, delay_ms: u64) -> u128 {
    now_ms.saturating_add(u128::from(delay_ms))
}

/// Small stable hash used only for deterministic jitter; this is not a secret
/// or security primitive.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_restart_never_consumes_budget() {
        let decision = decide_restart(
            ProviderRestartConfig {
                mode: ProviderRestartMode::None,
                max_attempts: 0,
                backoff_ms: 0,
            },
            0,
            RestartOutcome::Failure,
            "session-a",
        );
        assert_eq!(decision, RestartDecision::NoRestart);
    }

    #[test]
    fn budget_is_monotonic_and_exhausts() {
        let config = ProviderRestartConfig {
            mode: ProviderRestartMode::OnFailure,
            max_attempts: 2,
            backoff_ms: 100,
        };
        let first = decide_restart(config, 0, RestartOutcome::Failure, "session-a");
        let second = decide_restart(config, 1, RestartOutcome::Failure, "session-a");
        let exhausted = decide_restart(config, 2, RestartOutcome::Failure, "session-a");
        assert!(matches!(first, RestartDecision::Restart { attempt: 1, .. }));
        assert!(matches!(
            second,
            RestartDecision::Restart { attempt: 2, .. }
        ));
        assert_eq!(exhausted, RestartDecision::BudgetExhausted);
    }

    #[test]
    fn jitter_is_deterministic_and_exponential() {
        let config = ProviderRestartConfig {
            mode: ProviderRestartMode::Always,
            max_attempts: 4,
            backoff_ms: 100,
        };
        let a = decide_restart(config, 0, RestartOutcome::Failure, "session-a");
        let b = decide_restart(config, 0, RestartOutcome::Failure, "session-a");
        let c = decide_restart(config, 1, RestartOutcome::Failure, "session-a");
        assert_eq!(a, b);
        let RestartDecision::Restart {
            delay_ms: first, ..
        } = a
        else {
            panic!("expected restart");
        };
        let RestartDecision::Restart {
            delay_ms: second, ..
        } = c
        else {
            panic!("expected restart");
        };
        assert!((75..=125).contains(&first));
        assert!((150..=250).contains(&second));
    }

    #[test]
    fn stable_success_never_schedules_restart() {
        let config = ProviderRestartConfig {
            mode: ProviderRestartMode::Always,
            max_attempts: 3,
            backoff_ms: 1,
        };
        assert_eq!(
            decide_restart(config, 2, RestartOutcome::StableSuccess, "session-a"),
            RestartDecision::NoRestart
        );
        assert_eq!(due_at_ms(u128::MAX, u64::MAX), u128::MAX);
    }
}
