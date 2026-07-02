# eva-agent/src / Agent 运行源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载 Agent 生命周期、私有队列、事件处理和 Agent-local state。当前为骨架，V0.4 先让 Agent 能消费一个事件并调用 host API。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V0.4 |
| `runtime.rs` | Agent event handling boundary | 骨架 | V0.4 |
| `lifecycle.rs` | Agent lifecycle 状态机 | 骨架 | V0.4 |
| `queue.rs` | 私有 bounded queue | 骨架 | V0.4 |
| `state.rs` | Agent-local state ownership | 骨架 | V0.4/V1.2 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 Agent descriptor、runtime handle 和 handler trait。 | Agent 可由 manifest 构造。 |
| 2 | 实现 lifecycle 和 queue。 | 状态转换和 queue full 可测。 |
| 3 | 接 Lua host 和 capability host API。 | Agent 可消费事件。 |
| 4 | 增加 timeout、cancel、retry 和 task status。 | V0.5 长任务可控。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Runtime | 事件 handler | 未实现 | 定义 trait 和 result。 |
| Lifecycle | start/stop/drain | 未实现 | 定义状态机。 |
| Queue | bounded queue | 未实现 | 实现 overflow 行为。 |
| State | Agent-local state | 未实现 | 设计 storage adapter。 |
