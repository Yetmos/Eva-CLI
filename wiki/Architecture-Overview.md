# Architecture Overview

Eva-CLI is designed as a local multi-Agent runtime where Rust owns authority and
Lua owns hot-reloadable Agent behavior. The system boundary is built around
typed events, controlled capabilities, explicit memory services, and recoverable
runtime generations.

Canonical source:
[docs/en/architecture-overview.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/architecture-overview.md)

## Architecture Decision

Eva-CLI is a Rust-managed, Topic EventBus driven multi-Agent runtime. Agent
business behavior is implemented in Lua so workflows can be updated without
rebuilding the host process, while Rust keeps authority over:

- permissions
- manifest and schema validation
- sandboxing
- external I/O
- secret handling
- recovery
- audit
- long-term state

## Runtime Layers

```text
Ingress
  -> Recoverable EventBus
  -> Scheduler
  -> Lua Agent Runtime
  -> AdapterRegistry / MemoryService / KnowledgeService / HardwareAdapter
  -> Supervisor and runtime generation switching
```

## Main Components

| Component | Responsibility |
| --- | --- |
| Ingress | Accept CLI, API, UI, and external events. |
| EventBus | Route typed Topic events and preserve recoverable coordination history when configured. |
| Scheduler | Match events to Agent queues by Topic, target, priority, load, and policy. |
| Lua Agent Runtime | Execute isolated Agent behavior and local orchestration. |
| AdapterRegistry | Expose external tools, Agents, MCP servers, skills, local models, and hardware under policy. |
| MemoryService | Store private Agent memory and approved global memory. |
| KnowledgeService | Provide searchable project and reference knowledge. |
| ContextBuilder | Assemble policy-filtered context for a specific Agent invocation. |
| Supervisor | Manage Runtime lifecycle, health, generation switching, draining, and rollback. |

## Architecture Diagram

![Eva-CLI architecture diagram](https://raw.githubusercontent.com/Yetmos/Eva-CLI/main/assets/eva-cli-architecture.svg)

## Implementation Contracts

- Every executable capability must have a manifest, schema, policy, owner,
  version, and audit identity.
- Hot reload may update scripts, routes, registrations, and selected manifest
  fields.
- Permission expansion, transport changes, state backend changes, and MCP
  command changes require a runtime switch or restart path.
- Translations must preserve the architectural meaning of the English canonical
  docs.

## Key Boundaries

- Lua does not directly own filesystem, network, process, hardware, or secret
  access.
- EventBus carries coordination events; durable business state belongs in
  explicit stores.
- Discovery creates candidates; authorization happens later through policy,
  schema, sandbox, and audit checks.
- External capability output must be validated before Lua receives it.
