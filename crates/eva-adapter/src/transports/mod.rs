//! Adapter transport boundaries.

pub mod builtin;
pub mod eventbus;
pub mod hardware;
pub mod http;
pub mod lua_capability;
pub mod mcp;
pub mod skill;
pub mod stdio;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "authorized Adapter transport implementations";
