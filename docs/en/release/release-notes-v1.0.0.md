# Eva-CLI V1.0.0 Source Checkpoint

Language: English | [简体中文](../../zh-CN/release/V1.0发布说明.md)

> Historical snapshot. This page describes commit `437087c`, not the current `main` branch.

![Release history boundary](../../../assets/release-history-boundary.svg)

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-03 |
| Status | Source checkpoint; not a tagged release |
| Tag | None |
| Commit | [`437087c`](https://github.com/Yetmos/Eva-CLI/commit/437087ca032a9607c897a1d3f30eccbeeb237e85) |
| GitHub Release | None |
| CI | [Run 28658770611](https://github.com/Yetmos/Eva-CLI/actions/runs/28658770611), successful |

## Included At This Commit

- `eva --version` and `eva version --output json` exposed the V1.0 version surface.
- `RuntimeBuilder::in_memory_v10()` identified the in-memory runtime as
  `in_memory_v1.0` with generation `basic-v1.0`.
- `doctor`, `config validate`, `inspect`, `run --example basic`, and task
  status/log/cancel commands formed the supported CLI boundary.
- JSON success and error envelopes, trace fields, structured errors, and exit
  codes were covered by the basic example and CI.

## Not Included

This checkpoint did not provide external Adapter/MCP/Skill/Hardware execution,
a real Lua VM, durable task storage, a daemon, supervisor process management,
backup/restore execution, signed artifacts, installers, or a GitHub Release.

## Reproduce The Checkpoint

```powershell
git switch --detach 437087ca032a9607c897a1d3f30eccbeeb237e85
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -- --version
cargo run -- run --example basic --task-id req-release-v10 --output json
cargo run -- task status --task req-release-v10 --output json
./scripts/build-site-i18n.ps1
./scripts/validate-i18n.ps1
```

## Artifacts

There is no V1.0 tag, GitHub Release, Release asset, Actions release artifact,
or GHCR image. The immutable evidence is the commit and its successful CI run.

## Current Documentation

For current behavior, use the [user manual](../guide/user-manual.md), the
[current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and the [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
