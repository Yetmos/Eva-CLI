# eva-cli/src

更新时间：2026-07-10

本目录承载 CLI 命令解析、执行分发、文本/JSON 输出、exit code 映射和本地/持久诊断文件读写。V1.11.4 开始把稳定命令组从 `run.rs` 拆到子模块：`version`、`doctor`、`config validate`、`inspect`、`task`、`adapter`、`mcp`、`skill`、`discovery`、`memory`、`observability`、`hardware`、`backup`、`snapshot`、`restore`、`upgrade` 和 `release` 已迁移到 `run/` 子模块，并继续复用 `run.rs` 的 JSON envelope、trace 字段和 exit code helper。

## 文件职责

| 文件 | 当前状态 | 说明 |
| --- | --- | --- |
| `lib.rs` | 已实现 | 导出 CLI 顶层入口。 |
| `run.rs` | V1.11.4 拆分中 | 顶层命令解析、formatter、exit code、共享 JSON envelope/trace helper，以及尚未拆出的 V1.0 `run --example basic`。 |
| `run/version_cmd.rs` | V1.11.4 已实现 | `version` / `--version` 的 text/JSON writer，输出 release label、runtime mode 和稳定 command contract。 |
| `run/doctor_cmd.rs` | V1.11.4 已实现 | `doctor` parser、workspace/config/schema/runtime boundary check executor 和 text/JSON writer；继续复用 `doctor.rs` 的检查实现。 |
| `run/config_cmd.rs` | V1.11.4 已实现 | `config validate` parser、`ValidationReport`、schema path summary 和 text/JSON writer；保持 schema validation error context 的 `config.validate` envelope 不变。 |
| `run/inspect_cmd.rs` | V1.11.4 已实现 | `inspect` / `inspect durable` parser、project inspect writer、durable backend diagnostics reader/writer 和 JSON formatter；保持 `inspect` / `inspect.durable` envelope 不变。 |
| `run/task_cmd.rs` | V1.11.4 已实现 | `task status/logs/cancel` parser、local/durable task store reader/writer、cancel state update 和 task JSON formatter；`run --example basic` 继续通过该模块写 task snapshot。 |
| `run/adapter_cmd.rs` | V1.11.4 已实现 | `adapter list/probe` parser、runtime probe、text/JSON writer 和 adapter report formatter；保持外部能力诊断 envelope 不变。 |
| `run/mcp_cmd.rs` | V1.11.4 已实现 | `mcp list/probe` parser、allowlist probe、text/JSON writer 和 MCP report formatter；保持未授权/非 MCP adapter 错误映射不变。 |
| `run/skill_cmd.rs` | V1.11.4 已实现 | `skill list/run` parser、Skill runtime policy gate、adapter-backed runner invocation、text/JSON writer 和 invoke trace formatter；保持 artifact/audit evidence JSON shape 不变。 |
| `run/discovery_cmd.rs` | V1.11.4 已实现 | `discovery scan` parser、source scan、source report text/JSON writer 和 discovery candidate formatter。 |
| `run/memory_cmd.rs` | V1.11.4 已实现 | `memory context` parser、in-memory/durable context seed、text/JSON writer、memory/knowledge/Lua context formatter；保持 redaction、expiration 和 durable round trip 输出不变。 |
| `run/observability_cmd.rs` | V1.11.4 已实现 | `observability smoke` parser、file JSONL backend smoke、text/JSON writer 和 degraded report formatter；保持 audit/metrics/span 计数和 continuity key 输出不变。 |
| `run/hardware_cmd.rs` | V1.11.4 已实现 | `hardware list/probe/bind` parser、project device discovery、bind plan、runtime policy gate audit 和 hardware candidate/plan formatter；保持 plan-first、不打开 raw I/O 和 JSON envelope 不变。 |
| `run/backup_cmd.rs` | V1.11.4 已实现 | `backup create` parser、BackupService 调用、signed/sealed archive text/JSON writer 和 backup result formatter；snapshot/restore 继续复用其 backup result helper。 |
| `run/snapshot_cmd.rs` | V1.11.4 已实现 | `snapshot create/promote` parser、ReleaseSnapshotService 调用、release pointer plan formatter 和 snapshot JSON writer；restore plan 继续复用其 snapshot helper。 |
| `run/restore_cmd.rs` | V1.14.4 已更新 | `restore plan/apply/rollback` parser、restore apply dry-run、filesystem artifact/lock gate、policy/health/rollback writer、staged mutation plan parser/formatter、mutation/rollback engine 调用、operator confirmation 输出和 restore JSON formatter；旧 no-step plan 保持 gated `mutation_executed:false` contract，带 staged steps 的 apply 可输出 `mutation_executed:true`，rollback 可输出 `rollback_executed:true`。 |
| `run/upgrade_cmd.rs` | V1.11.4 已实现 | `upgrade check/apply` parser、apply lock、state-store supervisor handoff、runtime binary smoke、release pointer mutation 和 rollback formatter；保持 lock-only 与 handoff JSON contract 不变。 |
| `run/release_cmd.rs` | V1.11.4 已实现 | `release check/security/perf/migration` 的 parser、artifact/distribution/security scan/benchmark evidence reader、文本/JSON writer 和 release report formatter；保持 V1.11.1-V1.11.3 release evidence gate 的公开 JSON shape 与 exit code 不变。 |
| `run/emit_cmd.rs` | V1.11.5.1 已实现 | `emit` parser、typed `eva-core::Event` 构造、in-memory/durable EventBus publish、text/JSON receipt formatter；支持 payload、target、request/generation 和 trace metadata。 |
| `run/agent_cmd.rs` | V1.12.5 已更新 | `agent status/drain/reload` parser、AgentRuntime/AgentLifecycle/DrainCoordinator/GenerationController evidence 构造、text/JSON formatter；`drain/reload` 连接 running daemon 时通过 mailbox 写入 daemon mutation state，无 daemon 时保持 `mutation_executed:false` evidence。 |
| `run/daemon_cmd.rs` | V1.15.4 已更新 | `daemon start/status/stop/shutdown/submit/cancel/drain/reload` parser、foreground daemon smoke/control mailbox writer；`start` JSON 新增 task/provider process recovery report 和 `hardware_hotplug` subscriber evidence。 |
| `run/version_cmd.rs` runtime marker | V1.15.4 已更新 | `version` / `--version` 输出新增 `mcp_http_auth_v1.13.6`、`mcp_compat_matrix_v1.13.7`、`provider_supervision_release_gate_v1.13.8`、`restore_staged_mutation_planner_v1.14.1`、`restore_file_mutation_engine_v1.14.2`、`restore_rollback_apply_v1.14.3`、`restore_operator_confirmation_v1.14.4`、`service_manager_abstraction_v1.14.5`、`hardware_os_permission_provider_v1.15.1` 和 `hardware_hotplug_subscriber_v1.15.4`，用于标识 MCP HTTP/auth transport boundary、compatibility matrix release gate、provider supervision release gate、restore staged mutation planner、restore file mutation engine、restore rollback apply、operator confirmation 输出、service-manager 抽象层 readiness、硬件 OS permission provider 诊断和 daemon hotplug subscriber。 |
| `run/capability_cmd.rs` | V1.11.5.3 已实现 | `capability list/probe/call` parser、CapabilityRegistry/provider selection/permission gate/runtime policy/adapter-backed host evidence 构造、text/JSON formatter；`call` 默认 dry-run，确认后受控 invoke。 |
| `doctor.rs` | 已更新 | workspace/config/schema/runtime builder/Lua host 诊断。 |
| `inspect.rs` | V0.3 已实现 | 从 `ProjectConfig` 和 `RuntimeSummary` 构造综合 inspect report。 |
| `emit.rs` | 入口保留 | typed ingress event 命令的顶层占位；真实 CLI 行为由 `run/emit_cmd.rs` 承载。 |
| `agent.rs` | 入口保留 | Agent lifecycle 命令的顶层占位；真实 CLI 行为由 `run/agent_cmd.rs` 承载。 |
| `adapter.rs` | 边界保留 | 后续可从 `run.rs` 拆出 adapter 子命令。 |
| `capability.rs` | 入口保留 | capability routing 命令的顶层占位；真实 CLI 行为由 `run/capability_cmd.rs` 承载。 |

