# Eva-CLI 使用手册

更新时间：2026-07-14

适用版本：Eva-CLI `1.11.5-alpha`（源码版本）

Eva-CLI 是 Eva 本地运行时的命令行入口。当前版本适合从源码进行配置校验、运行 basic 示例、检查任务和运行时边界，以及验证受控的 Adapter、MCP、Skill、备份和发布流程；它还不是可直接部署的生产守护服务。

## 使用前须知

- 本手册中的命令默认从仓库根目录运行，并统一使用 `cargo run -q -- <eva 参数>`。构建后也可以把这部分替换为 `target/debug/eva`、`target/release/eva` 或 Windows 下对应的 `.exe`。
- `run --example basic` 使用 `<项目根>/examples/basic` 中的示例配置，但任务快照默认写入 `<项目根>/.eva/tasks`。
- `daemon` 当前提供本机前台进程、文件锁、PID、状态和控制邮箱边界，不会启动生产 provider 监督进程。
- `restore apply/rollback` 和带 `--state-store` 的 `upgrade apply` 可以修改本地文件或状态。执行前必须核对 plan、确认令牌、policy、artifact、lock 和 health gate。
- 外部 Adapter、MCP 或 Skill 是否真的执行，取决于 manifest、allowlist、policy、provider 可用性和命令是否已确认。不要把 `probe` 或 dry-run 的成功当作外部操作已经完成。

![Eva-CLI 日常使用路径](../../assets/eva-cli-user-manual-flow.zh-CN.svg)

## 安装与构建

需要先安装 Git 和 stable Rust toolchain（包含 `cargo`）。

```powershell
git clone https://github.com/Yetmos/Eva-CLI.git
cd Eva-CLI
cargo build --release --locked --bin eva
cargo run -q -- --version
```

版本输出的前两行应为：

```text
eva 1.11.5-alpha
release: V1.11.5-alpha
```

当前仓库不提供签名安装器或 Homebrew、Winget、Apt 软件源。其他安装、升级和卸载边界见[安装升级卸载说明](../release/安装升级卸载说明.md)。

## 快速开始

按以下顺序可以完成一次最小可用性检查：

```powershell
cargo run -q -- doctor
cargo run -q -- config validate
cargo run -q -- inspect runtime
cargo run -q -- run --example basic
cargo run -q -- task status
cargo run -q -- task logs
```

预期结果：

1. `doctor` 找到 workspace、`config/eva.yaml`、拆分配置目录和 schema；warning 不等同于命令失败。
2. `config validate` 完成跨文件校验，并输出 Agent、Adapter、Capability、Policy 和 Route 数量。
3. `inspect runtime` 输出当前配置和 no-op runtime 边界摘要，不代表所有标为 `planned` 的服务都已在生产模式运行。
4. `run --example basic` 执行受限 Lua basic loop，并写入任务快照。
5. `task status/logs` 读取刚才生成的任务状态和日志。

脚本或 CI 需要机器可读结果时，在命令末尾加 `--output json`。

## 命令总览

| 命令组 | 主要用途 | 默认副作用 |
| --- | --- | --- |
| `version`、`doctor`、`config validate`、`inspect` | 版本、环境、配置和运行时诊断 | 只读 |
| `run`、`task`、`emit` | 运行 basic 示例、管理任务快照、发布 typed event | `run` 和 `task cancel` 写任务状态；`emit` 仅在指定 durable backend 时持久化 |
| `daemon`、`agent` | 本机 daemon smoke/control 和 Agent lifecycle | 写本地 daemon 状态；连接运行中的 daemon 时 drain/reload 可写控制状态 |
| `capability`、`adapter`、`mcp`、`skill`、`discovery` | 能力路由、外部 provider 和发现 | list/scan 主要只读；确认调用或 Skill runner 可能执行外部程序 |
| `memory`、`observability` | 构造演示上下文，验证 audit/metric/trace sink | 可写 durable memory/knowledge 和 observability 文件 |
| `hardware`、`backup`、`snapshot`、`restore`、`upgrade` | 硬件计划、备份和生命周期操作 | 从 plan-only 到受门禁保护的本地文件/状态修改 |
| `release` | 发布准备、安全、性能和迁移检查 | 只读；可读取外部 evidence 文件 |

