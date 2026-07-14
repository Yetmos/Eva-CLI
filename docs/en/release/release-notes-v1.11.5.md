# Eva-CLI V1.11.5 Alpha Tag Notes

Language: English | [简体中文](../../zh-CN/release/V1.11.5-alpha发布说明.md)

> Historical snapshot pinned to `v1.11.5-alpha`. The tag exists, but its
> release workflow failed and no GitHub Release was published. Current `main`
> still reports `1.11.5-alpha` while containing later, untagged internal
> evidence; those post-tag changes are not part of this snapshot.

![Release history boundary](../../../assets/release-history-boundary.svg)

| Field | Recorded value |
| --- | --- |
| Date | 2026-07-09 |
| Status | Alpha tag; remote release incomplete |
| Tag | [`v1.11.5-alpha`](https://github.com/Yetmos/Eva-CLI/tree/v1.11.5-alpha) |
| Commit | [`9b86adf`](https://github.com/Yetmos/Eva-CLI/commit/9b86adfda121) |
| GitHub Release | None |
| Release workflow | [Run 28991416907](https://github.com/Yetmos/Eva-CLI/actions/runs/28991416907), failed |

The Windows and macOS verification jobs succeeded. The Ubuntu
`Workspace tests` step failed, so native archive builds, package publication,
and GitHub Release publication were skipped.

## Included In The Tag

- `eva emit` constructed typed events and published to the in-memory EventBus
  or a configured V1.6 durable EventBus log.
- `eva agent status`, `drain`, and `reload` exposed project/runtime lifecycle
  evidence through the in-process command boundary.
- `eva capability list`, `probe`, and `call` exposed registry, selection,
  permission, policy, and adapter-backed host paths.
- `capability call` remained dry-run unless `--confirm <request-id>` matched;
  providers outside the manifest allowlist were rejected.
- Version metadata and marker `cli_runtime_commands_v1.11.5` were pinned to
  `1.11.5-alpha`.

## Not Included

Agent commands did not restart a daemon, mutate a live scheduler, or apply hot
reload. Confirmed capability calls were controlled invocations and reported
`mutation_executed:false`. The tag did not include the later V1.12-V1.17
internal evidence, complete provider supervision, real hardware access,
production service-manager apply, signed packages, or release upload.

## Reproduce The Tag

```powershell
git switch --detach v1.11.5-alpha
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p eva-cli emit
cargo test -p eva-cli agent
cargo test -p eva-cli capability
./scripts/validate-version-management.ps1 -Tag v1.11.5-alpha
cargo run -- emit /input/user --payload hello --output json
cargo run -- agent status --agent root-agent --output json
cargo run -- capability list --output json
cargo run -- release check --output json
```

These commands reproduce the tagged source locally; they do not change the
recorded failure of the tag-triggered remote workflow.

## Artifacts

- No GitHub Release exists, so there is no Release page and there are **0**
  uploaded Release assets. GitHub can still synthesize repository archives for
  the tag outside a Release record.
- The failed workflow produced no release-evidence, package-evidence, or native
  archive Actions artifacts.
- GHCR has no `1.11.5-alpha` image.
- The annotated Git tag and commit remain the immutable source evidence.

## Current Documentation

For current `main` behavior, use the [user manual](../guide/user-manual.md),
[current capability gaps](../planning/v1.x-incomplete-feature-inventory.md),
and [implementation plan](../planning/v1.x-real-runtime-implementation-plan.md).
