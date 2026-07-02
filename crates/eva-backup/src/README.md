# eva-backup/src / 备份源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载备份服务、迁移包、release snapshot 和 manifest 完整性校验。当前为骨架，V1.4 先实现 plan、manifest、digest 和 dry-run restore。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.4 |
| `backup_service.rs` | backup plan 和 artifact 生成 | 骨架 | V1.4 |
| `migration_package.rs` | 迁移包 manifest 和兼容性 | 骨架 | V1.4 |
| `release_snapshot.rs` | release snapshot 和 restore plan | 骨架 | V1.4 |
| `manifest_verifier.rs` | artifact/manifest integrity verification | 骨架 | V1.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 backup plan、scope、manifest、digest。 | 备份可 dry-run。 |
| 2 | 实现 artifact 写入和 verify。 | digest mismatch 可失败。 |
| 3 | 定义 migration package 和 release snapshot。 | 版本兼容可诊断。 |
| 4 | 接 lifecycle rollback。 | restore/apply 可回滚。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| BackupService | plan/execute/result | 未实现 | 定义 artifact ref。 |
| MigrationPackage | manifest/precheck | 未实现 | 定义兼容规则。 |
| ReleaseSnapshot | snapshot/restore plan | 未实现 | 禁止直接 restore。 |
| ManifestVerifier | digest/schema/version | 未实现 | 实现校验结果。 |
