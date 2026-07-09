# eva-runtime/src

更新时间：2026-07-10

本目录承载 runtime 组合根实现。V1.0 在 V0.5 task diagnostics loop 上固定 release core 标识、quickstart 门禁和已知限制文档，但仍保持同步 in-memory 边界。

| 文件 | V0.5 状态 | 说明 |
| --- | --- | --- |
| `basic.rs` | 已更新 | V1.0 in-memory basic event loop；生成 `BasicRunReport`、`TaskReport`、Lua generation、dead-letter/replay 摘要。 |
| `recovery.rs` | V1.13.5 已更新 | `RuntimeRecoveryCoordinator`；扫描 durable task snapshots 和 provider process snapshots，把未完成 task 标记为 `interrupted` 或 `recovering` 并写回 task store；active provider session 在 restart recovery 中标记为 `interrupted`；带 durable EventBus 时只 redrive 未 ack、已到期且没有 replay 子事件的 dead-letter，并写入 recovery audit。 |
| `scheduler_retry.rs` | V1.12.4 已实现 | daemon live tick 的 durable dead-letter retry dispatch；按 `next_attempt_after_ms` 选择 due 事件，投递 scheduler mailbox，并用 `scheduler-retry` consumer ack/fail replay event。 |
| `daemon.rs` | V1.15.4 已更新 | 本机 foreground daemon、filesystem mailbox control loop、durable task submit/cancel、scheduler retry tick、provider execution-state recovery、`agent-control.state` drain/reload mutation state，以及 `hardware-hotplug.state` hotplug subscriber smoke。 |
| `task.rs` | 新增 | `TaskStatus`、`TaskReport`、`TaskLogEntry`、`CancellationRecord`、`RetryPolicy`、dead-letter/replay summary，并提供 `TaskReport` 到 `eva_storage::TaskStateSnapshot` 的持久化映射。 |
| `builder.rs` | 已更新 | `RuntimeMode::InMemoryV05`、`RuntimeOptions::in_memory_v05()`、`RuntimeBuilder::in_memory_v05()`。 |
| `runtime.rs` | 已更新 | `Runtime::run_basic` 委托给 `basic.rs`，保留 summary/shutdown 行为。 |
| `services.rs` | 已更新 | `RuntimeServices::in_memory_v05` 标记 task registry、dead-letter replay、hot-reload generation ready。 |
| `shutdown.rs` | 已实现 | 幂等 shutdown 状态记录。 |
| `lib.rs` | 已更新 | re-export V1.0 core 使用的 task 和 runtime 类型。 |

## V1.0 数据流

1. `BasicRunOptions` 控制 event、request/task id、timeout、cancel、retry、dead-letter replay。
2. `basic.rs` 运行 V1.0 in-memory loop。
3. `AgentRuntime::run_next_with_control` 产生 attempts/status/error。
4. runtime 将 agent run、dead-letter、replay 和 capability 结果聚合为 `TaskReport`。
5. `eva-cli` 通过 `eva-storage::FileSystemTaskStateStore` 将 `TaskReport` 写入 `.eva/tasks`，供 `task status/logs/cancel` 跨进程读取。

## 验证

```powershell
cargo test -p eva-runtime
```

当前测试覆盖成功、missing route、cancel、timeout、dead-letter replay、V0.5/V1.0 builder service summary、task report 状态映射、V1.6.4 recovery scanner、event redrive checkpoint、recovery audit、V1.12.4 scheduler retry tick、daemon retry smoke、V1.12.5 agent drain/reload mutation state、V1.13.5 provider interrupted/backoff recovery、daemon start provider recovery、V1.15.4 hotplug subscriber state 重启一致性、recovery/live retry 去重和 corrupt-store smoke。
