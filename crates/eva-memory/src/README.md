# eva-memory/src

Updated: 2026-07-10

This directory contains Eva's memory, knowledge, durable persistence, redaction,
observability, and context contracts.

| File | Responsibility | Status |
| --- | --- | --- |
| `lib.rs` | Re-exports public memory, knowledge, durable store, maintenance reports, retrieval reports, redaction, observability, and context types. | V1.15.8 |
| `memory_service.rs` | Defines private/global memory records, writes, reads, retention, visibility, versions, TTL/expiration, compression metadata, Agent-private authorization, and observed write/read helpers. | V1.15.8 |
| `knowledge_service.rs` | Defines knowledge ids, source metadata, items, tags, ranked search, duplicate-id checks, lightweight digests, index rebuild, external retrieval provider gate decisions, supervised provider execution, schema parsing, policy-driven retrieval redaction, and retrieval reports. | V1.15.8 |
| `durable.rs` | Filesystem memory and knowledge stores under durable backend `state/memory` and `state/knowledge`, including `index.lock`, TTL GC, and rebuild checkpoints. | V1.15.6 |
| `redaction.rs` | Sensitive token/password/secret/API-key redaction before context injection, with policy-driven key and prefix rules. | V1.15.8 |
| `observability.rs` | Records memory write/read/search/context audit events and memory operation/redaction metrics. | V1.15.8 |
| `context_builder.rs` | Combines unexpired, redacted memory and knowledge into `BuiltContext` under `ContextBudget`, writes observed read/search/context evidence when requested, then projects a `LuaContextSnapshot`. | V1.15.8 |

## Local Contracts

- `MemoryVisibility::Private` requires an owner `AgentId` and can only be read by that same Agent.
- `MemoryVisibility::Global` has no private owner and is available to context assembly for any Agent.
- `ContextBuilder` is the path that joins memory with knowledge for request execution.
- Expired memory is filtered with an explicit `now_ms` reference before context output.
- Sensitive memory and knowledge text is redacted before entering `BuiltContext`.
- Redaction can be driven by `memory_policy.redaction` without exposing raw values to Lua snapshots.
- Observed memory write/read/search/context operations carry request/agent trace fields and metrics.
- Durable stores rebuild in-memory services before context use; they do not grant raw file handles.
- Durable memory/knowledge reads, writes, GC, and rebuild checkpoints acquire `index.lock` before touching index files.
- Interrupted maintenance can recover stale started checkpoints before re-running compaction.
- External retrieval invokes a supervised capability host only after policy allow, rejects invalid provider schema, redacts sensitive knowledge, and records source audit before indexing.
- `LuaContextSnapshot` deliberately carries counts and audit details, not service handles.

## Tests

```powershell
cargo test -p eva-memory
```

Coverage includes privacy isolation, global visibility, version increments,
TTL filtering, durable round trips, knowledge index rebuild, index-lock
conflicts, TTL compaction, stale maintenance checkpoint recovery, external
retrieval gate/provider execution decisions, provider failure/timeout/schema
rejection, policy-driven redaction, tagged search, memory operation audit/metrics,
budget truncation, and context construction without cross-Agent private memory
leakage.
