# eva-release / Release Hardening

Updated: 2026-07-20

`eva-release` owns the V1.5 release-hardening boundary and later additive
runtime-readiness gates. It turns the final 1.x readiness work into executable
Rust contracts instead of leaving it as a manual checklist. The crate does not
build installers, run signing services, execute scanners, run benchmarks, or
start runtime processes by itself. It parses and verifies the readiness evidence
that Eva-CLI and CI can prove today, then exposes that evidence to
`eva-cli release ...`.

## Implemented Scope

| Area | Public Type | Behavior |
| --- | --- | --- |
| Release checklist | `ReleaseHardeningService`, `ReleaseReadinessReport`, `ReleaseGate` | Aggregates cross-platform, stability, docs, security, performance, migration, durable runtime, Lua runtime, signed backup archive, restore apply/rollback/operator confirmation, supervisor handoff, service-manager abstraction, daemon runtime, MCP compatibility, provider supervision, hardware safety, observability policy, public JSON contract readiness, and the V1.x closure report. |
| Cross-platform readiness | `PlatformReadiness` | Records Windows/Linux/macOS CI expectations, shell model, path assumptions, and smoke commands. |
| Stability readiness | `StabilityScenario` | Captures task diagnostics, cancellation, dead-letter replay, restore planning, and upgrade planning scenarios. |
| Security review | `SecurityReviewReport`, `SecurityFinding`, `SecuritySeverity` | Covers policy, Lua sandbox, secret redaction, MCP allowlist, V1.15.1 hardware OS permission diagnostics/raw-handle boundaries, and lifecycle apply risk. |
| Performance baseline | `PerformanceBaselineReport`, `PerformanceBudget`, `PerformanceObservation` | Defines source-release smoke budgets separately from observations; missing and synthetic observations remain `unmeasured`/Warn instead of passing. |
| Migration and compatibility | `MigrationGuide`, `MigrationStep`, `CompatibilityPolicy` | Documents V1.4 -> V1.5 migration steps, compatibility guarantees, public CLI contracts, and deprecation policy. |
| Backup archive readiness | `ReleaseGate` | Records V1.10.3 signed backup archive, optional sealing, remote target contract, and pre-restore evidence as release readiness evidence. |
| Restore apply gate readiness | `ReleaseGate` | Records V1.10.4 confirmation, policy approval, filesystem lock and health gate, V1.14.2 staged file mutation evidence, V1.14.3 rollback apply evidence, and V1.14.4 operator confirmation output. |
| Supervisor handoff readiness | `ReleaseGate` | Records V1.10.5 blue-green handoff, release pointer mutation, persisted state, and rollback-on-health-failure evidence. |
| Service-manager abstraction readiness | `ReleaseGate` | Records the original V1.14.5 `ServiceManagerAdapter`, Fake handoff/rollback evidence, and explicit `service_manager` config. Platform adapters and the identity-bound direct daemon entrypoint now exist in code, but this gate still stops before controlled real-host stop/boot/reboot evidence, a destructive harness, and production certification. |
| Release artifact evidence | `ReleaseArtifactEvidence`, `ReleaseArtifactVerificationReport` | Parses a V1.11.1 key/value evidence manifest, verifies a SHA-256 keyed signature, source commit provenance, SBOM marker, and scan status, then exposes a blocking release gate when evidence is supplied. |
| Distribution evidence | `ReleaseDistributionEvidence`, `ReleaseDistributionVerificationReport` | Parses a V1.11.2 key/value evidence manifest for Windows/Linux/macOS install smoke, install/upgrade/uninstall docs, and package-manager dry-run status, then exposes a blocking release gate when evidence is supplied. |
| Security scan evidence | `ReleaseSecurityScanEvidence`, `ReleaseSecurityScanVerificationReport` | Parses a V1.11.3 external scanner evidence manifest and blocks readiness when the scanner did not pass or any high/critical finding is present. |
| Benchmark evidence | `ReleaseBenchmarkEvidence`, `ReleaseBenchmarkVerificationReport` | Parses V1.11.3 measurements, validates producer-claimed thresholds against the consumer-owned release workflow budget catalog, and blocks unknown, mismatched, failed, or over-budget evidence. |
| MCP compatibility readiness | `ReleaseGate` | Records V1.13.7 stdio/HTTP transport, tool schema, stream lifecycle, dangling-session, and explicit server-surface fixture evidence as `REL-MCP-COMPAT-001`. |
| Provider supervision readiness | `ReleaseGate` | Records V1.13.8 supervisor slot, process table, credential scope, admission, stream artifact, recovery, and MCP compatibility gate evidence as `REL-PROVIDER-SUPERVISION-001`. |
| Hardware safety readiness | `ReleaseGate` | Records V1.15.5 simulator parity, permission denial, lease cleanup, and daemon hotplug smoke evidence as `REL-HARDWARE-SAFETY-001`, accepting simulator-only alpha evidence without claiming real device I/O. |
| Observability policy readiness | `ReleaseGate` | Records V1.16.1-V1.16.4 runtime audit wiring, tracing bridge, SDK exporter smoke, and retention/rotation/corrupt-record policy as `REL-OBSERVABILITY-POLICY-001`, without claiming a real database sink. |
| Public JSON contract readiness | `ReleaseGate` | Records V1.17.4 golden subset fixtures and `scripts/validate-cli-json-contracts.ps1` as `REL-JSON-CONTRACT-001`, allowing additive fields while blocking removed or renamed public JSON fields. |
| V1.x closure readiness | `V1xClosureReport`, `ReleaseGate` | Records V1.17.6 `REL-V1X-CLOSURE-001`, required gate coverage, additive `closure` JSON, and external blockers for production signing, package repositories, platform service-manager tests, real hardware fixtures, and production database sink work. |

