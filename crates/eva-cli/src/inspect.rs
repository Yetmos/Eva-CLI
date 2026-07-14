//! 配置和无操作运行时状态的 CLI 检查报告，负责稳定的展示数据投影。
//! CLI inspect reports for configuration and no-op runtime state.

use crate::run::{display_path, json_array, json_string};
use eva_config::ProjectConfig;
use eva_core::EvaError;
use eva_runtime::{RuntimeBuilder, RuntimeSummary, ServiceSummary};

/// 本模块的架构职责；明确 inspect 只展示已验证配置和只读运行时摘要。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "inspect validated configuration and V0.3 no-op runtime status";

/// `eva inspect` 的组合报告，是文本和 JSON 输出共用的数据快照。
/// Combined `eva inspect` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectReport {
    /// 规范化后的项目根路径。
    pub project_root: String,
    /// 实际加载的 Eva 主配置路径。
    pub eva_config_path: String,
    /// 配置声明的运行环境名称。
    pub environment: String,
    /// 是否启用热重载。
    pub hot_reload: bool,
    /// 已验证的 Agent 配置投影。
    pub agents: Vec<AgentInspect>,
    /// 已验证的 Adapter 配置投影。
    pub adapters: Vec<AdapterInspect>,
    /// 已验证的 Capability 配置投影。
    pub capabilities: Vec<CapabilityInspect>,
    /// 已验证的路由规则投影。
    pub routes: Vec<RouteInspect>,
    /// 已加载策略及其 domain 摘要。
    pub policies: Vec<PolicyInspect>,
    /// 只读运行时构建摘要。
    pub runtime: RuntimeInspect,
}

/// 单个 Agent 的可观察配置摘要，不包含脚本内容或运行时私有状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInspect {
    /// 稳定 Agent ID。
    pub id: String,
    /// 配置中的启用状态。
    pub enabled: bool,
    /// 规范化后的脚本路径。
    pub script: String,
    /// 订阅的 topic pattern 文本。
    pub subscriptions: Vec<String>,
    /// 可选父 Agent ID。
    pub parent: Option<String>,
    /// 子 Agent ID 列表。
    pub children: Vec<String>,
}

/// 单个 Adapter 的可观察配置摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInspect {
    /// 稳定 Adapter ID。
    pub id: String,
    /// 面向用户的 Adapter 名称。
    pub name: String,
    /// 配置中的启用状态。
    pub enabled: bool,
    /// 外部或内置传输类型。
    pub transport: String,
    /// Adapter 声明提供的 capability 名称。
    pub capabilities: Vec<String>,
}

/// 单个 Capability 声明及 provider 候选的摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityInspect {
    /// Capability 清单项的稳定 ID。
    pub id: String,
    /// 面向用户的名称。
    pub name: String,
    /// 配置中的启用状态。
    pub enabled: bool,
    /// Capability 实现类别。
    pub kind: String,
    /// 用于调用和路由的规范 capability 名称。
    pub capability: String,
    /// 可执行该 capability 的 Adapter ID 列表。
    pub providers: Vec<String>,
}

/// 一条 topic 路由规则的可观察摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteInspect {
    /// 已校验的 topic pattern。
    pub pattern: String,
    /// 路由投递模式。
    pub delivery: String,
    /// 目标 Agent ID 列表。
    pub agents: Vec<String>,
}

/// 单个策略文件及其顶层 domain 摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInspect {
    /// 规范化后的策略文件路径。
    pub path: String,
    /// 策略声明的 domain 名称。
    pub domains: Vec<String>,
}

/// 只读运行时状态及服务边界摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInspect {
    /// 运行时实现模式。
    pub mode: String,
    /// 聚合运行状态。
    pub status: String,
    /// 当前运行时代际 ID。
    pub generation_id: String,
    /// 各服务边界的状态列表。
    pub services: Vec<ServiceInspect>,
}

/// 一个运行时服务边界的状态摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInspect {
    /// 稳定服务名称。
    pub name: String,
    /// 机器可读服务状态。
    pub state: String,
    /// 面向操作者的补充说明。
    pub detail: String,
}

