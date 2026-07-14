//! 中文：策略域解析、有效层构建和高风险运行时门禁决策。
//! Policy domain parsing and runtime gate decisions.

use crate::{EffectivePolicy, PermissionSet, PolicyLayer, SandboxPolicy};
use eva_config::{policy::PolicyDocument, ProjectConfig};
use eva_core::{AdapterId, AgentId, CapabilityName, EvaError, Topic, TopicPattern};
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};

/// 中文：从 `config/policies/*.yaml` 加载并规范化后的全部类型化策略域。
/// Typed policy domains loaded from `config/policies/*.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyDomainSet {
    /// 中文：参与解析的策略文档数量，供诊断确认配置来源。
    pub source_count: usize,
    /// 中文：Adapter、Skill 和重试相关策略。
    pub adapter: AdapterPolicyDomain,
    /// 中文：记忆写入前的脱敏策略。
    pub memory: MemoryPolicyDomain,
    /// 中文：硬件发现、绑定和热插拔策略。
    pub hardware: HardwarePolicyDomain,
    /// 中文：MCP 服务及逐工具访问策略。
    pub mcp_server: McpServerPolicyDomain,
    /// 中文：必须显式放行的高风险运行时动作集合。
    pub runtime: RuntimePolicyDomain,
    /// 中文：Lua 执行环境的资源和副作用约束。
    pub lua_sandbox: SandboxPolicy,
    /// 中文：由各策略域派生、用于计算最终权限交集的有序层。
    layers: Vec<PolicyLayer>,
}

/// 中文：Adapter 默认权限、显式例外、Skill 和重试策略的聚合域。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterPolicyDomain {
    /// 中文：适用于所有 Adapter 的权限上界。
    pub defaults: PermissionSet,
    /// 中文：被明确允许写入工作区的 Adapter 标识。
    pub allow_write_workspace: BTreeSet<AdapterId>,
    /// 中文：无论 Provider 如何选择都禁止调用的 capability。
    pub deny_capabilities: BTreeSet<CapabilityName>,
    /// 中文：Skill 发现和执行门禁配置。
    pub skill: SkillPolicy,
    /// 中文：Adapter 调用失败后的重试配置。
    pub retry: RetryPolicyDomain,
}

/// 中文：Skill 清单、来源和运行时门禁策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPolicy {
    /// 中文：是否要求每个 Skill 声明输入输出结构。
    pub require_schema: bool,
    /// 中文：是否接受用户主目录下的本地 Skill。
    pub allow_user_local_skills: bool,
    /// 中文：Skill 可以选择的稳定运行时门禁名称。
    pub allowed_runtime_gates: BTreeSet<String>,
    /// 中文：按 Skill 类型名称阻止的来源类别。
    pub deny_kinds: BTreeSet<String>,
}

/// 中文：Adapter 调用的默认尝试上限和按 capability 覆盖项。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetryPolicyDomain {
    /// 中文：未配置 capability 覆盖时使用的最大尝试次数。
    pub default_max_attempts: u32,
    /// 中文：按 capability 名称索引的重试覆盖策略。
    pub capabilities: BTreeMap<CapabilityName, CapabilityRetryPolicy>,
}

/// 中文：单个 capability 的最大尝试次数和可选固定退避时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRetryPolicy {
    /// 中文：包括首次调用在内的最大尝试次数。
    pub max_attempts: u32,
    /// 中文：两次尝试之间的可选等待毫秒数。
    pub backoff_ms: Option<u64>,
}

/// 中文：硬件功能总开关、资源声明和访问白名单策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwarePolicyDomain {
    /// 中文：是否启用硬件运行时路径。
    pub enabled: bool,
    /// 中文：是否允许绕过驱动抽象执行原始 I/O。
    pub allow_raw_io: bool,
    /// 中文：设备声明模式，例如独占或共享。
    pub claim: String,
    /// 中文：是否允许硬件通过网络桥接。
    pub network_bridge: bool,
    /// 中文：硬件操作的可选最大超时毫秒数。
    pub max_timeout_ms: Option<u64>,
    /// 中文：可绑定的硬件总线名称白名单；空集合表示不按总线限制。
    pub allowed_buses: BTreeSet<String>,
    /// 中文：即使硬件功能开启也禁止的 capability。
    pub denied_capabilities: BTreeSet<CapabilityName>,
    /// 中文：热插拔设备身份检查和事件策略。
    pub hotplug: HardwareHotplugPolicy,
}

/// 中文：硬件热插拔身份验证、隔离和事件发出策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareHotplugPolicy {
    /// 中文：是否要求重新连接设备与已登记身份完全一致。
    pub require_identity_match: bool,
    /// 中文：是否把未知设备置于隔离状态而非自动绑定。
    pub quarantine_unknown_devices: bool,
    /// 中文：热插拔状态变化时允许发出的事件主题。
    pub emit_events: Vec<Topic>,
}

/// 中文：MCP 服务总开关、监听地址和逐工具策略。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpServerPolicyDomain {
    /// 中文：是否允许启动 MCP 服务边界。
    pub enabled: bool,
    /// 中文：可选监听地址，由配置层负责格式校验。
    pub bind: Option<String>,
    /// 中文：按稳定工具名称索引的访问策略。
    pub tools: BTreeMap<String, McpToolPolicy>,
}

/// 中文：单个 MCP 工具的主体、资源和超时白名单。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpToolPolicy {
    /// 中文：工具是否可被运行时调用。
    pub enabled: bool,
    /// 中文：允许调用该工具的 Agent；空集合表示不按 Agent 限制。
    pub allowed_agents: BTreeSet<AgentId>,
    /// 中文：允许经该工具调用的 capability。
    pub allowed_capabilities: BTreeSet<CapabilityName>,
    /// 中文：允许经该工具选择的 Provider/Adapter。
    pub allowed_providers: BTreeSet<AdapterId>,
    /// 中文：工具可发出或操作的主题模式。
    pub allowed_topics: Vec<TopicPattern>,
    /// 中文：该工具调用的可选最大超时毫秒数。
    pub timeout_ms: Option<u64>,
}

/// 中文：需要配置显式授权才可执行的高风险运行时动作域。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimePolicyDomain {
    /// 中文：明确允许的高风险动作；未列入集合即默认拒绝。
    pub allow_high_risk_actions: BTreeSet<HighRiskAction>,
}

/// 中文：记忆子系统当前支持的策略集合。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryPolicyDomain {
    /// 中文：持久化和审计前应用的敏感信息脱敏规则。
    pub redaction: RedactionPolicyDomain,
}

/// 中文：敏感键名、令牌前缀和替换文本组成的脱敏策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPolicyDomain {
    /// 中文：是否启用自动脱敏。
    pub enabled: bool,
    /// 中文：发生脱敏时是否记录不含原始秘密的审计事件。
    pub audit_redactions: bool,
    /// 中文：替换敏感值时使用的固定文本。
    pub replacement: String,
    /// 中文：键名中触发脱敏的大小写归一化片段。
    pub sensitive_key_fragments: BTreeSet<String>,
    /// 中文：值以这些前缀开头时视为敏感令牌。
    pub sensitive_token_prefixes: BTreeSet<String>,
}

