> Language: English
> Translation: [Simplified Chinese](../../zh-CN/architecture/eva-core模块设计.md)
> Translation status: current

# eva-core Contract Module

Updated: 2026-07-13

`eva-core` is the dependency-free contract foundation of the Eva-CLI Rust
workspace. It defines the values exchanged across configuration, EventBus,
Scheduler, Agent, Lua, capability, Adapter, storage, runtime, release, and CLI
boundaries. It performs no external I/O or runtime composition.

![eva-core contract boundary](../../assets/eva-core-contract-boundary.svg)

## 1. Implemented Boundary

| `eva-core` owns | `eva-core` does not own |
| --- | --- |
| Strongly typed Eva identifiers | ID allocation, registry lifecycle, persistence |
| Concrete Topic and TopicPattern parsing/matching | Subscription tables, load balancing, delivery |
| Event, target, payload and trace-link contracts | Event publication, replay, dead letters, handlers |
| Capability names/references and provider hints | Provider selection, authorization or invocation |
| Invoke request/response lifecycle values | Timeout enforcement, cancellation or transport execution |
| Structured cross-crate error values | Retry scheduling, logging, audit persistence or exit codes |

The crate has no dependencies in `Cargo.toml`; all current implementation uses
the Rust standard library and other `eva-core` modules.

## 2. Public Module Surface

| Module | Public contracts |
| --- | --- |
| `ids` | `AgentId`, `AdapterId`, `CapabilityId`, `RequestId`, `EventId`, `GenerationId` |
| `topic` | `Topic`, `TopicPattern`, `TopicPatternSegment` |
| `event` | `Event`, `EventMetadata`, `EventPayload`, `EventTarget`, `TraceContext` |
| `capability` | `CapabilityName`, `CapabilityRef`, `ProviderHint` |
| `invoke` | `InvokeInput`, `InvokeOutput`, `InvokeMetadata`, `InvokeRequest`, `InvokeResponse`, `InvokeStatus`, `InvokeTarget` |
| `error` | `ErrorKind`, `ProviderCode`, `ErrorContext`, `EvaError` |

These high-use types are re-exported from `eva_core` and form the stable import
surface for downstream crates.

## 3. Identifier Contracts

Each identifier is a distinct Rust newtype, so an `AgentId` cannot be passed as
an `AdapterId` accidentally. All six ID types share these validation rules:

- non-empty, with no leading or trailing whitespace;
- at most 128 bytes;
- no `/` or `\` path separator;
- ASCII letters, digits, `.`, `_`, `-`, and `:` only.

IDs implement ordering, hashing, display, `FromStr`, and `TryFrom<&str>`. They
do not allocate themselves and do not imply that a referenced object exists.

## 4. Topic Contracts

### 4.1 Concrete Topic

A `Topic` is an absolute slash-separated path such as `/input/user`. Parsing
rejects relative paths, empty segments, trailing slashes, whitespace, and any
wildcard character.

### 4.2 Topic Pattern

A `TopicPattern` uses the same path form and supports:

| Segment | Meaning |
| --- | --- |
| literal | exact segment match |
| `*` | exactly one segment |
| trailing `**` | zero or more remaining segments |

Wildcards must occupy a complete segment, and `**` is valid only as the final
segment. Matching is a pure linear contract operation. Route order, fanout,
compete selection, and mailbox delivery belong to `eva-scheduler`.

## 5. Event Contracts

An `Event` contains:

```text
Event
  event_id: EventId
  topic: Topic
  target: Broadcast | Agent | Capability | Adapter
  payload: Empty | Text | Bytes
  metadata:
    created_at: SystemTime
    request_id?: RequestId
    trace: correlation_id? + causation_id?
    generation_id?: GenerationId
