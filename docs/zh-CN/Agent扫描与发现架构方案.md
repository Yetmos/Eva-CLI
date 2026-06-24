> Language: 简体中文
> Canonical source: ../en/agent-discovery.md
> Translation status: current

# Agent 扫描与发现架构方案

更新日期：2026-06-12

文档关系：

- 总体入口：`总体架构方案.md`
- 配置体系：`项目配置方案.md`
- Agent 调度核心：`Rust与Lua事件总线智能体调度架构方案.md`
- 外部 Agent 扩展：`Lua调用外部Agent动态Adapter架构方案.md`
- Lua Capability 热更新：`Lua承载Skill-MCP-Tool热更新架构方案.md`
- 外接硬件接入与热插拔：`外接硬件接入与热插拔架构方案.md`

## 1. 方案定位

本文定义 Eva-CLI 如何扫描、识别、校验并注册用户电脑上可用的 Agent 能力。

核心结论：

- Agent 扫描由 **Rust Runtime** 负责，不能由 Lua 直接访问文件系统、环境变量或 shell。
- 扫描结果分为 **内部 Lua Agent** 和 **外部 Agent Adapter** 两类。
- 内部 Lua Agent 注册到 Scheduler，参与 Topic EventBus 调度。
- 外部 Agent、CLI、MCP server、本地模型、工作流能力和已授权硬件能力注册到 AdapterRegistry。
- 扫描只做发现和归一化，不代表自动授权调用；调用前仍必须经过 policy 校验。
- 默认只扫描白名单目录、显式配置目录和白名单命令，不做全盘扫描。

该方案的目标不是寻找电脑上的所有可执行文件，而是建立一套可控、可观测、可缓存、可热加载的 Agent Discovery 机制。

## 2. 目标与非目标

### 2.1 目标

- 自动发现项目内配置的 Lua Agent。
- 自动发现用户环境中可用的外部 Agent 能力，例如 Codex、Claude、Gemini、Ollama、MCP server、OMX skills 和显式配置的硬件设备。
- 将不同来源的能力归一成统一的 discovered agent 数据模型。
- 为 Scheduler 和 AdapterRegistry 提供稳定注册入口。
- 支持 CLI 查看、重新扫描、健康检查和缓存。
- 支持跨平台路径规则，覆盖 macOS、Linux 和 Windows。
- 保证扫描过程不泄露密钥、不执行不可信脚本、不扩大 Lua 权限。

### 2.2 非目标

- 不做全盘文件扫描。
- 不自动读取 SSH key、API key、浏览器数据或 shell history。
- 不让 Lua 直接扫描用户电脑。
- 不因为发现某个命令存在就默认允许所有 Agent 调用它。
- 不把任意可执行文件都包装成 Agent。
- 不绕过 Adapter manifest、policy 和 capability 路由。
- 不主动 claim 未授权硬件设备，也不把设备发现等同于硬件调用授权。

## 3. 总体架构

```text
Eva-CLI Startup / CLI scan command / Hot reload
                    |
                    v
          [AgentDiscoveryService - Rust]
                    |
        +-----------+-----------+
        |                       |
        v                       v
 [InternalAgentScanner]   [ExternalAdapterScanner]
        |                       |
        v                       v
 config/agents/**/agent.yaml config/adapters/*.yaml
 config/agents/**/main.lua   ~/.codex/prompts/*.md
                           ~/.codex/skills/*/SKILL.md
                           ~/.agents/skills/*/SKILL.md
                           PATH allowlist commands
                           MCP manifests
                           config/capabilities/*.yaml
                           config/adapters/hardware/*.yaml
        |                       |
        +-----------+-----------+
                    |
                    v
          [DiscoveryNormalizer]
                    |
                    v
          [Schema + Policy Precheck]
                    |
                    v
          [DiscoveryCache]
                    |
        +-----------+-----------+
        |                       |
        v                       v
   [Scheduler]            [AdapterRegistry]
 Lua Agent runtime        External Agent adapters
        |                       |
        +-----------+-----------+
                    |
                    v
              [EventBus Topics]
```

系统新增一个核心服务：

```text
AgentDiscoveryService
```

它属于 Rust Runtime，不属于 Lua Agent，也不属于外部 Adapter。

## 4. 扫描对象分类

### 4.1 内部 Lua Agent

内部 Lua Agent 是系统自身运行的 Agent，由配置文件和 Lua 脚本组成。

默认扫描来源：

```text
config/agents/**/agent.yaml
config/agents/**/main.lua
```

推荐以 `config/agents/**/agent.yaml` 为准。Lua 脚本默认放在同一个 Agent 目录下，例如 `main.lua`。旧的散落脚本只能作为兼容辅助来源，不能跳过配置和权限校验。

内部 Lua Agent 推荐采用一个 Agent 一个目录：

```text
config/agents/
  root/
    agent.yaml
    main.lua
  route-a/
    agent.yaml
    main.lua
    route-aa/
      agent-a11/
        agent.yaml
        main.lua
      agent-a12/
        agent.yaml
        main.lua
```

目录可以物理嵌套，但运行时父子关系必须以 `agent.yaml` 中的 `parent`、`children`、`subscriptions` 和 `permissions.emit` 为准。

示例：

```yaml
id: agent-a
enabled: true
parent: root-agent
script: main.lua
subscriptions:
  - /sys/route-a
children:
  - agent-a11
  - agent-a12
permissions:
  emit:
    - /sys/route-a/**
  tools:
    - invoke_agent
```

