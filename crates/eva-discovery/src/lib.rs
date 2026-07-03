//! Discovery boundary for trusted sources and health probing.

pub mod cache;
pub mod health;
pub mod normalizer;
pub mod scanner;
pub mod service;
pub mod sources;

pub use cache::DiscoveryCache;
pub use health::{DiscoveryHealth, DiscoveryHealthStatus};
pub use normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
pub use scanner::{
    DiscoveryScanReport, DiscoverySource, DiscoverySourceReport, ProjectDiscoverySource,
};
pub use service::DiscoveryService;
