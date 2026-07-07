# Eva-CLI V1.6.4 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.6.4-alpha`

Eva-CLI V1.6.4-alpha completes the V1.6.4 runtime crash recovery coordinator
slice. It builds on the durable backend, filesystem EventLog, durable
dead-letter redrive, task snapshots, audit sink, and artifact evidence from
V1.6.1 through V1.6.3 by adding restart recovery decisions, controlled event
redrive checkpoints, and durable recovery audit smoke.

## Highlights

- Adds `FileSystemTaskStateStore::list_snapshots()` and
  `RuntimeRecoveryCoordinator` so restart recovery can scan durable task
  snapshots and mark incomplete `queued` / `running` tasks as `interrupted` or
  `recovering`.
- Adds single-event durable redrive through `DurableEventBus::redrive_dead_letter`
  and preserves policy metadata with `RedrivePolicy`.
- Redrives only events that have task dead-letter evidence, durable dead-letter
  records, event log records that are not `acked`, and a due
  `next_attempt_after_ms` checkpoint.
- Skips already acked, not-yet-due, missing-evidence, and invalid-id redrive
  candidates with explicit report evidence.
- Writes successful redrive receipts back to task snapshots as `replayed_events`.
- Adds `AuditAction::RuntimeRecovered` and durable recovery audit records through
  `FileSystemAuditSink`.
- Adds release gate `REL-DURABLE-RECOVERY-001` to `release check`.

## Compatibility

V1.6.4-alpha does not change public CLI command names, JSON envelope shape, or
exit codes. The new recovery APIs and release gate are additive. The release
check output gains durable recovery evidence, and migration remains compatible
from V1.5.1.

It remains an alpha checkpoint because real provider process recovery, scheduler
backoff dispatch, complete runtime state restoration, SQLite/WAL indexing,
signed installers, provenance bundles, and destructive apply gates are still
planned follow-up work.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.6.4-alpha`
- `cargo run -- --version`
- `cargo run -- release check --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.6.4-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.6.4-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths,
complete provider/runtime recovery, and provenance bundles remain future release
scope.