/// 中文：策略门禁能够识别并审计的稳定高风险动作。
/// Stable high-risk runtime actions understood by the policy gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HighRiskAction {
    /// 中文：调用外部 Adapter capability。
    AdapterInvoke,
    /// 中文：建立携带 Provider 凭据的受限会话。
    ProviderCredentialSession,
    /// 中文：通过 MCP 调用注册工具。
    McpToolCall,
    /// 中文：通过 MCP 向事件总线发出主题事件。
    McpTopicEmit,
    /// 中文：运行发现到的 Skill。
    SkillRun,
    /// 中文：把物理设备绑定到运行时驱动。
    HardwareBind,
    /// 中文：执行绕过普通驱动约束的原始硬件 I/O。
    HardwareRawIo,
    /// 中文：创建可能包含运行状态的备份。
    BackupCreate,
    /// 中文：把恢复计划实际应用到项目。
    RestoreApply,
    /// 中文：应用版本升级及其文件变更。
    UpgradeApply,
    /// 中文：修改发布指针或当前版本标记。
    ReleasePointerMutation,
    /// 中文：在 Supervisor 代际之间移交控制权。
    SupervisorHandoff,
}

/// 中文：一次高风险策略判定所需的动作和可选作用域上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyRequest {
    /// 中文：必须判定的高风险动作。
    pub action: HighRiskAction,
    /// 中文：MCP 工具或 Skill 运行时门禁名称。
    pub tool: Option<String>,
    /// 中文：发起动作的可选 Agent 身份。
    pub agent: Option<AgentId>,
    /// 中文：动作涉及的可选 capability。
    pub capability: Option<CapabilityName>,
    /// 中文：实际执行动作的可选 Provider。
    pub provider: Option<AdapterId>,
    /// 中文：承载会话或调用的可选 Adapter。
    pub adapter: Option<AdapterId>,
    /// 中文：事件发出动作涉及的可选主题。
    pub topic: Option<Topic>,
    /// 中文：硬件动作涉及的可选总线名称。
    pub bus: Option<String>,
    /// 中文：调用方请求的可选超时毫秒数。
    pub timeout_ms: Option<u64>,
}

/// 中文：一次策略判定的允许标志、原因和稳定审计证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    /// 中文：本次判定对应的动作。
    pub action: HighRiskAction,
    /// 中文：动作是否获准继续执行。
    pub allowed: bool,
    /// 中文：面向操作员的具体判定原因。
    pub reason: String,
    /// 中文：可直接写入审计后端的稳定键值记录。
    pub audit: Vec<String>,
}

/// 中文：持有规范化策略域并对运行时请求执行默认拒绝判定的门禁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyGate {
    /// 中文：构造时加载的不可变策略域快照。
    domains: PolicyDomainSet,
}

impl Default for SkillPolicy {
    /// 中文：默认要求 Schema、拒绝用户本地 Skill，仅允许普通运行门禁。
    fn default() -> Self {
        Self {
            require_schema: true,
            allow_user_local_skills: false,
            allowed_runtime_gates: ["normal"].into_iter().map(str::to_owned).collect(),
            deny_kinds: BTreeSet::new(),
        }
    }
}

impl Default for HardwarePolicyDomain {
    /// 中文：默认完全关闭硬件和原始 I/O，并采用独占设备声明。
    fn default() -> Self {
        Self {
            enabled: false,
            allow_raw_io: false,
            claim: "exclusive".to_owned(),
            network_bridge: false,
            max_timeout_ms: None,
            allowed_buses: BTreeSet::new(),
            denied_capabilities: BTreeSet::new(),
            hotplug: HardwareHotplugPolicy::default(),
        }
    }
}

impl Default for HardwareHotplugPolicy {
    /// 中文：默认要求身份匹配并隔离未知设备，不自动发出事件。
    fn default() -> Self {
        Self {
            require_identity_match: true,
            quarantine_unknown_devices: true,
            emit_events: Vec::new(),
        }
    }
}

impl Default for RedactionPolicyDomain {
    /// 中文：默认启用脱敏和审计，并覆盖常见凭据键名及 `sk-` 令牌。
    fn default() -> Self {
        Self {
            enabled: true,
            audit_redactions: true,
            replacement: "[REDACTED]".to_owned(),
            sensitive_key_fragments: [
                "password",
                "secret",
                "token",
                "api_key",
                "apikey",
                "authorization",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            sensitive_token_prefixes: ["sk-"].into_iter().map(str::to_owned).collect(),
        }
    }
}

impl PolicyDomainSet {
    /// 中文：从已加载项目配置中的策略文档构建类型化策略域。
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        Self::from_documents(&project.policies)
    }

    /// 中文：解析策略文档并派生最终权限计算所需的有序策略层。
    ///
    /// 同名域按文档遍历顺序采用后值，未知域由配置兼容层忽略；只有会影响权限或沙箱
    /// 上界的域才形成 `PolicyLayer`。没有任何可合并层时补入默认拒绝层，确保后续交集
    /// 计算永远不会因空集合而得到宽松策略。
    pub fn from_documents(documents: &[PolicyDocument]) -> Result<Self, EvaError> {
        let mut domains = Self {
            source_count: documents.len(),
            ..Self::default()
        };
        let mut layers = Vec::new();

        for document in documents {
            for (name, value) in &document.domains {
                match name.as_str() {
                    "adapter_policy" => {
                        domains.adapter = parse_adapter_policy(value)?;
                        layers.push(PolicyLayer::new(
                            layer_name(document, "adapter_policy"),
                            domains.adapter.defaults.clone(),
                            SandboxPolicy::default(),
                        ));
                    }
                    "memory_policy" => {
                        domains.memory = parse_memory_policy(value)?;
                    }
                    "hardware_policy" => {
                        domains.hardware = parse_hardware_policy(value)?;
                        layers.push(PolicyLayer::new(
                            layer_name(document, "hardware_policy"),
                            hardware_permissions(&domains.hardware),
                            SandboxPolicy::default(),
                        ));
                    }
                    "mcp_server" => {
                        domains.mcp_server = parse_mcp_server_policy(value)?;
                    }
                    "runtime_policy" => {
                        domains.runtime = parse_runtime_policy(value)?;
                    }
                    "lua_sandbox" => {
                        domains.lua_sandbox = parse_lua_sandbox(value)?;
                        layers.push(PolicyLayer::new(
                            layer_name(document, "lua_sandbox"),
                            PermissionSet::deny_all(),
                            domains.lua_sandbox.clone(),
                        ));
                    }
                    _ => {}
                }
            }
        }

        if layers.is_empty() {
            layers.push(PolicyLayer::new(
                "policy.default",
                PermissionSet::deny_all(),
                SandboxPolicy::default(),
            ));
        }
        domains.layers = layers;
        Ok(domains)
    }

