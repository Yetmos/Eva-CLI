# eva-lifecycle / 生命周期管理

更新时间：2026-07-20

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-lifecycle` 是 V1.4/V1.10 的 Supervisor、runtime generation、drain、rollback、upgrade apply lock 和 blue-green handoff 边界。它管理运行时代际切换和失败恢复，不承载 Lua 业务决策，不生成 backup artifact，也不绕过 `eva-backup` 的 snapshot/restore plan 校验。

V1.10.5 之后，`upgrade apply --state-store <path>` 可以在 confirmation、apply lock、`supervisor.handoff` policy、`release.pointer_mutation` policy、runtime binary smoke 和 candidate health 都通过后提交建模的 handoff，并在 state store 中写入 `state/release-pointer`。当前还实现了 host-bound Windows Service/systemd/launchd Adapter、规范化 daemon service argv identity，以及把 SCM control 或 Unix signal 转换为 cooperative stop token 的 direct entrypoint 边界；token 由 runtime 复用既有 drain/shutdown 事务。真实 host stop/boot/reboot transcript、destructive harness、production gate 和真实 blue-green 切流仍不在该代码证据内。

## 已实现能力

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| Generation | 已完成 V1.4 | `RuntimeGeneration` 和 `GenerationController` 支持 active/candidate、promote、failed candidate、old generation draining。 |
| Drain | 已完成 V1.4 | `DrainCoordinator` 输出 plan/completed/timed_out，并显式 `accepts_new_work:false`。 |
| Rollback | 已完成 V1.4 | `RollbackCoordinator` 根据 failed handoff 和可选 `RestorePlan` 生成 rollback steps、risks、audit。 |
| Supervisor | 已完成 V1.4 | `InMemorySupervisor` 支持 start candidate、commit healthy candidate 和 structured report。 |
| Upgrade apply lock | 已完成 V1.10.3 | `UpgradeApplyCoordinator` 获取 filesystem lock，冲突返回稳定 `Conflict`。 |
| Blue-green handoff | 已完成 V1.10.5 | `SupervisorHandoffCoordinator` 验证 policy、lock、runtime binary smoke 和 health，提交 candidate generation、drain 旧 generation、写 release pointer，并持久化 handoff state。 |
| Service-manager adapters | 代码路径已实现 | `ServiceManagerAdapter` 定义 typed 边界；Windows Service/systemd/launchd Adapter 生成并校验平台定义，`FakeServiceManagerAdapter` 仅覆盖本地开发 evidence。 |
| Direct service entrypoint | 代码路径已实现 | `ServiceManagerEntryPoint` 绑定 executable、native argv、working directory、kind/name 与 identity digest；`run_service_entrypoint` 在 Windows 进入 SCM dispatcher，在 Unix 安装只写 atomic token 的 stop handler。 |
| Backup integration | 已完成 V1.4 | rollback 可携带 `eva-backup::RestorePlan` 的 snapshot/risk 信息。 |
| CLI | 已完成 V1.10.5 | `eva upgrade check` 输出 readiness；`upgrade apply --state-store` 可提交受控 handoff/pointer mutation。 |

## Public API

| 类型/函数 | 说明 |
| --- | --- |
| `GenerationState` | `pending`、`active`、`draining`、`retired`、`failed`。 |
| `RuntimeGeneration` | generation id、release ref 和 state。 |
| `GenerationController` | active/candidate 状态机，支持 start/promote/fail candidate。 |
| `DrainCoordinator` | 创建 drain plan，完成或标记 timeout。 |
| `RollbackCoordinator` | 为 failed handoff 生成 rollback plan，可纳入 backup restore plan 风险。 |
| `InMemorySupervisor` | V1.4 mock supervisor，用于验证 generation handoff 语义。 |
| `RuntimeHealth` | candidate runtime health input。 |
| `SupervisorReport` | active/candidate generation、health、audit 摘要。 |
| `SupervisorHandoffCoordinator` | V1.10.5 blue-green handoff 协调器，负责 policy 后的 candidate commit、drain、release pointer mutation 和 rollback 输出。 |
| `RuntimeBinaryProbe` | runtime binary smoke 结果；CLI 默认使用 managed-by-cli simulated probe，也可传 `--runtime-binary <path>`。 |
| `FileSystemSupervisorStateStore` | 将 `handoff.prepared`、`handoff.committed` 和 `state/release-pointer` 写入本地 state store。 |
| `SupervisorHandoffReport` | handoff 状态、apply/mutation 标记、runtime binary、release pointer、rollback、steps/risks/audit。 |
| `ServiceManagerAdapter` | OS service-manager typed adapter trait；Windows Service/systemd/launchd production Adapter 均已接线。 |
| `ServiceManagerEntryPoint` | 规范化 direct daemon executable/argv/working directory，并以稳定 digest 绑定 identity。 |
| `run_service_entrypoint` | 校验 host kind/name 后进入 Unix signal bridge 或 Windows SCM dispatcher；回调只收到 cooperative stop token。 |
| `ServiceStopToken` | 可克隆、幂等的 atomic stop token；具体 drain/shutdown 由 `eva-runtime` 执行。 |
| `FakeServiceManagerAdapter` | 本地测试用 fake adapter，验证 candidate handoff、health failure block 和 rollback audit。 |

## CLI 验证入口

```powershell
cargo run -- upgrade check --output json
cargo run -- upgrade apply --plan upgrade.plan --confirm plan-upgrade --lock-store .eva/locks --state-store .eva/supervisor --output json
```

输出会包含：

- `supervisor.active_generation`
- `supervisor.candidate_generation`
- `migration.status`
- `drain.status`
- `rollback.status`
- `risks`，明确 CLI 不启动真实 runtime 进程。

`upgrade apply --state-store` 的 JSON 包含：

- `status: "committed"` 或 `"blocked"`
- `handoff.mutation_executed`
- `handoff.release_pointer.pointer_path`
- `handoff.rollback_plan`
- `state_store.path`

## 模块边界

`eva-lifecycle` 做：

- 管理 runtime generation 的创建、激活、drain、回滚计划。
- 协调高风险 apply/restore 的执行前状态。
- 在显式 policy approval 后提交本地 blue-green handoff state 和 release pointer mutation。
- 定义 OS service-manager adapter trait、三平台 production Adapter 和规范化 direct daemon entrypoint。
- 将 Unix signal/Windows SCM stop control 收敛为 atomic token，禁止在 handler 中直接执行阻塞 drain。
- 记录 lifecycle audit、trace 和失败原因。

`eva-lifecycle` 不做：

- 不生成 backup artifact 或校验 digest。
- 不执行 Lua 业务逻辑。
- 不决定 Adapter 或 capability 的业务路由。
- 不静默执行不可逆 mutation，所有高风险路径必须先有 plan。
- 不把代码级 Adapter/entrypoint 测试当作真实 host evidence；stop/boot/reboot transcript、destructive lifecycle harness 与 production release gate 仍需受控平台环境。
- 不实现真实双 Runtime、Ingress/Canary 切流或 blue-green rollback。

## 验证计划

```powershell
cargo test -p eva-lifecycle
cargo run -- upgrade check --output json
```

当前测试覆盖：

- candidate promote 后新 generation 变 active，旧 generation 进入 draining。
- drain plan 停止接收新工作并可完成。
- rollback plan 保留 previous generation 并记录风险。
- in-memory supervisor 只提交 healthy candidate。
- handoff 在 policy、lock、health 通过后写入 release pointer 和 committed state。
- candidate health 失败时不写 pointer，并输出 rollback plan。
- filesystem state store 可恢复 `handoff.prepared` / `handoff.committed` / `state/release-pointer` 证据。
- service-manager fake adapter 可提交 healthy candidate、阻断 failed candidate，并执行 rollback audit。
- direct entrypoint argv 对空格路径保持原生参数边界，identity digest 漂移会 fail closed。
- stop bridge 只设置 token；runtime 测试验证 token 进入同一 drain/shutdown、stopped/PID cleanup/lease release 事务。

## English

`eva-lifecycle` owns supervisor boundaries, runtime generations, drain planning,
rollback planning, upgrade locks, modeled handoff state, host-bound service
adapters, canonical direct-service argv identity, and the cooperative OS-stop
bridge.

P6-003 adds the upgrade apply lock model. `upgrade apply` can acquire a
filesystem-backed lock for a confirmed plan and report `apply_allowed:false`;
real generation promotion remains behind later destructive apply gates.

V1.10.5 adds `SupervisorHandoffCoordinator` and filesystem state persistence.
The CLI can commit a controlled modeled handoff with `--state-store`. The current
code also includes Windows Service/systemd/launchd adapters and an identity-bound
direct daemon entrypoint: Windows SCM controls or Unix signals set an atomic stop
token, and runtime code performs the existing drain/shutdown transaction. This
does not claim controlled real-host stop/boot/reboot evidence, a destructive
lifecycle harness, a production release gate, or real blue-green traffic
switching.
