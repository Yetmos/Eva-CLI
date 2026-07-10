# Eva-CLI V1.11.5 Alpha Release Notes

Status: alpha prerelease
Tag: `v1.11.5-alpha`

Eva-CLI V1.11.5-alpha completes the CLI runtime command completion slice. It
adds operator-facing command surfaces for typed event emission, Agent lifecycle
evidence, and capability provider routing while preserving the stable JSON
envelope, trace fields, exit-code mapping, and plan-first safety boundary.

## Highlights

- Adds `eva emit`, which constructs typed `eva-core::Event` values and
  publishes them to either the in-memory EventBus boundary or a V1.6 durable
  backend EventBus log.
- Adds `eva agent status`, `eva agent drain`, and `eva agent reload` over
  ProjectConfig manifests, `AgentRuntime`, `AgentLifecycle`,
  `DrainCoordinator`, and `GenerationController` evidence.
- Adds `eva capability list`, `eva capability probe`, and `eva capability call`
  over `CapabilityRegistry`, provider selection plans, permission gates,
  runtime policy decisions, `CapabilityRouter`, and
  `AdapterBackedCapabilityHost`.
- Keeps `capability call` dry-run by default. A call executes only when
  `--confirm <request-id>` matches the request id, and providers outside a
  capability manifest allowlist are rejected before invocation.
- Updates release metadata, release readiness docs, i18n manifest entries, and
  the static website content to the `1.11.5-alpha` checkpoint.

## Compatibility

V1.11.5-alpha is additive for public CLI commands, JSON envelopes, exit codes,
and existing release evidence gates. Existing command groups keep their output
shape. The new Agent commands are lifecycle evidence boundaries and do not
restart a daemon, mutate a live scheduler, or apply a real hot reload. Confirmed
capability calls remain controlled CLI invokes and still report
`mutation_executed:false`.

This is still an alpha checkpoint. Real database sink support, OS provider
supervision, real hardware access, production daemon-driven hot-reload
orchestration, signed installers, OS package repositories, and production
signing or attestation credentials remain future release scope. V1.15.8 adds
policy-driven memory redaction/audit JSONL evidence; V1.16.1 adds JSONL
best-effort runtime audit wiring; V1.16.2 adds a tracing subscriber bridge into
the existing JSONL/dev-console sinks; V1.16.3 adds explicit OpenTelemetry SDK
OTLP trace/metrics exporter smoke and collector-degraded reporting; V1.16.4
adds JSONL/durable-audit retention, rotation, max-size, and corrupt-record
policy plus a database policy kind boundary. These are not a complete
production telemetry backend with a real database sink or a production
retrieval scheduler. V1.17.1 adds the `run_command_module_split_v1.17.1`
runtime marker by moving `run --example basic` parser/runtime glue/task snapshot
writing/output code into `run/run_cmd.rs` without changing the public text or
JSON contract. V1.17.2 adds the `operator_execution_fields_v1.17.2` runtime
marker and exposes top-level `mutation_executed` on restore, upgrade, and
hardware operator outputs while preserving existing compatibility fields;
`capability call` continues to show `invocation_executed` separately from
`mutation_executed:false`. V1.17.3 adds the `operator_apply_text_v1.17.3`
runtime marker and text-only operator summaries for restore, upgrade, and
hardware high-risk paths. Production release upload remains future work because
it still needs signing/attestation and package repository credentials.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test -p eva-cli emit`
- `cargo test -p eva-cli agent`
- `cargo test -p eva-cli capability`
- `cargo test -p eva-cli run_basic_example_json_succeeds`
- `cargo test -p eva-cli task_`
- `cargo test -p eva-cli restore_apply`
- `cargo test -p eva-cli upgrade_apply`
- `cargo test -p eva-cli hardware_commands_report_candidates_and_bind_plan`
- `cargo test -p eva-release`
- `scripts/validate-i18n.ps1`
- `scripts/validate-version-management.ps1 -Tag v1.11.5-alpha`
- `cargo run -- --version`
- `cargo run -- release check --output json`
- `cargo run -- emit /input/user --payload hello --output json`
- `cargo run -- agent status --agent root-agent --output json`
- `cargo run -- capability list --output json`

## Artifacts

- GitHub Release source archives.
- Workflow `release-evidence-v1.11.5-alpha` artifact when the release workflow
  completes.
- GHCR container package: `ghcr.io/yetmos/eva-cli:1.11.5-alpha` when package
  publication succeeds.

Signed installers, OS package-manager packages, production provider
supervision, destructive apply paths, and provenance bundles remain future
release scope.
