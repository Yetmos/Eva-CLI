# Website and Documentation i18n

> Language: English
> Published default: docs/en/operations/website-docs-i18n.md
> Translation: [Simplified Chinese](../../zh-CN/operations/网站与文档多语言适配方案.md)

Updated: 2026-07-13

## Current Model

- English is the default public locale and owns stable public slugs.
- Simplified Chinese is the current detail authority for architecture and
  implementation specifications.
- `/` and `/docs/` publish English by default. Localized website pages use
  `/<locale>/`, and localized documents use `/docs/<locale>/`.
- Website HTML is generated at build time. Published output remains static
  HTML, Markdown, and assets.
- Locale codes use BCP 47 consistently in directory names, manifest entries,
  HTML `lang`, and `hreflang` values.

## Repository Layout

```text
docs/
  README.md
  _i18n/
    manifest.json
    glossary.json
  assets/
  en/{guide,architecture,capabilities,operations,release,planning,tooling}/
  zh-CN/{guide,architecture,capabilities,operations,release,planning,tooling}/
website/
  _templates/
  _i18n/
  _blog/
  index.html
  zh-CN/index.html
  docs/index.html
```

`docs/_i18n/manifest.json` is the machine-readable registry for locales,
document IDs, source and translation paths, translation status, and localized
content assets. Document IDs remain stable when files move.

## Document Lifecycle

When adding or changing a document:

1. Update the detailed Chinese source when the change affects architecture or
   implementation semantics.
2. Update the English page as a full translation or an explicitly scoped
   summary.
3. Register both paths and the honest translation status in the manifest.
4. Update the locale documentation indexes and website document cards when the
   page is a public entry.
5. Build and validate the generated site.

When removing or merging a document, remove both locale paths, its manifest and
website entries, all incoming links, and assets used only by that document.
Superseded roadmap and milestone history belongs in Git history, immutable
release tags, and release notes rather than the live documentation tree.

## Translation Status

The supported values are `current`, `needs-review`, `stale`, `missing`, and
`partial`. A non-missing status requires an existing translation path. Missing
translations must not be emitted as localized `hreflang` pages.

Do not translate commands, JSON keys, Topic names, Lua bindings, file paths,
error codes, or other machine contracts. Shared terminology belongs in
`docs/_i18n/glossary.json`.

## Localized Assets and Blog Content

Images with readable text are registered in the manifest `assets` section.
The English image is the default source, and each localized variant is mapped
by locale. Language-neutral brand assets do not need variants.

Blog metadata lives in `website/_blog/posts.json`; localized HTML sources live
under `website/_blog/content/<locale>/`. The build and validation scripts check
post IDs, locale codes, categories, slugs, content paths, canonical URLs, and
`hreflang` links.

## Build and Publish

Run from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-site-i18n.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\validate-i18n.ps1
```

The build script regenerates localized home pages, the documentation index,
and blog pages from templates and locale data. GitHub Pages then publishes
`website/`, `docs/`, and shared assets. Do not hand-edit generated HTML when a
template, locale JSON file, manifest entry, or blog source owns the content.
