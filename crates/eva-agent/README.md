# eva-agent / Agent 运行边界

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-agent` 负责 Agent 生命周期、私有队列、事件处理边界和 Agent-local state。它消费 scheduler 投递的事件，通过受控 host API 调用 Lua/capability/memory 等服务，不直接实现 Lua sandbox，也不直接处理外部 provider transport。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Runtime | 骨架 | 执行 Agent event handler，管理 timeout、cancel、retry、trace。 |
| Lifecycle | 骨架 | 管理 created、starting、running、draining、stopped、failed 状态。 |
| Queue | 骨架 | Agent 私有 bounded queue，定义 overflow 和 backpressure。 |
| State | 骨架 | Agent-local state 读写边界，最终落到 `eva-storage` 或 `eva-memory`。 |
| Host API 调用 | 未实现 | 通过 trait 调用 Lua、capability、memory，不直接访问外部 I/O。 |
| 任务状态 | 未实现 | V0.5 输出 task status/logs/cancel 所需状态。 |

## 模块边界

`eva-agent` 做：

- 管理 Agent 的运行生命周期和事件 handler 边界。
- 从 mailbox 消费事件并输出结构化结果。
- 通过 host API 调用 Lua、capability、memory。
- 记录 trace、audit 和失败状态。

`eva-agent` 不做：

- 不解析 Lua 文件和 sandbox 细节，这属于 `eva-lua-host`。
- 不调用 Adapter transport、MCP、HTTP、shell 或硬件。
- 不保存 durable event log。
- 不决定全局 Topic 路由。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.4 | 定义 Agent descriptor、runtime handle、event handler trait。 | `eva-core`、`eva-config` | 可从 AgentManifest 构造 runtime descriptor。 |
| 2 | V0.4 | 实现 lifecycle 状态机和幂等 start/stop/drain。 | 标准库 | 非法状态转换有结构化错误。 |
| 3 | V0.4 | 实现 bounded queue 和消费循环。 | `eva-scheduler` mailbox | 队列满、关闭、取消行为可测。 |
| 4 | V0.4 | 接 Lua host 最小 `on_event(event, ctx)`。 | `eva-lua-host` | Agent 能消费一个事件并返回响应。 |
| 5 | V0.4 | 接 capability host API，支持一个 builtin capability。 | `eva-capability` | Lua/Agent 能拿到 capability 结构化结果。 |
| 6 | V0.5 | 实现 timeout、cancel、retry、task status。 | EventBus、Storage | CLI 可查询和取消长任务。 |
| 7 | V1.2 | 接 memory/context API。 | `eva-memory` | Agent 私有记忆和上下文受 policy 限制。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export runtime、lifecycle、queue、state。 |
| `src/runtime.rs` | Agent 事件处理边界 | `RESPONSIBILITY` 占位 | 定义 handler trait、runtime handle、task result。 |
| `src/lifecycle.rs` | 生命周期状态机 | `RESPONSIBILITY` 占位 | 定义状态枚举和合法转换。 |
| `src/queue.rs` | 私有队列和 overflow | `RESPONSIBILITY` 占位 | 实现 bounded queue 接口和 backpressure 错误。 |
| `src/state.rs` | Agent-local state | `RESPONSIBILITY` 占位 | 定义 state key、snapshot、storage adapter。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | 状态机和队列 | 未开始 | 覆盖生命周期、queue full、cancel。 |
| 集成测试 | Agent 消费事件 | 未开始 | 事件进入 mailbox 后调用 mock Lua host。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.4 | `cargo test -p eva-agent` | lifecycle、queue、runtime handler 可测。 |
| V0.4 | `cargo test -p eva-runtime` | AgentRuntime 可被 runtime 装配。 |
| V0.5 | CLI task 测试 | 状态查询、日志和取消语义稳定。 |

## English

`eva-agent` owns Agent lifecycle, private queues, event handling boundaries, and Agent-local state. It calls Lua, capability, and memory services only through controlled host APIs.
