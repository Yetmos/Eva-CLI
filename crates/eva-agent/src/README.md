# eva-agent/src

更新时间：2026-07-03

| 文件 | V0.5 状态 | 说明 |
| --- | --- | --- |
| `lifecycle.rs` | 已实现 | `AgentLifecycle`、`AgentLifecycleState`，管理 created/running/draining/stopped/failed。 |
| `queue.rs` | 已实现 | `AgentQueue` bounded FIFO。 |
| `runtime.rs` | 已更新 | `AgentRuntime`、`AgentRunControl`、`AgentHandlerOutput`、`AgentRunRecord`、`AgentRunStatus`。 |
| `state.rs` | 已实现 | `AgentStateSnapshot`，为后续 status/report 预留。 |
| `lib.rs` | 已更新 | re-export V0.5 公开类型。 |

## V0.5 控制点

`runtime.rs` 是 V0.5 的关键文件：

- `run_next` 保留 V0.4 默认单次执行行为。
- `run_next_with_control` 加入 timeout/cancel/retry 控制。
- `AgentRunRecord.attempts` 为 CLI task report 提供重试证据。
- `AgentRunStatus` 新增 `cancelled` 和 `timed_out`。

验证：`cargo test -p eva-agent`。
