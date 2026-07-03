# eva-cli/src

更新时间：2026-07-03

| 文件 | V1.0 状态 | 说明 |
| --- | --- | --- |
| `run.rs` | 已更新 | 命令解析、formatter、exit code、`version`、`config validate`、`inspect`、V1.0 `run --example basic`、`task status/logs/cancel`、本地 `.eva/tasks` task state。 |
| `doctor.rs` | 已更新 | workspace/config/schema/runtime builder/Lua host 诊断。 |
| `inspect.rs` | V0.3 已实现 | 从 `ProjectConfig` 和 `RuntimeSummary` 构造综合 inspect report。 |
| `emit.rs` | 边界保留 | 后续 typed ingress event 命令。 |
| `agent.rs` | 边界保留 | 后续 Agent list/status/cancel 的更完整命令面。 |
| `adapter.rs` | 边界保留 | 后续 adapter list/probe。 |
| `capability.rs` | 边界保留 | 后续 capability list/inspect/dry-run invoke。 |
| `lib.rs` | 已实现 | 导出 CLI 顶层入口。 |

## V1.0 本地任务状态

`run.rs` 在 `eva run --example basic` 成功返回报告后，写入两类文件：

- `.eva/tasks/<task-id>.task`
- `.eva/tasks/latest-basic.task`

文件内容是稳定的行式 key/value 诊断格式，只服务 V1.0 CLI 本地读取；它不是公开持久化数据库格式。`task status/logs/cancel` 会读取这些文件并重新输出标准 text/JSON envelope。

## 验证

```powershell
cargo test -p eva-cli
```

当前测试覆盖 version text/JSON、basic run、task status/logs/cancel、cancelled run、config validate、inspect、doctor 和错误输出。
