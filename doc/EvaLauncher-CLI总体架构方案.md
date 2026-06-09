# EvaLauncher-CLI 总体架构方案

更新日期：2026-06-09

## 1. 文档定位

本文是 EvaLauncher-CLI 的总体架构入口，整合以下两份子方案：

- `Rust与Lua事件总线智能体调度架构方案.md`：定义 Rust + Lua + EventBus + Topic 的 Agent 调度核心。
- `Lua调用外部Agent动态Adapter架构方案.md`：定义 Lua 调用 Claude、Codex、MCP server、本地模型和企业内部 Agent 的动态 Adapter 扩展层。

总体结论：

- 系统核心是 **Rust 托管的 Topic EventBus Agent 运行时**。
- Agent 业务逻辑由 **Lua 脚本**承载，支持热更新和局部状态。
- Agent 之间通过 **Topic 事件**协作，不共享隐式全局状态。
- Claude、Codex、Gemini、本地模型、MCP server 等外部能力通过 **动态 Adapter** 接入。
- MCP 支持双向集成：内部 Agent 调用外部 MCP server；系统自身作为 MCP server 暴露受控工具。

本文用于统一架构边界、模块关系、运行时流程、数据契约、可靠性、安全策略、配置策略和设计校验口径。具体 Topic 匹配、Adapter 协议和配置格式以对应子方案为准。

## 2. 目标与非目标

### 2.1 目标

- 构建一个可热更新、可扩展、可观测的多 Agent 调度系统。
- 使用 Rust 承担运行时、调度、权限、可靠性和外部能力托管。
- 使用 Lua 承担 Agent 业务逻辑、局部状态转换和轻量编排。
- 使用 Topic EventBus 实现 Agent 间解耦协作。
- 使用 AdapterRegistry 接入外部 Agent、CLI、HTTP API、MCP server 和内部 Agent。
- 支持单进程、多进程、分布式和持久化消息队列等部署形态。

### 2.2 非目标

- 不复刻中心化 LangGraph 状态图。
- 不让 Lua 直接执行 shell 命令、读取密钥或连接外部 provider。
- 不将 Rust 动态库插件作为默认扩展机制。
- 不默认提供强一致分布式事务。
- 不把 EventBus 当作所有业务状态的存储系统。
- 不允许 MCP 成为不受控的通用代理。

## 3. 总体架构

```text
                  External Input / CLI / API / UI
                                |
                                v
                      Event { topic, payload, meta }
                                |
                                v
                           [EventBus]
                                |
                                v
                        [Scheduler - Rust]
                                |
                +---------------+---------------+
                |               |               |
                v               v               v
           [Agent A]       [Agent B]       [Agent N]
           Lua State       Lua State       Lua State
           Inbox Queue     Inbox Queue     Inbox Queue
                |               |               |
                +---------------+---------------+
                                |
                                v
                        [Rust Tool Layer]
                                |
                                v
                       [AdapterRegistry]
                                |
                           [AdapterRouter]
                                |
      +-------------+-------------+-------------+-------------+-------------+
      |             |             |             |             |             |
      v             v             v             v             v             v
 BuiltinAdapter  StdioAdapter  HttpAdapter  EventBusAdapter  McpAdapter  Future
      |             |             |             |             |
      v             v             v             v             v
 Rust tools      Codex CLI     Claude API    Internal Agent   MCP server
                 Claude CLI    Gemini API    Local Agent      tools/resources
```

系统分为三层：

1. **调度层**：EventBus、Scheduler、Topic 路由、Agent 队列。
2. **执行层**：AgentRuntime、Lua State、Lua sandbox、Rust tool bindings。
3. **扩展层**：AdapterRegistry、AdapterRouter、MCP client/server、外部 provider。

## 4. 核心设计原则

### 4.1 Rust 管系统边界

Rust 负责不可妥协的系统能力：

- EventBus 和 Scheduler。
- Agent 生命周期。
- 私有队列、超时、取消、重试和死信。
- Lua State 生命周期和沙箱。
- Adapter 注册、权限、路由和观测。
- MCP 连接、MCP server 暴露和 tool/resource/prompt 校验。
- 状态持久化、审计日志和链路追踪。

