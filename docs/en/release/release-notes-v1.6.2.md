# Eva-CLI V1.6.2 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.6.2-alpha`

Eva-CLI V1.6.2-alpha continues the V1.6 durable runtime/storage line. It adds a
filesystem durable EventLog and a durable EventBus redrive baseline on top of
the V1.6.1 backend manifest and migration-lock layout.

## Highlights

- Adds `FileSystemEventLog` under the durable backend `events/log` directory.
- Persists publish, ack, fail, consumer, structured error, target, payload, and
  event metadata fields across reopen.
- Adds `DurableEventBus` and `FileSystemDeadLetterStore` for queryable
  dead-letter records under `events/dead_letters`.
- Adds durable redrive that creates `:replay-N` child event IDs and publishes
  new delivery attempts.
- Adds default `RedrivePolicy` fields for `retry_delay_ms` and
  `next_attempt_after_ms`; the fields are serialized for compatibility, while
  delayed scheduling remains future runtime/scheduler work.
- Marks V1.6.2 complete in the V1.x real runtime implementation plan.

## Compatibility

V1.6.2-alpha does not change public CLI command names, JSON envelope shape,
exit codes, or existing V1.5/V1.6.1 release commands. It remains an alpha
checkpoint because durable task/audit stores, runtime crash recovery, scheduler
backoff dispatch, and durable smoke diagnostics are still planned follow-up
work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `scripts/validate-i18n.ps1`
- `cargo run -- --version`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.6.2-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.6.2-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths, full
runtime crash recovery, and provenance bundles remain future release scope.
