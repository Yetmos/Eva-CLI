# eva-backup / 备份迁移

更新时间：2026-07-04

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-backup` 是 V1.4 的备份、迁移包和 ReleaseSnapshot 可信执行边界。它把高风险恢复能力拆成 backup artifact、manifest verification、migration preflight、release snapshot 和 restore plan，保证恢复与升级先计划、先校验、先审计，再交给 lifecycle 执行。

V1.4 的实现仍然是 in-memory / plan-first：可以创建并校验 artifact，生成 snapshot 和 restore plan，但不会执行 destructive restore、移动 release pointer 或修改 workspace 状态。

## V1.4 已实现能力

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| BackupService | 已完成 V1.4 | `BackupPlan` 解析 scope、actor、reason 和 generation；`BackupService::create` 写入 `ArtifactStore` 并生成 `BackupManifest`。 |
| Artifact verification | 已完成 V1.4 | `ManifestVerifier::verify_artifact` 比对 expected/actual digest，digest mismatch 返回结构化 `Conflict`。 |
| Restore apply dry-run | 已完成 P6 | `RestoreApplyValidator` 只验证 durable backup artifact 存在、key 匹配和 digest 匹配，不执行破坏性恢复。 |
| MigrationPackage | 已完成 V1.4 | `MigrationPackageManifest` 声明 source/target、affected sections、reversible 和 checksum；`MigrationPackageService::verify_preflight` 输出 ready/planned/blocked。 |
| ReleaseSnapshot | 已完成 V1.4 | `ReleaseSnapshotService::create` 将 snapshot 绑定到已验证 backup manifest、release ref、generation 和 health status。 |
| Restore plan | 已完成 V1.4 | `ReleaseSnapshotService::restore_plan` 只返回 steps、risks、audit 和 `apply_allowed:false`。 |
| CLI | 已完成 V1.4 | `eva backup create`、`eva snapshot create`、`eva restore plan` 输出统一 JSON envelope。 |

## Public API

| 类型/函数 | 说明 |
| --- | --- |
| `BackupEntry` | backup scope 中的一条相对路径和 bytes，可标记 redacted。 |
| `BackupScope` | project id 与 entries 的集合，拒绝空 scope。 |
| `BackupPlan` | request、generation、actor、reason、scope、dry-run 和风险提示。 |
| `BackupManifest` | artifact type、request、generation、project、entries、digest 和 audit。 |
| `BackupService::create` | 写入 `ArtifactStore`，生成 manifest，并立即校验 digest。 |
| `ManifestVerifier::verify_artifact` | 校验 artifact digest，失败时输出 expected/actual digest context。 |
| `RestoreApplyValidator::dry_run` | 校验 restore apply plan 指向的 backup artifact key 和 digest；输出 `apply_allowed:false`。 |
| `MigrationPackageManifest` | 迁移包 id、source/target version、affected sections、checksum。 |
| `MigrationPackageService::verify_preflight` | 校验当前版本是否匹配 source version，并提示不可逆迁移风险。 |
| `ReleaseSnapshotService::create` | 创建 pre/post release snapshot。 |
| `ReleaseSnapshotService::restore_plan` | 生成 plan-first restore plan，不执行恢复。 |

## CLI 验证入口

```powershell
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
```

`restore plan` 的 JSON 中必须保持：

- `status: "planned"`
- `apply_allowed: false`
- `steps` 包含 snapshot manifest、lifecycle operation lease、drain、stage restore 和 health verification。

## 模块边界

`eva-backup` 做：

- 定义 backup、migration、snapshot 的计划、manifest、校验结果。
- 与 `eva-storage::ArtifactStore` 集成保存 artifact。
- 输出 restore/apply 的风险摘要和审计字段。
- 为 `eva-lifecycle` 提供可验证的 restore plan。

`eva-backup` 不做：

- 不由 Agent 直接触发不可逆 restore。
- 不替代 lifecycle generation 切换。
- 不绕过 policy 修改 workspace 或数据目录。
- 不静默忽略 digest 或 manifest mismatch。
- 不在 V1.4 做加密、签名、远端灾备或真实磁盘 archive。

## 验证计划

```powershell
cargo test -p eva-backup
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
```

当前测试覆盖：

- backup artifact round-trip 和 digest verification。
- digest mismatch 返回 `Conflict`。
- migration source version mismatch 返回 blocked preflight。
- release snapshot restore plan 保持 plan-first。

## English

`eva-backup` owns V1.4 backup artifacts, migration package preflight, release snapshots, manifest verification, and plan-first restore boundaries. It verifies artifacts but does not execute destructive restore in V1.4.

P6-004 adds `ReleasePointerPlan` through
`ReleaseSnapshotService::release_pointer_plan`. The plan validates snapshot
confirmation and records pointer-change steps, risks, and audit while keeping
`apply_allowed:false`.
