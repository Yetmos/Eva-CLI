# EvaLauncher-CLI 项目配置方案

更新日期：2026-06-09

## 1. 方案定位

本文定义 EvaLauncher-CLI 的项目配置体系，回答以下问题：

- 哪些内容应该进入配置文件。
- 配置文件使用 JSON 还是 YAML。
- 主配置、Agent 配置、Adapter manifest、MCP policy 如何拆分。
- 配置如何校验、合并、热加载和覆盖。

结论：

- **配置文件推荐使用 YAML**，因为 Agent、Topic、权限、路由和 Adapter 配置需要人工维护，YAML 可读性更好。
- **运行时事件和协议消息继续使用 JSON**，例如 `Event`、`AgentInvokeRequest`、MCP JSON-RPC payload。
- **配置必须有 schema 校验**。推荐使用 JSON Schema 描述配置结构，即使配置文件本身是 YAML。
- 第一版不要拆太碎，先使用 `eva.yaml`、`agents/*.yaml`、`adapters/*.yaml`、`policies/*.yaml`。
- 如果后续提供 JSON 等价主配置，文件名使用 `eva.json`，不使用 `evalauncher.json`。

## 2. YAML 与 JSON 的取舍

### 2.1 配置使用 YAML

YAML 适合：

- 人工编辑。
- 分层配置。
- 多条 Topic pattern。
- Agent 权限。
- Adapter manifest。
- MCP allowlist。
- 超时、并发、重试策略。

示例：

```yaml
scheduler:
  target_overrides_topic: true
  default_delivery: fanout
```

### 2.2 协议消息使用 JSON

JSON 适合：

- EventBus 事件。
- Adapter 请求和响应。
- MCP JSON-RPC 消息。
- 审计日志结构化输出。
- 程序生成和机器交换。

示例：

```json
{
  "topic": "/adapter/invoke",
  "payload": {
    "capability": "repo.analyze"
  }
}
```

### 2.3 Schema 使用 JSON Schema

推荐做法：

```text
YAML 配置文件
  -> 解析为 serde Value
  -> 使用 JSON Schema 校验
  -> 反序列化为强类型 Rust Config
```

这样可以兼顾 YAML 可读性和 JSON Schema 生态。

## 3. 推荐目录结构

```text
config/
  eva.yaml

  agents/
    planner.yaml
    echo.yaml
    reviewer.yaml

  adapters/
    codex-cli.yaml
    claude-api.yaml
    github-mcp.yaml

  policies/
    sandbox.yaml
    adapter.yaml
    mcp.yaml

  routes/
    topics.yaml

  schemas/
    eva.schema.json
    agent.schema.json
    adapter.schema.json
    policy.schema.json
    routes.schema.json
```

MVP 可以简化为：

```text
config/
  eva.yaml
  agents/
    echo.yaml
  adapters/
    codex-cli.yaml
```

## 4. 主配置

`config/eva.yaml` 负责全局运行时配置。

```yaml
runtime:
  env: dev
  workspace: C:/Users/admin/Desktop/project/EvaLauncher-CLI
  data_dir: .evalauncher/data
  script_dir: agents
  adapter_dir: config/adapters
  hot_reload: true

eventbus:
  backend: memory
  broadcast_capacity: 4096
  dead_letter:
    enabled: true
    backend: memory
    retention_days: 7

scheduler:
  target_overrides_topic: true
  default_delivery: fanout
  max_route_targets: 32

state:
  backend: sqlite
  sqlite_path: .evalauncher/data/state.db

observability:
  log_level: info
  tracing: true
  metrics: true
  audit: true
  otel_endpoint_env: OTEL_EXPORTER_OTLP_ENDPOINT

config:
  agent_dir: config/agents
  adapter_dir: config/adapters
  policy_dir: config/policies
  route_file: config/routes/topics.yaml
```

应放在主配置中的内容：

