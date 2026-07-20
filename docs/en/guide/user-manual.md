# Eva-CLI User Manual

Last updated: 2026-07-20

Applies to: Eva-CLI `1.11.5-alpha` with V1.17.6 V1.x closure release gate

This manual is for developers, testers, and documentation maintainers using
Eva-CLI from source. V1.11.5-alpha is a source alpha checkpoint: the repository
builds, the CLI surface is executable, durable EventBus redrive evidence,
restart-readable task snapshots, durable audit records, artifact metadata
evidence, runtime recovery scanner/redrive/audit smoke, durable diagnostics,
restricted Lua VM `on_event` execution, Lua host observability,
`ctx.tools.call` capability binding, and Lua timeout/instruction/cancel/memory
execution limits, shadow-load health checks, generation route gating, drain
evidence, rollback audit evidence, release evidence gates, and CLI command
module split coverage, typed event emission, daemon control, daemon-backed Agent
drain/reload mutation, daemon runtime release gate, Agent lifecycle evidence,
host-bound service lifecycle commands, an identity-bound hidden direct service
entrypoint, and capability provider routing commands are in place. V1.17.6 adds the V1.x
closure release gate/report on top of the synchronized manuals, release notes,
website cards, generated localized HTML, and i18n manifest. High-risk mutation
paths remain plan-first and require explicit confirmation, policy, evidence,
lock, and health gates.

## Current Position

| Area | Current status |
| --- | --- |
| Release shape | Source-oriented alpha with Git tags/Releases, unsigned native CLI archives, GHCR packages for supported tags, checksums, and release evidence. Signed installers and OS package repositories are not available. |
| Runtime | `run --example basic` executes the V1.0 in-memory basic runtime loop through the restricted Lua VM, host binding, resource-limit, and hot-reload lifecycle boundary. |
| External capabilities | Declared stdio/http providers, MCP JSON-RPC tools, and Skill workflows can execute through controlled runners; Discovery remains candidate-only. Production OS process supervision is not available. |
| Risky actions | Hardware raw I/O remains blocked. Snapshot promotion, staged restore apply/rollback, and local release-pointer upgrade apply are implemented behind plan, confirmation, policy, evidence, lock, and health gates. |
| Service lifecycle | Windows Service/systemd/launchd adapters can install an identity-bound hidden direct daemon entrypoint. The code reuses daemon drain/shutdown for OS stop, but real-host stop/boot/reboot and destructive-harness evidence are not production-certified. |
| Release checks | V1.17.6 adds `REL-OBSERVABILITY-POLICY-001`, `REL-V1X-CLOSURE-001`, and additive `closure` JSON to `release check`. `release check/security/perf/migration` still cover Lua VM, daemon runtime readiness, release evidence, CLI split readiness, public JSON contract readiness, and runtime command completion evidence. |

![Eva-CLI source workflow](../../assets/eva-cli-user-manual-flow.svg)

## Prerequisites

| Dependency | Recommendation | Purpose |
| --- | --- | --- |
| Git | GitHub SSH or HTTPS access | Clone, commit, and push. |
| Rust toolchain | Stable Rust with `cargo` | Build the workspace, run the CLI, and run tests. |
| PowerShell or Bash | Either is fine | Run commands; PowerShell is the documented Windows shell. |
| Network access | Needed for push | Push commits to GitHub. |

Build and verify from source:

```powershell
git clone git@github.com:Yetmos/Eva-CLI.git
cd Eva-CLI
cargo build
cargo run -- --version
```

Expected version output includes:

```text
eva 1.11.5-alpha
release: V1.11.5-alpha
```

## Quick Start

Run this sequence from the repository root:

| Step | Command | Expected result |
| --- | --- | --- |
| Version | `cargo run -- --version` | Prints version, release label, and supported contracts. |
| Doctor | `cargo run -- doctor --output json` | Checks workspace roots, schema files, Lua host boundary, and runtime builder. |
| Config validation | `cargo run -- config validate --output json` | Loads `config/eva.yaml` and split manifests. |
| Inspect runtime | `cargo run -- inspect runtime --output json` | Prints agents, adapters, capabilities, routes, policy, and runtime summary. |
| Inspect durable | `cargo run -- inspect durable --durable-backend .eva/durable --output json` | Reports backend schema, migration status, and pending redrive count. |
| Run basic loop | `cargo run -- run --example basic --output json` | Executes the in-memory basic loop and writes `.eva/tasks` by default. |
| Emit event | `cargo run -- emit /input/user --payload hello --output json` | Publishes a typed Event to the in-memory EventBus boundary. |
| Daemon smoke | `cargo run -- daemon start --foreground --dev --output json` | Verifies local pid/lock/state, durable backend, provider/task recovery state, policy, observability, and shutdown contract without starting providers. |
| Agent status | `cargo run -- agent status --agent root-agent --output json` | Reports Agent lifecycle and manifest evidence. |
| Capability probe | `cargo run -- capability probe repo.analyze --output json` | Reports provider plan and permission gate evidence. |
| Task status | `cargo run -- task status --output json` | Reads the latest task report. |
| Release gate | `cargo run -- release check --output json` | Prints release readiness including `REL-DAEMON-RUNTIME-001`, `REL-OBSERVABILITY-POLICY-001`, `REL-JSON-CONTRACT-001`, `REL-V1X-CLOSURE-001`, and `closure`. |

