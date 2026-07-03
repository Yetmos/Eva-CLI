> Language: English
> Detail authority: ../zh-CN/当前项目从零到完整实现实施计划.md
> Translation status: summary

# Full Implementation Plan

This page is the English entry for the full Eva-CLI implementation plan. The
current full-detail source is the Simplified Chinese document:
[当前项目从零到完整实现实施计划](../zh-CN/当前项目从零到完整实现实施计划.md).

The plan splits implementation into two delivery layers:

- **Core 1.0**: stabilize the executable CLI, configuration validation,
  EventBus, Scheduler, AgentRuntime, Lua host, controlled capability calls, and
  a minimal end-to-end example.
- **Complete 1.x**: add Adapter/MCP/Skill integration, discovery, memory and
  knowledge services, hardware hotplug, backup/snapshot services, supervisor
  lifecycle, release hardening, and cross-platform validation.

Current baseline as of 2026-07-03:

- The Rust workspace contains 19 crates.
- `eva-core` already implements the main data contracts for Topic, Event,
  Invoke, Capability, IDs, and structured errors.
- `eva-config` already loads `eva.yaml`, Agent manifests, Adapter manifests,
  Capability manifests, routes, policy documents, and project-level config.
- V0.3 is implemented: `eva-cli` provides `doctor`, `config validate`,
  `inspect`, structured text/JSON output, exit-code mapping, and a guarded
  `run` command that stops before the V0.4 event loop.
- V0.3 is implemented in `eva-runtime`: no-op `RuntimeBuilder`,
  `RuntimeSummary`, service summaries, and idempotent shutdown.
- Runtime execution crates such as EventBus, Scheduler, Agent, Lua host,
  capability execution, Adapter/MCP/Discovery, memory, hardware, backup, and
  lifecycle remain future implementation work.

Use the Chinese detailed plan for the module-by-module progress tables,
versioned iteration plan, implementation order, diagrams, and acceptance
criteria.
