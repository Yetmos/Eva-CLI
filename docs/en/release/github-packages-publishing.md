# Eva-CLI GitHub Packages Publishing

Date: 2026-07-06
Scope: GitHub Packages delivery through GitHub Container Registry

Eva-CLI publishes GitHub Packages as container images in GitHub Container
Registry (GHCR). This channel complements GitHub Releases; it does not replace
the release tag, the GitHub Release record, or the release evidence artifact.

The package name is:

```text
ghcr.io/yetmos/eva-cli
```

The container entrypoint is the `eva` CLI binary.

## Channel Contract

The GitHub Packages channel is implemented in `.github/workflows/release.yml`.
It runs after the Windows, macOS, and Linux release verification matrix and
before the GitHub Release body is created or updated.

The package job:

- checks out the immutable release tag;
- validates version management for that tag;
- builds a local container image and runs `eva --version` as a package smoke
  test;
- publishes a multi-platform image to GHCR for `linux/amd64` and `linux/arm64`;
- records package name, registry URL, source tag, source SHA, digest, tags, and
  platforms in `release-evidence/package-ghcr.json`;
- uploads `package-evidence-${RELEASE_TAG}` and lets the publish job merge that
  file into `release-evidence-${RELEASE_TAG}`.

## Tag Rules

All package tags are derived from the same release tag that drives the GitHub
Release workflow.

| Release type | Git tag example | GHCR tags |
| --- | --- | --- |
| alpha | `v1.5.1-alpha` | `1.5.1-alpha`, `sha-<short>` |
| beta | `v1.5.1-beta.1` | `1.5.1-beta.1`, `sha-<short>` |
| stable | `v1.5.1` | `1.5.1`, `1.5`, `latest`, `sha-<short>` |

Only stable releases may update `latest`. Alpha and beta releases must not
publish or move `latest`.

## Permissions

The package job uses the repository `GITHUB_TOKEN` with these job permissions:

```yaml
permissions:
  contents: read
  packages: write
```

A personal access token is not part of the default path. Use a least-privilege
PAT only when a future package flow needs cross-repository private package
access that `GITHUB_TOKEN` cannot provide.

## Pull Verification

After a successful release workflow, verify the package with:

```powershell
docker pull ghcr.io/yetmos/eva-cli:1.5.1
docker run --rm ghcr.io/yetmos/eva-cli:1.5.1 --version
```

For a stable release, also verify:

```powershell
docker pull ghcr.io/yetmos/eva-cli:latest
```

The digest returned by Docker or the GHCR package page must match
`release-evidence/package-ghcr.json`.

## Scope Limits

GitHub Packages is not a Cargo crate registry replacement. Public Rust crate
publication remains a separate crates.io decision.

This GHCR channel does not add signed installers, OS package-manager packages,
or provenance bundles. Those remain future release scope.

The existing `v1.5.0` release tag predates this package channel and remains
immutable. It should not be moved or republished only to backfill GHCR. Package
publication applies to later release tags that contain this workflow and
Dockerfile support.