## V1.0/V1.6.4 任务状态

## V1.9.1 Config Validation

`eva config validate` 继续保持相同 text/JSON envelope。V1.9.1 后，`load_project_config` 会先用 `config/schemas/*.schema.json` 校验主配置、manifest、policy 和 routes，再进入 typed loader；schema 错误在 JSON error context 中包含 `path`、`schema_path`、`field`、`schema_rule` 和 `suggestion`。

当前 CLI 回归覆盖合法项目 JSON 成功，以及 routes schema `additionalProperties` 失败时的 `config.validate` JSON error context。

`run.rs` 在 `eva run --example basic` 成功返回报告后，默认写入两类文件：

- `.eva/tasks/<task-id>.task`
- `.eva/tasks/latest-basic.task`

文件内容是稳定的行式 key/value 诊断格式，由 `eva-storage::FileSystemTaskStateStore` 读写。传入 `--durable-backend <path>` 时，CLI 会打开 V1.6 durable backend 并改用其 `tasks/` 目录；`task status/logs/cancel` 会读取同一位置并重新输出标准 text/JSON envelope。未传 `--durable-backend` 时，`.eva/tasks` 仍只是兼容本地诊断路径。

## V1.1 External Capability Surface

CLI 拥有第一版外部能力命令面；V1.11.4 后其中 `adapter`、`mcp`、`skill` 和 `discovery` 已拆入 `run/` 子模块：

