# eva-memory / Memory And Knowledge

Updated: 2026-07-04

`eva-memory` is implemented for the V1.2 checkpoint. It owns Agent-private
memory, shared global memory, project knowledge indexing, and request-scoped
context assembly. It does not replace EventBus delivery, durable event logs, or
Adapter/MCP discovery. The current implementation is intentionally in-memory so
the privacy and context contracts can be tested before durable storage is added.

## V1.2 Capability Matrix

| Area | Public Types | V1.2 Behavior |
| --- | --- | --- |
| Private memory | `MemoryWrite::private`, `MemoryReadRequest::private`, `MemoryVisibility::Private` | Records are keyed by `agent_id`; another Agent receives `PermissionDenied` when requesting the owner scope. |
| Global memory | `MemoryWrite::global`, `MemoryReadRequest::global`, `MemoryVisibility::Global` | Shared facts are explicit records with versions and audit reason fields. |
| Versioning | `StateVersion` on `MemoryRecord` | Rewrites increment monotonically per `(visibility, owner, key)`. |
| Knowledge index | `KnowledgeId`, `KnowledgeSource`, `KnowledgeItem`, `KnowledgeSearch` | Documents/snippets are indexed with lightweight digests, tags, source URI, summary, and ranked substring search. |
| Context assembly | `ContextBudget`, `ContextRequest`, `ContextBuilder`, `BuiltContext` | Request context combines current Agent private memory, global memory, and knowledge results under explicit budgets. |
| Lua boundary | `LuaContextSnapshot` | Lua receives counts and audit summary only; it does not receive service handles or cross-Agent memory access. |

## Module Boundaries

`eva-memory` does:

- Define memory records, visibility, retention, request ids, versions, and audit reason fields.
- Enforce Agent-private reads by `agent_id` at the service API.
- Index traceable knowledge items with source URI, title, digest, summary, content, tags, and request id.
- Build request-level context from private memory, global memory, and knowledge search results.
- Produce `LuaContextSnapshot` for downstream Lua host integration.

`eva-memory` does not:

- Persist records beyond the current in-memory service instance.
- Store EventBus delivery logs or scheduler state.
- Grant Adapter, MCP, hardware, or filesystem authority.
- Execute retrieval against external services.
- Allow Lua or Agents to request another Agent's private memory.

## Verification

```powershell
cargo test -p eva-memory
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
```

The CLI smoke proves the public envelope includes `memory`, `global_memory`,
`knowledge`, `lua_context`, and `audit`. Unit tests cover private isolation,
global visibility, version increments, duplicate knowledge ids, tagged search,
context budgets, and leakage prevention.

## Next Scope

V1.5 hardening may add durable storage backends, expiration, redaction, index
rebuilds, sensitive-data scanning, and richer policy integration. Those future
features must preserve the V1.2 invariant that private memory is scoped by
`agent_id` and request context is assembled through `ContextBuilder`.
