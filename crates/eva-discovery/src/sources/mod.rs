//! 可信发现来源适配器集合。
//! Trusted discovery sources.

/// 从可信本地适配器清单发现 Codex 工作流。
pub mod codex;
/// 从 MCP 适配器允许列表发现工具。
pub mod mcp;
/// 从本地工作区状态发现 OMX 工作流。
pub mod omx;
/// 从标准输入输出适配器清单发现 PATH 命令。
pub mod path_commands;
/// 项目适配器发现来源的职责标记。
pub mod project_adapters;
/// 项目 Agent 发现来源的职责标记。
pub mod project_agents;
/// 外部注册表发现边界。
pub mod registry;

/// 本模块的架构职责：提供受约束的可信发现来源适配器。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "trusted discovery source adapters";
