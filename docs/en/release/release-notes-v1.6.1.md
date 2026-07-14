# Eva-CLI V1.6.1 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.6.1-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.6.1-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.6.1-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.6.1-alpha) |
| Commit | [`db60dd8`](https://github.com/Yetmos/Eva-CLI/commit/db60dd80942b) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.6.1-alpha) |
| Release workflow | [Run 28836359238](https://github.com/Yetmos/Eva-CLI/actions/runs/28836359238), successful |

## Included In The Tag

- `FileSystemDurableBackend` with `backend.manifest`, schema version `1`,
  layout `eva.durable.v1`, and stable store directories.
- Read-write `migration.lock` handling, including cleanup after failed validation.
- A read-only verification path that did not create files or acquire the lock.
- `InMemoryDurableBackend` remained available for tests and compatibility.

## Not Included

This tag did not contain the durable EventBus, task/audit stores, recovery
coordinator, or durable diagnostics. Those were added by later V1.6 tags. It
also did not provide destructive apply or signed distribution.

## Reproduce The Release

```powershell
git switch --detach v1.6.1-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-storage
./scripts/validate-version-management.ps1 -Tag v1.6.1-alpha
cargo run -- --version
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence plus unsigned native archives and
  matching evidence for Linux, Windows, and macOS; these artifacts expire
  independently of the Release.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.6.1-alpha`.

The `REL-DURABLE-BACKEND-001` gate in `release check` was a compiled readiness
declaration; it did not run the remote workflow or a production migration.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
