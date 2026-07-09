# eva-backup / 备份迁移

更新时间：2026-07-09

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-backup` 是 V1.4/V1.10 的备份、迁移包、ReleaseSnapshot 和 restore evidence 可信执行边界。它把高风险恢复能力拆成 signed backup archive、manifest verification、migration preflight、release snapshot、restore plan、pre-restore evidence 和 restore apply gate，保证恢复与升级先计划、先校验、先审计，再交给 lifecycle 执行。

V1.10.4 之后，backup artifact 是稳定 archive payload，带 checksum、manifest signature、可选 sealed archive metadata 和 remote target typed contract。`restore apply` 可以在 confirmation、pre-restore evidence、policy approval、apply lock 和 health check 都通过后产出 gated report。V1.14.1 允许 plan 文件声明 staged mutation steps，并生成 preview、affected paths、preflight hash 和 rollback manifest；V1.14.2 在这些 gate 全部通过且 plan 声明 mutation steps 时执行 copy/delete/replace file mutation，并为每一步写 transaction log。旧 no-step plan 仍只产出 gated report；rollback apply、release pointer 和 supervisor handoff 仍是后续边界。

## 已实现能力

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| BackupService | 已完成 V1.10.3 | `BackupPlan` 解析 scope、actor、reason、generation、signing key、可选 encryption key 和 remote target；`BackupService::create` 写入 signed archive 并生成 `BackupManifest`。 |
| Signed archive | 已完成 V1.10.3 | `BackupArchiveCodec` 生成稳定 archive bytes，计算 sealed/plaintext checksum，写入 keyed SHA-256 signature，并支持可选 sealed archive。 |
| Artifact verification | 已完成 V1.10.3 | `ManifestVerifier::verify_artifact` 复算 artifact bytes digest；`verify_backup_archive` 同时校验 artifact key、checksum 和 signature，mismatch 返回结构化 `Conflict`。 |
| Restore apply gate | 已完成 V1.10.4 | `RestoreApplyValidator` 验证 durable backup artifact 和 pre-restore backup evidence 的 key/digest；`RestoreApplyCoordinator` 要求 policy approval、apply lock 和 health check 后返回 gated/blocked report，不执行破坏性恢复。 |
| Restore staged mutation planner | 已完成 V1.14.1 | `RestoreStagedMutationPlanner` 校验 copy/delete/replace steps，拒绝 path traversal、Windows prefix/backslash 和 symlink target kind，并生成 deterministic `preflight_hash`、preview、affected paths 与 rollback manifest。 |
| Restore mutation engine | 已完成 V1.14.2 | `RestoreMutationEngine` 在 source artifact digest、target pre-restore digest 和 safe target path 都通过后执行 staged file mutation；每步写 transaction log，失败时停止并输出 `rollback_required`。 |
| Remote target contract | 已完成 V1.10.3 | `RemoteBackupTarget` 以 typed contract 记录 filesystem/object-store/S3-compatible 灾备目标；当前只记录和签名，不执行远端上传。 |
| MigrationPackage | 已完成 V1.4 | `MigrationPackageManifest` 声明 source/target、affected sections、reversible 和 checksum；`MigrationPackageService::verify_preflight` 输出 ready/planned/blocked。 |
| ReleaseSnapshot | 已完成 V1.4 | `ReleaseSnapshotService::create` 将 snapshot 绑定到已验证 backup manifest、release ref、generation 和 health status。 |
| Restore plan | 已完成 V1.4 | `ReleaseSnapshotService::restore_plan` 只返回 steps、risks、audit 和 `apply_allowed:false`。 |
| CLI | 已完成 V1.10.4 | `eva backup create` 输出 archive signature/encryption metadata；`restore apply --dry-run` 强制 plan 携带 pre-restore backup evidence；非 dry-run `restore apply` 还要求 `--lock-store`、policy allow 和 health check。 |

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
| `RestoreApplyCoordinator::apply` | 在 dry-run evidence、policy approval、apply lock 和 health check 之后输出 gated/blocked report；当前保持 `mutation_executed:false`。 |
| `RestoreMutationStep` / `RestoreStagedMutationPlanner` | 把 restore target 拆成 copy/delete/replace file steps，生成 plan-only `mutation_plan` evidence。 |
| `RestoreStagedMutationPlan` / `RestoreRollbackEntry` | 暴露 `affected_paths`、`preview`、`preflight_hash` 和 rollback manifest，供后续 mutation engine/rollback apply 使用。 |
| `RestoreMutationEngine` / `RestoreMutationApplyReport` | 执行 gated staged file mutation，记录 transaction log、`mutation_executed`、`rollback_required` 和失败 step。 |
| `RestoreApplyLock` / `RestoreApplyLockStore` | 表达 restore apply 独占锁；filesystem store 写入 `{plan_id}.restore.lock`，冲突返回稳定 `Conflict`。 |
| `RestoreApplyHealthCheck` | 表达 pre-apply health gate；失败时阻断 apply 并要求 rollback plan。 |
| `InMemoryRestoreApplyLockStore` / `FileSystemRestoreApplyLockStore` | 分别用于单元测试和 CLI 持久锁目录。 |
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
cargo run -- restore apply --plan restore-plan.txt --confirm plan-123 --artifact-store .eva/artifacts --lock-store .eva/locks --output json
```