发现后注册到：

```text
Scheduler Agent Registry
```

### 4.2 外部 CLI Agent

外部 CLI Agent 是通过命令行调用的外部能力。

默认白名单命令：

```text
codex
claude
gemini
ollama
```

扫描方式：

- 在 `PATH` 中查找命令是否存在。
- 执行受控 health/version 命令，例如 `--version`。
- 不执行自然语言 prompt。
- 不读取 shell profile 之外的隐式配置。
- 不自动推断 API key 是否有效，只标记认证状态未知或待验证。

发现后注册为 `stdio` 或 `process` 类 Adapter。

PATH 命令发现必须额外处理跨平台差异：

- Windows 下需要区分 `.exe`、`.cmd`、`.bat`、PowerShell shim 和 npm shim。
- macOS / Linux 下需要处理符号链接、wrapper script 和权限位。
- 同名命令出现在多个 PATH 目录时，必须记录全部候选，并按规则选择一个或标记 `command_ambiguous`。
- 命令路径必须 canonicalize，并记录真实路径和显示路径。
- 项目 workspace 内的同名命令不能默认覆盖系统命令，除非显式配置允许。
- health check 输出必须限制大小，避免命令异常输出大量内容。
- health check stderr 只能截断保存摘要，不能把完整 stderr 当作日志长期保存。

内置白名单命令也应有固定 probe 规格：

```text
command  probe args     expected
codex    --version      exit 0 or known version output
claude   --version      exit 0 or known version output
gemini   --version      exit 0 or known version output
ollama   --version      exit 0 or known version output
```

禁止使用自然语言 prompt、联网请求或会修改本地状态的命令作为 discovery probe。

### 4.3 外部 HTTP/API Agent

外部 HTTP/API Agent 默认不通过局域网或公网探测发现，只通过显式 manifest 发现。

默认扫描来源：

```text
config/adapters/*.yaml
```

示例：

```yaml
id: claude-api
name: Claude API Adapter
transport: http
endpoint_env: ANTHROPIC_BASE_URL
api_key_env: ANTHROPIC_API_KEY
capabilities:
  - chat.reply
  - task.plan
```

Discovery 只校验字段完整性和 policy，不主动发送真实业务请求。

### 4.4 MCP Server

MCP server 只能通过显式配置或受信任配置目录发现。

默认扫描来源：

```text
config/adapters/*mcp*.yaml
config/mcp/*.yaml
```

可选兼容来源：

```text
~/.config/eva/mcp/*.yaml
```

MCP discovery 应读取：

- server id
- transport
- command 或 endpoint
- tools allowlist
- resources allowlist
- prompts allowlist
- timeout
- trust level

MCP server 发现后注册到 `McpAdapter`，不能直接暴露给 Lua。

MCP discovery 只读取 manifest 和必要的 tool/resource/prompt 元数据。对 MCP server 的 `tools/list`、`resources/list`、`prompts/list` 建议按 policy 控制：

- 显式 manifest 可允许启动 MCP server 做 metadata probe。
- 用户目录自动发现的 MCP 配置默认不启动 server，只展示 manifest。
- metadata probe 必须有超时、输出大小限制和并发限制。
- tool schema 需要记录 hash，用于判断 cache 是否失效。
- MCP tool capability 映射必须经过 allowlist，不能把 server 返回的所有 tool 自动暴露为业务 capability。

### 4.5 OMX / Codex Skills 与 Prompts

如果用户安装了 Codex 或 oh-my-codex，可以扫描其本地 prompt 和 skill。

默认来源：

```text
~/.codex/prompts/*.md
~/.codex/skills/*/SKILL.md
~/.agents/prompts/*.md
~/.agents/skills/*/SKILL.md
```

这些对象不一定都是可独立执行的 Agent。Discovery 应区分：

```text
prompt_role     可作为角色提示词
workflow_skill  可作为工作流能力
runtime_worker  仅 team/swarm 运行态使用
```

例如 `worker` skill 只能在 team/swarm 运行态下使用，不能在普通模式下作为通用外部 Agent 自动调用。

Skill discovery 只负责识别和归一化，不等于授权执行。可调用 skill 还必须满足：

- 来源目录受信任，或存在项目内显式 `skill` Adapter manifest。
- 能归类为 `workflow_skill`，不是 `runtime_worker`。
- 有稳定 capability，例如 `workflow.code_review`。
- 有输入 schema、输出 schema 和运行态要求。
- policy 允许当前 trust level、runtime gate 和 workspace 权限。

缺少 schema 的 skill 可以展示在 `eva agent list`，但不能自动注册为可调用 Adapter。

Skill 类型识别建议不要只依赖文件名。优先级：

```text
项目内显式 skill Adapter manifest
  -> SKILL.md front matter / metadata
  -> 受信任目录约定
  -> 内容启发式分类
```

启发式分类只能用于展示，不能用于自动注册为可调用 Adapter。

推荐记录：

```text
skill_id
skill_path
skill_kind
runtime_gate
declared_permissions
input_schema_present
output_schema_present
source_trust
```

### 4.6 外接硬件设备

外接硬件设备具有持续 watch 和热插拔语义，不完全等同于启动时的一次性文件扫描。

默认扫描来源：

```text
config/adapters/hardware/*.yaml
config/policies/hardware.yaml
OS hotplug notification
startup hardware enumeration
```

