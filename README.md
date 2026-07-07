# Eva-CLI

> Language: English | [简体中文](README.zh-CN.md)

Eva-CLI is a Rust-based CLI runtime for controlled multi-agent workflows,
release hardening, diagnostics, configuration validation, request-scoped
memory/knowledge context assembly, hardware binding plans, backup/lifecycle
checks, and source-release operations.

The repository has reached the V1.5 release-hardening checkpoint: a compileable
Rust workspace, configuration examples, schemas, the in-memory basic runtime
loop, local task diagnostics, Adapter/MCP/Skill/Discovery control surfaces,
request-scoped memory/knowledge context assembly, hardware discovery and
plan-first binding diagnostics, backup/snapshot/restore/upgrade planning,
release readiness/security/performance/migration checks, CI gates, quickstart,
release notes, and explicit known limitations.

Current managed project version: `V1.6.1-alpha` (`Cargo.toml` version
`1.6.1-alpha`, prerelease Git tag form `v1.6.1-alpha`). Version policy is defined in
[Version Management Plan](docs/en/release/version-management-plan.md).

The website uses English as the default public entry with stable
slugs, while the Simplified Chinese documents remain the source of truth for
some detailed architecture and implementation-spec content.

Canonical website:

- https://www.eva-cli.com/
- https://www.eva-cli.com/zh-CN/

The website source is maintained in [website/](website/), documentation is
maintained in [docs/](docs/), and Rust source code lives in [src/](src/) and
[crates/](crates/).

## Current Project Progress

Updated: 2026-07-06

Eva-CLI has moved past a design-only repository and now has a V1.5 release
surface. It includes a compileable Rust workspace, configuration examples and
schemas, implemented foundation contracts, project configuration loading, a
V1.0 CLI quickstart, the `in_memory_v1.0` basic runtime composition root, local
`.eva/tasks` diagnostics, Adapter/MCP/Skill/Discovery diagnostics,
MemoryService/KnowledgeService/ContextBuilder, controlled hardware discovery
and binding plans, backup artifact verification, release snapshot restore
plans, lifecycle generation/drain/rollback checks, executable release
hardening gates, and CI release gates.

| Area | Status | Evidence | Remaining Work |
| --- | --- | --- | --- |
| Architecture and docs | Mostly complete for the first implementation cycle | English and Simplified Chinese docs, diagrams, website pages, roadmap, risk review | Keep docs synchronized with implementation; turn remaining design assumptions into executable contracts |
| Website and docs publishing | Implemented | Static website source, localized content, validation/build scripts | Continue content maintenance and CI verification as product behavior changes |
| Rust workspace layout | Implemented | Root `Cargo.toml`, binary shim, 20 workspace crates under `crates/` | Keep dependency direction strict as behavior is added |
| Configuration examples and schemas | Implemented first pass | `config/` contains sample `eva.yaml`, agent/adapter/capability/policy manifests, routes, and JSON schemas; `eva-config` loads and validates the project config | Add deeper schema tooling and integration checks as runtime behavior expands |
| `eva-core` foundation contracts | Implemented first pass | Topic, ID, Capability, Event, Invoke, and Error contracts with stable re-exports | Downstream crates continue adopting these public types |
| `eva-cli` | V1.5 implemented | `version`, `doctor`, `config validate`, `inspect`, `run --example basic`, `task status/logs/cancel`, `adapter list/probe`, `mcp list/probe`, `skill list/run`, `discovery scan`, `memory context`, `hardware list/probe/bind`, `backup create`, `snapshot create`, `restore plan`, `upgrade check`, `release check/security/perf/migration`, text/JSON output, trace fields, and exit-code mapping | Keep command contracts stable as future apply paths are added |
| Runtime composition | V1.0 core implemented | No-op builder, V1.0 in-memory builder, `RuntimeSummary`, service summaries, `TaskReport`, and idempotent shutdown | Durable/runtime lifecycle work remains later scope |
| EventBus and Scheduler | V0.4/V0.5 implemented for basic loop | EventBus publish/ack/fail/dead-letter/replay diagnostics; Scheduler topic routing and mailbox delivery | Durable replay/backoff remains later scope |
| Agent and Lua host | V0.5 implemented for basic loop | Agent lifecycle, bounded queue, timeout/cancel/retry run control, Lua loading, sandbox gate, controlled bindings, generation marker | Real Lua VM and generation swap remain later scope |
| Capability and Adapter layers | V1.1 controlled envelopes implemented | `eva-capability` has V0.4 builtins; `eva-adapter` now builds authorized handles, routes capabilities to providers, probes adapters, and invokes MCP/Skill controlled envelopes | Real stdio/http process execution and richer policy evaluation remain later scope |
| Policy, observability, storage | Mixed | `eva-policy` and `eva-observability` have V0.2 contracts; `eva-storage` has V0.4 in-memory stores/logs | Durable storage, richer audit sinks, and metrics remain later scope |
| Discovery, MCP, memory, hardware, backup, lifecycle, release | Mixed | Discovery and MCP have V1.1 side-effect-free candidates/probes; memory has V1.2 in-memory private/global records, knowledge search, ContextBuilder, and Lua context snapshots; hardware has V1.3 discovery candidates, registry leases, simulated driver binding, hotplug state machine, Adapter hardware transport, and CLI binding plans; backup and lifecycle have V1.4 backup artifact verification, migration preflight, release snapshot restore plans, generation handoff, drain, rollback, and upgrade checks; release has V1.5 readiness/security/performance/migration gates | Real apply paths, signed artifacts, and packaged installers remain later scope |
| Verification baseline | Passing and gated | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, V1.0 quickstart smoke commands, V1.1 external capability smoke commands, V1.2 memory context smoke, V1.3 hardware smoke, V1.4 backup/lifecycle smoke, V1.5 release hardening smoke, and website i18n validation | Add gates as future runtime behavior expands |

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