`--version` / `version --output json` includes `mcp_http_auth_v1.13.6`,
`mcp_compat_matrix_v1.13.7`, and `provider_supervision_release_gate_v1.13.8`
when the runtime supports bounded MCP JSON-RPC over `http://` endpoints with
auth/session headers and has the repo-local MCP compatibility matrix plus
provider supervision release gates. It still does not mean OS provider process
supervision is enabled. V1.16.4 also adds
`observability_retention_policy_v1.16.4`, which means JSONL/durable-audit
retention and rotation policy is available; it does not mean a real database
sink is enabled. V1.17.1 adds `run_command_module_split_v1.17.1`, which means
`run --example basic` parser/runtime glue/output code lives in `run/run_cmd.rs`
while preserving the same public text/JSON contract. V1.17.2 adds
`operator_execution_fields_v1.17.2`, which fixes the operator-facing distinction
between invocation execution and destructive mutation execution. V1.17.3 adds
`operator_apply_text_v1.17.3`, which means restore, upgrade, and hardware
high-risk text output includes operator summaries with plan, target, final
state, rollback path, and risk evidence. V1.17.4 adds
`json_contract_diff_suite_v1.17.4`, `contracts/cli-json/*.json` fixtures, and
`scripts/validate-cli-json-contracts.ps1` to block removed or renamed public
JSON fields while allowing additive fields. V1.17.6 adds
`v1x_closure_gate_v1.17.6`, plus `REL-OBSERVABILITY-POLICY-001`,
`REL-V1X-CLOSURE-001`, and additive `release check` `closure` JSON; production
signing, package repository publication, destructive platform service-manager
stop/boot/reboot tests, real
hardware fixtures, and production database sink remain recorded external
blockers.

Use text output for human inspection and `--output json` for scripts or CI.

## Command Surface

| Command group | Common commands | Current purpose | External side effects |
| --- | --- | --- | --- |
| Version | `version`, `--version` | Print version, release label, and contracts. | No |
| Diagnostics | `doctor` | Check workspace, config roots, schema files, and runtime boundary. | No |
| Config | `config validate` | Validate `eva.yaml`, manifests, policies, routes, and schemas. | No |
| Inspect | `inspect`, `inspect durable` | Show project configuration, runtime summary, or durable backend diagnostics. | No |
| Runtime | `run --example basic` | Execute the V1.0 in-memory basic loop through the restricted Lua VM boundary. | Writes `.eva/tasks` or durable backend `tasks/` |
| Emit | `emit <topic>` | Publish a typed Event to in-memory or durable EventBus. | Writes durable backend `events/log/` when `--durable-backend` is set |
| Daemon | `daemon start/status/stop/shutdown/submit/cancel/drain/reload` | Verify the local daemon lease/PID/state, durable backend, provider/task recovery, policy, observability, shutdown contract, and filesystem mailbox control plane. | Writes daemon state/observability/control directories; the default smoke releases the lease and PID but retains the fixed lock anchor |
| Service | `service install/status/start/stop/restart/uninstall` | Apply the configured host-bound Windows Service/systemd/launchd lifecycle. Production definitions point at the hidden direct daemon entrypoint; Fake requires explicit `--dev`. | Mutates the host service manager for production kinds, or isolated `.eva/service-manager` development state for Fake |
| Agent | `agent status/drain/reload` | Report Agent lifecycle, drain plans, and generation reload evidence. | With a running daemon, `drain/reload` write daemon mutation state; without one they report `mutation_executed:false` |
| Capability | `capability list/probe/call` | Report provider routing and run dry-run or confirmed controlled invokes. | `call` defaults to dry-run; confirmed invokes report `invocation_executed` and keep `mutation_executed:false` |
| Task | `task status/logs/cancel` | Read or mark task diagnostics. | Writes task cancel marker |
| Adapter | `adapter list/probe` | List or probe manifest-derived adapter handles. | No |
| MCP | `mcp list/probe` | List or probe allowlisted MCP tools. | No |
| Skill | `skill list/run` | Run controlled workflow skill runners and write artifact evidence. | Manifest allowlisted runner only |
| Discovery | `discovery scan` | Scan trusted config sources for candidates. | No |
| Memory | `memory context` | Build request-scoped memory and knowledge context and write memory audit/metrics JSONL evidence. | No |
| Hardware | `hardware list/probe/bind` | Discover hardware manifests and produce binding plans. | No |
| Backup | `backup create` | Create and verify a backup artifact in memory or a filesystem artifact store. | Writes the selected artifact store when `--artifact-store` is set |
| Snapshot | `snapshot create/promote` | Create a release snapshot or confirm its release-pointer promotion plan. | Promotion is gated by the snapshot id and artifact evidence |
| Restore | `restore plan/apply/rollback` | Plan restore, apply staged file mutations, or reverse a rollback-required transaction. | Apply/rollback can mutate the target filesystem after all gates pass |
| Upgrade | `upgrade check/apply` | Check readiness or commit a local state-store handoff and release-pointer mutation. | `apply --state-store` writes local supervisor state after policy and health gates |
| Release | `release check/security/perf/migration` | Run release readiness, security, performance, and migration gates; `release check` includes daemon runtime, observability policy, public JSON contract, and V1.x closure readiness. | No |