    /// 中文：返回用于有效策略计算的有序层。
    pub fn layers(&self) -> &[PolicyLayer] {
        &self.layers
    }

    /// 中文：对已派生层求交集，得到不可扩张的最终权限和沙箱策略。
    pub fn effective_policy(&self) -> Result<EffectivePolicy, EvaError> {
        EffectivePolicy::from_layers(self.layers.clone())
    }
}

impl HighRiskAction {
    /// 中文：返回配置、协议和审计日志使用的稳定动作名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdapterInvoke => "adapter.invoke",
            Self::ProviderCredentialSession => "provider.credential_session",
            Self::McpToolCall => "mcp.tool.call",
            Self::McpTopicEmit => "mcp.topic.emit",
            Self::SkillRun => "skill.run",
            Self::HardwareBind => "hardware.bind",
            Self::HardwareRawIo => "hardware.raw_io",
            Self::BackupCreate => "backup.create",
            Self::RestoreApply => "restore.apply",
            Self::UpgradeApply => "upgrade.apply",
            Self::ReleasePointerMutation => "release.pointer_mutation",
            Self::SupervisorHandoff => "supervisor.handoff",
        }
    }

    /// 中文：从稳定动作名称解析枚举，未知名称作为配置错误拒绝。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "adapter.invoke" => Ok(Self::AdapterInvoke),
            "provider.credential_session" => Ok(Self::ProviderCredentialSession),
            "mcp.tool.call" => Ok(Self::McpToolCall),
            "mcp.topic.emit" => Ok(Self::McpTopicEmit),
            "skill.run" => Ok(Self::SkillRun),
            "hardware.bind" => Ok(Self::HardwareBind),
            "hardware.raw_io" => Ok(Self::HardwareRawIo),
            "backup.create" => Ok(Self::BackupCreate),
            "restore.apply" => Ok(Self::RestoreApply),
            "upgrade.apply" => Ok(Self::UpgradeApply),
            "release.pointer_mutation" => Ok(Self::ReleasePointerMutation),
            "supervisor.handoff" => Ok(Self::SupervisorHandoff),
            _ => Err(
                EvaError::invalid_argument("unknown high-risk policy action")
                    .with_context("action", value),
            ),
        }
    }
}

impl RuntimePolicyRequest {
    /// 中文：创建只包含动作的请求，其余作用域由对应判定器按需要求。
    pub fn new(action: HighRiskAction) -> Self {
        Self {
            action,
            tool: None,
            agent: None,
            capability: None,
            provider: None,
            adapter: None,
            topic: None,
            bus: None,
            timeout_ms: None,
        }
    }

    /// 中文：附加 MCP 工具或 Skill 运行门禁名称。
    pub fn with_tool(mut self, value: impl Into<String>) -> Self {
        self.tool = Some(value.into());
        self
    }

    /// 中文：附加发起动作的 Agent 身份。
    pub fn with_agent(mut self, value: AgentId) -> Self {
        self.agent = Some(value);
        self
    }

    /// 中文：附加动作涉及的 capability。
    pub fn with_capability(mut self, value: CapabilityName) -> Self {
        self.capability = Some(value);
        self
    }

    /// 中文：附加实际执行动作的 Provider。
    pub fn with_provider(mut self, value: AdapterId) -> Self {
        self.provider = Some(value);
        self
    }

    /// 中文：附加承载调用或凭据会话的 Adapter。
    pub fn with_adapter(mut self, value: AdapterId) -> Self {
        self.adapter = Some(value);
        self
    }

    /// 中文：附加 MCP 发出动作的目标主题。
    pub fn with_topic(mut self, value: Topic) -> Self {
        self.topic = Some(value);
        self
    }

    /// 中文：附加硬件操作使用的总线名称。
    pub fn with_bus(mut self, value: impl Into<String>) -> Self {
        self.bus = Some(value.into());
        self
    }

    /// 中文：附加调用方请求的超时毫秒数。
    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = Some(value);
        self
    }
}

impl PolicyDecision {
    /// 中文：把判定转换为控制流结果；拒绝时携带动作、原因和审计轨迹。
    pub fn ensure_allowed(&self) -> Result<(), EvaError> {
        if self.allowed {
            Ok(())
        } else {
            Err(
                EvaError::permission_denied("runtime policy denied high-risk action")
                    .with_context("action", self.action.as_str())
                    .with_context("reason", &self.reason)
                    .with_context("audit", self.audit.join(";")),
            )
        }
    }
}

