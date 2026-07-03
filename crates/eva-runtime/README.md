# eva-runtime / 运行时组合根

更新时间：2026-07-03

`eva-runtime` 是 Eva-CLI 的 composition root。下层 crate 不反向依赖 runtime，真实服务 wiring 和跨模块闭环都由本 crate 统一装配。

## 当前实现

| 版本 | 能力 | 当前行为 |
| --- | --- | --- |
| V0.3 | no-op builder | `RuntimeBuilder::new().build(project)` 构造只读 runtime summary，用于 `doctor` 和 `inspect`。 |
| V0.3 | shutdown | `Runtime::shutdown()` 幂等更新 summary status。 |
| V0.4 | in-memory builder | `RuntimeBuilder::in_memory_v04()` 构造 V0.4 service summary，storage/eventbus/scheduler/agent/lua/capability 标记为 ready。 |
| V0.4 | basic run | `Runtime::run_basic(project, BasicRunOptions)` 执行最小事件运行闭环。 |

## V0.4 Basic 闭环

`run_basic` 的顺序如下：

1. 构造 typed `Event`，写入 request id 和 generation id。
2. `InMemoryEventBus::publish` append 到 `InMemoryEventLog`，返回 `EventReceipt`。
3. runtime 从 `ProjectConfig.routes` 构造 `SubscriptionTable`。
4. scheduler 匹配 Topic 并投递到 `MailboxRegistry`。
5. runtime drain mailbox，把事件交给 `AgentRuntime` 私有 queue。
6. `AgentRuntime::run_next` 调用注入的 Lua handler。
7. `LuaHost` 解析受控 `on_event` 返回 table。
8. 如果 Lua result 请求 capability，runtime 通过 `CapabilityRouter` 调用 builtin。
9. EventBus ack/fail，并返回 `BasicRunReport`。

## 公开入口

```rust
use eva_runtime::{BasicRunOptions, RuntimeBuilder};
```

## 报告内容

`BasicRunReport` 包含 runtime mode、generation、project root、event id/topic、publish receipt、delivery plan、Agent run records、Lua results、capability response 和 audit 摘要。CLI 的 `eva run --example basic --output json` 会直接输出这些字段。

## 验证

```powershell
cargo test -p eva-runtime
cargo run -- run --example basic --output json
```

已覆盖：V0.3 no-op summary、幂等 shutdown、V0.4 basic 成功路径、missing route 失败路径。

## 后续限制

- V0.4 只装配 in-memory 服务，不提供 durable crash recovery。
- `RuntimeServices` 仍是 summary 容器，不长期持有后台线程或外部 provider handle。
- Adapter/MCP/Discovery/Memory/Hardware/Backup/Lifecycle 仍是 planned 服务。
- timeout、cancel、retry、task status 留给 V0.5。
