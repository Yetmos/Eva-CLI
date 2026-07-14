//! 面向用户的 Eva 命令行边界，集中导出命令模块与进程入口。
//! User-facing CLI boundary.

/// Adapter 命令的公开边界占位模块。
pub mod adapter;
/// Agent 命令的公开边界占位模块。
pub mod agent;
/// Capability 命令的公开边界占位模块。
pub mod capability;
/// 环境与配置自检报告模块。
pub mod doctor;
/// 事件注入命令的公开边界占位模块。
pub mod emit;
/// 配置和运行时检查报告模块。
pub mod inspect;
/// CLI 参数解析、命令执行和输出契约模块。
pub mod run;

/// 根二进制使用的入口；保持薄包装以让真实 CLI 行为可由库测试覆盖。
/// Entry point used by the root binary shim.
pub fn run() {
    run::run();
}
