# eva-core

`eva-core` 定义 Eva-CLI 各 crate 共享的无副作用基础契约。它保持零依赖，不执行 runtime 装配、文件系统、网络、shell、数据库、Lua、MCP、硬件或 provider 操作。

`eva-core` defines the dependency-free, side-effect-free contracts shared by the Eva-CLI workspace. Runtime orchestration and external I/O belong to downstream crates.

## Public Contracts

| Module | Main types | Boundary |
| --- | --- | --- |
| `topic` | `Topic`, `TopicPattern`, `TopicPatternSegment` | Validated absolute Topic paths and deterministic wildcard matching. |
| `ids` | `AgentId`, `AdapterId`, `CapabilityId`, `RequestId`, `EventId`, `GenerationId` | Distinct validated ID types prevent cross-domain mixups. |
| `capability` | `CapabilityName`, `CapabilityRef`, `ProviderHint` | Stable capability naming plus optional provider-selection data. |
| `event` | `Event`, `EventTarget`, `EventPayload`, `EventMetadata`, `TraceContext` | Event identity, target, payload, correlation, causation, request, and generation metadata. |
| `invoke` | `InvokeRequest`, `InvokeResponse`, `InvokeTarget`, `InvokeInput`, `InvokeOutput`, `InvokeStatus`, `InvokeMetadata` | Side-effect-free request and response envelopes for Agent, Capability, and Adapter invocation. |
| `error` | `EvaError`, `ErrorKind`, `ProviderCode`, `ErrorContext` | Stable machine-readable error categories, retry classification, provider codes, and non-sensitive context. |

Common types are re-exported from the crate root:

```rust
use eva_core::{
    AgentId, CapabilityName, EvaError, Event, InvokeRequest, Topic, TopicPattern,
};
```

## Contract Invariants

- A concrete `Topic` is an absolute path with at least one segment. It rejects whitespace, empty segments, trailing slashes, and wildcards.
- `TopicPattern` supports exact segments, `*` for one segment, and `**` only as the final segment for zero or more trailing segments.
- ID types share validation rules but remain distinct Rust types. Callers cannot accidentally substitute an `AdapterId` for an `AgentId`.
- Capability names use stable dot-separated segments. A `ProviderHint` is routing data only and never grants permission.
- Event payloads are `Empty`, `Text`, or `Bytes`; adding JSON or serialization dependencies requires an explicit cross-workspace contract decision.
- Invoke envelopes describe intent and outcomes. They do not execute an Agent, Capability, or Adapter.
- `Timeout` and `Unavailable` errors are retryable by default; callers may override retryability when a concrete provider/runtime contract requires it.
- Error messages and `ErrorContext` must not contain secrets, tokens, or large raw provider responses.

## Ownership

- `eva-core` owns shared data semantics and validation.
- `eva-policy` decides authorization.
- `eva-config` owns configuration loading and schema validation.
- `eva-eventbus`, `eva-scheduler`, and `eva-agent` own delivery and execution state.
- `eva-capability` and `eva-adapter` own capability routing and provider execution.
- `eva-runtime` composes those services; `eva-cli` exposes them to users.

Downstream crates may depend on `eva-core`; `eva-core` must not depend on runtime or integration crates.

## Verification

```powershell
cargo test -p eva-core
cargo doc -p eva-core --no-deps
cargo test --workspace
```

The source modules and rustdoc are the authority for individual methods. This README records only the stable package boundary and must not become an implementation task ledger.
