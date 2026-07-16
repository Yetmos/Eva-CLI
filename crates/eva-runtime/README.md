# eva-runtime / 运行时组合根

更新时间：2026-07-16

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
| W1-L02 | persisted TaskEnvelope | `TaskEnvelope` 固定 kind、Agent、inline bytes/artifact ref、idempotency key 和 attempt policy；daemon mailbox v2 无损传递，task state v3 跨重开恢复，legacy mailbox v1 在 mutation/observability 前显式映射为 `legacy.submit`；inline Debug 全链路脱敏。 |
| W1-L05 | task handler registry | `TaskHandlerRegistry` 以稳定 kind 注册同步 handler；dispatch 在调用前重验 inline/artifact 原始字节与摘要，unknown kind 固定失败且不访问 artifact，默认只注册无副作用 `runtime.echo`，`legacy.submit` 不可执行。 |
| W1-L06 | durable task worker | `TaskWorkerRuntime` 在 ready gate 后扫描 queued task，以 task-state v4 CAS 绑定 daemon owner、attempt、deadline 和 cancel token，再调用 registry 并 fenced 提交 completed/failed/timed_out/cancelled；双 worker 竞争只有一个 handler 调用，shutdown 在发布 stopped 前先关闭 claim。 |
| W1-L07 | heartbeat and liveness | worker 为每个同步 handler attempt 启动可唤醒 heartbeat loop，每次续租都复验 writer generation、owner、attempt 和 cancel token；task/daemon status 从同一 lease 证据派生 live/degraded/stale 与 heartbeat age，ownerless running 记录保守视为 stale。 |
| V1.13.5 | provider execution recovery | daemon start 扫描 durable provider process table 和 task store；残留 active provider session 标记为 `interrupted`，关联 task 保留为 interrupted/recovering，只有显式 retryable restart policy 才生成 scheduler backoff 证据。 |
| V1.15.4 | hardware hotplug subscriber | daemon start 运行 manifest snapshot hotplug subscriber，把逻辑设备状态写入 durable EventBus 和 `hardware-hotplug.state`，并在 report 中输出 `raw_handles_exposed:false`。 |
| V1.15.6 | memory/knowledge maintenance smoke | daemon start 对 durable memory/knowledge store 执行一次 `index.lock` 保护的 TTL GC 和 knowledge rebuild checkpoint，输出 `memory_maintenance` report，并写 `memory.maintenance` audit。 |
| V1.16.1 | runtime audit sink wiring | daemon recovery/control、submit/cancel task lifecycle 和 scheduler retry tick 会 best-effort 写入 JSONL audit/metrics/span；sink 失败不阻断 daemon 主流程。 |

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
use eva_runtime::{BasicRunOptions, DaemonControlRequest, DaemonStartOptions, RuntimeBuilder, TaskEnvelope, TaskHandlerRegistry, TaskReport, TaskWorkerRuntime};
```

关键类型：

| 类型 | 用途 |
| --- | --- |
| `RuntimeBuilder::in_memory_v05()` | 构造 V0.5 summary，标记 task registry、dead-letter replay、hot-reload generation ready。 |
| `RuntimeBuilder::in_memory_v10()` | 构造兼容 V1.0 basic loop 的 legacy summary；其中 advanced capability 标记描述该运行模式，不代表整个 workspace 的当前能力状态。 |
| `BasicRunOptions` | 配置 event id、request/task id、topic、payload、timeout、cancel、retry、dead-letter replay。 |
| `BasicRunReport` | CLI `run` 的完整机器可读报告。 |
| `TaskReport` | `task status/logs/cancel` 使用的状态、日志、取消、retry、dead-letter 摘要。 |
| `TaskHandlerRegistry` | daemon-owned task kind→handler 映射；只在 payload 完整性重验和 handler 成功后返回 `TaskHandlerResult`，worker lifecycle/CAS 由 W1-L06 承接。 |
| `TaskWorkerRuntime` | daemon-owned 单 worker 线程；ready 前暂停 claim，运行中周期续租 fenced heartbeat、把 durable cancel 传播为只读 view、隔离 handler panic，并在 daemon lease 释放前 stop/join。 |
| `RuntimeRecoveryCoordinator` | V1.6.4/V1.13.5 recovery coordinator；读取 task snapshots 和 provider process snapshots，持久化 interrupted/recovering 状态，可通过 durable EventBus 执行受控 redrive checkpoint，并可记录 `runtime.recovered` audit。 |
| `DaemonStartOptions` | V1.12.1 daemon foreground/dev smoke 的 durable backend、state、lock、pid 和 observability 路径配置。 |
| `DaemonMemoryMaintenanceReport` | V1.15.6 daemon start 中 memory TTL GC 与 knowledge rebuild checkpoint 的维护证据。 |
| `DaemonControlRequest` | 本机 control mailbox 请求；v2 submit 封装完整强类型 TaskEnvelope，reader 继续兼容 v1，其他 operation 保留 task/plan/generation 参数。 |

## V1.12 Daemon Boundary And Control Mailbox

`eva-runtime::daemon` 提供本机 daemon 进程边界 smoke，而不是生产后台守护进程：

- `start_daemon` 在固定且永不替换的 `daemon.lock` 上获取 OS lock，并原子发布含 PID、process token、writer generation、heartbeat 和 expiry 的 `daemon.lease`，再扫描 durable task/provider process recovery state、policy domain 和 file JSONL observability backend。
- 成功后写入 `daemon.state` 和带完整 lease identity 的 `daemon.pid`；foreground smoke 会立即调用 `Runtime::shutdown()`，删除 PID、将 lease 标为 `released`，但永久保留未持锁的 `daemon.lock` anchor。
- 显式传入 `shutdown_after_smoke=false` 时进入前台 control loop，通过 `state/control/requests` 和 `state/control/responses` 处理本机 filesystem mailbox 请求。
- control operation 覆盖 status、shutdown、submit task、cancel task、drain 和 reload plan；status/shutdown 作用于前台 daemon，submit/cancel 写 durable task lifecycle store，drain/reload 会写入 `agent-control.state`，记录 drain gate、reload generation route 和旧 generation draining 状态。
- submit v2 请求必须携带完整 TaskEnvelope；envelope 是唯一 submit Agent 身份，daemon 会拒绝通用 control Agent 分叉并再次确认 Agent 当前存在且 enabled。reader 在读取前拒绝 symlink/directory 等非普通 request；损坏摘要、Agent 分叉、未知/disabled Agent 等 poison request 会先改名移出 pending，再通过同步临时摘要、安全删除原目录项和发布 rejected marker 的顺序隔离，不会把原 inline payload 搬入 rejected 记录，也不会结束 control loop。
- 长驻 daemon 在 ready 前创建暂停的 task worker，ready 后才允许 queued→running claim；claim/heartbeat/finish/cancel 在最新 record version 上合并，terminal outcome 元数据不可由普通 CAS 改写，CLI status 输出 owner、freshness、heartbeat age 和 result 摘要但不输出 cancel token。
- `send_daemon_control_request` 只有在 running state、版本化 PID projection、fresh active lease 与 live OS-lock owner 完整一致时才可用，避免 stale state、PID reuse 或 stopped smoke 被误读成 live daemon；status 的 text/JSON lease 同时报告 live/degraded/stale 与 heartbeat age，unavailable 错误保留 stale freshness context。
- JSON/report 中固定输出 `provider_processes_started:false`，避免把边界 smoke 误读成 provider supervision。
- JSON/report 中新增 `recovery` 对象，包含 scanned/recovered task、provider process、backoff 和 skipped evidence。
- JSON/report 中新增 `hardware_hotplug` 对象，包含 watcher kind、published typed events、`hardware-hotplug.state` 和 `raw_handles_exposed:false` evidence。
- JSON/report 中新增 `memory_maintenance` 对象，包含 `memory_gc`、`knowledge_rebuild`、checkpoint path、stale checkpoint recovery 和 `memory.maintenance` audit evidence。
- V1.16.1 后，daemon recovery/control、submit/cancel task lifecycle 和 scheduler retry tick 通过 `BestEffortObservabilityPipeline` 写入现有 JSONL audit/metrics/span；不可写 backend 只记录 degraded evidence，不改变 control response。
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

## V1.13.5 Provider Execution Recovery

`RuntimeRecoveryCoordinator::recover_task_store_with_provider_processes()` 同时读取
`FileSystemTaskStateStore` 和 `ProviderProcessTable`。恢复语义保持保守：

- active provider session 会被标记为 inactive + `health=interrupted`，并追加 recovery audit；
- 关联 non-terminal task 会保留 durable task snapshot，不会丢失 task id、日志或原始状态证据；
- 默认不会重放 provider 调用，避免重复外部副作用；
- 只有 provider snapshot 的 `restart_policy` 明确为 `scheduler_backoff` / `retry_backoff` / `retryable` 且 retry 预算未耗尽时，task 才会进入 `recovering` 并在 report 中写入 `provider_backoff_tasks`；
- daemon start 会把该 report 暴露到 `DaemonStartReport.recovery` 和 CLI JSON `recovery` 字段。

## 当前非目标

- 当前 daemon 路径提供本机 filesystem mailbox 控制面、前台 loop、scheduler retry tick、agent drain/reload state、provider execution-state recovery、manifest snapshot hotplug subscriber、一次性 memory/knowledge maintenance 和 best-effort observability wiring；不提供生产后台 service-manager 集成、远程网络监听、OS provider process supervisor、真实 OS hotplug watcher、长驻 memory scheduler、生产级 OTel/数据库 sink 或完整 scheduler apply。
- recovery checkpoint 已恢复 task/event/audit evidence 和 durable provider process snapshots，但不会重启或杀死真实 OS provider 进程；CLI 仍会把最近一次 basic task report 写入 `.eva/tasks` 供后续命令读取。
- W1-L07 worker 只保证当前 live writer 下的 fenced heartbeat 与竞争 worker 不重复 claim；启动 recovery 仍会把跨 generation 的旧 queued/running/cancelling 快照保守标记为 interrupted/recovering。retry/ACK、effect ledger、基于 stale/effect 的 crash backlog 重排和有 deadline 的 shutdown drain 分别留给 W1-L08 至 W1-L11。
- Lua 执行已使用受限真实 VM，并具备 host binding、资源限制和 generation lifecycle；当前 daemon reload 记录 generation route/drain 状态，但不等同于生产级进程内 VM 热替换。
- Adapter、MCP、Discovery、Memory、Hardware、Backup 和 Lifecycle 已有受控 1.x 实现并由 CLI/runtime 按场景组合；真实 OS provider supervision、raw hardware I/O 和生产 service-manager handoff 仍在边界之外。

## 验证

```powershell
cargo test -p eva-runtime
cargo test -p eva-runtime daemon -- --nocapture
cargo test -p eva-runtime recovery -- --nocapture
cargo run -- run --example basic --output json
cargo run -- run --example basic --timeout-ms 0 --replay-dead-letters --output json
```

已覆盖：V0.3 no-op summary、幂等 shutdown、V0.5/V1.0 builder summary、basic 成功路径、missing route 错误路径、cancelled task、timeout task、dead-letter replay 报告，以及 V1.6.4 recovery scanner、event redrive checkpoint、recovery audit、corrupt-store smoke、V1.13.5 provider interrupted/backoff recovery、daemon start provider recovery smoke、V1.15.4 hotplug subscriber state 重启一致性、V1.15.6 memory/knowledge maintenance smoke、V1.16.1 daemon control/task/scheduler retry observability smoke，以及 W1-L02 mailbox v1/v2、TaskEnvelope 停机重开、poison request 隔离、W1-L06 双 worker claim/ready gate/panic/timeout/cancel 终态/shutdown claim gate/真实 daemon echo 执行，以及 W1-L07 heartbeat 与 cancel/finish CAS 竞争、活 worker 续租、竞争 worker 不抢占、freshness 阈值和 CLI live/stale/not-applicable 输出。
