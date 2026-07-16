# eva-cli / 命令行入口

更新时间：2026-07-10

`eva-cli` 负责命令解析、文本/JSON 输出、trace 字段和 exit code 映射。当前命令面覆盖 basic/durable runtime、daemon/control mailbox、typed event emit、Agent 与 capability 操作、受控 Adapter/MCP/Skill 执行、Discovery、Memory、Observability、Hardware、Backup/Restore/Lifecycle 和 release gates；各命令实现位于 `src/run/` 子模块并复用统一 envelope。

CLI 不启动生产后台 daemon；`daemon start` 默认验证本机 pid/lock/state、durable backend、recovery、policy、observability、hardware hotplug subscriber、memory/knowledge maintenance 和 shutdown contract，`--no-shutdown-after-smoke` 才保持前台 mailbox loop。受控执行路径已包括 manifest-gated provider/MCP/Skill 调用、带确认和 policy gate 的 capability call、staged restore apply/rollback，以及本地 supervisor state/release-pointer handoff；这些能力不等同于 raw hardware I/O、平台 service-manager 命令或生产 OS 进程升级。

V1.15.8 的 CLI 变化是 runtime marker 公开 `memory_redaction_audit_v1.15.8`，表示 `memory context` 会使用 `memory_policy.redaction` 并写入 memory read/search/context audit/metrics JSONL。V1.16.1 的 CLI 变化是 runtime marker 公开 `runtime_audit_sink_wiring_v1.16.1`，表示 daemon recovery/control、task lifecycle、scheduler retry、provider supervision 和 restore apply/rollback 已写入现有 JSONL best-effort pipeline。V1.16.2 的 CLI 变化是 runtime marker 公开 `tracing_subscriber_bridge_v1.16.2`，并让 `observability smoke --tracing-sink jsonl|dev-console` 验证 tracing span/event 到现有 JSONL/dev-console sink 的映射和脱敏。V1.16.3 的 CLI 变化是 runtime marker 公开 `opentelemetry_sdk_exporter_v1.16.3`，并让 `observability smoke --otel-endpoint <url>` 显式验证 SDK OTLP trace/metrics exporter；未传 endpoint 时 `otel_exporter` 保持 `null`。V1.16.4 的 CLI 变化是 runtime marker 公开 `observability_retention_policy_v1.16.4`，表示 JSONL/durable-audit retention/rotation policy 已落地；它仍不表示真实 database sink 或生产检索调度。V1.17.2 的 CLI 变化是 runtime marker 公开 `operator_execution_fields_v1.17.2`，并把 `invocation_executed` 与 `mutation_executed` 作为 operator-facing 字段固定到 capability/restore/upgrade/hardware 关键输出。V1.17.3 的 CLI 变化是 runtime marker 公开 `operator_apply_text_v1.17.3`，并让 restore/upgrade/hardware 高风险 text 输出统一展示 plan、confirm token、target、final state、rollback path 和 risk 摘要；V1.17.4 的 CLI 变化是 runtime marker 公开 `json_contract_diff_suite_v1.17.4`，新增 `contracts/cli-json/*.json` golden subset fixtures 和 `scripts/validate-cli-json-contracts.ps1`，并让 `release check` 输出 `REL-JSON-CONTRACT-001`。V1.17.6 的 CLI 变化是 runtime marker 公开 `v1x_closure_gate_v1.17.6`，`release check` 新增 `REL-OBSERVABILITY-POLICY-001`、`REL-V1X-CLOSURE-001` 和 additive `closure` JSON；生产 release upload 仍因凭据/仓库权限阻塞，并会作为 external blocker 记录。

## 当前命令

