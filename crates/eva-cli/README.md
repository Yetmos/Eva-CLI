# eva-cli / 命令行入口

更新时间：2026-07-07

`eva-cli` 负责命令解析、文本/JSON 输出、trace 字段和 exit code 映射。当前命令面覆盖 V1.0 core runtime、V1.1 外部能力诊断、V1.2 request-scoped memory/knowledge context、V1.3 plan-first 硬件接入诊断、V1.4 backup/lifecycle planning、V1.5 release hardening，以及 V1.6 durable task store 入口。

CLI 不启动后台 daemon；`run --example basic` 同步执行后，默认把最新 task report 写入 `.eva/tasks`，供 `task status/logs/cancel` 跨命令读取。传入 `--durable-backend <path>` 时，run/task 命令改用 V1.6 durable backend 的 `tasks/` 目录。外部能力、记忆上下文、硬件、备份、生命周期和发布加固命令当前都以可验证诊断 surface 为主，不打开真实 stdio/http/MCP server、raw hardware I/O，也不执行 destructive restore 或真实进程升级。

## 当前命令

| 命令 | 当前行为 |
| --- | --- |
| `eva version` / `eva --version` | 输出 V1.5 release label、runtime contract 和稳定命令契约。 |
| `eva doctor` | 检查 workspace、配置根、schema、Lua host crate 边界、runtime builder。 |
| `eva config validate` | 加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy、routes，并输出摘要。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 runtime service summary。 |
| `eva run --example basic` | 执行 V1.0 in-memory basic event loop，并写入本地或 durable backend task report。 |
| `eva task status` | 读取 `.eva/tasks` 或 `--durable-backend` 中最新或指定 task 的状态、attempts、retry、取消和 dead-letter 摘要。 |
| `eva task logs` | 读取 task logs。 |
| `eva task cancel` | 对未终态 task 写入取消标记；对已终态 task 记录 cancel request 但不改变终态。 |
| `eva adapter list/probe` | 列出和 probe 已授权 Adapter handle，不启动真实 provider。 |
| `eva mcp list/probe` | 列出和 probe MCP allowlist tool，不启动真实 MCP server。 |
| `eva skill list/run` | 返回受控 workflow skill envelope，不启动 Codex/OMX workflow runner。 |
| `eva discovery scan` | 返回 discovery candidate，明确 discovery 不授予 runtime handle。 |
| `eva memory context` | 为单个 Agent 构造 V1.2 request context，输出 private/global memory、knowledge、Lua snapshot 和 audit。 |
| `eva hardware list/probe/bind` | 发现、probe 并计划硬件绑定；高风险动作 plan-first，V1.3 不打开 raw I/O。 |
| `eva backup create` | 创建并校验 V1.4 in-memory backup artifact。 |
| `eva snapshot create` | 创建绑定到 backup manifest 的 release snapshot。 |
| `eva restore plan` | 生成 restore plan；V1.4 保持 `apply_allowed:false`。 |
| `eva restore apply` | 解析 `--plan`、`--confirm` 和 `--artifact-store`，但在 P6 apply gates 完成前返回 `unsupported`，不执行破坏性恢复。 |
| `eva upgrade check` | 诊断 generation、migration、drain、rollback readiness，不启动真实进程。 |
| `eva release check` | 聚合跨平台、稳定性、文档、安全、性能和迁移门禁，输出 V1.5 release readiness。 |
| `eva release security` | 输出 policy、sandbox、secret、MCP、hardware 和 lifecycle apply 风险的安全评审。 |
| `eva release perf` | 输出 EventBus、Scheduler、Adapter、memory、backup 和 release check 的性能预算基线。 |
| `eva release migration` | 输出 V1.4 -> V1.5 迁移步骤和兼容性策略。 |

## 输出契约

成功 JSON 使用统一 envelope：`ok`、`command`、`exit_code`、`data`、`trace`。错误 JSON 使用：`ok`、`command`、`exit_code`、`error`、`trace`。外部能力 invoke 的 `data` 内也会包含 request-level `trace`，用于把 provider audit 和请求链路关联起来。文本模式保持人类可读摘要。

## Exit Code

| Code | 含义 |
| --- | --- |
| `0` | 成功。 |
| `1` | 内部错误。 |
| `2` | 配置、路径、manifest、route、schema 或 task state 问题。 |
| `3` | policy 拒绝。 |
| `4` | runtime 当前不可用或能力未实现。 |
| `5` | 预留给外部 capability unavailable。 |
| `64` | 命令用法错误。 |

## `eva run --example basic`

