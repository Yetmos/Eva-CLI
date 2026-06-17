# Eva-CLI Documentation

The documentation now uses English as the canonical source and keeps localized
content under locale-specific directories.

## Language Entrances

- [English](en/README.md) - canonical documentation source.
- [简体中文](zh-CN/README.md) - current Chinese translation set.

## Maintenance Rules

- Create or update the English canonical document first.
- Register every document ID, source path, translation path, and translation
  status in [_i18n/manifest.json](_i18n/manifest.json).
- Keep published document IDs and English slugs stable.
- Keep old Chinese root-level paths readable during the migration window.
- Do not translate code, commands, JSON keys, Topic names, Lua bindings, file
  paths, or error codes.

## Translation Status

Translation status values:

- `current`: synchronized with the English source.
- `needs-review`: translated, but needs human review.
- `stale`: English source changed after the translation.
- `missing`: the locale has no page for that document.
- `partial`: only a summary, navigation, or key sections are translated.

## Locale Rules

Locale codes use BCP 47 exactly as written in the manifest, HTML `lang`,
`hreflang`, and directory names. Examples: `en`, `zh-CN`, `pt-BR`, `ar`.
