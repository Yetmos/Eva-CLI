//! Capability registry and host API boundary.

pub mod generation;
pub mod host_api;
pub mod registry;
pub mod router;
pub mod selection;

pub use generation::CapabilityGeneration;
pub use host_api::CapabilityHostApi;
pub use registry::{CapabilityDescriptor, CapabilityRegistry};
pub use router::CapabilityRouter;
pub use selection::{
    CapabilityProviderCandidate, CapabilityProviderPlan, CapabilityProviderSelection,
    CapabilityProviderSource,
};