`restore plan` 的 JSON 中必须保持：

- `status: "planned"`
- `apply_allowed: false`
- `steps` 包含 snapshot manifest、lifecycle operation lease、drain、stage restore 和 health verification。

`restore apply --dry-run` 和 gated `restore apply` 的 key/value plan 必须包含 pre-restore evidence：

```text
plan_id=plan-123
backup_artifact_id=backup-for-snapshot-v14
backup_digest=sha256:<hex>
pre_restore_backup_artifact_id=pre-restore-plan-123
pre_restore_backup_digest=sha256:<hex>
```

V1.14.1 起，plan 文件还可以追加 staged mutation preview 字段；这些字段只生成 `mutation_plan`，不会写盘：

```text
restore_target_root=workspace
mutation_step=copy|config/eva.yaml|backup/config|sha256:<hex>|none|file
mutation_step=replace|bin/eva|backup/bin|sha256:<hex>|sha256:<old>|file
mutation_step=delete|logs/old.log|none|none|sha256:<old>|file
```

## 模块边界

`eva-backup` 做：

- 定义 backup、migration、snapshot 的计划、manifest、校验结果。
- 与 `eva-storage::ArtifactStore` 集成保存 artifact。
- 生成 signed archive、可选 sealed archive 和 remote target typed contract。
- 在 restore apply dry-run 和 apply gate 前强制验证 pre-restore backup evidence。
- 生成 staged mutation preview、affected paths、preflight hash 和 rollback manifest。
- 在 gate 通过且 plan 声明 mutation steps 时执行 copy/delete/replace file mutation，并写 transaction log。
- 输出 restore/apply 的风险摘要、apply lock、health gate 和审计字段。
- 为 `eva-lifecycle` 提供可验证的 restore plan。

`eva-backup` 不做：

- 不由 Agent 直接触发不可逆 restore。
- 不替代 lifecycle generation 切换。
- 不绕过 policy 修改 workspace 或数据目录。
- 不静默忽略 digest 或 manifest mismatch。
- 不执行远端灾备上传。
- 不绕过 signed archive、pre-restore evidence、policy、lock、health、staged plan 或 transaction log 执行 mutation。
- 不移动 release pointer，不启动 supervisor handoff，不执行真实 rollback apply。

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
- policy 未显式允许 `restore.apply` 时不会创建 apply lock。
- apply lock 冲突返回稳定 `Conflict`。
- health check 失败时返回 `blocked`、`apply_allowed:false`，并由 CLI 输出 rollback plan。
- gated restore apply report 保持 `mutation_executed:false`。
- staged mutation planner 拒绝 path traversal/symlink target，并保证 preview/preflight hash 可复算。
- mutation engine 在临时目录中执行 copy/delete/replace 并写 transaction log；中途失败输出 `rollback_required`。
- digest mismatch 返回 `Conflict`。
- migration source version mismatch 返回 blocked preflight。
- release snapshot restore plan 保持 plan-first。

## English

`eva-backup` owns signed backup archives, migration package preflight, release snapshots, manifest verification, pre-restore evidence, restore apply gates, staged mutation plans, and the gated staged file mutation engine. It does not move release pointers, start supervisor handoff, or apply rollback manifests yet.

P6-004 adds `ReleasePointerPlan` through
`ReleaseSnapshotService::release_pointer_plan`. The plan validates snapshot
confirmation and records pointer-change steps, risks, and audit while keeping
`apply_allowed:false`.
