# Module Partitioning Plan

> Language: English
> Published default: docs/en/architecture/module-partitioning.md
> Translation: [简体中文](../../zh-CN/architecture/模块划分方案.md)

Updated: 2026-06-24

## Purpose

This document turns the existing Eva-CLI architecture into a module
architecture plan. The repository is still mostly documentation, so the goal is
not to describe current Rust code. The goal is to define the target workspace
shape, crate boundaries, dependency direction, runtime handoffs, and ownership
rules.

The plan follows the established architecture rule: Rust owns runtime authority,
permissions, schemas, sandboxing, external I/O, recovery, audit, and long-term
state; Lua owns hot-reloadable Agent and capability business logic.

## Partitioning Principles

- Keep stable contracts in small foundation crates before adding runtime logic.
- Put concrete side effects behind ports and registries, not behind direct
  calls from Lua, Scheduler, or domain types.
- Make `eva-runtime` the only composition root that wires concrete
  implementations together.
- Let `Discovery` discover and normalize capabilities; let `Policy` decide
  whether execution is allowed.
- Keep EventBus, Scheduler, AgentRuntime, Lua host, AdapterRegistry, and MCP
  server separable by ownership and dependency direction.
- Keep module names aligned with stable architecture concepts, not temporary
  implementation phases.

## Module Overview

![Module partition overview](../../assets/module-partition-overview.svg)

The target workspace should use crates for stable bounded contexts and modules
inside each crate for implementation detail. This structure is an architecture
boundary map, not a delivery sequence.

## Recommended Workspace Layout

```text
Eva-CLI/
  Cargo.toml
  src/
    main.rs                 # thin binary shim, delegates to eva-cli

  crates/
    eva-core/
      src/
        event.rs
        topic.rs
        ids.rs
        capability.rs
        invoke.rs
        error.rs
        lib.rs

    eva-config/
      src/
        eva_yaml.rs
        manifest/
        schema.rs
        lib.rs

    eva-policy/
      src/
        effective.rs
        permissions.rs
        sandbox.rs
        lib.rs

    eva-observability/
      src/
        trace.rs
        metrics.rs
        audit.rs
        lib.rs

    eva-storage/
      src/
        state_store.rs
        event_log.rs
        artifact_store.rs
        sqlite.rs
        lib.rs

    eva-eventbus/
      src/
        bus.rs
        in_memory.rs
        recoverable.rs
        dead_letter.rs
        lib.rs

    eva-scheduler/
      src/
        registry.rs
        routing.rs
        subscription.rs
        matcher.rs
        mailbox.rs
        lib.rs

    eva-agent/
      src/
        runtime.rs
        lifecycle.rs
        state.rs
        queue.rs
        lib.rs

    eva-lua-host/
      src/
        loader.rs
        sandbox.rs
        bindings.rs
        hot_reload.rs
        lib.rs

    eva-capability/
      src/
        registry.rs
        router.rs
        generation.rs
        host_api.rs
        lib.rs

    eva-adapter/
      src/
        manifest.rs
        registry.rs
        router.rs
        runtime.rs
        error.rs
        transports/
          builtin.rs
          stdio.rs
          http.rs
          eventbus.rs
          mcp.rs
          skill.rs
          hardware.rs
          lua_capability.rs
        lib.rs

    eva-mcp/
      src/
        client.rs
        server.rs
        tool_mapping.rs
        policy.rs
        schema.rs
        lib.rs

    eva-discovery/
      src/
        service.rs
        scanner.rs
        normalizer.rs
        health.rs
        cache.rs
        sources/
          project_agents.rs
          project_adapters.rs
          codex.rs
          omx.rs
          path_commands.rs
          mcp.rs
        lib.rs

    eva-memory/
      src/
        memory_service.rs
        knowledge_service.rs
        context_builder.rs
        lib.rs

    eva-hardware/
      src/
        discovery.rs
        registry.rs
        driver.rs
        hotplug.rs
        state.rs
        lib.rs

    eva-backup/
      src/
        backup_service.rs
        migration_package.rs
        release_snapshot.rs
        manifest_verifier.rs
        lib.rs

    eva-lifecycle/
      src/
        supervisor.rs
        generation.rs
        drain.rs
        rollback.rs
        lib.rs

    eva-runtime/
      src/
        builder.rs
        runtime.rs
        services.rs
        shutdown.rs
        lib.rs

    eva-cli/
      src/
        run.rs
        emit.rs
        inspect.rs
        agent.rs
        adapter.rs
        capability.rs
        lib.rs
```

## Crate Responsibilities

| Crate | Owns | Must Not Own |
| --- | --- | --- |
| `eva-core` | Pure types: `Event`, `Topic`, IDs, capability names, invoke request/response, structured errors | Tokio task wiring, file system access, provider-specific code |
| `eva-config` | `eva.yaml`, manifests, schema loading, config normalization | Permission decisions, runtime mutation |
| `eva-policy` | Effective permissions, sandbox rules, request-level narrowing | Discovery scanning, concrete I/O |
| `eva-observability` | Trace fields, metrics labels, audit sink traits | Business routing or policy decisions |
| `eva-storage` | State store, durable event log, artifact store interfaces and local implementations | Agent business logic |
| `eva-eventbus` | Event publication, recoverable log integration, dead-letter path | Topic subscription matching for Agent routing |
| `eva-scheduler` | Topic matcher, subscription table, Agent mailbox delivery | Lua execution, adapter invocation |
| `eva-agent` | Agent lifecycle, private queue, event handling boundary | Lua sandbox internals, external provider transport |
| `eva-lua-host` | Lua state loading, sandbox, host bindings, hot reload | Policy expansion, direct shell/network/file access |
| `eva-capability` | Capability registry, generation swap, host API traits | Concrete provider or hardware driver implementation |
| `eva-adapter` | Adapter manifest, registry, router, transport runtimes | Discovery scanning, global business state |
| `eva-mcp` | MCP client/server protocol, tool mapping, MCP policy helpers | Unrestricted proxying into internal Topics |
| `eva-discovery` | Trusted source scanning, normalization, health probing, cache | Final execution authorization |
| `eva-memory` | Agent memory, global memory, knowledge, context assembly | EventBus storage semantics |
| `eva-hardware` | Device discovery, driver binding, hotplug state, hardware adapter | Lua raw I/O access |
| `eva-backup` | Backup, migration package, release snapshot, artifact verification | Agent-owned restore or rollback mutation |
| `eva-lifecycle` | Supervisor, runtime generation, drain, rollback | Lua business decisions |
| `eva-runtime` | Composition root, service wiring, startup/shutdown | Domain contracts that lower crates need |
| `eva-cli` | CLI command parsing and user-facing commands | Core runtime ownership |

