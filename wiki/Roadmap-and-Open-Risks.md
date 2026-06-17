# Roadmap and Open Risks

Eva-CLI has a coherent architecture direction, but the repository is still in
the design-spec stage. The next useful work is to convert the current
architecture into machine-checkable contracts and a minimal runnable loop.

Canonical source:
[docs/en/design-risk-review.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/design-risk-review.md)

## Current Assessment

The direction is coherent:

- Rust owns authority.
- Lua owns hot-reloadable behavior.
- Discovery is not authorization.
- AdapterRegistry is the controlled capability boundary.
- EventBus coordinates Agents without owning hidden global business state.

The largest remaining risk is that platform capability definitions are more
detailed than Bot behavior semantics. The first implementation should prove a
small end-to-end loop before expanding the platform surface.

## Minimal First Runtime Loop

The first executable milestone should prove:

1. user input enters Ingress
2. Ingress publishes a typed Topic event
3. Scheduler routes the event to one Agent queue
4. Lua Agent handles the event in an isolated state
5. Agent calls one controlled tool through Rust
6. Rust validates policy and schema before execution
7. result returns to Lua as a structured value
8. memory/context access records provenance
9. runtime emits trace and audit data
10. failure returns a structured, retry-aware error

## Required Contracts Before Broad Implementation

- `AgentManifest`, `AdapterManifest`, and `CapabilityManifest` schemas.
- MCP, hardware, sandbox, and adapter policy schemas.
- `ctx.tools`, `ctx.host`, `ctx.memory`, `ctx.global_memory`, and
  `ctx.knowledge` Lua host API contracts.
- Capability naming registry and conflict rules.
- Adapter error classes with origin, retryability, audit ID, and redaction
  behavior.
- Hot reload rollback test fixtures.
- Memory provenance and access policy test fixtures.

## Key Risks

| Risk | Mitigation |
| --- | --- |
| Bot behavior is underspecified compared with runtime mechanics. | Start with one narrow Agent loop and formalize behavior contracts. |
| EventBus, memory, and state ownership blur. | Keep EventBus for coordination only; use explicit state stores for durable state. |
| Adapter policy merge rules become inconsistent. | Define deterministic merge order and provenance reporting. |
| Lua host API grows too wide. | Keep API narrow, stable, and testable; require Rust policy for all host access. |
| MCP or hardware widens host access implicitly. | Require explicit manifests, sandbox policy, schemas, and audit for every capability. |
| Hot reload creates unsafe mixed generations. | Use generation switching, smoke checks, draining, and rollback. |

## Suggested Milestones

1. Define manifest and policy schemas.
2. Implement minimal Ingress, EventBus, Scheduler, and one Lua Agent.
3. Implement one controlled tool adapter with schema validation.
4. Add structured tracing, audit identity, and error classes.
5. Add private memory and ContextBuilder provenance.
6. Add generation switching for one Lua Agent.
7. Expand to MCP, skills, and hardware only after the closed loop is stable.
