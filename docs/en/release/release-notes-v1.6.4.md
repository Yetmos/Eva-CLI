# Eva-CLI V1.6.4 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.6.4-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.6.4-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.6.4-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.6.4-alpha) |
| Commit | [`142c88f`](https://github.com/Yetmos/Eva-CLI/commit/142c88f68eb4) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.6.4-alpha) |
| Release workflow | [Run 28844115726](https://github.com/Yetmos/Eva-CLI/actions/runs/28844115726), successful |

## Included In The Tag

- `RuntimeRecoveryCoordinator` scanned durable task snapshots and classified
  incomplete tasks as interrupted or recovering.
- Controlled single-event redrive required task evidence, a dead-letter record,
  an unacknowledged event, and a due retry checkpoint.
- Successful redrive receipts were written to `replayed_events`.
- `AuditAction::RuntimeRecovered` and compiled gate
  `REL-DURABLE-RECOVERY-001`.

## Not Included

This was a recovery decision and evidence baseline, not complete process or
provider recovery. It did not include scheduler dispatch, complete runtime
state restoration, SQLite/WAL indexing, destructive apply, or production
recovery validation.

## Reproduce The Release

```powershell
git switch --detach v1.6.4-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-runtime -p eva-eventbus -p eva-storage -p eva-release
./scripts/validate-version-management.ps1 -Tag v1.6.4-alpha
cargo run -- --version
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.6.4-alpha`.

`release check` evaluated built-in recovery evidence; it did not perform a
real crash-recovery deployment.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
