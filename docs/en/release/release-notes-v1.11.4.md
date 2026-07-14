# Eva-CLI V1.11.4 Alpha Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.11.4-alpha发布说明.md)

> Historical snapshot pinned to the immutable `v1.11.4-alpha` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-09 |
| Status | Alpha prerelease |
| Tag | [`v1.11.4-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.11.4-alpha) |
| Commit | [`e33e480`](https://github.com/Yetmos/Eva-CLI/commit/e33e4802f1b8) |
| GitHub Release | [Published as prerelease](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.11.4-alpha) |
| Release workflow | [Run 28988017594](https://github.com/Yetmos/Eva-CLI/actions/runs/28988017594), successful |

## Included In The Tag

- Moved release, version, config, adapter, MCP, discovery, memory,
  observability, task, skill, hardware, backup, snapshot, restore, upgrade,
  inspect, and doctor command handling into focused `run/*.rs` modules.
- Kept shared JSON envelopes, trace fields, exit-code mapping, and output
  formatting centralized.
- Preserved the existing compiled release evidence declarations and added
  marker `release_distribution_cli_split_v1.11.4`.

## Not Included

The split was intended to preserve behavior. It did not add `emit`, `agent`, or
`capability` commands; those arrived in the next tag. It also did not provide
destructive restore mutation, production service-manager handoff, real hardware
I/O, provider supervision, or signing credentials.

## Reproduce The Release

```powershell
git switch --detach v1.11.4-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-cli
./scripts/validate-version-management.ps1 -Tag v1.11.4-alpha
cargo run -- --version
cargo run -- release check --output json
```

## Artifacts

- The GitHub Release has generated source archives and **0** uploaded assets.
- Actions retained `release-evidence-v1.11.4-alpha`,
  `package-evidence-v1.11.4-alpha`, and unsigned Linux/Windows/macOS native
  archives with evidence; they are retention-limited workflow artifacts.
- GHCR contains `ghcr.io/yetmos/eva-cli:1.11.4-alpha`.

The release gates were compiled evidence checks, not execution of external
scanners, production handoff, or signing.

## Current Documentation

See the [user manual](../guide/user-manual.md), [current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
