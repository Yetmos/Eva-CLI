//! 本模块提供 `lib` 相关实现。
//! Lua host boundary for sandboxed Agent execution.

/// 声明 `bindings` 子模块。
pub mod bindings;
/// 声明 `hot_reload` 子模块。
pub mod hot_reload;
/// 声明 `loader` 子模块。
pub mod loader;
/// 声明 `sandbox` 子模块。
pub mod sandbox;
/// 声明 `vm` 子模块。
pub mod vm;

pub use bindings::{LuaEventResult, LuaHost, LuaHostContext, LuaHostObservation};
pub use hot_reload::{
    LuaGeneration, LuaShadowCandidate, LuaShadowLoadReport, LuaShadowLoadStatus, LuaShadowLoader,
    LuaShadowScriptReport,
};
pub use loader::LuaScript;
pub use sandbox::LuaSandboxPolicy;
pub use vm::{LuaCancellationToken, LuaExecutionLimits, LuaVmAdapter, MluaVmAdapter};