```

The default target is `Broadcast`. `EventTarget` expresses intent only; it does
not route or execute the event. In the current Scheduler, only a direct Agent
target has special routing behavior.

`TraceContext::child_of` preserves an existing correlation root, or uses the
parent event as the new root, and records the parent as causation.
`Event::child_event` applies that linkage to a new event. The payload remains
opaque: schema interpretation belongs to configuration, Adapter, or caller
boundaries.

## 6. Capability Contracts

`CapabilityName` is a validated dotted name such as `repo.analyze` and exposes
its namespace and segments. `CapabilityRef` pairs a name with an optional
`ProviderHint`.

A provider hint is advisory contract data. `eva-core` does not resolve it,
check a manifest allowlist, grant permission, or invoke the named provider.
Those responsibilities belong to `eva-capability`, `eva-policy`, and
`eva-adapter`.

## 7. Invoke Contracts

`InvokeTarget` identifies an Agent, Capability, or Adapter. `InvokeInput` and
`InvokeOutput` reuse the opaque `EventPayload` representation.

```text
InvokeRequest
  request_id
  target
  input
  metadata: timeout? + trace + generation_id? + caller?

InvokeResponse
  request_id
  status: Accepted | Completed | Failed | Cancelled | Timeout
  output?
  error?
  metadata
```

`Completed`, `Failed`, `Cancelled`, and `Timeout` are terminal statuses;
`Accepted` is not. Constructors preserve a structured error for failure,
cancellation, and timeout paths.

The timeout field is a budget declaration. `eva-core` does not run a clock or
cancel work. Likewise, `caller` is correlation data, not an authenticated
principal. Runtime and provider boundaries must enforce both semantics.

## 8. Error Contract

`EvaError` carries:

```text
kind
message
retryable
provider_code?
context: ordered key/value entries
```

`ErrorKind` currently covers invalid argument, not found, conflict, permission
denied, timeout, unavailable, internal, and unsupported conditions. The
optional provider code preserves a provider-specific machine label without
making its protocol part of the shared enum.

`retryable` is classification data, not a command to retry. Scheduler, Agent,
Adapter, and CLI layers decide whether and how to retry. Trace persistence,
audit output, redaction, and process exit mapping also stay outside this crate.

## 9. Cross-Crate Invariants

- Downstream crates reuse these contracts instead of defining look-alike ID,
  Topic, Event, Invoke, or Error shapes.
- Constructors validate at the boundary; fields remain private where mutation
  could violate a contract.
- Core values contain no registry handles, file paths to execute, sockets,
  process objects, Lua values, MCP messages, or storage clients.
- Event and Invoke payloads remain opaque; `eva-core` never interprets business
  JSON or provider protocol schemas.
- Correlation, generation, target, provider hint, timeout, caller, and
  retryability metadata never grant permission or prove execution.
- Adding provider-specific state to a shared enum is avoided unless it becomes
  a stable cross-crate concept.

## 10. Runtime Handoff

| Consumer | Use of `eva-core` | Execution owned elsewhere |
| --- | --- | --- |
| `eva-config` | Parses IDs, Topics, patterns, capabilities | File/schema and cross-reference validation |
| `eva-eventbus` | Stores and returns Event contracts | Publish, ack, fail, replay |
| `eva-scheduler` | Matches TopicPattern and creates Agent deliveries | Mailboxes and route selection |
| `eva-agent` / `eva-lua-host` | Passes Event and structured results/errors | Queue, handler, sandbox and limits |
| `eva-capability` / `eva-adapter` | Builds Invoke values and provider references | Gates, routing, supervision and transports |
| `eva-storage` | Persists serialized representations of contracts | Layout, checksums, files and migration lock |
| `eva-runtime` / `eva-cli` | Correlates requests, reports and exit behavior | Composition, execution and presentation |

## 11. Content That Must Stay Out

- YAML and schema loaders;
- policy merging or permission grants;
- EventBus logs, dead-letter queues and Scheduler registries;
- Agent queues, Lua VM state or hot reload;
- Adapter manifests, provider selection, credentials or transport protocols;
- MCP JSON-RPC messages and sessions;
- memory, hardware, backup, lifecycle or release services;
- filesystem/network/database/process access;
- CLI JSON envelopes and exit-code policy.

## 12. Summary

`eva-core` is an implemented, side-effect-free vocabulary: six ID newtypes,
Topic matching, Event and trace linkage, capability references, Invoke
lifecycle values, and structured errors. Its most important invariant is that
metadata describes intent and correlation while execution authority remains in
the owning runtime crate.