## Service Manager Boundary

Do not invoke the hidden `daemon __service-entry` command manually. A production
`service install` builds its argument vector from the canonical executable and
project root, adds the configured service kind/name, and binds those values to
an identity digest. When the service manager starts it, that process directly
owns the daemon PID and lease; it does not launch another child.

Windows SCM controls and Unix service signals only set a cooperative stop token.
The daemon control loop turns the token into its existing generation-bound
`Shutdown` request, including task drain, stopped-state publication, PID cleanup,
and lease release. Unit and process tests cover this contract. They do not
replace controlled Windows SCM/systemd/launchd stop/boot/reboot transcripts,
mandatory cleanup in a destructive lifecycle harness, or a production release
gate. `upgrade apply` also remains a local modeled handoff and does not perform
real blue-green switching.

## Emit Typed Events

`emit` constructs an `eva-core::Event` and publishes it through the same
EventBus contract used by the runtime. Without `--durable-backend`, the event is
published to an in-memory bus and the command returns the publish receipt. With
`--durable-backend <path>`, the command opens the V1.6 durable backend and writes
the event record under `events/log/`.

```powershell
cargo run -- emit /input/user --event-id evt-manual-1 --payload hello --output json
cargo run -- emit /input/user --event-id evt-manual-2 --payload-bytes-hex 68656c6c6f --target-agent root-agent --durable-backend .eva/durable --output json
```

| Option | Default | Meaning |
| --- | --- | --- |
| `<topic>` or `--topic <topic>` | Required | Concrete event topic, such as `/input/user`. |
| `--event-id <id>` | Generated | Stable event id; generated ids use the `evt-cli-emit-*` prefix. |
| `--payload <text>` | Empty | UTF-8 text payload. JSON text is carried as text, not parsed. |
| `--payload-empty` | Empty | Explicit empty payload. |
| `--payload-bytes-hex <hex>` | Empty | Binary payload encoded as hex. |
| `--target-agent`, `--target-capability`, `--target-adapter` | Broadcast | Directed delivery target. |
| `--request-id`, `--generation`, `--correlation-id`, `--causation-id` | unset | Optional metadata carried on the event. |
| `--durable-backend <path>` | unset | Persist through the durable EventBus log. |

## Agent Lifecycle Evidence

`agent status`, `agent drain`, and `agent reload` expose the current Agent
manifest/runtime/lifecycle boundaries as operator evidence. They reuse
`AgentRuntime`, `AgentLifecycle`, `DrainCoordinator`, and `GenerationController`
to report lifecycle state, drain plans, and generation-swap evidence. When a
running daemon is available through the supplied state/lock/pid paths,
`drain/reload` send mailbox requests and write daemon-side mutation state.
Without a daemon they keep the evidence-only `mutation_executed:false` contract.
They still do not restart providers or apply a production hot reload.

