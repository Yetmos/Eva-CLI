//! Trusted discovery sources.

pub mod codex;
pub mod mcp;
pub mod omx;
pub mod path_commands;
pub mod project_adapters;
pub mod project_agents;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "trusted discovery source adapters";
