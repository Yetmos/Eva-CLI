# eva-cli / 命令行入口

更新时间：2026-07-09

`eva-cli` 负责命令解析、文本/JSON 输出、trace 字段和 exit code 映射。当前命令面覆盖 V1.0 core runtime、V1.1 外部能力诊断、V1.2 request-scoped memory/knowledge context、V1.3 plan-first 硬件接入诊断、V1.4 backup/lifecycle planning、V1.5 release hardening、V1.6 durable task store 入口、V1.9.3 Discovery source report、V1.9.4 durable memory context、V1.9.5 observability smoke 输出，以及 V1.10.4 signed backup archive restore apply gate。V1.11.4 起 `version`、`doctor`、`config validate`、`inspect`、`task`、`adapter`、`mcp`、`skill`、`discovery`、`memory`、`observability`、`hardware`、`backup`、`snapshot`、`restore`、`upgrade` 和 `release` 命令实现迁移到 `run/` 子模块；V1.12.6 daemon 本机控制面已包含 durable task lifecycle、scheduler retry tick、daemon-backed agent drain/reload mutation state 和 release readiness gate，继续复用统一 JSON envelope 和 exit code helper。

CLI 不启动生产后台 daemon；`daemon start` 默认验证本机 pid/lock/state、durable backend、provider/task recovery state、policy、observability 和 shutdown contract，并输出 `provider_processes_started:false`。显式传入 `--no-shutdown-after-smoke` 时会保持前台进程运行，通过受控 filesystem mailbox 支持 `status`、`shutdown`、`submit`、`cancel`、`drain` 和 `reload` 控制请求；没有 running daemon 时稳定返回 `unavailable`。`run --example basic` 同步执行后，默认把最新 task report 写入 `.eva/tasks`，供 `task status/logs/cancel` 跨命令读取。传入 `--durable-backend <path>` 时，run/task 命令改用 V1.6 durable backend 的 `tasks/` 目录。外部能力、记忆上下文、硬件、备份、生命周期和发布加固命令当前都以可验证诊断 surface 为主；V1.8 已允许 manifest-gated stdio/http、MCP JSON-RPC 和 Skill workflow runner 进入受控真实执行路径，但仍不打开 raw hardware I/O，也不执行 destructive restore 或真实进程升级。

## 当前命令

