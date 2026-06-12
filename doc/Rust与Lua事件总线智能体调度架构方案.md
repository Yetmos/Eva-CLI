# Rust + Lua + EventBus Topic Agent 调度架构方案

更新日期：2026-06-12

文档关系：

- 总体入口：`总体架构方案.md`
- 外部 Agent、动态 Adapter 与 MCP 子方案：`Lua调用外部Agent动态Adapter架构方案.md`
- Lua 承载 Tool / Skill / MCP handler 热更新子方案：`Lua承载Skill-MCP-Tool热更新架构方案.md`

## 1. 方案定位

本方案基于 `方案.txt` 重新整理，目标不是实现一个中心化的 LangGraph 克隆，而是实现一套 **Topic 驱动的 EventBus 多 Agent 调度系统**。

核心思路：

- Rust 作为高性能异步运行时，负责 EventBus、Scheduler、Agent 生命周期、并发隔离、超时、重试和观测。
- Lua 作为 Agent 的脚本化业务逻辑，负责意图识别、工具调用编排、局部状态转换和热更新。
- EventBus 作为核心通信机制，负责外部系统、Scheduler、Agent 之间的事件传递。
- EventBus 支持纯进程内 best-effort、可恢复进程内 EventBus 和外部持久化消息队列三类运行形态。
- Topic 是事件的路由地址，例如 `/input/user`、`/task/created`、`/sys/route-a/route-aa`。
- Scheduler 订阅 EventBus，根据 `topic`、目标 Agent、订阅规则和负载情况，把事件路由到对应 Agent 的私有队列。
- 每个 Agent 拥有独立 Lua State 和私有消息队列，运行在独立 Tokio task 中。

该架构更适合：

- 高解耦、多 Agent 协作。
- 事件驱动工作流。
- 需要热更新脚本逻辑的 Agent 系统。
- Agent 数量、能力、订阅关系会动态变化的场景。
- 需要类似路径的层级 Topic 订阅和通配匹配的场景。
- 不要求所有流程都由一个中心化有向图显式定义的系统。

## 2. 与 LangGraph 思路的区别

LangGraph 的核心是显式图：节点、边、条件边、共享状态、checkpoint 和中断恢复。

本方案的核心是事件调度：Agent 通过 Topic 事件协作，每个 Agent 维护局部状态，Scheduler 负责 Topic 匹配和路由，EventBus 负责消息传播。

| 维度 | LangGraph 风格 | 本方案 |
| --- | --- | --- |
| 控制流 | 显式图定义节点和边 | 事件驱动，Agent 订阅和发布 Topic |
| 路由方式 | 条件边、节点返回下一步 | Scheduler 根据 `target`、`topic` 和订阅规则投递 |
| 状态管理 | 中心化共享 State | Agent 局部状态为主，全局状态需额外存储 |
| 扩展方式 | 新增节点并修改图定义 | 新增 Agent 或 Topic 订阅规则即可接入 |
| 热更新 | 通常更新图或节点实现 | 替换 Lua 脚本或 Agent 实例 |
| 调试方式 | 图执行轨迹和 checkpoint | 事件 trace、Topic 链路、Agent 日志、死信队列 |
| 适用场景 | 长流程、确定性强、可图形化表达 | 高解耦、动态协作、异步事件系统 |

两者不冲突。必要时，可以在单个 Agent 内部使用小型图流程处理局部任务，但系统级编排仍由 EventBus + Scheduler + Topic 完成。

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
     +--------+--------+--------+
     |        |        |        |
     v        v        v        v
 [Agent A] [Agent B] [Agent C] [Agent N]
 LuaState  LuaState  LuaState  LuaState
 Queue     Queue     Queue     Queue
     |        |        |        |
     +--------+--------+--------+
              |
              v
       Rust async tools
  HTTP / DB / File / LLM / System APIs
