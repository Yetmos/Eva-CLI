# Eva-CLI V1.5.1 Release Notes

Language: English | [简体中文](../../zh-CN/release/V1.5.1发布说明.md)

> Historical snapshot. This page is pinned to the immutable `v1.5.1` tag.

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-06 |
| Status | Stable patch release |
| Tag | [`v1.5.1`](https://github.com/Yetmos/Eva-CLI/tree/v1.5.1) |
| Commit | [`968c343`](https://github.com/Yetmos/Eva-CLI/commit/968c3430a9ff) |
| GitHub Release | [Published](https://github.com/Yetmos/Eva-CLI/releases/tag/v1.5.1) |
| Release workflow | [Run 28772939702](https://github.com/Yetmos/Eva-CLI/actions/runs/28772939702), successful |

## Included In The Tag

- Enabled GHCR publication from the release workflow without moving the
  existing `v1.5.0` tag.
- Recorded package digest, tags, source tag, source SHA, and package URL in
  package evidence.
- Allowed stable releases to update `latest`; prerelease tags remained
  excluded from `latest`.
- Preserved the V1.5 CLI, configuration, JSON, and documentation paths.

## Not Included

This patch did not add new runtime commands, native binary archives, signed
installers, OS package-manager packages, or provenance bundles.

## Reproduce The Release

```powershell
git switch --detach v1.5.1
./scripts/validate-version-management.ps1 -Tag v1.5.1
./scripts/validate-i18n.ps1
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -- release check --output json
docker build -t eva-cli:1.5.1-smoke .
docker run --rm eva-cli:1.5.1-smoke --version
```

## Artifacts

- The GitHub Release provides generated source archives and has **0** uploaded
  Release assets.
- Actions retained `release-evidence-v1.5.1` and
  `package-evidence-v1.5.1`; they are retention-limited workflow artifacts.
- GHCR contains `1.5.1`, `1.5`, and `latest` tags for
  `ghcr.io/yetmos/eva-cli`.
- This workflow did not produce native binary archive artifacts.

## Current Documentation

For current behavior, use the [user manual](../guide/user-manual.md), the
[current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and the [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
