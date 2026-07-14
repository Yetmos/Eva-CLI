# Eva-CLI Version Management Plan

Date: 2026-07-14
Scope: Cargo versions, CLI labels, Git tags, GitHub Releases, and GHCR tags

This document separates version facts enforced by repository code from operator
policy. `scripts/validate-version-management.ps1` is a repository consistency
check; it is not a GitHub governance or publication verifier.

![Eva-CLI release history and version boundary](../../../assets/release-history-boundary.svg)

## Current Version State

| Surface | Current state |
| --- | --- |
| `main` Cargo/CLI line | `1.11.5-alpha` / `V1.11.5-alpha` |
| Existing tag | `v1.11.5-alpha`, pointing to commit `9b86adf` |
| Release for that tag | None. Its Ubuntu verification failed, so publication jobs were skipped. |
| Latest successful GitHub Release | `v1.11.4-alpha` |

Current `main` contains changes made after the `v1.11.5-alpha` tag while retaining
the same Cargo version. The next publication must first move to a new SemVer value;
the existing tag must not be repointed to current `main`.

## Version Forms

| Context | Form | Example |
| --- | --- | --- |
| Cargo prerelease | `MAJOR.MINOR.PATCH-alpha[.N]` or `-beta[.N]` | `1.12.0-beta.1` |
| Cargo stable | `MAJOR.MINOR.PATCH` | `1.12.0` |
| Human prerelease label | `V` plus Cargo version | `V1.12.0-beta.1` |
| Human stable label | `VMAJOR.MINOR.PATCH-release` | `V1.12.0-release` |
| Git prerelease tag | `v` plus Cargo version | `v1.12.0-beta.1` |
| Git stable tag | `vMAJOR.MINOR.PATCH` | `v1.12.0` |
| GHCR prerelease tags | Exact version and `sha-<7-char-sha>` | `1.12.0-beta.1`, `sha-abc1234` |
| GHCR stable tags | Exact version, `MAJOR.MINOR`, `latest`, and SHA | `1.12.0`, `1.12`, `latest` |

The stable Cargo and Git forms omit `-release`; adding that suffix would turn the
version into a SemVer prerelease. Only `alpha` and `beta` prerelease identifiers are
accepted by the current validator.

## Source Of Truth

Before publication, the root `Cargo.toml` package and workspace versions drive the
expected CLI label and tag. For a published version, the immutable Git tag and its
commit SHA are the source identity.

Other surfaces are derived records:

- the CLI embeds `CARGO_PKG_VERSION` and also carries an explicit release status and
  human label checked by the validator;
- README and documentation labels must match the Cargo-derived human version;
- a GitHub Release is a mutable record attached to the tag;
- a GHCR tag is a registry reference that a workflow rerun can repush;
- a GHCR digest identifies the image content;
- workflow evidence is generated after the tag and binds itself back to
  `source_tag` and `source_sha`. It is not stored in the tag commit.

## What Automated Validation Enforces

Run the check without a tag in CI:

```powershell
./scripts/validate-version-management.ps1
```

The release workflow supplies the selected tag:

```powershell
./scripts/validate-version-management.ps1 -Tag $env:RELEASE_TAG
```

The script currently verifies:

- the two root Cargo version declarations match;
- the version is stable SemVer or an `alpha`/`beta` prerelease supported by the
  repository regex;
- a supplied tag exactly equals `v` plus the Cargo version;
- the README and CLI files named in the script contain the expected human label;
- the CLI release status constant matches the derived status;
- the i18n manifest registers the version and package publishing documents at the
  required paths;
- CI, release workflow, Dockerfile, `.dockerignore`, and documentation contain the
  expected static wiring strings for version validation and GHCR.

These are file-content assertions. The script does not verify that a tag is
annotated, is new, descends from `main`, or exists on GitHub. It does not inspect
branch protection, `Cargo.lock`, release notes, workflow results, GitHub Release
settings, registry digests, milestones, labels, or pull-request metadata. Static
workflow strings also do not prove that a job executed successfully.

## Tag Release And Package Rules

- Repository policy requires an annotated tag created from the reviewed release
  commit. Automation currently validates only the tag text and Cargo version.
- Once a tag has been pushed, do not force-push, move, delete-and-recreate, or reuse
  it. Publish a new prerelease serial or patch instead.
- The release workflow marks `alpha` and `beta` tags as GitHub prereleases. A stable
  tag is not marked prerelease.
- Manual dispatch checks out the selected existing tag. It cannot publish fixes that
  exist only on `main`.
- The workflow can update an existing GitHub Release body and can repush GHCR tags.
  Consumers that require immutable package content must pin the digest.
- Stable GHCR releases may update `MAJOR.MINOR` and `latest`; prereleases must not.
- `ghcr.io/yetmos/eva-cli` is a container distribution channel, not a substitute for
  crates.io and not the version source of truth.

## Repository Governance Boundary

As of this document date, GitHub exposes only the `main` branch, it is not protected,
and the repository has no milestones or `version:*` / `status:*` labels. The
repository also contains no pull-request template that enforces a version-impact
declaration.

Therefore the following remain operator policy rather than automated guarantees:

- review and green CI before tagging;
- annotated tags originating from the intended release commit;
- release-note, migration, and compatibility review;
- choosing major, minor, or patch impact;
- preventing direct pushes or requiring pull-request approvals.

A major bump is appropriate for incompatible public contracts or persistence
formats. A minor bump is appropriate for a new compatible feature surface. A patch
or prerelease serial is appropriate for compatible fixes, diagnostics, and release
process corrections. These decision rules are not inferred by the validator.

## Versioned Release Procedure

1. Choose a version that has no existing remote tag.
2. Update the root Cargo package/workspace versions and regenerate `Cargo.lock`.
3. Update the CLI status/label, README labels, release notes, and translated docs.
4. Run tests, docs/i18n validation, and
   `scripts/validate-version-management.ps1` without a tag.
5. Push the commit and confirm the complete CI matrix is green.
6. Create and push an annotated tag, then let the tag workflow validate it with
   `-Tag $env:RELEASE_TAG`.
7. Verify the workflow result, GitHub Release record, GHCR digest, and Actions
   artifacts independently.

For example:

```powershell
git tag -a v1.12.0-alpha -m "Eva-CLI V1.12.0-alpha"
git push origin v1.12.0-alpha
```

## Repair And Rollback

- Before tag push: repair the release commit and rerun all checks.
- After tag push: leave the tag immutable and create a new version. This applies even
  when the release workflow failed before creating a GitHub Release.
- If GHCR push succeeded before a later job failed, record and review the orphaned
  digest; a new tag does not automatically remove it.
- A workflow rerun against the old tag is useful only for transient infrastructure
  failures. It cannot incorporate a code fix committed after the tag.
- Record rollback reason, affected tag/digest, and replacement version outside the
  mutable GitHub Release body as well as in release notes or an issue.

## Related Documents

- [Project release plan](project-release-plan.md)
- [GitHub Packages publishing](github-packages-publishing.md)
- [Install, upgrade, and uninstall](install-upgrade-uninstall.md)
