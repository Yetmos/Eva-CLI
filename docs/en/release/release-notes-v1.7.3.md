# Eva-CLI V1.7.3 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.7.3-alpha`

Eva-CLI V1.7.3-alpha completes the V1.7.3 Lua resource-limit slice. It builds
on the V1.7.1 real Lua VM boundary and V1.7.2 host bindings by making Lua
execution interruptible through wall-clock timeout, instruction budget,
cancellation token, and memory budget checks.

## Highlights

- Adds `LuaExecutionLimits` for wall-clock timeout, instruction budget,
  cancellation token, and memory limit configuration.
- Uses the `mlua` hook boundary to interrupt long-running Lua scripts before
  they can monopolize the runtime.
- Maps Lua timeout to stable `ErrorKind::Timeout` / `lua_timeout` evidence.
- Maps instruction-budget exhaustion to stable
  `lua_instruction_budget_exceeded` evidence with the configured budget in
  error context.
- Adds `LuaCancellationToken` and propagates cancellation through runtime basic
  run options so cancelled scripts stop before later capability calls.
- Configures `Lua::set_memory_limit` when a memory budget is present and maps
  Lua memory allocation failures to `lua_memory_limit_exceeded`.
- Adds release gate `REL-LUA-RESOURCE-LIMITS-001` to `release check`.

## Compatibility

V1.7.3-alpha is additive for public CLI commands, JSON envelopes, exit codes,
and Lua host bindings. Existing scripts that do not opt into explicit limits
continue to run through the default compatibility path.

It remains an alpha checkpoint because shadow load, generation swap, rollback,
real provider execution, signed installers, OS packages, destructive apply
paths, and provenance bundles remain future work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test -p eva-lua-host`
- `cargo test -p eva-runtime timeout_basic_run_records_dead_letter_and_replay`
- `cargo test -p eva-runtime cancelled_basic_run_returns_task_record`
- `cargo test -p eva-release`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.7.3-alpha`
- `cargo run -- --version`
- `cargo run -- run --example basic --output json`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.7.3-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.7.3-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths,
complete provider/runtime recovery, and provenance bundles remain future release
scope.