### 4.2 Lua 管业务意图

Lua 负责可热更新的业务逻辑：

- 识别事件意图。
- 更新 Agent 局部状态。
- 调用 Rust 白名单工具。
- 发布衍生 Topic 事件。
- 编排局部工作流。

Lua 不直接访问：

- shell。
- 文件系统。
- 网络。
- 环境变量。
- API key。
- MCP server session。

### 4.3 Topic 是系统级路由契约

事件使用路径式 Topic：

```text
/user/input
/task/created
/adapter/invoke
/mcp/tool/called
/funcA/funcAB/funcABCC
```

Scheduler 必须按 `/` 分段匹配，支持：

```text
/funcA/funAC       精确匹配
/funcA/*           单段通配
/funcA/**          多段尾部通配
```

不能用简单字符串前缀替代 Topic matcher。

### 4.4 Adapter 是受控能力单元

Adapter 的定义：

```text
Adapter = manifest + capability + policy + transport + protocol + runtime state
```

Adapter 接入能力包括：

- Claude API。
- Codex CLI。
- Gemini API。
- 本地模型。
- 企业内部 Agent。
- MCP server。
- 系统内部 Agent。

所有 Adapter 都必须经过 Registry、Router 和 policy，不允许 Lua 绕过。

## 5. 核心模块

### 5.1 EventBus

EventBus 是全局事件发布通道。

EventBus 后端可以选择进程内 `broadcast`、Redis Streams、NATS、Kafka、RabbitMQ 或 PostgreSQL LISTEN/NOTIFY。进程内后端适合单进程运行，持久化队列适合跨进程和分布式运行。

职责：

- 接收外部输入和 Agent 发布的事件。
- 广播给 Scheduler、观测器和审计器。
- 不直接执行 Agent 业务逻辑。
- 不直接承担复杂 Topic 匹配。

### 5.2 Scheduler

Scheduler 是事件路由器。

职责：

- 订阅 EventBus。
- 维护 Agent 注册表。
- 维护 Topic 订阅表。
- 根据 `target`、Topic pattern、优先级和负载选择 Agent。
- 将事件投递到 Agent 私有队列。
- 处理 Agent 不在线、队列满和投递失败。
- 将失败事件写入死信队列。

默认路由优先级：

```text
target 直接路由
  -> 精确 Topic 订阅
  -> 通配 Topic 订阅
  -> 规则路由
```

### 5.3 AgentRuntime

每个 Agent 是独立运行单元：

- 独立 Tokio task。
- 独立 Lua State。
- 独立 bounded inbox queue。
- 独立局部状态。
- 独立超时和错误边界。
- 独立脚本版本。

Agent 处理流程：

```text
从 inbox 读取 Event
  -> 检查幂等和状态
  -> 调用 Lua on_event(event, ctx)
  -> Lua 调用 ctx.emit / ctx.tools
  -> Rust 记录 trace、状态、结果
```

### 5.4 Lua Sandbox

Lua 必须运行在受控环境：

- 禁用 `os`、`io`、`debug` 等危险库。
- 禁止直接访问文件、网络和环境变量。
- 所有外部能力通过 Rust 工具白名单暴露。
- 限制执行时间和内存。
- 校验 Lua 返回值和 Lua 发出的 Topic。
- CPU 密集任务下沉到 Rust worker 或独立进程。

### 5.5 Rust Tool Layer

Rust Tool Layer 是 Lua 能力边界：

```text
ctx.emit(topic, payload, options)
ctx.tools.invoke_agent(request)
ctx.tools.http_get(...)
ctx.tools.state_get(...)
ctx.tools.state_set(...)
```

所有工具调用都应：

- 有 schema。
- 有超时。
- 有权限。
- 有 tracing。
- 有结构化错误。

### 5.6 AdapterRegistry 与 AdapterRouter

AdapterRegistry 负责：

