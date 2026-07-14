//! 中文：Agent、Adapter 和 capability 清单的配置边界。
//! Manifest configuration boundaries.

pub mod adapter;
pub mod agent;
pub mod capability;

/// 中文：本模块负责把三类清单规范化为下游可直接注册的类型化数据。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "normalize Agent, Adapter, and capability manifests";