完整参数以当前代码生成的帮助为准：

```powershell
cargo run -q -- --help
```

## 通用参数与输出

多数命令支持以下选项：

| 选项 | 说明 |
| --- | --- |
| `--project <path>`、`-p <path>` | 指定 Eva 项目根目录；默认是当前目录。 |
| `--output text`、`-o text` | 人工阅读的文本输出，通常为默认值。 |
| `--output json`、`-o json` | 稳定 JSON envelope，适合脚本和 CI。 |
| `--help`、`-h` | 显示当前完整帮助。 |
| `--version`、`-V`（单独使用） | 显示版本、release label、runtime marker 和支持的 command contract。 |

例如，从仓库外诊断项目：

```powershell
cargo run -q -- doctor --project C:\path\to\Eva-CLI --output json
```

## 配置与只读诊断

```powershell
cargo run -q -- doctor --output json
cargo run -q -- config validate --output json
cargo run -q -- inspect all --output json
cargo run -q -- inspect config --output json
cargo run -q -- inspect runtime --output json
```

主要配置位置：

| 路径 | 用途 |
| --- | --- |
| `config/eva.yaml` | runtime、state、memory、observability 和拆分配置入口 |
| `config/agents/` | Agent manifest 与 Lua handler |
| `config/adapters/` | stdio、HTTP、MCP、Skill 和 hardware Adapter manifest |
| `config/capabilities/` | Capability 声明与 provider 选择 |
| `config/policies/` | sandbox、MCP、memory、hardware 和 lifecycle policy |
| `config/routes/topics.yaml` | Topic 路由 |
| `config/schemas/` | 配置 JSON Schema |

修改任一配置后，至少重新运行 `config validate`。`doctor` 的职责更宽，会同时检查目录、schema 和 runtime 边界；`inspect` 用于查看加载后的有效摘要。

当前 `inspect all/config/runtime/routes/policy/agents/adapters/capabilities` 都返回同一份项目全景报告，并不会按主题过滤；只有 `inspect durable` 进入独立的 durable backend 诊断路径。

已有 durable backend 时，可以单独检查其布局、schema、迁移状态和待 redrive 数量：

```powershell
cargo run -q -- inspect durable --durable-backend .eva/durable --output json
```

## 运行示例、任务与事件

### 运行 Basic 示例

```powershell
cargo run -q -- run --example basic --task-id req-demo-1 --output json
cargo run -q -- task status --task req-demo-1 --output json
cargo run -q -- task logs --task req-demo-1 --output json
```

常用运行参数：

| 选项 | 默认值 | 说明 |
| --- | --- | --- |
| `--task-id <id>` | `req-basic-1` | 指定 request/task id。 |
| `--timeout-ms <ms>` | `30000` | Lua handler 超时预算；`--no-timeout` 可关闭预算。 |
| `--retry-attempts <n>` | `1` | 最少执行 1 次的重试上限。 |
| `--cancel` | 关闭 | 在 handler 前走取消诊断路径。 |
| `--replay-dead-letters` | 关闭 | 为 dead-letter 生成 replay receipt 摘要。 |
| `--durable-backend <path>` | 未设置 | 把任务快照写到 durable backend 的 `tasks/`，而不是 `.eva/tasks`。 |

`task cancel` 修改已持久化的 task 状态，不等同于向另一个正在运行的 CLI 进程发送实时中断。运行中 daemon 的任务使用 `daemon cancel`。

### 发布类型化事件

```powershell
cargo run -q -- emit /input/user --event-id evt-manual-1 --payload hello --output json
cargo run -q -- emit /input/user --payload-bytes-hex 68656c6c6f --target-agent root-agent --durable-backend .eva/durable --output json
```

