# Eva-CLI

> Language: English | [简体中文](README.zh-CN.md)

Eva-CLI is currently in the architecture and specification consolidation stage.
The repository mainly contains design documents under `docs/`; it is not yet a
final runnable CLI implementation. The documentation and website have been
migrated to an English canonical source with a multilingual structure that can
expand over time.

Website:

- https://Eva-CLI.com
- https://www.Eva-CLI.com

The website source is maintained in [website/](website/), documentation is
maintained in [docs/](docs/), and future Rust source code will live in
[src/](src/) and [crates/](crates/).

## Current Progress

Eva-CLI has completed most of the architecture and design-document stage for
the first implementation cycle. The next milestone is to move from documents
into executable structure:

1. create the Rust project and module layout;
2. define the first manifest, event, policy, error, and Lua host API contracts;
3. build a minimum runnable CLI skeleton;
4. implement the minimum end-to-end Runtime loop;
5. expand one module at a time under test coverage.

See [Zero to 1.0 Roadmap](docs/en/zero-to-one-roadmap.md) for the staged
release path from design documents to a 1.0 release.

## Repository Layout

```text
Eva-CLI/
  src/                 # Future main program source
  crates/              # Future Rust workspace crates
  docs/                # Architecture documents and implementation specs
  website/             # Static website source
  examples/            # Examples and integration demos
  assets/              # Shared images, diagrams, and visual assets
  .github/workflows/   # CI, deployment, and automation workflows
```

The website is a static site with no runtime dependency. The GitHub Pages
workflow runs `scripts/build-site-i18n.ps1` to generate localized HTML, runs
`scripts/validate-i18n.ps1` to validate the structure, then publishes the
combined `website/`, `docs/`, and `assets/` content.

## Architecture Overview

![Eva-CLI architecture overview](assets/eva-cli-architecture.svg)

The diagram summarizes the current design path: ingress and configuration hot
reload enter the Rust-managed Runtime, flow through a Recoverable EventBus and
Scheduler, and are dispatched to Lua Agents. Lua only handles business logic
inside the sandbox and accesses controlled capabilities through the Rust Tool
Layer, AdapterRegistry, MemoryService, KnowledgeService, and HardwareAdapter.

## Documentation Entrances

Default documentation entrances:

- [English canonical docs](docs/en/README.md)
- [Simplified Chinese docs](docs/zh-CN/README.md)
- [Documentation maintenance guide](docs/README.md)

Recommended reading order for the English canonical documentation:

1. [Architecture Overview](docs/en/architecture-overview.md): start with system
   boundaries, core modules, and the overall conclusion.
2. [Rust, Lua, and EventBus Scheduler](docs/en/rust-lua-eventbus-scheduler.md):
   understand the Runtime, EventBus, Scheduler, Lua Agents, and Topic routing.
3. [Lua External Agent Adapter](docs/en/lua-external-agent-adapter.md):
   understand how external Agents, CLI tools, HTTP APIs, MCP servers, and
   Skills are connected through adapters.
4. [Lua Skill, MCP, and Tool Hot Reload](docs/en/lua-skill-mcp-tool-hot-reload.md):
   understand how tools, Lua Skills, and MCP tool handlers are pushed down into
   Lua and updated through hot reload.
5. [Skill Implementation Plan](docs/en/skill-implementation.md): understand
   how workflow Skills, runtime workers, and Lua Skills become controlled
   `workflow.*` capabilities.
6. [Agent Memory and Knowledge Base](docs/en/agent-memory-knowledge-base.md):
   understand Agent-private memory, system-wide memory, knowledge bases, and
   context-building boundaries.
7. [Agent Discovery](docs/en/agent-discovery.md): understand how project
   configuration, user environments, MCP, Skills, and Lua capabilities are
   discovered and registered.
8. [Hardware Hotplug](docs/en/hardware-hotplug.md): understand how USB, serial,
   BLE, network, and vendor SDK devices are connected through HardwareAdapter
   with hotplug support.
9. [Project Configuration](docs/en/project-configuration.md): understand YAML
   configuration, schemas, policies, manifests, and hot-reload boundaries.
10. [Process-Level Upgrade](docs/en/process-level-upgrade.md): understand the
   Supervisor, runtime generations, blue-green switching, draining, recovery,
   and rollback.
11. [Backup, Migration Package, and Release Snapshot](docs/en/backup-migration-release-snapshot.md):
    understand why trusted backup, migration, release snapshot, restore, and
    rollback execution belongs to the Runtime while Agents only request and
    explain operations.
12. [Design Risk Review](docs/en/design-risk-review.md): review design-only
    risks, semantic gaps, and areas that still need stronger specification.
12. [Zero to 1.0 Roadmap](docs/en/zero-to-one-roadmap.md): follow the staged
    implementation path from architecture documents to module layout,
    contracts, a minimum runtime loop, and release readiness.

## Document Responsibilities

