# Eva-CLI V1.7.1 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.7.1-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.7.1-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.7.1-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.7.1-alpha) |
| Commit | [`8b54135`](https://github.com/Yetmos/Eva-CLI/commit/8b54135a1375) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.7.1-alpha) |
| Release workflow | [Run 28847938482](https://github.com/Yetmos/Eva-CLI/actions/runs/28847938482), successful |

## Included In The Tag

- `MluaVmAdapter` with vendored Lua 5.4 and only table, string, utf8, and math
  libraries loaded.
- Real `on_event` execution for supported return/global/table script shapes.
- Controlled event and context data without file, socket, process, memory
  service, or provider handles.
- Stable `lua_syntax_error` and `lua_runtime_error` mapping without host paths.
- A compatibility fallback for legacy controlled scripts and compiled gate
  `REL-LUA-VM-EXECUTION-001`.

## Not Included

This tag did not contain `ctx.tools`, host log/audit bindings, execution
budgets, shadow load, generation routing, or live provider execution. Later
V1.7 tags added all of those boundaries except live provider execution.

## Reproduce The Release

```powershell
git switch --detach v1.7.1-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-lua-host
cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability
./scripts/validate-version-management.ps1 -Tag v1.7.1-alpha
cargo run -- run --example basic --output json
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.7.1-alpha`.

The Lua gate represented compiled fixture evidence, not production script
isolation or provider validation.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
