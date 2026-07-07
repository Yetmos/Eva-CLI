# eva-release/src

Updated: 2026-07-04

This directory contains the V1.5 release-hardening model. The implementation is
pure Rust data assembly with validation on inputs that can be invalid, such as
release targets or migration version strings. It deliberately avoids filesystem
mutation and external command execution so that `release` CLI checks are safe to
run in local development and CI.

## File Responsibilities

| File | Responsibility |
| --- | --- |
| `lib.rs` | Re-exports the public release-hardening API and declares the module responsibility. |
| `checklist.rs` | Defines `ReleaseHardeningService`, readiness reports, release gates, platform readiness, and stability scenarios, including the V1.6.4 durable recovery gate. |
| `security.rs` | Defines security severity and findings for policy, sandbox, secret, MCP, hardware, and lifecycle boundaries. |
| `performance.rs` | Defines source-release performance budgets and the baseline report. |
| `migration.rs` | Defines migration steps and the V1.5 compatibility policy. |

## Data Flow

`ReleaseHardeningService::v15()` is the single construction entry point. It can
produce four independent reports:

- `readiness(target)`: aggregate release gate report for `all`, `windows`,
  `linux`, or `macos`;
- `security_review()`: policy/sandbox/secret/MCP/hardware/lifecycle security
  review;
- `performance_baseline()`: fixed V1.5 performance budget table;
- `migration_guide(from, to)`: migration steps and compatibility policy.

`eva-cli` converts these reports into text or JSON. The crate itself stays free
of CLI formatting so future release tooling can reuse the same data contracts.

## Design Notes

- Warning findings are intentionally preserved. For example, destructive
  restore and real process handoff are tracked as future risks instead of being
  hidden because the current CLI keeps them plan-first.
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
  command surface.