| 命令 | 当前行为 |
| --- | --- |
| `eva version` / `eva --version` | 输出 V1.5 release label、runtime contract 和稳定命令契约。 |
| `eva doctor` | 检查 workspace、配置根、schema、Lua host crate 边界、runtime builder。 |
| `eva config validate` | 加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy、routes，并输出摘要。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 runtime service summary。 |
| `eva run --example basic` | 执行 V1.0 in-memory basic event loop，并写入本地或 durable backend task report。 |
| `eva daemon start/status/stop/shutdown/submit/cancel/drain/reload` | 验证 V1.12/V1.13.5 daemon pid/lock/state、durable backend、provider/task recovery、policy、observability、shutdown contract 和本机 filesystem mailbox 控制面；不启动 provider 进程。 |
| `eva agent status/drain/reload` | 输出 Agent lifecycle evidence；连接 running daemon 时 `drain/reload` 写入 `agent-control.state`，无 daemon 时回退 `mutation_executed:false` evidence。 |
| `eva task status` | 读取 `.eva/tasks` 或 `--durable-backend` 中最新或指定 task 的状态、attempts、retry、取消和 dead-letter 摘要。 |
| `eva task logs` | 读取 task logs。 |
| `eva task cancel` | 对未终态 task 写入取消标记；对已终态 task 记录 cancel request 但不改变终态。 |
| `eva adapter list/probe` | 列出和 probe 已授权 Adapter handle，不启动真实 provider。 |
| `eva mcp list/probe` | 列出和 probe MCP allowlist tool，不启动真实 MCP server。 |
| `eva skill list/run` | 运行受控 workflow skill runner；先校验 schema/runtime gate 和 V1.9.2 policy runtime gate，再写 stdout/stderr/artifact evidence。 |
| `eva discovery scan` | 返回 discovery candidate 和 source reports，明确 discovery 不授予 runtime handle。 |
| `eva memory context` | 为单个 Agent 构造 request context，输出 private/global memory、knowledge、Lua snapshot 和 audit；可用 `--durable-backend` 走 V1.9.4 durable memory/knowledge round trip。 |
| `eva observability smoke` | 写入 V1.9.5 file JSONL observability backend，验证 audit、runtime/provider/task metrics、OTel-style spans 和 best-effort degradation。 |
| `eva hardware list/probe/bind` | 发现、probe 并计划硬件绑定；高风险动作 plan-first，V1.3 不打开 raw I/O。 |
| `eva backup create` | 创建并校验 signed backup archive；`--encrypt` 生成 sealed archive metadata。 |
| `eva snapshot create` | 创建绑定到 backup manifest 的 release snapshot。 |
| `eva restore plan` | 生成 restore plan；V1.4 保持 `apply_allowed:false`。 |
| `eva restore apply` | `--dry-run` 校验 backup artifact 与 pre-restore backup evidence，并可输出 V1.14.1 `mutation_plan`；非 dry-run 还要求 `--lock-store`、policy allow 和 health check。无 mutation steps 时输出 gated report 且 `mutation_executed:false`；有 staged steps 且全部 gate 通过时执行 V1.14.2 file mutation 并输出 `mutation_executed:true`。 |
| `eva restore rollback` | 复用 apply plan、confirmation、artifact evidence、policy、rollback lock 和 health gate，读取 `{plan_id}.restore.txn` 或 `--transaction-log`，只在 transaction status 为 `rollback_required` 时用 pre-restore archive entry 倒序恢复已提交步骤，并输出 `rollback_executed:true` 或二级失败 evidence。 |
| `eva upgrade check` | 诊断 generation、migration、drain、rollback readiness，不启动真实进程。 |
| `eva release check` | 聚合跨平台、稳定性、文档、安全、性能、迁移和 daemon runtime readiness 门禁；可读取 V1.11 artifact、distribution、security scan 和 benchmark evidence，输出 release readiness。 |
| `eva release security` | 输出 policy、sandbox、secret、MCP、hardware 和 lifecycle apply 风险的安全评审。 |
| `eva release perf` | 输出 EventBus、Scheduler、Adapter、memory、backup 和 release check 的性能预算基线；可读取 benchmark evidence 使用真实测量输入。 |
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

## V1.12 Daemon Boundary And Control Mailbox

`eva daemon start/status/stop/shutdown/submit/cancel/drain/reload` 固定本机 daemon 进程边界和控制面，但不启动生产后台守护进程，也不启动外部 provider。`start` 默认运行 foreground smoke：获取 `daemon.lock`，验证 durable backend，扫描 task/provider process recovery state，验证 policy domain 和 observability JSONL sink，写入 `daemon.state` / `daemon.pid`，随后执行 shutdown contract 并移除 lock/pid。显式传入 `--no-shutdown-after-smoke` 时，命令保持前台运行并处理 `state/control/requests` 到 `state/control/responses` 的本机 filesystem mailbox；每轮 control polling 前会执行 V1.12.4 scheduler retry tick，把 due dead-letter replay event 投递到 scheduler mailbox，并以 `scheduler-retry` consumer 更新 durable log ack/fail。V1.12.5 后，`eva agent drain/reload` 可连接该 daemon 并写入 `agent-control.state`，记录 drain gate 和 reload 后新 work generation；V1.13.5 会把残留 active provider session 标记为 interrupted 并在 `recovery` JSON 中报告；V1.14.3 的 `version` runtime marker 包含 `mcp_http_auth_v1.13.6`、`mcp_compat_matrix_v1.13.7`、`provider_supervision_release_gate_v1.13.8`、`restore_staged_mutation_planner_v1.14.1`、`restore_file_mutation_engine_v1.14.2` 和 `restore_rollback_apply_v1.14.3`。daemon smoke 仍不启动 provider，也不是完整生产热更新 apply。

```powershell
cargo run -- daemon start --foreground --dev --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --observability-backend .eva/daemon-observability --output json
cargo run -- daemon start --foreground --dev --no-shutdown-after-smoke --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --observability-backend .eva/daemon-observability --output json
cargo run -- daemon status --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --output json
cargo run -- daemon submit --task req-daemon-task-1 --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --output json
cargo run -- daemon shutdown --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --output json
```

