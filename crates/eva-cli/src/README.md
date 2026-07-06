# eva-cli/src

更新时间：2026-07-04

本目录承载 CLI 命令解析、执行分发、文本/JSON 输出、exit code 映射和本地诊断文件读写。V1.5 仍把主要命令实现集中在 `run.rs`，这样 version、task、external capability、memory context、hardware、backup、lifecycle 和 release command 的 envelope 与错误映射保持一致。

## 文件职责

| 文件 | 当前状态 | 说明 |
| --- | --- | --- |
| `lib.rs` | 已实现 | 导出 CLI 顶层入口。 |
| `run.rs` | 已更新到 V1.5 | 命令解析、formatter、exit code、`version`、`config validate`、`inspect`、V1.0 `run --example basic`、`task status/logs/cancel`、V1.1 Adapter/MCP/Skill/Discovery、V1.2 `memory context`、V1.3 `hardware list/probe/bind`、V1.4 `backup create` / `snapshot create` / `restore plan` / `upgrade check`、V1.5 `release check` / `release security` / `release perf` / `release migration`。 |
| `doctor.rs` | 已更新 | workspace/config/schema/runtime builder/Lua host 诊断。 |
| `inspect.rs` | V0.3 已实现 | 从 `ProjectConfig` 和 `RuntimeSummary` 构造综合 inspect report。 |
| `emit.rs` | 边界保留 | 后续 typed ingress event 命令。 |
| `agent.rs` | 边界保留 | 后续 Agent list/status/cancel 的更完整命令面。 |
| `adapter.rs` | 边界保留 | 后续可从 `run.rs` 拆出 adapter 子命令。 |
| `capability.rs` | 边界保留 | 后续 capability list/inspect/dry-run invoke。 |

## V1.0 本地任务状态

`run.rs` 在 `eva run --example basic` 成功返回报告后，写入两类文件：

- `.eva/tasks/<task-id>.task`
- `.eva/tasks/latest-basic.task`

文件内容是稳定的行式 key/value 诊断格式，由 `eva-storage::FileSystemTaskStateStore` 读写；它不是公开持久化数据库格式。`task status/logs/cancel` 会读取这些文件并重新输出标准 text/JSON envelope。

## V1.1 External Capability Surface

`run.rs` 拥有第一版外部能力命令面：

- Parser branches for `adapter`, `mcp`, `skill`, and `discovery` subcommands.
- Execution bridges into `eva-adapter`, `eva-mcp`, and `eva-discovery` without adding persistent CLI state.
- Text and JSON writers for Adapter list/probe, MCP list/probe, Skill list/run, and Discovery scan.
- Provider invocation JSON, such as `skill run`, includes both `audit` and
  request-level `trace` inside `data`, while the top-level envelope keeps the
  CLI command span trace.
- Tests covering V1.1 JSON envelopes, blocked MCP tool probes, and V1.1 version identity.

## V1.2 Memory Context Surface

`run.rs` 实现 `eva memory context`。该命令加载项目配置，种子化一个 in-memory V1.2 context，调用 `eva_memory::ContextBuilder`，并输出与其他 CLI 命令一致的 JSON envelope。它是诊断 smoke，不是 durable memory persistence。

当前 V1.2 测试覆盖：

- V1.2 version identity and `context_v1.2` runtime marker.
- `memory.context` JSON output with private memory, global memory, knowledge, Lua context summary, and audit fields.
- Existing V1.1 external capability commands to prevent regressions.

## V1.3 Hardware Surface

`run.rs` 新增 `eva hardware list/probe/bind`：

- Parser branch: `parse_hardware_command` 分发 `list`、`probe`、`bind`。
- `hardware list`：加载项目配置并调用 `discover_project_devices`，输出 hardware candidates。
- `hardware probe --adapter <id>`：过滤单个 Adapter 的硬件候选；找不到时返回 `NotFound`。
- `hardware bind --adapter <id>`：生成 `HardwareBindPlan`，包含 candidate、status、apply 标记、plan steps 和 risks。
- `hardware bind --apply`：V1.3 仍不打开设备，只把 `apply_requested` 写入计划并验证逻辑边界。
- JSON writer 输出 `hardware.list`、`hardware.probe`、`hardware.bind` 三种 command envelope。
- Tests 覆盖 V1.3 version identity、硬件候选 JSON、bind plan JSON 和 blocked disabled manifest。

`scale-main` 默认 disabled，因此 `hardware bind` 返回 `status: blocked`。这是有意设计：V1.3 要证明 hardware Adapter 边界和 plan-first 体验，而不是在开发机上触发真实设备 I/O。

## V1.4 Backup And Lifecycle Surface

`run.rs` 新增 V1.4 plan-first 命令：

- `backup create`：调用 `eva_backup::BackupService`，默认写入 in-memory `ArtifactStore`；传入 `--artifact-store <path>` 时写入 filesystem artifact store，生成 manifest 并校验 digest。
- `snapshot create`：调用 `ReleaseSnapshotService` 生成 pre/post release snapshot，并可通过 `--artifact-store <path>` 落盘其依赖的 backup artifact。
- `restore plan`：调用 `ReleaseSnapshotService::restore_plan`，输出 `apply_allowed:false`，并可通过 `--artifact-store <path>` 生成 filesystem backup evidence。
- `upgrade check`：调用 `eva_lifecycle` 的 in-memory supervisor、generation、drain、rollback 状态机，并结合 migration preflight。

这些命令是 release/lifecycle readiness smoke，不执行真实文件恢复、release pointer 切换或 OS 进程启动。

## V1.5 Release Hardening Surface

`run.rs` 新增 V1.5 发布加固命令：

- `release check`：调用 `eva_release::ReleaseHardeningService::readiness`，输出跨平台、稳定性、文档、安全、性能和迁移门禁。
- `release security`：输出 security findings，覆盖 policy、Lua sandbox、secret redaction、MCP allowlist、hardware handle 和 lifecycle apply 风险。
- `release perf`：输出 release-smoke 性能预算，覆盖 EventBus、Scheduler、Adapter probe、memory context、backup 和 release check。
- `release migration`：输出 V1.4 -> V1.5 迁移步骤和兼容性策略。

这些命令共享 success/error JSON envelope、trace 字段和 exit code 映射。它们不写 `.eva/tasks`，不执行外部 provider，也不把 plan-first restore/upgrade 变成 apply。

## 保持集中实现的原因

V1.0 到 V1.5 的 CLI surface 仍处于收敛期。命令 implementations 暂时集中在 `run.rs`，可以让以下行为保持一致：

- success/error JSON envelope。
- trace 字段和 command 名称。
- exit code 映射。
- text output 的摘要风格。
- tests 对一处入口执行完整命令。

后续当命令形态稳定，可以把 adapter、memory、hardware、backup、lifecycle、release 子命令拆到独立文件，但拆分不能改变公开 JSON envelope。

## 验证

```powershell
cargo test -p eva-cli
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- upgrade check --output json
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
```
