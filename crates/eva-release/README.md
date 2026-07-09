# eva-release / Release Hardening

Updated: 2026-07-09

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
| Release checklist | `ReleaseHardeningService`, `ReleaseReadinessReport`, `ReleaseGate` | Aggregates cross-platform, stability, docs, security, performance, migration, durable runtime, Lua runtime, signed backup archive, restore apply gate, supervisor handoff, daemon runtime, and MCP compatibility readiness. |
| Cross-platform readiness | `PlatformReadiness` | Records Windows/Linux/macOS CI expectations, shell model, path assumptions, and smoke commands. |
| Stability readiness | `StabilityScenario` | Captures task diagnostics, cancellation, dead-letter replay, restore planning, and upgrade planning scenarios. |
| Security review | `SecurityReviewReport`, `SecurityFinding`, `SecuritySeverity` | Covers policy, Lua sandbox, secret redaction, MCP allowlist, hardware handle boundaries, and lifecycle apply risk. |
| Performance baseline | `PerformanceBaselineReport`, `PerformanceBudget` | Defines source-release smoke budgets for EventBus, Scheduler, Adapter probe, memory context, backup, and release check. |
| Migration and compatibility | `MigrationGuide`, `MigrationStep`, `CompatibilityPolicy` | Documents V1.4 -> V1.5 migration steps, compatibility guarantees, public CLI contracts, and deprecation policy. |
| Backup archive readiness | `ReleaseGate` | Records V1.10.3 signed backup archive, optional sealing, remote target contract, and pre-restore evidence as release readiness evidence. |
| Restore apply gate readiness | `ReleaseGate` | Records V1.10.4 confirmation, policy approval, filesystem lock, health gate, rollback-required plan, and `mutation_executed:false` staged boundary evidence. |
| Supervisor handoff readiness | `ReleaseGate` | Records V1.10.5 blue-green handoff, release pointer mutation, persisted state, and rollback-on-health-failure evidence. |
| Release artifact evidence | `ReleaseArtifactEvidence`, `ReleaseArtifactVerificationReport` | Parses a V1.11.1 key/value evidence manifest, verifies a SHA-256 keyed signature, source commit provenance, SBOM marker, and scan status, then exposes a blocking release gate when evidence is supplied. |
| Distribution evidence | `ReleaseDistributionEvidence`, `ReleaseDistributionVerificationReport` | Parses a V1.11.2 key/value evidence manifest for Windows/Linux/macOS install smoke, install/upgrade/uninstall docs, and package-manager dry-run status, then exposes a blocking release gate when evidence is supplied. |
| Security scan evidence | `ReleaseSecurityScanEvidence`, `ReleaseSecurityScanVerificationReport` | Parses a V1.11.3 external scanner evidence manifest and blocks readiness when the scanner did not pass or any high/critical finding is present. |
| Benchmark evidence | `ReleaseBenchmarkEvidence`, `ReleaseBenchmarkVerificationReport` | Parses V1.11.3 production benchmark measurements, converts them into the existing performance report shape, and blocks readiness when observed latency exceeds budget. |
| MCP compatibility readiness | `ReleaseGate` | Records V1.13.7 stdio/HTTP transport, tool schema, stream lifecycle, dangling-session, and explicit server-surface fixture evidence as `REL-MCP-COMPAT-001`. |

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
findings map to policy exit code `3`; over-budget performance gates map to
runtime-unavailable exit code `4`.

## Boundary Rules

`eva-release` does:

- collect release gates into stable Rust structs;
- make the cross-platform CI matrix visible to the CLI;
- keep security findings explicit and auditable;
- define performance budgets as release-smoke contracts;
- document the migration and compatibility policy that V1.5 promises;
- expose signed backup archive, pre-restore evidence, restore apply gate, and supervisor handoff evidence as readiness gates;
- expose signed artifact, distribution install smoke, and package-manager dry-run evidence as opt-in readiness gates;
- expose external scanner and measured benchmark evidence as opt-in readiness gates;
- expose the repo-local MCP compatibility matrix as a required readiness gate;
- preserve known future risks as warnings instead of silently enabling apply paths.

`eva-release` does not:

- run package signing services or artifact publishing;
- execute destructive restore;
- replace OS service-manager supervision;
- run external security scanners directly;
- execute benchmark commands directly;
- certify external MCP servers or provide HTTPS/TLS transport;
- replace `eva-backup`, `eva-lifecycle`, `eva-policy`, or CI.

## Verification

```powershell
cargo test -p eva-release
cargo test -p eva-cli
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
boundaries, all performance budgets are within threshold, and the V1.4 -> V1.5
migration has no breaking changes. V1.10.3 adds a required signed backup
archive gate. V1.10.4 adds a required restore apply gate for confirmation,
policy approval, lock, health, rollback-required output, and staged mutation
evidence while destructive file mutation remains disabled. V1.10.5 adds a
required supervisor handoff gate for controlled local release pointer mutation
and persisted handoff state while production service-manager integration remains
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
or production streaming.
