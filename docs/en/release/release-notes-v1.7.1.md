# Eva-CLI V1.7.1 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.7.1-alpha`

Eva-CLI V1.7.1-alpha completes the V1.7.1 Lua VM execution-boundary slice.
It upgrades `eva-lua-host` from static `on_event` field parsing to a real
`mlua`-backed VM adapter while keeping the existing `LuaEventResult`, sandbox,
and compatibility contracts intact.

## Highlights

- Adds `LuaVmAdapter` and `MluaVmAdapter` in `eva-lua-host`.
- Uses vendored Lua 5.4 through `mlua` with `default-features = false`.
- Loads only table, string, utf8, and math standard libraries for the first VM
  boundary; `os`, `io`, `package`, and `debug` are not loaded.
- Executes real Lua `on_event` handlers from `return root`, global `on_event`,
  or `root.on_event` script shapes.
- Injects a controlled event table and V1.2 context-count snapshot without raw
  file, socket, process, memory-service, or provider handles.
- Converts Lua result tables into the existing `LuaEventResult` fields so the
  basic runtime and CLI JSON output remain compatible.
- Maps compile errors to `lua_syntax_error` and handler failures to
  `lua_runtime_error` without leaking host filesystem paths.
- Keeps a compatibility fallback for legacy static-field scripts that are not
  valid Lua chunks but match the old controlled contract.
- Adds release gate `REL-LUA-VM-EXECUTION-001` to `release check`.

## Compatibility

V1.7.1-alpha is additive for public CLI commands, JSON envelopes, and exit
codes. Existing `run --example basic`, task diagnostics, durable diagnostics,
and release checks remain compatible.

It remains an alpha checkpoint because `ctx.tools`, `ctx.host`, timeout and
instruction budgets, memory limits, shadow load, generation swap, rollback,
real provider execution, signed installers, OS packages, destructive apply
paths, and provenance bundles remain future work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test -p eva-lua-host`
- `cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability`
- `cargo test -p eva-cli run_basic_example_json_succeeds`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.7.1-alpha`
- `cargo run -- --version`
- `cargo run -- inspect durable --durable-backend .eva/local-durable-smoke --output json`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.7.1-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.7.1-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths,
complete provider/runtime recovery, and provenance bundles remain future release
scope.