impl RuntimePolicyGate {
    /// 中文：从项目策略文档创建运行时门禁，解析失败时不产生部分门禁。
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        Ok(Self::new(PolicyDomainSet::from_project(project)?))
    }

    /// 中文：使用已经规范化的策略域快照创建门禁。
    pub fn new(domains: PolicyDomainSet) -> Self {
        Self { domains }
    }

    /// 中文：返回门禁使用的策略域，只允许调用方读取和诊断。
    pub fn domains(&self) -> &PolicyDomainSet {
        &self.domains
    }

    /// 中文：查询指定 capability 的 Adapter 重试退避时间；没有覆盖时返回 `None`。
    pub fn adapter_retry_backoff_ms(&self, capability: &CapabilityName) -> Option<u64> {
        self.domains
            .adapter
            .retry
            .capabilities
            .get(capability)
            .and_then(|policy| policy.backoff_ms)
    }

    /// 中文：按动作类型分派到专用判定器，并返回带审计证据的确定性结果。
    ///
    /// 每个专用判定器都采用默认拒绝：动作需要的作用域缺失、目标未声明、功能关闭或
    /// 请求突破资源上限时均不得继续。只有完整通过对应检查的请求才构造允许结果。
    pub fn decide(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        match request.action {
            HighRiskAction::McpToolCall => self.decide_mcp_tool_call(request),
            HighRiskAction::McpTopicEmit => self.decide_mcp_topic_emit(request),
            HighRiskAction::HardwareBind => self.decide_hardware_bind(request),
            HighRiskAction::HardwareRawIo => self.decide_hardware_raw_io(request),
            HighRiskAction::AdapterInvoke => self.decide_adapter_invoke(request),
            HighRiskAction::ProviderCredentialSession => {
                self.decide_provider_credential_session(request)
            }
            HighRiskAction::SkillRun => self.decide_skill_run(request),
            HighRiskAction::BackupCreate
            | HighRiskAction::RestoreApply
            | HighRiskAction::UpgradeApply
            | HighRiskAction::ReleasePointerMutation
            | HighRiskAction::SupervisorHandoff => self.decide_explicit_runtime_action(request),
        }
    }

    /// 中文：判定 Adapter capability 是否未被全局禁止且超时位于默认上限内。
    fn decide_adapter_invoke(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        if let Some(capability) = &request.capability {
            if self.domains.adapter.deny_capabilities.contains(capability) {
                return denied(
                    action,
                    format!("capability {capability} is globally denied"),
                );
            }
        }
        if let Some(timeout) = request.timeout_ms {
            if let Some(limit) = self.domains.adapter.defaults.max_timeout_ms {
                if timeout > limit {
                    return denied(action, "adapter request timeout exceeds policy limit");
                }
            }
        }
        allowed(action, "adapter request is within policy domain")
    }

    /// 中文：要求凭据会话同时声明 Adapter 和 Provider，且二者必须是同一身份。
    ///
    /// 该相等约束阻止已认证会话跨 Provider 复用，从边界上避免凭据混淆和权限串用。
    fn decide_provider_credential_session(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        let Some(adapter) = &request.adapter else {
            return denied(action, "provider credential session requires adapter scope");
        };
        let Some(provider) = &request.provider else {
            return denied(
                action,
                "provider credential session requires provider scope",
            );
        };
        if adapter != provider {
            return denied(
                action,
                "provider credential session cannot be reused across providers",
            );
        }
        allowed(
            action,
            "provider credential session is scoped to requested provider",
        )
    }

    /// 中文：判定 Skill 请求的运行时门禁是否被显式列入允许集合。
    fn decide_skill_run(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        let Some(gate) = request.tool.as_deref() else {
            return denied(action, "skill runtime gate is required");
        };
        if !self
            .domains
            .adapter
            .skill
            .allowed_runtime_gates
            .contains(gate)
        {
            return denied(action, format!("skill runtime gate {gate} is not allowed"));
        }
        allowed(action, "skill runtime gate is allowed")
    }

    /// 中文：逐项验证 MCP 服务、工具开关、Agent、capability、Provider 和超时约束。
    ///
    /// 各身份白名单为空时表示该维度不限制；非空时请求携带的对应身份必须在集合内。
    fn decide_mcp_tool_call(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        let Some(tool_name) = request.tool.as_deref() else {
            return denied(action, "MCP tool name is required");
        };
        let Some(tool) = self.domains.mcp_server.tools.get(tool_name) else {
            return denied(action, format!("MCP tool {tool_name} is not declared"));
        };
        if !self.domains.mcp_server.enabled || !tool.enabled {
            return denied(action, format!("MCP tool {tool_name} is disabled"));
        }
        if let Some(agent) = &request.agent {
            if !tool.allowed_agents.is_empty() && !tool.allowed_agents.contains(agent) {
                return denied(
                    action,
                    format!("agent {agent} is not allowed for {tool_name}"),
                );
            }
        }
        if let Some(capability) = &request.capability {
            if !tool.allowed_capabilities.is_empty()
                && !tool.allowed_capabilities.contains(capability)
            {
                return denied(
                    action,
                    format!("capability {capability} is not allowed for {tool_name}"),
                );
            }
        }
        if let Some(provider) = &request.provider {
            if !tool.allowed_providers.is_empty() && !tool.allowed_providers.contains(provider) {
                return denied(
                    action,
                    format!("provider {provider} is not allowed for {tool_name}"),
                );
            }
        }
        if let Some(timeout) = request.timeout_ms {
            if let Some(limit) = tool.timeout_ms {
                if timeout > limit {
                    return denied(
                        action,
                        format!("MCP tool {tool_name} timeout exceeds policy"),
                    );
                }
            }
        }
        allowed(action, format!("MCP tool {tool_name} is allowed"))
    }

    /// 中文：只允许启用的 `topic.emit` 工具向白名单模式匹配的主题发出事件。
    fn decide_mcp_topic_emit(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        let Some(tool) = self.domains.mcp_server.tools.get("topic.emit") else {
            return denied(action, "topic.emit policy is not declared");
        };
        if !self.domains.mcp_server.enabled || !tool.enabled {
            return denied(action, "topic.emit is disabled");
        }
        let Some(topic) = &request.topic else {
            return denied(action, "topic.emit requires a topic");
        };
        if !tool
            .allowed_topics
            .iter()
            .any(|pattern| pattern.matches(topic))
        {
            return denied(action, format!("topic {topic} is not allowlisted"));
        }
        allowed(action, format!("topic {topic} is allowed"))
    }

    /// 中文：判定硬件功能、总线、capability 和超时是否满足绑定策略。
    fn decide_hardware_bind(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        if !self.domains.hardware.enabled {
            return denied(action, "hardware policy is disabled");
        }
        if let Some(bus) = &request.bus {
            if !self.domains.hardware.allowed_buses.is_empty()
                && !self.domains.hardware.allowed_buses.contains(bus)
            {
                return denied(action, format!("hardware bus {bus} is not allowed"));
            }
        }
        if let Some(capability) = &request.capability {
            if self
                .domains
                .hardware
                .denied_capabilities
                .contains(capability)
            {
                return denied(
                    action,
                    format!("hardware capability {capability} is denied"),
                );
            }
        }
        if let Some(timeout) = request.timeout_ms {
            if let Some(limit) = self.domains.hardware.max_timeout_ms {
                if timeout > limit {
                    return denied(action, "hardware timeout exceeds policy limit");
                }
            }
        }
        allowed(action, "hardware bind is within policy domain")
    }

    /// 中文：原始硬件 I/O 必须同时满足硬件总开关和专用显式授权。
    fn decide_hardware_raw_io(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        if !self.domains.hardware.enabled {
            return denied(action, "hardware policy is disabled");
        }
        if !self.domains.hardware.allow_raw_io {
            return denied(action, "hardware raw I/O is disabled by policy");
        }
        self.decide_hardware_bind(request)
    }

    /// 中文：对备份、恢复、升级、发布指针和移交动作执行显式白名单判定。
    fn decide_explicit_runtime_action(&self, request: RuntimePolicyRequest) -> PolicyDecision {
        let action = request.action;
        if self
            .domains
            .runtime
            .allow_high_risk_actions
            .contains(&action)
        {
            allowed(action, "runtime policy explicitly allows high-risk action")
        } else {
            denied(
                action,
                "high-risk action requires explicit runtime_policy allow_high_risk_actions entry",
            )
        }
    }
}

/// 中文：把 YAML Adapter 策略映射解析为默认权限、例外、Skill 和重试域。
fn parse_adapter_policy(value: &Value) -> Result<AdapterPolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "adapter_policy")?;
    let mut policy = AdapterPolicyDomain::default();
    if let Some(defaults) = get(mapping, "defaults") {
        policy.defaults = parse_permission_defaults(defaults, "adapter_policy.defaults")?;
    }
    policy.allow_write_workspace = parse_adapter_set(
        get(mapping, "allow_write_workspace"),
        "adapter_policy.allow_write_workspace",
    )?;
    policy.deny_capabilities = parse_capability_set(
        get(mapping, "deny_capabilities"),
        "adapter_policy.deny_capabilities",
    )?;
    if let Some(skill) = get(mapping, "skill") {
        policy.skill = parse_skill_policy(skill)?;
    }
    if let Some(retry) = get(mapping, "retry") {
        policy.retry = parse_retry_policy(retry)?;
    }
    Ok(policy)
}

