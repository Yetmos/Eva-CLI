# Eva-CLI Documentation

> Language: English
> Published default: docs/en/README.md
> Current detail authority: [简体中文](../zh-CN/README.md)

Eva-CLI has reached the V1.5 source-release checkpoint. The repository now
contains a compileable Rust workspace, executable CLI surfaces, configuration
validation, an in-memory basic runtime loop, local task diagnostics, controlled
Adapter/MCP/Skill/Discovery surfaces, request-scoped memory and knowledge
context assembly, hardware binding plans, backup/lifecycle diagnostics, and
release readiness, security, performance, migration, and compatibility checks.

Important source rule: English documents currently provide the default public
entry, stable slugs, and summary coverage. The Simplified Chinese documents
under `docs/zh-CN/` remain the source of truth for detailed architecture and
implementation-spec content until the English full-detail migration catches up.

## Recommended Reading Order

1. [Architecture Overview](architecture-overview.md)
2. [Module Partitioning Plan](module-partitioning.md)
3. [eva-core Module Design](eva-core-module.md)
4. [Rust, Lua, and EventBus Scheduler](rust-lua-eventbus-scheduler.md)
5. [Lua External Agent Adapter](lua-external-agent-adapter.md)
6. [Lua Skill, MCP, and Tool Hot Reload](lua-skill-mcp-tool-hot-reload.md)
7. [Skill Implementation Plan](skill-implementation.md)
8. [Agent Memory and Knowledge Base](agent-memory-knowledge-base.md)
9. [Agent Discovery](agent-discovery.md)
10. [Hardware Hotplug](hardware-hotplug.md)
11. [Project Configuration](project-configuration.md)
12. [Topic Routing Hybrid Sync](topic-routing-hybrid-sync.md)
13. [IDEA Plugin Toolchain Requirements](idea-plugin-toolchain.md)
14. [Process-Level Upgrade](process-level-upgrade.md)
15. [Backup, Migration Package, and Release Snapshot](backup-migration-release-snapshot.md)
16. [Design Risk Review](design-risk-review.md)
17. [Zero to 1.0 Roadmap](zero-to-one-roadmap.md)
18. [Full Implementation Plan](full-implementation-plan.md)
19. [Eva-CLI V1.0 Quickstart](v1.0-quickstart.md)
20. [Eva-CLI V1.0 Known Limitations](v1.0-known-limitations.md)
21. [Eva-CLI V1.0.0 Release Notes](release-notes-v1.0.0.md)
22. [Eva-CLI V1.5 Release Hardening](v1.5-release-hardening.md)
23. [Eva-CLI V1.5 Migration Guide](v1.5-migration-guide.md)
24. [Eva-CLI V1.5 Compatibility Policy](v1.5-compatibility-policy.md)
25. [Eva-CLI V1.5.0 Release Notes](release-notes-v1.5.0.md)
26. [Eva-CLI V1.5 GitHub Release Plan](v1.5-github-release-plan.md)
27. [V0.1 Baseline Acceptance](v0.1-baseline-acceptance.md)
28. [V0.2 Contract and Configuration Acceptance](v0.2-contract-config-acceptance.md)
29. [Command-Line Tool Feature Design](command-line-tool-feature-design.md)
30. [Website and Documentation i18n](website-docs-i18n.md)

## Core Boundaries

- Rust owns runtime boundaries, permissions, schemas, sandboxing, secrets,
  process lifecycle, audit, timeout handling, and recovery.
- Lua owns hot-reloadable business logic, local Agent state transitions, tool
  orchestration, and result mapping.
- Topic EventBus coordinates Agents, but does not store hidden global business
  state.
- AdapterRegistry exposes external capabilities through controlled manifests,
  schemas, policies, transports, and audit hooks.
- Discovery normalizes possible capabilities, but does not authorize execution.
- Memory and knowledge are managed by Runtime services; Lua only uses controlled
  APIs such as `ctx.memory`, `ctx.global_memory`, and `ctx.knowledge`.

## Source and Translation Policy

`docs/en/` owns the default public entry and stable English URLs. For detailed
architecture decisions, constraints, API semantics, and implementation specs,
use the mapped `docs/zh-CN/` document as the current authority unless
`docs/_i18n/manifest.json` explicitly marks a full-detail English page as caught
up.

When changing architecture-detail content, update the Chinese detailed source
first, then sync the English page and manifest metadata honestly as summary,
partial, or full-detail coverage.
