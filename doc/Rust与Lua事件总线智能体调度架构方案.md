# Rust + Lua + EventBus Topic Agent 调度架构方案

更新日期：2026-06-08

## 1. 方案定位

本方案基于 `方案.txt` 重新整理，目标不是实现一个中心化的 LangGraph 克隆，而是实现一套 **Topic 驱动的 EventBus 多 Agent 调度系统**。

核心思路：

- Rust 作为高性能异步运行时，负责 EventBus、Scheduler、Agent 生命周期、并发隔离、超时、重试和观测。
- Lua 作为 Agent 的脚本化业务逻辑，负责意图识别、工具调用编排、局部状态转换和热更新。
- EventBus 作为核心通信机制，负责外部系统、Scheduler、Agent 之间的事件传递。
- Topic 是事件的路由地址，例如 `/user/input`、`/task/created`、`/funcA/funcAB/funcABCC`。
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

1. 外部系统或 Agent 向 EventBus 发布事件，例如 `/user/input`、`/task/created`、`/tool/completed`。
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

EventBus 是全局消息通道，不直接执行 Agent 逻辑，也不在第一阶段承担复杂 Topic 匹配。

建议第一阶段使用进程内实现：

- `tokio::sync::broadcast`：适合把所有事件广播给 Scheduler、观测器、审计器等系统订阅者。
- `tokio::sync::mpsc`：适合 Scheduler 到 Agent 的点对点私有队列。

推荐组合：

- 全局 EventBus 使用 `broadcast` 发布 `Event`。
- Scheduler 负责 Topic 订阅匹配。
- Scheduler 到 Agent 使用每个 Agent 独立的 bounded `mpsc` 私有队列。
- 死信队列使用单独 `mpsc` 或持久化表。

MVP 投递语义：

- 进程内 EventBus 是 best-effort，不承诺进程崩溃后恢复。
- Scheduler 到 Agent 的私有队列建议 bounded，避免单个 Agent 拖垮进程。
- Agent 队列满、Agent 不在线、目标不存在时，Scheduler 应写入死信队列并记录原因。
- 生产环境如需至少一次投递，应切换到持久化消息队列和幂等消费。

后续需要跨进程或分布式时，可以替换为：

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
- 发布后续事件。

建议约定每个 Agent 暴露统一入口：

```lua
function on_event(event, ctx)
  if event.topic == "/user/input" then
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
- `topic`：事件路由地址，例如 `/user/input`、`/task/completed`、`/funcA/funcAB/funcABCC`。
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
/funcA/funcAB/funcABCC
```

推荐规则：

- 必须以 `/` 开头。
- `/` 只作为分段符，不代表文件系统路径。
- 不建议以 `/` 结尾，根 Topic `/` 除外。
- 不允许出现空段，例如 `/funcA//funAC`。
- Segment 建议只包含字母、数字、`-`、`_`、`.`。
- 系统 Topic 建议使用稳定命名，例如 `/system/started`、`/agent/failed`。
- 业务函数 Topic 可以保留函数命名，例如 `/funcA/funcAB/funcABCC`。

推荐基础 Topic：