硬件 discovery 只做：

- 读取硬件 manifest。
- 枚举可观察设备描述。
- 将设备描述归一为受控 `DeviceDescriptor`。
- 判断设备是否匹配 manifest。
- 标记 `observed`、`matched`、`authorized` 等状态。

硬件 discovery 不做：

- 不自动 claim 未授权设备。
- 不把操作系统设备路径暴露给 Lua。
- 不发送 raw IO。
- 不自动安装驱动、修改系统权限或触发蓝牙配对。

授权后的硬件能力注册到 AdapterRegistry，由 HardwareAdapterRuntime 负责设备句柄、协议、热插拔和命令队列。详细状态机和事件契约见 `外接硬件接入与热插拔架构方案.md`。

## 5. 数据模型

Discovery 的数据模型分两层：

- `DiscoveredAgent` 描述扫描阶段发现到的原始能力。
- `RegistrationDecision` 描述该能力是否被注册、注册到哪里、为什么被拒绝或降级。

不要把 `DiscoveredAgent` 直接等同于运行态对象。运行态对象仍然是 Scheduler 中的 `AgentHandle` 或 AdapterRegistry 中的 `AgentAdapter`。

### 5.1 DiscoveredAgent

```rust
pub struct DiscoveredAgent {
    pub id: String,
    pub name: Option<String>,
    pub kind: DiscoveredAgentKind,
    pub source: DiscoverySource,
    pub agent_dir: Option<PathBuf>,
    pub provider: Option<String>,
    pub capabilities: Vec<String>,
    pub manifest_path: Option<PathBuf>,
    pub command: Option<PathBuf>,
    pub args: Vec<String>,
    pub script_path: Option<PathBuf>,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub subscriptions: Vec<String>,
    pub enabled: bool,
    pub trust_level: TrustLevel,
    pub health: DiscoveryHealth,
    pub policy_ref: Option<String>,
    pub metadata: serde_json::Value,
}
```

建议补充以下实现约束：

- `id` 必须在归一化阶段生成稳定值，同一个来源重复扫描不能变化。
- `manifest_path`、`command`、`script_path` 必须保存规范化后的绝对路径，同时 CLI 输出可显示相对路径。
- `metadata` 只能保存非敏感补充信息，不允许保存密钥、token、完整环境变量值或未脱敏命令输出。
- `capabilities` 只能引用 capability 注册表中已知能力；未知 capability 可保留展示，但默认不能自动路由。

### 5.2 DiscoveredAgentKind

```rust
pub enum DiscoveredAgentKind {
    LuaAgent,
    CliAgent,
    HttpAgent,
    McpServer,
    CodexPrompt,
    WorkflowSkill,
    LocalModel,
    InternalEventBusAgent,
}
```

### 5.3 DiscoverySource

```rust
pub enum DiscoverySource {
    ProjectConfig(PathBuf),
    ProjectScript(PathBuf),
    UserCodexPrompt(PathBuf),
    UserCodexSkill(PathBuf),
    UserAgentsSkill(PathBuf),
    PathCommand(PathBuf),
    ExplicitManifest(PathBuf),
    RuntimeState(PathBuf),
}
```

### 5.4 TrustLevel

```rust
pub enum TrustLevel {
    Project,
    UserLocal,
    SystemPath,
    ExplicitRemote,
    Unknown,
}
```

`Unknown` 只能展示，默认不能注册为可调用 Adapter。

### 5.5 DiscoveryHealth

```rust
pub struct DiscoveryHealth {
    pub status: DiscoveryHealthStatus,
    pub checked_at: DateTime<Utc>,
    pub version: Option<String>,
    pub message: Option<String>,
}
```

```rust
pub enum DiscoveryHealthStatus {
    NotChecked,
    Available,
    Degraded,
    Unavailable,
    PermissionDenied,
    InvalidManifest,
}
```

### 5.6 RegistrationDecision

注册决策用于解释“发现到了什么”和“实际启用了什么”之间的差异。

```rust
pub struct RegistrationDecision {
    pub discovered_id: String,
    pub target: RegistrationTarget,
    pub action: RegistrationAction,
    pub reason: RegistrationReason,
    pub policy_ref: Option<String>,
    pub shadowed_by: Option<String>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}
```

```rust
pub enum RegistrationTarget {
    Scheduler,
    AdapterRegistry,
    DisplayOnly,
}

pub enum RegistrationAction {
    Registered,
    Rejected,
    Disabled,
    Shadowed,
    Degraded,
}
```

推荐 reason code：

```text
registered
disabled_by_manifest
schema_invalid
policy_denied
health_unavailable
health_permission_denied
capability_unknown
capability_missing_schema
runtime_gate_mismatch
trust_level_unknown
duplicate_id_shadowed
path_not_allowed
command_not_allowed
command_ambiguous
manifest_required
```

`eva agent explain <id>` 必须优先展示 `RegistrationDecision`，而不是只展示 health status。

### 5.7 CapabilityDescriptor

建议为 capability 建立集中注册表，避免不同 Adapter 使用同名不同义的字符串。

```rust
pub struct CapabilityDescriptor {
    pub name: String,
    pub description: Option<String>,
    pub input_schema_ref: Option<String>,
    pub output_schema_ref: Option<String>,
    pub side_effect: CapabilitySideEffect,
    pub default_timeout_ms: u64,
    pub retry_policy_ref: Option<String>,
    pub allow_auto_route: bool,
}
```

