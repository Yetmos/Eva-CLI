//! 本模块提供 `generation` 相关实现。
//! Capability generation swaps and rollback-safe handles.

use eva_core::GenerationId;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability generation swaps and rollback-safe handles";

/// 表示 `CapabilityGeneration` 数据结构。
/// Read-only capability generation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGeneration {
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: GenerationId,
    /// 记录 `capability_count` 字段对应的值。
    pub capability_count: usize,
}

impl CapabilityGeneration {
    /// 创建并初始化当前类型的实例。
    pub fn new(generation_id: GenerationId, capability_count: usize) -> Self {
        Self {
            generation_id,
            capability_count,
        }
    }
}