```

事件流：

1. 外部系统或 Agent 向 EventBus 发布事件，例如 `/input/user`、`/task/created`、`/tool/completed`。
2. Scheduler 订阅 EventBus。
3. Scheduler 根据 `target`、`topic`、订阅表和路由规则选择 Agent。
4. Scheduler 将事件投递到目标 Agent 的私有 `mpsc` 队列。
5. Agent task 从私有队列读取事件。
6. Agent 调用 Lua `on_event(event, ctx)`。
7. Lua 逻辑可以调用 Rust 暴露的异步工具函数。
8. Agent 处理完成后可以发布新 Topic 事件回 EventBus。

## 4. 核心组件职责

### 4.1 Rust Runtime

Rust Runtime 负责强约束和系统级能力：

- Tokio 异步运行时。
- EventBus 实现。
- Scheduler Topic 路由。
- Agent 注册、启动、停止、重启。
- Agent 私有队列管理。
- Lua State 生命周期管理。
- Rust async 工具函数注册。
- 超时、重试、取消、死信队列。
- tracing、metrics、审计日志。
- 状态持久化和恢复。
- Lua 沙箱限制。

### 4.2 EventBus

EventBus 是全局消息通道，不直接执行 Agent 逻辑，也不承担复杂 Topic 匹配。

进程内运行形态可以使用：

- `tokio::sync::broadcast`：适合把所有事件广播给 Scheduler、观测器、审计器等系统订阅者。
- `tokio::sync::mpsc`：适合 Scheduler 到 Agent 的点对点私有队列。
- Durable Event Log / WAL / Spool：适合在保留进程内低延迟分发的同时支持崩溃恢复和重放。

推荐组合：

- 全局 EventBus 使用 `broadcast` 发布 `Event`。
- Scheduler 负责 Topic 订阅匹配。
- Scheduler 到 Agent 使用每个 Agent 独立的 bounded `mpsc` 私有队列。
- 死信队列使用单独 `mpsc` 或持久化表。

纯进程内投递语义：

- 纯进程内 EventBus 是 best-effort，不承诺进程崩溃后恢复。
- Scheduler 到 Agent 的私有队列建议 bounded，避免单个 Agent 拖垮进程。
- Agent 队列满、Agent 不在线、目标不存在时，Scheduler 应写入死信队列并记录原因。
- 从未持久化、也未确认给调用方的纯内存事件在崩溃后不可恢复。

可恢复进程内投递语义：

- 事件返回 `accepted` 前必须先写 Durable Event Log / WAL / Spool。
- 内存 EventBus 负责低延迟分发。
- Runtime 崩溃或系统重启后根据 consumer watermark 重放未 ack 事件。
- 该形态提供至少一次投递语义，Agent 必须幂等消费。
- 外部副作用必须使用 idempotency key。
- 如需跨机器恢复，应切换到外部持久化消息队列。

跨进程或分布式运行形态可以替换为：

- Redis Streams
- NATS
- Kafka
- RabbitMQ
- PostgreSQL LISTEN/NOTIFY

### 4.3 Scheduler

Scheduler 是事件路由器，但不是 LangGraph 式图执行器。

职责：

- 订阅 EventBus。
- 维护 Agent 注册表。
- 维护 Topic 订阅规则。
- 根据 `target`、`topic`、标签、优先级、负载选择目标 Agent。
- 将事件投递到 Agent 私有队列。
- 处理投递失败、队列满、Agent 不在线。
- 将失败事件写入死信队列。

不建议 Scheduler 做：

- 直接执行业务逻辑。
- 直接调用 Lua 函数。
- 保存复杂业务状态。
- 依赖隐式全局变量决定路由。
- 用字符串前缀替代分段 Topic 匹配。

### 4.4 Agent 容器

每个 Agent 是一个独立运行单元：

- 独立 Tokio task。
- 独立 Lua State。
- 独立私有消息队列。
- 独立局部状态。
- 独立超时和错误边界。

推荐结构：

```rust
pub struct AgentHandle {
    pub id: AgentId,
    pub sender: mpsc::Sender<Event>,
    pub status: AgentStatus,
}