| 命令 | 当前行为 |
| --- | --- |
| `eva version` / `eva --version` | 输出 V1.5 release label、runtime contract 和稳定命令契约。 |
| `eva doctor` | 检查 workspace、配置根、schema、Lua host crate 边界、runtime builder。 |
| `eva config validate` | 加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy、routes，并输出摘要。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 runtime service summary。 |
| `eva run --example basic` | 执行 V1.0 in-memory basic event loop，并写入本地或 durable backend task report。 |
| `eva daemon start/status/stop/shutdown/submit/cancel/drain/reload` | 验证 V1.12/V1.13.5/V1.15.4/V1.15.6/V1.16.1 daemon pid/lock/state、durable backend、provider/task recovery、policy、observability、hardware hotplug subscriber、memory/knowledge maintenance、JSONL audit wiring、shutdown contract 和本机 filesystem mailbox 控制面；`submit` 可传完整 TaskEnvelope；不启动 provider 进程，也不暴露 raw hardware handle。 |
| `eva emit` | 向 in-memory 或 durable EventBus 发布 typed Event，支持显式 Agent/Capability/Adapter target 和 trace metadata。 |
| `eva agent status/drain/reload` | 输出 Agent lifecycle evidence；连接 running daemon 时 `drain/reload` 写入 `agent-control.state`，无 daemon 时回退 `mutation_executed:false` evidence。 |
| `eva capability list/probe/call` | 展示 registry/provider plan；`call` 经过 permission/runtime policy gate，只有 `--confirm` 匹配 request id 且非 dry-run 时才执行。 |
| `eva task status` | 读取 `.eva/tasks` 或 `--durable-backend` 中最新或指定 task 的状态、attempts、retry、取消和 dead-letter 摘要；v3 记录额外显示 kind、Agent、input kind/size/digest、artifact ref、幂等键和 attempt policy，但不回显 inline 原文。 |
| `eva task logs` | 读取 task logs。 |
| `eva task cancel` | 对未终态 task 写入取消标记；对已终态 task 记录 cancel request 但不改变终态。 |
| `eva adapter list/probe` | 列出和 probe 已授权 Adapter handle，不启动真实 provider。 |
| `eva mcp list/probe` | 列出和 probe MCP allowlist tool，不启动真实 MCP server。 |
| `eva skill list/run` | 运行受控 workflow skill runner；先校验 schema/runtime gate 和 V1.9.2 policy runtime gate，再写 stdout/stderr/artifact evidence。 |
| `eva discovery scan` | 返回 discovery candidate 和 source reports，明确 discovery 不授予 runtime handle。 |
| `eva memory context` | 为单个 Agent 构造 request context，输出 private/global memory、knowledge、Lua snapshot 和 audit；可用 `--durable-backend` 走 V1.9.4 durable memory/knowledge round trip；V1.15.8 起写 `.eva/data/observability` memory JSONL evidence。 |
| `eva observability smoke` | 写入 V1.9.5 file JSONL observability backend，并在 V1.16.2 验证 tracing subscriber bridge、audit、runtime/provider/task metrics、OTel-style spans 和 best-effort degradation；V1.16.3 支持 `--otel-endpoint` 显式运行 SDK OTLP trace/metrics exporter smoke；V1.16.4 的 retention/rotation policy 由观测/存储 API 和 config/schema 暴露。 |
| `eva hardware list/probe/bind` | 发现、probe 并计划硬件绑定；高风险动作 plan-first，V1.3 不打开 raw I/O。 |
| `eva backup create` | 创建并校验 signed backup archive；`--encrypt` 生成 sealed archive metadata。 |
| `eva snapshot create/promote` | 创建绑定到 backup manifest 的 release snapshot；promote 生成带确认的 release-pointer plan，但不直接移动 pointer。 |
| `eva restore plan` | 生成 restore plan；V1.4 保持 `apply_allowed:false`。 |
| `eva restore apply` | `--dry-run` 校验 backup artifact 与 pre-restore backup evidence，并可输出 V1.14.1 `mutation_plan` 和 V1.14.4 `operator_confirmation`；非 dry-run 还要求 `--lock-store`、policy allow 和 health check。无 mutation steps 时输出 gated report 且 `mutation_executed:false`；有 staged steps 且全部 gate 通过时执行 V1.14.2 file mutation 并输出 `mutation_executed:true`；V1.17.2 让 dry-run 也显式输出 `mutation_executed:false`；V1.17.3 text 输出补 `final_state`、`rollback_path` 和风险摘要；V1.16.1 非 dry-run 成功路径写 `.eva/data/observability` JSONL。 |
| `eva restore rollback` | 复用 apply plan、confirmation、artifact evidence、policy、rollback lock 和 health gate，读取 `{plan_id}.restore.txn` 或 `--transaction-log`，只在 transaction status 为 `rollback_required` 时用 pre-restore archive entry 倒序恢复已提交步骤，并输出 `rollback_executed:true` 或二级失败 evidence；V1.17.2 同步输出顶层 `mutation_executed` 作为兼容性 operator 字段；V1.17.3 text 输出补 `final_state`、`rollback_path` 和风险摘要；V1.14.4 同样输出 `operator_confirmation`。 |
| `eva upgrade check/apply` | `check` 诊断 generation、migration、drain 和 rollback；`apply --state-store` 在 policy、lock、runtime-binary smoke 和 health gate 通过后提交本地 handoff state 与 release pointer，仍不执行平台 service-manager handoff。 |
| `eva release check` | 聚合跨平台、稳定性、文档、安全、性能、迁移、daemon runtime、hardware safety、V1.16 observability policy、V1.17.4 public JSON contract diff 和 V1.17.6 V1.x closure readiness 门禁；可读取 V1.11 artifact、distribution、security scan 和 benchmark evidence，输出 release readiness 与 additive `closure` report。 |
| `eva release security` | 输出 policy、sandbox、secret、MCP、hardware 和 lifecycle apply 风险的安全评审。 |
| `eva release perf` | 输出 EventBus、Scheduler、Adapter、memory、backup 和 release check 的性能预算基线；无 evidence 时明确为 `unmeasured`，可读取 benchmark evidence 展示观测，但 production 权威仍要求 verified envelope。 |
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