```powershell
cargo run -- agent status --agent root-agent --output json
cargo run -- agent drain --agent root-agent --generation gen-v1115-agent --output json
cargo run -- agent reload --agent root-agent --from-generation gen-old --to-generation gen-new --from-release 1.11.4-alpha --to-release 1.11.5-alpha --output json
cargo run -- agent drain --agent root-agent --generation gen-old --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --durable-backend .eva/daemon-durable --output json
cargo run -- agent reload --agent root-agent --from-generation gen-old --to-generation gen-new --state-dir .eva/daemon-state --lock-dir .eva/daemon-locks --pid-dir .eva/daemon-pids --durable-backend .eva/daemon-durable --output json
```

| Command | Key fields | Meaning |
| --- | --- | --- |
| `agent status` | `lifecycle`, `queued_events`, `subscriptions` | Manifest-backed Agent snapshot; enabled Agents report a locally-started `running` runtime boundary. |
| `agent drain` | `drain.accepts_new_work:false`, `drain.status`, `mutation_executed` | Drain plan evidence; with a running daemon, writes `agent-control.state` and returns `true`. |
| `agent reload` | `active_generation`, `previous_generation`, `drain`, `audit`, `mutation_executed` | Generation promotion and old-generation drain evidence; with a running daemon, records new work routing to the target generation. |

## Capability Routing

`capability list`, `capability probe`, and `capability call` expose the provider
routing boundary used by Lua and Agent tool calls. The commands reuse
capability manifests, provider selection plans, permission gates, runtime policy
decisions, and the adapter-backed host. `capability call` defaults to dry-run;
to execute, pass `--confirm <request-id>` matching the request id. Provider
choices outside a capability manifest allowlist are rejected before invocation.

```powershell
cargo run -- capability list --output json
cargo run -- capability probe repo.analyze --output json
cargo run -- capability call config.lint --input config --request-id req-manual-cap --output json
cargo run -- capability call config.lint --input config --request-id req-manual-cap --confirm req-manual-cap --output json
```

| Command | Key fields | Meaning |
| --- | --- | --- |
| `capability list` | `capabilities[].providers`, `required_adapter_capabilities` | Manifest-derived capability registry and provider selection metadata. |
| `capability probe` | `provider_plan`, `providers`, `permission_gate` | Read-only provider route and adapter health evidence. |
| `capability call` | `status`, `confirmed`, `invocation_executed`, `mutation_executed`, `response` | Dry-run by default; confirmed calls execute through the builtin router or adapter-backed host without implying destructive mutation. |

## Basic Runtime Loop

`run --example basic` is the executable runtime loop in the current release. It
runs synchronously and writes the latest task report under `<project>/.eva/tasks`
by default. Passing `--durable-backend <path>` opens a V1.6 durable backend and
uses its `tasks/` directory instead, while preserving the same JSON envelope.

```powershell
cargo run -- run --example basic --output json
cargo run -- task status --output json
cargo run -- task logs --output json
cargo run -- run --example basic --task-id req-durable-1 --durable-backend .eva/durable --output json
cargo run -- task status --task req-durable-1 --durable-backend .eva/durable --output json
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--task-id <id>` | `req-basic-1` | Request/task id. |
| `--durable-backend <path>` | unset | Use the durable backend `tasks/` layout instead of `<project>/.eva/tasks`. |
| `--timeout-ms <ms>` | `30000` | Handler timeout budget; `0` exercises timeout diagnostics. |
| `--no-timeout` | Off | Removes the timeout budget. |
| `--retry-attempts <n>` | `1` | Retry limit. |
| `--cancel` | Off | Simulates cancellation before handler execution. |
| `--replay-dead-letters` | Off | Produces replay receipts for dead-letter events. |

## Controlled External Capability Execution

V1.8 and later commands expose adapter, MCP, skill, and discovery diagnostics
and allow manifest-gated stdio/http, MCP JSON-RPC, and Skill workflow runners
to enter controlled execution paths. They still do not start undeclared external
servers, provider CLIs, or workflow runners.

| Scenario | Command |
| --- | --- |
| List adapters | `cargo run -- adapter list --output json` |
| Probe adapter | `cargo run -- adapter probe --adapter github-mcp --output json` |
| Probe by capability | `cargo run -- adapter probe --capability repo.issue.list --output json` |
| List MCP allowlist | `cargo run -- mcp list --output json` |
| Probe MCP tool | `cargo run -- mcp probe --adapter github-mcp --tool list_issues --output json` |
| List skills | `cargo run -- skill list --output json` |
| Run controlled skill workflow | `cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json` |
| Scan discovery candidates | `cargo run -- discovery scan --output json` |

## Plan-First Safety Boundary

![Eva-CLI safety boundary](../../assets/eva-cli-user-manual-safety.svg)

