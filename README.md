# Eva-CLI

> Language: English | [简体中文](README.zh-CN.md)

Eva-CLI is currently moving from architecture and specification consolidation
into executable Rust implementation. The repository is not yet a final runnable
CLI, but it now contains the Rust workspace scaffold, configuration examples,
schemas, and the first implemented foundation contract crate. The website uses
English as the default public entry with stable slugs, while the Simplified
Chinese documents remain the source of truth for some detailed architecture and
implementation-spec content.

Website:

- https://Eva-CLI.com
- https://www.Eva-CLI.com

The website source is maintained in [website/](website/), documentation is
maintained in [docs/](docs/), and Rust source code lives in [src/](src/) and
[crates/](crates/).

## Current Project Progress

Updated: 2026-07-01

Eva-CLI has moved past a design-only repository. It now has a compileable Rust
workspace, module boundary scaffolding, configuration examples and schemas, and
the first real foundation contract crate (`eva-core`). Most runtime crates are
still placeholders that define ownership boundaries but do not yet implement
behavior.

| Area | Status | Evidence | Remaining Work |
| --- | --- | --- | --- |
| Architecture and docs | Mostly complete for the first implementation cycle | English and Simplified Chinese docs, diagrams, website pages, roadmap, risk review | Keep docs synchronized with implementation; turn remaining design assumptions into executable contracts |
| Website and docs publishing | Implemented | Static website source, localized content, validation/build scripts | Continue content maintenance and CI verification as product behavior changes |
| Rust workspace layout | Implemented | Root `Cargo.toml`, binary shim, 19 workspace crates under `crates/` | Keep dependency direction strict as behavior is added |
| Configuration examples and schemas | Partial | `config/` contains sample `eva.yaml`, agent/adapter/capability/policy manifests, and JSON schemas | Wire schema loading and validation into `eva-config` and CLI commands |
| `eva-core` foundation contracts | Implemented first pass | Topic, ID, Capability, Event, Invoke, and Error contracts with 47 unit tests and stable re-exports | Downstream crates still need to adopt these public types; serde/JSON support should be reviewed separately |
| `eva-cli` | Scaffold only | Root binary delegates to `eva-cli`; command modules exist | Implement real `run`, `validate`, `doctor`, `emit`, `inspect`, agent, adapter, and capability commands |
| Runtime composition | Scaffold only | `eva-runtime` crate and modules exist | Build service wiring, startup/shutdown, config loading, and runtime lifecycle |
| EventBus and Scheduler | Scaffold only | `eva-eventbus` and `eva-scheduler` crates exist | Implement publish/recover/dead-letter behavior, topic subscriptions, routing, and mailbox delivery |
| Agent and Lua host | Scaffold only | `eva-agent` and `eva-lua-host` crates exist | Implement lifecycle, queues, Lua loading, sandboxing, bindings, and hot reload |
| Capability and Adapter layers | Scaffold only | `eva-capability` and `eva-adapter` crates exist | Implement registries, provider routing, authorized transports, errors, and generation swaps |
| Policy, observability, storage | Scaffold only | `eva-policy`, `eva-observability`, and `eva-storage` crates exist | Implement effective permission narrowing, trace/audit/metrics contracts, state store, event log, and artifacts |
| Discovery, MCP, memory, hardware, backup, lifecycle | Scaffold only | Dedicated crates and module boundaries exist | Implement trusted discovery, MCP mapping, memory/context services, hardware hotplug, backup/release snapshots, and supervisor generation flow |
| Verification baseline | Passing | `cargo fmt --check`, `cargo test -p eva-core`, `cargo check --workspace`, `cargo doc -p eva-core --no-deps`, `cargo test --workspace` | Add CI coverage for future runtime behavior, schema validation, examples, and integration tests |

## Implementation Plan

The implementation should continue in small, testable stages. Each stage should
leave behind compileable artifacts, focused tests, and updated documentation.

