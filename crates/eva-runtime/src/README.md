# eva-runtime/src

更新时间：2026-07-03

本目录承载 runtime 组合根实现。V0.5 在 V0.4 basic loop 上增加任务诊断、失败路径和 generation 标记，但仍保持同步 in-memory 边界。

| 文件 | V0.5 状态 | 说明 |
| --- | --- | --- |
| `basic.rs` | 已更新 | V0.5 in-memory basic event loop；生成 `BasicRunReport`、`TaskReport`、Lua generation、dead-letter/replay 摘要。 |
| `task.rs` | 新增 | `TaskStatus`、`TaskReport`、`TaskLogEntry`、`CancellationRecord`、`RetryPolicy`、dead-letter/replay summary。 |
| `builder.rs` | 已更新 | `RuntimeMode::InMemoryV05`、`RuntimeOptions::in_memory_v05()`、`RuntimeBuilder::in_memory_v05()`。 |
| `runtime.rs` | 已更新 | `Runtime::run_basic` 委托给 `basic.rs`，保留 summary/shutdown 行为。 |
| `services.rs` | 已更新 | `RuntimeServices::in_memory_v05` 标记 task registry、dead-letter replay、hot-reload generation ready。 |
| `shutdown.rs` | 已实现 | 幂等 shutdown 状态记录。 |
| `lib.rs` | 已更新 | re-export V0.5 task 和 runtime 类型。 |

## V0.5 数据流

1. `BasicRunOptions` 控制 event、request/task id、timeout、cancel、retry、dead-letter replay。
2. `basic.rs` 运行 V0.5 in-memory loop。
3. `AgentRuntime::run_next_with_control` 产生 attempts/status/error。
4. runtime 将 agent run、dead-letter、replay 和 capability 结果聚合为 `TaskReport`。
5. `eva-cli` 将 `TaskReport` 写入 `.eva/tasks`，供 `task status/logs/cancel` 读取。

## 验证

```powershell
cargo test -p eva-runtime
```

当前测试覆盖成功、missing route、cancel、timeout、dead-letter replay、V0.5 builder service summary 和 task report 状态映射。