```rust
pub enum CapabilitySideEffect {
    None,
    ReadOnlyExternal,
    WritesWorkspace,
    WritesExternal,
    ShellOrProcess,
}
```

最小注册表至少覆盖：

```text
chat.reply
task.plan
repo.analyze
code.review
code.generate
workflow.code_review
mcp.tool.call
mcp.resource.read
mcp.prompt.render
```

业务别名，例如 `github.issue.create`，必须能解析到具体 Adapter、MCP tool 或内部实现。

## 6. 扫描流程

Discovery 流程必须产出可解释的中间状态，推荐内部状态机：

```text
source_enumerated
  -> raw_item_found
  -> normalized
  -> schema_validated
  -> policy_prechecked
  -> health_checked
  -> decision_recorded
  -> registered | rejected | display_only | shadowed
```

任一阶段失败都不应直接丢弃对象，而应生成 `DiscoveryDiagnostic`，供 CLI、UI、审计日志和 cache 展示。

### 6.1 启动扫描

```text
读取 config/eva.yaml
  -> 解析 discovery 配置
  -> 扫描 config/agents
  -> 扫描 config/adapters
  -> 扫描用户 prompt/skill 目录
  -> 查找 PATH 白名单命令
  -> 归一化 discovered agent
  -> schema 校验
  -> policy 预检查
  -> health check
  -> 写入 discovery cache
  -> 注册到 Scheduler 或 AdapterRegistry
  -> 发布发现事件
```

启动扫描建议分为两个阶段：

```text
Phase A: scan + normalize + validate
Phase B: register + publish events
```

只有 Phase A 全部完成后，才进入 Phase B。这样可以在注册前发现重复 ID、capability 冲突和 policy 冲突，避免半注册状态。

### 6.2 手动扫描

CLI 命令：

```bash
eva agent scan
eva agent list
eva adapter scan
eva adapter list
eva mcp scan
```

推荐参数：

```bash
eva agent scan --json
eva agent scan --refresh
eva agent scan --source codex
eva agent scan --source project
eva agent scan --no-health-check
```

推荐追加参数：

```bash
eva agent scan --dry-run
eva agent scan --diff
eva agent scan --source path
eva agent scan --source mcp
eva agent scan --explain-invalid
eva agent list --all
eva agent list --registered
eva agent list --invalid
eva discovery cache inspect
eva discovery cache clear
```

`--dry-run` 只扫描、校验和输出注册决策，不修改 Scheduler、AdapterRegistry 或 cache。

`--diff` 展示本次扫描相对当前 cache / runtime registry 的变化：

```text
added
removed
changed
health_changed
policy_changed
shadowed
unshadowed
```

### 6.3 热加载扫描

监听以下目录变化：

```text
config/agents/
config/adapters/
config/mcp/
```

用户目录下的 `~/.codex` 和 `~/.agents` 默认不做高频文件监听，只在启动或手动 scan 时刷新，避免额外开销和隐私争议。

### 6.4 热加载事务

热加载不能边扫描边修改运行态注册表。推荐事务流程：

```text
文件变化 debounce
  -> 构建新的 discovery snapshot
  -> schema 校验
  -> policy 预检查
  -> health check
  -> 计算 registry diff
  -> 新 Agent / Adapter 预热
  -> 原子替换 Scheduler / AdapterRegistry 索引
  -> 旧对象进入 draining
  -> 发布 /discovery/reloaded
```

失败处理：

- 新配置校验失败时，保留旧 registry。
- 新 Adapter health check 失败时，按 policy 决定降级、展示或拒绝注册。
- 被删除的 Adapter 先停止新路由，再等待 inflight 完成或超时取消。
- 被删除的 Lua Agent 先停止接收新事件，再 drain inbox 或写入死信队列。
- 订阅关系变化必须和 Agent runtime 版本一起提交，避免 Topic 表与 Agent 实例不一致。

### 6.5 并发与超时

扫描过程应有全局和局部限制：

```text
global_scan_timeout_ms
max_concurrent_scanners
max_concurrent_health_checks
max_files_per_dir
max_manifest_bytes
max_command_output_bytes
command_check_timeout_ms
```

单个来源超时不能阻塞其他来源。全局超时触发时，应返回部分结果和明确的 `DiscoveryErrorKind::Timeout`。

## 7. 配置设计

在 `config/eva.yaml` 中新增 discovery 配置：

```yaml
discovery:
  enabled: true
  scan_on_startup: true
  cache_path: .eva/data/discovery-cache.json
  health_check: true
  watch_project_dirs: true
  scan_user_dirs: true
  scan_path_commands: true

  project:
    agent_dir: config/agents
    agent_manifest_glob: "**/agent.yaml"
    adapter_dir: config/adapters
    capability_dir: config/capabilities
    capability_manifest_glob: "*.yaml"
    mcp_dir: config/mcp

  user:
    codex_prompt_dir: ~/.codex/prompts
    codex_skill_dir: ~/.codex/skills
    agents_prompt_dir: ~/.agents/prompts
    agents_skill_dir: ~/.agents/skills

  path_commands:
    allowlist:
      - codex
      - claude
      - gemini
      - ollama

  limits:
    max_files_per_dir: 500
    max_manifest_bytes: 1048576
    max_command_output_bytes: 65536
    global_scan_timeout_ms: 30000
    max_concurrent_scanners: 8
    max_concurrent_health_checks: 4
    command_check_timeout_ms: 2000

  schema:
    schema_dir: config/schemas
    cache_schema_version: 1

  trust:
    allow_user_local_skills: false
    allow_user_local_prompts: true
    allow_system_path_commands: true
    require_explicit_manifest_for_http: true
```