/// 中文：解析 Adapter 默认布尔权限和超时上限，缺失字段保持默认拒绝。
fn parse_permission_defaults(value: &Value, path: &'static str) -> Result<PermissionSet, EvaError> {
    let mapping = expect_mapping(value, path)?;
    let mut permissions = PermissionSet::deny_all();
    permissions.network = bool_field(mapping, "network", permissions.network, path)?;
    permissions.shell = bool_field(mapping, "shell", permissions.shell, path)?;
    permissions.read_workspace =
        bool_field(mapping, "read_workspace", permissions.read_workspace, path)?;
    permissions.write_workspace = bool_field(
        mapping,
        "write_workspace",
        permissions.write_workspace,
        path,
    )?;
    if let Some(value) = optional_u64_field(mapping, "max_timeout_ms", path)? {
        permissions.max_timeout_ms = Some(value);
    }
    Ok(permissions)
}

/// 中文：解析 Skill Schema、来源、运行门禁和类型拒绝集合。
fn parse_skill_policy(value: &Value) -> Result<SkillPolicy, EvaError> {
    let mapping = expect_mapping(value, "adapter_policy.skill")?;
    Ok(SkillPolicy {
        require_schema: bool_field(mapping, "require_schema", true, "adapter_policy.skill")?,
        allow_user_local_skills: bool_field(
            mapping,
            "allow_user_local_skills",
            false,
            "adapter_policy.skill",
        )?,
        allowed_runtime_gates: parse_string_set(
            get(mapping, "allowed_runtime_gates"),
            "adapter_policy.skill.allowed_runtime_gates",
        )?,
        deny_kinds: parse_string_set(
            get(mapping, "deny_kinds"),
            "adapter_policy.skill.deny_kinds",
        )?,
    })
}

/// 中文：解析默认重试上限及按 capability 索引的覆盖项。
fn parse_retry_policy(value: &Value) -> Result<RetryPolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "adapter_policy.retry")?;
    let mut retry = RetryPolicyDomain::default();
    if let Some(default) = get(mapping, "default") {
        let default_mapping = expect_mapping(default, "adapter_policy.retry.default")?;
        retry.default_max_attempts = optional_u64_field(
            default_mapping,
            "max_attempts",
            "adapter_policy.retry.default",
        )?
        .unwrap_or(0)
        .try_into()
        .map_err(|_| EvaError::invalid_argument("retry max_attempts is too large"))?;
    }
    if let Some(capabilities) = get(mapping, "capabilities") {
        let capability_mapping = expect_mapping(capabilities, "adapter_policy.retry.capabilities")?;
        for (key, value) in capability_mapping {
            let capability = capability_key(key, "adapter_policy.retry.capabilities")?;
            let config = expect_mapping(value, "adapter_policy.retry.capabilities.*")?;
            let max_attempts = optional_u64_field(
                config,
                "max_attempts",
                "adapter_policy.retry.capabilities.*",
            )?
            .unwrap_or(retry.default_max_attempts.into())
            .try_into()
            .map_err(|_| EvaError::invalid_argument("retry max_attempts is too large"))?;
            retry.capabilities.insert(
                capability,
                CapabilityRetryPolicy {
                    max_attempts,
                    backoff_ms: optional_u64_field(
                        config,
                        "backoff_ms",
                        "adapter_policy.retry.capabilities.*",
                    )?,
                },
            );
        }
    }
    Ok(retry)
}

/// 中文：解析记忆策略域；当前只包含可选脱敏子域。
fn parse_memory_policy(value: &Value) -> Result<MemoryPolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "memory_policy")?;
    let mut policy = MemoryPolicyDomain::default();
    if let Some(redaction) = get(mapping, "redaction") {
        policy.redaction = parse_redaction_policy(redaction)?;
    }
    Ok(policy)
}

/// 中文：在安全默认值基础上解析脱敏开关、替换文本和敏感匹配集合。
fn parse_redaction_policy(value: &Value) -> Result<RedactionPolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "memory_policy.redaction")?;
    let mut policy = RedactionPolicyDomain::default();
    policy.enabled = bool_field(
        mapping,
        "enabled",
        policy.enabled,
        "memory_policy.redaction",
    )?;
    policy.audit_redactions = bool_field(
        mapping,
        "audit_redactions",
        policy.audit_redactions,
        "memory_policy.redaction",
    )?;
    policy.replacement = string_field(
        mapping,
        "replacement",
        &policy.replacement,
        "memory_policy.redaction",
    )?;
    if policy.replacement.trim().is_empty() {
        return Err(
            EvaError::invalid_argument("redaction replacement cannot be empty")
                .with_context("field", "memory_policy.redaction.replacement"),
        );
    }
    if let Some(keys) = get(mapping, "sensitive_key_fragments") {
        policy.sensitive_key_fragments =
            parse_lowercase_string_set(keys, "memory_policy.redaction.sensitive_key_fragments")?;
    }
    if let Some(prefixes) = get(mapping, "sensitive_token_prefixes") {
        policy.sensitive_token_prefixes = parse_lowercase_string_set(
            prefixes,
            "memory_policy.redaction.sensitive_token_prefixes",
        )?;
    }
    Ok(policy)
}

/// 中文：解析硬件总开关、原始 I/O、声明方式、总线及热插拔策略。
fn parse_hardware_policy(value: &Value) -> Result<HardwarePolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "hardware_policy")?;
    let defaults = get(mapping, "defaults")
        .map(|value| expect_mapping(value, "hardware_policy.defaults"))
        .transpose()?;
    let mut policy = HardwarePolicyDomain::default();
    policy.enabled = bool_field(mapping, "enabled", policy.enabled, "hardware_policy")?;
    if let Some(defaults) = defaults {
        policy.allow_raw_io = bool_field(
            defaults,
            "allow_raw_io",
            policy.allow_raw_io,
            "hardware_policy.defaults",
        )?;
        policy.claim = string_field(defaults, "claim", &policy.claim, "hardware_policy.defaults")?;
        policy.network_bridge = bool_field(
            defaults,
            "network_bridge",
            policy.network_bridge,
            "hardware_policy.defaults",
        )?;
        policy.max_timeout_ms =
            optional_u64_field(defaults, "max_timeout_ms", "hardware_policy.defaults")?;
    }
    policy.allowed_buses = parse_string_set(
        get(mapping, "allowed_buses"),
        "hardware_policy.allowed_buses",
    )?;
    policy.denied_capabilities = parse_capability_set(
        get(mapping, "denied_capabilities"),
        "hardware_policy.denied_capabilities",
    )?;
    if let Some(hotplug) = get(mapping, "hotplug") {
        policy.hotplug = parse_hardware_hotplug_policy(hotplug)?;
    }
    Ok(policy)
}