- `--payload` 按 UTF-8 文本透传；即使内容是 JSON，也不会自动解析为结构化对象。
- `--payload-empty` 与 `--payload-bytes-hex <hex>` 分别表示空 payload 和二进制 payload。
- `--target-agent`、`--target-capability`、`--target-adapter` 互斥；不指定时为 broadcast。
- 未指定 `--durable-backend` 时，事件只发布到本次命令的 in-memory EventBus 并返回 receipt；该命令本身不会启动 Agent handler。

## Daemon 与 Agent 控制

不带 `--no-shutdown-after-smoke` 的 `daemon start` 会完成一次启动检查后立即 shutdown，并清理 lock/PID。要验证控制邮箱，需要两个终端。

终端 A：

```powershell
cargo run -q -- daemon start --foreground --dev --no-shutdown-after-smoke --output json
```

终端 B：

```powershell
cargo run -q -- daemon status --output json
cargo run -q -- daemon submit --task req-daemon-1 --output json
cargo run -q -- daemon cancel --task req-daemon-1 --reason "manual stop" --output json
cargo run -q -- daemon shutdown --output json
```

`status`、`submit`、`cancel`、`drain`、`reload` 和 `shutdown` 通过文件系统 control mailbox 与前台 daemon 通信。即使是一次性 smoke，`daemon start` 也会以读写模式打开 durable backend，并执行 recovery scan、memory maintenance 和 scheduler retry 边界；但 `provider_processes_started` 仍为 `false`。

默认目录由 `runtime.data_dir` 派生；示例配置使用 `.eva/data/durable`、`.eva/data/daemon/state`、`.eva/data/daemon/locks`、`.eva/data/daemon/pids` 和 `.eva/data/observability`。`--background` 虽可被解析，但当前运行时尚未实现后台模式，请保持终端 A 前台运行。

Agent 命令可以单独查看或规划 lifecycle：

```powershell
cargo run -q -- agent status --agent root-agent --output json
cargo run -q -- agent drain --agent root-agent --generation gen-current --output json
cargo run -q -- agent reload --agent root-agent --from-generation gen-old --to-generation gen-new --output json
```

没有可用 daemon 时，`agent drain/reload` 返回本地计划，`mutation_executed` 为 `false`；连接到相同 state/lock/PID 目录中的运行中 daemon 后，命令才会写 daemon-side Agent 控制状态。这不等同于完整 provider 重启或生产热更新。

## Capability、Adapter、MCP 与 Skill

先 list/probe，再决定是否执行：

```powershell
cargo run -q -- capability list --output json
cargo run -q -- capability probe repo.analyze --output json
cargo run -q -- adapter list --output json
cargo run -q -- adapter probe --adapter github-mcp --output json
cargo run -q -- mcp list --output json
cargo run -q -- mcp probe --adapter github-mcp --tool list_issues --output json
cargo run -q -- skill list --output json
cargo run -q -- discovery scan --output json
```

`discovery scan` 只返回候选，不授予 runtime handle。MCP tool 或 provider 不在 manifest allowlist、policy 不允许、凭据缺失或进程不可用时，probe/call 会返回 blocked 或 unavailable。

`capability call` 默认是 dry-run。只有 `--confirm` 与 `--request-id` 完全一致时才进入执行路径：

```powershell
cargo run -q -- capability call config.lint --input config --request-id req-cap-1 --output json
cargo run -q -- capability call config.lint --input config --request-id req-cap-1 --confirm req-cap-1 --output json
```

确认调用以及 `skill run` 可能启动 manifest 允许的外部 runner。执行前先检查对应 `config/adapters/`、`config/capabilities/` 和 `config/policies/`，不要仅根据命令名称判断副作用。

例如，运行示例配置中 allowlist 允许的 Skill workflow：

```powershell
cargo run -q -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
```

## Memory 与 Observability

```powershell
cargo run -q -- memory context --agent root-agent --query context --private-limit 1 --output json
cargo run -q -- observability smoke --backend .eva/observability --output json
```

`memory context` 当前构造的是演示上下文。未指定 durable backend 时，seed memory/knowledge 只存在于进程内，但 audit/metric 会写入 `runtime.data_dir` 下的 `observability/`；指定 `--durable-backend` 后，演示记录也会写入该 durable store。

