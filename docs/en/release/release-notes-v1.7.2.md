# Eva-CLI V1.7.2 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.7.2-alpha`

Eva-CLI V1.7.2-alpha completes the V1.7.2 Lua host binding slice. It builds
on the V1.7.1 real Lua VM boundary by exposing read-only request, trace, and
memory context tables, Lua host observability, and a controlled `ctx.tools.call`
path through `CapabilityHostApi`.

## Highlights

- Adds read-only `ctx.request`, `ctx.trace`, and `ctx.memory` tables while
  retaining the V1.7.1 top-level context count fields for compatibility.
- Removes the Lua global `rawset` entry to keep read-only context snapshots
  from being bypassed by scripts.
- Adds `ctx.host.log(level, message)` and `ctx.host.audit(message)`, producing
  traceable `LuaHostObservation` records that runtime task logs and CLI JSON can
  report.
- Adds `LuaHost::run_on_event_with_tools` and `ctx.tools.call(capability,
  value)`, routing through `CapabilityHostApi` without exposing raw provider,
  file, socket, process, memory-service, knowledge-service, or audit-sink
  handles.
- Converts Lua nil, booleans, numbers, strings, arrays, and object tables into
  JSON-compatible text payloads for capability invocation.
- Returns a constrained Lua response table containing `request_id`, `status`,
  `ok`, `output`, `error`, and `error_kind`.
- Rejects unknown or disabled capability calls through the Lua host boundary.
- Updates the basic example Lua script to call `config.lint` directly from
  `ctx.tools.call` instead of returning a legacy `capability` field for a
  runtime-side second pass.
- Adds release gate `REL-LUA-HOST-BINDINGS-001` to `release check`.

## Compatibility

V1.7.2-alpha is additive for public CLI commands, JSON envelopes, and exit
codes. Legacy `LuaEventResult` fields, the V1.7.1 static parser compatibility
fallback, durable diagnostics, and release checks remain compatible.

It remains an alpha checkpoint because Lua timeout/instruction budgets, memory
limits, shadow load, generation swap, rollback, real provider execution, signed
installers, OS packages, destructive apply paths, and provenance bundles remain
future work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test -p eva-lua-host`
- `cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability`
- `cargo test -p eva-cli run_basic_example_json_succeeds`
- `cargo test -p eva-release`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.7.2-alpha`
- `cargo run -- --version`
- `cargo run -- run --example basic --output json`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.7.2-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.7.2-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths,
complete provider/runtime recovery, and provenance bundles remain future release
scope.