## CLI Surface

`eva-cli` exposes the crate through these commands:

```powershell
cargo run -- release check --output json
cargo run -- release check --artifact-evidence release-evidence/release-artifact.evidence --output json
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
cargo run -- release check --security-scan-evidence release-evidence/release-security-scan.evidence --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release check --target windows --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release perf --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release migration --output json
```

All commands use the standard CLI JSON envelope:

- `ok`
- `command`
- `exit_code`
- `data`
- `trace`

`release check` returns exit code `0` when no required gate is blocked. A
blocked required gate maps to configuration exit code `2`; blocked security
findings map to policy exit code `3`; failed, policy-mismatched, or over-budget
benchmark evidence maps to runtime-unavailable exit code `4`. A default alpha
`release perf` remains executable with exit code `0`, but reports `unmeasured`
and never emits a measured Pass without benchmark evidence. V1.17.6 also returns additive
`data.closure`; its `ready_with_external_blockers` status records production
blockers without changing the alpha readiness exit behavior when all required
local gates pass.

## Boundary Rules

`eva-release` does:

- collect release gates into stable Rust structs;
- make the cross-platform CI matrix visible to the CLI;
- keep security findings explicit and auditable;
- define performance budgets as release-smoke contracts;
- document the migration and compatibility policy that V1.5 promises;
- expose signed backup archive, pre-restore evidence, restore apply/rollback/operator confirmation, and supervisor handoff evidence as readiness gates;
- expose the V1.14.5 service-manager abstraction and Fake handoff/rollback evidence as a required readiness gate, while keeping code-only platform entrypoint coverage distinct from production evidence;
- expose V1.15.1 hardware OS permission diagnostics through `SEC-HW-001` security evidence;
- expose V1.15.4 daemon hotplug subscriber and watcher crash lease-release evidence through `SEC-HW-001` security evidence;
- expose V1.15.5 hardware safety evidence through the required `REL-HARDWARE-SAFETY-001` readiness gate;
- expose V1.16 observability policy evidence through the required `REL-OBSERVABILITY-POLICY-001` readiness gate;
- expose signed artifact, distribution install smoke, and package-manager dry-run evidence as opt-in readiness gates;
- expose external scanner and measured benchmark evidence as opt-in readiness gates;
- expose the repo-local MCP compatibility matrix as a required readiness gate;
- expose the current provider supervision baseline as a required readiness gate;
- expose public CLI JSON contract fixtures as a required readiness gate;
- expose a V1.x closure report that summarizes required local gates and records production-only external blockers;
- preserve known future risks as warnings instead of silently enabling apply paths.

`eva-release` does not:

- run package signing services or artifact publishing;
- execute destructive restore;
- replace OS service-manager supervision;
- claim production Windows Service, systemd, or launchd supervision from Fake, adapter-unit, or direct-entrypoint code tests;
- run external security scanners directly;
- execute benchmark commands directly;
- certify external MCP servers or provide HTTPS/TLS transport;
- manage OS provider processes or claim user-isolated OS supervision;
- claim real USB/serial/BLE/socket/vendor SDK I/O from simulator-only hardware safety evidence;
- replace `eva-backup`, `eva-lifecycle`, `eva-policy`, or CI.

