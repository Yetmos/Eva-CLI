# Eva-CLI V1.0.0 Release Notes

Date: 2026-07-03

Eva-CLI V1.0.0 is the first core release checkpoint. It makes the current executable CLI, configuration validation, in-memory basic runtime loop, task diagnostics, documentation, and CI gates coherent enough for new users to install from source and run the supported quickstart.

## Added

- `eva --version` and `eva version --output json` report the V1.0 core release surface.
- `RuntimeBuilder::in_memory_v10()` and `RuntimeMode::InMemoryV10` identify the V1.0 basic runtime as `in_memory_v1.0` with generation `basic-v1.0`.
- GitHub Actions CI runs formatting, clippy, workspace tests, V1.0 quickstart smoke commands, and website/i18n validation.
- V1.0 quickstart, known limitations, and release notes are documented in `docs/en/` and `docs/zh-CN/`.

## Stabilized

- `doctor`, `config validate`, `inspect`, `run --example basic`, and `task status/logs/cancel` remain the V1.0 core CLI contract.
- JSON success/error envelopes, trace fields, structured errors, and exit-code mapping remain the public diagnostic shape.
- `examples/basic/` remains the supported end-to-end example and regression baseline.

## Explicit Non-Goals

- No external Adapter/MCP/Skill/Hardware execution in V1.0.
- No real Lua VM, durable task database, daemon, supervisor, backup/restore, or release snapshot implementation in V1.0.
- No packaged installers or signed release artifacts in this checkpoint.

## Verification

The V1.0 release gate is:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -- --version
cargo run -- version --output json
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- inspect runtime --output json
cargo run -- run --example basic --task-id req-release-v10 --output json
cargo run -- task status --task req-release-v10 --output json
cargo run -- task logs --task req-release-v10 --output json
cargo run -- run --example basic --task-id req-release-timeout --timeout-ms 0 --replay-dead-letters --output json
cargo run -- run --example basic --task-id req-release-cancel --cancel --output json
./scripts/build-site-i18n.ps1
./scripts/validate-i18n.ps1
```
