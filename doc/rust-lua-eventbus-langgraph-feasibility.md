# Rust + Lua + EventBus 实现类 LangGraph 能力可行性方案

更新日期：2026-06-08

## 1. 结论

使用 Rust + Lua + EventBus 实现类似 LangGraph 的工作流/Agent 编排能力是可行的。

推荐目标不是完整复刻 LangGraph，而是实现一套更适合本项目技术栈的执行框架：

- Rust 负责可靠的图执行内核、状态管理、检查点、恢复、并发和错误处理。
- Lua 负责灵活的业务节点、条件路由、Agent 编排和工具调用胶水逻辑。
- EventBus 负责事件流、观测、调试、人审中断、外部 worker 通信和 UI 推送。

该方案的关键不在“画图”或“节点连接”本身，而在：

- 状态如何合并。
- 节点如何幂等执行。
- 中断后如何恢复。
- 执行过程如何持久化。
- 运行时事件如何被观察、回放和调试。

## 2. 目标能力

第一阶段建议实现以下 LangGraph 风格能力：

- 图式工作流：节点、边、条件边、开始节点、结束节点。
- 共享状态：所有节点基于同一个状态对象读取和返回增量更新。
- Reducer：对状态字段进行可控合并，例如覆盖、追加消息、集合合并。
- 条件路由：节点执行后可以根据状态或返回结果选择下一节点。
- Checkpoint：每一步执行后保存状态快照和执行位置。
- Interrupt / Resume：节点可主动中断，等待人工输入或外部系统结果后继续。
- Event Stream：运行时持续发出节点开始、节点完成、状态更新、错误、中断等事件。
- CLI 调试：可以运行、恢复、查看状态、查看事件时间线。

暂不建议第一阶段实现：

- 分布式调度。
- 多租户权限系统。
- 可视化图编辑器。
- 完整 LangGraph API 兼容。
- 任意 Lua 沙箱外系统调用。
- 复杂事务和跨节点分布式锁。

## 3. 总体架构

```text
Lua DSL / Lua Nodes
        |
        v
Rust Graph Compiler
        |
        v
Rust Runtime Kernel <----> State Store / Checkpoint Store
        |
        v
EventBus
        |
        v
CLI / UI / Tracer / Human-in-the-loop / External Workers
```

### 3.1 Rust 图执行内核

Rust 运行时负责所有强约束能力：

- 加载 Lua 图定义。
- 编译为内部 Graph IR。
- 校验节点、边、条件路由和 reducer。
- 调度节点执行。
- 合并状态 delta。
- 写入 checkpoint。
- 发布运行时事件。
- 处理重试、超时、取消、错误恢复。
- 支持 interrupt/resume。

Rust 侧应该保持“状态权威”和“执行权威”，Lua 只负责返回节点结果，不能直接修改全局执行状态。

### 3.2 Lua 编排层

Lua 适合作为上层编排 DSL：

- 写节点逻辑。
- 调用工具函数。
- 编写条件路由。
- 组合 Agent prompt。
- 根据状态决定下一步。

示例：

```lua
graph.node("plan", function(state, ctx)
  return {
    update = {
      plan = "执行计划内容"
    },
    goto = "execute"
  }
end)

graph.node("review", function(state, ctx)
  if state.need_human_review then
    return {
      interrupt = {
        reason = "需要人工审批",
        payload = state.plan
      }
    }
  end

  return {
    update = {
      reviewed = true
    },
    goto = "__end__"
  }
end)

graph.edge("__start__", "plan")
graph.edge("plan", "review")
```

### 3.3 EventBus 事件层

EventBus 负责运行时可观测性和外部集成，但不应作为唯一状态源。

建议事件类型：

```text
graph.started
graph.finished
graph.failed
graph.resumed

node.started
node.output
node.finished
node.failed

state.delta
state.committed

route.selected
checkpoint.saved

interrupt.raised
interrupt.resolved

token.streamed
tool.started
tool.finished
tool.failed
```

第一阶段可以用进程内 EventBus：

- `tokio::sync::broadcast`
- `tokio::sync::mpsc`

后续需要跨进程或分布式时再替换为：

- NATS
- Redis Streams
- Kafka
- PostgreSQL LISTEN/NOTIFY

## 4. 核心数据模型

### 4.1 Graph

```rust
pub struct Graph {
    pub nodes: HashMap<NodeId, NodeDef>,
    pub edges: Vec<EdgeDef>,
    pub reducers: HashMap<String, ReducerDef>,
}
```

### 4.2 NodeDef

