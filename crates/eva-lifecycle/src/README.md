# eva-lifecycle/src / 生命周期源码

更新时间：2026-07-04

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 V1.4 supervisor、runtime generation、drain 和 rollback 源码。实现重点是可测试状态机和 plan-first lifecycle coordination，不启动真实 OS 进程。

## 文件职责

| 文件 | 职责 | V1.4 状态 | 关键类型/函数 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 已完成 | re-export generation、drain、rollback、supervisor 类型。 |
| `generation.rs` | runtime generation state and handoff | 已完成 | `GenerationState`、`RuntimeGeneration`、`GenerationController`。 |
| `drain.rs` | draining old runtime generations | 已完成 | `DrainStatus`、`DrainPlan`、`DrainCoordinator`。 |
| `rollback.rs` | failed handoff rollback coordination | 已完成 | `RollbackPlan`、`RollbackCoordinator`。 |
| `supervisor.rs` | supervisor process/runtime ownership | 已完成 | `InMemorySupervisor`、`RuntimeHealth`、`SupervisorReport`。 |

## 关键不变量

- 初始 controller 必须以 active generation 创建。
- 同一时刻只能存在一个 candidate generation。
- promote candidate 后，新 generation 变 active，旧 active 进入 draining。
- drain plan 必须 `accepts_new_work:false`。
- rollback reason 不能为空。
- supervisor commit candidate 前必须通过 matching generation health check。

## 验证

```powershell
cargo test -p eva-lifecycle
```

## P6-003 Upgrade Apply Lock Model

`apply_lock.rs` owns `UpgradeApplyPlan`, `UpgradeApplyCoordinator`,
`InMemoryUpgradeApplyLockStore`, and `FileSystemUpgradeApplyLockStore`.
The model proves lock acquisition and conflict behavior for `upgrade apply`
without starting a runtime process or promoting a generation.
