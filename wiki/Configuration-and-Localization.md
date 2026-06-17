# Configuration and Localization

Eva-CLI configuration is data, not executable code. Documentation and website
content use English as the canonical source and keep localized content in
locale-specific paths.

Canonical sources:

- [docs/en/project-configuration.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/project-configuration.md)
- [docs/en/website-docs-i18n.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/website-docs-i18n.md)
- [website/README.md](https://github.com/Yetmos/Eva-CLI/blob/main/website/README.md)

## Configuration Scope

Project configuration should cover:

- project-level `eva.yaml`
- Agent manifests
- Adapter manifests
- Capability manifests
- MCP policy
- hardware policy
- sandbox policy
- runtime and observability settings

## Validation Rules

Configuration must be validated with structured parsers and JSON Schema-like
contracts before it affects runtime registries or execution.

Runtime must reject ambiguous or invalid config before activation rather than
letting partial state leak into execution.

## Hot Reload Boundaries

Safe hot reload can update:

- labels
- routes
- subscription rules
- selected policy limits
- Lua source references

Changes that require a runtime generation switch or restart include:

- permission expansion
- transport changes
- state backend changes
- MCP command changes
- sandbox changes

## Merge Rules

Configuration merge order must be deterministic. Project config, user config,
environment overrides, and built-in defaults should record provenance so audit
and debugging can explain why a runtime value was selected.

## Documentation i18n

Current documentation policy:

- English is canonical.
- `/` and `/docs/` are English by default.
- Localized website pages live at `/<locale>/`.
- Localized documentation lives under `/docs/<locale>/`.
- Locale codes use BCP 47 consistently.
- Missing translations must fall back visibly and must not be advertised as
  localized `hreflang` pages.

Current locale set:

| Locale | Status |
| --- | --- |
| `en` | canonical |
| `zh-CN` | current translation set |

## Website Build

The website has no runtime dependency. Build-time scripts generate static HTML
from templates and locale JSON:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-site-i18n.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\validate-i18n.ps1
```

GitHub Pages publishes `website/`, `docs/`, and `assets/` into a static site.
