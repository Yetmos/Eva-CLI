# Eva-CLI Documentation

> Language: English
> Published default: docs/en/README.md
> Current detail authority: [简体中文](../zh-CN/README.md)

Eva-CLI is currently in the architecture-design stage. The repository documents
the target runtime, extension model, memory model, discovery model, hardware
integration, configuration system, and process-level recovery strategy before
the executable implementation is finalized.

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
12. [IDEA Plugin Toolchain Requirements](idea-plugin-toolchain.md)
13. [Process-Level Upgrade](process-level-upgrade.md)
14. [Backup, Migration Package, and Release Snapshot](backup-migration-release-snapshot.md)
15. [Design Risk Review](design-risk-review.md)
16. [Zero to 1.0 Roadmap](zero-to-one-roadmap.md)
17. [Command-Line Tool Feature Design](command-line-tool-feature-design.md)
18. [Website and Documentation i18n](website-docs-i18n.md)

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
