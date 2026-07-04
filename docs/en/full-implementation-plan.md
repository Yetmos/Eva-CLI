> Language: English
> Detail authority: ../zh-CN/当前项目从零到完整实现实施计划.md
> Translation status: summary

# Full Implementation Plan

This page is the English entry for the full Eva-CLI implementation plan. The full-detail source remains the Simplified Chinese document: [当前项目从零到完整实现实施计划.md](../zh-CN/当前项目从零到完整实现实施计划.md).

The plan splits implementation into two delivery layers:

- **Core 1.0**: stabilize the executable CLI, configuration validation, EventBus, Scheduler, AgentRuntime, Lua host, controlled capability calls, and a minimal end-to-end example.
- **Complete 1.x**: add Adapter/MCP/Skill integration, discovery, memory and knowledge services, hardware hotplug, backup/snapshot services, supervisor lifecycle, release hardening, and cross-platform validation.

Current baseline as of 2026-07-04:

- The Rust workspace contains 19 crates.
- `eva-core` implements the main data contracts for Topic, Event, Invoke, Capability, IDs, and structured errors.
- `eva-config` loads `eva.yaml`, Agent manifests, Adapter manifests, Capability manifests, routes, policy documents, and project-level config.
- V0.3 is implemented: `eva-cli` provides `doctor`, `config validate`, `inspect`, structured text/JSON output, exit-code mapping, and no-op runtime inspection.
- V0.4 is implemented: `eva-storage`, `eva-eventbus`, `eva-scheduler`, `eva-agent`, `eva-lua-host`, and `eva-capability` provide the in-memory minimum runtime loop.
- V0.5 is implemented: `eva-agent` records timeout/cancel/retry attempts, `eva-eventbus` exposes in-memory dead-letter replay diagnostics, `eva-runtime` emits `TaskReport`, and `eva-cli` supports `task status/logs/cancel` over local `.eva/tasks` reports.
- V1.0 core is implemented: root/workspace version is `1.0.0`, `eva-cli` supports `eva --version` and `eva version --output json`, `eva-runtime` exposes `RuntimeBuilder::in_memory_v10()`, CI runs the release gates, and V1.0 quickstart/known limitations/release notes are documented.
- V1.1 external capability ecosystem is implemented: `eva-adapter` provides authorized handles, registry, router, probe, and controlled MCP/Skill envelopes; `eva-mcp` provides allowlist probes and an in-memory client surface; `eva-discovery` emits non-authorizing candidates; `eva-cli` exposes `adapter`, `mcp`, `skill`, and `discovery` commands.
- V1.2 memory and knowledge context is implemented: `eva-memory` provides in-memory private/global records, knowledge indexing, budgeted `ContextBuilder`, and `LuaContextSnapshot`; `eva-lua-host` accepts the controlled context snapshot; `eva-cli` exposes `memory context`.
- V1.3 controlled hardware access is implemented: root/workspace version is `1.3.0`, `eva-hardware` provides non-authorizing discovery candidates, stable device identities, `DeviceRegistry` leases, simulated driver binding, and hotplug state; `eva-adapter` exposes the hardware transport through that lease boundary; `eva-cli` exposes `hardware list/probe/bind` as plan-first diagnostics.
- V1.4 backup and lifecycle planning is implemented: root/workspace version is `1.4.0`, `eva-backup` provides backup artifact verification, migration preflight, release snapshots, and plan-first restore; `eva-lifecycle` provides generation handoff, drain planning, rollback planning, and in-memory supervisor readiness; `eva-cli` exposes `backup create`, `snapshot create`, `restore plan`, and `upgrade check`.
- `eva-runtime` now supports `RuntimeBuilder::in_memory_v10()` and `Runtime::run_basic`.
- `eva-cli` now supports `eva run --example basic` plus `eva task status`, `eva task logs`, and `eva task cancel` as the V1.0 core loop.
- `examples/basic/` is a complete minimal Eva workspace and exercises CLI -> EventBus -> Scheduler -> Agent -> controlled Lua host -> builtin capability -> task diagnostics.

Primary V1.0 verification commands:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -- run --example basic --timeout-ms 0 --replay-dead-letters --output json
cargo run -- run --example basic --cancel --output json
cargo test --workspace
cargo run -- --version
cargo run -- version --output json
cargo run -- run --example basic --output json
cargo run -- task status --output json
cargo run -- task logs --output json
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- mcp list --output json
cargo run -- mcp probe --adapter github-mcp --tool list_issues --output json
cargo run -- skill list --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- discovery scan --output json
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
cargo run -- backup create --output json
cargo run -- snapshot create --output json
cargo run -- restore plan --output json
cargo run -- upgrade check --output json
./scripts/build-site-i18n.ps1
./scripts/validate-i18n.ps1
```

V1.4 is complete as a source release checkpoint for controlled external capability visibility, diagnostics, request-scoped memory/knowledge context, non-authorizing hardware access planning, and plan-first backup/lifecycle operations. Real Lua VM sandboxing, durable task recovery, packaged installers, signed release artifacts, real stdio/http/MCP process execution, durable memory storage, real hardware I/O, destructive restore, and real supervisor process management remain later-version scope.

Use the Chinese detailed plan for the module-by-module progress tables, versioned iteration plan, implementation order, diagrams, and acceptance criteria.
