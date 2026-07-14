//! 中文：按运行模式把已校验项目配置装配成具体 Runtime。
//! Runtime builder for V0.3 no-op composition.

use crate::runtime::{Runtime, RuntimeSummary};
use crate::services::RuntimeServices;
use eva_config::ProjectConfig;
use eva_core::{EvaError, GenerationId};
use std::fmt;

/// 中文：本模块是运行时组合根，负责选择服务集合并建立代际摘要。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "compose concrete runtime services from validated configuration";

/// 中文：组合根支持的运行时执行模式。
/// Runtime execution mode selected by the composition root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// 中文：只构建摘要和边界，不启动具有副作用的服务。
    /// V0.3 mode: build summaries and boundaries without starting side effects.
    Noop,
    /// 中文：装配用于基础示例的 V0.4 内存事件循环。
    /// V0.4 mode: wire the in-memory event loop for example execution.
    InMemoryV04,
    /// 中文：在 V0.4 基础上加入任务诊断能力。
    /// V0.5 mode: wire the in-memory loop plus task diagnostics.
    InMemoryV05,
    /// 中文：在 V0.5 循环上暴露稳定的 V1.0 核心发布面。
    /// V1.0 mode: stabilize the core release surface over the V0.5 loop.
    InMemoryV10,
}

/// 中文：可被 CLI 检查和覆盖的稳定运行时构建选项。
/// Runtime builder options that are stable enough for CLI inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// 中文：要装配的服务和执行能力级别。
    pub mode: RuntimeMode,
    /// 中文：本次运行时实例绑定的代际标识。
    pub generation_id: GenerationId,
}

/// 中文：从已通过配置校验的项目构建 Eva 运行时。
/// Builds an Eva runtime from already validated project configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBuilder {
    /// 中文：构建时使用的模式与代际选项。
    options: RuntimeOptions,
}

impl Default for RuntimeOptions {
    /// 中文：默认采用无副作用的 Noop 模式。
    fn default() -> Self {
        Self::noop()
    }
}

impl Default for RuntimeBuilder {
    /// 中文：默认创建 Noop 组合根。
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeMode {
    /// 中文：返回用于 CLI 和诊断输出的稳定模式名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Noop => "noop",
            Self::InMemoryV04 => "in_memory_v0.4",
            Self::InMemoryV05 => "in_memory_v0.5",
            Self::InMemoryV10 => "in_memory_v1.0",
        }
    }
}

impl fmt::Display for RuntimeMode {
    /// 中文：使用稳定模式名称格式化运行模式。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl RuntimeOptions {
    /// 中文：返回 V0.3 无副作用模式及其固定默认代际。
    /// Returns the V0.3 no-op runtime options.
    pub fn noop() -> Self {
        Self {
            mode: RuntimeMode::Noop,
            generation_id: GenerationId::parse("noop-v0.3")
                .expect("static V0.3 generation id is valid"),
        }
    }

    /// 中文：返回 V0.4 内存运行模式及其固定默认代际。
    /// Returns V0.4 in-memory runtime options.
    pub fn in_memory_v04() -> Self {
        Self {
            mode: RuntimeMode::InMemoryV04,
            generation_id: GenerationId::parse("basic-v0.4")
                .expect("static V0.4 generation id is valid"),
        }
    }

    /// 中文：返回 V0.5 内存运行模式及其固定默认代际。
    /// Returns V0.5 in-memory runtime options.
    pub fn in_memory_v05() -> Self {
        Self {
            mode: RuntimeMode::InMemoryV05,
            generation_id: GenerationId::parse("basic-v0.5")
                .expect("static V0.5 generation id is valid"),
        }
    }

    /// 中文：返回 V1.0 核心发布模式及其固定默认代际。
    /// Returns V1.0 core release runtime options.
    pub fn in_memory_v10() -> Self {
        Self {
            mode: RuntimeMode::InMemoryV10,
            generation_id: GenerationId::parse("basic-v1.0")
                .expect("static V1.0 generation id is valid"),
        }
    }

