//! 中文：已装配 Runtime 实例、代际摘要和关闭状态。
//! Runtime instance and summary state.

use crate::basic::{BasicRunOptions, BasicRunReport};
use crate::builder::{RuntimeMode, RuntimeOptions};
use crate::config_generation::RuntimeConfigGeneration;
use crate::services::{RuntimeServices, ServiceSummary};
use crate::shutdown::{ShutdownReport, ShutdownState};
use eva_config::ProjectConfig;
use eva_core::EvaError;

/// 中文：本模块拥有一个已装配代际的服务句柄、只读摘要和关闭标记。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "own the assembled Eva runtime instance";

/// 中文：Runtime 对外暴露的最小生命周期状态。
/// V0.3 runtime lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// 中文：运行时已经成功装配。
    Built,
    /// 中文：运行时已经收到关闭请求。
    Shutdown,
}

/// 中文：`eva inspect` 使用的只读运行时摘要。
/// Read-only summary shown by `eva inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSummary {
    /// 中文：本代际采用的运行模式。
    pub mode: RuntimeMode,
    /// 中文：当前生命周期状态。
    pub status: RuntimeStatus,
    /// 中文：本代际的稳定标识字符串。
    pub generation_id: String,
    /// 中文：项目配置声明的运行环境名称。
    pub environment: String,
    /// 中文：规范化后的项目根目录文本。
    pub project_root: String,
    /// 中文：项目声明的 Agent 总数。
    pub agents_total: usize,
    /// 中文：其中处于启用状态的 Agent 数量。
    pub agents_enabled: usize,
    /// 中文：项目声明的 Adapter 总数。
    pub adapters_total: usize,
    /// 中文：其中处于启用状态的 Adapter 数量。
    pub adapters_enabled: usize,
    /// 中文：项目声明的 capability 总数。
    pub capabilities_total: usize,
    /// 中文：其中处于启用状态的 capability 数量。
    pub capabilities_enabled: usize,
    /// 中文：规范化路由规则总数。
    pub routes_total: usize,
    /// 中文：已加载策略文档总数。
    pub policies_total: usize,
    /// 中文：本模式下各服务边界的只读状态摘要。
    pub services: Vec<ServiceSummary>,
}

/// 中文：当前代际的 Runtime 所有者，保持摘要、服务表和关闭状态一致。
/// Runtime owner for the current generation.
#[derive(Debug, Clone, PartialEq)]
pub struct Runtime {
    generation: RuntimeConfigGeneration,
    /// 中文：对 CLI 和诊断层公开的当前代际摘要。
    summary: RuntimeSummary,
    /// 中文：本代际已装配服务的状态容器。
    services: RuntimeServices,
    /// 中文：幂等关闭请求的内部计数和标记。
    shutdown: ShutdownState,
}

impl RuntimeStatus {
    /// 中文：返回用于协议和诊断输出的稳定状态名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Built => "built",
            Self::Shutdown => "shutdown",
        }
    }
}

impl RuntimeSummary {
    /// 中文：从项目配置、构建选项和已装配服务计算一次只读摘要。
    pub fn from_project(
        project: &ProjectConfig,
        options: &RuntimeOptions,
        services: &RuntimeServices,
    ) -> Self {
        Self {
            mode: options.mode,
            status: RuntimeStatus::Built,
            generation_id: options.generation_id.as_str().to_owned(),
            environment: project.eva.runtime.env.clone(),
            project_root: project.project_root.display().to_string(),
            agents_total: project.agents.len(),
            agents_enabled: project.agents.iter().filter(|agent| agent.enabled).count(),
            adapters_total: project.adapters.len(),
            adapters_enabled: project
                .adapters
                .iter()
                .filter(|adapter| adapter.enabled)
                .count(),
            capabilities_total: project.capabilities.len(),
            capabilities_enabled: project
                .capabilities
                .iter()
                .filter(|capability| capability.enabled)
                .count(),
            routes_total: project.routes.routes.len(),
            policies_total: project.policies.len(),
            services: services.summaries().to_vec(),
        }
    }
}

impl Runtime {
    /// 中文：从已一致构建的摘要和服务表创建 Runtime，关闭状态从未请求开始。
    pub fn new(
        generation: RuntimeConfigGeneration,
        summary: RuntimeSummary,
        services: RuntimeServices,
    ) -> Self {
        Self {
            generation,
            summary,
            services,
            shutdown: ShutdownState::default(),
        }
    }

    pub fn generation(&self) -> &RuntimeConfigGeneration {
        &self.generation
    }

    /// 中文：返回当前代际摘要的只读视图。
    pub fn summary(&self) -> &RuntimeSummary {
        &self.summary
    }

    /// 中文：返回已装配服务状态的只读视图。
    pub fn services(&self) -> &RuntimeServices {
        &self.services
    }

    /// 中文：使用当前摘要运行 V1.0 基础内存事件循环和任务诊断。
    /// Runs the V1.0 basic in-memory event loop with task diagnostics.
    pub fn run_basic(
        &self,
        project: &ProjectConfig,
        options: BasicRunOptions,
    ) -> Result<BasicRunReport, EvaError> {
        crate::basic::run_basic(&self.summary, project, options)
    }

    /// 中文：记录幂等关闭请求，并同步把公开摘要状态推进为 `Shutdown`。
    /// Marks the no-op runtime as shutdown. The operation is idempotent.
    pub fn shutdown(&mut self) -> ShutdownReport {
        let report = self.shutdown.request();
        self.summary.status = RuntimeStatus::Shutdown;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeBuilder;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 中文：返回 Runtime 测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 中文：验证重复关闭只增加请求计数，不产生不同的生命周期结果。
    fn shutdown_is_idempotent() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut runtime = RuntimeBuilder::new().build(&project).unwrap();

        let first = runtime.shutdown();
        let second = runtime.shutdown();

        assert!(!first.already_shutdown);
        assert!(second.already_shutdown);
        assert_eq!(runtime.summary().status, RuntimeStatus::Shutdown);
    }
}
