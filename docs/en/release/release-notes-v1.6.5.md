# Eva-CLI V1.6.5 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.6.5-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.6.5-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.6.5-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.6.5-alpha) |
| Commit | [`e63b6bc`](https://github.com/Yetmos/Eva-CLI/commit/e63b6bc71894) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.6.5-alpha) |
| Release workflow | [Run 28845812493](https://github.com/Yetmos/Eva-CLI/actions/runs/28845812493), successful |

## Included In The Tag

- Read-only EventLog, dead-letter, and EventBus opening paths.
- `eva_runtime::inspect_durable_backend()` and
  `DurableDiagnosticsReport` with schema, layout, migration, event,
  dead-letter, and pending-redrive counts.
- `eva inspect durable --durable-backend <path>` with text and
  `inspect.durable` JSON output.
- Compiled gate `REL-DURABLE-DIAGNOSTICS-001` and
  `inspect-durable.json` workflow evidence.

## Not Included

This tag did not wire a complete runtime audit path, scheduler backoff
dispatcher, provider process recovery, destructive apply, production database,
or signed distribution. The diagnostic command was read-only.

## Reproduce The Release

```powershell
git switch --detach v1.6.5-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/validate-version-management.ps1 -Tag v1.6.5-alpha
$durable = Join-Path $env:TEMP "eva-v1.6.5-smoke"
cargo run -- run --example basic --task-id req-v165 --durable-backend $durable --output json
cargo run -- inspect durable --durable-backend $durable --output json
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.6.5-alpha`.

The diagnostics gate verified built-in fixture evidence; it did not execute
production storage or recovery.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
