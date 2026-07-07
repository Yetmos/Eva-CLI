# Eva-CLI V1.7.4 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.7.4-alpha`

Eva-CLI V1.7.4-alpha completes the V1.7.4 Lua hot-reload lifecycle slice. It
builds on the V1.7.1 real Lua VM boundary, V1.7.2 host bindings, and V1.7.3
resource limits by adding shadow-load health checks, generation route gating,
old-generation drain evidence, rollback audit records, and a release readiness
gate for the hot-reload lifecycle.

## Highlights

- Adds `LuaShadowLoader`, `LuaShadowCandidate`, `LuaShadowLoadReport`, and
  script-level shadow reports for dry-run health checks before a generation can
  be promoted.
- Runs shadow-load scripts with timeout, instruction-budget, memory-budget, and
  no-op capability-host boundaries, so health checks do not switch scheduler
  routes or call real provider paths.
- Adds `GenerationRouteGate` to keep new work on the active generation until a
  candidate generation has passed shadow health.
- Adds `GenerationDrainEvidence` and generation swap drain planning so the old
  generation records `accepts_new_work=false`, in-flight counts, completion
  state, and audit evidence after a switch.
- Adds `plan_generation_lifecycle_rollback()` so failed candidates keep the
  scheduler route on the previous healthy generation and carry drain evidence
  into rollback audit records.
- Adds release gate `REL-LUA-HOT-RELOAD-001` and audit marker
  `lua_hot_reload_lifecycle_ready` to `release check`.

## Compatibility

V1.7.4-alpha is additive for public CLI commands, JSON envelopes, exit codes,
Lua host bindings, and resource-limit behavior. Existing scripts continue to
run through the same restricted `on_event(ctx)` execution boundary.

It remains an alpha checkpoint because real provider execution, full runtime
audit wiring, complete daemon-driven hot-reload orchestration, signed
installers, OS packages, destructive apply paths, and provenance bundles remain
future release scope.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test -p eva-lua-host shadow_load`
- `cargo test -p eva-scheduler generation`
- `cargo test -p eva-lifecycle drain`
- `cargo test -p eva-lifecycle rollback`
- `cargo test -p eva-release`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.7.4-alpha`
- `cargo run -- --version`
- `cargo run -- run --example basic --output json`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.7.4-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.7.4-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths,
complete provider/runtime recovery, and provenance bundles remain future release
scope.
