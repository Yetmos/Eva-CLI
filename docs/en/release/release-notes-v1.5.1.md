# Eva-CLI V1.5.1 Release Notes

Status: stable release
Tag: `v1.5.1`

Eva-CLI V1.5.1 is a patch release for the V1.5 release-hardening line. It
keeps the V1.5 CLI and JSON contracts stable while enabling the GitHub Packages
delivery channel through GHCR.

## Highlights

- Publishes the release container image to `ghcr.io/yetmos/eva-cli` from the
  GitHub Release workflow.
- Records GHCR package digest, tags, source tag, source SHA, and package URL in
  `release-evidence/package-ghcr.json`.
- Keeps stable releases eligible for the `latest` container tag while
  prerelease tags remain excluded from `latest`.
- Preserves the existing `v1.5.0` source release as immutable history instead of
  republishing it only to backfill package metadata.
- Hardens the GitHub Pages artifact path used by the release documentation.

## Compatibility

V1.5.1 does not introduce breaking CLI, config, JSON, or documentation path
changes for the V1.5 line.

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.5.1` artifact.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.5.1`.

Signed installers, OS package-manager packages, and provenance bundles remain
future release scope.