/// 中文：解析热插拔身份校验、未知设备隔离和事件主题列表。
fn parse_hardware_hotplug_policy(value: &Value) -> Result<HardwareHotplugPolicy, EvaError> {
    let mapping = expect_mapping(value, "hardware_policy.hotplug")?;
    Ok(HardwareHotplugPolicy {
        require_identity_match: bool_field(
            mapping,
            "require_identity_match",
            true,
            "hardware_policy.hotplug",
        )?,
        quarantine_unknown_devices: bool_field(
            mapping,
            "quarantine_unknown_devices",
            true,
            "hardware_policy.hotplug",
        )?,
        emit_events: parse_topic_list(
            get(mapping, "emit_events"),
            "hardware_policy.hotplug.emit_events",
        )?,
    })
}

/// 中文：解析 MCP 服务开关、监听地址及具名工具策略映射。
fn parse_mcp_server_policy(value: &Value) -> Result<McpServerPolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "mcp_server")?;
    let mut policy = McpServerPolicyDomain {
        enabled: bool_field(mapping, "enabled", false, "mcp_server")?,
        bind: get(mapping, "bind")
            .map(|value| required_str(value, "mcp_server.bind").map(str::to_owned))
            .transpose()?,
        tools: BTreeMap::new(),
    };
    if let Some(tools) = get(mapping, "tools") {
        let tools = expect_mapping(tools, "mcp_server.tools")?;
        for (tool_key, tool_value) in tools {
            let tool_name = required_key(tool_key, "mcp_server.tools")?.to_owned();
            policy
                .tools
                .insert(tool_name, parse_mcp_tool_policy(tool_value)?);
        }
    }
    Ok(policy)
}

/// 中文：解析单个 MCP 工具的身份白名单、主题模式和超时限制。
fn parse_mcp_tool_policy(value: &Value) -> Result<McpToolPolicy, EvaError> {
    let mapping = expect_mapping(value, "mcp_server.tools.*")?;
    Ok(McpToolPolicy {
        enabled: bool_field(mapping, "enabled", false, "mcp_server.tools.*")?,
        allowed_agents: parse_agent_set(
            get(mapping, "allowed_agents"),
            "mcp_server.tools.*.allowed_agents",
        )?,
        allowed_capabilities: parse_capability_set(
            get(mapping, "allowed_capabilities"),
            "mcp_server.tools.*.allowed_capabilities",
        )?,
        allowed_providers: parse_adapter_set(
            get(mapping, "allowed_providers"),
            "mcp_server.tools.*.allowed_providers",
        )?,
        allowed_topics: parse_topic_pattern_list(
            get(mapping, "allowed_topics"),
            "mcp_server.tools.*.allowed_topics",
        )?,
        timeout_ms: optional_u64_field(mapping, "timeout_ms", "mcp_server.tools.*")?,
    })
}

/// 中文：解析高风险动作显式白名单，未知动作会使配置加载失败。
fn parse_runtime_policy(value: &Value) -> Result<RuntimePolicyDomain, EvaError> {
    let mapping = expect_mapping(value, "runtime_policy")?;
    let actions = get(mapping, "allow_high_risk_actions")
        .map(|value| {
            sequence(value, "runtime_policy.allow_high_risk_actions")?
                .iter()
                .map(|value| {
                    HighRiskAction::parse(required_str(
                        value,
                        "runtime_policy.allow_high_risk_actions",
                    )?)
                })
                .collect::<Result<BTreeSet<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(RuntimePolicyDomain {
        allow_high_risk_actions: actions,
    })
}

/// 中文：在安全基线之上解析 Lua 禁用库、资源限制、外部副作用和校验开关。
fn parse_lua_sandbox(value: &Value) -> Result<SandboxPolicy, EvaError> {
    let mapping = expect_mapping(value, "lua_sandbox")?;
    let mut sandbox = SandboxPolicy {
        disabled_lua_libs: parse_string_set(
            get(mapping, "disabled_libs"),
            "lua_sandbox.disabled_libs",
        )?,
        ..SandboxPolicy::default()
    };
    if let Some(limits) = get(mapping, "limits") {
        let limits = expect_mapping(limits, "lua_sandbox.limits")?;
        if let Some(memory) = optional_u64_field(limits, "memory_mb", "lua_sandbox.limits")? {
            sandbox.memory_mb =
                Some(memory.try_into().map_err(|_| {
                    EvaError::invalid_argument("lua sandbox memory_mb is too large")
                })?);
        }
        sandbox.execution_timeout_ms =
            optional_u64_field(limits, "execution_timeout_ms", "lua_sandbox.limits")?;
    }
    if let Some(filesystem) = get(mapping, "filesystem") {
        sandbox.filesystem_enabled = bool_field(
            expect_mapping(filesystem, "lua_sandbox.filesystem")?,
            "enabled",
            sandbox.filesystem_enabled,
            "lua_sandbox.filesystem",
        )?;
    }
    if let Some(network) = get(mapping, "network") {
        sandbox.network_enabled = bool_field(
            expect_mapping(network, "lua_sandbox.network")?,
            "enabled",
            sandbox.network_enabled,
            "lua_sandbox.network",
        )?;
    }
    if let Some(environment) = get(mapping, "environment") {
        sandbox.environment_enabled = bool_field(
            expect_mapping(environment, "lua_sandbox.environment")?,
            "enabled",
            sandbox.environment_enabled,
            "lua_sandbox.environment",
        )?;
    }
    sandbox.return_schema_validation = bool_field(
        mapping,
        "return_schema_validation",
        sandbox.return_schema_validation,
        "lua_sandbox",
    )?;
    sandbox.emitted_topic_validation = bool_field(
        mapping,
        "emitted_topic_validation",
        sandbox.emitted_topic_validation,
        "lua_sandbox",
    )?;
    Ok(sandbox)
}

/// 中文：把硬件域中可参与全局交集计算的网络与超时约束投影为权限集合。
fn hardware_permissions(policy: &HardwarePolicyDomain) -> PermissionSet {
    let mut permissions = PermissionSet::deny_all();
    permissions.network = policy.network_bridge;
    permissions.max_timeout_ms = policy.max_timeout_ms;
    permissions
}

/// 中文：构造允许判定，并生成稳定的动作、结论和原因审计记录。
fn allowed(action: HighRiskAction, reason: impl Into<String>) -> PolicyDecision {
    let reason = reason.into();
    PolicyDecision {
        action,
        allowed: true,
        audit: vec![
            format!("policy.action:{}", action.as_str()),
            "policy.decision:allow".to_owned(),
            format!("policy.reason:{reason}"),
        ],
        reason,
    }
}

/// 中文：构造拒绝判定，并生成与允许路径结构一致的审计记录。
fn denied(action: HighRiskAction, reason: impl Into<String>) -> PolicyDecision {
    let reason = reason.into();
    PolicyDecision {
        action,
        allowed: false,
        audit: vec![
            format!("policy.action:{}", action.as_str()),
            "policy.decision:deny".to_owned(),
            format!("policy.reason:{reason}"),
        ],
        reason,
    }
}

/// 中文：要求 YAML 节点为映射，并在类型错误中附加配置路径。
fn expect_mapping<'a>(value: &'a Value, path: &'static str) -> Result<&'a Mapping, EvaError> {
    value.as_mapping().ok_or_else(|| {
        EvaError::invalid_argument("policy domain field must be a mapping")
            .with_context("field", path)
    })
}

/// 中文：按字符串键从 YAML 映射读取可选值。
fn get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_owned()))
}