`observability smoke` 会在 `--backend` 下写 audit、metric 和 tracing JSONL。还可以用 `--tracing-sink dev-console` 检查控制台 bridge，或用 `--otel-endpoint` 验证 OTLP HTTP exporter；外部 collector 不可用时应检查输出中的 degraded 状态。

## 备份、恢复与升级

![Eva-CLI 副作用与安全边界](../../assets/eva-cli-user-manual-safety.zh-CN.svg)

先使用不会修改待恢复或待升级目标的命令检查计划。下面的 backup、snapshot 和 restore plan 仍会在指定的 artifact store 中写入合成制品：

```powershell
cargo run -q -- hardware list --output json
cargo run -q -- hardware probe --adapter scale-main --output json
cargo run -q -- hardware bind --adapter scale-main --output json
cargo run -q -- backup create --artifact-store .eva/artifacts --output json
cargo run -q -- snapshot create --snapshot-id snapshot-manual --artifact-store .eva/artifacts --output json
cargo run -q -- restore plan --snapshot-id snapshot-manual --artifact-store .eva/artifacts --output json
cargo run -q -- upgrade check --output json
```

当前边界：

| 命令 | 实际行为 |
| --- | --- |
| `hardware bind [--apply]` | 输出 permission、plan 和风险；当前 CLI 不打开 raw I/O handle。 |
| `backup create` | 创建并校验内置示例内容生成的 contract/smoke artifact，并不备份当前工作区；指定 store 后写盘，`--dry-run` 也可能写 artifact。 |
| `snapshot create` | 基于合成 backup evidence 创建 snapshot；指定 store 时持久化 artifact，不代表已经捕获真实工作区。 |
| `snapshot promote` | 每次重新创建合成 backup/snapshot，再校验确认值并生成 pointer plan；即使确认不匹配，也可能已经写入 artifact，但不会移动 release pointer。 |
| `restore plan` | 基于合成 snapshot/backup 返回恢复计划，可能写 artifact store；`apply_allowed:false`，不修改恢复目标。 |
| `restore apply --dry-run` | 解析 plan 并输出 mutation preview、影响路径、确认信息和 rollback manifest。 |
| `restore apply` | 所有 gate 通过后执行 plan 中声明的 staged copy/replace/delete，并写 transaction log；失败时可能要求 rollback。 |
| `restore rollback` | 只接受可校验的 rollback-required transaction，并按 pre-restore evidence 逆序恢复。 |
| `upgrade apply` | 不带 `--state-store` 时保持 lock-only；带 store 且 policy 通过后会写 handoff state，全部 gate 通过才写 release pointer；`--runtime-binary` 会实际执行指定二进制的 `--version`。 |

`restore plan` 和 `upgrade check` 只输出诊断报告，不会生成 apply 所需的严格 `key=value` plan 文件。下面的 plan 必须由受审计的 operator/release 流程按对应 contract 生成；尖括号内容不能直接照抄：

```powershell
cargo run -q -- restore apply --dry-run --plan <restore-plan> --confirm <plan-id> --artifact-store <artifact-dir> --lock-store <lock-dir> --output json
cargo run -q -- restore apply --plan <restore-plan> --confirm <plan-id> --artifact-store <artifact-dir> --lock-store <lock-dir> --output json
cargo run -q -- restore rollback --plan <restore-plan> --confirm <plan-id> --artifact-store <artifact-dir> --lock-store <lock-dir> --transaction-log <transaction-log> --output json
cargo run -q -- upgrade apply --plan <upgrade-plan> --confirm <plan-id> --lock-store <lock-dir> --state-store <state-dir> --output json
```

示例配置默认没有 `runtime_policy.allow_high_risk_actions`，因此 `restore apply`（非 dry-run）、`restore rollback` 和带 state store 的 `upgrade apply` 会被 policy 拒绝。只有经过独立审核的 policy 显式允许 `restore.apply`（restore 与 rollback 共用）、`supervisor.handoff` 和 `release.pointer_mutation` 后，相关 mutation 才可能通过；不要为了让命令返回成功而临时放开这些 action。

