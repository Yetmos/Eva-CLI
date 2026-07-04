# Eva-CLI Documentation

The documentation currently separates the published default language from the
content authority:

- The current documentation set tracks the V1.5 source-release checkpoint:
  executable CLI diagnostics, release-hardening gates, compatibility policy,
  migration notes, and the remaining target apply paths.
- `docs/en/` is the default public documentation entry. Documents are grouped
  into topic directories such as `guide/`, `architecture/`, `capabilities/`,
  `operations/`, `release/`, `planning/`, and `tooling/`.
- `docs/zh-CN/` is the current source of truth for detailed architecture and
  implementation-spec content. It uses the same topic directory layout as the
  English entry. Most English pages are summaries until the full-detail English
  migration is completed.

## Language Entrances

- [English](en/README.md) - default public entry.
- [简体中文](zh-CN/README.md) - current detailed architecture source.

## Maintenance Rules

- For architecture-detail or implementation-spec changes, update the
  corresponding `docs/zh-CN/` document first while `zh-CN` remains the content
  authority.
- Then sync the English page as either a faithful full-detail translation or a
  clearly scoped summary. Do not let an English summary override the Chinese
  detailed specification.
- Register every document ID, source path, translation path, and translation
  status in [_i18n/manifest.json](../docs/_i18n/manifest.json).
- Keep `contentAuthority.locale` in the manifest aligned with the locale that
  currently contains the most complete implementation-spec detail.
- Register content assets that contain readable text in the manifest `assets`
  section, with locale-specific image paths and translation status.
- Keep published document IDs stable; when moving a document, update
  `docs/_i18n/manifest.json` and regenerate the website.
- Keep historical root-level Chinese documents under `docs/zh-CN/legacy/` while
  the categorized `docs/zh-CN/*/` tree remains the current source.
- Do not translate code, commands, JSON keys, Topic names, Lua bindings, file
  paths, or error codes.

## Translation Status

Translation status values:

- `current`: synchronized with the English source.
- `needs-review`: translated, but needs human review.
- `stale`: English source changed after the translation.
- `missing`: the locale has no page for that document.
- `partial`: only a summary, navigation, or key sections are translated.

## Coverage Labels

Locale `coverage` values describe content completeness, not URL ownership:

- `default-summary`: default public entry, stable slugs, summary-level detail.
- `detailed-source`: current source of truth for detailed architecture specs.

## Locale Rules

Locale codes use BCP 47 exactly as written in the manifest, HTML `lang`,
`hreflang`, and directory names. Examples: `en`, `zh-CN`, `pt-BR`, `ar`.