/// 从已验证项目配置构建 inspect 报告。
///
/// 这里先构建只读 runtime summary，再投影配置对象；任何 runtime 边界失败都会原样返回
/// `EvaError`，避免输出部分成功、部分缺失的快照。
/// Builds the inspect report from an already validated project config.
pub fn inspect_project(project: &ProjectConfig) -> Result<InspectReport, EvaError> {
    let runtime = RuntimeBuilder::new().build(project)?;
    Ok(InspectReport {
        project_root: display_path(&project.project_root),
        eva_config_path: display_path(&project.eva_config_path),
        environment: project.eva.runtime.env.clone(),
        hot_reload: project.eva.runtime.hot_reload,
        agents: project
            .agents
            .iter()
            .map(|agent| AgentInspect {
                id: agent.id.as_str().to_owned(),
                enabled: agent.enabled,
                script: display_path(&agent.script),
                subscriptions: agent
                    .subscriptions
                    .iter()
                    .map(|topic| topic.as_str().to_owned())
                    .collect(),
                parent: agent.parent.as_ref().map(|value| value.as_str().to_owned()),
                children: agent
                    .children
                    .iter()
                    .map(|value| value.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        adapters: project
            .adapters
            .iter()
            .map(|adapter| AdapterInspect {
                id: adapter.id.as_str().to_owned(),
                name: adapter.name.clone(),
                enabled: adapter.enabled,
                transport: adapter.transport.as_str().to_owned(),
                capabilities: adapter
                    .capabilities
                    .iter()
                    .map(|capability| capability.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        capabilities: project
            .capabilities
            .iter()
            .map(|capability| CapabilityInspect {
                id: capability.id.as_str().to_owned(),
                name: capability.name.clone(),
                enabled: capability.enabled,
                kind: capability.kind.as_str().to_owned(),
                capability: capability.capability.as_str().to_owned(),
                providers: capability
                    .adapter_providers()
                    .map(|provider| provider.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        routes: project
            .routes
            .routes
            .iter()
            .map(|route| RouteInspect {
                pattern: route.pattern.as_str().to_owned(),
                delivery: route.delivery.as_str().to_owned(),
                agents: route
                    .agents
                    .iter()
                    .map(|agent| agent.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        policies: project
            .policies
            .iter()
            .map(|policy| PolicyInspect {
                path: display_path(&policy.path),
                domains: policy.domains.keys().cloned().collect(),
            })
            .collect(),
        runtime: RuntimeInspect::from_summary(runtime.summary()),
    })
}

impl RuntimeInspect {
    /// 将内部运行时摘要投影为 CLI 稳定字段，隔离实现类型与用户输出契约。
    fn from_summary(summary: &RuntimeSummary) -> Self {
        Self {
            mode: summary.mode.as_str().to_owned(),
            status: summary.status.as_str().to_owned(),
            generation_id: summary.generation_id.clone(),
            services: summary.services.iter().map(ServiceInspect::from).collect(),
        }
    }
}

impl From<&ServiceSummary> for ServiceInspect {
    /// 将服务内部状态转换为可序列化的 CLI 摘要。
    fn from(summary: &ServiceSummary) -> Self {
        Self {
            name: summary.name.clone(),
            state: summary.state.as_str().to_owned(),
            detail: summary.detail.clone(),
        }
    }
}

impl InspectReport {
    /// 将完整报告编码为稳定 JSON；所有字符串经公共转义器处理。
    pub fn to_json(&self) -> String {
        format!(
            "{{\"project_root\":{},\"eva_config_path\":{},\"environment\":{},\"hot_reload\":{},\"agents\":{},\"adapters\":{},\"capabilities\":{},\"routes\":{},\"policies\":{},\"runtime\":{}}}",
            json_string(&self.project_root),
            json_string(&self.eva_config_path),
            json_string(&self.environment),
            self.hot_reload,
            json_array(self.agents.iter().map(AgentInspect::to_json)),
            json_array(self.adapters.iter().map(AdapterInspect::to_json)),
            json_array(self.capabilities.iter().map(CapabilityInspect::to_json)),
            json_array(self.routes.iter().map(RouteInspect::to_json)),
            json_array(self.policies.iter().map(PolicyInspect::to_json)),
            self.runtime.to_json(),
        )
    }
}

impl AgentInspect {
    /// 将 Agent 摘要编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"enabled\":{},\"script\":{},\"subscriptions\":{},\"parent\":{},\"children\":{}}}",
            json_string(&self.id),
            self.enabled,
            json_string(&self.script),
            json_array(self.subscriptions.iter().map(|value| json_string(value))),
            self.parent
                .as_ref()
                .map(|value| json_string(value))
                .unwrap_or_else(|| "null".to_owned()),
            json_array(self.children.iter().map(|value| json_string(value))),
        )
    }
}

impl AdapterInspect {
    /// 将 Adapter 摘要编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"name\":{},\"enabled\":{},\"transport\":{},\"capabilities\":{}}}",
            json_string(&self.id),
            json_string(&self.name),
            self.enabled,
            json_string(&self.transport),
            json_array(self.capabilities.iter().map(|value| json_string(value))),
        )
    }
}

impl CapabilityInspect {
    /// 将 Capability 摘要编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"name\":{},\"enabled\":{},\"kind\":{},\"capability\":{},\"providers\":{}}}",
            json_string(&self.id),
            json_string(&self.name),
            self.enabled,
            json_string(&self.kind),
            json_string(&self.capability),
            json_array(self.providers.iter().map(|value| json_string(value))),
        )
    }
}

impl RouteInspect {
    /// 将路由摘要编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"pattern\":{},\"delivery\":{},\"agents\":{}}}",
            json_string(&self.pattern),
            json_string(&self.delivery),
            json_array(self.agents.iter().map(|value| json_string(value))),
        )
    }
}

impl PolicyInspect {
    /// 将策略摘要编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"path\":{},\"domains\":{}}}",
            json_string(&self.path),
            json_array(self.domains.iter().map(|value| json_string(value))),
        )
    }
}

impl RuntimeInspect {
    /// 将运行时摘要编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"mode\":{},\"status\":{},\"generation_id\":{},\"services\":{}}}",
            json_string(&self.mode),
            json_string(&self.status),
            json_string(&self.generation_id),
            json_array(self.services.iter().map(ServiceInspect::to_json)),
        )
    }
}

impl ServiceInspect {
    /// 将单个服务状态编码为 JSON 对象。
    fn to_json(&self) -> String {
        format!(
            "{{\"name\":{},\"state\":{},\"detail\":{}}}",
            json_string(&self.name),
            json_string(&self.state),
            json_string(&self.detail),
        )
    }
}