## Dependency Direction

![Dependency direction rules](../../assets/module-dependency-rules.svg)

Allowed dependency direction:

```text
eva-cli / eva-supervisor / test harness
  -> eva-runtime
  -> eva-discovery / eva-adapter / eva-mcp / eva-memory / eva-hardware / eva-backup / eva-lifecycle
  -> eva-agent / eva-lua-host / eva-capability
  -> eva-eventbus / eva-scheduler
  -> eva-core / eva-config / eva-policy / eva-storage / eva-observability
```

Important restrictions:

- `eva-core` must not depend on any runtime, adapter, Lua, MCP, or CLI crate.
- `eva-scheduler` must not import `eva-lua-host` or `eva-adapter`; it only
  delivers events to registered mailboxes.
- `eva-lua-host` must call host API traits, not concrete file, network, shell,
  MCP, or hardware implementations.
- `eva-adapter` must not change permissions. It receives effective policy and
  enforces it.
- `eva-discovery` must not grant authority. It returns discovered candidates
  and rejected reasons.
- `eva-runtime` may depend on almost every crate because it wires them together.
  No lower crate should depend on `eva-runtime`.

## Runtime Handoff

![Runtime module flow](../../assets/module-runtime-flow.svg)

The main call chain should remain:

```text
Ingress
  -> EventBus
  -> Scheduler
  -> AgentRuntime
  -> Lua on_event
  -> Rust Tool Layer / Capability Host API
  -> AdapterRouter or Runtime Service
  -> AdapterRuntime / MemoryService / HardwareService / MCP Server
  -> structured response or emitted Topic
```

The handoff rules are:

- Ingress only constructs validated events or command requests.
- EventBus publishes events and recovery metadata, but it does not decide which
  Agent should execute business logic.
- Scheduler routes by `target`, exact Topic, wildcard Topic, and explicit
  routing rules.
- AgentRuntime owns one Agent queue, one Lua state generation, and one timeout
  boundary.
- Lua can transform data, keep local Agent state, emit Topics, and request
  controlled tools.
- Tool Layer validates schema, policy, timeout, audit fields, and cancellation.
- AdapterRouter chooses a provider by explicit provider or capability index.
- Concrete transports own provider protocol details only after authorization.

## Cross-Cutting Architecture Rules

### Error Model

Cross-module errors must be structured rather than plain strings:

```text
kind
message
retryable
provider_code?
correlation_id?
causation_id?
```

Individual crates may use richer internal error types, but public cross-crate
APIs should preserve stable error kinds and JSON-compatible shapes.

### Permission Model

Permissions only narrow as requests move through the system:

```text
system policy
  -> manifest permissions
  -> user/session policy
  -> request constraints
  -> effective permissions
```

No crate may expand effective permissions. A module that detects a required
permission expansion must return a structured rejection.

### State Ownership

- Agent local state belongs to `eva-agent` and Lua generations.
- Durable business state belongs to `eva-storage` and `eva-memory`.
- Event recovery state belongs to `eva-eventbus`.
- Adapter runtime state belongs to `eva-adapter`.
- Runtime generation state belongs to `eva-lifecycle`.

Modules must not share hidden mutable global state across these boundaries.

### Observability Fields

Critical handoffs must preserve these fields where applicable:

```text
event_id
request_id
topic
agent_id
adapter_id
capability
provider
correlation_id
causation_id
subscription_pattern
generation
script_version
latency_ms
error_kind
```

## Architecture Risks

| Risk | Control |
| --- | --- |
| Scheduler grows business logic | Keep routing rule data-only and move business decisions to Agents |
| Lua host becomes a hidden system API | Expose only typed host APIs with manifest + policy checks |
| Adapter registry becomes a plugin free-for-all | Require manifest, schema, policy, health, audit, and explicit trust source |
| MCP becomes an unrestricted proxy | Maintain tool/resource/prompt allowlists and per-client policy |
| Discovery accidentally authorizes execution | Keep discovered candidates separate from registered runtime handles |
| State ownership becomes ambiguous | Keep Agent, durable business, event recovery, Adapter, and lifecycle state in separate modules |
| Concrete transports leak inward | Restrict provider, MCP, shell, HTTP, skill, and hardware details to adapter/service crates |

## Summary

The clean module cut is:

```text
foundation contracts
  -> event and routing kernel
  -> isolated Agent and Lua execution boundary
  -> controlled capability and adapter boundary
  -> discovery and runtime services
  -> application, supervisor, and operational surfaces
```

This partition keeps Eva-CLI's architecture explicit: Rust owns trusted
runtime boundaries, Lua owns replaceable business behavior, EventBus and
Scheduler own coordination, Adapter and service crates own side effects, and
`eva-runtime` is the only place where concrete implementations are composed.
