# Eva-CLI V1.7.4 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.7.4-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.7.4-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.7.4-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.7.4-alpha) |
| Commit | [`d8ca81c`](https://github.com/Yetmos/Eva-CLI/commit/d8ca81ce8b3b) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.7.4-alpha) |
| Release workflow | [Run 28859068979](https://github.com/Yetmos/Eva-CLI/actions/runs/28859068979), successful |

## Included In The Tag

- `LuaShadowLoader` dry-run health checks with resource limits and a no-op
  capability host.
- `GenerationRouteGate` kept new work on the active generation until candidate
  health passed.
- `GenerationDrainEvidence` recorded route/drain planning and in-flight counts.
- `plan_generation_lifecycle_rollback()` preserved the prior healthy route and
  emitted rollback audit evidence.
- Compiled gate `REL-LUA-HOT-RELOAD-001` and marker
  `lua_hot_reload_lifecycle_ready`.

## Not Included

This tag did not start, drain, or replace a production daemon. It provided
shadow, route, drain, and rollback planning/evidence boundaries, not complete
daemon-driven hot reload, service-manager integration, provider recovery,
destructive apply, or signed distribution.

## Reproduce The Release

```powershell
git switch --detach v1.7.4-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-lua-host shadow_load
cargo test -p eva-scheduler generation
cargo test -p eva-lifecycle drain
cargo test -p eva-lifecycle rollback
./scripts/validate-version-management.ps1 -Tag v1.7.4-alpha
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.7.4-alpha`.

The hot-reload gate evaluated built-in lifecycle evidence and did not operate a
live daemon or scheduler.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
