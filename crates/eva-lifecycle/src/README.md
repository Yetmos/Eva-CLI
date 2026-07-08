# eva-lifecycle/src / 生命周期源码

更新时间：2026-07-08

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 V1.4/V1.10 supervisor、runtime generation、drain、rollback、upgrade apply lock 和 blue-green handoff 源码。实现重点是可测试状态机、policy-gated mutation、持久 handoff evidence 和 rollback coordination；V1.10.5 仍不替代真实 OS service manager。

## 文件职责

| 文件 | 职责 | V1.4 状态 | 关键类型/函数 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 已完成 | re-export generation、drain、rollback、supervisor 类型。 |
| `generation.rs` | runtime generation state and handoff | 已完成 | `GenerationState`、`RuntimeGeneration`、`GenerationController`。 |
| `drain.rs` | draining old runtime generations | 已完成 | `DrainStatus`、`DrainPlan`、`DrainCoordinator`。 |
| `rollback.rs` | failed handoff rollback coordination | 已完成 | `RollbackPlan`、`RollbackCoordinator`。 |
| `supervisor.rs` | supervisor process/runtime ownership | 已完成 | `InMemorySupervisor`、`RuntimeHealth`、`SupervisorReport`。 |
| `apply_lock.rs` | upgrade apply lock acquisition boundary | 已完成 V1.10.3 | `UpgradeApplyPlan`、`UpgradeApplyCoordinator`、filesystem/in-memory lock store。 |
| `handoff.rs` | blue-green supervisor handoff and release pointer mutation | 已完成 V1.10.5 | `SupervisorHandoffCoordinator`、`RuntimeBinaryProbe`、`FileSystemSupervisorStateStore`、`SupervisorHandoffReport`。 |

## 关键不变量

- 初始 controller 必须以 active generation 创建。
- 同一时刻只能存在一个 candidate generation。
- promote candidate 后，新 generation 变 active，旧 active 进入 draining。
- drain plan 必须 `accepts_new_work:false`。
- rollback reason 不能为空。
- supervisor commit candidate 前必须通过 matching generation health check。
- handoff 必须先通过 `supervisor.handoff` 和 `release.pointer_mutation` policy approval。
- release pointer 只能在 lock、runtime binary smoke、candidate health 和 old generation drain 之后写入。
- health failure 必须保持 previous generation active，不写 `state/release-pointer`，并输出 rollback plan。
- filesystem state store 必须至少写入 `handoff.prepared`，成功 handoff 还必须写入 `handoff.committed` 和 `state/release-pointer`。

## 验证

```powershell
cargo test -p eva-lifecycle
cargo test -p eva-lifecycle handoff
```

## P6-003 Upgrade Apply Lock Model

`apply_lock.rs` owns `UpgradeApplyPlan`, `UpgradeApplyCoordinator`,
`InMemoryUpgradeApplyLockStore`, and `FileSystemUpgradeApplyLockStore`.
The model proves lock acquisition and conflict behavior for `upgrade apply`
without starting a runtime process or promoting a generation.

## V1.10.5 Supervisor Handoff

`handoff.rs` owns the controlled apply boundary for `upgrade apply --state-store`.
It uses the existing generation controller and in-memory supervisor semantics,
then persists handoff state and release pointer evidence to a filesystem store.
This is a real local state mutation, but not a daemonized OS service-manager
handoff.
