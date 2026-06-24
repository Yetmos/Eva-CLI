# Backup, Migration Package, and Release Snapshot

> Language: English
> Canonical: docs/en/backup-migration-release-snapshot.md
> Translation: [简体中文](../zh-CN/备份迁移包与ReleaseSnapshot架构方案.md)

Updated: 2026-06-17

## Purpose

This document defines where Eva-CLI should place the authority for backups,
migration packages, and release snapshots.

The decision is simple: **the Runtime owns the trusted execution layer, while
Agents only plan, explain, request, and summarize operations through controlled
Runtime APIs**.

Backups, migration packages, and release snapshots are high-impact operations.
They can overwrite files, change durable state, move release pointers, or alter
the rollback surface. Their correctness must be deterministic, reproducible,
auditable, and recoverable. They must not depend on an LLM prompt as the source
of truth for file boundaries, overwrite policy, schema compatibility, or
rollback behavior.

## Scope

This document covers three related operation families:

| Operation | Runtime responsibility | Agent responsibility |
| --- | --- | --- |
| Backup | Create, verify, list, retain, restore, and audit recoverable artifacts. | Recommend scope, explain impact, request a backup, and summarize results. |
| Migration package | Build, verify, sign or checksum, dry-run, apply, reject, and audit versioned migration artifacts. | Draft rationale, map user intent to a package request, explain compatibility warnings, and analyze failure logs. |
| Release snapshot | Capture before/after release state, compare snapshots, attach health evidence, and support rollback decisions. | Prepare release notes, interpret snapshot diffs, and request Runtime-controlled snapshot operations. |

The Runtime is responsible for the artifact and state transition. The Agent is
responsible for intent handling and human-facing explanation.

## Core Decision

Eva-CLI should treat backup, migration, and release snapshot capabilities as
Runtime services:

```text
Agent
  -> proposes intent
  -> calls controlled Runtime API
  -> explains result

Runtime
  -> validates policy and schema
  -> acquires locks
  -> creates manifests and checksums
  -> performs dry-run and apply boundaries
  -> writes audit records
  -> owns restore and rollback semantics
```

An Agent may never directly implement the trusted parts of these operations by
copying arbitrary paths, creating ad hoc archives, editing release pointers, or
restoring durable state outside Runtime policy.

## Ownership Boundary

### Runtime-Owned

The Runtime owns:

- backup scope resolution
- path canonicalization and workspace boundary checks
- exclusion and redaction rules
- secret handling and encryption hooks
- migration package schema validation
- package compatibility checks
- release snapshot identity and provenance
- file locks and operation leases
- atomic or staged writes
- dry-run versus apply separation
- checksum, manifest, and optional signature validation
- restore and rollback authority
- retention and garbage collection policy
- audit records and trace IDs
- failure recovery and retry rules

These are correctness and safety concerns. They must be executable code with
tests, not Agent instructions.

### Agent-Owned

Agents may:

- infer which Runtime operation the user is asking for
- collect missing non-destructive context
- recommend a backup or snapshot before risky work
- draft migration descriptions and release notes
- call Runtime APIs with explicit operation requests
- explain preflight warnings and policy denials
- summarize created artifacts and verification evidence
- analyze logs after a Runtime operation fails

Agents should not become silent policy engines. If a Runtime policy rejects an
operation, the Agent can explain the rejection but cannot bypass it.

## Runtime Services

The design should expose this capability through a small set of Runtime-owned
services.

| Service | Responsibility |
| --- | --- |
| `OperationCoordinator` | Serializes high-impact operations, owns operation IDs, leases, cancellation, and state transitions. |
| `BackupService` | Creates, verifies, restores, lists, and expires backup artifacts. |
| `MigrationPackageService` | Builds, imports, verifies, dry-runs, applies, and rejects migration packages. |
| `ReleaseSnapshotService` | Captures release state before and after activation, compares snapshots, and records rollback evidence. |
| `ArtifactStore` | Stores artifacts, manifests, checksums, metadata, retention marks, and quarantine records. |
| `ManifestVerifier` | Validates artifact manifests, schema versions, compatibility ranges, checksums, and optional signatures. |
| `PolicyEngine` | Decides whether an actor may create, restore, apply, compare, export, or delete artifacts. |
| `AuditLog` | Records actor, reason, scope, result, hashes, warnings, failures, and recovery actions. |