示例命令：

```powershell
cargo run -- run --example basic --output json
cargo run -- run --example basic --timeout-ms 0 --replay-dead-letters --output json
cargo run -- run --example basic --cancel --output json
cargo run -- run --example basic --task-id req-durable-1 --durable-backend .eva/durable --output json
```

常用选项：

| 选项 | 含义 |
| --- | --- |
| `--task-id <id>` | 指定 request/task id；默认 `req-basic-1`。 |
| `--durable-backend <path>` | 使用 durable backend 的 `tasks/` 目录，而不是 `<project>/.eva/tasks`。 |
| `--timeout-ms <ms>` | 设置 Agent handler timeout budget；`0` 会触发 timeout 诊断路径。 |
| `--retry-attempts <n>` | 设置 retry 上限；当前 basic handler 只在结构化 retryable 错误时重试。 |
| `--cancel` | 在 handler 前模拟取消请求，生成 cancelled task。 |
| `--replay-dead-letters` | 对当前 run 的 dead-letter 事件生成 replay receipt 摘要。 |

## `eva task ...`

`task` 命令默认读取 `<project>/.eva/tasks`；传入 `--durable-backend <path>` 时读取该 backend 的 `tasks/`：

```powershell
cargo run -- task status --output json
cargo run -- task logs --task req-basic-1 --output json
cargo run -- task cancel --task req-basic-1 --reason "manual stop" --output json
cargo run -- task status --task req-durable-1 --durable-backend .eva/durable --output json
```

未传 `--durable-backend` 时，V1.0 的 task 记录仍是本地诊断文件，不是 durable task database。目录已在 `.gitignore` 中排除。

## V1.1 External Capability Commands

V1.1 增加外部能力生态命令，同时保留 V1.0 basic runtime 路径：

- `eva adapter list`：列出从项目 manifest 派生的授权 Adapter handle。
- `eva adapter probe --adapter <id>`：返回 side-effect-free health、transport 和 capability 详情。
- `eva adapter probe --capability <name> [--provider <id>]`：验证 AdapterRouter 选择，不调用外部 provider。
- `eva mcp list`：列出 MCP transport adapters 和 tool allowlist。
- `eva mcp probe --adapter <id> --tool <name>`：报告 tool 是否在 allowlist 中；blocked probe 也返回诊断 envelope，因为未调用 provider。
- `eva skill list`：列出受控 workflow skill adapters 和 runtime gates。
- `eva skill run --skill <id> --input <json>`：返回受控 Skill envelope 和 audit trail，不启动 workflow execution。
- `eva discovery scan`：返回 trusted candidates，并用 `handle_granted=false` 证明 discovery 不是授权。

## V1.2 Memory Context Command

`eva memory context` 是 V1.2 的 `eva-memory` 和 `eva-lua-host` smoke：

```powershell
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
```

| Option | Meaning |
| --- | --- |
| `--agent <id>` | Agent whose private memory may enter the context. Defaults to `root-agent`. |
| `--query <text>` | Knowledge search query. Defaults to `memory`. |
| `--request-id <id>` | Request id attached to seeded context records. Defaults to `req-memory-1`. |
| `--private-limit <n>` | Maximum private memory records returned. |
| `--global-limit <n>` | Maximum global memory records returned. |
| `--knowledge-limit <n>` | Maximum knowledge search results returned. |

JSON output contains `memory`, `global_memory`, `knowledge`, `lua_context`, and `audit`. The command uses an in-memory demonstration context derived from project configuration; durable memory storage remains later scope.

## V1.3 Hardware Commands

V1.3 新增 `hardware list/probe/bind`，用于诊断 hardware Adapter manifest 和生成绑定计划：

```powershell
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --apply --output json
```

| 命令 | 行为 |
| --- | --- |
| `hardware list` | 加载项目配置，调用 `eva_hardware::discover_project_devices`，输出所有 hardware candidates。 |
| `hardware probe --adapter <id>` | 过滤单个 Adapter 的候选，仍不授予 handle。 |
| `hardware bind --adapter <id>` | 生成绑定计划、风险提示和 plan steps；disabled/rejected 设备返回 `blocked`。 |
| `hardware bind --apply` | V1.3 只校验逻辑计划并保留 plan-first 输出，不打开真实设备。 |

`scale-main` 默认 disabled，因此 JSON 中会看到：

- `trust: "rejected"`
- `health: "disconnected"`
- `handle_granted: false`
- `rejected_reason: "hardware adapter manifest is disabled"`
- `hardware bind` 的 `status: "blocked"`