/// 中文：读取可选布尔字段；缺失时采用调用方给定的安全默认值。
fn bool_field(
    mapping: &Mapping,
    key: &'static str,
    default: bool,
    path: &'static str,
) -> Result<bool, EvaError> {
    get(mapping, key)
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                EvaError::invalid_argument("policy field must be a boolean")
                    .with_context("field", format!("{path}.{key}"))
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

/// 中文：读取可选字符串字段；缺失时复制默认值。
fn string_field(
    mapping: &Mapping,
    key: &'static str,
    default: &str,
    path: &'static str,
) -> Result<String, EvaError> {
    get(mapping, key)
        .map(|value| required_str(value, path).map(str::to_owned))
        .transpose()
        .map(|value| value.unwrap_or_else(|| default.to_owned()))
}

/// 中文：读取可选无符号整数字段，并拒绝负数或其他 YAML 类型。
fn optional_u64_field(
    mapping: &Mapping,
    key: &'static str,
    path: &'static str,
) -> Result<Option<u64>, EvaError> {
    get(mapping, key)
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                EvaError::invalid_argument("policy field must be an unsigned integer")
                    .with_context("field", format!("{path}.{key}"))
            })
        })
        .transpose()
}

/// 中文：把可选字符串列表解析为有序去重集合；缺失时返回空集合。
fn parse_string_set(
    value: Option<&Value>,
    path: &'static str,
) -> Result<BTreeSet<String>, EvaError> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| required_str(value, path).map(str::to_owned))
        .collect()
}

/// 中文：校验非空无边缘空白的字符串列表，并统一转成小写集合。
fn parse_lowercase_string_set(
    value: &Value,
    path: &'static str,
) -> Result<BTreeSet<String>, EvaError> {
    sequence(value, path)?
        .iter()
        .map(|value| {
            let value = required_str(value, path)?;
            if value.trim().is_empty() || value.trim() != value {
                return Err(EvaError::invalid_argument(
                    "policy list values must be non-empty strings",
                )
                .with_context("field", path));
            }
            Ok(value.to_ascii_lowercase())
        })
        .collect()
}

/// 中文：把可选字符串列表解析为经过校验的 Agent 标识集合。
fn parse_agent_set(
    value: Option<&Value>,
    path: &'static str,
) -> Result<BTreeSet<AgentId>, EvaError> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| AgentId::parse(required_str(value, path)?))
        .collect()
}

/// 中文：把可选字符串列表解析为经过校验的 Adapter 标识集合。
fn parse_adapter_set(
    value: Option<&Value>,
    path: &'static str,
) -> Result<BTreeSet<AdapterId>, EvaError> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| AdapterId::parse(required_str(value, path)?))
        .collect()
}

/// 中文：把可选字符串列表解析为经过校验的 capability 名称集合。
fn parse_capability_set(
    value: Option<&Value>,
    path: &'static str,
) -> Result<BTreeSet<CapabilityName>, EvaError> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| CapabilityName::parse(required_str(value, path)?))
        .collect()
}

/// 中文：把可选字符串列表解析为保持配置顺序的具体主题。
fn parse_topic_list(value: Option<&Value>, path: &'static str) -> Result<Vec<Topic>, EvaError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| Topic::parse(required_str(value, path)?))
        .collect()
}

/// 中文：把可选字符串列表解析为保持配置顺序的主题匹配模式。
fn parse_topic_pattern_list(
    value: Option<&Value>,
    path: &'static str,
) -> Result<Vec<TopicPattern>, EvaError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| TopicPattern::parse(required_str(value, path)?))
        .collect()
}

/// 中文：要求 YAML 节点为列表，并在错误中保留字段路径。
fn sequence<'a>(value: &'a Value, path: &'static str) -> Result<&'a Vec<Value>, EvaError> {
    value.as_sequence().ok_or_else(|| {
        EvaError::invalid_argument("policy field must be a list").with_context("field", path)
    })
}

/// 中文：读取必需映射键，并拒绝空值或首尾空白以保证索引稳定。
fn required_key<'a>(value: &'a Value, path: &'static str) -> Result<&'a str, EvaError> {
    let value = required_str(value, path)?;
    if value.trim().is_empty() || value.trim() != value {
        Err(
            EvaError::invalid_argument("policy mapping key must be non-empty and trimmed")
                .with_context("field", path),
        )
    } else {
        Ok(value)
    }
}

/// 中文：要求 YAML 节点为字符串，并为类型错误附加字段路径。
fn required_str<'a>(value: &'a Value, path: &'static str) -> Result<&'a str, EvaError> {
    value.as_str().ok_or_else(|| {
        EvaError::invalid_argument("policy field must be a string").with_context("field", path)
    })
}

/// 中文：把必需且稳定的映射键解析为 capability 名称。
fn capability_key(value: &Value, path: &'static str) -> Result<CapabilityName, EvaError> {
    CapabilityName::parse(required_key(value, path)?)
}

