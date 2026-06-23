# Zero to 1.0 Roadmap

Eva-CLI should move from architecture documents to a 1.0 release through small,
testable stages. The project should not jump directly from design documents to
a large runtime implementation.

Canonical source:
[docs/en/zero-to-one-roadmap.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/zero-to-one-roadmap.md)

## Current Position

The repository has completed most of the architecture and design-document stage
for the first implementation cycle. The next milestone is to create executable
structure and contracts, then prove a minimal end-to-end Runtime loop.

## Stages

| Stage | Goal | Primary artifacts |
| --- | --- | --- |
| 1. Architecture and design documents | Define product goals, boundaries, extension model, recovery strategy, and risks. | Architecture, runtime, adapter, memory, discovery, config, hardware, backup, upgrade, and risk documents. |
| 2. Module layout | Convert architecture into a concrete source tree. | `Cargo.toml`, Rust module layout, first example directory. |
| 3. Contract definitions | Make module boundaries executable and reviewable. | Rust traits/types, event types, manifest schemas, policy schemas, Lua host API contracts. |
| 4. Minimum runnable skeleton | Prove the repository can build and start. | `eva` entry point, `run`, `validate`, `doctor`, config loading, logging, CI tests. |
| 5. Minimum end-to-end Runtime loop | Validate one narrow real behavior path. | One user input, one Topic event, one Lua Agent, one controlled Rust tool, trace, audit, structured errors. |
| 6. Module implementation | Fill in real behavior one tested slice at a time. | Config, EventBus, Scheduler, Lua runtime, Tool layer, AdapterRegistry, memory, discovery, hot reload, MCP, Skills, hardware, backup. |
| 7. Integration and release preparation | Make the CLI installable, diagnosable, and release-ready. | Examples, quickstart, install docs, cross-platform CI, security review, release notes, migration guidance. |

## Minimum Runtime Loop

The first executable loop should prove:

1. user input enters Ingress;
2. Ingress publishes a typed Topic event;
3. Scheduler routes the event to one Agent queue;
4. Lua Agent handles the event in isolated state;
5. Lua Agent calls one controlled Rust tool;
6. Rust validates schema and policy before execution;
7. tool result returns to Lua as a structured value;
8. Runtime emits trace and audit data;
9. failure returns a structured, retry-aware error.

## 1.0 Release Bar

Eva-CLI 1.0 means the core promises are stable, not that every planned
capability is complete.

Required 1.0 properties:

- CLI installation and startup are reliable.
- The minimum Agent runtime loop is stable.
- Manifest, policy, and Lua host API contracts are versioned.
- Lua Agents can call controlled tools through Rust validation.
- Structured errors, trace, and audit are available.
- Config validation catches unsupported or unsafe setups early.
- Documentation, examples, and release artifacts match the implementation.
- Breaking changes have migration notes.
- Unsupported advanced capabilities are clearly marked.

## Immediate Next Work

The next practical implementation sequence is:

1. create the Rust project and module layout;
2. define the first contract set for manifests, events, policies, errors, and
   Lua host APIs;
3. build a minimum runnable skeleton;
4. implement the minimum end-to-end Runtime loop;
5. expand one module at a time with focused tests.
