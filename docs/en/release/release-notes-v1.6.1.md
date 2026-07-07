# Eva-CLI V1.6.1 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.6.1-alpha`

Eva-CLI V1.6.1-alpha starts the V1.6 durable runtime/storage line. It adds the
durable backend schema and migration-lock baseline that later EventBus, task,
audit, and artifact stores will share.

## Highlights

- Adds `FileSystemDurableBackend` with `backend.manifest`, schema version `1`,
  layout version `eva.durable.v1`, and stable `events/`, `state/`, `tasks/`,
  `audit/`, and `artifacts/` directories.
- Adds read-write migration locking with `migration.lock`, including cleanup
  when a read-write open fails during validation.
- Adds read-only backend verification that does not create files or take a
  migration lock.
- Keeps `InMemoryDurableBackend` available for tests and compatibility while
  later durable stores are implemented.
- Marks V1.6.1 complete in the V1.x real runtime implementation plan.

## Compatibility

V1.6.1-alpha does not change public CLI command names, JSON envelope shape, exit
codes, or existing V1.5 release-hardening commands. It is a prerelease because
the complete V1.6 durable EventBus, task/audit store, recovery coordinator, and
durable smoke gate remain planned follow-up work.

## Verification

- `cargo fmt --check`
- `cargo test -p eva-storage`
- `cargo check -p eva-storage`
- `git diff --check`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.6.1-alpha` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.6.1-alpha` when the release
  workflow publishes package evidence.

Signed installers, OS package-manager packages, destructive apply paths, and
full provenance bundles remain future release scope.