pub struct AgentRuntime {
    pub id: AgentId,
    pub lua: Lua,
    pub inbox: mpsc::Receiver<Event>,
    pub eventbus: EventBusHandle,
    pub state_store: AgentStateStore,
}
```

### 4.5 Lua 脚本

Lua 脚本负责 Agent 业务逻辑：

- 判断是否响应事件。
- 解析用户输入或任务事件。
- 调用 Rust 工具函数。
- 更新 Agent 局部状态。
- 发布衍生事件。

建议约定每个 Agent 暴露统一入口：

```lua
function on_event(event, ctx)
  if event.topic == "/input/user" then
    local result = ctx.tools.llm_chat(event.payload.text)
    ctx.emit("/agent/reply", {
      agent_id = ctx.agent_id,
      text = result
    })
  end
end
```

推荐 `ctx.emit` 语义：

```lua
ctx.emit(topic, payload, options)
```

- `topic`：新事件的路由地址。
- `payload`：业务数据。
- `options.target`：可选目标 Agent。
- `options.priority`：可选优先级。
- `options.correlation_id`：默认继承当前事件。
- `options.causation_id`：默认使用当前事件 ID。

## 5. 事件与 Topic 模型

### 5.1 Event 结构

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

字段说明：

- `id`：事件唯一 ID，用于去重、重试、死信定位。
- `topic`：事件路由地址，例如 `/input/user`、`/task/completed`、`/sys/route-a/route-aa`。
- `source`：事件来源，例如 `cli`、`agent:planner`、`tool:http`。
- `target`：可选目标 Agent ID。存在时优先走直接目标路由。
- `correlation_id`：同一业务链路的关联 ID。
- `causation_id`：触发当前事件的上游事件 ID。
- `priority`：调度优先级。
- `payload`：业务数据，不参与通用路由语义。
- `created_at`：事件创建时间，用于排查、超时和审计。

这样定义的原因：

- `topic` 负责“发给谁”和“属于哪类事件”。
- `payload` 只放业务参数，避免 Scheduler 依赖业务结构。
- `target` 支持强制点对点投递，适合回复某个 Agent 或恢复指定任务。
- `correlation_id` 和 `causation_id` 支持完整链路追踪。
- `id` 支持幂等、重试和死信队列。

### 5.2 Topic 命名规则

Topic 使用类路径格式：

```text
/domain/action
/domain/resource/action
/sys/route-a/route-aa
```

推荐规则：

- 必须以 `/` 开头。
- `/` 只作为分段符，不代表文件系统路径。
- 不建议以 `/` 结尾，根 Topic `/` 除外。
- 不允许出现空段，例如 `/sys//route-a`。
- Segment 建议只包含字母、数字、`-`、`_`、`.`。
- `/sys` 作为内部 Agent 层级路由根，例如 `/sys`、`/sys/route-a`、`/sys/route-a/route-aa`。
- `/sys` 不代表 EventBus 组件本身，只表示系统内部 Agent 路由入口。
- 运行时控制、生命周期和观测 Topic 可继续使用独立命名空间，例如 `/runtime/started`、`/agent/failed`、`/eventbus/dead-letter`。
- 业务任务 Topic 可以使用业务域命名，例如 `/task/created`、`/task/completed`。

推荐基础 Topic：

```text
/runtime/started
/runtime/shutdown

/sys
/sys/route-a
/sys/route-a/route-aa

/agent/registered
/agent/started
/agent/stopped
/agent/failed

/input/user
/input/cancelled

/task/created
/task/assigned
/task/completed
/task/failed

/tool/started
/tool/completed
/tool/failed

/llm/token
/llm/completed
/llm/failed
```

### 5.3 Topic 订阅模式

Scheduler 应按 `/` 分段匹配 Topic，不要使用简单 `starts_with`。

