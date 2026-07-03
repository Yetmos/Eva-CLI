# Eva Basic Example

This example is the V1.0 core minimum runtime loop. It is a complete Eva workspace that can be loaded with:

```powershell
cargo run -- run --example basic --output json
```

## Runtime Path

1. `eva-cli` resolves `--example basic` to `examples/basic` and loads `config/eva.yaml`.
2. `eva-runtime` builds the V1.0 in-memory core services.
3. `eva-eventbus` publishes `/input/user` and appends the event to `eva-storage`.
4. `eva-scheduler` matches `/input/user` and delivers the event to `root-agent`.
5. `eva-agent` runs a bounded queue with timeout/cancel/retry controls.
6. `eva-lua-host` validates the controlled script contract and extracts the static `on_event` result.
7. The script requests `config.lint`, and `eva-capability` executes the builtin capability.
8. The CLI prints traceable text or JSON output with delivery, Agent, Lua, capability, task, logs, and audit fields.
9. The CLI writes the latest task report to `.eva/tasks` so `task status/logs/cancel` can inspect it.

## V1.0 Diagnostics

```powershell
cargo run -- run --example basic --task-id req-demo --output json
cargo run -- task status --task req-demo --output json
cargo run -- task logs --task req-demo --output json
cargo run -- task cancel --task req-demo --reason "manual check" --output json
```

Failure-path examples:

```powershell
cargo run -- run --example basic --task-id req-timeout --timeout-ms 0 --replay-dead-letters --output json
cargo run -- run --example basic --task-id req-cancel --cancel --output json
```

## Files

| Path | Purpose |
| --- | --- |
| `config/eva.yaml` | Minimal project root configuration and split config pointers. |
| `config/agents/root-agent/agent.yaml` | Root Agent manifest. |
| `config/agents/root-agent/main.lua` | Controlled V1.0 `on_event` script. |
| `config/routes/topics.yaml` | Routes `/input/user` to `root-agent`. |
| `config/capabilities/config-lint-skill.yaml` | Declares the `config.lint` capability used by the Lua result. |
| `config/capabilities/config-lint.lua` | Lua-shaped capability source kept as future handler reference. |

## Scope

The example intentionally uses only in-memory storage and builtin capabilities. It does not start adapters, MCP, hardware, memory, backup, supervisor services, a background task daemon, or a durable task database.