- Parser branches for `adapter`, `mcp`, `skill`, and `discovery` subcommands.
- Execution bridges into `eva-adapter`, `eva-mcp`, and `eva-discovery` without adding persistent CLI state.
- Text and JSON writers for Adapter list/probe, MCP list/probe, Skill list/run, and Discovery scan.
- Provider invocation JSON, such as `skill run`, includes both `audit` and
  request-level `trace` inside `data`, while the top-level envelope keeps the
  CLI command span trace.
- V1.8.4 routes `skill run` into the schema-gated Skill workflow runner, which
  records stdout/stderr/run-report/artifact evidence while preserving the same
  JSON envelope shape.
- V1.9.2 checks the Skill runtime gate through `RuntimePolicyGate` before the
  runner is started.
- V1.9.3 writes `discovery scan` source reports in text/JSON, including source
  id, cache key, timeout, elapsed time, status, candidate counts, error, and
  rejected reason.
- V1.9.4 allows `memory context --durable-backend <path>` to seed durable
  memory/knowledge files, reload them, filter expired memory, redact sensitive
  values, and report TTL/compression metadata in JSON.
- Tests covering V1.1 JSON envelopes, blocked MCP tool probes, and V1.1 version identity.

## V1.2 Memory Context Surface

`run/memory_cmd.rs` 实现 `eva memory context`。该命令加载项目配置，种子化一个 in-memory V1.2 context，调用 `eva_memory::ContextBuilder`，并输出与其他 CLI 命令一致的 JSON envelope。它是诊断 smoke，不是 durable memory persistence。

当前 V1.2 测试覆盖：

- V1.2 version identity and `context_v1.2` runtime marker.
- `memory.context` JSON output with private memory, global memory, knowledge, Lua context summary, and audit fields.
- Existing V1.1 external capability commands to prevent regressions.

## V1.3 Hardware Surface

`run/hardware_cmd.rs` 实现 `eva hardware list/probe/bind`：

- Parser branch: `parse_hardware_command` 分发 `list`、`probe`、`bind`。
- `hardware list`：加载项目配置并调用 `discover_project_devices`，输出 hardware candidates。
- `hardware probe --adapter <id>`：过滤单个 Adapter 的硬件候选；找不到时返回 `NotFound`。
- `hardware bind --adapter <id>`：生成 `HardwareBindPlan`，包含 candidate、status、apply 标记、OS permission evidence、plan steps、risks 和 V1.9.2 policy audit。
- `hardware bind --apply`：V1.15.1 仍不打开设备，只验证逻辑边界、runtime policy gate 和平台权限诊断；权限缺失时保持 `blocked` 且不 claim lease。
- JSON writer 输出 `hardware.list`、`hardware.probe`、`hardware.bind` 三种 command envelope。
- Tests 覆盖 V1.3 version identity、硬件候选 JSON、bind plan JSON 和 blocked disabled manifest。

`scale-main` 默认 disabled，因此 `hardware bind` 返回 `status: blocked`。这是有意设计：V1.15.1 要证明 hardware Adapter 边界、plan-first 体验和 OS permission provider remediation，而不是在开发机上触发真实设备 I/O。