```text
/system/started
/system/shutdown

/agent/registered
/agent/started
/agent/stopped
/agent/failed

/user/input
/user/cancelled

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
| `/funcA/funAC` | `/funcA/funAC` | `/funcA/funAC/x` | 精确匹配 |
| `/funcA/*` | `/funcA/funAC` | `/funcA/funcAB/funcABCC` | `*` 匹配一段 |
| `/funcA/funcAB/*` | `/funcA/funcAB/funcABCC` | `/funcA/funAC` | 固定前两段，第三段任意 |
| `/funcA/**` | `/funcA/funAC`、`/funcA/funcAB/funcABCC` | `/funcB/funAC` | `**` 匹配剩余任意层级 |
| `/**` | 所有合法 Topic | 无 | 全局订阅，适合审计和 tracing |

示例：

```text
事件 topic = /funcA/funcAB/funcABCC

命中:
- /funcA/**
- /funcA/funcAB/*
- /funcA/funcAB/funcABCC

不命中:
- /funcA/*
- /funcA/funAC
- /funcB/**
```

### 5.4 路由优先级

Scheduler 推荐按以下顺序处理：

1. 直接目标路由：事件带 `target = agent_id`，直接投递给目标 Agent。
2. 精确 Topic 订阅：例如 `/funcA/funAC`。
3. 通配 Topic 订阅：例如 `/funcA/*`、`/funcA/**`。
4. 规则路由：根据 payload、标签、优先级或负载选择 Agent。

默认语义：

- `target` 存在时，只投递给目标 Agent，不再做普通 Topic fan-out。
- `target` 不存在时，按照 Topic 订阅匹配。
- 一个事件命中多个 Agent 时，MVP 默认 fan-out 给所有匹配 Agent。
- 后续如需竞争消费，应引入 `consumer_group` 或 `delivery_mode`，不要靠隐式约定。

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
  "topic": "/funcA/funcAB/funcABCC",
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

如果某个 Agent 订阅 `/funcA/**`，它会收到该事件。另一个 Agent 只订阅 `/funcA/funAC`，则不会收到该事件。

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

必须从第一版开始记录事件链路。

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

## 12. 最小化原型

第一阶段只实现以下能力：

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
11. Echo Agent demo。
12. 基础 tracing。
13. 超时处理。
14. 死信队列。

建议 demo：

```text
CLI 发布 /user/input
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

Topic 匹配 demo：

```text
Agent A 订阅 /funcA/**
Agent B 订阅 /funcA/funAC
Agent C 订阅 /funcA/funcAB/*

发布 /funcA/funcAB/funcABCC:
- Agent A 收到
- Agent C 收到
- Agent B 不收到

发布 /funcA/funAC:
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
    http.rs
    llm.rs

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

## 15. 生产增强路线

### 阶段 1：单进程原型

- 进程内 EventBus。
- Topic Event 模型。
- Scheduler。
- 精确匹配和 `*`、`**` Topic pattern。
- 多 Agent task。
- Lua `on_event`。
- Echo Agent。
- 基础 tracing。

### 阶段 2：可靠性增强

- 超时。
- 重试。
- 死信队列。
- 事件去重。
- Agent 局部状态持久化。
- Lua 沙箱限制。
- Topic 权限校验。

### 阶段 3：热更新与工具系统

- Lua 脚本热更新。
- Topic 订阅热更新。
- 工具白名单。
- async HTTP / LLM / DB 工具。
- Agent script version。
- 状态迁移。

### 阶段 4：分布式扩展

- Redis Streams / NATS 替换进程内 EventBus。
- Scheduler 多实例。
- Agent 多进程。
- 按 key 分区保证局部顺序。
- OpenTelemetry 全链路追踪。

## 16. 风险与规避

### 16.1 事件丢失

规避：

- MVP 可接受进程内 best-effort。
- 生产使用持久化消息队列。
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
- 为 `/funcA/*`、`/funcA/**`、`/funcA/funcAB/*` 建立单元测试。

## 17. 何时不适用

不建议使用该架构的场景：

- 流程步骤少且固定，普通函数调用即可完成。
- 强确定性长流程更重要，使用 LangGraph 或状态机更简单。
- 要求强一致事务，例如金融转账。
- 团队缺少 Rust 和 Lua 维护能力。
- 业务不需要热更新、动态 Agent 或层级 Topic 路由。

## 18. 验收标准

MVP 完成时应满足：

- 可以启动 EventBus 和 Scheduler。
- 可以注册至少两个 Agent。
- 每个 Agent 拥有独立 Lua State。
- CLI 可以发布 `/user/input` 事件。
- Scheduler 可以把事件投递到目标 Agent。
- Scheduler 支持 `/funcA/funAC` 精确匹配。
- Scheduler 支持 `/funcA/*` 单段通配。
- Scheduler 支持 `/funcA/**` 多段尾部通配。
- `/funcA/*` 不应匹配 `/funcA/funcAB/funcABCC`。
- `/funcA/**` 应匹配 `/funcA/funAC` 和 `/funcA/funcAB/funcABCC`。
- Agent 可以执行 Lua `on_event`。
- Lua 可以通过 `ctx.emit` 发布新 Topic 事件。
- Lua 可以调用一个 Rust async 工具函数。
- Agent 处理事件有 timeout。
- 失败事件进入死信队列。
- tracing 中能看到完整 correlation 链路和命中的 Topic pattern。

## 19. 测试建议

第一版至少补以下测试：

- Topic parser：合法 Topic、非法空段、尾部 `/`、非法 `**` 位置。
- Topic matcher：exact、`*`、`**`、不允许字符串前缀误匹配。
- Scheduler routing：`target` 优先、fan-out、多 Agent 命中、无命中。
- Failure path：Agent 不在线、队列满、Lua 报错、Lua 超时。
- Integration demo：CLI 发布 `/user/input`，Echo Agent 发布 `/agent/reply`。

## 20. 总结

Rust + Lua + EventBus 的合适落点是 **Topic 驱动的事件 Agent 调度架构**，而不是默认复刻 LangGraph 的中心化有向状态图。

该方案的优势是解耦、并发、热更新、动态扩展和层级 Topic 路由；代价是事件一致性、状态管理、Topic 语义和调试复杂度更高。

建议先用进程内 EventBus + Scheduler + 多 Lua Agent 做最小原型，再逐步补齐超时、重试、死信、持久化、热更新和分布式消息队列。

