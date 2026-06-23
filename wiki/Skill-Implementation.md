# Skill Implementation

Skills are a controlled capability surface in Eva-CLI. They are not raw script
execution, not unrestricted `SKILL.md` interpretation, and not a Lua-owned
permission bypass.

Canonical source:
[docs/en/skill-implementation.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/skill-implementation.md)

## Design Position

Eva-CLI classifies Skills before registration:

| Skill class | Source | Runtime object | Default registration |
| --- | --- | --- | --- |
| `workflow_skill` | Codex or OMX `SKILL.md`, or explicit project Skill manifest | `SkillAdapter` | Register only with manifest, schemas, policy, and runtime gate approval. |
| `runtime_worker` | Team, swarm, ralph, worker-only runtime surfaces | Discovery metadata only | Display-only outside the matching runtime. |
| `lua_skill` | Project-local Lua workflow plus capability manifest | `LuaCapabilityAdapter` and `LuaCapabilityRuntime` | Register as a hot-reloadable `workflow.*` capability. |

This split keeps external workflow packages, runtime-only worker surfaces, and
project-owned Lua behavior under different authority models.

## Registration Pipeline

```text
AgentDiscoveryService
  -> DiscoveryNormalizer
  -> schema and policy validation
  -> RegistrationDecision
  -> AdapterRegistry / CapabilityRegistry
```

Discovery may find a Skill, but discovery does not authorize execution. Every
candidate must resolve to an explicit registration decision:

- `registered`
- `rejected`
- `display_only`
- `disabled`
- `shadowed`

The decision should include reason codes such as
`capability_missing_schema`, `runtime_gate_mismatch`, `policy_denied`,
`runtime_worker_display_only`, or `trust_level_unknown`.

## Manifest Requirements

External workflow Skills are represented as `transport: skill` adapter
manifests. Project-local Lua Skills are represented as `kind: lua_skill`
capability manifests.

Both lanes require:

- stable ID and version;
- declared capability names, usually under `workflow.*`;
- input and output schemas;
- runtime gate;
- declared permissions;
- timeout and concurrency limits;
- audit identity and manifest provenance.

Runtime must reject or downgrade Skills that cannot be classified, lack schemas,
request denied permissions, or require a runtime mode that is not active.

## Invocation Boundary

Lua Agents invoke Skills through capabilities, not by passing file paths,
command templates, shell snippets, environment variables, or arbitrary host
paths.

```text
ctx.tools.invoke_agent
  -> Rust Tool Layer
  -> caller permission check
  -> AdapterRouter
  -> SkillAdapter or LuaCapabilityAdapter
  -> input schema validation
  -> timeout and concurrency guard
  -> Skill execution
  -> output schema validation
  -> audit and metrics
  -> AgentInvokeResponse
```

Rust owns source path canonicalization, trust assignment, schema validation,
effective permissions, secrets, filesystem/network/shell/process boundaries,
timeouts, cancellation, rate limits, audit, metrics, tracing, and rollback.

## Hot Reload

`lua_skill` supports generation switching after manifest, schema, policy,
sandbox, Lua load, and health-check validation succeed. Failed reloads keep the
old generation active.

`workflow_skill` reload is discovery-driven. Permission expansion, transport
changes, and runtime gate expansion should require a runtime generation switch
instead of ordinary hot reload.

## Verification Focus

The first implementation should include tests for:

- trusted Skill registration;
- display-only behavior for unregistered local Skills;
- schema-required rejection;
- runtime gate mismatch;
- denied workspace writes;
- invalid path or payload escape attempts;
- deterministic routing when several providers expose the same capability;
- rollback when a Lua Skill reload fails;
- audit records for approved writes.

## Relationship to Other Pages

- [[Adapters and Capabilities]] owns the general adapter boundary.
- [[Memory, Knowledge, and Discovery]] owns candidate discovery and conflict
  handling.
- [[Runtime and Scheduling]] owns generation switching and Runtime lifecycle.
- [[Roadmap and Open Risks]] explains why Skill contracts must be
  machine-checkable before broad implementation.
