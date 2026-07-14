# Eva-CLI V1.5.0 Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.5发布说明.md)

> Historical snapshot. This page is pinned to the immutable `v1.5.0` tag.

![Release history boundary](../../../assets/release-history-boundary.svg)

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-04 |
| Status | Stable release |
| Tag | [`v1.5.0`](https://github.com/Yetmos/Eva-CLI/tree/v1.5.0) |
| Commit | [`74d85e7`](https://github.com/Yetmos/Eva-CLI/commit/74d85e7da58ac40ef5d30b38e2844dee503a44c0) |
| GitHub Release | [Published](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.5.0) |
| Release workflow | [Run 28698982685](https://github.com/Yetmos/Eva-CLI/actions/runs/28698982685), successful |

## Included In The Tag

- Added the `eva-release` crate and `eva release check`, `security`, `perf`,
  and `migration` commands.
- Kept the V1.0-V1.4 command names, JSON envelope, and exit-code contracts.
- Kept restore, upgrade, and hardware operations plan-first and non-destructive.
- Added Windows, Linux, and macOS smoke coverage for the release command group.

`release check` evaluated declarations and built-in evidence for the tag. A
`ready` result did not execute CI, external scanners, or production rollout.

## Not Included

This tag did not include packaged installers, signed artifacts, production
benchmarks, real Supervisor process management, destructive restore apply, a
real MCP server transport, or hardware raw I/O.

## Reproduce The Release

```powershell
git switch --detach v1.5.0
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -- --version
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
./scripts/validate-i18n.ps1
```

## Artifacts

- The GitHub Release provides GitHub-generated source archives and has **0**
  uploaded Release assets.
- The successful workflow retained `release-evidence-v1.5.0` as an Actions
  artifact; Actions artifacts are retention-limited and are not Release assets.
- No `1.5.0` GHCR image was published.

## Current Documentation

For current behavior, use the [user manual](../guide/user-manual.md), the
[current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and the [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
