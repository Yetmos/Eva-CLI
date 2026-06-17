# Contributor Guide

This guide explains how to contribute without moving Eva-CLI away from its
current architecture contract.

## Before Changing Architecture

Read these first:

1. [docs/en/architecture-overview.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/architecture-overview.md)
2. [docs/en/design-risk-review.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/design-risk-review.md)
3. The specific domain document for the area being changed.

Architecture decisions should be changed in `docs/en/` first. Update localized
documents and this wiki afterward.

## Documentation Rules

- Treat English files in `docs/en/` as canonical.
- Register new documents in `docs/_i18n/manifest.json`.
- Keep published English slugs stable.
- Do not translate code identifiers, commands, JSON keys, Topic names, Lua
  bindings, file paths, or error codes.
- If an image contains readable text, register it in the manifest `assets`
  section and provide localized variants where needed.

## Website Rules

The website is static and generated at build time.

Run these checks after website or i18n changes:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-site-i18n.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\validate-i18n.ps1
```

The GitHub Pages workflow publishes `website/`, `docs/`, and `assets/`.

## Runtime Design Rules

- Rust owns authority, policy, schemas, secrets, sandboxing, audit, lifecycle,
  and recovery.
- Lua owns hot-reloadable business behavior inside a narrow host API.
- EventBus is for coordination, not hidden durable business state.
- Discovery creates candidates, not execution permission.
- Adapters must be manifest-driven, schema-validated, policy-checked, and
  audited.
- Hot reload must use generation validation, activation, draining, and rollback.

## Testing Expectations

Early implementation work should include focused tests for:

- Topic routing and Scheduler delivery
- manifest and policy rejection
- adapter input/output schema validation
- Lua host API boundaries
- hot reload activation and rollback
- memory provenance and access filtering
- structured error classes and retryability

## Wiki Maintenance

This `wiki/` directory is a source copy of the GitHub Wiki pages. GitHub Wiki
uses a separate git repository, so changes here still need to be copied or
pushed to the wiki remote when that remote is initialized.

The wiki should stay short and navigational. Detailed design belongs in
`docs/en/`.
