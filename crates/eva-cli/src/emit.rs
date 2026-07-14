//! CLI 事件注入命令的公开边界占位；校验和发布由内部命令模块完成。
//! CLI emit command placeholders.

/// 本模块的架构职责，用于文档和边界检查，不参与运行时分支。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "emit validated ingress events";