| Scenario | Command | Current boundary |
| --- | --- | --- |
| Hardware candidates | `cargo run -- hardware list --output json` | Reads manifests; does not open devices. |
| Hardware probe | `cargo run -- hardware probe --adapter scale-main --output json` | Reports health, trust, and handle status. |
| Hardware bind plan | `cargo run -- hardware bind --adapter scale-main --output json` | Produces plan steps, risks, `mutation_executed:false`, and text operator summary; no raw I/O handle. |
| Backup artifact | `cargo run -- backup create --output json` | Uses an in-memory artifact store. |
| Release snapshot | `cargo run -- snapshot create --output json` | Links to a verified backup manifest. |
| Restore plan | `cargo run -- restore plan --output json` | Returns `apply_allowed:false` and `mutation_executed:false`. |
| Upgrade readiness | `cargo run -- upgrade check --output json` | Reports migration, drain, rollback readiness, and `mutation_executed:false`. |

Confirmed mutation commands are available for prepared plans:

| Scenario | Command shape | Current boundary |
| --- | --- | --- |
| Snapshot promotion | `snapshot promote --snapshot-id <id> --confirm <id> --artifact-store <path>` | Confirms the snapshot promotion plan; does not perform production service-manager handoff. |
| Restore apply | `restore apply --plan <path> --confirm <plan-id> --artifact-store <path> --lock-store <path>` | Executes staged copy/delete/replace steps only after artifact, policy, lock, health, and confirmation gates; writes a transaction log and can report `rollback_required`. |
| Restore rollback | `restore rollback --plan <path> --confirm <plan-id> --artifact-store <path> --lock-store <path>` | Reverses committed staged steps from signed pre-restore evidence after drift and transaction-log checks. |
| Upgrade apply | `upgrade apply --plan <path> --confirm <plan-id> --lock-store <path> --state-store <path>` | Can commit local handoff state and release-pointer mutation; platform service-manager activation remains external work. |

## Release Gates

```powershell
cargo run -- release check --output json
./scripts/validate-cli-json-contracts.ps1
cargo run -- release security --output json
cargo run -- release perf --output json
cargo run -- release migration --output json
```

| Command | Focus |
| --- | --- |
| `release check` | Cross-platform, stability, docs, security, performance, migration, compatibility, daemon runtime, hardware safety, observability policy, JSON contract, and V1.x closure gates. |
| `release security` | Policy, Lua sandbox, secret redaction, MCP allowlist, hardware, and lifecycle risks. |
| `release perf` | EventBus, Scheduler, Adapter, memory, backup, and release-check budgets. |
| `release migration` | V1.5.1 to V1.11.5-alpha migration steps and compatibility policy. |

## Paths

| Path | Purpose |
| --- | --- |
| `Cargo.toml` | Root package and workspace members. |
| `crates/eva-cli/` | CLI parsing, output envelope, and exit-code mapping. |
| `config/eva.yaml` | Project root configuration and runtime settings. |
| `config/agents/` | Agent manifests and Lua scripts. |
| `config/adapters/` | Adapter manifests. |
| `config/capabilities/` | Capability manifests and Lua capability examples. |
| `config/policies/` | Sandbox, MCP, hardware, and adapter policies. |
| `config/routes/topics.yaml` | Topic routes. |
| `config/schemas/` | JSON schemas. |
| `.eva/tasks/` | Default local task diagnostics written by `run --example basic`; not committed. |
| Durable backend `tasks/` | Optional task snapshot store used when `--durable-backend <path>` is provided. |

## JSON Envelope and Exit Codes

Successful JSON output uses `ok`, `command`, `exit_code`, `data`, and `trace`.
Error JSON output uses `ok`, `command`, `exit_code`, `error`, and `trace`.

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Internal error. |
| `2` | Configuration, path, manifest, route, schema, or task state issue. |
| `3` | Policy denied. |
| `4` | Runtime unavailable or capability not implemented in this release. |
| `5` | External capability unavailable. |
| `64` | Command usage error. |

## Non-Goals in V1.11.5 Alpha

V1.11.5-alpha does not provide signed installers, production signing or
attestation credentials, Homebrew/Winget/Apt publication, production-certified
service-manager stop/boot/reboot evidence, a destructive lifecycle harness,
real blue-green handoff, OS provider process supervision or credential
vault isolation, production MCP streaming/TLS certification, raw hardware I/O,
real hardware fixtures, a production observability database sink/retention
scheduler, or long-lived production memory/retrieval scheduling.

## Recommended Verification

```powershell
cargo test -p eva-cli
cargo run -- --version
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- run --example basic --output json
cargo run -- release check --output json
.\scripts\build-site-i18n.ps1
.\scripts\validate-i18n.ps1
```
