//! Lua host boundary for sandboxed Agent execution.

pub mod bindings;
pub mod hot_reload;
pub mod loader;
pub mod sandbox;
pub mod vm;

pub use bindings::{LuaEventResult, LuaHost, LuaHostContext, LuaHostObservation};
pub use hot_reload::LuaGeneration;
pub use loader::LuaScript;
pub use sandbox::LuaSandboxPolicy;
pub use vm::{LuaCancellationToken, LuaExecutionLimits, LuaVmAdapter, MluaVmAdapter};