| Document | Responsibility |
| --- | --- |
| [Architecture Overview](docs/en/architecture-overview.md) | Main entry point for system goals, non-goals, module boundaries, runtime flow, security principles, and pending design work. |
| [Rust, Lua, and EventBus Scheduler](docs/en/rust-lua-eventbus-scheduler.md) | Defines the Rust Runtime, Lua Agents, EventBus, Topics, Scheduler, state, consistency, and hot reload. |
| [Lua External Agent Adapter](docs/en/lua-external-agent-adapter.md) | Defines AdapterRegistry, AdapterRouter, McpAdapter, SkillAdapter, HardwareAdapter, and external capability transports such as stdio, HTTP, EventBus, and hardware. |
| [Lua Skill, MCP, and Tool Hot Reload](docs/en/lua-skill-mcp-tool-hot-reload.md) | Defines `lua_tool`, `lua_skill`, `lua_mcp_handler`, the Lua Capability Runtime, host APIs, security sandboxing, and generation swaps. |
| [Skill Implementation Plan](docs/en/skill-implementation.md) | Defines Skill classification, manifests, runtime gates, invocation routing, security boundaries, hot reload, and verification rules. |
| [Agent Memory and Knowledge Base](docs/en/agent-memory-knowledge-base.md) | Defines Agent-private memory, system-wide memory, knowledge bases, ContextBuilder, permissions, audit, and consistency boundaries. |
| [Agent Discovery](docs/en/agent-discovery.md) | Defines how AgentDiscoveryService scans, identifies, validates, caches, and registers Agents, adapters, MCP, Skills, and Lua capabilities. |
| [Hardware Hotplug](docs/en/hardware-hotplug.md) | Defines HardwareDiscoveryService, DeviceRegistry, DriverBinding, HardwareAdapterRuntime, device hotplug, hardware Topics, and security boundaries. |
| [Project Configuration](docs/en/project-configuration.md) | Defines the `config/` directory, `eva.yaml`, Agent/Adapter/Capability manifests, policies, schemas, and hot-reload strategy. |
| [Process-Level Upgrade](docs/en/process-level-upgrade.md) | Defines the OS service manager, Supervisor, Runtime, Ingress Gate, Durable Event Log, State Store, and blue-green traffic switching. |
| [Backup, Migration Package, and Release Snapshot](docs/en/backup-migration-release-snapshot.md) | Defines why backup, migration package, release snapshot, restore, rollback, manifest verification, and artifact audit belong to Runtime services. |
| [Design Risk Review](docs/en/design-risk-review.md) | Reviews design risks around Bot behavior, event consistency, state ownership, permission closure, capability semantics, and error recovery. |
| [Zero to 1.0 Roadmap](docs/en/zero-to-one-roadmap.md) | Defines the staged path from architecture documents to module layout, contracts, a minimum runnable skeleton, a minimum Runtime loop, module implementation, and 1.0 release readiness. |

## Current Design Position

The current design target is a multi-Agent scheduling system that combines a
Rust-managed runtime, hot-reloadable Lua Agents, Topic EventBus, dynamic
adapters, bidirectional MCP integration, HardwareAdapter, and process-level
recovery.

Core boundaries:

- Rust owns system boundaries, permissions, schemas, sandboxing, secrets,
  process lifecycle, audit, timeout handling, and recovery.
- Lua owns hot-reloadable business logic, Agent-local state, tool orchestration,
  and result mapping.
- Topic EventBus coordinates Agent collaboration; it must not become an
  implicit global business state store.
- Adapters connect external capabilities, including CLI tools, HTTP APIs, MCP,
  Skills, local models, internal Agents, and hardware.
- Discovery only performs discovery and normalization. It is not authorization;
  execution must still pass through manifests, schemas, and policies.
- External hardware is connected through the `hardware` transport and
  HardwareAdapter. Lua must not directly access device handles, system device
  paths, or raw IO.
- Hot reload covers scripts, hot-reloadable manifest fields, routes, and
  registry generations by default. Permission expansion, transports, MCP
  commands, state backends, and similar boundary changes require runtime rebuild
  or blue-green switching.

## Current Gaps

The current `docs/` directory is an architecture proposal set, not a final
implementation specification. Before implementation, the following contracts
still need to become machine-verifiable:

- JSON Schemas for `AgentManifest`, `AdapterManifest`, `CapabilityManifest`,
  MCP policy, hardware policy, and sandbox policy.
- Lua binding APIs for `ctx.tools` and `ctx.host`.
- Capability naming registry and conflict-handling rules.
- MCP server authentication, session isolation, and per-client rate limits.
- Audit fields and rollback guidance for adapters, Skills, and Lua capabilities
  that write into a workspace.
- Verifiable schemas for external hardware manifests, including device matching,
  logical binding, generation, and raw IO policy.

The design already covers the target architecture, but Bot behavior semantics,
state consistency, permission merging, capability registration, Lua bindings,
schemas, error recovery, and verification invariants still need to be hardened
into executable specifications. See [Design Risk Review](docs/en/design-risk-review.md)
for details.