建议支持三种模式：

```rust
pub enum TopicPattern {
    Exact(String),
    Segment(Vec<TopicSegment>),
}

pub enum TopicSegment {
    Literal(String),
    AnyOne,  // *
    AnyTail, // **，只能出现在最后一段
}
```

匹配规则：

| 订阅模式 | 匹配 Topic | 不匹配 Topic | 说明 |
| --- | --- | --- | --- |
| `/sys/route-a` | `/sys/route-a` | `/sys/route-a/x` | 精确匹配 |
| `/sys/*` | `/sys/route-a` | `/sys/route-a/route-aa` | `*` 匹配一段 |
| `/sys/route-a/*` | `/sys/route-a/route-aa` | `/sys/route-b` | 固定前两段，第三段任意 |
| `/sys/**` | `/sys/route-a`、`/sys/route-a/route-aa` | `/task/created` | `**` 匹配剩余任意层级 |
| `/**` | 所有合法 Topic | 无 | 全局订阅，适合审计和 tracing |

示例：

```text
事件 topic = /sys/route-a/route-aa

命中:
- /sys/**
- /sys/route-a/*
- /sys/route-a/route-aa

不命中:
- /sys/*
- /sys/route-a
- /task/**
```

### 5.4 路由优先级

Scheduler 推荐按以下顺序处理：

1. 直接目标路由：事件带 `target = agent_id`，直接投递给目标 Agent。
2. 精确 Topic 订阅：例如 `/sys/route-a`。
3. 通配 Topic 订阅：例如 `/sys/*`、`/sys/**`。
4. 规则路由：根据 payload、标签、优先级或负载选择 Agent。

默认语义：

- `target` 存在时，只投递给目标 Agent，不再做普通 Topic fan-out。
- `target` 不存在时，按照 Topic 订阅匹配。
- 一个事件命中多个 Agent 时，默认 fan-out 给所有匹配 Agent。
- 如需竞争消费，应引入 `consumer_group` 或 `delivery_mode`，不要靠隐式约定。

示例：

```rust
pub enum RouteRule {
    TargetAgent,
    Topic {
        pattern: TopicPattern,
        agent_ids: Vec<AgentId>,
    },
    Predicate {
        name: String,
    },
}
```

### 5.5 Topic 事件示例

```json
{
  "id": "evt_001",
  "topic": "/sys/route-a/route-aa",
  "source": "cli",
  "target": null,
  "correlation_id": "corr_123",
  "causation_id": null,
  "priority": "normal",
  "payload": {
    "text": "hello"
  },
  "created_at": "2026-06-08T10:00:00Z"
}
```

如果某个 Agent 订阅 `/sys/**`，它会收到该事件。另一个 Agent 只订阅 `/sys/route-a`，则不会收到该事件。

### 5.6 `/sys` 层级 Agent 路由

`/sys` 用于表达系统内部 Agent 的层级路由。推荐语义是“逐级处理、显式下发”：

```text
/sys
  -> /sys/route-a
      -> /sys/route-a/route-aa
```

订阅关系示例：

```text
/sys                    -> root-agent
/sys/route-a            -> agent-a
/sys/route-a/route-aa   -> agent-a11, agent-a12
```

执行链路：

1. 外部输入或上游模块发布 `/sys`。
2. `root-agent` 处理完成后显式 `ctx.emit("/sys/route-a", payload)`。
3. `agent-a` 处理完成后显式 `ctx.emit("/sys/route-a/route-aa", payload)`。
4. `agent-a11` 和 `agent-a12` 同时收到 `/sys/route-a/route-aa` 并各自处理。

默认不做父 Topic 到子 Topic 的自动递归投递。也就是说，发布 `/sys/route-a` 时，只会命中订阅 `/sys/route-a` 或匹配该 Topic 的通配订阅者，不会因为目录或命名层级自动投递给 `/sys/route-a/route-aa` 的订阅者。

