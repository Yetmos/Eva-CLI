# eva-release / Release Hardening

Updated: 2026-07-08

`eva-release` owns the V1.5 release-hardening boundary and later additive
runtime-readiness gates. It turns the final
1.x readiness work into executable Rust contracts instead of leaving it as a
manual checklist. The crate does not build installers, sign artifacts, run
external scanners, or start runtime processes. It aggregates the readiness
evidence that Eva-CLI can prove in source form today and exposes that evidence
to `eva-cli release ...`.

## Implemented Scope

| Area | Public Type | Behavior |
| --- | --- | --- |
| Release checklist | `ReleaseHardeningService`, `ReleaseReadinessReport`, `ReleaseGate` | Aggregates cross-platform, stability, docs, security, performance, migration, durable runtime, Lua runtime, signed backup archive, restore apply gate, and supervisor handoff readiness. |
| Cross-platform readiness | `PlatformReadiness` | Records Windows/Linux/macOS CI expectations, shell model, path assumptions, and smoke commands. |
| Stability readiness | `StabilityScenario` | Captures task diagnostics, cancellation, dead-letter replay, restore planning, and upgrade planning scenarios. |
| Security review | `SecurityReviewReport`, `SecurityFinding`, `SecuritySeverity` | Covers policy, Lua sandbox, secret redaction, MCP allowlist, hardware handle boundaries, and lifecycle apply risk. |
| Performance baseline | `PerformanceBaselineReport`, `PerformanceBudget` | Defines source-release smoke budgets for EventBus, Scheduler, Adapter probe, memory context, backup, and release check. |
| Migration and compatibility | `MigrationGuide`, `MigrationStep`, `CompatibilityPolicy` | Documents V1.4 -> V1.5 migration steps, compatibility guarantees, public CLI contracts, and deprecation policy. |
| Backup archive readiness | `ReleaseGate` | Records V1.10.3 signed backup archive, optional sealing, remote target contract, and pre-restore evidence as release readiness evidence. |
| Restore apply gate readiness | `ReleaseGate` | Records V1.10.4 confirmation, policy approval, filesystem lock, health gate, rollback-required plan, and `mutation_executed:false` staged boundary evidence. |
| Supervisor handoff readiness | `ReleaseGate` | Records V1.10.5 blue-green handoff, release pointer mutation, persisted state, and rollback-on-health-failure evidence. |

## CLI Surface

`eva-cli` exposes the crate through these commands:

```powershell
cargo run -- release check --output json
cargo run -- release check --target windows --output json
cargo run -- release security --output json
cargo run -- release perf --output json
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
- preserve known future risks as warnings instead of silently enabling apply paths.

`eva-release` does not:

- run package signing or artifact publishing;
- execute destructive restore;
- replace OS service-manager supervision;
- run external security scanners;
- benchmark real production latency;
- replace `eva-backup`, `eva-lifecycle`, `eva-policy`, or CI.

## Verification

```powershell
cargo test -p eva-release
cargo test -p eva-cli
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
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
future work.