### 7.1 Schema 文件

推荐为以下配置提供 JSON Schema：

```text
config/schemas/eva.schema.json
config/schemas/discovery.schema.json
config/schemas/agent.schema.json
config/schemas/adapter.schema.json
config/schemas/mcp.schema.json
config/schemas/capability.schema.json
```

YAML 配置加载流程：

```text
YAML -> serde_yaml::Value -> serde_json::Value -> JSON Schema validate -> strongly typed Rust config
```

所有 schema 校验错误应包含：

```text
source path
json pointer
expected type or constraint
actual value summary
```

不要在错误中输出密钥、完整环境变量值或大段 manifest 内容。

### 7.2 配置覆盖与权限收紧

Discovery 配置只决定“是否扫描”和“从哪里扫描”，不能直接放宽 Adapter 或 Agent 权限。

权限合并顺序应保持：

```text
系统默认 policy
  -> discovery trust policy
  -> Adapter / Agent manifest
  -> Agent permissions
  -> 用户/会话 policy
  -> request 级约束
```

最终权限只能收紧，不能放宽。

### 7.3 用户目录扫描默认策略

用户目录扫描容易引发隐私和误注册问题，建议默认策略：

- `scan_user_dirs` 可以为 true，但 `allow_user_local_skills` 默认为 false。
- 用户 prompt 可以展示为 `prompt_role`，默认不注册为 Adapter。
- 用户 workflow skill 没有项目内显式 Adapter manifest 时，只展示，不自动调用。
- 用户目录扫描不做热监听，只在启动或手动 scan 时执行。

## 8. EventBus Topic

Discovery 过程应发布结构化事件，便于观测、审计和 UI 展示。

推荐 Topic：

```text
/discovery/started
/discovery/agent/found
/discovery/adapter/found
/discovery/agent/invalid
/discovery/adapter/invalid
/discovery/health/checked
/discovery/completed
/discovery/failed
```

示例事件：

```json
{
  "topic": "/discovery/adapter/found",
  "source": "system:discovery",
  "payload": {
    "id": "codex-cli",
    "kind": "CliAgent",
    "source": "PathCommand",
    "capabilities": ["repo.analyze", "code.review"],
    "health": {
      "status": "Available",
      "version": "codex-cli 1.2.3"
    }
  }
}
```

## 9. 注册规则

### 9.1 注册到 Scheduler

满足以下条件的对象注册到 Scheduler：

- `kind = LuaAgent`
- `enabled = true`
- 配置 schema 有效
- Lua script 存在且位于允许目录
- subscriptions 合法
- permissions 合法

注册结果：

```text
AgentId -> AgentHandle
TopicPattern -> AgentId
```

注册前必须额外校验：

- `id` 唯一，且不与保留系统 ID 冲突。
- `script_path` 在允许目录内，且不是越权符号链接。
- `parent`、`children` 引用存在，缺失时按 policy 拒绝或降级。
- `subscriptions` 与 `permissions.emit` 使用同一套 Topic parser 校验。
- `permissions.tools` 含 `invoke_agent` 时，必须显式配置可调用 capability 或 provider。
- Agent 注册后的订阅表版本必须记录到 runtime state，便于热加载回滚。

### 9.2 注册到 AdapterRegistry

满足以下条件的对象注册到 AdapterRegistry：

- `kind` 属于 `CliAgent`、`HttpAgent`、`McpServer`、`LocalModel` 或 `WorkflowSkill`
- manifest 或内置 adapter template 有效
- capability 非空
- policy 允许注册
- health 不是 `InvalidManifest`

`WorkflowSkill` 额外要求：

- `metadata.kind = workflow_skill`
- `metadata.runtime_gate` 与当前运行态匹配
- `metadata.input_schema` 和 `metadata.output_schema` 存在
- 不是 team/swarm 专用 worker
- 没有要求未授权的 shell、network 或 workspace write 权限

注册结果：

```text
AdapterId -> AgentAdapter
Capability -> AdapterId candidates
```

注册前必须额外校验：

- `AdapterId` 唯一，且与 shadowed 项关系明确。
- transport 参数完整，例如 `stdio.command`、`http.endpoint`、`mcp.server_transport`。
- command / endpoint / skill path 不能由 Lua 或事件 payload 覆盖。
- capability 必须存在于 capability 注册表，或者 manifest 明确声明 `allow_unknown_capability: true` 且 policy 允许。
- 具有副作用的 capability 默认不能自动路由，除非 capability descriptor 允许。
- 写 workspace、写外部系统、shell/process 类能力必须有 audit 字段。

### 9.3 不自动注册的对象

以下对象只展示，不自动注册：

- trust level 为 `Unknown`
- 没有 capability 的 prompt 文件
- team/swarm 专用 worker
- 缺少输入输出 schema 的 workflow skill
- runtime gate 与当前运行态不匹配的 skill
- 缺少 manifest 的远程 HTTP 服务
- health 为 `PermissionDenied` 或 `InvalidManifest`

### 9.4 注册决策事件