## V1.4 Backup And Lifecycle Surface

`run/backup_cmd.rs` 实现 `backup create`，`run/snapshot_cmd.rs` 实现 `snapshot create/promote`，`run/restore_cmd.rs` 实现 `restore plan/apply`，`run/upgrade_cmd.rs` 实现 upgrade plan-first 和 handoff 命令：

- `backup create`：调用 `eva_backup::BackupService`，默认写入 in-memory `ArtifactStore`；传入 `--artifact-store <path>` 时写入 filesystem artifact store，生成 signed archive manifest 并校验 digest/signature；传入 `--encrypt` 时生成 sealed archive metadata。
- `snapshot create`：调用 `ReleaseSnapshotService` 生成 pre/post release snapshot，并可通过 `--artifact-store <path>` 落盘其依赖的 backup artifact。
- `restore plan`：调用 `ReleaseSnapshotService::restore_plan`，输出 `apply_allowed:false`，并可通过 `--artifact-store <path>` 生成 filesystem backup evidence。
- `restore apply`：带 `--dry-run` 时读取 plan 文件并验证 filesystem artifact store 中的 backup artifact key/digest 与 pre-restore backup evidence，保持 `apply_allowed:false`，并在 plan 声明 staged mutation steps 时输出 plan-only `mutation_plan`；非 dry-run 必须带 `--lock-store`，默认 project policy 拒绝 `restore.apply` 且不会留下锁文件，显式 allow 后获取 `{plan_id}.restore.lock` 并运行 health gate。无 mutation steps 时返回 `status:"gated"`、`mutation_executed:false`；有 staged steps 时调用 `RestoreMutationEngine` 写目标文件和 `{plan_id}.restore.txn`，成功返回 `status:"applied"`、`mutation_executed:true`，失败返回 `rollback_required`。
- `upgrade check`：调用 `eva_lifecycle` 的 in-memory supervisor、generation、drain、rollback 状态机，并结合 migration preflight。
- `upgrade apply`：默认仍是 lock-only；带 `--state-store` 时还要求 `supervisor.handoff` 和 `release.pointer_mutation` policy allow，通过 runtime binary smoke 和 health gate 后写 `state/release-pointer` 与 handoff state，health 失败输出 rollback plan 且不写 pointer。

这些命令是 release/lifecycle readiness smoke。V1.10.4 `restore apply` 已补 confirmation、artifact evidence、policy approval、apply lock、health check 和 rollback-required 输出；V1.14.1 追加 copy/delete/replace staged mutation preview、affected paths、preflight hash 和 rollback manifest；V1.14.2 已在 staged plan 存在且 gate 全部通过时执行文件 mutation 并写 transaction log；V1.14.3 `restore rollback` 可在 confirmation、artifact evidence、policy、rollback lock、health、transaction log 和 pre-restore archive entry 全部通过后执行反向文件恢复；V1.14.4 在 dry-run/apply/rollback text 与 JSON 中追加 `operator_confirmation`，显式列出 confirm token、target root、affected count、状态位和不可逆风险。V1.10.5 `upgrade apply --state-store` 会执行本地 release pointer mutation 和 handoff state 持久化，但仍不是生产 service-manager/daemon handoff；V1.14.5 只新增 service-manager trait、typed config、fake adapter evidence 和版本 marker。

P6-003 adds `upgrade apply --plan <path> --confirm <plan_id> --lock-store <path>`.
Without `--state-store`, it reads a key/value upgrade plan, creates a filesystem
lock, returns `apply_allowed:false`, records the destructive supervisor handoff
policy decision in audit, and does not start runtime handoff.

V1.10.5 extends the same command with `--state-store <path>`, `--runtime-binary
<path>` and `--health healthy|failed|unavailable`. The state-store path is the
controlled mutation boundary: it writes `handoff.prepared`,
`handoff.committed`, and `state/release-pointer` only after explicit policy
approval and healthy candidate evidence.

P6-004 adds `snapshot promote --snapshot-id <id> --confirm <snapshot_id>`.
It creates a release pointer plan with audit evidence, returns
`apply_allowed:false`, and does not move `state/release-pointer`.

## V1.5 Release Hardening Surface

