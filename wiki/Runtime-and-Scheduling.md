# Runtime and Scheduling

The core runtime model combines a Rust host, isolated Lua Agent states, a Topic
EventBus, and a Scheduler that delivers typed events to private Agent queues.

Canonical sources:

- [docs/en/rust-lua-eventbus-scheduler.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/rust-lua-eventbus-scheduler.md)
- [docs/en/process-level-upgrade.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/process-level-upgrade.md)
- [docs/en/backup-migration-release-snapshot.md](https://github.com/Yetmos/Eva-CLI/blob/main/docs/en/backup-migration-release-snapshot.md)

## Ownership Split

| Area | Owner |
| --- | --- |
| Async runtime, lifecycle, isolation, timeout, retry, metrics, recovery | Rust |
| Intent recognition, local Agent transitions, tool orchestration, result mapping | Lua |
| Durable state, memory, configuration provenance, audit records | Rust-managed services |
| Cross-Agent coordination signal | Topic EventBus |

## Event Topics

Topics are route addresses such as:

```text
/input/user
/task/created
/sys/route-a/route-aa
/hardware/device/connected
```

Topic names route events. They are not a replacement for explicit state stores
or memory services.

## EventBus Modes

Eva-CLI design allows three deployment shapes:

- In-process best-effort EventBus for simple local execution.
- Recoverable in-process EventBus with durable event log and snapshots.
- External durable queue integration for distributed or long-running workloads.

## Scheduler Rules

The Scheduler must:

- match by Topic, target Agent, subscription rule, priority, load, and policy
- preserve per-Agent queue isolation
- apply backpressure and timeout handling at Rust boundaries
- record trace IDs, causality, retries, and audit fields
- keep failure and retry semantics explicit

## Lua Agent Runtime

Each Agent runs in an isolated Lua state with a private queue. Lua code can call
only the host APIs intentionally exposed by Rust, such as controlled tool,
memory, and context APIs.

Lua should handle:

- local task interpretation
- local state transitions
- sequencing allowed tool calls
- transforming validated results into Agent output

Lua should not handle:

- provider secrets
- shell execution
- arbitrary network or filesystem access
- host policy bypass
- durable global state ownership

## Hot Reload

Lua updates use generation switching:

1. Load a new generation.
2. Validate manifests, schemas, policy, and sandbox compatibility.
3. Run smoke checks.
4. Route new work to the new generation.
5. Drain in-flight work from the old generation.
6. Keep rollback available if activation or health checks fail.

## Process Recovery

Eva-CLI uses layered recovery:

- OS service manager starts the Supervisor.
- Supervisor owns Runtime lifecycle and replacement.
- Runtime reports health and can request controlled replacement.
- Durable Event Log and State Store preserve recoverable work.
- Runtime generation switching supports blue-green style activation.

Long-running tasks need explicit snapshot, reattach, timeout, idempotency, and
event replay contracts. Best-effort in-memory work must be labeled as
best-effort.

## Backup and Release Evidence

Backups, migration packages, and release snapshots are Runtime-owned safety
capabilities. Agents can request and explain these operations, but Runtime owns
scope resolution, locks, manifests, checksums, dry-run/apply separation,
restore, rollback, release pointer movement, and audit records.