- 扫描 `adapters/*.yaml` 或等价 JSON manifest。
- 校验 manifest schema。
- 创建 transport runtime。
- 建立 capability 索引。
- 健康检查。
- 热加载、替换、卸载。

AdapterRouter 负责：

- 根据 provider 精确选择 Adapter。
- 根据 capability 自动选择 Adapter。
- 过滤不健康、超并发、权限不足的 Adapter。
- 根据优先级、负载和错误率评分。
- 返回结构化错误。

### 5.7 MCP 子系统

MCP 子系统包含两个方向：

```text
内部调用外部 MCP:
Lua Agent -> McpAdapter -> MCP server tools/resources/prompts

外部调用内部 Agent:
MCP Client -> EvaLauncher MCP Server -> EventBus / AdapterRegistry / Scheduler
```

MCP 边界：

- MCP tool 必须经过 allowlist。
- MCP resource 必须经过 URI allowlist。
- MCP prompt 必须经过 schema 校验。
- 外部 MCP client 不能任意发布内部 Topic。
- `agent.invoke`、`adapter.invoke` 等 MCP tools 必须有明确权限。

## 6. 关键数据契约

### 6.1 Event

```rust
pub struct Event {
    pub id: String,
    pub topic: String,
    pub source: String,
    pub target: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub priority: EventPriority,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
```

### 6.2 Lua Agent 入口

```lua
function on_event(event, ctx)
  if event.topic == "/user/input" then
    ctx.emit("/agent/reply", {
      text = "..."
    })
  end
end
```

### 6.3 Adapter 调用请求

```rust
pub struct AgentInvokeRequest {
    pub request_id: String,
    pub capability: String,
    pub provider: Option<String>,
    pub prompt: Option<String>,
    pub payload: serde_json::Value,
    pub context: InvokeContext,
    pub timeout_ms: Option<u64>,
    pub stream: bool,
    pub reply_topic: Option<String>,
    pub stream_topic: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}
```

## 7. 典型流程

### 7.1 用户输入到 Lua Agent

```text
CLI/API 发布 /user/input
  -> EventBus
  -> Scheduler 匹配 /user/input
  -> 投递到 PlannerAgent inbox
  -> PlannerAgent Lua on_event
  -> ctx.emit("/agent/reply")
```

### 7.2 Lua 调用 Codex / Claude

```text
Lua ctx.tools.invoke_agent({
  capability = "repo.analyze",
  provider = "codex-cli"
})
  -> Rust Tool Layer
  -> AdapterRegistry
  -> AdapterRouter
  -> StdioAdapter
  -> Codex CLI
  -> AgentInvokeResponse
```

### 7.3 Lua 调用 MCP Tool

```text
Lua ctx.tools.invoke_agent({
  capability = "mcp.tool.call",
  provider = "github-mcp",
  payload.tool = "create_issue"
})
  -> McpAdapter
  -> MCP tools/list schema 校验
  -> MCP tool 调用
  -> 结构化结果返回 Lua
```

### 7.4 外部 MCP Client 调用内部 Agent

```text
MCP Client 调用 agent.invoke
  -> EvaLauncher MCP Server
  -> 权限校验
  -> 转换为 /agent/invoke 或 /adapter/invoke
  -> EventBus / AdapterRegistry
  -> 内部 Agent 或外部 Adapter
  -> MCP tool result
```

### 7.5 Lua 热更新

```text
监听脚本变化
  -> 加载新 Lua State
  -> 执行 init / health_check
  -> 校验 Topic 订阅
  -> 暂停 Agent 接收新事件
  -> 等当前事件完成或超时取消
  -> 替换 Lua State 和订阅版本
  -> 失败则回滚
```

## 8. 状态与一致性

状态分层：

```text
Agent 局部状态
  -> 会话上下文、处理进度、脚本版本、已处理事件

全局业务状态
  -> task、session、user、tool call、audit log

系统运行状态
  -> Agent status、Adapter health、subscription table、dead letter
```

进程内运行形态可使用 best-effort 事件投递；需要恢复、重放和跨进程协作的运行形态应支持：

- 关键事件落库。
- 已处理事件去重。
- Agent 状态版本。
- Adapter request 幂等。
- 外部副作用 idempotency key。
- 死信队列和失败重放。