See [Zero to 1.0 Roadmap](docs/en/planning/zero-to-one-roadmap.md) for the staged
release path from design documents to a 1.0 release, and see
[eva-core Module Design](docs/en/architecture/eva-core-module.md) plus
[crates/eva-core/README.md](crates/eva-core/README.md) for the implemented
foundation contract layer.

## V1.0 Quickstart

The supported V1.0 source-install path is documented in
[Eva-CLI V1.0 Quickstart](docs/en/guide/v1.0-quickstart.md). The short path is:

```powershell
git clone https://github.com/Yetmos/Eva-CLI.git
cd Eva-CLI
cargo build --release
cargo run -- --version
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- inspect runtime --output json
cargo run -- run --example basic --task-id req-readme-v10 --output json
cargo run -- task status --task req-readme-v10 --output json
cargo run -- task logs --task req-readme-v10 --output json
```

V1.0 scope and non-goals are explicit in
[Known Limitations](docs/en/release/v1.0-known-limitations.md), and the release summary
is in [V1.0.0 Release Notes](docs/en/release/release-notes-v1.0.0.md).

## V1.1 External Capability Smoke

V1.1 adds a controlled external capability ecosystem without starting real
stdio/http/MCP server processes. These commands prove that external capability
surfaces are visible, probeable, and callable through a safe envelope:

```powershell
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- adapter probe --capability workflow.code_review --provider code-review-skill --output json
cargo run -- mcp list --output json
cargo run -- mcp probe --adapter github-mcp --tool list_issues --output json
cargo run -- skill list --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- discovery scan --output json
```

The key V1.1 boundary is that discovery returns candidates only. Runtime
authority still comes from validated manifests and `eva-adapter` handles.

## V1.2 Memory And Knowledge Context Smoke

V1.2 adds a request-scoped context layer. `eva-memory` owns private Agent
memory, global memory, knowledge indexing, and budgeted context assembly, while
`eva-lua-host` receives only a controlled `LuaContextSnapshot` summary.

```powershell
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
```

The output includes `memory`, `global_memory`, `knowledge`, `lua_context`, and
`audit` fields. Private records are selected only for the requested `agent_id`;
global memory and knowledge remain explicit context inputs rather than EventBus
state.

## V1.3 Hardware Access Smoke

V1.3 adds controlled hardware discovery and plan-first binding diagnostics.
`eva-hardware` owns discovery candidates, stable device identities,
`DeviceRegistry` leases, simulated driver binding, and hotplug state. The
Adapter hardware transport uses that boundary and reports `raw_io:false` in
audit output.

```powershell
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
```

The sample `scale-main` adapter is intentionally disabled until real device
identifiers are configured. Hardware binding therefore returns a blocked plan
instead of opening USB, serial, BLE, network, or vendor SDK raw I/O.

## V1.4 Backup And Lifecycle Smoke

V1.4 adds plan-first backup, snapshot, restore, and upgrade lifecycle commands.
`eva-backup` verifies in-memory artifacts and produces release restore plans,
while `eva-lifecycle` models generation handoff, drain, rollback, and supervisor
readiness without starting real processes.

