//! 本模块提供 `lib` 相关实现。
//! Capability registry and host API boundary.

/// 声明 `gate` 子模块。
pub mod gate;
/// 声明 `generation` 子模块。
pub mod generation;
/// 声明 `host_api` 子模块。
pub mod host_api;
/// 声明 `registry` 子模块。
pub mod registry;
/// 声明 `router` 子模块。
pub mod router;
/// 声明 `selection` 子模块。
pub mod selection;

pub use gate::CapabilityPermissionGate;
pub use generation::CapabilityGeneration;
pub use host_api::CapabilityHostApi;
pub use registry::{CapabilityDescriptor, CapabilityRegistry};
pub use router::CapabilityRouter;
pub use selection::{
    CapabilityProviderCandidate, CapabilityProviderPlan, CapabilityProviderSelection,
    CapabilityProviderSource,
};
