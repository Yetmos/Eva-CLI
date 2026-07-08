# eva-backup / 备份迁移

更新时间：2026-07-08

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-backup` 是 V1.4/V1.10 的备份、迁移包、ReleaseSnapshot 和 restore evidence 可信执行边界。它把高风险恢复能力拆成 signed backup archive、manifest verification、migration preflight、release snapshot、restore plan 和 pre-restore evidence，保证恢复与升级先计划、先校验、先审计，再交给 lifecycle 执行。

V1.10.3 之后，backup artifact 是稳定 archive payload，带 checksum、manifest signature、可选 sealed archive metadata 和 remote target typed contract。恢复仍然保持 plan-first：不会执行 destructive restore、移动 release pointer 或修改 workspace 状态。

## 已实现能力

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| BackupService | 已完成 V1.10.3 | `BackupPlan` 解析 scope、actor、reason、generation、signing key、可选 encryption key 和 remote target；`BackupService::create` 写入 signed archive 并生成 `BackupManifest`。 |
| Signed archive | 已完成 V1.10.3 | `BackupArchiveCodec` 生成稳定 archive bytes，计算 sealed/plaintext checksum，写入 keyed SHA-256 signature，并支持可选 sealed archive。 |
| Artifact verification | 已完成 V1.10.3 | `ManifestVerifier::verify_artifact` 复算 artifact bytes digest；`verify_backup_archive` 同时校验 artifact key、checksum 和 signature，mismatch 返回结构化 `Conflict`。 |
| Restore apply dry-run | 已完成 V1.10.3 | `RestoreApplyValidator` 验证 durable backup artifact 和 pre-restore backup evidence 的 key/digest，不执行破坏性恢复。 |
| Remote target contract | 已完成 V1.10.3 | `RemoteBackupTarget` 以 typed contract 记录 filesystem/object-store/S3-compatible 灾备目标；当前只记录和签名，不执行远端上传。 |
| MigrationPackage | 已完成 V1.4 | `MigrationPackageManifest` 声明 source/target、affected sections、reversible 和 checksum；`MigrationPackageService::verify_preflight` 输出 ready/planned/blocked。 |
| ReleaseSnapshot | 已完成 V1.4 | `ReleaseSnapshotService::create` 将 snapshot 绑定到已验证 backup manifest、release ref、generation 和 health status。 |
| Restore plan | 已完成 V1.4 | `ReleaseSnapshotService::restore_plan` 只返回 steps、risks、audit 和 `apply_allowed:false`。 |
| CLI | 已完成 V1.10.3 | `eva backup create` 输出 archive signature/encryption metadata；`restore apply --dry-run` 强制 plan 携带 pre-restore backup evidence。 |

## Public API

| 类型/函数 | 说明 |
| --- | --- |
| `BackupEntry` | backup scope 中的一条相对路径和 bytes，可标记 redacted。 |
| `BackupScope` | project id 与 entries 的集合，拒绝空 scope。 |
| `BackupPlan` | request、generation、actor、reason、scope、dry-run、signing key、可选 encryption key 和 remote target。 |
| `BackupManifest` | artifact type、request、generation、project、entries、digest、archive signature/encryption metadata 和 audit。 |
| `BackupArchiveCodec` | 生成 signed/sealed archive，并可用 encryption key 打开 archive。 |
| `RemoteBackupTarget` | 记录 typed remote disaster-recovery target，不执行上传。 |
| `BackupService::create` | 写入 `ArtifactStore`，生成 signed archive manifest，并立即校验 digest/signature。 |
| `ManifestVerifier::verify_artifact` | 复算 artifact bytes digest，失败时输出 expected/actual digest context。 |
| `ManifestVerifier::verify_backup_archive` | 校验 archive artifact key、checksum 和 signature。 |
| `RestoreApplyValidator::dry_run` | 校验 restore apply plan 指向的 backup artifact 和 pre-restore evidence key/digest；输出 `apply_allowed:false`。 |
| `MigrationPackageManifest` | 迁移包 id、source/target version、affected sections、checksum。 |
| `MigrationPackageService::verify_preflight` | 校验当前版本是否匹配 source version，并提示不可逆迁移风险。 |
| `ReleaseSnapshotService::create` | 创建 pre/post release snapshot。 |
| `ReleaseSnapshotService::restore_plan` | 生成 plan-first restore plan，不执行恢复。 |

## CLI 验证入口

```powershell
cargo run -- backup create --output json
cargo run -- backup create --encrypt --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
```

`restore plan` 的 JSON 中必须保持：

- `status: "planned"`
- `apply_allowed: false`
- `steps` 包含 snapshot manifest、lifecycle operation lease、drain、stage restore 和 health verification。

`restore apply --dry-run` 的 key/value plan 必须包含 pre-restore evidence：

```text
plan_id=plan-123
backup_artifact_id=backup-for-snapshot-v14
backup_digest=sha256:<hex>
pre_restore_backup_artifact_id=pre-restore-plan-123
pre_restore_backup_digest=sha256:<hex>
```

## 模块边界

`eva-backup` 做：

- 定义 backup、migration、snapshot 的计划、manifest、校验结果。
- 与 `eva-storage::ArtifactStore` 集成保存 artifact。
- 生成 signed archive、可选 sealed archive 和 remote target typed contract。
- 在 restore apply dry-run 前强制验证 pre-restore backup evidence。
- 输出 restore/apply 的风险摘要和审计字段。
- 为 `eva-lifecycle` 提供可验证的 restore plan。

`eva-backup` 不做：

- 不由 Agent 直接触发不可逆 restore。
- 不替代 lifecycle generation 切换。
- 不绕过 policy 修改 workspace 或数据目录。
- 不静默忽略 digest 或 manifest mismatch。
- 不执行远端灾备上传。
- 不把 signed archive 或 pre-restore evidence 直接升级成 destructive apply。

## 验证计划

```powershell
cargo test -p eva-backup
cargo test -p eva-cli restore_apply
cargo run -- backup create --output json
cargo run -- backup create --encrypt --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
```

当前测试覆盖：

- backup artifact round-trip 和 digest verification。
- signed archive signature verification failure 会阻断。
- archive bytes corrupt 会返回 `Conflict`。
- optional sealed archive 可用 matching encryption key 打开。
- pre-restore backup evidence 缺失时 restore apply dry-run 阻断。
- digest mismatch 返回 `Conflict`。
- migration source version mismatch 返回 blocked preflight。
- release snapshot restore plan 保持 plan-first。

## English

`eva-backup` owns signed backup archives, migration package preflight, release snapshots, manifest verification, pre-restore evidence, and plan-first restore boundaries. It verifies artifacts but does not execute destructive restore.

P6-004 adds `ReleasePointerPlan` through
`ReleaseSnapshotService::release_pointer_plan`. The plan validates snapshot
confirmation and records pointer-change steps, risks, and audit while keeping
`apply_allowed:false`.
