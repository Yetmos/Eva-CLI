# Eva-CLI Version Management Plan

Date: 2026-07-06
Scope: project version numbers, release status, and GitHub repository version management

This document defines how Eva-CLI names versions, increments version fields, and manages GitHub branches, tags, releases, milestones, issues, and pull requests. It complements the [project release plan](project-release-plan.md), which defines release gates and evidence.

## Version Format

The human-facing project version uses this format:

```text
V<major>.<minor>.<patch>-<status>
```

Example:

```text
V1.0.15-alpha
```

| Field | Example | Meaning | Increment rule |
| --- | --- | --- | --- |
| `major` | `1` | Major version | Increment after large refactors, architecture boundary changes, core runtime updates, incompatible public contract changes, or a new stability era across multiple modules. |
| `minor` | `0` | Minor version | Increment after smaller feature additions, non-core feature removals, feature-set adjustments, or command-surface expansion that keeps the core architecture compatible. |
| `patch` | `15` | Patch version | Increment for existing feature iteration, fixes, user experience improvements, documentation updates, diagnostics, compatibility patches, or internal optimization. |
| `status` | `alpha` | Version status | One of `alpha`, `beta`, or `release`. |

When a higher-order field increments, lower-order fields reset to zero. For example, `V1.4.9-release` becomes `V1.5.0-alpha` for the next minor line, and `V1.9.12-release` becomes `V2.0.0-alpha` for the next major line.

## Status Rules

| Status | Meaning | GitHub Release setting | Usage |
| --- | --- | --- | --- |
| `alpha` | Internal validation. Features may be incomplete, and commands, config, docs, or implementation boundaries may still change. | Mark as prerelease, not latest. | Design validation, developer trials, CI/DI validation. |
| `beta` | External validation. Features are mostly frozen; only blockers, compatibility issues, documentation gaps, and release-gate fixes should land. | Mark as prerelease, not latest. | Candidate validation, migration rehearsals, platform matrix checks. |
| `release` | Stable public release. Must pass release gates and provide auditable evidence. | Do not mark as prerelease; may be latest. | Stable user-facing version. |

Alpha builds may still add or remove functionality. Beta builds should not introduce new features. Published release builds are repaired through later patch releases rather than by changing the original tag.

## Git, Cargo, and Display Mapping

Different systems need different version forms:

| Context | Format | Example |
| --- | --- | --- |
| Docs, roadmap, milestone, release title | `V<major>.<minor>.<patch>-<status>` | `V1.0.15-alpha` |
| Git tag, alpha/beta | `v<major>.<minor>.<patch>-<status>` or `v<major>.<minor>.<patch>-<status>.<n>` | `v1.0.15-alpha`, `v1.0.15-beta.1` |
| Git tag, stable release | `v<major>.<minor>.<patch>` | `v1.0.15` |
| Cargo package version, alpha/beta | `<major>.<minor>.<patch>-<status>` or `<major>.<minor>.<patch>-<status>.<n>` | `1.0.15-alpha` |
| Cargo package version, stable release | `<major>.<minor>.<patch>` | `1.0.15` |

Human-facing text may say `V1.0.15-release` to make the status explicit. Git tags and stable Cargo versions omit `-release` so the final version remains a normal stable SemVer version. Git tags use a lowercase `v` prefix, matching the existing release plan.

## Increment Decisions

Each PR that affects a release line must state its version impact.

Use a major version bump when the change refactors core architecture, changes runtime boundaries, changes module dependencies, changes persistence formats, removes or incompatibly changes stable CLI/config/JSON contracts, replaces key runtime models, or requires user migration steps that compatibility layers cannot hide.

Use a minor version bump when the change adds a complete feature surface, removes an experimental alpha/beta capability without breaking stable contracts, changes feature composition while preserving compatibility, or completes a visible roadmap stage.

Use a patch version bump when the change iterates existing features, fixes bugs, improves diagnostics/logging/docs, repairs platform compatibility, improves CI/DI gates, or optimizes internals without changing public commands, config, schemas, or user workflows.

## GitHub Repository Management

| Branch | Purpose | Rule |
| --- | --- | --- |
| `main` | Default integration branch and release source. | Must stay green; release tags are created from `main` or a release commit already merged to `main`. |
| `feature/<topic>` | Feature development. | Merge by PR; PR description states version impact. |
| `fix/<topic>` | Bug fix. | Merge by PR; defaults to patch impact unless public contracts change. |
| `docs/<topic>` | Documentation update. | Merge by PR; release docs must update manifest and indexes. |
| `release/v<major>.<minor>` | Optional release preparation branch. | Use only when a minor line needs a freeze; accept only gate fixes, docs fixes, and version corrections. |
| `hotfix/v<major>.<minor>.<patch>` | Optional emergency repair branch for a published version. | Branch from the release tag, merge the fix back to `main`, and publish a new patch tag. |

Daily development should stay lightweight with `main` plus PRs. Create `release/*` or `hotfix/*` only when beta/release freeze or published-version maintenance requires it.

## Tag and GitHub Release Rules

Alpha/beta tags use forms such as `v1.0.15-alpha` or `v1.0.15-beta.1`. Stable release tags use `v1.0.15`.

Published tags must be annotated, must not be force-pushed, and must not be moved to a different commit. The tag commit must contain matching `Cargo.toml`, `Cargo.lock`, release notes, and release evidence.

GitHub Releases must bind to immutable tags:

- `alpha` and `beta`: mark prerelease and do not mark latest.
- `release`: do not mark prerelease; only the newest stable release should be latest.
- Release titles use the human-facing version, such as `Eva-CLI V1.0.15-alpha` or `Eva-CLI V1.0.15-release`.
- Release bodies include change summary, compatibility notes, migration notes, verification evidence, known issues, and documentation links.

## Milestones, Issues, and PRs

GitHub milestones use human-facing names such as `V1.0.15-alpha`, `V1.0.15-beta`, and `V1.0.15-release`.

Use labels for version impact: `version:major`, `version:minor`, `version:patch`.

Use labels for release state: `status:alpha`, `status:beta`, `status:release-blocker`.

PR descriptions must state the version impact, whether CLI/config/JSON/docs/CI/website are affected, which verification commands ran, and whether migration or release notes are required. PRs marked `version:major` or `status:release-blocker` must include the needed release notes, migration guide, or compatibility policy updates before merge.

## Release Flow

1. Choose the target version, for example `V1.0.15-alpha`.
2. Confirm release blockers in the milestone are closed or moved.
3. Update versions, release notes, migration notes, compatibility notes, and docs.
4. Run CI gates, release gates, docs build, and i18n validation.
5. Merge the release commit to `main` and confirm the platform matrix is green.
6. Create an annotated tag.

```powershell
git tag -a v1.0.15-alpha -m "Eva-CLI V1.0.15-alpha"
git push origin v1.0.15-alpha
```

Stable release:

```powershell
git tag -a v1.0.15 -m "Eva-CLI V1.0.15-release"
git push origin v1.0.15
```

7. Wait for the GitHub Release workflow to finish.
8. Check prerelease/latest settings, release body, source archives, and release evidence.
9. Close the milestone and create the next milestone.

## Repair and Rollback

If a problem is found before tag creation, fix the release commit and tag afterward. If an alpha or beta has already been published, prefer a new `alpha.N`, `beta.N`, or patch version instead of rewriting the tag. If a stable release is public, never move the original tag; publish a new patch release such as `v1.0.16`.

Any rollback must record the reason, impact, and replacement version. Do not rely only on GitHub UI state.
