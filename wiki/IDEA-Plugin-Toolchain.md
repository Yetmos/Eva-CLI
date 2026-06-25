# IDEA Plugin Toolchain

The IDEA Plugin is the developer toolchain surface for Eva-CLI projects. It
helps authors edit Agents, manifests, Topic routes, policies, scenarios, Lua
scripts, and documentation without taking over Runtime authority.

Canonical sources:

- [docs/en/idea-plugin-toolchain.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/idea-plugin-toolchain.md)
- [docs/zh-CN/IDEA插件开发工具链功能方案.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/zh-CN/IDEA%E6%8F%92%E4%BB%B6%E5%BC%80%E5%8F%91%E5%B7%A5%E5%85%B7%E9%93%BE%E5%8A%9F%E8%83%BD%E6%96%B9%E6%A1%88.md)
- [Toolchain feature map](https://github.com/Yetmos/Eva-CLI/blob/main/docs/assets/idea-plugin-toolchain-feature-map.svg)

## Boundary

The plugin owns editor intelligence, project navigation, inspections, run
configurations, tool windows, and local developer feedback.

Eva-CLI Runtime still owns:

- policy and effective permissions
- secrets
- sandboxing
- external I/O
- Adapter execution
- audit traces
- durable state
- rollback and lifecycle control

Runtime Bridge connects the IDE to the local Runtime through typed,
versioned requests. It must not expose arbitrary shell execution, raw secret
reads, unrestricted file writes, or environment-variable reads.

## MVP Features

| Area | Required Support |
| --- | --- |
| Workspace detection | Detect `eva.yaml`, Agent manifests, Adapter manifests, Topic routes, schemas, docs, and scenario fixtures. |
| Manifest editing | Schema validation, completion, duplicate ID detection, workspace path validation, and permission preview. |
| Lua Agent editing | Syntax support, host API completion, Topic/capability references, unsafe API inspections, and scenario gutter actions. |
| Topic navigation | Topic completion, route declaration jumps, wildcard validation, and producer/subscriber graph. |
| Runtime Bridge | Health check, config validation, effective config inspection, capability snapshot, scenario dry run, Topic resolution, and trace opening. |
| Diagnostics | Inline errors, Problems view integration, quick fixes, stale cache warnings, and Runtime diagnostics. |

## Developer Workflows

| Workflow | Plugin Support | Evidence |
| --- | --- | --- |
| Create an Agent | Generate `agent.yaml`, `main.lua`, optional constraints, and scenario fixture. | Workspace index discovers the Agent and config validation passes. |
| Add a capability call | Complete known capability aliases and providers from manifests. | Inspection confirms the Agent has an allowed permission path. |
| Add a Topic route | Validate Topic syntax and show producers/subscribers. | Topic graph includes the new route and wildcard rules are valid. |
| Dry run a scenario | Send a typed bridge request with scenario ID and workspace context. | Tool window shows events, tool calls, denials, artifacts, and trace link. |
| Debug a policy denial | Link diagnostics to Agent permissions, Adapter policy, and global policy. | Developer can see which policy layer rejected the request. |

## Non-Goals

The plugin must not:

- execute arbitrary shell fragments for Lua or manifests
- bypass Runtime policy, sandbox, timeout, audit, or Adapter routing
- maintain a hidden registry that disagrees with Runtime
- treat discovered capabilities as authorized capabilities
- write IDE-local state into Runtime memory or knowledge stores
- rewrite production configuration without a visible file edit

## Verification

Plugin work should be covered by parser fixtures, index fixtures, inspection
tests, quick-fix tests, Runtime Bridge contract tests, run configuration tests,
and UI smoke tests with fixture projects.

The useful completion point is practical: a developer can create or edit an
Agent, validate config, dry run a scenario, inspect failures, and jump back to
the source file from inside IDEA.
