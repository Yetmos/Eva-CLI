# eva-backup/src / 备份源码

更新时间：2026-07-08

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 V1.4/V1.10 备份、迁移包、release snapshot、signed archive、pre-restore evidence 和 manifest 完整性校验源码。实现重点是把高风险恢复路径表达成可测试的 plan、archive、manifest、digest、signature 和 audit，而不是直接执行 destructive restore。

## 文件职责

| 文件 | 职责 | V1.4 状态 | 关键类型/函数 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 已完成 | re-export backup、manifest verifier、migration、snapshot 类型。 |
| `archive.rs` | signed/sealed archive contract | 已完成 V1.10.3 | `BackupArchiveCodec`、`BackupSigningKey`、`BackupEncryptionKey`、`RemoteBackupTarget`、`BackupArchiveVerifier`。 |
| `backup_service.rs` | backup plan 和 artifact 生成 | 已完成 V1.10.3 | `BackupEntry`、`BackupScope`、`BackupPlan`、`BackupManifest`、`BackupService::create`。 |
| `manifest_verifier.rs` | artifact/manifest integrity verification | 已完成 V1.10.3 | `ManifestVerifier::verify_artifact`、`ManifestVerifier::verify_backup_archive`、`VerificationReport`。 |
| `restore_apply.rs` | restore apply dry-run validation | 已完成 V1.10.3 | `RestoreApplyPlan`、`PreRestoreBackupEvidence`、`RestoreApplyValidator`，验证 durable artifact 和 pre-restore evidence，不执行恢复。 |
| `migration_package.rs` | 迁移包 manifest 和兼容性 | 已完成 | `MigrationPackageManifest`、`MigrationPackageService::verify_preflight`。 |
| `release_snapshot.rs` | release snapshot 和 restore plan | 已完成 | `ReleaseSnapshot`、`SnapshotRole`、`RestorePlan`、`ReleaseSnapshotService`。 |

## 关键不变量

- Backup scope 必须至少包含一条 entry。
- Backup entry path 必须是稳定相对路径，不能包含 `..` 或反斜杠。
- Backup create 后立即通过 `ManifestVerifier` 校验 artifact digest 和 archive signature。
- Signed archive 必须同时记录 sealed checksum 和 plaintext checksum。
- Optional sealed archive 需要 matching encryption key 才能打开。
- Remote target 只作为 typed contract 进入签名 manifest，不执行上传。
- Digest mismatch 必须返回 `Conflict`，不能降级成 warning。
- Restore apply dry-run 必须验证 durable artifact key/digest 和 pre-restore backup evidence，不能执行恢复。
- Migration package preflight 不执行迁移逻辑，只输出 ready/planned/blocked。
- Restore plan 在 V1.4 永远 `apply_allowed:false`。

## 验证

```powershell
cargo test -p eva-backup
```

## P6-004 Release Pointer Plan

`release_snapshot.rs` now also exposes `ReleasePointerPlan` via
`ReleaseSnapshotService::release_pointer_plan`. It validates the requested
snapshot promotion confirmation and keeps release pointer movement plan-first
with `apply_allowed:false`.