每个对象都应产生注册决策事件：

```text
/discovery/registration/decided
```

示例：

```json
{
  "topic": "/discovery/registration/decided",
  "source": "system:discovery",
  "payload": {
    "id": "code-review",
    "target": "AdapterRegistry",
    "action": "Rejected",
    "reason": "capability_missing_schema",
    "diagnostics": [
      {
        "level": "error",
        "message": "workflow skill is missing output_schema"
      }
    ]
  }
}
```

该事件应脱敏，不输出完整 manifest、环境变量值或密钥路径内容。

## 10. 去重与优先级

同一个 Agent 能力可能来自多个来源。

推荐优先级：

```text
project manifest
  -> explicit user config
  -> builtin adapter template
  -> PATH discovery
  -> user prompt/skill discovery
```

去重 key：

```text
kind + id
```

如果同名不同来源冲突：

- 项目配置优先。
- 用户显式配置优先于自动扫描。
- 自动扫描结果必须记录 shadowed_by。
- CLI list 应展示冲突原因。

### 10.1 冲突类型

建议区分以下冲突：

```text
duplicate_id_same_source
duplicate_id_different_source
same_capability_multiple_provider
same_command_multiple_path
same_skill_multiple_directory
manifest_overrides_auto_discovery
```

不是所有冲突都应拒绝：

- 同 capability 多 provider 是正常情况，交给 AdapterRouter 排序。
- 同 ID 多来源必须 shadow 或拒绝。
- 同 command 多路径应按 PATH 顺序选择，同时记录所有候选。
- 项目显式 manifest 可以覆盖 PATH 自动发现。

### 10.2 Pin 与 Override

建议支持显式 pin：

```yaml
discovery:
  overrides:
    codex-cli:
      prefer_source: project_manifest
      command_path: /usr/local/bin/codex
```

pin 必须经过 path allowlist 和 policy 校验，不能绕过安全边界。

## 11. 安全策略

Discovery 必须遵守以下安全边界：

- 不执行被扫描目录中的任意脚本。
- 不读取密钥内容。
- 不打印环境变量值。
- 不把 `PATH` 中任意命令注册为 Agent。
- 不跨出 workspace 读取项目 Agent 脚本，除非配置显式允许。
- 不允许 Lua 修改 discovery 配置。
- 不允许发现结果绕过 adapter policy。
- health check 只能执行白名单参数，例如 `--version`。
- 扫描失败不能阻塞核心运行时启动，除非项目配置声明为 required。

### 11.1 脱敏策略

Discovery 产生的日志、事件、cache 和 CLI 输出都必须脱敏：

- 环境变量只显示变量名，不显示值。
- 命令输出最多保存截断摘要。
- 路径可以显示，但敏感目录内容不能展开。
- manifest 中疑似 secret 的字段值必须替换为 `<redacted>`。
- 错误堆栈不能包含完整 request payload、token 或 provider 响应正文。

推荐敏感字段名匹配：

```text
token
api_key
apikey
secret
password
credential
authorization
cookie
private_key
```

### 11.2 路径安全

所有路径进入系统前必须规范化：

```text
expand home
resolve relative path
canonicalize when path exists
reject parent traversal outside allowed roots
record display path separately
```

符号链接处理：

- 项目 Agent 脚本不能通过 symlink 跳出允许目录。
- 用户显式配置目录可以允许 symlink，但必须记录真实路径和 trust level。
- cache 中保存 source hash 时，应基于真实文件内容，而不是只基于 symlink 路径。

### 11.3 Health Check 副作用边界

health check 是 discovery 的一部分，不能产生业务副作用：

- 不发送自然语言 prompt。
- 不调用会写 workspace 或外部系统的命令。
- 不触发 provider 真实推理请求。
- 不验证 API key 是否可用，除非 Adapter manifest 明确允许只读 metadata probe。
- 不启动长期驻留进程，除非是显式配置的 MCP metadata probe 且有超时。

## 12. 缓存与失效

扫描结果缓存到：

```text
.eva/data/discovery-cache.json
```

缓存内容：

- discovered agents
- registration decisions
- diagnostics
- source path
- source mtime
- source hash
- command path
- command candidates
- health status
- checked_at
- schema version
- Eva-CLI version
- discovery config hash

失效条件：

- manifest 文件 mtime 或 hash 变化。
- discovery 配置变化。
- Eva-CLI 版本变化。
- 用户执行 `eva agent scan --refresh`。
- command path 变化。
- health check 超过 TTL。
- capability registry 变化。
- policy 文件变化。
- cache schema version 不兼容。

缓存只能加速展示，不能替代启动时的 policy 校验。

### 12.1 Cache 写入与锁

cache 写入必须使用原子替换：

```text
write discovery-cache.json.tmp
  -> fsync tmp
  -> rename to discovery-cache.json
```

同一 workspace 只允许一个写 cache 的 discovery 进程。多进程并发 scan 时：

- 获得 lock 的进程可以写 cache。
- 未获得 lock 的进程可以执行 dry-run 或只读 cache。
- lock 超时应返回结构化错误，不应破坏 cache。

### 12.2 Cache 损坏恢复

cache 读取失败时：

- 记录 `CacheCorrupt` diagnostic。
- 将损坏文件移动到带时间戳的 `.bad` 文件，或保留原文件并忽略。
- 重新扫描并写入新 cache。
- 不因为 cache 损坏阻塞核心运行时启动。