关键 JSON 字段：

| 字段 | 含义 |
| --- | --- |
| `provider_processes_started:false` | 该命令只验证 daemon 边界，不进入 provider supervision。 |
| `durable_backend` | 启动前验证过的 durable backend layout 和 schema。 |
| `recovery` | 启动时扫描 task/provider process snapshot 后的 scanned/recovered/backoff/skipped evidence。 |
| `policy` | 从项目 policy documents 得到的 effective policy layer evidence。 |
| `observability` | file JSONL backend 的 audit/metric/span smoke evidence。 |
| `shutdown` | foreground smoke 结束时调用 `Runtime::shutdown()` 的幂等报告。 |
| `trace_id` | control mailbox request/response 的链路标识；默认来自 CLI request id。 |
| `request_file` / `response_file` | 本机 filesystem mailbox 的请求和响应文件路径。 |
| daemon `submit` / `cancel` | `submit` 写入 durable `queued` task lifecycle；`cancel` 将非终态 task 推进到 `cancelling` 并追加日志。 |

## V1.1 External Capability Commands

V1.1 增加外部能力生态命令，同时保留 V1.0 basic runtime 路径：

- `eva adapter list`：列出从项目 manifest 派生的授权 Adapter handle。
- `eva adapter probe --adapter <id>`：返回 side-effect-free health、transport 和 capability 详情。
- `eva adapter probe --capability <name> [--provider <id>]`：验证 AdapterRouter 选择，不调用外部 provider。
- `eva mcp list`：列出 MCP transport adapters 和 tool allowlist。
- `eva mcp probe --adapter <id> --tool <name>`：报告 tool 是否在 allowlist 中；blocked probe 也返回诊断 envelope，因为未调用 provider。
- `eva skill list`：列出受控 workflow skill adapters 和 runtime gates。
- `eva skill run --skill <id> --input <json>`：运行受控 Skill workflow；默认 `codex_skill` 使用内置受控 runner，manifest 显式 `skill.runner.command` 时才启动 allowlist process runner，并写入 artifact/audit evidence。
- `eva discovery scan`：返回 trusted candidates 和 `source_reports`，用 `handle_granted=false` 证明 discovery 不是授权，并暴露 source timeout/cache key/status/reject reason。

## V1.2 Memory Context Command

`eva memory context` 是 `eva-memory` 和 `eva-lua-host` smoke。默认使用 in-memory seed；传入 `--durable-backend <path>` 时会写入 durable backend 的 `state/memory` 与 `state/knowledge` 后重新读取构建 context：

```powershell
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
cargo run -- memory context --agent root-agent --query memory --durable-backend .eva/ci-memory --output json
```

| Option | Meaning |
| --- | --- |
| `--agent <id>` | Agent whose private memory may enter the context. Defaults to `root-agent`. |
| `--query <text>` | Knowledge search query. Defaults to `memory`. |
| `--request-id <id>` | Request id attached to seeded context records. Defaults to `req-memory-1`. |
| `--private-limit <n>` | Maximum private memory records returned. |
| `--global-limit <n>` | Maximum global memory records returned. |
| `--knowledge-limit <n>` | Maximum knowledge search results returned. |
| `--durable-backend <path>` | Use V1.6 durable backend state directories for V1.9.4 memory/knowledge files. |

JSON output contains `memory`, `global_memory`, `knowledge`, `lua_context`, and `audit`. Memory records include `created_at_ms`, `expires_at_ms`, and `compression`; context output filters expired records and redacts sensitive token/password/secret/API-key shaped values before Lua snapshot creation.

## V1.9.5 Observability Smoke Command

`eva observability smoke` 是 V1.9.5 file observability backend 的 CLI smoke。它显式打开 backend path，写入一条 runtime audit event、runtime/provider/task 三类 metric point，以及 CLI 和 provider 两条 OTel-style span JSONL 记录。后端不可用时命令保持成功，并在输出中标记 `degraded` 和 `degraded_reasons`。