`eva daemon start/status/stop/shutdown/submit/cancel/drain/reload` 固定本机 daemon 进程边界和控制面，但不启动生产后台守护进程，也不启动外部 provider。`start` 默认运行 foreground smoke：在永久 `daemon.lock` anchor 上获取 OS lock，原子发布含 PID/process token/writer generation/heartbeat/expiry 的 `daemon.lease`，验证 durable backend，扫描 task/provider process recovery state，验证 policy domain 和 observability JSONL sink，运行 V1.15.4 manifest snapshot hotplug subscriber，执行 V1.15.6 durable memory/knowledge maintenance smoke，写入 `daemon.state` / 版本化 `daemon.pid` / `hardware-hotplug.state` / memory checkpoint / knowledge checkpoint，随后执行 shutdown contract、删除 PID 并将 lease 标为 `released`；未持锁的 `daemon.lock` anchor 永久保留。显式传入 `--no-shutdown-after-smoke` 时，命令保持前台运行并处理 `state/control/requests` 到 `state/control/responses` 的本机 filesystem mailbox，同时按固定间隔续租 heartbeat；status 只有在 state、完整 PID projection、fresh lease 与 live OS lock 全部一致时才报告可用。每轮 control polling 前会执行 V1.12.4 scheduler retry tick，把 due dead-letter replay event 投递到 scheduler mailbox，并以 `scheduler-retry` consumer 更新 durable log ack/fail。V1.16.1 后，daemon recovery/control、submit/cancel task lifecycle 和 scheduler retry tick 会 best-effort 写 audit/metric/span JSONL；sink 失败不改变 control flow。V1.12.5 后，`eva agent drain/reload` 可连接该 daemon 并写入 `agent-control.state`，记录 drain gate 和 reload 后新 work generation；V1.13.5 会把残留 active provider session 标记为 interrupted 并在 `recovery` JSON 中报告；V1.16.4 的 `version` runtime marker 包含 `mcp_http_auth_v1.13.6`、`mcp_compat_matrix_v1.13.7`、`provider_supervision_release_gate_v1.13.8`、`restore_staged_mutation_planner_v1.14.1`、`restore_file_mutation_engine_v1.14.2`、`restore_rollback_apply_v1.14.3`、`restore_operator_confirmation_v1.14.4`、`service_manager_abstraction_v1.14.5`、`hardware_os_permission_provider_v1.15.1`、`hardware_hotplug_subscriber_v1.15.4`、`hardware_safety_release_gate_v1.15.5`、`memory_knowledge_maintenance_v1.15.6`、`knowledge_retrieval_provider_v1.15.7`、`memory_redaction_audit_v1.15.8`、`runtime_audit_sink_wiring_v1.16.1`、`tracing_subscriber_bridge_v1.16.2`、`opentelemetry_sdk_exporter_v1.16.3` 和 `observability_retention_policy_v1.16.4`。daemon smoke 仍不启动 provider，也不是完整生产热更新 apply；V1.16.1 只证明现有 JSONL best-effort pipeline 的 runtime wiring；V1.16.2 只证明 tracing bridge 映射和脱敏；V1.16.3 只证明显式 `observability smoke --otel-endpoint` 的 SDK OTLP exporter smoke；V1.16.4 只证明 JSONL/durable-audit retention policy，不执行平台 service-manager 命令、真实 OS hotplug notification、真实硬件 I/O、生产检索调度、长驻 memory scheduler 或真实 database sink。

