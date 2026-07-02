# eva-lifecycle/src / 生命周期源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 supervisor、runtime generation、drain 和 rollback。当前为骨架，V1.4 先实现 generation 状态机和失败回滚协议。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.4 |
| `supervisor.rs` | supervisor process/runtime ownership | 骨架 | V1.4 |
| `generation.rs` | runtime generation state and handoff | 骨架 | V1.4 |
| `drain.rs` | draining old runtime generations | 骨架 | V1.4 |
| `rollback.rs` | failed handoff rollback coordination | 骨架 | V1.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 supervisor trait 和 runtime handle。 | mock runtime 可启动停止。 |
| 2 | 定义 generation 状态机。 | handoff 可验证。 |
| 3 | 实现 drain token 和 deadline。 | 切换前可停止接收。 |
| 4 | 实现 rollback plan 和 audit fields。 | 失败切换可恢复。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Supervisor | start/stop/restart | 未实现 | 定义 health summary。 |
| Generation | pending/active/draining | 未实现 | 定义合法转换。 |
| Drain | deadline/result | 未实现 | 接 Agent/EventBus。 |
| Rollback | plan/reason/audit | 未实现 | 接 Backup snapshot。 |