```powershell
cargo run -- observability smoke --backend .eva/ci-observability --output json
cargo run -- observability smoke --backend .eva/ci-observability
```

JSON output contains `backend_root`, `degraded`, `degraded_reasons`, `audit_events`, `metric_points`, `otel_spans`, and `continuity_key`. Backend files are written as:

- `audit.jsonl`
- `metrics.jsonl`
- `otel-spans.jsonl`

## V1.3 Hardware Commands

V1.3 新增 `hardware list/probe/bind`，用于诊断 hardware Adapter manifest 和生成绑定计划：

V1.11.4 后该命令组由 `src/run/hardware_cmd.rs` 承载 parser、project device discovery、bind plan、runtime policy gate audit 和 JSON formatter；`run.rs` 只保留顶层 dispatch。

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
| `hardware bind --adapter <id>` | 生成绑定计划、风险提示、plan steps 和 V1.9.2 policy audit；disabled/rejected 设备返回 `blocked`。 |
| `hardware bind --apply` | V1.3 只校验逻辑计划并保留 plan-first 输出；V1.9.2 追加 runtime policy gate 证据，不打开真实设备。 |

`scale-main` 默认 disabled，因此 JSON 中会看到：

- `trust: "rejected"`
- `health: "disconnected"`
- `handle_granted: false`
- `rejected_reason: "hardware adapter manifest is disabled"`
- `hardware bind` 的 `status: "blocked"`

这条命令面验证硬件身份、发现、绑定计划和风险提示，但不执行 USB、串口、BLE、网络或 vendor SDK I/O。

## V1.4 Backup And Lifecycle Commands

V1.4 新增备份、snapshot、restore plan 和 upgrade check：

V1.11.4 后 `backup create` 由 `src/run/backup_cmd.rs` 承载 parser、BackupService 调用、signed/sealed archive JSON formatter 和 artifact store 输出；`snapshot create/promote` 由 `src/run/snapshot_cmd.rs` 承载 parser、ReleaseSnapshotService 调用和 release pointer plan formatter；`restore plan/apply` 由 `src/run/restore_cmd.rs` 承载 parser、dry-run/apply gate、lock/health/rollback writer 和 restore JSON formatter，并继续复用 backup/snapshot 模块导出的 result helper；`upgrade check/apply` 由 `src/run/upgrade_cmd.rs` 承载 parser、apply lock、state-store handoff、runtime binary smoke、release pointer/rollback formatter，保持公开 JSON shape 不变。

```powershell
cargo run -- backup create --output json
cargo run -- backup create --encrypt --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- restore apply --dry-run --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --output json
cargo run -- restore apply --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --lock-store .eva/locks --output json
cargo run -- backup create --artifact-store .eva/artifacts --output json
cargo run -- snapshot promote --snapshot-id snapshot-v15 --confirm snapshot-v15 --artifact-store .eva/artifacts --output json
cargo run -- upgrade check --output json
cargo run -- upgrade apply --plan upgrade-plan.txt --confirm plan-upgrade --lock-store .eva/locks --output json
cargo run -- upgrade apply --plan upgrade-plan.txt --confirm plan-upgrade --lock-store .eva/locks --state-store .eva/supervisor --output json
```

| 命令 | 行为 |
| --- | --- |
| `backup create` | 构造 `BackupPlan`，默认写入 in-memory `ArtifactStore`；传入 `--artifact-store <path>` 时写入 filesystem artifact store，生成 signed archive `BackupManifest`，并立即校验 digest/signature；`--encrypt` 使用本地开发 key 生成 sealed archive。 |
| `snapshot create` | 创建 pre/post release snapshot，并关联已验证 backup artifact；传入 `--artifact-store <path>` 时同步落盘 snapshot 依赖的 backup artifact。 |
| `restore plan` | 输出 restore steps、risks、audit，且 `apply_allowed:false`；传入 `--artifact-store <path>` 时使用同一 filesystem artifact store 生成可追溯 backup evidence。 |
| `restore apply` | `--dry-run` 读取 plan 文件并验证 filesystem artifact store 中的 backup digest 和 pre-restore backup evidence，保持 `apply_allowed:false`，并在 plan 声明 staged mutation steps 时输出 `mutation_plan`；非 dry-run 必须提供 `--lock-store`，默认 project policy 会拒绝 `restore.apply`，显式 allow 后才获取 `{plan_id}.restore.lock` 并运行 health gate。无 mutation steps 时返回 `status:"gated"`、`mutation_executed:false`；有 staged steps 时执行 file mutation、写 `{plan_id}.restore.txn`，成功返回 `status:"applied"`、`mutation_executed:true`，失败返回 `rollback_required`。 |
| `upgrade check` | 输出 supervisor candidate、migration preflight、drain plan 和 rollback plan。 |
| `upgrade apply` | 未传 `--state-store` 时保持 lock-only 输出；传入 `--state-store` 且 project policy 同时允许 `supervisor.handoff` 和 `release.pointer_mutation` 时，会提交本地 blue-green handoff、写 `state/release-pointer`、持久化 `handoff.prepared` / `handoff.committed`，health 失败时输出 rollback plan 且不写 pointer。 |

