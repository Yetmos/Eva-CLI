# Signed Installers, Provenance, and Real Apply Paths Roadmap

Date: 2026-07-06
Status: active implementation plan
Scope: GitHub Release artifacts, supply-chain provenance, native installers,
durable runtime state, real provider execution, and high-risk apply commands.

This document is the implementation ledger for the work that follows the
V1.5.1 GHCR package checkpoint. Every feature change in this program must update
the progress table before it is committed.

## Current Baseline

Eva-CLI V1.5.1 already has:

- tag-driven GitHub Release verification on Windows, macOS, and Linux;
- GHCR package publication for `ghcr.io/yetmos/eva-cli`;
- `release-evidence/package-ghcr.json` with package digest, tags, source tag,
  source SHA, package URL, and platform metadata;
- source-install documentation, release hardening checks, and V1.5 compatibility
  rules;
- plan-first backup, snapshot, restore, and upgrade commands.

Eva-CLI V1.5.1 does not yet have:

- native signed installers or OS package-manager packages;
- SLSA/GitHub Artifact Attestation provenance for binaries or release archives;
- SBOM/provenance attestations recorded for GHCR in release evidence;
- durable runtime state, event log, or artifact store;
- real provider process execution for stdio/http/MCP;
- destructive apply commands such as `restore apply`, `upgrade apply`, or
  `snapshot promote`.

## Implementation Principles

- Release provenance comes before native installers.
- Unsiged archive packaging comes before platform signing.
- Durable state comes before destructive apply commands.
- Every apply command must be plan-first, auditable, idempotent, and gated by a
  verifiable artifact digest.
- Agent and Lua code must not move release pointers, execute restore, or start
  provider processes directly.
- The GitHub Release workflow must fail closed if evidence generation, signing,
  or attestation verification fails.

## Phase Design

### Phase 1: Provenance And Release Evidence

Goal: make the existing GHCR and source release path produce stronger,
machine-verifiable evidence before adding more artifact types.

Deliverables:

- GitHub Actions permissions for OIDC and artifact attestations.
- Docker Buildx SBOM/provenance output for GHCR images.
- `release-evidence/package-ghcr.json` records provenance/SBOM status and
  verification commands.
- Release body surfaces provenance and SBOM availability.
- Documentation and website mention the new evidence boundary.

Exit criteria:

- `scripts/validate-version-management.ps1` passes.
- `scripts/build-site-i18n.ps1` and `scripts/validate-i18n.ps1` pass when site
  text changes.
- Release workflow syntax remains valid.
- The release evidence schema remains backward-compatible with V1.5.1 fields.

### Phase 2: Native Release Archives

Goal: publish cross-platform command-line archives before platform-specific
signing.

Deliverables:

- Build `eva` release binaries for Windows, macOS, and Linux.
- Package archives with stable names:
  - `eva-cli-<version>-x86_64-pc-windows-msvc.zip`
  - `eva-cli-<version>-x86_64-apple-darwin.tar.gz`
  - `eva-cli-<version>-aarch64-apple-darwin.tar.gz`
  - `eva-cli-<version>-x86_64-unknown-linux-gnu.tar.gz`
- Generate `SHA256SUMS`.
- Capture `release-evidence/native-artifacts.json`.
- Upload artifacts to the GitHub Release.

Exit criteria:

- Each archive contains the binary, README or install note, license metadata,
  and checksum evidence.
- Each binary runs `eva --version` before packaging.
- Release body lists archive names and checksum instructions.

### Phase 3: Signed Installers

Goal: add platform signing only after unsigned archives are deterministic and
validated.

Deliverables:

- Windows: signed `.exe` archive or installer using the configured signing
  provider.
- macOS: Developer ID signed and notarized tar/pkg path.
- Linux: signed checksum file first, then `.deb`/`.rpm` when package metadata is
  ready.
- `release-evidence/signing.json` with signer identity, certificate metadata,
  timestamp, verification status, and unsigned fallback status.

Exit criteria:

- Signing failures block signed artifact publication.
- Unsigned fallback is explicit and never mislabeled as signed.
- Verification commands are documented for every platform.

### Phase 4: Durable Stores

Goal: replace in-memory runtime evidence with durable interfaces before real
apply commands.

Deliverables:

- `FilesystemArtifactStore` or `SqliteArtifactStore` with SHA-256 digests.
- Durable task state and event log interfaces.
- CLI flags to select project-local durable stores.
- Migration path that preserves current in-memory tests.