- 运行环境。
- workspace。
- 数据目录。
- EventBus backend。
- Scheduler 默认策略。
- 状态存储 backend。
- observability。
- 配置目录位置。

不应放在主配置中的内容：

- 单个 Agent 的脚本路径和订阅。
- 单个 Adapter 的 command、endpoint、权限。
- MCP tool allowlist。
- 大量 Topic route 明细。

## 5. Agent 配置

每个 Agent 一个配置文件。

`config/agents/planner.yaml`：

```yaml
id: planner
enabled: true
script: agents/planner.lua
script_version: 2026-06-09.1

subscriptions:
  - /user/input
  - /task/**
  - /adapter/completed
  - /adapter/failed

inbox:
  capacity: 256
  overflow: dead_letter

timeout:
  event_ms: 30000
  tool_ms: 60000

state:
  backend: sqlite
  namespace: agent.planner

permissions:
  emit:
    - /agent/**
    - /task/**
    - /adapter/invoke
  tools:
    - invoke_agent
    - state_get
    - state_set
  adapters:
    capabilities:
      - chat.reply
      - task.plan
      - repo.analyze
```

Agent 配置应包含：

- `id`
- `enabled`
- Lua 脚本路径。
- Topic 订阅。
- inbox 容量和溢出策略。
- 事件处理超时。
- 状态命名空间。
- 可发布 Topic。
- 可调用工具。
- 可调用 Adapter capability。

Agent 配置不应包含：

- API key。
- 外部命令。
- 真实 provider 密钥。
- 任意文件系统权限。

## 6. Topic 路由配置

`config/routes/topics.yaml`：

```yaml
routes:
  - pattern: /user/input
    delivery: fanout
    agents:
      - planner

  - pattern: /funcA/**
    delivery: fanout
    agents:
      - func-router

  - pattern: /adapter/completed
    delivery: fanout
    agents:
      - planner

  - pattern: /adapter/failed
    delivery: fanout
    agents:
      - planner
```

字段说明：

- `pattern`：Topic pattern，支持 exact、`*`、`**`。
- `delivery`：`fanout` 或后续扩展的 `compete`。
- `agents`：目标 Agent 列表。

约束：

- `**` 只能出现在最后一段。
- 不允许空段 Topic。
- `target` 直接路由仍优先于 routes。
- Topic route 只配置路由，不配置业务逻辑。

## 7. Lua 沙箱配置

`config/policies/sandbox.yaml`：

```yaml
lua_sandbox:
  disabled_libs:
    - os
    - io
    - debug

  limits:
    memory_mb: 64
    execution_timeout_ms: 30000

  filesystem:
    enabled: false

  network:
    enabled: false

  environment:
    enabled: false

  return_schema_validation: true
  emitted_topic_validation: true
```

沙箱配置是全局下限。Agent 配置只能在此基础上收紧，不能放宽。

## 8. Adapter Manifest 配置

Adapter manifest 推荐使用 YAML 文件，但字段保持与文档中的 JSON 示例一致。

`config/adapters/codex-cli.yaml`：

```yaml
id: codex-cli
name: Codex CLI Adapter
version: 1.0.0
enabled: true
transport: stdio

command: codex
args:
  - exec
  - --json

capabilities:
  - repo.analyze
  - code.review
  - code.generate

permissions:
  read_workspace: true
  write_workspace: true
  network: false
  shell: false
  env: []

limits:
  timeout_ms: 300000
  max_concurrency: 1
  max_prompt_bytes: 200000

routing:
  priority: 100
  default_for:
    - repo.analyze
    - code.review
```

`config/adapters/claude-api.yaml`：

```yaml
id: claude-api
name: Claude API Adapter
version: 1.0.0
enabled: true
transport: http

endpoint: https://api.anthropic.com/v1/messages

capabilities:
  - chat.reply
  - task.plan
  - code.review

permissions:
  read_workspace: false
  write_workspace: false
  network: true
  shell: false
  env:
    - ANTHROPIC_API_KEY

limits:
  timeout_ms: 120000
  max_concurrency: 4
  max_prompt_bytes: 100000

routing:
  priority: 80
  default_for:
    - chat.reply
    - task.plan
```

