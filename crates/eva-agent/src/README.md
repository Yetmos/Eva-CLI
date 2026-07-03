# eva-agent/src

更新时间：2026-07-03

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `lifecycle.rs` | 已实现 | `AgentLifecycle`、`AgentLifecycleState`，管理 created/running/draining/stopped/failed。 |
| `queue.rs` | 已实现 | `AgentQueue` bounded FIFO。 |
| `runtime.rs` | 已实现 | `AgentRuntime`、`AgentHandlerOutput`、`AgentRunRecord`、`AgentRunStatus`。 |
| `state.rs` | 已实现 | `AgentStateSnapshot`，为后续 status/report 预留。 |
| `lib.rs` | 已实现 | re-export V0.4 公开类型。 |

验证：`cargo test -p eva-agent`。
