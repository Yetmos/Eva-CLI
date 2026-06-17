# Adapters and Capabilities

Adapters and Lua capabilities are the controlled bridge between Agent behavior
and the outside world. Lua asks for a capability; Rust resolves, validates,
authorizes, executes, audits, and returns a structured result.

Canonical sources:

- [docs/en/lua-external-agent-adapter.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/lua-external-agent-adapter.md)
- [docs/en/lua-skill-mcp-tool-hot-reload.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/lua-skill-mcp-tool-hot-reload.md)
- [docs/en/hardware-hotplug.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/hardware-hotplug.md)

## Adapter Boundary

Lua calls a controlled Tool Layer. Rust resolves the call through
AdapterRegistry and AdapterRouter, then invokes the selected adapter under the
declared manifest, schema, transport, policy, timeout, and audit rules.

Supported adapter families include:

- external Agent adapters such as Claude, Codex, Gemini, and local models
- MCP server adapters
- Skill and workflow adapters
- CLI, HTTP, and internal service adapters
- hardware adapters

## Required Manifest Fields

Every adapter needs:

- stable adapter ID and version
- transport type and endpoint or command descriptor
- input and output schema
- permission policy and sandbox policy
- timeout, retry, concurrency, and rate-limit settings
- audit identity and ownership metadata

## Lua Capability Types

| Capability | Meaning |
| --- | --- |
| `lua_tool` | A reusable local tool implementation callable by Agents. |
| `lua_skill` | A workflow or domain skill implemented in Lua. |
| `lua_mcp_handler` | An MCP tool handler exposed by Eva-CLI as an MCP server. |

## Host-Owned Boundaries

Rust remains responsible for:

- manifest validation
- schema validation
- permission checks
- secret access
- filesystem and network policy
- MCP protocol handling
- audit events
- timeout and cancellation
- generation activation and rollback

Lua remains responsible for business intent, validation-friendly
transformations, and local orchestration inside the allowed host API.

## Hardware Boundary

Lua never accesses raw device handles, system device paths, or unchecked I/O.
Hardware integration is routed through:

- HardwareDiscoveryService
- DeviceRegistry
- DriverBinding
- HardwareAdapterRuntime
- Hardware EventBridge

Hardware manifests must declare matching rules, allowed operations, transport
type, raw I/O policy, rate limits, audit fields, and whether the device can be
used by Lua-facing capabilities.

## Failure Semantics

Adapter and hardware failures must return structured errors with:

- retryability
- origin
- audit ID
- redaction-safe diagnostics
- declared error class

Lua may branch on declared error classes, but it cannot bypass Rust policy
enforcement.
