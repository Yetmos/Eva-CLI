# Eva-CLI GitHub Packages Publishing

Date: 2026-07-14
Scope: the GHCR container path implemented by `.github/workflows/release.yml`

Eva-CLI publishes a container package at `ghcr.io/yetmos/eva-cli`. This is one
release output, not the source of version truth and not a replacement for the
GitHub Release record.

![Eva-CLI package and release evidence path](../../../assets/release-workflow-evidence-path.svg)

## Current Published Package

The latest successful public package corresponds to `v1.11.4-alpha`:

```text
image=ghcr.io/yetmos/eva-cli
tag=1.11.4-alpha
digest=sha256:9902e18131088408936d16e554078cd8f1c70b457a16b8c5ecc7dbb518ff8ad3
platforms=linux/amd64,linux/arm64
```

The later `v1.11.5-alpha` release workflow failed during Ubuntu verification, so
its package job was skipped. Current `main` still reports `1.11.5-alpha`, but it is
not represented by a successful package publication.

## Actual Publication Sequence

The `packages` job starts only after the Ubuntu, Windows, and macOS `verify` matrix
passes. It then:

1. checks out the selected immutable source tag and runs
   `scripts/validate-version-management.ps1` with that tag;
2. treats package support as enabled only when both `Dockerfile` and
   `.dockerignore` exist;
3. builds a local smoke image for the runner architecture and runs
   `eva --version` in it;
4. pushes a Buildx image for `linux/amd64` and `linux/arm64` with provenance and
   SBOM generation enabled;
5. runs `docker buildx imagetools inspect` against the pushed digest;
6. writes `release-evidence/package-ghcr.json` and uploads a package evidence
   Actions artifact;
7. lets the final `publish` job merge the package result into provenance and
   distribution evidence.

The pushed-digest inspect is a registry manifest metadata check. It is not a pull,
install, or runtime smoke test. The workflow does not run the pushed arm64 image.

## Tag And Digest Rules

All image tags are derived from the selected Git tag:

| Release kind | Git tag | GHCR tags |
| --- | --- | --- |
| Alpha/beta | `v1.12.0-beta.1` | `1.12.0-beta.1`, `sha-<7-char-sha>` |
| Stable | `v1.12.0` | `1.12.0`, `1.12`, `latest`, `sha-<7-char-sha>` |

Prereleases do not update `latest` or the `MAJOR.MINOR` tag. Stable releases do.
However, the workflow does not enforce registry tag immutability: manual dispatch
against an existing tag can push the same labels again. Use the digest when content
identity matters.

## Evidence Semantics

For a published image, `package-ghcr.json` records the source tag/SHA, package URL,
image tags, platforms, digest, metadata-inspection command, and the Buildx settings
used for provenance and SBOM generation.

The evidence stores suggested provenance and SBOM inspection commands, but the
workflow does not execute those two formatted commands. It executes only the base
digest inspection. Therefore:

- `package_manager_dry_run.status=passed` means the pushed digest was inspectable;
- it does not prove that a consumer pulled or ran either platform;
- Buildx provenance and SBOM are not an OCI image signature;
- there is no production signing key or signature verification in this path.

The final release gate consumes the package status through
`release-distribution.evidence`. The package is already in GHCR at that point.

## Permissions And Trust Boundary

The job uses the repository `GITHUB_TOKEN` with the permissions declared in the
workflow:

```yaml
permissions:
  contents: read
  id-token: write
  packages: write
  attestations: write
```

No personal access token is required for the current same-repository package path.
The image build uses Debian bookworm stages, runs the final image as the non-root
`eva` user, sets `/workspace` as its working directory, and copies only the `eva`
binary into the runtime image. Project config or durable state must be supplied by
the caller when commands need it.

## Consumer Verification

Prefer the digest recorded in the successful GitHub Release or package evidence:

```powershell
$Image = "ghcr.io/yetmos/eva-cli@sha256:9902e18131088408936d16e554078cd8f1c70b457a16b8c5ecc7dbb518ff8ad3"
docker pull $Image
docker run --rm $Image --version
docker buildx imagetools inspect $Image
```

These are consumer-side checks; the release workflow currently performs only the
local pre-push version smoke and post-push digest metadata inspection.

## Failure Boundary

The package job runs in parallel with native archive jobs and before the final
publish job. Consequently:

- a successful GHCR push can remain even if a native archive or final evidence gate
  later fails;
- a failed final release must record and review any already-published digest;
- retrying the old tag cannot include code fixes made after that tag;
- repairing a release should use a new prerelease serial or patch tag instead of
  moving the original tag.

## Scope Limits

This workflow does not publish signed container images, native installers,
Homebrew/Winget/Apt packages, or Rust crates. GitHub Packages does not replace
crates.io for public Cargo crate distribution.

## Related Documents

- [Project release plan](project-release-plan.md)
- [Version management](version-management-plan.md)
- [Install, upgrade, and uninstall](install-upgrade-uninstall.md)