Adapter manifest 应包含：

- `id`
- `name`
- `version`
- `enabled`
- `transport`
- transport 参数。
- capabilities。
- permissions。
- limits。
- routing。

Adapter manifest 不应包含：

- API key 明文。
- 用户 token 明文。
- Lua 可覆盖的 command。
- 不受控 shell 片段。

## 9. MCP 配置

MCP Adapter 同样放在 `config/adapters`。

`config/adapters/github-mcp.yaml`：

```yaml
id: github-mcp
name: GitHub MCP Adapter
version: 1.0.0
enabled: true
transport: mcp

mcp:
  server_transport: stdio
  command: github-mcp-server
  args: []
  tool_allowlist:
    - create_issue
    - list_issues
    - get_pull_request
  resource_allowlist: []
  prompt_allowlist: []

capabilities:
  - mcp.tool.call
  - github.issue.create
  - github.issue.list
  - github.pr.get

permissions:
  read_workspace: false
  write_workspace: false
  network: true
  shell: false
  env:
    - GITHUB_TOKEN

limits:
  timeout_ms: 60000
  max_concurrency: 2
  max_prompt_bytes: 20000

routing:
  priority: 70
  default_for:
    - github.issue.create
    - github.issue.list
```

MCP 配置必须包含：

- MCP server 启动方式。
- tool allowlist。
- resource allowlist。
- prompt allowlist。
- env 白名单。
- capability 映射。

不允许：

- Lua 直接指定 MCP server command。
- 外部 MCP client 任意发布内部 Topic。
- `topic.emit` 无限制开放。

## 10. MCP Server 对外暴露配置

`config/policies/mcp.yaml`：

```yaml
mcp_server:
  enabled: true
  bind: 127.0.0.1:8765

  tools:
    agent.invoke:
      enabled: true
      allowed_agents:
        - planner
        - reviewer
      timeout_ms: 120000

    adapter.invoke:
      enabled: true
      allowed_capabilities:
        - repo.analyze
        - code.review
        - chat.reply
      allowed_providers:
        - codex-cli
        - claude-api
      timeout_ms: 120000

    adapter.list:
      enabled: true

    adapter.health:
      enabled: true

    topic.emit:
      enabled: false
      allowed_topics:
        - /user/input
        - /task/**
```

原则：

- 默认关闭高风险工具。
- `topic.emit` 默认关闭。
- 每个 MCP tool 必须有独立 policy。
- 外部 MCP client 的能力不能超过本地 policy。

## 11. Adapter 与 Agent 权限策略

`config/policies/adapter.yaml`：

```yaml
adapter_policy:
  defaults:
    network: false
    shell: false
    read_workspace: false
    write_workspace: false
    max_timeout_ms: 120000

  allow_write_workspace:
    - codex-cli

  deny_capabilities:
    - shell.execute
    - deployment.run

  retry:
    default:
      max_attempts: 0
    capabilities:
      repo.analyze:
        max_attempts: 2
        backoff_ms: 1000
      code.review:
        max_attempts: 1
        backoff_ms: 1000
```

权限合并顺序：

```text
系统默认 policy
  -> Adapter manifest
  -> Agent permissions
  -> 用户/会话 policy
  -> request 级约束
```

最终权限只能收紧，不能放宽。

## 12. 配置加载与覆盖顺序

推荐加载顺序：

```text
默认内置配置
  -> config/eva.yaml
  -> config/policies/*.yaml
  -> config/routes/topics.yaml
  -> config/agents/*.yaml
  -> config/adapters/*.yaml
  -> 环境变量引用解析
  -> CLI 参数覆盖
```

CLI 参数只能覆盖低风险运行参数，例如：