```rust
pub struct NodeDef {
    pub id: NodeId,
    pub kind: NodeKind,
    pub timeout_ms: Option<u64>,
    pub retry: RetryPolicy,
}

pub enum NodeKind {
    LuaFunction { name: String },
    NativeRust { name: String },
    ExternalWorker { queue: String },
}
```

### 4.3 NodeResult

```rust
pub struct NodeResult {
    pub update: serde_json::Value,
    pub goto: Option<NodeId>,
    pub interrupt: Option<InterruptPayload>,
}
```

### 4.4 Reducer

Reducer 决定多个节点对同一个 state key 写入时如何合并。

建议内置：

- `overwrite`：直接覆盖。
- `append`：数组追加。
- `append_messages`：消息列表追加，并可按 message id 去重。
- `set_union`：集合并集。
- `merge_object`：对象浅合并。
- `error_on_conflict`：检测到冲突直接报错。

不建议第一阶段允许任意 Lua reducer，因为这会让状态合并不可预测，也会增加恢复和回放难度。

## 5. 执行流程

### 5.1 正常执行

1. CLI 或 API 提交 run 请求。
2. Rust runtime 创建 `run_id` 和初始状态。
3. 发布 `graph.started`。
4. 从 `__start__` 进入第一个节点。
5. 发布 `node.started`。
6. 调用 Lua 或 Rust 节点。
7. 节点返回 `NodeResult`。
8. 发布 `state.delta`。
9. Rust 使用 reducer 合并状态。
10. 写 checkpoint。
11. 发布 `checkpoint.saved` 和 `state.committed`。
12. 根据 `goto` 或 edge 选择下一节点。
13. 重复执行，直到 `__end__`。
14. 发布 `graph.finished`。

### 5.2 中断与恢复

节点可以返回：

```json
{
  "interrupt": {
    "reason": "need_human_approval",
    "payload": {
      "plan": "..."
    }
  }
}
```

Runtime 行为：

1. 保存当前 checkpoint。
2. 将 run 标记为 `interrupted`。
3. 发布 `interrupt.raised`。
4. CLI/UI/外部系统展示等待信息。
5. 用户或外部系统提交 resume payload。
6. Runtime 读取 checkpoint。
7. 将 resume payload 合并到状态或传给当前节点。
8. 发布 `graph.resumed`。
9. 继续后续节点。

## 6. 持久化设计

第一阶段建议使用 SQLite，简单可靠，方便 CLI 和本地调试。

### 6.1 表设计建议

```sql
CREATE TABLE graph_runs (
  run_id TEXT PRIMARY KEY,
  graph_name TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE graph_checkpoints (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL,
  step INTEGER NOT NULL,
  node_id TEXT NOT NULL,
  state_json TEXT NOT NULL,
  next_node_id TEXT,
  created_at TEXT NOT NULL,
  UNIQUE(run_id, step)
);

CREATE TABLE graph_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL,
  step INTEGER,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
```

### 6.2 Checkpoint 原则

- 恢复只信 checkpoint，不信 EventBus。
- EventBus 事件可以落库，用于调试和回放。
- 每个节点执行完成后保存 checkpoint。
- interrupt 前必须保存 checkpoint。
- 节点重试必须记录 attempt 信息。

## 7. Rust 模块划分建议

```text
src/
  graph/
    mod.rs
    model.rs
    compiler.rs
    validate.rs

  runtime/
    mod.rs
    executor.rs
    scheduler.rs
    state.rs
    reducer.rs
    interrupt.rs

  lua/
    mod.rs
    loader.rs
    bindings.rs
    sandbox.rs

  eventbus/
    mod.rs
    event.rs
    in_memory.rs
    sink.rs

  checkpoint/
    mod.rs
    sqlite.rs
    memory.rs

  cli/
    mod.rs
    run.rs
    resume.rs
    inspect.rs
```

## 8. 推荐技术选型

### 8.1 Rust crate

- Lua 集成：`mlua`
- 异步运行时：`tokio`
- JSON 状态：`serde`、`serde_json`
- 错误处理：`thiserror`、`anyhow`
- SQLite：`sqlx` 或 `rusqlite`
- CLI：`clap`
- 日志与 tracing：`tracing`、`tracing-subscriber`
- 时间：`chrono` 或 `time`

### 8.2 Lua 约束

Lua 脚本必须运行在受控环境中：

- 禁用危险库：`os`、`io`、`debug`。
- 限制脚本执行时间。
- 限制内存使用。
- 工具调用必须通过 Rust 暴露的安全 API。
- 所有 node 返回值必须通过 schema 校验。

