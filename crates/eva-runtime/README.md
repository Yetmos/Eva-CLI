# eva-runtime / 运行时组合根

更新时间：2026-07-03

`eva-runtime` 是 Eva-CLI 的 composition root。下层 crate 不反向依赖 runtime；跨模块服务装配、运行闭环和 V0.5 任务诊断都由本 crate 统一组合。

## 当前实现

| 版本 | 能力 | 当前行为 |
| --- | --- | --- |
| V0.3 | no-op builder | `RuntimeBuilder::new().build(project)` 构造只读 runtime summary，用于 `doctor` 和 `inspect`。 |
| V0.3 | shutdown | `Runtime::shutdown()` 幂等更新 summary status。 |
| V0.4 | in-memory basic loop | `RuntimeBuilder::in_memory_v04()` 保留最小 EventBus -> Scheduler -> Agent -> LuaHost -> Capability 闭环。 |
| V0.5 | task diagnostics loop | `RuntimeBuilder::in_memory_v05()` 增加 task status/logs/cancel、timeout、retry、dead-letter replay 和 Lua generation marker。 |

## V0.5 Basic 闭环

`Runtime::run_basic(project, BasicRunOptions)` 仍使用同步 in-memory 路径，但报告内容升级为可诊断任务记录：

1. 构造 typed `Event`，写入 request id 和 generation id。
2. `InMemoryEventBus::publish` append 到 `InMemoryEventLog`，返回 `EventReceipt`。
3. runtime 从 `ProjectConfig.routes` 构造 `SubscriptionTable`。
4. scheduler 匹配 Topic 并投递到 `MailboxRegistry`。
5. runtime drain mailbox，把事件交给 `AgentRuntime` 私有 queue。
6. `AgentRuntime::run_next_with_control` 应用 timeout、cancel 和 retry 控制。
7. `LuaHost` 验证 sandbox，并解析受控 `on_event` 返回 table。
8. 如果 Lua result 请求 capability，runtime 通过 `CapabilityRouter` 调用 builtin。
9. EventBus ack/fail；失败事件写入 dead-letter，可选择生成 replay 证据。
10. 返回 `BasicRunReport`，其中包含 `TaskReport`、task logs、dead letters、replayed events、Lua generation 和 audit 摘要。

## 公开入口

```rust
use eva_runtime::{BasicRunOptions, RuntimeBuilder, TaskReport};
```

关键类型：

| 类型 | 用途 |
| --- | --- |
| `RuntimeBuilder::in_memory_v05()` | 构造 V0.5 summary，标记 task registry、dead-letter replay、hot-reload generation ready。 |
| `BasicRunOptions` | 配置 event id、request/task id、topic、payload、timeout、cancel、retry、dead-letter replay。 |
| `BasicRunReport` | CLI `run` 的完整机器可读报告。 |
| `TaskReport` | `task status/logs/cancel` 使用的状态、日志、取消、retry、dead-letter 摘要。 |

## V0.5 非目标

- 不启动后台 daemon，不承诺长生命周期任务调度。
- 不提供 durable crash recovery；CLI 只把最近一次 basic task report 写入 `.eva/tasks` 供后续命令读取。
- 不引入真实 Lua VM；`LuaGeneration` 是 generation marker，不是 VM swap 实现。
- Adapter/MCP/Discovery/Memory/Hardware/Backup/Lifecycle 仍属于后续版本。

## 验证

```powershell
cargo test -p eva-runtime
cargo run -- run --example basic --output json
cargo run -- run --example basic --timeout-ms 0 --replay-dead-letters --output json
```

已覆盖：V0.3 no-op summary、幂等 shutdown、V0.5 builder summary、basic 成功路径、missing route 错误路径、cancelled task、timeout task、dead-letter replay 报告。