`run/release_cmd.rs` 实现 V1.5 发布加固命令：

- `release check`：调用 `eva_release::ReleaseHardeningService::readiness`，输出跨平台、稳定性、文档、安全、性能、迁移、V1.6.4 durable recovery、V1.6.5 durable diagnostics 和 V1.12.6 daemon runtime readiness 门禁。
- `release check --artifact-evidence <path>`：读取 V1.11.1 signed artifact/provenance evidence，失败时返回配置门禁 exit code `2`。
- `release check --distribution-evidence <path>`：读取 V1.11.2 distribution evidence，校验 Windows/Linux/macOS install smoke、安装/升级/卸载文档路径和 package-manager dry-run，失败时返回配置门禁 exit code `2`。
- `release check --security-scan-evidence <path>`：读取 V1.11.3 external scanner evidence；scanner skipped/failed 或 high/critical finding 会阻断并返回配置门禁 exit code `2`。
- `release check --benchmark-evidence <path>`：读取 V1.11.3 measured benchmark evidence；空样本、非 passed 状态或 observed latency 超预算会阻断并返回配置门禁 exit code `2`。
- `release security`：输出 security findings，覆盖 policy、Lua sandbox、secret redaction、MCP allowlist、hardware handle 和 lifecycle apply 风险。
- `release perf`：输出 release-smoke 性能预算，覆盖 EventBus、Scheduler、Adapter probe、memory context、backup 和 release check；传入 `--benchmark-evidence <path>` 时使用真实测量输入并保持输出 JSON shape 稳定。
- `release migration`：输出 V1.4 -> V1.5 迁移步骤和兼容性策略。

这些命令共享 success/error JSON envelope、trace 字段和 exit code 映射。它们不写 `.eva/tasks`，不执行外部 provider，也不把 plan-first restore/upgrade 变成 apply。

## V1.6.5 Durable Diagnostics Surface

`run/inspect_cmd.rs` 实现 `eva inspect durable --durable-backend <path>` 诊断命令。它调用 `eva_runtime::inspect_durable_backend()`，以 read-only 模式报告 durable backend path、schema/layout version、migration status、event/dead-letter 计数和 `pending_redrive_count`，并保持 `inspect.durable` JSON envelope。诊断读取不会创建缺失的 `events/log` 或 `events/dead_letters` 子目录。

## V1.9.5 Observability Smoke Surface

`run/observability_cmd.rs` 实现 `eva observability smoke --backend <path>` 诊断命令。它调用 `eva_observability::BestEffortObservabilityPipeline`，写入 runtime audit event、runtime/provider/task metrics 和两条 OTel-style span JSONL 记录，并在 JSON envelope 中输出 `backend_root`、`degraded`、`degraded_reasons`、`audit_events`、`metric_points`、`otel_spans` 和 `continuity_key`。后端不可用时命令仍返回成功，用 degraded evidence 标记降级路径。

## 共享实现边界

V1.11.4 开始按命令组拆分实现，但共享 helper 仍集中在 `run.rs`，可以让以下行为保持一致：

- success/error JSON envelope。
- trace 字段和 command 名称。
- exit code 映射。
- text output 的摘要风格。
- tests 对同一个 CLI 入口执行完整命令。

`version`、`doctor`、`config validate`、`inspect`、`task`、`adapter`、`mcp`、`skill`、`discovery`、`memory`、`observability`、`hardware`、`backup`、`snapshot`、`restore`、`upgrade` 和 `release` 已完成拆分；后续拆分 `run --example basic` 等命令组时，仍不能改变公开 JSON envelope。

## 验证

```powershell
cargo test -p eva-cli
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- restore apply --dry-run --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --output json
cargo run -- restore apply --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --lock-store .eva/locks --output json
cargo run -- upgrade check --output json
cargo run -- upgrade apply --plan upgrade.plan --confirm plan-upgrade --lock-store .eva/locks --state-store .eva/supervisor --output json
cargo run -- inspect durable --durable-backend .eva/durable --output json
cargo run -- observability smoke --backend .eva/ci-observability --output json
cargo run -- release check --output json
cargo run -- release check --artifact-evidence release-evidence/release-artifact.evidence --output json
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
cargo run -- release check --security-scan-evidence release-evidence/release-security-scan.evidence --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release perf --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release migration --output json
```