Lua 示例：

```lua
function on_event(event, ctx)
  if event.topic == "/sys" then
    ctx.emit("/sys/route-a", {
      input = event.payload,
      root_result = "processed"
    })
  elseif event.topic == "/sys/route-a" then
    ctx.emit("/sys/route-a/route-aa", {
      input = event.payload,
      route_a_result = "processed"
    })
  end
end
```

权限建议：

```yaml
permissions:
  emit:
    - /sys/route-a/**
```

父 Agent 只允许向自己的子路由分支继续发布，避免一个 Agent 越权驱动其他路由分支。

## 6. 异步执行模型

### 6.1 Rust 层

Rust 层完全异步：

- EventBus 发布非阻塞。
- Scheduler 异步消费事件。
- 每个 Agent 通过 `tokio::spawn` 独立运行。
- 多个 Agent 可由 Tokio 多线程 runtime 并行调度。
- I/O 操作通过 Rust async function 完成。

### 6.2 Lua 层

单个 Lua State 内部是同步执行模型：

- 同一个 Lua State 一次只执行一个 Lua 方法。
- CPU 密集循环会阻塞该 Agent。
- 一个 Agent 的 Lua 阻塞不应影响其他 Agent，因为每个 Agent 使用独立 Lua State。
- 不建议多个 Agent 共享同一个 Lua State。

### 6.3 Lua 调用 Rust 异步函数

Lua 需要 I/O 时，通过 Rust 暴露的异步函数完成：

```rust
let http_get = lua.create_async_function(|_lua, url: String| async move {
    let text = reqwest::get(&url).await?.text().await?;
    Ok(text)
})?;

lua.globals().set("http_get", http_get)?;
```

Lua 脚本：

```lua
local data = http_get("https://api.example.com")
```

效果：

- Lua 代码看起来像同步调用。
- mlua 将 Rust Future 转换为 Lua 协程。
- 等待 I/O 时 Lua yield。
- Tokio 可以继续调度其他 Agent。

## 7. 状态管理

本方案不建议使用一个中心化共享 State 作为所有 Agent 的默认工作方式。

推荐分层：

### 7.1 Agent 局部状态

每个 Agent 维护自己的局部状态：

- 会话上下文。
- 最近处理的事件 ID。
- 工具调用缓存。
- Agent 内部配置。
- 当前脚本版本和订阅版本。

可定期持久化到：

- SQLite
- RocksDB
- Redis
- PostgreSQL

### 7.2 全局状态

跨 Agent 共享状态必须显式建模：

- 任务表。
- 会话表。
- 用户状态表。
- 工具调用记录。
- 审计日志。
- Topic 订阅表。

不要让 Agent 直接读写任意全局变量。跨 Agent 协作应通过事件和持久化存储完成。

### 7.3 幂等与恢复

必须记录：

- 已处理事件 ID。
- 已处理事件的 `topic`。
- Agent 当前状态版本。
- 外部工具调用 idempotency key。
- 失败事件和重试次数。

Agent 处理事件前应检查事件是否已经处理过，避免 EventBus 至少一次投递导致重复副作用。

## 8. 热更新设计

Lua 是热更新单元。

建议流程：

1. 监听 Lua 脚本文件变化。
2. 加载新脚本到新的 Lua State。
3. 执行初始化和自检。
4. 校验新脚本声明的 Topic 订阅是否有效。
5. 暂停目标 Agent 接收新事件。
6. 等当前事件处理完成或超时取消。
7. 替换 AgentRuntime 中的 Lua State 和订阅版本。
8. 恢复事件消费。
9. 失败时回滚到旧 Lua State 和旧订阅版本。

生产环境建议保留版本号：

```text
agent_id = "planner"
script_version = "2026-06-08.1"
subscription_version = "2026-06-08.1"
```

所有事件处理日志应记录 `script_version` 和命中的 Topic pattern，方便回溯问题。

