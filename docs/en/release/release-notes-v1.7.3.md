# Eva-CLI V1.7.3 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.7.3-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.7.3-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-07 |
| Status | Alpha prerelease |
| Tag | [`v1.7.3-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.7.3-alpha) |
| Commit | [`31f8470`](https://github.com/Yetmos/Eva-CLI/commit/31f84700529c) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.7.3-alpha) |
| Release workflow | [Run 28857036909](https://github.com/Yetmos/Eva-CLI/actions/runs/28857036909), successful |

## Included In The Tag

- `LuaExecutionLimits` for wall-clock timeout, instruction budget,
  cancellation, and memory limit.
- `mlua` hook interruption and stable `lua_timeout`,
  `lua_instruction_budget_exceeded`, and `lua_memory_limit_exceeded` evidence.
- `LuaCancellationToken` propagation through the basic runtime.
- Compatibility behavior for scripts without explicit limits and compiled gate
  `REL-LUA-RESOURCE-LIMITS-001`.

## Not Included

This tag did not contain shadow loading, generation route changes, drain or
rollback lifecycle evidence, daemon orchestration, or live provider execution.
V1.7.4 added the planning/evidence boundaries, not a production hot reload.

## Reproduce The Release

```powershell
git switch --detach v1.7.3-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-lua-host
cargo test -p eva-runtime timeout_basic_run_records_dead_letter_and_replay
cargo test -p eva-runtime cancelled_basic_run_returns_task_record
./scripts/validate-version-management.ps1 -Tag v1.7.3-alpha
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained release/package evidence and unsigned Linux/Windows/macOS
  native archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.7.3-alpha`.

The resource-limit gate used controlled tests; it was not a hostile workload or
multi-tenant production isolation certification.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
