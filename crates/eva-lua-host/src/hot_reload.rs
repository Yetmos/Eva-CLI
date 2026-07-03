//! Lua generation swap and rollback boundaries.

use eva_core::GenerationId;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Lua generation swap and rollback boundaries";

/// Read-only Lua generation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaGeneration {
    pub generation_id: GenerationId,
    pub script_count: usize,
}

impl LuaGeneration {
    pub fn new(generation_id: GenerationId, script_count: usize) -> Self {
        Self {
            generation_id,
            script_count,
        }
    }
}
