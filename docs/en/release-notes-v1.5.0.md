# Eva-CLI V1.5.0 Release Notes

Date: 2026-07-04

Eva-CLI V1.5.0 is the release-hardening checkpoint for the implemented 1.x
source surface. It adds executable release readiness checks while preserving
the V1.0-V1.4 command contracts.

## Added

- New `eva-release` crate with release readiness, security review, performance
  baseline, migration guide, and compatibility policy contracts.
- New CLI commands:
  - `eva release check`
  - `eva release security`
  - `eva release perf`
  - `eva release migration`
- CI smoke coverage for the new release command group on Windows, Linux, and
  macOS.
- V1.5 release-hardening docs, migration guide, compatibility policy, and
  release notes.

## Stabilized

- V1.0-V1.4 commands remain available and keep their JSON envelope shape.
- `restore plan`, `upgrade check`, and `hardware bind` remain plan-first and
  non-destructive.
- Security findings are explicit, including tracked future risks for real
  restore and process handoff.

## Explicit Non-Goals

- No packaged installers or signed release artifacts.
- No real Supervisor process management.
- No destructive restore apply path.
- No real MCP server startup or hardware raw I/O.
- No production-grade benchmark harness.

## Verification

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -- --version
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
./scripts/build-site-i18n.ps1
./scripts/validate-i18n.ps1
```
