# Eva-CLI Wiki

Eva-CLI is an architecture-stage project for a Rust-managed, Lua-extensible,
Topic EventBus driven multi-Agent runtime. The repository currently contains
the design documents, website source, localization pipeline, and placeholder
source directories that will guide the first executable implementation.

This wiki is the project guide layer. It explains how to read the repository,
what is already decided, what remains open, and how future implementation work
should stay aligned with the canonical documents.

## Current Project Status

- Stage: architecture and specification consolidation.
- Implementation status: no finalized runnable CLI runtime yet.
- Canonical docs: English documents under `docs/en/`.
- Localized docs: Simplified Chinese documents under `docs/zh-CN/`.
- Website: static GitHub Pages site maintained under `website/`.
- Main architecture image: `assets/eva-cli-architecture.svg`.

## Core Idea

Eva-CLI separates authority from hot-reloadable behavior:

- Rust owns runtime boundaries, permissions, schemas, sandboxing, secrets,
  process lifecycle, audit, timeout handling, and recovery.
- Lua owns Agent behavior, local business state transitions, tool
  orchestration, and result mapping inside a narrow host API.
- Topic EventBus coordinates typed events between ingress, Scheduler, Agents,
  adapters, memory services, and hardware bridges.
- AdapterRegistry exposes external Agents, MCP servers, CLI tools, HTTP APIs,
  local models, skills, and hardware through controlled manifests and policy.

## Recommended Reading Path

1. [[Architecture Overview]]
2. [[Runtime and Scheduling]]
3. [[Adapters and Capabilities]]
4. [[Memory, Knowledge, and Discovery]]
5. [[Configuration and Localization]]
6. [[Roadmap and Open Risks]]
7. [[Contributor Guide]]

For full source-of-truth detail, read the canonical repository docs:

- [English documentation](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/README.md)
- [Simplified Chinese documentation](https://github.com/Yetmos/Eva-CLI/blob/main/docs/zh-CN/README.md)
- [Website source](https://github.com/Yetmos/Eva-CLI/blob/main/website/README.md)

## Non-Goals

- Eva-CLI is not trying to copy a centralized LangGraph-style global state
  machine.
- Lua must not directly execute shell commands, read secrets, scan arbitrary
  user directories, or open arbitrary network connections.
- Discovery is not authorization.
- EventBus must not become an implicit global business state store.
- MCP must not become an unrestricted proxy into the host machine.

## Repository Map

```text
Eva-CLI/
  src/                 # Future main program source
  crates/              # Future Rust workspace crates
  docs/                # Canonical architecture and implementation specs
  website/             # Static website source
  examples/            # Future examples and integration demos
  assets/              # Shared diagrams and visual assets
  scripts/             # Website/i18n build and validation scripts
  .github/workflows/   # GitHub Pages deployment workflow
```

## Maintenance Rule

When this wiki disagrees with `docs/en/`, treat `docs/en/` as canonical and
update the wiki. The wiki should summarize and route readers; it should not
fork architecture decisions into a second source of truth.
