# Eva-CLI Documentation

> Language: English
> Canonical: docs/en/README.md
> Translation: [简体中文](../zh-CN/README.md)

Eva-CLI is currently in the architecture-design stage. The repository documents
the target runtime, extension model, memory model, discovery model, hardware
integration, configuration system, and process-level recovery strategy before
the executable implementation is finalized.

## Recommended Reading Order

1. [Architecture Overview](architecture-overview.md)
2. [Rust, Lua, and EventBus Scheduler](rust-lua-eventbus-scheduler.md)
3. [Lua External Agent Adapter](lua-external-agent-adapter.md)
4. [Lua Skill, MCP, and Tool Hot Reload](lua-skill-mcp-tool-hot-reload.md)
5. [Skill Implementation Plan](skill-implementation.md)
6. [Agent Memory and Knowledge Base](agent-memory-knowledge-base.md)
7. [Agent Discovery](agent-discovery.md)
8. [Hardware Hotplug](hardware-hotplug.md)
9. [Project Configuration](project-configuration.md)
10. [Process-Level Upgrade](process-level-upgrade.md)
11. [Backup, Migration Package, and Release Snapshot](backup-migration-release-snapshot.md)
12. [Design Risk Review](design-risk-review.md)
13. [Zero to 1.0 Roadmap](zero-to-one-roadmap.md)
14. [Website and Documentation i18n](website-docs-i18n.md)

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

English documents under `docs/en/` are the canonical source. Localized documents
must be registered in `docs/_i18n/manifest.json` and must preserve the
architecture decisions, constraints, and API semantics of the English source.