## 9. 可靠性机制

### 9.1 超时

每次 Agent 处理事件都应设置超时：

```rust
let result = tokio::time::timeout(agent_timeout, handle_event()).await;
```

超时后：

- 发布 `/agent/failed` 或 `/task/failed`。
- 记录错误原因。
- 进入重试或死信队列。

### 9.2 重试

建议按 Topic 或 Topic pattern 配置重试策略：

- 最大重试次数。
- 指数退避。
- 是否允许重试有副作用操作。
- 重试失败后进入死信队列。

### 9.3 死信队列

死信事件应包含：

- 原始事件。
- 原始 `topic`。
- 命中的订阅 pattern。
- 失败 Agent。
- 失败原因。
- 重试次数。
- 最后错误堆栈。
- 发生时间。

### 9.4 事件顺序

默认不要假设全局事件有序。

如果某类业务要求顺序，应按 key 分区：

- `session_id`
- `task_id`
- `user_id`
- `agent_id`
- `topic`

同一个 key 的事件进入同一个串行队列。不要用全局锁保证所有 Topic 的全局顺序。

## 10. 可观测性

必须始终记录事件链路。

建议每个事件携带或派生日志字段：

- `event_id`
- `topic`
- `target`
- `correlation_id`
- `causation_id`
- `agent_id`
- `subscription_pattern`
- `script_version`
- `span_id`
- `timestamp`

建议接入：

- `tracing`
- `tracing-subscriber`
- OpenTelemetry
- metrics counter / histogram

需要支持的调试能力：

- 查看某个 correlation 的完整事件链。
- 查看某个 Topic 的订阅者。
- 查看某个 Topic pattern 命中的事件历史。
- 查看某个 Agent 的事件处理历史。
- 查看失败事件和死信事件。
- 查看 Agent 当前状态。
- 查看 Lua 脚本版本。

## 11. Lua 沙箱

Lua 脚本必须运行在受控环境中：

- 禁用 `os`、`io`、`debug` 等危险库。
- 不允许直接访问文件系统。
- 不允许直接访问环境变量。
- 不允许直接创建网络连接。
- 所有外部能力通过 Rust 白名单工具函数暴露。
- 限制执行时间。
- 限制内存。
- 对 Lua 返回值做 schema 校验。
- 对 Lua 发出的 Topic 做格式校验和权限校验。

CPU 密集型任务不应放在 Lua 中执行。如果必须执行，应下沉到 Rust worker 或独立进程。

## 12. 核心能力范围

系统应具备以下核心能力：

1. 基于 Tokio 的进程内 EventBus。
2. `Event` 使用 `topic` 字段，不再使用 `event_type` 作为路由字段。
3. Scheduler 订阅 EventBus。
4. Scheduler 支持精确 Topic 和分段通配 Topic 匹配。
5. Agent 注册表：`HashMap<AgentId, mpsc::Sender<Event>>`。
6. 每个 Agent 一个 bounded 私有队列。
7. 每个 Agent 一个 Tokio task。
8. 每个 Agent 一个独立 Lua State。
9. Lua 脚本实现 `on_event(event, ctx)`。
10. Rust 暴露 `ctx.emit` 和一个示例 async tool。
11. Agent 示例链路。
12. 基础 tracing。
13. 超时处理。
14. 死信队列。

示例事件链：

```text
CLI 发布 /input/user
        |
        v
EventBus
        |
        v
Scheduler
        |
        v
EchoAgent.on_event()
        |
        v
发布 /agent/reply
```

Topic 匹配示例：

```text
Agent A 订阅 /sys/**
Agent B 订阅 /sys/route-a
Agent C 订阅 /sys/route-a/*

发布 /sys/route-a/route-aa:
- Agent A 收到
- Agent C 收到
- Agent B 不收到

发布 /sys/route-a:
- Agent A 收到
- Agent B 收到
- Agent C 不收到
```

## 13. 推荐模块划分

