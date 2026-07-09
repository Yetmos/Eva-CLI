# Eva-CLI V1.11.4 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.11.4-alpha`

Eva-CLI V1.11.4-alpha completes the CLI command module split slice. It keeps the
public command contracts stable while moving release, version, config, adapter,
MCP, discovery, memory, observability, task, skill, hardware, backup, snapshot,
restore, upgrade, inspect, and doctor command handling into focused modules.

## Highlights

- Splits major command parsers, executors, and formatters out of `run.rs` into
  `crates/eva-cli/src/run/*.rs` command modules.
- Keeps the shared JSON envelope, trace fields, exit-code mapping, and command
  output shape centralized and unchanged.
- Preserves release evidence gates for artifact provenance, distribution smoke,
  external scanner input, benchmark evidence, restore apply gates, and supervisor
  handoff gates.
- Updates the current release metadata to `1.11.4-alpha` and records the module
  split as a release checkpoint before adding new `emit`, `agent`, and
  `capability` commands.

## Compatibility

V1.11.4-alpha is intended to be behavior-preserving for existing public CLI
commands. It does not add destructive restore mutation, production service
manager handoff, real hardware I/O, or signed installer credentials.

## Verification

- `cargo fmt --check`
- `cargo test -p eva-cli`
- `cargo run -- --version`
- `cargo run -- release check --output json`
- `scripts/validate-version-management.ps1 -Tag v1.11.4-alpha`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.11.4-alpha` artifact when the release workflow
  completes.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.11.4-alpha` when package
  publication succeeds.

`emit`, `agent`, and `capability` command completion remains the next V1.11.5
slice.