These services may live inside the Rust Runtime boundary and be exposed to Lua
or external Agents only through narrow host APIs.

## Artifact Model

Every backup, migration package, and release snapshot should have a manifest.
The manifest is the durable contract for what was captured, why it exists, who
requested it, and how it can be verified.

Required manifest fields:

| Field | Meaning |
| --- | --- |
| `artifact_id` | Stable ID for the artifact. |
| `artifact_type` | `backup`, `migration_package`, or `release_snapshot`. |
| `created_at` | UTC timestamp. |
| `created_by` | Human, Agent, Supervisor, or Runtime actor identity. |
| `request_id` | Causal request ID from the Agent or CLI invocation. |
| `runtime_generation` | Runtime generation that created or validated the artifact. |
| `project_id` | Project or workspace identity. |
| `scope` | Runtime-resolved scope, not raw user-provided paths. |
| `schema_version` | Manifest schema version. |
| `compatibility` | Runtime, config, state, and package compatibility constraints. |
| `entries` | Captured files, state sections, checksums, sizes, and redaction markers. |
| `policy` | Policy version and decision record. |
| `verification` | Checksum, signature, dry-run, health, or restore verification result. |
| `audit_id` | Link to the Runtime audit record. |

The manifest should be generated by Runtime code. An Agent may propose metadata
such as a reason string, but it must not be trusted to provide final checksums,
resolved paths, or compatibility results.

## Backup Semantics

A backup is a point-in-time recovery artifact for Runtime-recognized project
state. It may include:

- configuration and manifests
- Lua Agent and capability code
- AdapterRegistry metadata
- selected State Store records
- Durable Event Log watermarks or bounded segments
- memory and knowledge metadata when policy allows it
- release pointer metadata
- operation journals needed for restore safety

A backup should not include:

- raw secrets
- unredacted credentials
- unsupported transient caches
- unacknowledged in-memory events
- arbitrary user files outside the resolved workspace scope
- external provider state that cannot be restored locally

Restore must be more restricted than create. Runtime policy should require a
validated manifest, compatible target, explicit actor authority, and audit
record before restoration can mutate state.

## Migration Package Semantics

A migration package is a versioned, inspectable, and verifiable artifact that
describes a state or layout transition. It should carry enough metadata for the
Runtime to decide whether it can be applied safely.

Required package properties:

- source and target schema versions
- compatibility range
- package format version
- affected state sections
- preflight requirements
- reversible or irreversible markers
- idempotency key
- checksum manifest
- optional signature or trusted publisher identity
- expected post-apply invariants

Migration package application should be Runtime-owned because it can change
state layout, config compatibility, indexes, release pointers, or event replay
semantics. Agents can explain what a package intends to do, but they should not
execute migration logic directly.

## Release Snapshot Semantics

A release snapshot is not just a backup. It is a release-control record that
connects artifacts, Runtime generation, configuration, health, and rollback
evidence.

The Runtime should support at least two snapshot roles:

| Snapshot role | Meaning |
| --- | --- |
| `pre_release` | Captures the current known-good state before activation or upgrade. |
| `post_release` | Captures the activated state plus health and compatibility evidence. |

Useful snapshot content:

- active release pointer
- Runtime generation ID
- binary or package digest
- config digest
- policy digest
- manifest registry digest
- Lua capability generation
- AdapterRegistry generation
- database or State Store schema version
- EventBus durable watermark
- health-check results
- migration package IDs applied during the release
- backup artifact IDs associated with the release
- rollback eligibility and restrictions

The Supervisor and Runtime generation switching model should use release
snapshots as evidence. A rollback decision can be Agent-assisted, but the
snapshot capture, comparison, pointer movement, and rollback operation must be
Runtime-controlled.