## 9. 可靠性策略

### 9.1 投递语义

进程内后端：

- EventBus best-effort。
- Agent 私有队列 bounded。
- 队列满、Agent 不在线、目标不存在进入死信队列。

持久化后端：

- 使用持久化消息队列。
- 至少一次投递。
- Agent 侧幂等消费。
- 外部副作用使用 idempotency key。

### 9.2 超时、取消与重试

必须统一处理：

- Agent 事件处理超时。
- Lua 执行超时。
- Adapter 调用超时。
- MCP tool 调用超时。
- 用户取消。
- Agent / Adapter 取消。

重试策略按 Topic 或 capability 配置。默认只自动重试无副作用能力，例如 `repo.analyze`、`code.review`、`chat.reply`。

### 9.3 死信队列

死信记录至少包含：

- 原始事件或 request。
- Topic / capability。
- 目标 Agent / Adapter。
- 命中的 subscription pattern。
- 失败原因。
- 重试次数。
- 最后错误。
- 时间戳。

## 10. 安全模型

### 10.1 Lua 权限

Lua 只能使用 Rust 暴露的白名单能力。禁止：

- 任意 shell。
- 任意文件读写。
- 任意网络连接。
- 任意环境变量访问。
- 直接连接 MCP server。

### 10.2 Adapter 权限

Adapter 权限来自：

```text
系统 policy
  -> adapter manifest
  -> 用户/会话 policy
  -> request 级约束
```

最终权限只能收紧，不能放宽。

### 10.3 MCP 权限

MCP 必须限制：

- 可连接的 MCP server。
- 可调用的 tools。
- 可读取的 resources。
- 可渲染的 prompts。
- 可暴露给外部 MCP client 的内部 tools。
- `topic.emit` 的 Topic pattern。
- `agent.invoke` 的 Agent allowlist。
- `adapter.invoke` 的 capability/provider allowlist。

## 11. 可观测性

所有链路必须带：

- `event_id`
- `request_id`
- `topic`
- `capability`
- `provider`
- `adapter_id`
- `agent_id`
- `correlation_id`
- `causation_id`
- `subscription_pattern`
- `script_version`
- `span_id`
- `latency_ms`
- `error_kind`

需要支持：

- 查询某个 correlation 的完整事件链。
- 查询某个 Agent 的处理历史。
- 查询某个 Topic 的订阅者。
- 查询某个 Adapter 的健康状态。
- 查询某个 capability 的候选 Adapter。
- 查询某个 MCP tool 的调用历史。
- 查询死信事件和重试记录。

## 12. 模块划分

```text
src/
  eventbus/
    event.rs
    memory.rs
    dead_letter.rs

  scheduler/
    registry.rs
    routing.rs
    subscription.rs
    topic.rs

  agent/
    runtime.rs
    state.rs
    lifecycle.rs

  lua/
    loader.rs
    sandbox.rs
    bindings.rs
    hot_reload.rs

  adapter/
    manifest.rs
    registry.rs
    router.rs
    runtime.rs
    policy.rs
    protocol.rs
    transports/
      builtin.rs
      stdio.rs
      http.rs
      eventbus.rs
      mcp.rs

  mcp/
    client.rs
    server.rs
    tool_mapping.rs
    policy.rs
    schema.rs

  tools/
    external_agent.rs
    http.rs
    llm.rs
    state.rs

  observability/
    tracing.rs
    metrics.rs
    audit.rs

  cli/
    run.rs
    emit.rs
    inspect.rs
```

## 13. 能力范围

### 13.1 Topic Agent 调度能力

- 支持进程内或持久化 EventBus 后端。
- 支持 Scheduler 订阅 EventBus 并按 Topic 路由。
- 支持 Topic exact、`*`、`**` 分段匹配。
- 支持 `target` 直接路由优先于 Topic fan-out。
- 支持多个 Agent 独立 Tokio task 和独立 Lua State。
- 支持 Agent 私有 bounded inbox queue。
- 支持 Lua `on_event(event, ctx)` 入口。
- 支持 Lua 通过 `ctx.emit` 发布新 Topic 事件。
- 支持事件处理超时、死信队列和 tracing。