即使 binary probe 或 health check 失败，`upgrade apply --state-store` 也可能写入 `handoff.prepared`，但不会提交 release pointer。这些命令只实现本地受控边界，不会完成平台 service-manager 激活。plan 文件和 artifact contract 见[备份、迁移包与 ReleaseSnapshot 架构方案](../operations/备份迁移包与ReleaseSnapshot架构方案.md)。

## 发布检查

```powershell
cargo run -q -- release check --output json
cargo run -q -- release check --target windows --output json
cargo run -q -- release security --output json
cargo run -q -- release perf --output json
cargo run -q -- release migration --output json
```

- `release check` 聚合平台、稳定性、文档、安全、性能、迁移、daemon、observability 和公开 JSON contract gate。
- 可通过 `--artifact-evidence`、`--distribution-evidence`、`--security-scan-evidence` 和 `--benchmark-evidence` 提供外部证据。
- `status: ready` 只表示仓库内 required gate 通过。仍需检查 `closure.status` 和 `closure.blocked_external_items`；生产签名、包仓库、平台 service-manager、硬件 fixture 等外部事项可能仍未完成。

## JSON Envelope 与退出码

成功响应的基本结构：

```json
{
  "ok": true,
  "command": "config.validate",
  "exit_code": 0,
  "data": {},
  "trace": {}
}
```

参数已成功解析且由子命令处理的执行错误，在 `--output json` 下使用相同 envelope，并增加 `error`。发生在输出格式确定前的解析错误，以及未被子命令捕获的顶层执行错误，会直接写文本到 stderr。

| 退出码 | 含义 | 建议 |
| --- | --- | --- |
| `0` | 成功 | 继续检查业务字段；成功不一定代表外部 blocker 已消失。 |
| `1` | 内部错误 | 保留 stdout/stderr 和复现命令。 |
| `2` | 配置、路径、schema、evidence 或状态无效 | 运行 `doctor` 和 `config validate`，再检查 error context。 |
| `3` | policy 拒绝 | 核对 manifest 权限、policy 和确认信息。 |
| `4` | runtime 不可用、gate 未满足或当前版本不支持 | 查看 status、gate、health 和 rollback 字段。 |
| `5` | 外部 capability/provider 不可用或调用超时 | 检查 executable、网络、凭据、allowlist、timeout 和 provider health。 |
| `64` | 命令用法错误 | 运行 `cargo run -q -- --help`。 |

## 常见问题

| 现象 | 处理 |
| --- | --- |
| 找不到 `config/eva.yaml` | 从仓库根目录执行，或传入 `--project <path>`。 |
| `doctor` 成功但有 warning | 查看每个 warning 的 suggestion；只要 error count 为 0，命令仍可能返回成功。 |
| `task status` 找不到任务 | 先运行 `run --example basic`；使用 durable backend 时，查询命令必须传入同一路径。 |
| `daemon status` 返回 unavailable | 默认 start 是一次性 smoke；使用 `--no-shutdown-after-smoke` 保持终端 A 运行，并确保控制命令使用相同项目和路径。 |
| Adapter/MCP/Skill 被 blocked | 检查 manifest 是否 enabled、能力/tool 是否在 allowlist、policy 是否允许，以及 provider 是否可用。 |
| 硬件绑定一直 blocked | 示例 `scale-main` 默认禁用且使用占位 USB ID；当前版本不会授予 raw I/O handle。 |
| `release check` ready 但仍有 blocker | 查看 `closure.blocked_external_items`，区分仓库内 gate 与生产外部依赖。 |

## 当前限制

当前源码版本尚不提供：

- 签名安装器、生产 signing/attestation credential 和 Homebrew/Winget/Apt 发布；
- 生产后台 daemon、平台 service-manager handoff 和完整 provider 进程监督；
- OS credential vault 隔离、生产 MCP streaming/TLS 认证；
- 真实硬件 driver/raw I/O 和发布 fixture；
- 生产 observability database sink/retention scheduler，以及长驻 memory/retrieval 调度。

完整状态见[V1.x 未完整实现功能清单](../planning/V1.x未完整实现功能清单.md)。