cache schema 不兼容时：

- 兼容迁移可自动执行。
- 不兼容时丢弃旧 cache 并重新扫描。
- 不应尝试用旧 cache 直接注册运行态对象。

## 13. 错误处理

Discovery 错误必须结构化。

```rust
pub struct DiscoveryError {
    pub source: DiscoverySource,
    pub kind: DiscoveryErrorKind,
    pub message: String,
    pub retryable: bool,
}
```

```rust
pub enum DiscoveryErrorKind {
    Io,
    InvalidYaml,
    SchemaInvalid,
    PathNotAllowed,
    CommandNotFound,
    CommandTimeout,
    PermissionDenied,
    DuplicateId,
    UnsupportedKind,
    Timeout,
    CacheCorrupt,
    CacheLocked,
    PolicyDenied,
    CapabilityUnknown,
    HealthCheckFailed,
    OutputTooLarge,
}
```

错误处理原则：

- 单个来源失败不影响其他来源扫描。
- 无效 manifest 记录为 invalid item，而不是直接 panic。
- 启动扫描失败只影响对应 Agent 或 Adapter。
- required Agent 缺失时，运行时可以进入 degraded 状态。

## 14. CLI 输出

`eva agent list` 默认输出适合人读的表格：

```text
ID              TYPE        SOURCE                         STATUS
agent-a         lua_agent   config/agents/route-a/agent.yaml enabled
codex-cli       cli_agent   PATH:/usr/local/bin/codex      available
code-review     skill       ~/.codex/skills/code-review    available
github-mcp      mcp_server  config/mcp/github.yaml         invalid-policy
```

`--json` 输出完整结构，供 UI 或自动化调用。

```bash
eva agent list --json
```

`eva agent explain <id>` 输出来源、能力、policy、健康状态和注册原因。

```bash
eva agent explain codex-cli
```

推荐 `explain` 输出结构：

```text
ID: codex-cli
Kind: CliAgent
Source: PATH:/usr/local/bin/codex
Trust: SystemPath
Health: Available
Registered: yes
Target: AdapterRegistry
Capabilities:
  - repo.analyze
  - code.review
Decision:
  action: Registered
  reason: registered
Policy:
  read_workspace: true
  write_workspace: false
Diagnostics: none
```

被拒绝对象示例：

```text
ID: code-review
Kind: WorkflowSkill
Source: ~/.codex/skills/code-review/SKILL.md
Trust: UserLocal
Health: NotChecked
Registered: no
Decision:
  action: Rejected
  reason: capability_missing_schema
Diagnostics:
  - output_schema is missing
  - user local skills are display-only by current policy
```

### 14.1 JSON 输出稳定性

`--json` 输出应面向自动化，字段命名必须稳定。推荐顶层结构：

```json
{
  "schema_version": 1,
  "generated_at": "2026-06-11T00:00:00Z",
  "workspace": "...",
  "items": [],
  "summary": {
    "found": 0,
    "registered": 0,
    "rejected": 0,
    "display_only": 0,
    "shadowed": 0
  }
}
```

人读表格可以随版本优化，JSON 输出必须保持向后兼容或提升 `schema_version`。

### 14.2 Doctor 命令

建议新增：

```bash
eva agent doctor
eva adapter doctor
eva discovery doctor
```

检查项：

- schema 文件是否存在。
- discovery cache 是否可读写。
- PATH 白名单命令是否可 probe。
- 用户目录扫描是否被 policy 限制。
- capability 注册表是否覆盖所有 manifest 中声明的能力。
- policy 是否导致所有 Adapter 都被拒绝。

## 15. 推荐实现模块

```text
src/
  discovery/
    mod.rs
    service.rs
    scanner.rs
    normalizer.rs
    health.rs
    cache.rs
    error.rs
    decision.rs
    diagnostics.rs
    capability.rs
    path.rs
    sources/
      project_agents.rs
      project_adapters.rs
      codex.rs
      omx.rs
      path_commands.rs
      mcp.rs
      hardware_devices.rs
```

关键 trait：

```rust
#[async_trait::async_trait]
pub trait DiscoverySourceScanner: Send + Sync {
    fn source_name(&self) -> &'static str;

    async fn scan(
        &self,
        ctx: DiscoveryContext,
    ) -> Result<Vec<DiscoveredAgent>, DiscoveryError>;
}
```

```rust
pub struct AgentDiscoveryService {
    scanners: Vec<Box<dyn DiscoverySourceScanner>>,
    normalizer: DiscoveryNormalizer,
    health_checker: DiscoveryHealthChecker,
    cache: DiscoveryCache,
}
```

建议模块职责：

```text
service.rs       编排扫描、校验、决策、注册和事件发布
scanner.rs       定义 source scanner trait
normalizer.rs    将不同来源归一为 DiscoveredAgent
decision.rs      生成 RegistrationDecision
diagnostics.rs   统一 warning/error 结构
capability.rs    读取和校验 capability registry
path.rs          路径展开、canonicalize、allowed root 校验
health.rs        受控 health/version probe
cache.rs         cache 读写、锁、迁移和损坏恢复
error.rs         DiscoveryError / DiscoveryErrorKind
```

### 15.1 DiscoveryContext

scanner 不应直接读取全局状态，建议通过 `DiscoveryContext` 获取受控能力：