`restore apply --dry-run` 和 gated `restore apply` 的 plan 文件使用稳定 key/value 格式：

```text
plan_id=plan-123
backup_artifact_id=backup-for-snapshot-v14
backup_digest=sha256:<hex>
pre_restore_backup_artifact_id=pre-restore-plan-123
pre_restore_backup_digest=sha256:<hex>
```

V1.14.1 起，plan 文件还可以追加 staged mutation preview 字段：

```text
restore_target_root=workspace
mutation_step=copy|config/eva.yaml|backup/config|sha256:<hex>|none|file
mutation_step=replace|bin/eva|backup/bin|sha256:<hex>|sha256:<old>|file
mutation_step=delete|logs/old.log|none|none|sha256:<old>|file
```

这些字段在 `--dry-run` 中只生成 `mutation_plan`（`preview`、`affected_paths`、`preflight_hash`、`rollback_manifest`）。非 dry-run 且 confirmation、artifact evidence、policy、lock 和 health gate 全部通过后，会执行 staged file mutation 并写 transaction log。

V1.10.4/V1.14.1/V1.14.2/V1.14.3 `restore apply`/`restore rollback` 完成受控 destructive apply gate、staged mutation preview、staged file mutation engine 和 failed transaction rollback apply：confirmation、artifact evidence、policy approval、lock 和 health check 都通过后，只有 plan 声明 mutation steps 时才写目标文件；rollback 只接受 rollback-required transaction log 和可校验 pre-restore archive entry。它不移动 release pointer，不启动真实 Supervisor/Runtime 进程。

`upgrade apply` 不传 `--state-store` 时只创建 filesystem lock 并保持
`apply_allowed:false`，用于兼容旧的 plan-first lock path。V1.10.5 开始，传入
`--state-store` 后会在 policy allow、lock、runtime binary smoke 和 health gate
通过后提交本地 supervisor handoff state，并写入 `state/release-pointer`。这仍是
local supervisor adapter smoke，不是生产 service manager/daemon handoff。

`snapshot promote` creates a release pointer plan and still returns
`apply_allowed:false`. It does not move `state/release-pointer`.

```text
plan_id=plan-upgrade
from_generation=gen-v14
to_generation=gen-v15
from_release=1.4.0
to_release=1.5.1
```

V1.4/P6 dry-run and lock-only paths do not execute destructive restore, move release pointer, or start real Supervisor/Runtime processes. V1.10.5 `upgrade apply --state-store` is the controlled exception for local release pointer mutation after explicit policy approval.

## V1.5 Release Hardening Commands

V1.5 新增 `release` 命令组：

```powershell
cargo run -- release check --output json
cargo run -- release check --artifact-evidence release-evidence/release-artifact.evidence --output json
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
cargo run -- release check --security-scan-evidence release-evidence/release-security-scan.evidence --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release check --target windows --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release perf --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release migration --output json
```

