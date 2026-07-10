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

This is still an alpha checkpoint. Tracing/OTel export, retention/rotation, OS
provider supervision, real hardware access, production daemon-driven hot-reload
orchestration, signed installers, OS package repositories, and production
signing or attestation credentials remain future release scope. V1.15.8 adds
policy-driven memory redaction/audit JSONL evidence; V1.16.1 only adds JSONL
best-effort runtime audit wiring. These are not a production telemetry backend
or production retrieval scheduler.

## Verification

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test -p eva-cli emit`
- `cargo test -p eva-cli agent`
- `cargo test -p eva-cli capability`
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