```powershell
cargo run -- daemon start --foreground --dev --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --observability-backend .eva/daemon-observability --output json
cargo run -- daemon start --foreground --dev --no-shutdown-after-smoke --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --observability-backend .eva/daemon-observability --output json
cargo run -- daemon status --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --output json
cargo run -- daemon submit --task req-daemon-task-1 --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --output json
cargo run -- daemon submit --task req-daemon-task-2 --kind runtime.echo --agent root-agent --input "hello" --idempotency-key idem-daemon-task-2 --max-attempts 3 --retry-backoff-ms 250 --attempt-timeout-ms 5000 --durable-backend .eva/daemon-durable --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --output json
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
| daemon `submit` / `cancel` | `submit` 写入 durable `queued` lifecycle 与不可变 TaskEnvelope，并只在 envelope 内携带 Agent 身份；只有旧参数时生成 `legacy.submit`、首个 enabled Agent、空 inline input、task-ID 幂等键和单次 attempt。显式模式要求 `--kind`、`--agent` 以及 `--input` 或 artifact ref/digest；`cancel` 只推进 lifecycle 并保留原信封。 |

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

`eva memory context` 是 `eva-memory` 和 `eva-lua-host` smoke。默认使用 in-memory seed，并按 `memory_policy.redaction` 处理敏感字段；传入 `--durable-backend <path>` 时会写入 durable backend 的 `state/memory` 与 `state/knowledge` 后重新读取构建 context。V1.15.8 起该命令还会写入 `.eva/data/observability` 中的 memory read/search/context audit 和 metrics JSONL：

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

V1.16.2 后，该命令默认通过 tracing subscriber bridge 验证 span/event 到 JSONL sink 的映射；可用 `--tracing-sink dev-console` 验证脱敏后的 console 输出。V1.16.3 后，只有显式传入 `--otel-endpoint <url>` 时才运行 OpenTelemetry SDK OTLP HTTP/protobuf trace/metrics exporter smoke；collector 不可用时 `otel_exporter.degraded:true`，命令仍返回成功。未传 endpoint 时 JSON 中 `otel_exporter` 为 `null`。V1.16.4 的 retention/rotation policy 当前由 `eva-observability` 和 `eva-storage` API 测试覆盖，CLI 不隐式删除观测文件。

```powershell
cargo run -- observability smoke --backend .eva/ci-observability --output json
cargo run -- observability smoke --backend .eva/ci-observability --tracing-sink dev-console --output json
cargo run -- observability smoke --backend .eva/ci-observability --otel-endpoint http://127.0.0.1:4318 --otel-timeout-ms 500 --output json
cargo run -- observability smoke --backend .eva/ci-observability
```

JSON output contains `backend_root`, `degraded`, `degraded_reasons`, `audit_events`, `metric_points`, `otel_spans`, `continuity_key`, `tracing_bridge`, and `otel_exporter`. Backend files are written as:

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
| `hardware bind --adapter <id>` | 生成绑定计划、OS permission evidence、风险提示、plan steps、V1.9.2 policy audit 和 `mutation_executed:false`；disabled/rejected 设备返回 `blocked`。 |
| `hardware bind --apply` | V1.15.1/V1.17.2 只校验逻辑计划、runtime policy 和平台权限诊断；权限缺失时 `blocked`，不 claim lease，不打开真实设备，并保持 `mutation_executed:false`；V1.17.3 text 输出展示 operator summary、target、final state、rollback path 和风险摘要。 |

`scale-main` 默认 disabled，因此 JSON 中会看到：

- `trust: "rejected"`
- `health: "disconnected"`
- `handle_granted: false`
- `rejected_reason: "hardware adapter manifest is disabled"`
- `hardware bind` 的 `status: "blocked"`
- `permission.raw_device_path_exposed:false`，且 permission remediation 不包含 raw device handle

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
| `upgrade check` | 输出 supervisor candidate、migration preflight、drain plan、rollback plan 和 `mutation_executed:false`。 |
| `upgrade apply` | 未传 `--state-store` 时保持 lock-only 输出并报告 `mutation_executed:false`；传入 `--state-store` 且 project policy 同时允许 `supervisor.handoff` 和 `release.pointer_mutation` 时，会提交本地 blue-green handoff、写 `state/release-pointer`、持久化 `handoff.prepared` / `handoff.committed`，health 失败时输出 rollback plan 且不写 pointer；顶层 `mutation_executed` 与 `handoff.mutation_executed` 保持一致；V1.17.3 text 输出展示 operator summary、target generation、final state、rollback path 和风险摘要。 |

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

这些字段在 `--dry-run` 中只生成 `mutation_plan`（`preview`、`affected_paths`、`preflight_hash`、`rollback_manifest`）和 `operator_confirmation`（`confirm_token`、`target_root`、`affected_count`、状态位和不可逆风险）。非 dry-run 且 confirmation、artifact evidence、policy、lock 和 health gate 全部通过后，会执行 staged file mutation 并写 transaction log。

V1.10.4/V1.14.1/V1.14.2/V1.14.3/V1.14.4 `restore apply`/`restore rollback` 完成受控 destructive apply gate、staged mutation preview、staged file mutation engine、failed transaction rollback apply 和 operator confirmation 输出：confirmation、artifact evidence、policy approval、lock 和 health check 都通过后，只有 plan 声明 mutation steps 时才写目标文件；rollback 只接受 rollback-required transaction log 和可校验 pre-restore archive entry。它不移动 release pointer，不启动真实 Supervisor/Runtime 进程。

`upgrade apply` 不传 `--state-store` 时只创建 filesystem lock 并保持
`apply_allowed:false`，用于兼容旧的 plan-first lock path。V1.10.5 开始，传入
`--state-store` 后会在 policy allow、lock、runtime binary smoke 和 health gate
通过后提交本地 supervisor handoff state，并写入 `state/release-pointer`。这仍是
local supervisor adapter smoke，不是生产 service manager/daemon handoff。V1.14.5 新增的 service-manager abstraction marker 只标识 trait、typed config、fake handoff/rollback evidence 和 release gate 已存在，`upgrade apply` 仍未接入平台 service-manager handoff。

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
| `release check` | 调用 `eva_release::ReleaseHardeningService::readiness`，聚合 release gates、platform readiness、stability scenarios 和 audit，包括 V1.15.5 `REL-HARDWARE-SAFETY-001`、V1.16 `REL-OBSERVABILITY-POLICY-001`、V1.17.4 `REL-JSON-CONTRACT-001` 与 V1.17.6 `REL-V1X-CLOSURE-001`。 |
| `release check --artifact-evidence` | 读取 V1.11.1 key/value artifact evidence，校验 signed artifact、SHA-256 keyed signature、source commit provenance、SBOM 标记和 scan status；失败时 required gate blocked 并返回配置门禁 exit code `2`。 |
| `release check --distribution-evidence` | 读取 V1.11.2 key/value distribution evidence，校验 Windows/Linux/macOS install smoke、安装/升级/卸载文档路径和 package-manager dry-run；失败时 required gate blocked 并返回配置门禁 exit code `2`。 |
| `release check --security-scan-evidence` | 读取 V1.11.3 key/value external scanner evidence；scanner skipped/failed 或 high/critical finding 会阻断并返回配置门禁 exit code `2`。 |
| `release check --benchmark-evidence` | 读取 V1.11.3 legacy alpha benchmark evidence；空样本、非 passed 状态、未知/漂移的 claimed budget 或 observed latency 超过 consumer-owned policy 都会阻断并返回配置门禁 exit code `2`。 |
| `release security` | 输出 `SecurityReviewReport`，覆盖 policy、Lua sandbox、secret redaction、MCP allowlist、hardware raw I/O 和 lifecycle apply risk。 |
| `release perf` | 输出 `PerformanceBaselineReport`；默认预算没有伪造 observed 值，状态为 `unmeasured`。传入 `--benchmark-evidence <path>` 时展示测量并强制 consumer-owned budget policy，但该诊断路径本身不授予 production evidence 权威。 |
| `release migration` | 输出 `MigrationGuide` 和 `CompatibilityPolicy`，声明 V1.4 到 V1.5 无破坏性变更。 |

这些 release 命令不修改 `.eva/tasks`，不启动外部 provider，不执行真实 restore、supervisor handoff 或真实硬件 I/O；V1.10.5 的本地 handoff/pointer mutation 只存在于单独的 `upgrade apply --state-store` 路径。V1.15.5 的 hardware safety gate 接受 alpha simulator-only evidence，生产 release 仍必须补真实或虚拟硬件 fixture evidence。阻断门禁会映射到稳定 exit code：配置门禁 `2`、policy 门禁 `3`、性能门禁 `4`。

V1.11.4 已把 `version`、`doctor`、`config validate`、`inspect`、`task`、`adapter`、`mcp`、`skill`、`discovery`、`memory`、`observability`、`hardware`、`backup`、`snapshot`、`restore`、`upgrade` 和 release 命令组的 parser/writer/formatter 拆入 `src/run/` 子模块；V1.17.1 继续把 `run --example basic` 的 parser、runtime glue、task snapshot 写入和 text/JSON writer 拆入 `src/run/run_cmd.rs`；V1.17.2 补齐 operator execution-state 字段；V1.17.3 补齐高风险 apply text summary；V1.17.4 新增 public JSON contract diff suite，使用 golden subset fixtures 阻断删除/重命名字段并允许新增字段；V1.17.6 在 `release check` 增加 observability policy gate、V1.x closure gate 和 additive `closure` JSON。`run.rs` 保留顶层 dispatch、共享 formatter helper、trace 和 exit code 映射。拆分后的回归继续覆盖 version text/JSON、doctor sample project、config validation JSON、inspect text/durable diagnostics JSON、run basic JSON、task store JSON、外部能力诊断 JSON、skill run JSON、discovery source report JSON、memory/observability JSON、hardware list/probe/bind JSON、backup/lifecycle JSON、restore/upgrade apply gate JSON、`release_check`、`release_perf`、V1.5 release hardening JSON contract 和 `scripts/validate-cli-json-contracts.ps1`。

## 验证

```powershell
cargo test -p eva-cli
./scripts/validate-cli-json-contracts.ps1
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
cargo run -- observability smoke --backend .eva/ci-observability --tracing-sink dev-console --output json
cargo run -- observability smoke --backend .eva/ci-observability --otel-endpoint http://127.0.0.1:4318 --otel-timeout-ms 500 --output json
cargo run -- release check --output json
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
cargo run -- release check --security-scan-evidence release-evidence/release-security-scan.evidence --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release perf --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release migration --output json
```

当前测试覆盖 version text/JSON、config validate JSON、inspect text/durable diagnostics JSON、unknown command、JSON escaping、basic run JSON、cancelled basic run、daemon foreground smoke/lock conflict/bad durable backend、TaskEnvelope 显式/legacy submit、非法 digest 预投递拒绝、状态元数据与 inline 原文不泄露、agent daemon drain/reload mutation fallback、task status/logs/cancel、doctor sample project、V1.1 external capability commands、V1.2 memory context、V1.15.8 memory context observability、V1.3 hardware command JSON、V1.4 backup/lifecycle command JSON、V1.5 release hardening command JSON、V1.9.5 observability smoke JSONL backend、V1.10.4 restore apply policy denial、lock conflict、health failure rollback、gated report contract、V1.14.1 staged mutation plan preview/digest contract、V1.14.2 staged mutation apply contract、V1.14.3 restore rollback contract、V1.14.4 operator confirmation contract、V1.16.1 restore apply/rollback observability evidence、V1.16.2 tracing bridge smoke、V1.16.3 OTel exporter degraded smoke 和 V1.16.4 retention marker 断言，以及 V1.11.1 artifact evidence / V1.11.2 distribution evidence / V1.11.3 security scan and benchmark evidence release gates。
