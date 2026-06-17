# Website and Documentation i18n

> Language: English
> Canonical: docs/en/website-docs-i18n.md
> Translation: [简体中文](../zh-CN/网站与文档多语言适配方案.md)

Updated: 2026-06-16

## Purpose

This document defines the multilingual structure for the Eva-CLI website and
documentation.

## Decisions

- English is the canonical content source.
- `/` and `/docs/` are English by default.
- Localized website pages live at `/<locale>/`.
- Localized documentation lives under `/docs/<locale>/`.
- Locale codes use BCP 47 consistently across directory names, `lang`,
  `hreflang`, and manifest entries.
- Website pages are generated at build time from templates and locale JSON.
- Published output remains static HTML, Markdown, and assets.
- Missing translations fall back visibly; they must not be advertised as
  localized `hreflang` pages.

## Current Rollout

The first implemented rollout includes English and Simplified Chinese website
pages, English canonical documentation, Simplified Chinese documentation copies,
localized architecture diagrams, `docs/_i18n/manifest.json`,
`docs/_i18n/glossary.json`, build scripts, and validation scripts.

The build pipeline supports adding more locales by adding manifest entries,
locale JSON, translated documentation, and localized content assets without
duplicating HTML templates.

## Localized Assets

Content images that contain readable text are tracked in the manifest `assets`
section. The English source image remains the default asset, and each localized
variant is mapped by locale code:

```json
{
  "id": "architecture-diagram",
  "source": "assets/eva-cli-architecture.svg",
  "translations": {
    "zh-CN": "assets/eva-cli-architecture.zh-CN.svg"
  }
}
```

Website templates resolve these asset mappings at build time. Documentation
pages should reference the asset for their own language directly. Brand assets
such as the Eva-CLI logo are language-neutral and do not need locale variants.
