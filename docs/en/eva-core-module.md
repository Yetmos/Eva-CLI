> Language: English
> Translation: [简体中文](../zh-CN/eva-core模块设计.md)
> Translation status: current

# eva-core Module Design

Updated: 2026-06-30

`eva-core` is the foundational contract module in the Eva-CLI Rust workspace. It defines stable shared data structures so EventBus, Scheduler, AgentRuntime, Adapter, Capability, Runtime, and CLI crates use the same event, Topic, ID, invocation, and error model.

![eva-core contract boundary](../assets/eva-core-contract-boundary.svg)

## 1. Purpose

`eva-core` defines the system's shared language. It should not execute side effects. Keep it free of I/O, runtime task wiring, provider-private protocols, and implicit global state.

| Design Goal | Description |
| --- | --- |
| Stable contracts | Pin down data types that travel across crate APIs. |
| Low coupling | Downstream modules depend on `eva-core`; `eva-core` does not depend on runtime modules. |
| No side effects | Do not access files, network, databases, shell, Lua, MCP, or hardware directly. |
| Strong boundaries | Use newtypes to separate Agent IDs, Adapter IDs, Capability names, Topics, and Request IDs. |
| Testable | Validation, parsing, and matching logic can be covered by pure unit tests. |

## 2. Responsibility Boundary

| Area | `eva-core` Owns | Does Not Own |
| --- | --- | --- |
| Event | Event body, Topic, target, payload, timestamps, trace linkage fields | Broadcast, persistence, replay, dead-letter storage |
| Topic | Topic names, Topic patterns, wildcard rules, validation errors | Subscription registry, delivery policy, Agent mailboxes |
| ID | Stable ID newtypes such as `AgentId`, `AdapterId`, `CapabilityName`, `RequestId`, `EventId` | ID allocation strategy or registry lifecycle |
| Invoke | Agent, Capability, and Adapter request/response contracts | Executing Lua, HTTP, stdio, MCP, or hardware |
| Error | Structured cross-crate errors and error categories | Log persistence, audit output, retry scheduling |

## 3. Submodule Plan

| Submodule | Planned Content | Main Downstream Users |
| --- | --- | --- |
| `ids` | Stable ID newtypes, parsing, display, serialization | All runtime crates |
| `topic` | `Topic`, `TopicPattern`, wildcard matching, format validation | `eva-scheduler`, `eva-policy`, `eva-config` |
| `event` | `Event`, `EventTarget`, payload, correlation/causation fields | `eva-eventbus`, `eva-scheduler`, `eva-agent` |
| `capability` | `CapabilityName` and provider-selection primitives | `eva-capability`, `eva-adapter` |
| `invoke` | Agent/Capability request, response, status enums | `eva-agent`, `eva-runtime`, `eva-cli` |
| `error` | `EvaError`, `ErrorKind`, retryable, provider code | All cross-module boundaries |

## 4. Recommended Minimum Delivery

The first implementation pass should avoid implementing every business object. Lock down the smallest contracts that downstream modules need.

| Priority | Contract | Acceptance Standard |
| --- | --- | --- |
| 1 | Topic | Parses `/input/user` and `/sys/route-a`; rejects empty segments and invalid prefixes. |
| 2 | TopicPattern | Supports exact, `*`, and `**`; guarantees `**` only appears as the last segment. |
| 3 | ID newtypes | `AgentId`, `AdapterId`, `RequestId`, and related types cannot be mixed accidentally. |
| 4 | Event | Events carry `event_id`, `topic`, payload, and optional trace linkage fields. |
| 5 | Error | Errors include kind, message, retryable, and optional provider code. |

## 5. Dependency Rules

```text
eva-core
  <- eva-config
  <- eva-policy
  <- eva-eventbus
  <- eva-scheduler
  <- eva-agent
  <- eva-capability
  <- eva-adapter
  <- eva-runtime
  <- eva-cli
```

Constraints:

- `eva-core` must not depend on other Eva runtime crates.
- `eva-core` may depend on small stable general-purpose libraries, but each dependency must serve contract expression, such as serialization, time, or error derivation.
- Do not implement providers, transports, registries, runtime builders, or CLI commands in `eva-core`.
- Do not read `config/` from `eva-core`; only define foundational types that parsed configuration may reference.

## 6. Runtime Relationship

| Runtime Node | How It Uses `eva-core` |
| --- | --- |
| Ingress | Builds valid `Event` values or invoke requests. |
| EventBus | Publishes, records, and recovers `Event` values. |
| Scheduler | Uses `Topic` / `TopicPattern` to select target Agents. |
| AgentRuntime | Hands events to Lua and receives structured responses or errors. |
| CapabilityRouter | Uses `CapabilityName` and invoke requests to select providers. |
| AdapterRuntime | Returns unified responses or `EvaError`. |
| CLI | Emits stable inspect, emit, and validate output. |

## 7. Content That Should Stay Out

| Should Not Go Here | Owning Module |
| --- | --- |
| YAML/JSON Schema loading | `eva-config` |
| Permission merge and effective policy | `eva-policy` |
| Event log and dead-letter queue | `eva-eventbus` / `eva-storage` |
| Lua State, sandbox, and bindings | `eva-lua-host` |
| Adapter manifest and transport runtime | `eva-adapter` |
| MCP server/client protocol details | `eva-mcp` |
| Runtime generation, drain, rollback | `eva-lifecycle` / `eva-runtime` |

## 8. Summary

`eva-core` should first implement five foundational contracts: Topic, ID, Event, Invoke, and Error. The more stable this crate is, the less rework later modules such as `eva-config`, `eva-eventbus`, `eva-scheduler`, `eva-agent`, and `eva-adapter` will need.

