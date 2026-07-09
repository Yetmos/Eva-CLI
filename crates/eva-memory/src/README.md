# eva-memory/src

Updated: 2026-07-10

This directory contains Eva's memory, knowledge, durable persistence, redaction,
and context contracts.

| File | Responsibility | Status |
| --- | --- | --- |
| `lib.rs` | Re-exports public memory, knowledge, durable store, maintenance reports, redaction, and context types. | V1.15.6 |
| `memory_service.rs` | Defines private/global memory records, writes, reads, retention, visibility, versions, TTL/expiration, compression metadata, and Agent-private authorization. | V1.9.4 |
| `knowledge_service.rs` | Defines knowledge ids, source metadata, items, tags, ranked search, duplicate-id checks, lightweight digests, index rebuild, and external retrieval provider gate decisions. | V1.9.4 |
| `durable.rs` | Filesystem memory and knowledge stores under durable backend `state/memory` and `state/knowledge`, including `index.lock`, TTL GC, and rebuild checkpoints. | V1.15.6 |
| `redaction.rs` | Sensitive token/password/secret/API-key redaction before context injection. | V1.9.4 |
| `context_builder.rs` | Combines unexpired, redacted memory and knowledge into `BuiltContext` under `ContextBudget`, then projects a `LuaContextSnapshot`. | V1.9.4 |

## Local Contracts

- `MemoryVisibility::Private` requires an owner `AgentId` and can only be read by that same Agent.
- `MemoryVisibility::Global` has no private owner and is available to context assembly for any Agent.
- `ContextBuilder` is the path that joins memory with knowledge for request execution.
- Expired memory is filtered with an explicit `now_ms` reference before context output.
- Sensitive memory and knowledge text is redacted before entering `BuiltContext`.
- Durable stores rebuild in-memory services before context use; they do not grant raw file handles.
- Durable memory/knowledge reads, writes, GC, and rebuild checkpoints acquire `index.lock` before touching index files.
- Interrupted maintenance can recover stale started checkpoints before re-running compaction.
- `LuaContextSnapshot` deliberately carries counts and audit details, not service handles.

## Tests

```powershell
cargo test -p eva-memory
```

Coverage includes privacy isolation, global visibility, version increments,
TTL filtering, durable round trips, knowledge index rebuild, index-lock
conflicts, TTL compaction, stale maintenance checkpoint recovery, external
retrieval gate decisions, redaction, tagged search, budget truncation, and
context construction without cross-Agent private memory leakage.
