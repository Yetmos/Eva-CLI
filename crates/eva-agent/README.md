# eva-agent / Agent 运行边界

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-agent` 负责 Agent 生命周期、私有队列和事件处理边界。V0.4 已实现同步 `AgentRuntime`，可接收 scheduler 投递的事件，并通过注入的 handler 调用 Lua host 或测试 handler。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 生命周期 | `AgentLifecycle`、`AgentLifecycleState` | 支持 created、running、draining、stopped、failed；非法 start/drain 返回 `Conflict`。 |
| 私有队列 | `AgentQueue` | bounded FIFO；容量为 0 无效，队列满返回 `Unavailable`。 |
| AgentRuntime | `AgentRuntime` | start 后才能 accept 事件；`run_next` 调用注入 handler 并生成 `AgentRunRecord`。 |
| Handler 输出 | `AgentHandlerOutput` | 保存 handler status 和可选文本 output。 |
| 执行记录 | `AgentRunRecord`、`AgentRunStatus` | 保存 agent id、event id、topic、handled/failed、handler status、output/error。 |
| 状态快照 | `AgentStateSnapshot` | 为后续 CLI/status 预留轻量状态报告。 |

## 模块边界

`eva-agent` 不解析 Lua 文件，不实现 sandbox，不直接调用 Adapter/MCP/hardware，也不保存 durable EventLog。runtime 注入 handler 后，AgentRuntime 只负责 queue、lifecycle 和标准化执行结果。

V0.4 中 `run_next` 是同步接口，适合 basic 示例和单元测试；timeout、cancel、retry、task status 会在 V0.5 扩展。

## 公开入口

```rust
use eva_agent::{AgentRuntime, AgentHandlerOutput, AgentRunStatus};
```

## 验证

```powershell
cargo test -p eva-agent
```

V0.4 已覆盖：未运行状态拒绝 accept、start 后接收事件、handler 执行记录、bounded queue overflow。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 增加 timeout、cancel、retry、task status/logs。 |
| V1.2 | 接入 memory/context API。 |