    /// 中文：覆盖默认代际标识，用于热重载和恢复场景。
    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.generation_id = generation_id;
        self
    }
}

impl RuntimeBuilder {
    /// 中文：创建使用默认 Noop 选项的构建器。
    pub fn new() -> Self {
        Self {
            options: RuntimeOptions::default(),
        }
    }

    /// 中文：创建 V0.4 内存运行时构建器。
    pub fn in_memory_v04() -> Self {
        Self {
            options: RuntimeOptions::in_memory_v04(),
        }
    }

    /// 中文：创建 V0.5 内存运行时构建器。
    pub fn in_memory_v05() -> Self {
        Self {
            options: RuntimeOptions::in_memory_v05(),
        }
    }

    /// 中文：创建 V1.0 核心发布运行时构建器。
    pub fn in_memory_v10() -> Self {
        Self {
            options: RuntimeOptions::in_memory_v10(),
        }
    }

    /// 中文：使用调用方提供的完整选项创建构建器。
    pub fn with_options(options: RuntimeOptions) -> Self {
        Self { options }
    }

    /// 中文：校验运行时最低前置条件，按模式装配服务并生成当前代际摘要。
    ///
    /// Agent 或路由为空时拒绝构建，避免得到无法消费事件的“成功”运行时；服务表和摘要
    /// 只有在所有前置校验通过后才创建，因此错误路径不会暴露半装配实例。
    /// Builds a no-op runtime summary from validated configuration.
    pub fn build(&self, project: &ProjectConfig) -> Result<Runtime, EvaError> {
        if project.agents.is_empty() {
            return Err(
                EvaError::invalid_argument("runtime requires at least one Agent manifest")
                    .with_context("field", "agents"),
            );
        }
        if project.routes.routes.is_empty() {
            return Err(
                EvaError::invalid_argument("runtime requires at least one route")
                    .with_context("field", "routes"),
            );
        }

        let services = match self.options.mode {
            RuntimeMode::Noop => RuntimeServices::noop(project),
            RuntimeMode::InMemoryV04 => RuntimeServices::in_memory_v04(project),
            RuntimeMode::InMemoryV05 => RuntimeServices::in_memory_v05(project),
            RuntimeMode::InMemoryV10 => RuntimeServices::in_memory_v10(project),
        };
        let summary = RuntimeSummary::from_project(project, &self.options, &services);
        Ok(Runtime::new(summary, services))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 中文：返回运行时构建测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 中文：验证 Noop 构建器生成配置摘要且标记基础服务状态。
    fn noop_builder_summarizes_sample_project() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = RuntimeBuilder::new().build(&project).unwrap();
        let summary = runtime.summary();

        assert_eq!(summary.mode, RuntimeMode::Noop);
        assert_eq!(summary.generation_id, "noop-v0.3");
        assert_eq!(summary.agents_total, project.agents.len());
        assert!(summary
            .services
            .iter()
            .any(|service| service.name == "config"));
    }

    #[test]
    /// 中文：验证 V0.5 模式把任务诊断服务标记为就绪。
    fn v05_builder_marks_task_diagnostics_ready() {
        let project = load_project_config(workspace_root().join("examples/basic")).unwrap();
        let runtime = RuntimeBuilder::in_memory_v05().build(&project).unwrap();
        let summary = runtime.summary();

        assert_eq!(summary.mode, RuntimeMode::InMemoryV05);
        assert_eq!(summary.generation_id, "basic-v0.5");
        assert!(summary
            .services
            .iter()
            .any(|service| service.name == "task_registry"));
    }

    #[test]
    /// 中文：验证 V1.0 模式暴露稳定核心发布服务。
    fn v10_builder_marks_release_core_ready() {
        let project = load_project_config(workspace_root().join("examples/basic")).unwrap();
        let runtime = RuntimeBuilder::in_memory_v10().build(&project).unwrap();
        let summary = runtime.summary();

        assert_eq!(summary.mode, RuntimeMode::InMemoryV10);
        assert_eq!(summary.generation_id, "basic-v1.0");
        assert!(summary
            .services
            .iter()
            .any(|service| service.name == "release_core"));
    }
}
