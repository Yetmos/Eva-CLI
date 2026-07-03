# eva-cli/src

更新时间：2026-07-03

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `run.rs` | 已更新 | 命令解析、formatter、exit code、`config validate`、`inspect`、`run --example basic`。 |
| `doctor.rs` | V0.3 已实现 | workspace/config/schema/runtime builder 诊断。 |
| `inspect.rs` | V0.3 已实现 | 从 `ProjectConfig` 和 `RuntimeSummary` 构造综合 inspect report。 |
| `emit.rs` | 边界保留 | 后续 typed ingress event 命令。 |
| `agent.rs` | 边界保留 | 后续 list/status/cancel。 |
| `adapter.rs` | 边界保留 | 后续 adapter list/probe。 |
| `capability.rs` | 边界保留 | 后续 capability list/inspect/dry-run invoke。 |
| `lib.rs` | 已实现 | 导出 CLI 顶层入口。 |

验证：`cargo test -p eva-cli`。
