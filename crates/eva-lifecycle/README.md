# eva-lifecycle / 生命周期管理

更新时间：2026-07-04

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-lifecycle` 是 V1.4 的 Supervisor、runtime generation、drain 和 rollback 边界。它管理运行时代际切换和失败恢复，不承载 Lua 业务决策，不生成 backup artifact，也不绕过 `eva-backup` 的 snapshot/restore plan 校验。

V1.4 的实现是 in-memory lifecycle planning：可以表达 active/candidate generation、drain plan、rollback plan 和 mock supervisor health check，但不会启动真实进程、移动 release pointer 或执行真实升级。

## V1.4 已实现能力

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| Generation | 已完成 V1.4 | `RuntimeGeneration` 和 `GenerationController` 支持 active/candidate、promote、failed candidate、old generation draining。 |
| Drain | 已完成 V1.4 | `DrainCoordinator` 输出 plan/completed/timed_out，并显式 `accepts_new_work:false`。 |
| Rollback | 已完成 V1.4 | `RollbackCoordinator` 根据 failed handoff 和可选 `RestorePlan` 生成 rollback steps、risks、audit。 |
| Supervisor | 已完成 V1.4 | `InMemorySupervisor` 支持 start candidate、commit healthy candidate 和 structured report。 |
| Backup integration | 已完成 V1.4 | rollback 可携带 `eva-backup::RestorePlan` 的 snapshot/risk 信息。 |
| CLI | 已完成 V1.4 | `eva upgrade check` 输出 supervisor、migration preflight、drain 和 rollback readiness。 |

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

## CLI 验证入口

```powershell
cargo run -- upgrade check --output json
```

输出会包含：

- `supervisor.active_generation`
- `supervisor.candidate_generation`
- `migration.status`
- `drain.status`
- `rollback.status`
- `risks`，明确 CLI 不启动真实 runtime 进程。

## 模块边界

`eva-lifecycle` 做：

- 管理 runtime generation 的创建、激活、drain、回滚计划。
- 协调高风险 apply/restore 的执行前状态。
- 记录 lifecycle audit、trace 和失败原因。

`eva-lifecycle` 不做：

- 不生成 backup artifact 或校验 digest。
- 不执行 Lua 业务逻辑。
- 不决定 Adapter 或 capability 的业务路由。
- 不静默执行不可逆 mutation，所有高风险路径必须先有 plan。
- 不在 V1.4 启动真实 OS 进程、service manager、Supervisor binary 或 runtime binary。

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

## English

`eva-lifecycle` owns V1.4 supervisor boundaries, runtime generations, drain planning, and rollback planning. It models lifecycle safety without starting real processes in V1.4.

P6-003 adds the upgrade apply lock model. `upgrade apply` can acquire a
filesystem-backed lock for a confirmed plan and report `apply_allowed:false`;
real generation promotion remains behind later destructive apply gates.
