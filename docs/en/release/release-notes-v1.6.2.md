# Eva-CLI V1.6.2 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.6.2-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.6.2-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.6.2-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.6.2-alpha) |
| Commit | [`ec6d3e1`](https://github.com/Yetmos/Eva-CLI/commit/ec6d3e14c388) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.6.2-alpha) |
| Release workflow | [Run 28837592529](https://github.com/Yetmos/Eva-CLI/actions/runs/28837592529), successful |

## Included In The Tag

- `FileSystemEventLog` under `events/log` with restart-readable event and
  delivery metadata.
- `DurableEventBus` and `FileSystemDeadLetterStore` under
  `events/dead_letters`.
- Redrive child IDs using `:replay-N` and persisted retry timing fields.

## Not Included

`retry_delay_ms` and `next_attempt_after_ms` were compatibility metadata, not
a delayed scheduler. This tag also omitted durable task/audit stores, runtime
recovery, and durable diagnostics; later V1.6 tags supplied those slices.

## Reproduce The Release

```powershell
git switch --detach v1.6.2-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-eventbus -p eva-storage
./scripts/validate-version-management.ps1 -Tag v1.6.2-alpha
cargo run -- --version
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained `release-evidence-v1.6.2-alpha`,
  `package-evidence-v1.6.2-alpha`, and unsigned Linux/Windows/macOS native
  archives with evidence; Actions retention is independent of the Release.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.6.2-alpha`.

The V1.6.2 release gate represented built-in evidence and did not execute CI,
redrive scheduling, or a production durability test.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