## Verification

```powershell
cargo test -p eva-release
cargo test -p eva-cli
./scripts/validate-cli-json-contracts.ps1
cargo run -- release check --output json
cargo run -- release check --artifact-evidence release-evidence/release-artifact.evidence --output json
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
cargo run -- release check --security-scan-evidence release-evidence/release-security-scan.evidence --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release perf --benchmark-evidence release-evidence/release-benchmark.evidence --output json
cargo run -- release migration --output json
```

The module tests assert that V1.5 readiness has no blocked required gates, the
security review covers the required policy/sandbox/secret/MCP/hardware
boundaries including OS permission remediation, static performance budgets stay
unmeasured, measured benchmarks enforce consumer-owned thresholds, and the V1.4 -> V1.5
migration has no breaking changes. V1.10.3 adds a required signed backup
archive gate. V1.10.4 adds a required restore apply gate for confirmation,
policy approval, lock, health, rollback-required output, and staged mutation
evidence; V1.14.2 extends that evidence with gated staged file mutation,
transaction logs, and `mutation_executed:true` only after a staged commit;
V1.14.3 adds rollback apply from the staged transaction log and pre-restore
archive entries; V1.14.4 adds operator confirmation output with confirm token,
target root, affected count, state flags, irreversible warning, and next action.
V1.10.5 adds a
required supervisor handoff gate for controlled local release pointer mutation
and persisted handoff state while real blue-green service-manager handoff remains
future work. V1.11.1 adds an opt-in required gate for supplied signed release
artifact evidence; unsigned artifacts, signature mismatch, or provenance/source
commit mismatch block `release check`. V1.11.2 adds an opt-in required gate for
supplied distribution evidence; missing Windows/Linux/macOS install smoke,
missing install/upgrade/uninstall docs, or failed package-manager dry-run blocks
`release check`. V1.11.3 adds opt-in required gates for supplied external
security scan evidence and production benchmark evidence; scanner skipped or
high/critical findings block `release check`, and benchmark evidence over budget
blocks both `release check` and `release perf --benchmark-evidence`. V1.12.6
adds required daemon runtime readiness gate `REL-DAEMON-RUNTIME-001` for the
local foreground/filesystem daemon boundary, mailbox control plane, durable task
lifecycle, scheduler retry tick, and daemon-backed agent drain/reload mutation
evidence without claiming production service-manager support. V1.13.7 adds
required MCP compatibility gate `REL-MCP-COMPAT-001` for the repo-local
compatibility matrix; it does not certify real external MCP servers, HTTPS/TLS,
or production streaming. V1.13.8 adds required provider supervision gate
`REL-PROVIDER-SUPERVISION-001` for the current controlled provider baseline; it
does not claim OS process management, OS credential vaults, or user isolation.
V1.15.5 adds required hardware safety gate `REL-HARDWARE-SAFETY-001` for
simulator parity, OS permission denial, lease cleanup, and daemon hotplug smoke
evidence; alpha accepts simulator-only evidence, while production releases must
attach real or virtual hardware fixture evidence before claiming real device I/O.
V1.17.4 adds required public JSON contract gate `REL-JSON-CONTRACT-001` for
golden subset fixtures and `scripts/validate-cli-json-contracts.ps1`; additive
fields are allowed, but removed or renamed public fields block validation.
V1.17.6 adds required observability policy gate
`REL-OBSERVABILITY-POLICY-001` and closure gate `REL-V1X-CLOSURE-001`; the
additive `closure` report requires daemon, MCP, provider, restore,
service-manager abstraction, hardware safety, observability policy, and JSON
contract gates to pass, while listing production signing, repository
publication, platform service-manager tests, real hardware fixtures, and
production database sink as external blockers.
V1.14.5 added required service-manager abstraction gate
`REL-SERVICE-MANAGER-ABSTRACTION-001` for the adapter trait, Fake handoff and
rollback evidence, explicit config parsing, and docs/progress tracking. The
current workspace additionally contains host-bound Windows Service/systemd/
launchd adapters and an identity-bound direct daemon entrypoint whose stop token
reuses the runtime drain/shutdown transaction. The existing release gate does
not yet consume controlled real-host stop/boot/reboot transcripts, a destructive
lifecycle harness, or a production service gate, and it does not prove a real
blue-green handoff.