Exit criteria:

- `backup create`, `snapshot create`, and `restore plan` can read/write durable
  artifacts.
- Tests cover digest mismatch, missing artifact, and replay boundaries.
- Existing V1.5 JSON envelopes remain compatible.

### Phase 5: Real Provider Process Execution

Goal: allow controlled provider execution without opening destructive runtime
apply paths yet.

Deliverables:

- stdio process runner with allowlisted command, timeout, env scrubbing, and
  output limit.
- HTTP provider runner with URL allowlist, method restrictions, timeout, and
  audit fields.
- MCP process/session boundary with explicit startup and shutdown.
- Provider invocation audit linked to trace fields.

Exit criteria:

- Disabled providers remain inert by default.
- Every provider execution returns structured success/failure and audit fields.
- Tests cover timeout, denied command, oversized output, and process failure.

### Phase 6: Destructive Apply Gates

Goal: expose high-risk apply commands only after durable stores and provider
execution boundaries are proven.

Deliverables:

- `restore apply --plan <path> --confirm <plan_id>`.
- `upgrade apply --plan <path> --confirm <plan_id>`.
- `snapshot promote --snapshot-id <id> --confirm <snapshot_id>`.
- Runtime audit records for lock acquisition, backup-before-apply, apply,
  rollback candidate, and release pointer changes.

Exit criteria:

- Apply commands refuse to run without a matching plan ID, artifact digest,
  backup evidence, policy approval, and explicit confirmation.
- Interrupted apply can be inspected and safely resumed or rolled back.
- Agent/Lua access remains request-only; Runtime owns execution.

## Detailed Progress Table

Status values:

- Planned: not started.
- In Progress: current working item.
- Done: implemented, verified, committed, and pushed.
- Blocked: cannot proceed without external credential, platform service, or
  product decision.