## 9. MVP 范围

建议第一版只实现最小可用闭环：

1. 加载一个 Lua graph 文件。
2. 支持 `graph.node`、`graph.edge`、`graph.conditional`。
3. 支持 JSON state。
4. 支持内置 reducer。
5. 支持顺序执行和条件跳转。
6. 支持 SQLite checkpoint。
7. 支持 interrupt/resume。
8. 支持进程内 EventBus。
9. 支持 CLI：

```text
eva-graph run examples/research_agent.lua --input input.json
eva-graph resume <run_id> --input resume.json
eva-graph inspect-state <run_id>
eva-graph list-events <run_id>
```

## 10. 后续增强路线

### 阶段 1：单进程可靠执行

- Lua graph DSL。
- Rust runtime。
- SQLite checkpoint。
- CLI 调试。
- interrupt/resume。
- 基础事件流。

### 阶段 2：工具调用和 Agent 编排

- Rust 注册工具函数。
- Lua 调用工具。
- LLM 调用抽象。
- token streaming 事件。
- retry/backoff。
- timeout/cancel。

### 阶段 3：并行和可观测性

- 支持并行节点。
- reducer 冲突检测。
- graph event replay。
- Web UI 或 TUI timeline。
- OpenTelemetry 集成。

### 阶段 4：外部 worker 和分布式

- EventBus 替换为 NATS 或 Redis Streams。
- 外部 worker 执行节点。
- 多进程恢复。
- 队列消费确认。
- 分布式锁和租约。

## 11. 主要风险与规避

### 11.1 状态结构失控

风险：Lua 动态类型容易让状态字段结构漂移。

规避：

- 对初始 state 和 node update 做 JSON Schema 校验。
- reducer 必须显式声明。
- 未声明 reducer 的冲突写入直接报错。

### 11.2 节点副作用重复执行

风险：节点失败重试或恢复后可能重复调用外部 API。

规避：

- 节点必须尽量幂等。
- 对外部调用使用 idempotency key。
- checkpoint 中记录 tool call id 和 attempt。

### 11.3 EventBus 与状态不一致

风险：事件已发布，但 checkpoint 写入失败，导致观测层和真实状态不一致。

规避：

- 恢复只信 checkpoint。
- 关键事件可在 checkpoint 事务后发布。
- 落库事件作为调试材料，不作为执行真相。

### 11.4 Lua 沙箱逃逸

风险：Lua 脚本访问系统文件、环境变量或执行命令。

规避：

- 禁用危险标准库。
- 只暴露白名单 API。
- 限制执行时间和内存。
- 对脚本来源做信任分级。

### 11.5 并行写冲突

风险：多个节点同时写同一状态字段，合并结果不可预测。

规避：

- 第一阶段不做复杂并行。
- 后续并行必须要求每个可并行字段声明 reducer。
- 无 reducer 的冲突直接失败。

## 12. 验收标准

MVP 完成时应满足：

- 可以运行一个包含 3 个以上节点的 Lua graph。
- 支持普通边和条件边。
- 节点可以读取 state，并返回 update/goto/interrupt。
- Runtime 能正确合并 state delta。
- 每个节点完成后写入 checkpoint。
- 可以从 interrupt 状态 resume。
- 可以查看某个 run 的当前 state。
- 可以查看某个 run 的事件列表。
- 节点失败时能记录错误事件并标记 run 失败。
- 单元测试覆盖 reducer、路由、checkpoint、resume。

## 13. 推荐示例场景

第一个 demo 可以做“研究-总结-人工审核”流程：

```text
__start__
  |
  v
research
  |
  v
summarize
  |
  v
review
  |---- interrupt: 等待人工确认
  v
__end__
```

Lua 节点：

- `research`：模拟或调用搜索工具。
- `summarize`：生成摘要。
- `review`：如果 `require_review = true`，触发 interrupt；否则结束。

该 demo 可以同时验证：

- 节点执行。
- 状态更新。
- 条件路由。
- 中断。
- 恢复。
- 事件流。
- checkpoint。

## 14. 参考

- LangGraph State / Reducer 概念：https://langchain-ai.github.io/langgraph/how-tos/state-reducers/
- LangGraph Persistence：https://docs.langchain.com/oss/python/langgraph/persistence
- LangGraph Interrupts：https://docs.langchain.com/oss/python/langgraph/interrupts
- LangGraph Streaming：https://langchain-ai.github.io/langgraph/cloud/concepts/streaming/

