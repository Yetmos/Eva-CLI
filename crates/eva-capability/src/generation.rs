//! Capability generation swaps and rollback-safe handles.

use eva_core::GenerationId;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability generation swaps and rollback-safe handles";

/// Read-only capability generation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGeneration {
    pub generation_id: GenerationId,
    pub capability_count: usize,
}

impl CapabilityGeneration {
    pub fn new(generation_id: GenerationId, capability_count: usize) -> Self {
        Self {
            generation_id,
            capability_count,
        }
    }
}
