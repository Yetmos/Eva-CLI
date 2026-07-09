# eva-runtime / 运行时组合根

更新时间：2026-07-09

`eva-runtime` 是 Eva-CLI 的 composition root。下层 crate 不反向依赖 runtime；跨模块服务装配、运行闭环、V0.5 任务诊断和 V1.0 core 发布标识都由本 crate 统一组合。

## 当前实现

| 版本 | 能力 | 当前行为 |
| --- | --- | --- |
| V0.3 | no-op builder | `RuntimeBuilder::new().build(project)` 构造只读 runtime summary，用于 `doctor` 和 `inspect`。 |
| V0.3 | shutdown | `Runtime::shutdown()` 幂等更新 summary status。 |
| V0.4 | in-memory basic loop | `RuntimeBuilder::in_memory_v04()` 保留最小 EventBus -> Scheduler -> Agent -> LuaHost -> Capability 闭环。 |
| V0.5 | task diagnostics loop | `RuntimeBuilder::in_memory_v05()` 增加 task status/logs/cancel、timeout、retry、dead-letter replay 和 Lua generation marker。 |
| V1.0 | core release loop | `RuntimeBuilder::in_memory_v10()` 复用 V0.5 diagnostics，并将 runtime mode/generation 固定为 `in_memory_v1.0` / `basic-v1.0`。 |
| V1.6.4 | recovery checkpoint | `RuntimeRecoveryCoordinator` 扫描 durable task snapshots，把重启后残留的 `queued`/`running` task 标记为 `interrupted` 或 `recovering`；带 eventbus 的 checkpoint 可 redrive 未 ack 且已到期的 durable dead-letter，并把 recovery evidence 写入 durable audit。 |
| V1.12.1 | daemon process boundary | `start_daemon` / `daemon_status` / `stop_daemon` 固定本机 daemon pid/lock/state、foreground/dev smoke、durable backend、policy、observability 和 shutdown contract；不启动 provider 进程。 |
| V1.12.2 | daemon control mailbox | `send_daemon_control_request` 和 foreground control loop 定义受控 filesystem mailbox 协议，支持 status、shutdown、submit task、cancel task、drain 和 reload plan；request/response 均带 trace id，不暴露远程网络监听。 |
| V1.12.3 | durable task lifecycle | daemon submit/cancel 使用 `TaskStateSnapshot` lifecycle API：submit 写 `queued`，cancel 将非终态任务推进到 `cancelling` 并追加日志；recovery 会把 `queued`/`running`/`cancelling` 恢复为 `interrupted` 或 `recovering`。 |

