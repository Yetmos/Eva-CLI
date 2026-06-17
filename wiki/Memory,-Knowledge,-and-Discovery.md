# Memory, Knowledge, and Discovery

Memory, knowledge, and discovery are intentionally separate responsibilities.
Discovery finds candidates. Policy authorizes execution. Memory and knowledge
services provide controlled context and durable facts.

Canonical sources:

- [docs/en/agent-memory-knowledge-base.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/agent-memory-knowledge-base.md)
- [docs/en/agent-discovery.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/agent-discovery.md)

## Memory Layers

| Layer | Responsibility |
| --- | --- |
| Agent private memory | Agent-scoped facts, preferences, and local continuity. |
| Global memory | Approved cross-Agent facts. |
| Knowledge base | Project documents, design records, and searchable references. |
| ContextBuilder | Policy-filtered context assembly for a specific invocation. |

## Ownership Rule

Rust is the source of truth for memory and knowledge services. Lua Agents can
read scoped context and write private memory through controlled APIs.

Global memory changes should be proposals that pass policy, audit, and optional
review before becoming shared facts.

## ContextBuilder Contract

ContextBuilder must:

- apply policy before retrieval
- filter by Agent identity, task scope, and data sensitivity
- return provenance for included facts
- bound token size and retrieval count
- record audit data for memory reads and writes

EventBus can announce memory changes, but it does not store memory. Durable
memory is committed through explicit stores that support snapshots, migrations,
and rollback-safe upgrades.

## Discovery Sources

Discovery can scan:

- project configuration and manifests
- user-approved local skill directories
- explicit MCP server configuration
- adapter manifests
- hardware manifests and device bindings
- built-in runtime capabilities

## Discovery Pipeline

1. Scan approved roots only.
2. Parse manifests with structured parsers.
3. Normalize IDs, versions, transports, schemas, and policies.
4. Validate against schemas.
5. Build a candidate registry.
6. Mark conflicts, disabled entries, and policy failures.
7. Publish only validated capabilities to runtime registries.

## Security Boundary

Discovery is not execution permission. A discovered capability still needs:

- manifest validation
- policy approval
- schema validation
- sandbox selection
- runtime audit

## Conflict Handling

Capability IDs must be stable. Conflicts should be deterministic and visible:
the registry records winning and rejected candidates, origin paths, versions,
and rejection reasons.
