# eva-lifecycle / 生命周期管理

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-lifecycle` 负责 Supervisor、runtime generation、drain 和 rollback。它管理运行时代际切换和失败恢复，不承载 Lua 业务决策，不替代 backup 的 artifact 校验。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Supervisor | 骨架 | 拥有 runtime process/service 的启动、监控和停止边界。 |
| Generation | 骨架 | 管理 runtime generation id、active/pending/draining 状态。 |
| Drain | 骨架 | 在切换或 shutdown 前停止接收新任务并等待安全退出。 |
| Rollback | 骨架 | 切换失败后回滚到上一代 runtime 或 snapshot。 |
| Backup integration | 未实现 | V1.4 使用 `eva-backup` 的 restore plan 和 snapshot 校验。 |
| Audit | 未实现 | generation handoff、drain、rollback 全部记录审计。 |

## 模块边界

`eva-lifecycle` 做：

- 管理 runtime generation 的创建、激活、drain、回滚。
- 协调高风险 apply/restore 的执行阶段。
- 记录 lifecycle audit、trace 和失败原因。

`eva-lifecycle` 不做：

- 不生成 backup artifact 或校验 digest。
- 不执行 Lua 业务逻辑。
- 不决定 Adapter 或 capability 的业务路由。
- 不静默执行不可逆 mutation，所有高风险路径必须先有 plan。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.4 | 定义 supervisor trait、runtime handle、health summary。 | `eva-runtime` | supervisor 可启动/停止 mock runtime。 |
| 2 | V1.4 | 定义 generation state：pending、active、draining、retired、failed。 | `eva-core::GenerationId` | 状态转换可测。 |
| 3 | V1.4 | 实现 drain protocol：停止接收、等待任务、超时强制失败。 | `eva-agent`、`eva-eventbus` | drain 超时返回结构化错误。 |
| 4 | V1.4 | 实现 rollback protocol：保留上一代 handle，失败后恢复。 | `eva-backup` | 切换失败时上一代仍可用。 |
| 5 | V1.4 | 接 release snapshot restore plan。 | `eva-backup` | restore apply 前必须 verify plan。 |
| 6 | V1.5 | 增加 supervisor restart policy、崩溃恢复和健康检查。 | observability | runtime 失败可审计、可恢复。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export supervisor、generation、drain、rollback。 |
| `src/supervisor.rs` | runtime supervisor | `RESPONSIBILITY` 占位 | 定义 start/stop/restart/health API。 |
| `src/generation.rs` | generation 状态 | `RESPONSIBILITY` 占位 | 定义状态枚举、handoff、active handle。 |
| `src/drain.rs` | drain 旧 generation | `RESPONSIBILITY` 占位 | 定义 drain token、deadline、result。 |
| `src/rollback.rs` | 失败后回滚 | `RESPONSIBILITY` 占位 | 定义 rollback plan、reason、audit fields。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | generation/drain/rollback | 未开始 | 覆盖非法切换、drain 超时、rollback 成功/失败。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.4 | `cargo test -p eva-lifecycle` | generation、drain、rollback 可测。 |
| V1.4 | backup integration tests | restore/apply 必须先 verify plan。 |
| V1.5 | supervisor fault tests | runtime 失败可重启或回滚。 |

## English

`eva-lifecycle` owns supervisor boundaries, runtime generations, drain, and rollback. High-risk mutations must be planned and audited before execution.
