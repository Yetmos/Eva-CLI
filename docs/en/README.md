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

## Document Categories

### Guide

Start here when you want to run the project.

- [Eva-CLI User Manual](guide/user-manual.md)
- [Eva-CLI V1.0 Quickstart](guide/v1.0-quickstart.md)

### Architecture

Read these pages to understand the runtime model and module boundaries.

- [Architecture Overview](architecture/architecture-overview.md)
- [Module Partitioning Plan](architecture/module-partitioning.md)
- [eva-core Module Design](architecture/eva-core-module.md)
- [Rust, Lua, and EventBus Scheduler](architecture/rust-lua-eventbus-scheduler.md)
- [Topic Routing Hybrid Sync](architecture/topic-routing-hybrid-sync.md)

### Capabilities

These documents cover external capability access, memory, discovery, skills, and
hardware boundaries.

- [Lua External Agent Adapter](capabilities/lua-external-agent-adapter.md)
- [Lua Skill, MCP, and Tool Hot Reload](capabilities/lua-skill-mcp-tool-hot-reload.md)
- [Skill Implementation Plan](capabilities/skill-implementation.md)
- [Agent Memory and Knowledge Base](capabilities/agent-memory-knowledge-base.md)
- [Agent Discovery](capabilities/agent-discovery.md)
- [Hardware Hotplug](capabilities/hardware-hotplug.md)

### Operations

Operational documents cover configuration, lifecycle, backup/restore planning,
and documentation publishing.

- [Project Configuration](operations/project-configuration.md)
- [Process-Level Upgrade](operations/process-level-upgrade.md)
- [Backup, Migration Package, and Release Snapshot](operations/backup-migration-release-snapshot.md)
- [Website and Documentation i18n](operations/website-docs-i18n.md)

### Release

Release documents track shipped checkpoints, known limits, hardening gates, and
migration policy.

- [Eva-CLI Project Release Plan](release/project-release-plan.md)
- [Eva-CLI Version Management Plan](release/version-management-plan.md)
- [Eva-CLI V1.0 Known Limitations](release/v1.0-known-limitations.md)
- [Eva-CLI V1.0.0 Release Notes](release/release-notes-v1.0.0.md)
- [Eva-CLI V1.5 Release Hardening](release/v1.5-release-hardening.md)
- [Eva-CLI V1.5 Migration Guide](release/v1.5-migration-guide.md)
- [Eva-CLI V1.5 Compatibility Policy](release/v1.5-compatibility-policy.md)
- [Eva-CLI V1.5.0 Release Notes](release/release-notes-v1.5.0.md)
- [Eva-CLI V1.5 GitHub Release Plan](release/v1.5-github-release-plan.md)
- [Eva-CLI V1.5 Release Acceptance](release/v1.5-release-acceptance.md)

### Planning

Planning and acceptance documents preserve roadmap decisions and milestone
evidence.

- [Design Risk Review](planning/design-risk-review.md)
- [Zero to 1.0 Roadmap](planning/zero-to-one-roadmap.md)
- [Full Implementation Plan](planning/full-implementation-plan.md)
- [V0.1 Baseline Acceptance](planning/v0.1-baseline-acceptance.md)
- [V0.2 Contract and Configuration Acceptance](planning/v0.2-contract-config-acceptance.md)

### Tooling

Tooling documents describe CLI and IDE surfaces.

- [Command-Line Tool Feature Design](tooling/command-line-tool-feature-design.md)
- [IDEA Plugin Toolchain Requirements](tooling/idea-plugin-toolchain.md)

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
