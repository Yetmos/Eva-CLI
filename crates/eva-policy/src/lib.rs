//! 中文：策略域解析、权限收窄、沙箱合并和运行时决策的统一边界。
//! Policy evaluation and permission narrowing boundary.

pub mod domains;
pub mod effective;
pub mod mutation_inventory;
pub mod permissions;
pub mod sandbox;

pub use domains::{
    AdapterPolicyDomain, CapabilityRetryPolicy, HardwareHotplugPolicy, HardwarePolicyDomain,
    HighRiskAction, McpServerPolicyDomain, McpToolPolicy, MemoryPolicyDomain, PolicyDecision,
    PolicyDomainSet, RedactionPolicyDomain, RetryPolicyDomain, RuntimePolicyDomain,
    RuntimePolicyGate, RuntimePolicyRequest, SkillPolicy,
};
pub use effective::{EffectivePolicy, PolicyLayer};
pub use mutation_inventory::{
    MutationDecision, MutationGate, MutationInventoryEntry, MutationOperation, MUTATION_INVENTORY,
};
pub use permissions::{PermissionSet, PermissionSetDiff};
pub use sandbox::SandboxPolicy;
