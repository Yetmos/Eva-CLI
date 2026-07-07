# Eva-CLI V1.6.3 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.6.3-alpha`

Eva-CLI V1.6.3-alpha completes the V1.6.3 durable task/audit/artifact store
slice. It builds on the V1.6.1 durable backend layout and V1.6.2 durable
EventBus redrive baseline by making task snapshots restart-readable through
the durable backend, adding durable audit records, and hardening filesystem
artifact metadata.

## Highlights

- Adds `FileSystemTaskStateStore::from_durable_layout` so `run --example basic`
  and `task status/logs/cancel` can use the durable backend `tasks/` directory
  through `--durable-backend <path>`.
- Adds `FileSystemAuditSink` and `AuditRecord` under `eva-storage`, storing
  durable audit records under `audit/` with action, outcome, trace entries,
  message, fields, and millisecond timestamps.
- Adds trace-id lookup for durable audit records across span, request, event,
  correlation, and causation identifiers.
- Upgrades filesystem artifact metadata to v2 with key, digest, size, content
  type, retention policy, and optional retain-until timestamp.
- Keeps legacy artifact metadata readable while returning stable conflict
  errors for corrupt content type or retention metadata.
- Adds release gate `REL-DURABLE-STORES-001` to `release check`.

## Compatibility

V1.6.3-alpha does not change public CLI command names, JSON envelope shape,
exit codes, or existing V1.5/V1.6 release commands. The new durable backend
flag is additive, and legacy artifact metadata remains readable.

It remains an alpha checkpoint because runtime crash recovery, scheduler
backoff dispatch, runtime audit wiring, durable smoke diagnostics, signed
installers, and destructive apply gates are still planned follow-up work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.6.3-alpha`
- `cargo run -- --version`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.6.3-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.6.3-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths, full
runtime crash recovery, and provenance bundles remain future release scope.
