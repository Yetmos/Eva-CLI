# eva-memory/src

Updated: 2026-07-04

This directory contains the V1.2 in-memory implementation of Eva's memory,
knowledge, and context contracts.

| File | Responsibility | V1.2 Status |
| --- | --- | --- |
| `lib.rs` | Re-exports the public V1.2 memory, knowledge, and context types. | Implemented |
| `memory_service.rs` | Defines private/global memory records, writes, reads, retention, visibility, versions, and Agent-private authorization. | Implemented |
| `knowledge_service.rs` | Defines knowledge ids, source metadata, items, tags, ranked search, duplicate-id checks, and lightweight digests. | Implemented |
| `context_builder.rs` | Combines memory and knowledge into `BuiltContext` under `ContextBudget`, then projects a `LuaContextSnapshot`. | Implemented |

## Local Contracts

- `MemoryVisibility::Private` requires an owner `AgentId` and can only be read by that same Agent.
- `MemoryVisibility::Global` has no private owner and is available to context assembly for any Agent.
- `ContextBuilder` is the only V1.2 path that joins memory with knowledge for request execution.
- `LuaContextSnapshot` deliberately carries counts and audit details, not service handles.

## Tests

```powershell
cargo test -p eva-memory
```

Coverage includes privacy isolation, global visibility, version increments,
knowledge duplicate rejection, tagged search, budget truncation, and context
construction without cross-Agent private memory leakage.
