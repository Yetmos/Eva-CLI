# eva-backup / 备份迁移

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-backup` 负责备份、迁移包、release snapshot 和 artifact/manifest 完整性校验。它处理高风险恢复操作的计划和验证，但 Agent 不能直接拥有 restore、rollback 或 destructive mutation。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| BackupService | 骨架 | 生成项目配置、数据、artifact 的备份计划和备份结果。 |
| MigrationPackage | 骨架 | 构造跨版本迁移包、manifest、digest、兼容性说明。 |
| ReleaseSnapshot | 骨架 | 生成 release 前快照和 restore plan。 |
| ManifestVerifier | 骨架 | 校验 artifact digest、manifest 签名/格式和完整性。 |
| Restore plan | 未实现 | restore 先输出 plan、风险、影响范围，再由 lifecycle 执行。 |
| Lifecycle integration | 未实现 | V1.4 与 `eva-lifecycle` 协作实现 drain/rollback。 |

## 模块边界

`eva-backup` 做：

- 定义备份、迁移、snapshot 的计划、manifest、校验结果。
- 与 `eva-storage::ArtifactStore` 集成保存 artifact。
- 输出 restore/apply 的风险摘要和审计字段。

`eva-backup` 不做：

- 不由 Agent 直接触发不可逆 restore。
- 不替代 lifecycle generation 切换。
- 不绕过 policy 修改 workspace 或数据目录。
- 不静默忽略 digest 或 manifest mismatch。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.4 | 定义 backup plan、backup manifest、artifact digest、scope。 | `eva-storage` | 计划可 dry-run 输出。 |
| 2 | V1.4 | 实现 BackupService 初版，生成 artifact 并校验 digest。 | ArtifactStore | 创建后立即 verify 通过。 |
| 3 | V1.4 | 定义 MigrationPackage manifest、版本兼容、precheck。 | `eva-config` | 不兼容版本返回结构化错误。 |
| 4 | V1.4 | 定义 ReleaseSnapshot 和 restore plan。 | runtime/lifecycle | restore 不直接执行，先输出 plan。 |
| 5 | V1.4 | 实现 ManifestVerifier。 | `eva-core::EvaError` | digest mismatch 明确失败。 |
| 6 | V1.4 | 接 lifecycle drain/rollback。 | `eva-lifecycle` | apply 失败可回滚到 snapshot。 |
| 7 | V1.5 | 增加加密、签名、远端存储和灾备演练。 | 安全评审 | 高风险路径有集成测试。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export backup_service、migration_package、release_snapshot、manifest_verifier。 |
| `src/backup_service.rs` | 备份计划和执行 | `RESPONSIBILITY` 占位 | 定义 backup plan、scope、result、artifact ref。 |
| `src/migration_package.rs` | 迁移包构造和校验 | `RESPONSIBILITY` 占位 | 定义 manifest、compatibility、precheck。 |
| `src/release_snapshot.rs` | release snapshot 和 restore | `RESPONSIBILITY` 占位 | 定义 snapshot manifest、restore plan。 |
| `src/manifest_verifier.rs` | 完整性校验 | `RESPONSIBILITY` 占位 | 实现 digest、schema、version 检查。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | manifest/digest/plan | 未开始 | 覆盖 digest mismatch、restore dry-run、版本不兼容。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.4 | `cargo test -p eva-backup` | plan、manifest、digest、snapshot 可测。 |
| V1.4 | lifecycle integration tests | restore/apply 失败可 rollback。 |
| V1.5 | disaster recovery drill | 备份可恢复到可运行状态。 |

## English

`eva-backup` owns backups, migration packages, release snapshots, and artifact verification. Restore and apply operations must be planned, audited, and coordinated with lifecycle.
