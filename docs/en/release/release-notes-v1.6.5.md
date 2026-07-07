# Eva-CLI V1.6.5 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.6.5-alpha`

Eva-CLI V1.6.5-alpha completes the V1.6.5 durable diagnostics and smoke-gate
slice. It builds on the V1.6.1-V1.6.4 durable backend, EventBus, task/audit,
artifact, and recovery evidence by adding a stable read-only diagnostics report,
an `inspect.durable` CLI envelope, and CI/release smoke coverage for durable
backend inspection.

## Highlights

- Adds read-only opening paths for filesystem EventLog, dead-letter store, and
  DurableEventBus so diagnostics can inspect empty durable stores without
  creating `events/log` or `events/dead_letters` directories.
- Adds `eva_runtime::inspect_durable_backend()` and `DurableDiagnosticsReport`.
- Reports durable backend path, mode, schema version, layout version, migration
  status, event log count, dead-letter count, and `pending_redrive_count`.
- Counts pending redrive candidates only when the dead-letter is due and the
  original event has not already been acknowledged.
- Adds `eva inspect durable --durable-backend <path>` with text output and a
  stable `inspect.durable` JSON envelope.
- Adds release gate `REL-DURABLE-DIAGNOSTICS-001` and CI/release workflow smoke
  commands that create a durable backend and inspect it.
- Captures `inspect-durable.json` as release evidence.

## Compatibility

V1.6.5-alpha is additive. Existing public CLI commands, JSON envelope shape, and
exit codes remain compatible. The new `inspect durable` command requires an
explicit `--durable-backend <path>` and opens the backend read-only.

It remains an alpha checkpoint because full runtime audit wiring, scheduler
backoff dispatch, provider process recovery, real apply gates, signed installers,
OS packages, and provenance bundles remain future work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.6.5-alpha`
- `cargo run -- --version`
- `cargo run -- inspect durable --durable-backend .eva/local-durable-smoke --output json`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.6.5-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.6.5-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths,
complete provider/runtime recovery, and provenance bundles remain future release
scope.
