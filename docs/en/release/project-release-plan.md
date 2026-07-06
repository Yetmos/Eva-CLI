# Eva-CLI Project Release Plan

Date: 2026-07-06
Scope: project release process, CI/DI gates, and Windows/macOS/Linux support

This document defines the project-level release plan for Eva-CLI. It sits above
version-specific notes such as the V1.5 GitHub Release Plan and describes the
repeatable process for preparing, validating, publishing, and repairing a
release.

In this document, CI/DI means:

- CI: continuous integration for pull requests and branch pushes.
- DI: delivery/deployment integration, including GitHub Release publication,
  release evidence capture, and documentation site deployment.

## Release Objectives

The release process must provide:

- repeatable source release publication through Git tags and GitHub Releases;
- cross-platform validation on Windows, macOS, and Linux before publication;
- release evidence that can be reviewed after the release;
- documentation and website validation before public delivery;
- a repair path that avoids rewriting public history after a release is visible.

The current V1.5 release remains a source release. Packaged installers, signed
binary artifacts, provenance bundles, and package-manager publishing are future
release scope.

## Release Channels

| Channel | Owner | Trigger | Output |
| --- | --- | --- | --- |
| CI | `.github/workflows/ci.yml` | Pull request, push to `main`/`master`, manual dispatch | Rust, CLI smoke, website, and i18n validation logs |
| GitHub Release DI | `.github/workflows/release.yml` | Push tag `vMAJOR.MINOR.PATCH`, manual dispatch against an existing tag | GitHub Release, source archives, `release-evidence-*` artifact |
| Website DI | `.github/workflows/pages.yml` | Push changes under website, docs, assets, scripts, or the pages workflow | GitHub Pages deployment |

## Platform Matrix

Every release must keep the supported desktop platform matrix green.

| Platform | GitHub runner | Required shell assumptions | Required checks |
| --- | --- | --- | --- |
| Windows | `windows-latest` | PowerShell Core, Windows paths, no POSIX-only commands in release smoke | `cargo fmt`, `cargo clippy`, `cargo test`, CLI smoke, release gates |
| macOS | `macos-latest` | PowerShell Core for repo scripts, POSIX filesystem behavior for Cargo | `cargo fmt`, `cargo clippy`, `cargo test`, CLI smoke, release gates |
| Linux | `ubuntu-latest` | PowerShell Core for repo scripts, default CI host for website/page build | `cargo fmt`, `cargo clippy`, `cargo test`, CLI smoke, release gates, website build |

The release plan treats a platform-specific failure as release blocking unless
the failing check is explicitly documented as non-goal or experimental.

## Branch And Version Contract

- Release commits land on `main` unless the repository default branch changes.
- The root `Cargo.toml` package version and `[workspace.package].version` must
  match the release tag without the leading `v`.
- Release tags use `vMAJOR.MINOR.PATCH`, for example `v1.5.0`.
- Annotated tags are preferred for public releases.
- Once a GitHub Release has been published, repair through a patch release such
  as `v1.5.1` instead of rewriting the public tag.

## CI Gate

The CI gate runs on pull requests, branch pushes, and manual dispatch. It must
prove that the source tree is buildable and that the public command surface has
not regressed.

Required CI checks:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/build-site-i18n.ps1
./scripts/validate-i18n.ps1
```

The CI workflow also runs CLI smoke commands for version, doctor, config
validation, runtime inspection, basic task execution, adapter/MCP/Skill
diagnostics, discovery, memory context, hardware diagnostics, backup/snapshot,
restore planning, upgrade checks, and release gates.

## DI Gate

The release DI gate runs from the immutable release tag. It must not publish a
GitHub Release until all platform verification jobs pass.

Required release DI checks:

```powershell
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
```

The publish job captures each command output into `release-evidence/` and
uploads it as `release-evidence-${RELEASE_TAG}`. This artifact is the durable
machine-readable release evidence for the tag.

## Documentation And Website Gate

Documentation changes are release-relevant because the project currently ships
source releases with documented command contracts. Before release publication:

- update release notes, migration guidance, compatibility policy, and any
  version-specific release plan;
- register new documentation in `docs/_i18n/manifest.json`;
- keep Chinese and English document paths aligned with the existing docs tree;
- run the localized website build and i18n validation scripts;
- let the Pages workflow deploy only after the docs/site build succeeds.

## Release Procedure

1. Prepare the release commit on `main`.
2. Run the local preflight checks listed in the CI and DI gate sections.
3. Push the release commit and verify CI is green on Windows, macOS, and Linux.
4. Create an annotated release tag and push it to GitHub.
5. Verify that the GitHub Release workflow completes and uploads release evidence.
6. Verify that the GitHub Release body, source archives, and documentation links match the release tag.

## Binary Artifact Roadmap

Binary packaging is not part of the current V1.5 source release, but future
release automation should use this initial target map:

| Platform | Initial target | Future output |
| --- | --- | --- |
| Windows | `x86_64-pc-windows-msvc` | `.zip` archive and installer after signing is available |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` archive or universal bundle after signing/notarization is available |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` archive or universal bundle after signing/notarization is available |
| Linux | `x86_64-unknown-linux-gnu` | `.tar.gz` archive, package-manager integration later |

Do not add unsigned installers to the public release path without updating the
security review, compatibility policy, and rollback procedure.

## Failure And Repair Policy

If CI fails before tagging, fix the release branch and rerun CI. Do not create a
release tag from a commit that has not passed the platform matrix.

If the release workflow fails before the GitHub Release is visible, repair the
commit, delete the local tag, delete the remote tag only if no public release
exists, then recreate the tag from the repaired commit.

If the GitHub Release is already public, keep the published tag immutable and
ship a patch release.

## Completion Evidence

A release is complete only when all of the following evidence exists:

- CI is green for Windows, macOS, and Linux on the release commit.
- The release workflow is green for Windows, macOS, and Linux on the release tag.
- The GitHub Release exists for the tag.
- GitHub source archives are available.
- `release-evidence-${RELEASE_TAG}` is uploaded.
- Documentation and website validation completed successfully.
