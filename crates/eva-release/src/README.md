# eva-release/src

Updated: 2026-07-09

This directory contains the V1.5 release-hardening model and later additive
runtime-readiness gates. The implementation is
pure Rust data assembly with validation on inputs that can be invalid, such as
release targets or migration version strings. It deliberately avoids filesystem
mutation and external command execution so that `release` CLI checks are safe to
run in local development and CI.

## File Responsibilities

| File | Responsibility |
| --- | --- |
| `lib.rs` | Re-exports the public release-hardening API and declares the module responsibility. |
| `artifact.rs` | Defines V1.11.1 signed release artifact evidence, SHA-256 keyed signature verification, provenance/source commit checks, and key/value manifest parsing. |
| `distribution.rs` | Defines V1.11.2 distribution evidence, Windows/Linux/macOS install smoke verification, package-manager dry-run verification, and key/value manifest parsing. |
| `scanner.rs` | Defines V1.11.3 external security scanner evidence, finding severity normalization, high/critical blocking verification, and key/value manifest parsing. |
| `benchmark.rs` | Defines V1.11.3 production benchmark evidence, measured budget verification, and conversion into the stable `PerformanceBaselineReport` shape. |
| `checklist.rs` | Defines `ReleaseHardeningService`, readiness reports, release gates, platform readiness, and stability scenarios, including durable recovery/diagnostics, Lua runtime, V1.10.3 signed backup archive, V1.10.4 restore apply gate, V1.14.2 staged file mutation, V1.14.3 rollback apply, V1.14.4 operator confirmation, V1.10.5 supervisor handoff readiness, V1.11 release evidence gates, V1.12.6 daemon runtime readiness, V1.13.7 MCP compatibility readiness, and V1.13.8 provider supervision readiness. |
| `security.rs` | Defines security severity and findings for policy, sandbox, secret, MCP, hardware, and lifecycle boundaries. |
| `performance.rs` | Defines source-release performance budgets and the baseline report. |
| `migration.rs` | Defines migration steps and the V1.5 compatibility policy. |

## Data Flow

`ReleaseHardeningService::v15()` is the single construction entry point. It can
produce four independent reports:

- `readiness(target)`: aggregate release gate report for `all`, `windows`,
  `linux`, or `macos`;
- `readiness_with_artifact_evidence(target, evidence)`: aggregate release gate
  report plus a required V1.11.1 signed artifact/provenance gate;
- `readiness_with_distribution_evidence(target, evidence)`: aggregate release
  gate report plus a required V1.11.2 install smoke/package dry-run gate;
- `readiness_with_security_scan_evidence(target, evidence)`: aggregate release
  gate report plus a required V1.11.3 external scanner gate;
- `readiness_with_benchmark_evidence(target, evidence)`: aggregate release gate
  report plus a required V1.11.3 measured benchmark gate;
- `readiness_with_release_evidence(target, artifact, distribution, scan, benchmark)`: aggregate
  release gate report with all optional evidence gates;
- `security_review()`: policy/sandbox/secret/MCP/hardware/lifecycle security
  review;
- `performance_baseline()`: fixed V1.5 performance budget table;
- `migration_guide(from, to)`: migration steps and compatibility policy.

`eva-cli` converts these reports into text or JSON. The crate itself stays free
of CLI formatting so future release tooling can reuse the same data contracts.

## Design Notes

- Warning findings are intentionally preserved. Production service-manager
  handoff remains tracked as a future risk instead of being hidden behind the
  current local state-store handoff evidence.
- The signed backup archive gate proves archive signature and pre-restore
  evidence checks exist, but it does not enable destructive restore.
- The restore apply gate proves confirmation, policy approval, filesystem lock,
  health check, staged file mutation, rollback apply, and operator confirmation
  evidence exist, but it does not replace a production service-manager handoff.
- The supervisor handoff gate proves `upgrade apply --state-store` can commit a
  controlled local generation handoff and release pointer mutation, but it does
  not replace a production OS service manager.
- The daemon runtime readiness gate proves the local foreground/filesystem
  daemon boundary, mailbox control plane, durable task lifecycle, scheduler
  retry tick, and daemon-backed agent drain/reload mutation evidence are present,
  but it does not replace a production OS service manager or provider
  supervision.
- The MCP compatibility gate proves the repo-local stdio/HTTP transport, tool
  schema, stream lifecycle, dangling-session, and explicit server-surface
  matrix is present, but it does not certify real external MCP servers,
  HTTPS/TLS, or production streaming.
- The provider supervision gate proves the current controlled provider baseline
  covers supervisor slots, process-table evidence, credential scope, admission
  limits, stream artifacts, daemon recovery, and MCP compatibility readiness,
  but it does not replace OS process supervision, OS credential vaults, or user
  isolation.
- The release artifact evidence gate is opt-in until CI generates the key/value
  evidence manifest. When supplied, unsigned artifacts, signature mismatch, or
  provenance/source commit mismatch block readiness.
- The distribution evidence gate is opt-in and verifies the CI-generated
  key/value manifest. When supplied, missing Windows/Linux/macOS install smoke,
  missing install/upgrade/uninstall docs, or failed package-manager dry-run
  blocks readiness.
- The external security scan evidence gate is opt-in and verifies a CI-generated
  scanner manifest. When supplied, skipped/failed scans or high/critical
  findings block readiness.
- The benchmark evidence gate is opt-in and verifies measured command latency.
  When supplied, empty evidence, skipped/failed benchmark status, or observed
  latency over budget blocks readiness. The same evidence can feed
  `release perf --benchmark-evidence` without changing the JSON shape.
- Performance budgets are release-smoke thresholds, not statistically rigorous
  benchmarks. They exist to make regressions visible and to document the
  expected cost class of the current in-memory implementation.
- Cross-platform readiness records the CI target and shell/path assumptions; it
  does not claim packaged installer coverage.
- Compatibility policy is additive in V1.5: the new `release` command group does
  not remove or rename any V1.0-V1.4 command.

## Tests

The module-level tests cover:

- readiness aggregation has no blocked required gates;
- target filtering for platform checks;
- security review includes all required boundaries;
- all performance budgets are within threshold;
- V1.4 -> V1.5 migration remains compatible and includes the new release
  command surface;
- signed backup archive, restore apply/rollback/operator confirmation gate,
  supervisor handoff readiness,
  daemon runtime readiness, MCP compatibility readiness, and provider
  supervision readiness are present without creating a blocked release state.
- missing MCP compatibility matrix blocks the required gate.
- provider supervision readiness records the current boundaries without
  claiming OS process supervision.
- signed artifact evidence passes when the keyed signature and provenance match,
  and blocks when the artifact is unsigned.
- distribution evidence passes when three-platform install smoke and package
  dry-run evidence are passed, and blocks when the package dry-run fails.
- security scan evidence passes when the scanner status is passed and no
  high/critical finding exists, and blocks when a high severity finding appears.
- benchmark evidence passes when measured samples are within budget, blocks on
  regression, and can feed the stable performance report.