```text
src/
  eventbus/
    mod.rs
    event.rs
    memory.rs
    dead_letter.rs

  scheduler/
    mod.rs
    registry.rs
    routing.rs
    subscription.rs
    topic.rs

  agent/
    mod.rs
    runtime.rs
    state.rs
    lifecycle.rs

  lua/
    mod.rs
    loader.rs
    sandbox.rs
    bindings.rs
    hot_reload.rs

  tools/
    mod.rs
    external_agent.rs
    http.rs
    llm.rs

  adapter/
    mod.rs
    registry.rs
    router.rs
    runtime.rs
    transports/
      builtin.rs
      stdio.rs
      http.rs
      eventbus.rs
      mcp.rs

  mcp/
    mod.rs
    client.rs
    server.rs
    policy.rs

  observability/
    mod.rs
    tracing.rs
    metrics.rs

  cli/
    mod.rs
    run.rs
    emit.rs
    inspect.rs
```

## 14. 技术选型

Rust crate 建议：

- `tokio`：异步运行时。
- `mlua`：Lua 绑定和 async function。
- `serde` / `serde_json`：事件 payload。
- `thiserror` / `anyhow`：错误处理。
- `tracing` / `tracing-subscriber`：日志和链路。
- `clap`：CLI。
- `notify`：Lua 脚本热更新监听。
- `reqwest`：HTTP 工具示例。
- `sqlx` / `rusqlite` / `rocksdb`：状态持久化可选。

外部 Agent 调用、动态 Adapter 扩展与 MCP 双向集成详见 `Lua调用外部Agent动态Adapter架构方案.md`。该子方案约定 Lua 不直接调用 Claude、Codex、MCP server 或任意 shell 命令，而是通过 Rust 托管的 AdapterRegistry、AdapterRouter、McpAdapter 和统一 Topic / Tool 接口接入外部 Agent 能力；系统也可以作为 MCP server 对外暴露受控的 `agent.invoke`、`adapter.invoke` 等工具。

项目内 Tool、Lua Skill 和 MCP tool handler 的业务实现可以进一步由 `Lua承载Skill-MCP-Tool热更新架构方案.md` 定义的 Lua Capability Runtime 承载。该扩展不改变本文的 Agent 调度模型：Lua capability 仍通过 Rust Tool Layer、AdapterRegistry 或 MCP Server 进入运行链路，权限和 schema 不由 Lua 自行决定。

## 15. 运行形态

### 15.1 单进程运行形态

- EventBus 使用进程内 broadcast。
- Scheduler、AgentRuntime、Lua State 和 AdapterRuntime 位于同一进程。
- 死信队列可以使用内存队列或本地持久化表。
- 适合本地开发、CLI 工具和轻量自动化。

### 15.2 持久化消息运行形态

- EventBus 使用 Redis Streams、NATS、Kafka、RabbitMQ 或 PostgreSQL。
- 关键事件可恢复、可重放。
- Agent 通过已处理事件 ID 做幂等消费。
- 死信队列持久化保存失败原因和重试记录。

### 15.3 可恢复进程内运行形态

- EventBus 内存路径使用 `broadcast` / `mpsc`。
- accepted 事件先写本地 Durable Event Log / WAL / Spool。
- Scheduler 和 Agent 消费进度通过 consumer watermark 记录。
- Runtime 崩溃后按 watermark 重放未 ack 事件。
- 支持单机系统重启恢复和 Runtime generation 蓝绿切流。
- 不承诺恢复写入 Durable Event Log 前已经丢失的纯内存事件。

### 15.4 分布式运行形态

- Scheduler 可以多实例部署。
- Agent 可以多进程或多机器部署。
- 需要按 `session_id`、`task_id`、`agent_id` 或其他 key 分区保证局部顺序。
- 可观测性应接入 OpenTelemetry，所有事件链路必须带 correlation 信息。

## 16. 风险与规避

### 16.1 事件丢失

