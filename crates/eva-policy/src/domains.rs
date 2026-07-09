//! Policy domain parsing and runtime gate decisions.

use crate::{EffectivePolicy, PermissionSet, PolicyLayer, SandboxPolicy};
use eva_config::{policy::PolicyDocument, ProjectConfig};
use eva_core::{AdapterId, AgentId, CapabilityName, EvaError, Topic, TopicPattern};
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Typed policy domains loaded from `config/policies/*.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyDomainSet {
    pub source_count: usize,
    pub adapter: AdapterPolicyDomain,
    pub hardware: HardwarePolicyDomain,
    pub mcp_server: McpServerPolicyDomain,
    pub runtime: RuntimePolicyDomain,
    pub lua_sandbox: SandboxPolicy,
    layers: Vec<PolicyLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterPolicyDomain {
    pub defaults: PermissionSet,
    pub allow_write_workspace: BTreeSet<AdapterId>,
    pub deny_capabilities: BTreeSet<CapabilityName>,
    pub skill: SkillPolicy,
    pub retry: RetryPolicyDomain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPolicy {
    pub require_schema: bool,
    pub allow_user_local_skills: bool,
    pub allowed_runtime_gates: BTreeSet<String>,
    pub deny_kinds: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetryPolicyDomain {
    pub default_max_attempts: u32,
    pub capabilities: BTreeMap<CapabilityName, CapabilityRetryPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwarePolicyDomain {
    pub enabled: bool,
    pub allow_raw_io: bool,
    pub claim: String,
    pub network_bridge: bool,
    pub max_timeout_ms: Option<u64>,
    pub allowed_buses: BTreeSet<String>,
    pub denied_capabilities: BTreeSet<CapabilityName>,
    pub hotplug: HardwareHotplugPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareHotplugPolicy {
    pub require_identity_match: bool,
    pub quarantine_unknown_devices: bool,
    pub emit_events: Vec<Topic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpServerPolicyDomain {
    pub enabled: bool,
    pub bind: Option<String>,
    pub tools: BTreeMap<String, McpToolPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpToolPolicy {
    pub enabled: bool,
    pub allowed_agents: BTreeSet<AgentId>,
    pub allowed_capabilities: BTreeSet<CapabilityName>,
    pub allowed_providers: BTreeSet<AdapterId>,
    pub allowed_topics: Vec<TopicPattern>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimePolicyDomain {
    pub allow_high_risk_actions: BTreeSet<HighRiskAction>,
}

/// Stable high-risk runtime actions understood by the policy gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HighRiskAction {
    AdapterInvoke,
    ProviderCredentialSession,
    McpToolCall,
    McpTopicEmit,
    SkillRun,
    HardwareBind,
    HardwareRawIo,
    BackupCreate,
    RestoreApply,
    UpgradeApply,
    ReleasePointerMutation,
    SupervisorHandoff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyRequest {
    pub action: HighRiskAction,
    pub tool: Option<String>,
    pub agent: Option<AgentId>,
    pub capability: Option<CapabilityName>,
    pub provider: Option<AdapterId>,
    pub adapter: Option<AdapterId>,
    pub topic: Option<Topic>,
    pub bus: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub action: HighRiskAction,
    pub allowed: bool,
    pub reason: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyGate {
    domains: PolicyDomainSet,
}

impl Default for SkillPolicy {
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
    fn default() -> Self {
        Self {
            require_identity_match: true,
            quarantine_unknown_devices: true,
            emit_events: Vec::new(),
        }
    }
}

impl PolicyDomainSet {
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        Self::from_documents(&project.policies)
    }

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

    pub fn layers(&self) -> &[PolicyLayer] {
        &self.layers
    }

    pub fn effective_policy(&self) -> Result<EffectivePolicy, EvaError> {
        EffectivePolicy::from_layers(self.layers.clone())
    }
}

impl HighRiskAction {
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

    pub fn with_tool(mut self, value: impl Into<String>) -> Self {
        self.tool = Some(value.into());
        self
    }

    pub fn with_agent(mut self, value: AgentId) -> Self {
        self.agent = Some(value);
        self
    }

    pub fn with_capability(mut self, value: CapabilityName) -> Self {
        self.capability = Some(value);
        self
    }

    pub fn with_provider(mut self, value: AdapterId) -> Self {
        self.provider = Some(value);
        self
    }

    pub fn with_adapter(mut self, value: AdapterId) -> Self {
        self.adapter = Some(value);
        self
    }

    pub fn with_topic(mut self, value: Topic) -> Self {
        self.topic = Some(value);
        self
    }

    pub fn with_bus(mut self, value: impl Into<String>) -> Self {
        self.bus = Some(value.into());
        self
    }

    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = Some(value);
        self
    }
}

impl PolicyDecision {
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
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        Ok(Self::new(PolicyDomainSet::from_project(project)?))
    }

    pub fn new(domains: PolicyDomainSet) -> Self {
        Self { domains }
    }

    pub fn domains(&self) -> &PolicyDomainSet {
        &self.domains
    }

    pub fn adapter_retry_backoff_ms(&self, capability: &CapabilityName) -> Option<u64> {
        self.domains
            .adapter
            .retry
            .capabilities
            .get(capability)
            .and_then(|policy| policy.backoff_ms)
    }

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

fn hardware_permissions(policy: &HardwarePolicyDomain) -> PermissionSet {
    let mut permissions = PermissionSet::deny_all();
    permissions.network = policy.network_bridge;
    permissions.max_timeout_ms = policy.max_timeout_ms;
    permissions
}

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

fn expect_mapping<'a>(value: &'a Value, path: &'static str) -> Result<&'a Mapping, EvaError> {
    value.as_mapping().ok_or_else(|| {
        EvaError::invalid_argument("policy domain field must be a mapping")
            .with_context("field", path)
    })
}

fn get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_owned()))
}

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

fn parse_topic_list(value: Option<&Value>, path: &'static str) -> Result<Vec<Topic>, EvaError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    sequence(value, path)?
        .iter()
        .map(|value| Topic::parse(required_str(value, path)?))
        .collect()
}

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

fn sequence<'a>(value: &'a Value, path: &'static str) -> Result<&'a Vec<Value>, EvaError> {
    value.as_sequence().ok_or_else(|| {
        EvaError::invalid_argument("policy field must be a list").with_context("field", path)
    })
}

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

fn required_str<'a>(value: &'a Value, path: &'static str) -> Result<&'a str, EvaError> {
    value.as_str().ok_or_else(|| {
        EvaError::invalid_argument("policy field must be a string").with_context("field", path)
    })
}

fn capability_key(value: &Value, path: &'static str) -> Result<CapabilityName, EvaError> {
    CapabilityName::parse(required_key(value, path)?)
}

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

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[test]
    fn parses_sample_policy_domains() {
        let project = load_project_config(workspace_root()).unwrap();
        let domains = PolicyDomainSet::from_project(&project).unwrap();

        assert_eq!(domains.source_count, 4);
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
        assert!(!domains.lua_sandbox.filesystem_enabled);
        assert!(domains.layers().len() >= 3);
    }

    #[test]
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
    fn runtime_gate_requires_explicit_high_risk_allow() {
        let project = load_project_config(workspace_root()).unwrap();
        let gate = RuntimePolicyGate::from_project(&project).unwrap();

        let decision = gate.decide(RuntimePolicyRequest::new(HighRiskAction::RestoreApply));

        assert!(!decision.allowed);
        assert!(decision.reason.contains("explicit"));
    }

    #[test]
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