```text
--config
--log-level
--env
--dry-run
--no-hot-reload
```

不建议 CLI 直接覆盖：

- Adapter command。
- API key 名称。
- workspace 写权限。
- MCP tool allowlist。
- Agent emit allowlist。

## 13. 配置校验

配置启动时必须校验：

- YAML 语法。
- schema。
- Topic pattern。
- Agent ID 唯一性。
- Adapter ID 唯一性。
- capability 格式。
- env 白名单引用。
- 文件路径是否在 workspace 或允许目录内。
- MCP tool/resource/prompt allowlist 是否为空或过宽。
- 超时和并发是否超过全局 policy。

校验失败应阻止启动，除非明确运行在 `--dry-run` 或 `inspect` 模式。

推荐 CLI：

```text
evalauncher config validate
evalauncher config inspect
evalauncher config dump-effective
```

## 14. 热加载策略

可热加载：

- Agent subscriptions。
- Agent Lua script。
- Adapter enabled。
- Adapter routing priority。
- Topic routes。
- 部分 timeout / concurrency。

不可无缝热加载，需重建 runtime：

- Adapter transport。
- Adapter command。
- Adapter endpoint。
- MCP server command。
- 权限边界变更。
- 状态 backend。
- EventBus backend。

热加载流程：

```text
监听配置文件变化
  -> 解析新配置
  -> schema 校验
  -> policy 校验
  -> diff effective config
  -> 对可热加载项应用
  -> 对需重建项执行 draining / restart
  -> 失败则保留旧配置
```

## 15. 配置安全

禁止在配置中写明文密钥：

```yaml
# 不允许
api_key: sk-xxx
```

推荐只写环境变量名：

```yaml
permissions:
  env:
    - ANTHROPIC_API_KEY
```

Rust AdapterRuntime 根据 env allowlist 注入环境变量。Lua 不可读取。

配置文件安全要求：

- 配置不能包含 token 明文。
- 配置不能包含任意 shell 片段。
- command 和 args 必须分开。
- 所有路径必须规范化。
- 所有相对路径必须基于 workspace 或 config_dir。
- 写权限必须显式声明。

## 16. MVP 最小配置

第一版建议只实现以下配置：

```text
config/eva.yaml
config/agents/echo.yaml
config/adapters/codex-cli.yaml
```

`config/eva.yaml`：

```yaml
runtime:
  env: dev
  workspace: .
  hot_reload: true

eventbus:
  backend: memory
  broadcast_capacity: 1024

scheduler:
  target_overrides_topic: true
  default_delivery: fanout

observability:
  log_level: info
  tracing: true
```

`config/agents/echo.yaml`：

```yaml
id: echo
enabled: true
script: agents/echo.lua
subscriptions:
  - /user/input
inbox:
  capacity: 128
timeout:
  event_ms: 10000
permissions:
  emit:
    - /agent/reply
  tools: []
```

`config/adapters/codex-cli.yaml` 可以后续在 Adapter 阶段再启用。

## 17. 与现有文档的关系

- 总体架构：`EvaLauncher-CLI总体架构方案.md`
- Topic 调度：`Rust与Lua事件总线智能体调度架构方案.md`
- 动态 Adapter 与 MCP：`Lua调用外部Agent动态Adapter架构方案.md`

现有文档中的 `adapter.json` 表述可理解为 Adapter manifest 的结构示例。项目落地时建议使用 `adapters/*.yaml` 作为默认配置格式，同时保留 JSON 解析能力，便于机器生成。

## 18. 总结

项目配置应使用 **YAML 作为人工维护配置格式，JSON 作为运行时协议格式，JSON Schema 作为统一校验格式**。

第一版先实现少量配置文件，保证 EventBus、Scheduler、Agent 和基础 Adapter 可启动；后续再拆分 policy、routes、MCP server 和更细的权限配置。关键原则是：配置可读、权限可控、schema 可校验、热加载可回滚。