/// 中文：由文档路径和域名生成可追溯的策略层名称，内存文档使用稳定默认前缀。
fn layer_name(document: &PolicyDocument, domain: &str) -> String {
    if document.path.as_os_str().is_empty() {
        format!("policy.{domain}")
    } else {
        format!("{}:{domain}", document.path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use eva_core::ErrorKind;
    use std::path::{Path, PathBuf};

    /// 中文：返回策略集成测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 中文：解析测试使用的 capability 名称。
    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    /// 中文：解析测试使用的 Adapter 标识。
    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[test]
    /// 中文：验证仓库样例策略的各类型化域和派生层均可正确加载。
    fn parses_sample_policy_domains() {
        let project = load_project_config(workspace_root()).unwrap();
        let domains = PolicyDomainSet::from_project(&project).unwrap();

        assert_eq!(domains.source_count, 5);
        assert_eq!(domains.adapter.defaults.max_timeout_ms, Some(120_000));
        assert!(domains
            .adapter
            .allow_write_workspace
            .contains(&adapter("codex-cli")));
        assert!(domains
            .adapter
            .deny_capabilities
            .contains(&capability("shell.execute")));
        assert!(domains.hardware.enabled);
        assert!(!domains.hardware.allow_raw_io);
        assert!(domains.hardware.allowed_buses.contains("usb"));
        assert!(domains.mcp_server.enabled);
        assert!(domains.mcp_server.tools.contains_key("adapter.invoke"));
        assert!(domains.memory.redaction.enabled);
        assert!(domains
            .memory
            .redaction
            .sensitive_key_fragments
            .contains("token"));
        assert!(domains
            .memory
            .redaction
            .sensitive_token_prefixes
            .contains("sk-"));
        assert!(!domains.lua_sandbox.filesystem_enabled);
        assert!(domains.layers().len() >= 3);
    }

    #[test]
    /// 中文：验证自定义脱敏配置会归一化敏感键并保留替换及审计开关。
    fn parses_memory_redaction_policy_domain() {
        let value = serde_yaml::from_str::<Value>(
            r#"
memory_policy:
  redaction:
    enabled: true
    audit_redactions: false
    replacement: "[MASKED]"
    sensitive_key_fragments:
      - Credential
      - Session_Token
    sensitive_token_prefixes:
      - pk-
"#,
        )
        .unwrap();
        let document = PolicyDocument::try_from(value).unwrap();

        let domains = PolicyDomainSet::from_documents(&[document]).unwrap();

        assert!(domains.memory.redaction.enabled);
        assert!(!domains.memory.redaction.audit_redactions);
        assert_eq!(domains.memory.redaction.replacement, "[MASKED]");
        assert!(domains
            .memory
            .redaction
            .sensitive_key_fragments
            .contains("credential"));
        assert!(domains
            .memory
            .redaction
            .sensitive_key_fragments
            .contains("session_token"));
        assert!(domains
            .memory
            .redaction
            .sensitive_token_prefixes
            .contains("pk-"));
    }

    #[test]
    /// 中文：验证由策略域派生的有效策略不会扩张权限和沙箱限制。
    fn effective_policy_intersects_domain_layers() {
        let project = load_project_config(workspace_root()).unwrap();
        let domains = PolicyDomainSet::from_project(&project).unwrap();
        let effective = domains.effective_policy().unwrap();

        assert!(!effective.permissions.network);
        assert!(!effective.permissions.shell);
        assert!(!effective.sandbox.permits_lua_lib("os"));
        assert_eq!(effective.sandbox.memory_mb, Some(64));
    }

    #[test]
    /// 中文：验证符合工具、capability、Provider 和超时白名单的 MCP 调用获准。
    fn runtime_gate_allows_declared_mcp_adapter_invoke() {
        let project = load_project_config(workspace_root()).unwrap();
        let gate = RuntimePolicyGate::from_project(&project).unwrap();

        let decision = gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::McpToolCall)
                .with_tool("adapter.invoke")
                .with_capability(capability("repo.analyze"))
                .with_provider(adapter("codex-cli"))
                .with_timeout_ms(30_000),
        );

        assert!(decision.allowed, "{decision:?}");
        assert!(decision.audit.contains(&"policy.decision:allow".to_owned()));
    }

    #[test]
    /// 中文：验证 Adapter 与 Provider 相同时凭据会话获准且包含审计动作。
    fn runtime_gate_allows_provider_credential_session_for_matching_provider() {
        let gate = RuntimePolicyGate::new(PolicyDomainSet::default());

        let decision = gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::ProviderCredentialSession)
                .with_adapter(adapter("stdio-test"))
                .with_provider(adapter("stdio-test"))
                .with_capability(capability("repo.analyze")),
        );

        assert!(decision.allowed, "{decision:?}");
        assert!(decision
            .audit
            .contains(&"policy.action:provider.credential_session".to_owned()));
    }

    #[test]
    /// 中文：验证门禁可查询 capability 专属退避配置，未配置项返回空值。
    fn runtime_gate_exposes_adapter_retry_backoff_by_capability() {
        let domains =
            PolicyDomainSet::from_project(&load_project_config(workspace_root()).unwrap()).unwrap();
        let gate = RuntimePolicyGate::new(domains);

        assert_eq!(
            gate.adapter_retry_backoff_ms(&capability("repo.analyze")),
            Some(1000)
        );
        assert_eq!(
            gate.adapter_retry_backoff_ms(&capability("chat.reply")),
            None
        );
    }

    #[test]
    /// 中文：验证凭据会话不能跨 Provider 复用。
    fn runtime_gate_denies_provider_credential_session_cross_provider() {
        let gate = RuntimePolicyGate::new(PolicyDomainSet::default());

        let decision = gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::ProviderCredentialSession)
                .with_adapter(adapter("stdio-test"))
                .with_provider(adapter("other-provider"))
                .with_capability(capability("repo.analyze")),
        );

        assert!(!decision.allowed);
        assert!(decision.reason.contains("across providers"));
    }

    #[test]
    /// 中文：验证关闭的 MCP 主题发出能力产生可转换为权限错误的拒绝判定。
    fn runtime_gate_denies_disabled_mcp_topic_emit() {
        let project = load_project_config(workspace_root()).unwrap();
        let gate = RuntimePolicyGate::from_project(&project).unwrap();

        let decision = gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::McpTopicEmit)
                .with_topic(Topic::parse("/input/user").unwrap()),
        );

        assert!(!decision.allowed);
        let error = decision.ensure_allowed().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    /// 中文：验证原始硬件 I/O 在默认策略下被拒绝。
    fn runtime_gate_denies_hardware_raw_io_by_default() {
        let project = load_project_config(workspace_root()).unwrap();
        let gate = RuntimePolicyGate::from_project(&project).unwrap();

        let decision = gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::HardwareRawIo)
                .with_capability(capability("hardware.raw.write"))
                .with_bus("usb"),
        );

        assert!(!decision.allowed);
        assert!(decision.reason.contains("raw I/O"));
    }

    #[test]
    /// 中文：验证恢复应用等高风险动作必须由运行时策略显式放行。
    fn runtime_gate_requires_explicit_high_risk_allow() {
        let project = load_project_config(workspace_root()).unwrap();
        let gate = RuntimePolicyGate::from_project(&project).unwrap();

        let decision = gate.decide(RuntimePolicyRequest::new(HighRiskAction::RestoreApply));

        assert!(!decision.allowed);
        assert!(decision.reason.contains("explicit"));
    }

    #[test]
    /// 中文：验证配置中的高风险动作白名单能够精确放行对应动作。
    fn runtime_policy_can_explicitly_allow_high_risk_action() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime_policy:
  allow_high_risk_actions:
    - upgrade.apply
"#,
        )
        .unwrap();
        let document = PolicyDocument::try_from(value).unwrap();
        let gate = RuntimePolicyGate::new(PolicyDomainSet::from_documents(&[document]).unwrap());

        let decision = gate.decide(RuntimePolicyRequest::new(HighRiskAction::UpgradeApply));

        assert!(decision.allowed);
    }
}