| 命令 | 行为 |
| --- | --- |
| `release check` | 调用 `eva_release::ReleaseHardeningService::readiness`，聚合 release gates、platform readiness、stability scenarios 和 audit。 |
| `release check --artifact-evidence` | 读取 V1.11.1 key/value artifact evidence，校验 signed artifact、SHA-256 keyed signature、source commit provenance、SBOM 标记和 scan status；失败时 required gate blocked 并返回配置门禁 exit code `2`。 |
| `release check --distribution-evidence` | 读取 V1.11.2 key/value distribution evidence，校验 Windows/Linux/macOS install smoke、安装/升级/卸载文档路径和 package-manager dry-run；失败时 required gate blocked 并返回配置门禁 exit code `2`。 |
| `release check --security-scan-evidence` | 读取 V1.11.3 key/value external scanner evidence；scanner skipped/failed 或 high/critical finding 会阻断并返回配置门禁 exit code `2`。 |
| `release check --benchmark-evidence` | 读取 V1.11.3 measured benchmark evidence；空样本、非 passed 状态或 observed latency 超预算会阻断并返回配置门禁 exit code `2`。 |
| `release security` | 输出 `SecurityReviewReport`，覆盖 policy、Lua sandbox、secret redaction、MCP allowlist、hardware raw I/O 和 lifecycle apply risk。 |
| `release perf` | 输出 `PerformanceBaselineReport`，用 release-smoke budget 记录当前 in-memory 实现的性能边界；传入 `--benchmark-evidence <path>` 时使用真实测量输入并保持输出 JSON shape 稳定。 |
| `release migration` | 输出 `MigrationGuide` 和 `CompatibilityPolicy`，声明 V1.4 到 V1.5 无破坏性变更。 |

这些 release 命令不修改 `.eva/tasks`，不启动外部 provider，不执行真实 restore 或 supervisor handoff；V1.10.5 的本地 handoff/pointer mutation 只存在于单独的 `upgrade apply --state-store` 路径。阻断门禁会映射到稳定 exit code：配置门禁 `2`、policy 门禁 `3`、性能门禁 `4`。

V1.11.4 已把 `version`、`doctor`、`config validate`、`inspect`、`task`、`adapter`、`mcp`、`skill`、`discovery`、`memory`、`observability`、`hardware`、`backup`、`snapshot`、`restore`、`upgrade` 和 release 命令组的 parser/writer/formatter 拆入 `src/run/` 子模块；`run.rs` 保留顶层 dispatch、共享 formatter helper、trace 和 exit code 映射。拆分后的回归继续覆盖 version text/JSON、doctor sample project、config validation JSON、inspect text/durable diagnostics JSON、task store JSON、外部能力诊断 JSON、skill run JSON、discovery source report JSON、memory/observability JSON、hardware list/probe/bind JSON、backup/lifecycle JSON、restore/upgrade apply gate JSON、`release_check`、`release_perf` 和 V1.5 release hardening JSON contract。

## 验证

```powershell
cargo test -p eva-cli
cargo run -- --version
cargo run -- version --output json
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- inspect runtime --output json
cargo run -- run --example basic --output json
cargo run -- daemon start --foreground --dev --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --observability-backend .eva/daemon-observability --output json
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
cargo run -- restore apply --dry-run --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --output json
cargo run -- restore apply --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --lock-store .eva/locks --output json
cargo run -- upgrade check --output json
cargo run -- observability smoke --backend .eva/ci-observability --output json
cargo run -- release check --output json
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
cargo run -- release check --security-scan-evidence release-evidence/release-security-scan.evidence --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release perf --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release migration --output json
```

当前测试覆盖 version text/JSON、config validate JSON、inspect text/durable diagnostics JSON、unknown command、JSON escaping、basic run JSON、cancelled basic run、daemon foreground smoke/lock conflict/bad durable backend、agent daemon drain/reload mutation fallback、task status/logs/cancel、doctor sample project、V1.1 external capability commands、V1.2 memory context、V1.3 hardware command JSON、V1.4 backup/lifecycle command JSON、V1.5 release hardening command JSON、V1.9.5 observability smoke JSONL backend、V1.10.4 restore apply policy denial、lock conflict、health failure rollback、gated report contract、V1.14.1 staged mutation plan preview/digest contract、V1.14.2 staged mutation apply contract 和 V1.14.3 restore rollback contract，以及 V1.11.1 artifact evidence / V1.11.2 distribution evidence / V1.11.3 security scan and benchmark evidence release gates。