| ID | Feature Modification | Files / Areas | Acceptance Checks | Status | Commit |
| --- | --- | --- | --- | --- | --- |
| P0-001 | Create this implementation ledger and register it in docs/site indexes | `docs/en/release/signed-provenance-apply-roadmap.md`, `docs/zh-CN/release/signed-provenance-apply-roadmap.md`, `docs/_i18n/manifest.json`, docs/site indexes | `scripts/build-site-i18n.ps1`; `scripts/validate-i18n.ps1`; `scripts/validate-version-management.ps1` | Done | `0a3f6a6` |
| P1-001 | Add OIDC/attestation permissions and Buildx SBOM/provenance settings to GHCR release job | `.github/workflows/release.yml` | Workflow diff review; `scripts/validate-version-management.ps1` | Done | `1be7c44` |
| P1-002 | Extend `package-ghcr.json` with provenance and SBOM fields while preserving existing fields | `.github/workflows/release.yml`, release docs | `scripts/validate-version-management.ps1`; JSON field review | Done | `66416f7` |
| P1-003 | Update release body text to report GHCR provenance/SBOM availability | `.github/workflows/release.yml`, release docs | Release body generation review | Done | `cd55bf1` |
| P1-004 | Update website/docs to expose provenance status after the workflow change | `website/_i18n/*.json`, generated website pages, docs index | `scripts/build-site-i18n.ps1`; `scripts/validate-i18n.ps1` | Done | `4699b55` |
| P2-001 | Add release archive naming and manifest schema for native binaries | release docs, `.github/workflows/release.yml` | Workflow review; release evidence field review | Done | `6159169` |
| P2-002 | Build and smoke-test Windows release archive | `.github/workflows/release.yml` | `eva --version` inside packaged Windows artifact | Done | `9f4c37d` |
| P2-003 | Build and smoke-test Linux release archive | `.github/workflows/release.yml` | `eva --version` inside packaged Linux artifact | Done | `5533612` |
| P2-004 | Build and smoke-test macOS x86_64 and aarch64 release archives | `.github/workflows/release.yml` | `eva --version` inside packaged macOS artifacts | Done | `2d5d566` |
| P2-005 | Generate `SHA256SUMS` and `native-artifacts.json` release evidence | `.github/workflows/release.yml` | Checksum verification command in workflow | Done | `834b180` |
| P3-001 | Define signing provider configuration and failure policy | release docs, repository secrets documentation | Documented secret names and fallback behavior | Done | `76b7866` |
| P3-002 | Add Windows signing path | `.github/workflows/release.yml` | Signed artifact verification command | Blocked: signing credential required | Blocked: `WINDOWS_SIGNING_PROVIDER` and provider credentials are not configured |
| P3-003 | Add macOS signing and notarization path | `.github/workflows/release.yml` | Notarization verification command | Blocked: Apple Developer credential required | Blocked: Apple Developer ID and notarization credentials are not configured |
| P3-004 | Add signed checksum/provenance bundle for Linux archives | `.github/workflows/release.yml` | Signature verification command | Done | `d9e0498` |
| P4-001 | Replace lightweight artifact digest contract with SHA-256 while preserving old test intent | `crates/eva-storage` | `cargo test -p eva-storage` | Done | `528e9f4` |
| P4-002 | Add filesystem artifact store implementation | `crates/eva-storage` | artifact round trip, digest mismatch, missing artifact tests | Done | `ab9fa98` |
| P4-003 | Wire durable artifact store into backup/snapshot/restore commands behind explicit flags | `crates/eva-cli`, `crates/eva-backup` | CLI smoke commands with project-local artifact directory | Done | `d3745bf` |
| P4-004 | Add durable event/task state interface | `crates/eva-storage`, `crates/eva-runtime`, `crates/eva-cli` | task status/logs survive process boundary in tests | Done | `a27b2b0` |
| P5-001 | Add stdio provider runner contract and tests | `crates/eva-adapter` | denied command, timeout, output limit tests | Done | `0fce9eb` |
| P5-002 | Add HTTP provider runner contract and tests | `crates/eva-adapter` | URL allowlist, method denial, timeout tests | Done | `93dbfa7` |
| P5-003 | Add MCP process/session startup boundary | `crates/eva-mcp`, `crates/eva-adapter` | startup failure and shutdown tests | In Progress | local |
| P5-004 | Link provider invocation audit to trace fields | `crates/eva-adapter`, `crates/eva-observability`, `crates/eva-cli` | CLI JSON includes trace/audit fields | Planned | pending |
| P6-001 | Add restore apply command parser that refuses execution without durable stores | `crates/eva-cli` | command returns policy/runtime unavailable with stable JSON | Planned | pending |
| P6-002 | Implement restore apply dry-run validation over durable artifacts | `crates/eva-backup`, `crates/eva-cli` | digest mismatch and missing backup tests | Planned | pending |
| P6-003 | Add upgrade apply command parser and lock model | `crates/eva-lifecycle`, `crates/eva-cli` | lock acquisition and conflict tests | Planned | pending |
| P6-004 | Add snapshot promote command parser and release pointer plan | `crates/eva-backup`, `crates/eva-lifecycle`, `crates/eva-cli` | confirmation and audit tests | Planned | pending |

## Per-Change Update Rule

For every feature modification:

1. Change one progress row from Planned to In Progress before or during the
   implementation commit.
2. Implement only that row's scope.
3. Run the row's acceptance checks plus any directly affected tests.
4. Change the row to Done and fill in the commit hash after the commit exists.
5. Push the commit to GitHub before starting the next row.

When a row cannot be completed because it requires external credentials, mark it
Blocked and document the exact missing credential or service.

## Commit Discipline

Every commit in this program must use a Chinese intent line and Lore trailers.
The `Tested:` trailer must list the commands actually run. The `Not-tested:`
trailer must explicitly name any skipped platform or credential-dependent
verification.

## Verification Matrix

| Change Type | Required Verification |
| --- | --- |
| Docs/site only | `scripts/build-site-i18n.ps1`; `scripts/validate-i18n.ps1`; `scripts/validate-version-management.ps1` |
| Release workflow only | workflow diff review; `scripts/validate-version-management.ps1`; docs/site validation if text changes |
| Rust storage/runtime | `cargo fmt --check`; targeted crate tests; broaden to `cargo test --workspace` when shared contracts change |
| CLI command surface | targeted CLI smoke; JSON envelope review; `cargo test --workspace` |
| Apply path | plan/apply/rollback tests; policy denial tests; durable artifact tests; CLI smoke |

## Rollback Policy

- Release workflow changes can be reverted without changing release tags.
- Public tags must not be moved to repair a broken release. Ship a patch release
  instead.
- Apply commands must never mutate state until all required evidence is
  verified.
- Any apply-path failure must leave enough durable audit data to identify the
  last successful step.