| Phase | Goal | Main Deliverables | Exit Criteria |
| --- | --- | --- | --- |
| 0. Documentation and architecture baseline | Keep the design intent clear before adding behavior | Architecture docs, roadmap, module partitioning, risk review, website/docs structure | Docs explain ownership, non-goals, and release path |
| 1. Workspace and module scaffolding | Make crate boundaries and dependency direction concrete | Root binary shim, workspace crates, module files, README files | `cargo check --workspace` passes with all crates present |
| 2. Foundation contracts | Stabilize shared types before runtime behavior | `eva-core` Topic, IDs, Capability, Event, Invoke, Error contracts | `cargo test -p eva-core` covers parsing, validation, matching, construction, and status semantics |
| 3. Config and policy contracts | Turn manifests and policies into machine-verifiable inputs | `eva-config` schema loading/validation, manifest normalization, `eva-policy` effective permissions | `eva validate` can reject invalid sample config and unsafe policy expansions |
| 4. CLI skeleton | Establish user-facing development loop | Real `eva doctor`, `eva validate`, `eva run`, `eva emit`, `eva inspect` command surfaces | CLI builds from clean checkout and returns structured output and exit codes |
| 5. Event and scheduling kernel | Route one typed event without Lua or external providers | EventBus publish/recover path, scheduler subscriptions, agent mailbox contracts | One in-process event can be routed deterministically under tests |
| 6. Agent and Lua execution boundary | Run one controlled Lua Agent safely | Agent lifecycle, queue, Lua loader, sandbox, host bindings, timeout boundary | One Lua Agent can process one event in an isolated state generation |
| 7. Capability and adapter execution | Allow controlled tool/provider calls | Capability registry/router, AdapterRegistry, built-in/stdio/MCP/skill/hardware transport boundaries | One authorized capability call completes with trace, audit, and structured error handling |
| 8. Minimum end-to-end runtime loop | Prove the full architecture with one narrow path | Ingress -> EventBus -> Scheduler -> Agent -> Lua -> Tool -> response, plus example project | `examples/basic/` runs and integration tests cover success and failure |
| 9. Hot reload, recovery, and lifecycle | Make runtime changes and failures controlled | Generation swaps, drain, rollback, durable event log, backup/release snapshot integration | Runtime can reject unsafe changes, drain old generations, and recover from known failures |
| 10. Hardening and 1.0 readiness | Turn working internals into a release-quality CLI | CI, cross-platform checks, security review, quickstart, install docs, release notes, migration guidance | New users can install, run quickstart, diagnose failures, and rely on stable documented contracts |

See [Zero to 1.0 Roadmap](docs/en/zero-to-one-roadmap.md) for the staged
release path from design documents to a 1.0 release, and see
[eva-core Module Design](docs/en/eva-core-module.md) plus
[crates/eva-core/README.md](crates/eva-core/README.md) for the implemented
foundation contract layer.

## Repository Layout

```text
Eva-CLI/
  src/                 # Thin binary shim that delegates to eva-cli
  crates/              # Rust workspace crates and module boundaries
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

- [English docs](docs/en/README.md) - default public entry and stable slug set.
- [Simplified Chinese docs](docs/zh-CN/README.md) - current detailed source of truth.
- [Documentation maintenance guide](docs/README.md)

Recommended reading order for the English default documentation:

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
13. [Zero to 1.0 Roadmap](docs/en/zero-to-one-roadmap.md): follow the staged
    implementation path from architecture documents to module layout,
    contracts, a minimum runtime loop, and release readiness.
14. [Command-Line Tool Feature Design](docs/en/command-line-tool-feature-design.md):
    turn the runtime architecture into the target `eva` command surface,
    including command groups, output contracts, safety gates, and release
    priorities.

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
| [Command-Line Tool Feature Design](docs/en/command-line-tool-feature-design.md) | Defines the target `eva` command groups, global flags, safety gates, output contract, exit codes, and staged CLI implementation priorities. |

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
