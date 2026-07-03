//! Side-effect-free discovery health probing.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "health probing for discovered candidates";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryHealthStatus {
    Seen,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryHealth {
    pub candidate_id: String,
    pub status: DiscoveryHealthStatus,
    pub message: String,
}

impl DiscoveryHealthStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::Rejected => "rejected",
        }
    }
}