规避：

- 本地开发可使用纯进程内 best-effort。
- 单机高可用使用可恢复进程内 EventBus。
- 分布式生产使用持久化消息队列。
- accepted 事件必须先写 Durable Event Log 或外部队列。
- 关键事件落库。

### 16.2 重复消费

规避：

- 每个事件有唯一 ID。
- Agent 记录已处理事件。
- 外部副作用使用 idempotency key。

### 16.3 隐式流程难调试

规避：

- 强制 correlation_id。
- 记录 causation_id。
- 记录 `topic` 和命中的 `subscription_pattern`。
- 提供事件链查询。
- 接入 tracing / OpenTelemetry。

### 16.4 Lua 阻塞

规避：

- 每个 Agent 独立 Lua State。
- 每次事件处理设置 timeout。
- 禁止 CPU 密集脚本。
- 高风险脚本放到独立进程或 Rust worker。

### 16.5 跨 Agent 状态复杂

规避：

- 局部状态归 Agent。
- 全局状态显式建表。
- 跨 Agent 协作通过事件完成。
- 不使用隐式全局可变状态。

### 16.6 Topic 误匹配

规避：

- Topic 必须规范化后再进入 EventBus。
- Scheduler 必须按段匹配，不使用简单字符串前缀。
- `*` 只匹配一段，`**` 只允许出现在最后一段。
- 为 `/sys/*`、`/sys/**`、`/sys/route-a/*` 建立单元测试。

## 17. 何时不适用

不建议使用该架构的场景：

- 流程步骤少且固定，普通函数调用即可完成。
- 强确定性长流程更重要，使用 LangGraph 或状态机更简单。
- 要求强一致事务，例如金融转账。
- 团队缺少 Rust 和 Lua 维护能力。
- 业务不需要热更新、动态 Agent 或层级 Topic 路由。

## 18. 设计校验标准

系统设计应满足：

- 可以启动 EventBus 和 Scheduler。
- 可以注册至少两个 Agent。
- 每个 Agent 拥有独立 Lua State。
- CLI 可以发布 `/input/user` 事件。
- Scheduler 可以把事件投递到目标 Agent。
- Scheduler 支持 `/sys/route-a` 精确匹配。
- Scheduler 支持 `/sys/*` 单段通配。
- Scheduler 支持 `/sys/**` 多段尾部通配。
- `/sys/*` 不应匹配 `/sys/route-a/route-aa`。
- `/sys/**` 应匹配 `/sys/route-a` 和 `/sys/route-a/route-aa`。
- Agent 可以执行 Lua `on_event`。
- Lua 可以通过 `ctx.emit` 发布新 Topic 事件。
- Lua 可以调用一个 Rust async 工具函数。
- Agent 处理事件有 timeout。
- 失败事件进入死信队列。
- tracing 中能看到完整 correlation 链路和命中的 Topic pattern。

## 19. 验证矩阵

设计验证应覆盖：

- Topic parser：合法 Topic、非法空段、尾部 `/`、非法 `**` 位置。
- Topic matcher：exact、`*`、`**`、不允许字符串前缀误匹配。
- Scheduler routing：`target` 优先、fan-out、多 Agent 命中、无命中。
- Failure path：Agent 不在线、队列满、Lua 报错、Lua 超时。
- Integration path：CLI 发布 `/input/user`，示例 Agent 发布 `/agent/reply`。

## 20. 总结

Rust + Lua + EventBus 的合适落点是 **Topic 驱动的事件 Agent 调度架构**，而不是默认复刻 LangGraph 的中心化有向状态图。

该方案的优势是解耦、并发、热更新、动态扩展和层级 Topic 路由；代价是事件一致性、状态管理、Topic 语义和调试复杂度更高。

该方案的关键是先明确 Topic、Agent 边界、状态和可靠性语义，再根据运行形态选择纯进程内、可恢复进程内或外部持久化 EventBus 后端。