### 13.2 外部能力接入

- 支持 Adapter manifest。
- 支持 AdapterRegistry 和 AdapterRouter。
- 支持 builtin、stdio、http、eventbus、mcp transport。
- 支持 capability 路由和 provider 指定路由。
- 支持 `ctx.tools.invoke_agent` 同步调用。
- 支持 `/adapter/invoke`、`/adapter/completed`、`/adapter/failed`、`/adapter/stream` 异步事件。
- 支持 Adapter 超时、取消、并发限制和结构化错误。
- 禁止 Lua 传入任意 command 或直接读取 provider 密钥。

### 13.3 MCP 双向集成

- 支持内部 Agent 通过 `McpAdapter` 调用外部 MCP server。
- 支持 MCP tool/resource/prompt allowlist。
- 支持 MCP schema 校验。
- 支持系统作为 MCP server 暴露受控工具。
- 支持 `agent.invoke`、`adapter.invoke`、`adapter.list`、`adapter.health` 等 MCP tools。
- 禁止外部 MCP client 调用未授权 Agent、Adapter 或 Topic。

## 14. 设计校验标准

Topic Agent 调度校验：

- CLI 可以发布 `/user/input`。
- Scheduler 可以按 Topic 精确和通配匹配投递。
- 至少两个 Agent 可独立运行。
- 每个 Agent 有独立 Lua State。
- Lua 可以 `ctx.emit` 发布事件。
- Lua 可以调用 Rust async 工具。
- Agent 超时进入死信。
- tracing 能看到 correlation 链路。

Adapter 校验：

- 可以加载 `adapters/*.yaml` 或等价 JSON manifest。
- 可以按 capability 路由 Adapter。
- 可以调用 Codex CLI 或模拟 StdioAdapter。
- 可以调用 HTTP Adapter。
- Adapter 超时、取消、错误可结构化返回。
- Lua 不能传任意 command。

MCP 校验：

- McpAdapter 可以列出 allowlist 内 MCP tools。
- Lua 可以通过 `mcp.tool.call` 调用 MCP tool。
- MCP tool 参数不符合 schema 时被拒绝。
- 系统作为 MCP server 暴露 `agent.invoke`。
- 外部 MCP client 不能调用未授权 Agent、Adapter 或 Topic。

## 15. 风险与规避

| 风险 | 规避 |
| --- | --- |
| Topic 误匹配 | 分段 matcher，禁止 `starts_with` |
| 事件重复消费 | event id 去重，外部副作用 idempotency key |
| Lua 阻塞 | 独立 Lua State，事件处理 timeout，CPU 任务下沉 |
| Adapter 越权 | manifest + policy + request 级约束 |
| MCP 变成通用代理 | tool/resource/prompt allowlist，MCP server tool policy |
| 隐式流程难调试 | correlation_id、causation_id、trace、audit |
| 状态分裂 | Agent 局部状态和全局业务状态显式建模 |
| 热更新破坏订阅 | 脚本版本和订阅版本一起校验、替换、回滚 |

## 16. 文档索引

- Topic 调度细节：`Rust与Lua事件总线智能体调度架构方案.md`
- 外部 Agent 和 Adapter 细节：`Lua调用外部Agent动态Adapter架构方案.md`
- 项目配置方案：`EvaLauncher-CLI项目配置方案.md`
- MCP 官方规范参考：`https://modelcontextprotocol.io/specification/`

## 17. 总结

EvaLauncher-CLI 的总体架构应落在 **Rust 托管运行时 + Lua 热更新 Agent + Topic EventBus + 动态 Adapter + MCP 双向集成**。

Rust 负责边界和可靠性，Lua 负责业务意图，Topic 负责 Agent 间协作，Adapter 负责外部能力接入，MCP 负责与工具生态互通。这样既能保持 Agent 业务逻辑灵活，又能避免 Lua 越权、外部 provider 失控和事件链路不可观测。
