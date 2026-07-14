# Eva-CLI V1.7.2 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.7.2-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.7.2-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.7.2-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.7.2-alpha) |
| Commit | [`01a9349`](https://github.com/Yetmos/Eva-CLI/commit/01a934974648) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.7.2-alpha) |
| Release workflow | [Run 28852757503](https://github.com/Yetmos/Eva-CLI/actions/runs/28852757503), successful |

## Included In The Tag

- Read-only `ctx.request`, `ctx.trace`, and `ctx.memory` tables; global
  `rawset` was removed to protect the snapshot boundary.
- `ctx.host.log` and `ctx.host.audit` producing `LuaHostObservation` records.
- `ctx.tools.call` through `CapabilityHostApi` without exposing raw provider,
  file, socket, process, memory, knowledge, or audit handles.
- Constrained JSON-compatible request/response conversion and rejection of
  unknown or disabled capabilities.
- Compiled gate `REL-LUA-HOST-BINDINGS-001`.

## Not Included

The capability host boundary was not production provider supervision. This tag
also omitted timeout, instruction and memory budgets, shadow load, generation
swap, rollback execution, and destructive apply.

## Reproduce The Release

```powershell
git switch --detach v1.7.2-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-lua-host
cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability
./scripts/validate-version-management.ps1 -Tag v1.7.2-alpha
cargo run -- run --example basic --output json
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.7.2-alpha`.

The host-binding gate checked built-in evidence and did not call a production
provider fleet.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