```powershell
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- upgrade check --output json
```

Restore and upgrade commands remain diagnostic in V1.4: they do not execute
destructive restore, move release pointers, or start Supervisor/Runtime
processes.

## V1.5 Release Hardening Smoke

V1.5 adds executable release-hardening checks through `eva-release` and the
`release` CLI command group. These commands aggregate cross-platform readiness,
security findings, performance budgets, migration steps, and compatibility
policy without mutating project state or starting external providers.

```powershell
cargo run -- release check --output json
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
```

The release-hardening docs are:

- [V1.5 Release Hardening](docs/en/release/v1.5-release-hardening.md)
- [V1.5 Migration Guide](docs/en/release/v1.5-migration-guide.md)
- [V1.5 Compatibility Policy](docs/en/release/v1.5-compatibility-policy.md)
- [V1.6.1 Alpha Release Notes](docs/en/release/release-notes-v1.6.1.md)
- [V1.5.1 Release Notes](docs/en/release/release-notes-v1.5.1.md)
- [V1.5.0 Release Notes](docs/en/release/release-notes-v1.5.0.md)
- [V1.5 GitHub Release Plan](docs/en/release/v1.5-github-release-plan.md)
- [Version Management Plan](docs/en/release/version-management-plan.md)

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
- [Simplified Chinese docs](docs/zh-CN/中文文档入口.md) - current detailed source of truth.
- [Documentation maintenance guide](docs/README.md)

Recommended reading order for the English default documentation:

1. [Architecture Overview](docs/en/architecture/architecture-overview.md): start with system
   boundaries, core modules, and the overall conclusion.
2. [Rust, Lua, and EventBus Scheduler](docs/en/architecture/rust-lua-eventbus-scheduler.md):
   understand the Runtime, EventBus, Scheduler, Lua Agents, and Topic routing.
3. [Lua External Agent Adapter](docs/en/capabilities/lua-external-agent-adapter.md):
   understand how external Agents, CLI tools, HTTP APIs, MCP servers, and
   Skills are connected through adapters.
4. [Lua Skill, MCP, and Tool Hot Reload](docs/en/capabilities/lua-skill-mcp-tool-hot-reload.md):
   understand how tools, Lua Skills, and MCP tool handlers are pushed down into
   Lua and updated through hot reload.
5. [Skill Implementation Plan](docs/en/capabilities/skill-implementation.md): understand
   how workflow Skills, runtime workers, and Lua Skills become controlled
   `workflow.*` capabilities.
6. [Agent Memory and Knowledge Base](docs/en/capabilities/agent-memory-knowledge-base.md):
   understand Agent-private memory, system-wide memory, knowledge bases, and
   context-building boundaries.
7. [Agent Discovery](docs/en/capabilities/agent-discovery.md): understand how project
   configuration, user environments, MCP, Skills, and Lua capabilities are
   discovered and registered.
8. [Hardware Hotplug](docs/en/capabilities/hardware-hotplug.md): understand how USB, serial,
   BLE, network, and vendor SDK devices are connected through HardwareAdapter
   with hotplug support.
9. [Project Configuration](docs/en/operations/project-configuration.md): understand YAML
   configuration, schemas, policies, manifests, and hot-reload boundaries.
10. [Process-Level Upgrade](docs/en/operations/process-level-upgrade.md): understand the
   Supervisor, runtime generations, blue-green switching, draining, recovery,
   and rollback.
11. [Backup, Migration Package, and Release Snapshot](docs/en/operations/backup-migration-release-snapshot.md):
    understand why trusted backup, migration, release snapshot, restore, and
    rollback execution belongs to the Runtime while Agents only request and
    explain operations.
12. [Design Risk Review](docs/en/planning/design-risk-review.md): review historical
    design risks, semantic gaps, and areas that still need stronger executable
    contracts.
13. [Zero to 1.0 Roadmap](docs/en/planning/zero-to-one-roadmap.md): follow the staged
    implementation path from architecture documents to module layout,
    contracts, a minimum runtime loop, and release readiness.
14. [Command-Line Tool Feature Design](docs/en/tooling/command-line-tool-feature-design.md):
    turn the runtime architecture into the target `eva` command surface,
    including command groups, output contracts, safety gates, and release
    priorities.

## Document Responsibilities

