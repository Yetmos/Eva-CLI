# eva-memory / Memory And Knowledge

Updated: 2026-07-08

`eva-memory` owns Agent-private memory, shared global memory, project knowledge
indexing, durable memory/knowledge files, and request-scoped context assembly.
V1.9.4 adds a durable backend round-trip while preserving the original
invariant: private memory is scoped by `agent_id`, and context construction does
not grant service, provider, file, socket, or process handles.

## V1.9.4 Capability Matrix

| Area | Public Types | Behavior |
| --- | --- | --- |
| Private memory | `MemoryWrite::private`, `MemoryReadRequest::private`, `MemoryVisibility::Private` | Records are keyed by `agent_id`; another Agent receives `PermissionDenied` when requesting the owner scope. |
| Global memory | `MemoryWrite::global`, `MemoryReadRequest::global`, `MemoryVisibility::Global` | Shared facts are explicit records with versions and audit reason fields. |
| Versioning | `StateVersion` on `MemoryRecord` | Rewrites increment monotonically per `(visibility, owner, key)`. |
| TTL/expiration | `created_at_ms`, `expires_at_ms`, `snapshot_for_agent_at`, `ContextRequest::with_now_ms` | Expired records are omitted before context assembly. |
| Compression | `MemoryCompression::RunLength` | Durable files can store memory values with reversible run-length encoding while API reads return the original value. |
| Durable memory | `FileSystemMemoryStore` | Writes private/global memory under durable backend `state/memory` and can rebuild an in-memory service from files. |
| Knowledge index | `KnowledgeId`, `KnowledgeSource`, `KnowledgeItem`, `KnowledgeSearch` | Documents/snippets are indexed with lightweight digests, tags, source URI, summary, and ranked substring search. |
| Durable knowledge | `FileSystemKnowledgeStore`, `rebuild_from_items` | Knowledge files under durable backend `state/knowledge` can rebuild a searchable index. |
| Redaction | `redact_sensitive_text`, `ContextBuilder` | Context output redacts token/password/secret/API-key shaped values before Lua/context injection. |
| External retrieval gate | `ExternalKnowledgeRetrievalRequest` | External retrieval must pass a `RuntimePolicyGate` provider decision before any future provider call. |
| Context assembly | `ContextBudget`, `ContextRequest`, `ContextBuilder`, `BuiltContext` | Request context combines current Agent private memory, global memory, and knowledge results under explicit budgets. |
| Lua boundary | `LuaContextSnapshot` | Lua receives counts and audit summary only; it does not receive service handles or cross-Agent memory access. |

## Module Boundaries

`eva-memory` does:

- Define memory records, visibility, retention, request ids, versions, and audit reason fields.
- Enforce Agent-private reads by `agent_id` at the service API.
- Persist memory and knowledge files in the durable backend state directory.
- Filter expired memory and redact sensitive values before context assembly.
- Index traceable knowledge items with source URI, title, digest, summary, content, tags, and request id.
- Rebuild knowledge indexes from durable items.
- Build request-level context from private memory, global memory, and knowledge search results.
- Produce `LuaContextSnapshot` for downstream Lua host integration.

`eva-memory` does not:

- Store EventBus delivery logs or scheduler state.
- Grant Adapter, MCP, hardware, or filesystem authority.
- Execute retrieval against external services.
- Allow Lua or Agents to request another Agent's private memory.
- Run compaction/GC, cross-process locking, or production retrieval providers yet.

## Verification

```powershell
cargo test -p eva-memory
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
cargo run -- memory context --agent root-agent --query memory --durable-backend .eva/ci-memory --output json
```

The CLI smoke proves the public envelope includes `memory`, `global_memory`,
`knowledge`, `lua_context`, and `audit`. Durable smoke additionally proves
`state/memory` and `state/knowledge` round trips, expiration filtering,
compression metadata, and redaction.

## Next Scope

Next production work is provider-backed retrieval execution, background
compaction/GC, configurable redaction policy, cross-process index locking, and
audit sink wiring. Those features must preserve the invariant that private
memory is scoped by `agent_id` and request context is assembled through
`ContextBuilder`.