## V1.0 Basic 闭环

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
use eva_runtime::{BasicRunOptions, DaemonControlRequest, DaemonStartOptions, RuntimeBuilder, TaskReport};
```

关键类型：

| 类型 | 用途 |
| --- | --- |
| `RuntimeBuilder::in_memory_v05()` | 构造 V0.5 summary，标记 task registry、dead-letter replay、hot-reload generation ready。 |
| `RuntimeBuilder::in_memory_v10()` | 构造 V1.0 core summary，增加 release core 和 advanced capability planned 标记。 |
| `BasicRunOptions` | 配置 event id、request/task id、topic、payload、timeout、cancel、retry、dead-letter replay。 |
| `BasicRunReport` | CLI `run` 的完整机器可读报告。 |
| `TaskReport` | `task status/logs/cancel` 使用的状态、日志、取消、retry、dead-letter 摘要。 |
| `RuntimeRecoveryCoordinator` | V1.6.4 recovery coordinator；读取 task snapshots，持久化 interrupted/recovering 状态，可通过 durable EventBus 执行受控 redrive checkpoint，并可记录 `runtime.recovered` audit。 |
| `DaemonStartOptions` | V1.12.1 daemon foreground/dev smoke 的 durable backend、state、lock、pid 和 observability 路径配置。 |
| `DaemonControlRequest` | V1.12.2 本机 control mailbox 请求；封装 request id、trace id、operation、task/plan/generation 参数。 |

## V1.12 Daemon Boundary And Control Mailbox

`eva-runtime::daemon` 提供本机 daemon 进程边界 smoke，而不是生产后台守护进程：

- `start_daemon` 先获取 `daemon.lock`，再验证 durable backend、policy domain 和 file JSONL observability backend。
- 成功后写入 `daemon.state` 和 `daemon.pid`，foreground smoke 会立即调用 `Runtime::shutdown()` 并移除 lock/pid。
- 显式传入 `shutdown_after_smoke=false` 时进入前台 control loop，通过 `state/control/requests` 和 `state/control/responses` 处理本机 filesystem mailbox 请求。
- control operation 覆盖 status、shutdown、submit task、cancel task、drain 和 reload plan；status/shutdown 作用于前台 daemon，submit/cancel 写 durable task lifecycle store，drain/reload 会写入 `agent-control.state`，记录 drain gate、reload generation route 和旧 generation draining 状态。
- `send_daemon_control_request` 在没有 running state、lock 和 pid 时返回稳定 `Unavailable`，避免把 stopped smoke state 误读成 live daemon。
- JSON/report 中固定输出 `provider_processes_started:false`，避免把边界 smoke 误读成 provider supervision。
- 已有 lock 会返回 conflict；坏 durable backend 会在写 daemon state 前失败。

## V1.6.4 Recovery Checkpoint

`RuntimeRecoveryCoordinator::recover_task_store` 使用
`eva-storage::FileSystemTaskStateStore::list_snapshots()` 枚举 task snapshots。
task-only 入口只负责确定性状态修复：

- `queued` / `running` / `cancelling` 且无 dead-letter 证据的 task 标记为 `interrupted`。
- `queued` / `running` / `cancelling` 且已有 dead-letter 证据的 task 标记为 `recovering`。
- terminal task 不会被重写，避免重复处理已完成、失败、取消或超时的任务。

`RuntimeRecoveryCoordinator::recover_task_store_with_redrive` 额外接入
`eva-eventbus::DurableEventBus`：

- 只 redrive 同时存在 task dead-letter、durable dead-letter record 和 durable event log record 的 event。
- 原始 event 已 `acked` 时跳过，避免重复执行。
- `next_attempt_after_ms` 大于 checkpoint 的 `redrive_ready_at_ms` 时跳过，保留 backoff 证据。
- redrive 成功后写回 task snapshot 的 `replayed_events`，并在 report 中记录 redriven/skipped 证据。

`recover_task_store_with_audit` 和
`recover_task_store_with_redrive_and_audit` 会把 scanned/recovered/redriven/skipped
计数写入 `AuditAction::RuntimeRecovered`。V1.6.4 smoke 覆盖 clean start、
restart redrive 和 corrupt task store，`release check` 暴露
`REL-DURABLE-RECOVERY-001`。

## 当前非目标

- V1.12.5 只提供本机 filesystem mailbox 控制面、前台 loop、scheduler retry tick 和 agent drain/reload mutation state；不提供生产后台 service manager、远程网络监听、provider supervision 或完整生产 scheduler apply。
- recovery checkpoint 只恢复 task/event/audit evidence，不恢复 provider/runtime 执行态；CLI 仍会把最近一次 basic task report 写入 `.eva/tasks` 供后续命令读取。
- 不引入真实 Lua VM；`LuaGeneration` 是 generation marker，不是 VM swap 实现。
- Adapter/MCP/Discovery/Memory/Hardware/Backup/Lifecycle 仍属于后续版本。

## 验证

```powershell
cargo test -p eva-runtime
cargo test -p eva-runtime daemon -- --nocapture
cargo test -p eva-runtime recovery -- --nocapture
cargo run -- run --example basic --output json
cargo run -- run --example basic --timeout-ms 0 --replay-dead-letters --output json
```

已覆盖：V0.3 no-op summary、幂等 shutdown、V0.5/V1.0 builder summary、basic 成功路径、missing route 错误路径、cancelled task、timeout task、dead-letter replay 报告，以及 V1.6.4 recovery scanner、event redrive checkpoint、recovery audit 和 corrupt-store smoke。