```rust
pub struct DiscoveryContext {
    pub workspace: PathBuf,
    pub config_dir: PathBuf,
    pub allowed_roots: Vec<PathBuf>,
    pub limits: DiscoveryLimits,
    pub trust_policy: DiscoveryTrustPolicy,
    pub capability_registry: CapabilityRegistry,
}
```

`DiscoveryContext` 不应暴露密钥值或任意 shell 执行能力。

## 16. 与现有架构的关系

Discovery 不替代现有模块，而是给现有模块提供输入。

```text
AgentDiscoveryService
  -> Scheduler.register_agent(lua_agent)
  -> AdapterRegistry.register(adapter)
  -> CapabilityRegistry.register(lua_capability)
  -> EventBus.publish(discovery events)
```

Lua Agent 只能通过已有工具表达意图：

```lua
ctx.tools.invoke_agent({
  capability = "repo.analyze",
  provider = "codex-cli",
  prompt = "分析当前仓库"
})
```

Rust 侧根据 AdapterRegistry 和 policy 决定是否允许调用。

## 17. 最小可行版本

第一阶段只实现：

- 扫描 `config/agents/**/agent.yaml`
- 扫描 `config/adapters/*.yaml`
- 扫描 `config/capabilities/*.yaml` 并只注册通过 schema / policy 的 Lua capability
- 扫描 `PATH` 中的 `codex`、`claude`、`gemini`、`ollama`
- 输出 `eva agent scan --json`
- 写入 discovery cache
- 注册 Lua Agent 到 Scheduler
- 注册 CLI Adapter 到 AdapterRegistry
- 输出 RegistrationDecision
- `eva agent explain <id>`
- cache 原子写入和损坏恢复

暂缓：

- 用户目录下 prompt/skill 自动扫描。
- SkillAdapter 自动注册和 runtime gate 执行。
- HardwareAdapter 的热插拔 watch、设备 claim 和 generation 恢复。
- MCP server 自动 health check。
- UI 展示。
- 远程服务发现。
- 分布式 discovery。
- capability 注册表的完整业务别名体系。
- 用户目录 skill 自动调用。

## 18. 设计校验

该方案满足当前 Eva-CLI 的架构约束：

- Rust 管系统边界：扫描、校验、policy、health check 全部在 Rust。
- Lua 管业务意图：Lua 不直接扫描，不直接执行外部命令。
- Topic 保持系统路由契约：扫描结果通过 `/discovery/**` 事件进入 EventBus，内部 Agent 工作流通过 `/sys/**` 路由。
- Adapter 保持受控能力单元：外部能力必须进入 AdapterRegistry。
- HardwareAdapter 保持硬件能力边界：设备发现不等于授权，授权后仍必须通过 AdapterRegistry 和 policy 调用。
- 配置保持人工可维护：扫描目录、白名单命令和缓存路径都由 YAML 配置。
- 安全边界清晰：发现不等于授权，授权不等于执行，执行必须经过 policy。

### 18.1 验证矩阵

实现时至少覆盖以下验证：

| 类别 | 场景 | 期望 |
| --- | --- | --- |
| Agent manifest | 合法 `agent.yaml` + `main.lua` | 注册到 Scheduler |
| Agent manifest | 重复 Agent ID | 生成 duplicate diagnostic，不 panic |
| Agent manifest | script 跳出 workspace | 拒绝注册，reason 为 `path_not_allowed` |
| Topic | 非法 subscriptions | 拒绝注册，指出非法 Topic |
| Adapter manifest | 合法 stdio Adapter | 注册到 AdapterRegistry |
| Adapter manifest | 缺少 capability | 只展示或拒绝，不能自动路由 |
| Capability | 未知 capability | 默认拒绝自动注册 |
| PATH | 白名单命令存在 | probe version，生成 CliAgent |
| PATH | 同名命令多个路径 | 记录 candidates 和 shadow / ambiguous |
| PATH | version 超时 | health 为 `Unavailable` 或 diagnostic timeout |
| PATH | 输出过大 | 截断并返回 `OutputTooLarge` diagnostic |
| MCP | 显式 manifest | 读取 allowlist，不直接暴露全部 tool |
| MCP | metadata probe 超时 | 不影响其他来源扫描 |
| Skill | 缺少 input/output schema | display-only，不注册 Adapter |
| Skill | runtime_worker | display-only，不注册普通 Adapter |
| Cache | cache 损坏 | 忽略旧 cache，重新扫描 |
| Cache | schema version 不兼容 | 丢弃或迁移，不直接注册旧对象 |
| Hot reload | 新配置无效 | 保留旧 registry |
| Hot reload | 删除 Adapter | 停止新路由并 drain inflight |
| Security | manifest 含 secret 字段 | 日志和 JSON 输出脱敏 |
| CLI | `explain` rejected item | 输出拒绝原因和 policy 依据 |

### 18.2 待补功能清单

后续版本可以继续补：

- 内部 Agent catalog，通过显式签名 manifest 接入。
- 本地模型发现扩展，例如 LM Studio、llama.cpp server、vLLM 和 OpenAI-compatible endpoint。
- capability registry 的机器可读文档和 CLI 查询。
- discovery 结果 UI 页面。
- discovery audit report，列出每个 Agent / Adapter 的权限面。
- 多 workspace discovery cache 隔离。
- 分布式 discovery 与远程 runtime registry 同步。
- Adapter marketplace 或签名插件源，但默认不启用。
