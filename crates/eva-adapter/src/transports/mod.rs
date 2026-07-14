//! 本模块提供 `mod` 相关实现。
//! Adapter transport boundaries.

/// 声明 `builtin` 子模块。
pub mod builtin;
/// 声明 `eventbus` 子模块。
pub mod eventbus;
/// 声明 `hardware` 子模块。
pub mod hardware;
/// 声明 `http` 子模块。
pub mod http;
/// 声明 `lua_capability` 子模块。
pub mod lua_capability;
/// 声明 `mcp` 子模块。
pub mod mcp;
/// 声明 `skill` 子模块。
pub mod skill;
/// 声明 `stdio` 子模块。
pub mod stdio;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "authorized Adapter transport implementations";
