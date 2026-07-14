# Eva-CLI V1.6.3 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.6.3-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.6.3-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.6.3-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.6.3-alpha) |
| Commit | [`7685e11`](https://github.com/Yetmos/Eva-CLI/commit/7685e119d424) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.6.3-alpha) |
| Release workflow | [Run 28839282148](https://github.com/Yetmos/Eva-CLI/actions/runs/28839282148), successful |

## Included In The Tag

- `FileSystemTaskStateStore::from_durable_layout` and the additive
  `--durable-backend` CLI path for basic run and task diagnostics.
- `FileSystemAuditSink` with trace-id lookup across span, request, event,
  correlation, and causation identifiers.
- Artifact metadata v2 with digest, size, content type, and retention fields;
  legacy metadata remained readable and corrupt metadata returned
  `ErrorKind::Conflict`.
- Compiled gate `REL-DURABLE-STORES-001`.

## Not Included

This tag did not include crash recovery, scheduler dispatch, runtime audit
wiring, or durable diagnostics. It also did not make the gate an external
durability or production release test.

## Reproduce The Release

```powershell
git switch --detach v1.6.3-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-storage -p eva-release
./scripts/validate-version-management.ps1 -Tag v1.6.3-alpha
cargo run -- --version
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.6.3-alpha`.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