这条命令面验证硬件身份、发现、绑定计划和风险提示，但不执行 USB、串口、BLE、网络或 vendor SDK I/O。

## V1.4 Backup And Lifecycle Commands

V1.4 新增备份、snapshot、restore plan 和 upgrade check：

```powershell
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- restore apply --plan restore-plan.json --confirm plan-123 --artifact-store .eva/artifacts --output json
cargo run -- backup create --artifact-store .eva/artifacts --output json
cargo run -- snapshot promote --snapshot-id snapshot-v15 --confirm snapshot-v15 --artifact-store .eva/artifacts --output json
cargo run -- upgrade check --output json
cargo run -- upgrade apply --plan upgrade-plan.txt --confirm plan-upgrade --lock-store .eva/locks --output json
```

| 命令 | 行为 |
| --- | --- |
| `backup create` | 构造 `BackupPlan`，默认写入 in-memory `ArtifactStore`；传入 `--artifact-store <path>` 时写入 filesystem artifact store，生成 `BackupManifest`，并立即校验 digest。 |
| `snapshot create` | 创建 pre/post release snapshot，并关联已验证 backup artifact；传入 `--artifact-store <path>` 时同步落盘 snapshot 依赖的 backup artifact。 |
| `restore plan` | 输出 restore steps、risks、audit，且 `apply_allowed:false`；传入 `--artifact-store <path>` 时使用同一 filesystem artifact store 生成可追溯 backup evidence。 |
| `restore apply` | P6-001 默认拒绝执行；P6-002 在 `--dry-run` 下读取 plan 文件并验证 filesystem artifact store 中的 backup digest，仍保持 `apply_allowed:false`。 |
| `upgrade check` | 输出 supervisor candidate、migration preflight、drain plan 和 rollback plan。 |

`restore apply --dry-run` 的 plan 文件使用稳定 key/value 格式：

```text
plan_id=plan-123
backup_artifact_id=backup-for-snapshot-v14
backup_digest=sha256:<hex>
```

V1.4/P6 dry-run 不执行 destructive restore，不移动 release pointer，不启动真实 Supervisor/Runtime 进程。

`upgrade apply` creates a filesystem lock and still returns `apply_allowed:false`.
It does not perform runtime handoff.

`snapshot promote` creates a release pointer plan and still returns
`apply_allowed:false`. It does not move `state/release-pointer`.

```text
plan_id=plan-upgrade
from_generation=gen-v14
to_generation=gen-v15
from_release=1.4.0
to_release=1.5.1
```

V1.4/P6 dry-run and lock paths do not execute destructive restore, move release pointer, or start real Supervisor/Runtime processes.

## V1.5 Release Hardening Commands

V1.5 新增 `release` 命令组：

```powershell
cargo run -- release check --output json
cargo run -- release check --target windows --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
```

| 命令 | 行为 |
| --- | --- |
| `release check` | 调用 `eva_release::ReleaseHardeningService::readiness`，聚合 release gates、platform readiness、stability scenarios 和 audit。 |
| `release security` | 输出 `SecurityReviewReport`，覆盖 policy、Lua sandbox、secret redaction、MCP allowlist、hardware raw I/O 和 lifecycle apply risk。 |
| `release perf` | 输出 `PerformanceBaselineReport`，用 release-smoke budget 记录当前 in-memory 实现的性能边界。 |
| `release migration` | 输出 `MigrationGuide` 和 `CompatibilityPolicy`，声明 V1.4 到 V1.5 无破坏性变更。 |

这些命令不修改 `.eva/tasks`，不启动外部 provider，不执行真实 restore 或 supervisor handoff。阻断门禁会映射到稳定 exit code：配置门禁 `2`、policy 门禁 `3`、性能门禁 `4`。

## 验证

```powershell
cargo test -p eva-cli
cargo run -- --version
cargo run -- version --output json
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- inspect runtime --output json
cargo run -- run --example basic --output json
cargo run -- task status --output json
cargo run -- task logs --output json
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- mcp probe --adapter github-mcp --tool list_issues --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- discovery scan --output json
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- restore apply --plan restore-plan.json --confirm plan-123 --artifact-store .eva/artifacts --output json
cargo run -- upgrade check --output json
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
```

当前测试覆盖 version text/JSON、config validate JSON、inspect text、unknown command、JSON escaping、basic run JSON、cancelled basic run、task status/logs/cancel、doctor sample project、V1.1 external capability commands、V1.2 memory context、V1.3 hardware command JSON、V1.4 backup/lifecycle command JSON 和 V1.5 release hardening command JSON。