| Document | Responsibility |
| --- | --- |
| [Architecture Overview](docs/en/architecture/architecture-overview.md) | Main entry point for system goals, non-goals, module boundaries, runtime flow, security principles, and pending design work. |
| [Rust, Lua, and EventBus Scheduler](docs/en/architecture/rust-lua-eventbus-scheduler.md) | Defines the Rust Runtime, Lua Agents, EventBus, Topics, Scheduler, state, consistency, and hot reload. |
| [Lua External Agent Adapter](docs/en/capabilities/lua-external-agent-adapter.md) | Defines AdapterRegistry, AdapterRouter, McpAdapter, SkillAdapter, HardwareAdapter, and external capability transports such as stdio, HTTP, EventBus, and hardware. |
| [Lua Skill, MCP, and Tool Hot Reload](docs/en/capabilities/lua-skill-mcp-tool-hot-reload.md) | Defines `lua_tool`, `lua_skill`, `lua_mcp_handler`, the Lua Capability Runtime, host APIs, security sandboxing, and generation swaps. |
| [Skill Implementation Plan](docs/en/capabilities/skill-implementation.md) | Defines Skill classification, manifests, runtime gates, invocation routing, security boundaries, hot reload, and verification rules. |
| [Agent Memory and Knowledge Base](docs/en/capabilities/agent-memory-knowledge-base.md) | Defines Agent-private memory, system-wide memory, knowledge bases, ContextBuilder, permissions, audit, and consistency boundaries. |
| [Agent Discovery](docs/en/capabilities/agent-discovery.md) | Defines how AgentDiscoveryService scans, identifies, validates, caches, and registers Agents, adapters, MCP, Skills, and Lua capabilities. |
| [Hardware Hotplug](docs/en/capabilities/hardware-hotplug.md) | Defines HardwareDiscoveryService, DeviceRegistry, DriverBinding, HardwareAdapterRuntime, device hotplug, hardware Topics, and security boundaries. |
| [Project Configuration](docs/en/operations/project-configuration.md) | Defines the `config/` directory, `eva.yaml`, Agent/Adapter/Capability manifests, policies, schemas, and hot-reload strategy. |
| [Process-Level Upgrade](docs/en/operations/process-level-upgrade.md) | Defines the OS service manager, Supervisor, Runtime, Ingress Gate, Durable Event Log, State Store, and blue-green traffic switching. |
| [Backup, Migration Package, and Release Snapshot](docs/en/operations/backup-migration-release-snapshot.md) | Defines why backup, migration package, release snapshot, restore, rollback, manifest verification, and artifact audit belong to Runtime services. |
| [Design Risk Review](docs/en/planning/design-risk-review.md) | Reviews design risks around Bot behavior, event consistency, state ownership, permission closure, capability semantics, and error recovery. |
| [Zero to 1.0 Roadmap](docs/en/planning/zero-to-one-roadmap.md) | Defines the staged path from architecture documents to module layout, contracts, a minimum runnable skeleton, a minimum Runtime loop, module implementation, and 1.0 release readiness. |
| [Command-Line Tool Feature Design](docs/en/tooling/command-line-tool-feature-design.md) | Defines the target `eva` command groups, global flags, safety gates, output contract, exit codes, and staged CLI implementation priorities. |

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

## Remaining V1.x Gaps

V1.5 is a source-release and release-hardening checkpoint, not a packaged
installer distribution. Later release tags that contain package support publish
the GHCR container image `ghcr.io/yetmos/eva-cli`; the existing `v1.5.0` tag is
not republished retroactively. The main remaining work is now narrower and more
implementation-focused:

- Real provider execution for stdio/http/MCP processes, including authentication,
  session isolation, timeout handling, and rate limits.
- Durable EventBus, Scheduler, task, audit, and artifact stores beyond the
  current in-memory and local diagnostic surfaces.
- Real Lua VM execution, generation swaps, and stable `ctx.tools` / `ctx.host`
  bindings behind the Rust runtime boundary.
- Destructive apply paths such as `restore apply`, release pointer mutation,
  supervisor activation, and blue-green runtime process handoff.
- Signed release artifacts, cross-platform installers, package-manager packages,
  and artifact provenance.
- Deeper machine-verifiable schemas and policy checks as high-risk apply paths
  move from plan-only diagnostics to execution.

The current documentation now distinguishes implemented diagnostics from target
apply paths. See [Design Risk Review](docs/en/planning/design-risk-review.md) for the
original architectural risk inventory, and
[V1.5 Compatibility Policy](docs/en/release/v1.5-compatibility-policy.md) for the
contracts held stable by this source release.
