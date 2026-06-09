# Agent 扫描与发现架构方案

更新日期：2026-06-09

文档关系：

- 总体入口：`EvaLauncher-CLI总体架构方案.md`
- 配置体系：`EvaLauncher-CLI项目配置方案.md`
- Agent 调度核心：`Rust与Lua事件总线智能体调度架构方案.md`
- 外部 Agent 扩展：`Lua调用外部Agent动态Adapter架构方案.md`

## 1. 方案定位

本文定义 EvaLauncher-CLI 如何扫描、识别、校验并注册用户电脑上可用的 Agent 能力。

核心结论：

- Agent 扫描由 **Rust Runtime** 负责，不能由 Lua 直接访问文件系统、环境变量或 shell。
- 扫描结果分为 **内部 Lua Agent** 和 **外部 Agent Adapter** 两类。
- 内部 Lua Agent 注册到 Scheduler，参与 Topic EventBus 调度。
- 外部 Agent、CLI、MCP server、本地模型和工作流能力注册到 AdapterRegistry。
- 扫描只做发现和归一化，不代表自动授权调用；调用前仍必须经过 policy 校验。
- 默认只扫描白名单目录、显式配置目录和白名单命令，不做全盘扫描。

该方案的目标不是寻找电脑上的所有可执行文件，而是建立一套可控、可观测、可缓存、可热加载的 Agent Discovery 机制。

## 2. 目标与非目标

### 2.1 目标

- 自动发现项目内配置的 Lua Agent。
- 自动发现用户环境中可用的外部 Agent 能力，例如 Codex、Claude、Gemini、Ollama、MCP server 和 OMX skills。
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

## 3. 总体架构

```text
EvaLauncher Startup / CLI scan command / Hot reload
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
~/.config/evalauncher/mcp/*.yaml
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

## 5. 数据模型

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

## 6. 扫描流程

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

### 6.3 热加载扫描

监听以下目录变化：

```text
config/agents/
config/adapters/
config/mcp/
```

用户目录下的 `~/.codex` 和 `~/.agents` 默认不做高频文件监听，只在启动或手动 scan 时刷新，避免额外开销和隐私争议。

## 7. 配置设计

在 `config/eva.yaml` 中新增 discovery 配置：

```yaml
discovery:
  enabled: true
  scan_on_startup: true
  cache_path: .evalauncher/data/discovery-cache.json
  health_check: true
  watch_project_dirs: true
  scan_user_dirs: true
  scan_path_commands: true

  project:
    agent_dir: config/agents
    agent_manifest_glob: "**/agent.yaml"
    adapter_dir: config/adapters
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
    command_check_timeout_ms: 2000
```

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

### 9.2 注册到 AdapterRegistry

满足以下条件的对象注册到 AdapterRegistry：

- `kind` 属于 `CliAgent`、`HttpAgent`、`McpServer`、`LocalModel` 或 `WorkflowSkill`
- manifest 或内置 adapter template 有效
- capability 非空
- policy 允许注册
- health 不是 `InvalidManifest`

注册结果：

```text
AdapterId -> AgentAdapter
Capability -> AdapterId candidates
```

### 9.3 不自动注册的对象

以下对象只展示，不自动注册：

- trust level 为 `Unknown`
- 没有 capability 的 prompt 文件
- team/swarm 专用 worker
- 缺少 manifest 的远程 HTTP 服务
- health 为 `PermissionDenied` 或 `InvalidManifest`

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

## 12. 缓存与失效

扫描结果缓存到：

```text
.evalauncher/data/discovery-cache.json
```

缓存内容：

- discovered agents
- source path
- source mtime
- source hash
- command path
- health status
- checked_at
- schema version

失效条件：

- manifest 文件 mtime 或 hash 变化。
- discovery 配置变化。
- EvaLauncher 版本变化。
- 用户执行 `eva agent scan --refresh`。
- command path 变化。
- health check 超过 TTL。

缓存只能加速展示，不能替代启动时的 policy 校验。

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
    sources/
      project_agents.rs
      project_adapters.rs
      codex.rs
      omx.rs
      path_commands.rs
      mcp.rs
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

## 16. 与现有架构的关系

Discovery 不替代现有模块，而是给现有模块提供输入。

```text
AgentDiscoveryService
  -> Scheduler.register_agent(lua_agent)
  -> AdapterRegistry.register(adapter)
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
- 扫描 `PATH` 中的 `codex`、`claude`、`gemini`、`ollama`
- 输出 `eva agent scan --json`
- 写入 discovery cache
- 注册 Lua Agent 到 Scheduler
- 注册 CLI Adapter 到 AdapterRegistry

暂缓：

- 用户目录下 prompt/skill 自动扫描。
- MCP server 自动 health check。
- UI 展示。
- 远程服务发现。
- 分布式 discovery。

## 18. 设计校验

该方案满足当前 EvaLauncher-CLI 的架构约束：

- Rust 管系统边界：扫描、校验、policy、health check 全部在 Rust。
- Lua 管业务意图：Lua 不直接扫描，不直接执行外部命令。
- Topic 保持系统路由契约：扫描结果通过 `/discovery/**` 事件进入 EventBus，内部 Agent 工作流通过 `/sys/**` 路由。
- Adapter 保持受控能力单元：外部能力必须进入 AdapterRegistry。
- 配置保持人工可维护：扫描目录、白名单命令和缓存路径都由 YAML 配置。
- 安全边界清晰：发现不等于授权，授权不等于执行，执行必须经过 policy。