## Operation State Model

High-impact operations need explicit states so retries and recovery are safe.
The state model is Runtime-owned.

```text
requested
  -> admitted
  -> locked
  -> preflighted
  -> staged
  -> verified
  -> committed
  -> audited

failure paths:
  -> rejected
  -> quarantined
  -> rolled_back
  -> failed_with_recovery_required
```

The exact implementation can evolve, but the architectural invariant is that
the Runtime can explain whether an operation never started, was rejected before
mutation, staged but not committed, committed successfully, or requires recovery.

## Agent API Shape

Agents should receive a narrow API that makes authority explicit. Example
surface:

```text
ctx.runtime.backup.request(...)
ctx.runtime.backup.status(operation_id)
ctx.runtime.backup.list(...)

ctx.runtime.migration.verify(package_ref)
ctx.runtime.migration.dry_run(package_ref, target)
ctx.runtime.migration.apply(package_ref, target)

ctx.runtime.release_snapshot.create(role, release_ref)
ctx.runtime.release_snapshot.compare(left, right)
ctx.runtime.release_snapshot.status(operation_id)
```

The API should return structured values with operation ID, artifact ID, policy
decision, warnings, verification results, and audit ID. It should not expose
raw filesystem mutation primitives.

## Policy Rules

The Runtime policy should support at least:

- actor class: human, Agent, Supervisor, Runtime, CI
- operation type: create, verify, restore, apply, compare, delete, export
- workspace or project scope
- allowed path classes
- secret redaction and encryption requirements
- retention class
- maximum artifact size
- offline or online requirement
- release generation constraints
- package trust requirements
- approval requirement for destructive restore or irreversible migration

Agents can help users understand these rules, but the Runtime must enforce
them.

## Audit and Observability

Every high-impact operation should produce durable evidence:

- operation ID
- request ID and trace ID
- actor identity
- requested reason
- policy decision
- resolved scope
- artifact IDs
- manifest hashes
- warnings
- start and finish timestamps
- failure category
- rollback or recovery action

Logs must redact secrets and should be structured enough for Agents to explain
failures without needing unrestricted filesystem access.

## Integration With Existing Architecture

This design extends the existing Eva-CLI boundary decisions:

- Rust continues to own authority, recovery, audit, schemas, and process
  lifecycle.
- Lua and external Agents continue to own orchestration and explanation only
  through controlled host APIs.
- Process-level upgrade should call `ReleaseSnapshotService` around Runtime
  generation activation.
- Hot reload should use backup or snapshot artifacts when a change can alter
  durable state or rollback eligibility.
- Migration packages should be validated before they can affect State Store,
  EventBus replay, MemoryService, KnowledgeService, or AdapterRegistry layout.
- Artifact manifests should be registered in the same policy and audit model as
  other Runtime-managed capabilities.

## Security Invariants

- No raw secret should be written into a backup, package, snapshot, or log.
- No Agent can restore or apply an artifact by editing files directly.
- No imported package can execute code during verification.
- No artifact can cross a trust boundary without manifest verification.
- No release pointer can move without a Runtime audit record.
- No rollback can claim success without post-rollback verification evidence.
- No operation should rely on hidden global state outside its manifest, policy,
  and audit record.

## Open Design Questions

The following details should be resolved when the implementation specification
is written:

- artifact storage layout and retention defaults
- encryption key management for sensitive backup sections
- signature requirements for third-party migration packages
- exact restore policy for memory and knowledge data
- event log segment capture and replay compatibility rules
- CI release snapshot format
- manual approval UX for destructive restore and irreversible migration

These are implementation-level details. They do not change the architectural
decision that trusted execution belongs to the Runtime.

## Summary

Backups, migration packages, and release snapshots are not Agent features. They
are Runtime safety capabilities.

Agents can make them easier to use by interpreting intent, drafting notes,
requesting operations, and explaining outcomes. The Runtime must remain the
source of truth for scope, policy, verification, mutation, rollback, and audit.
