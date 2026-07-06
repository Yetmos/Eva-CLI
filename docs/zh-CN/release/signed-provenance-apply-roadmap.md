# 签名安装器、Provenance 与真实 Apply 路径实施路线图

日期：2026-07-06
状态：进行中的实施计划
范围：GitHub Release 制品、供应链 provenance、原生安装器、durable runtime state、
真实 provider 执行，以及高风险 apply 命令。

本文档是 V1.5.1 GHCR package 检查点之后的实施台账。这个计划内的每一次功能修改，
都必须先更新下面的进度表，再提交。

## 当前基线

Eva-CLI V1.5.1 已经具备：

- 基于 tag 的 GitHub Release workflow，覆盖 Windows、macOS、Linux 三平台验证；
- `ghcr.io/yetmos/eva-cli` 的 GHCR package 发布；
- `release-evidence/package-ghcr.json`，记录 package digest、tags、source tag、
  source SHA、package URL 和 platform metadata；
- 源码安装文档、发布加固检查和 V1.5 兼容性规则；
- plan-first 的 backup、snapshot、restore 和 upgrade 命令。

Eva-CLI V1.5.1 还没有：

- 原生签名安装器或系统包管理器 package；
- 面向二进制或 release archive 的 SLSA/GitHub Artifact Attestation provenance；
- 在 release evidence 中记录并验证 GHCR SBOM/provenance attestation；
- durable runtime state、event log 或 artifact store；
- stdio/http/MCP 的真实 provider 进程执行；
- `restore apply`、`upgrade apply`、`snapshot promote` 等破坏性 apply 命令。

## 实施原则

- release provenance 先于原生安装器。
- 未签名 archive packaging 先于平台签名。
- durable state 先于破坏性 apply 命令。
- 每个 apply 命令必须 plan-first、可审计、幂等，并由可验证 artifact digest gate。
- Agent 和 Lua 代码不得直接移动 release pointer、执行 restore 或启动 provider 进程。
- GitHub Release workflow 在 evidence、签名或 attestation 验证失败时必须 fail closed。

## 阶段设计

### Phase 1：Provenance 与 Release Evidence

目标：在增加更多制品类型前，先让现有 GHCR 和源码 release 路径产出更强的机器可验证证据。

交付物：

- GitHub Actions OIDC 和 artifact attestations 权限。
- GHCR image 的 Docker Buildx SBOM/provenance 输出。
- `release-evidence/package-ghcr.json` 记录 provenance/SBOM 状态和验证命令。
- GitHub Release 正文展示 provenance 与 SBOM 可用性。
- 文档和官网同步新的 evidence 边界。

退出标准：

- `scripts/validate-version-management.ps1` 通过。
- 站点文案变更时，`scripts/build-site-i18n.ps1` 与 `scripts/validate-i18n.ps1` 通过。
- release workflow 语法保持可读、可审查。
- release evidence schema 对 V1.5.1 已有字段保持兼容。

### Phase 2：原生 Release Archives

目标：先发布跨平台命令行 archive，再进入平台签名。

交付物：

- 构建 Windows、macOS、Linux 的 `eva` release binary。
- 使用稳定命名打包 archive：
  - `eva-cli-<version>-x86_64-pc-windows-msvc.zip`
  - `eva-cli-<version>-x86_64-apple-darwin.tar.gz`
  - `eva-cli-<version>-aarch64-apple-darwin.tar.gz`
  - `eva-cli-<version>-x86_64-unknown-linux-gnu.tar.gz`
- 生成 `SHA256SUMS`。
- 捕获 `release-evidence/native-artifacts.json`。
- 上传制品到 GitHub Release。

退出标准：

- 每个 archive 包含 binary、README 或 install note、license metadata 和 checksum evidence。
- 每个 binary 在打包前运行 `eva --version`。
- GitHub Release 正文列出 archive 名称和 checksum 校验说明。

### Phase 3：签名安装器

目标：在 unsigned archives 可复现、可验证之后，再增加平台签名。

交付物：

- Windows：使用配置好的 signing provider 签名 `.exe` archive 或 installer。
- macOS：Developer ID 签名并 notarize tar/pkg 路径。
- Linux：先签名 checksum file，再在 package metadata 准备好后补 `.deb` / `.rpm`。
- `release-evidence/signing.json` 记录 signer identity、certificate metadata、
  timestamp、verification status 和 unsigned fallback status。

退出标准：

- 签名失败会阻断 signed artifact 发布。
- unsigned fallback 必须显式标注，不能伪装成 signed。
- 每个平台都有明确验证命令。

### Phase 4：Durable Stores

目标：在开放真实 apply 命令前，把 in-memory runtime evidence 替换为 durable interfaces。

交付物：

- 带 SHA-256 digest 的 `FilesystemArtifactStore` 或 `SqliteArtifactStore`。
- durable task state 和 event log interfaces。
- CLI flag 可选择 project-local durable stores。
- 保留当前 in-memory 测试路径。

退出标准：

- `backup create`、`snapshot create`、`restore plan` 可以读写 durable artifacts。
- 测试覆盖 digest mismatch、missing artifact 和 replay 边界。
- 既有 V1.5 JSON envelope 保持兼容。

### Phase 5：真实 Provider 进程执行

目标：允许受控 provider 执行，但暂不开放破坏性 runtime apply 路径。

交付物：

- stdio process runner，包含 allowlisted command、timeout、env scrubbing 和 output limit。
- HTTP provider runner，包含 URL allowlist、method restrictions、timeout 和 audit fields。
- MCP process/session 边界，显式 startup 和 shutdown。
- provider invocation audit 关联 trace fields。

退出标准：

- disabled provider 默认仍然 inert。
- 每次 provider execution 都返回结构化 success/failure 和 audit fields。
- 测试覆盖 timeout、denied command、oversized output 和 process failure。

### Phase 6：破坏性 Apply Gates

目标：只在 durable stores 和 provider execution boundary 被证明后，开放高风险 apply 命令。

交付物：

- `restore apply --plan <path> --confirm <plan_id>`。
- `upgrade apply --plan <path> --confirm <plan_id>`。
- `snapshot promote --snapshot-id <id> --confirm <snapshot_id>`。
- Runtime audit records 覆盖 lock acquisition、backup-before-apply、apply、
  rollback candidate 和 release pointer changes。

退出标准：

- apply 命令缺少 matching plan ID、artifact digest、backup evidence、policy approval
  或 explicit confirmation 时必须拒绝执行。
- 中断后的 apply 可以 inspect，并能安全 resume 或 rollback。
- Agent/Lua 仍只能 request；Runtime 拥有执行权。

## 详细进度表

状态含义：

- Planned：未开始。
- In Progress：当前正在处理。
- Done：已实现、已验证、已提交并推送。
- Blocked：缺少外部凭据、平台服务或产品决策，无法继续。

| ID | 功能修改 | 文件 / 范围 | 验收检查 | 状态 | 提交 |
| --- | --- | --- | --- | --- | --- |
| P0-001 | 创建本实施台账，并注册到 docs/site 索引 | `docs/en/release/signed-provenance-apply-roadmap.md`、`docs/zh-CN/release/signed-provenance-apply-roadmap.md`、`docs/_i18n/manifest.json`、docs/site indexes | `scripts/build-site-i18n.ps1`；`scripts/validate-i18n.ps1`；`scripts/validate-version-management.ps1` | Done | `0a3f6a6` |
| P1-001 | 为 GHCR release job 增加 OIDC/attestation 权限和 Buildx SBOM/provenance 设置 | `.github/workflows/release.yml` | workflow diff review；`scripts/validate-version-management.ps1` | Done | `1be7c44` |
| P1-002 | 扩展 `package-ghcr.json`，记录 provenance 和 SBOM 字段，同时保留旧字段 | `.github/workflows/release.yml`、release docs | `scripts/validate-version-management.ps1`；JSON 字段审查 | Done | `66416f7` |
| P1-003 | 更新 GitHub Release 正文，展示 GHCR provenance/SBOM 可用性 | `.github/workflows/release.yml`、release docs | Release body generation review | In Progress | pending |
| P1-004 | workflow 变更后同步官网/文档中的 provenance 状态 | `website/_i18n/*.json`、生成后的官网页面、docs index | `scripts/build-site-i18n.ps1`；`scripts/validate-i18n.ps1` | Planned | pending |
| P2-001 | 增加 native binary 的 release archive 命名和 manifest schema | release docs、`.github/workflows/release.yml` | workflow review；release evidence 字段审查 | Planned | pending |
| P2-002 | 构建并 smoke-test Windows release archive | `.github/workflows/release.yml` | packaged Windows artifact 内运行 `eva --version` | Planned | pending |
| P2-003 | 构建并 smoke-test Linux release archive | `.github/workflows/release.yml` | packaged Linux artifact 内运行 `eva --version` | Planned | pending |
| P2-004 | 构建并 smoke-test macOS x86_64 与 aarch64 release archives | `.github/workflows/release.yml` | packaged macOS artifacts 内运行 `eva --version` | Planned | pending |
| P2-005 | 生成 `SHA256SUMS` 和 `native-artifacts.json` release evidence | `.github/workflows/release.yml` | workflow 内执行 checksum verification command | Planned | pending |
| P3-001 | 定义 signing provider 配置和失败策略 | release docs、repository secrets documentation | 记录 secret names 和 fallback behavior | Planned | pending |
| P3-002 | 增加 Windows signing 路径 | `.github/workflows/release.yml` | signed artifact verification command | Blocked：需要 signing credential | pending |
| P3-003 | 增加 macOS signing 与 notarization 路径 | `.github/workflows/release.yml` | notarization verification command | Blocked：需要 Apple Developer credential | pending |
| P3-004 | 为 Linux archives 增加 signed checksum/provenance bundle | `.github/workflows/release.yml` | signature verification command | Planned | pending |
| P4-001 | 用 SHA-256 替换 lightweight artifact digest contract，同时保留旧测试意图 | `crates/eva-storage` | `cargo test -p eva-storage` | Planned | pending |
| P4-002 | 增加 filesystem artifact store 实现 | `crates/eva-storage` | artifact round trip、digest mismatch、missing artifact tests | Planned | pending |
| P4-003 | 通过显式 flag 把 durable artifact store 接入 backup/snapshot/restore 命令 | `crates/eva-cli`、`crates/eva-backup` | 使用 project-local artifact directory 的 CLI smoke | Planned | pending |
| P4-004 | 增加 durable event/task state interface | `crates/eva-storage`、`crates/eva-runtime`、`crates/eva-cli` | task status/logs 在测试中跨进程边界保留 | Planned | pending |
| P5-001 | 增加 stdio provider runner contract 和测试 | `crates/eva-adapter` | denied command、timeout、output limit tests | Planned | pending |
| P5-002 | 增加 HTTP provider runner contract 和测试 | `crates/eva-adapter` | URL allowlist、method denial、timeout tests | Planned | pending |
| P5-003 | 增加 MCP process/session startup boundary | `crates/eva-mcp`、`crates/eva-adapter` | startup failure 和 shutdown tests | Planned | pending |
| P5-004 | 将 provider invocation audit 关联 trace fields | `crates/eva-adapter`、`crates/eva-observability`、`crates/eva-cli` | CLI JSON 包含 trace/audit fields | Planned | pending |
| P6-001 | 增加 restore apply 命令解析，durable stores 不可用时拒绝执行 | `crates/eva-cli` | 命令返回 policy/runtime unavailable，并保持稳定 JSON | Planned | pending |
| P6-002 | 基于 durable artifacts 实现 restore apply dry-run validation | `crates/eva-backup`、`crates/eva-cli` | digest mismatch 和 missing backup tests | Planned | pending |
| P6-003 | 增加 upgrade apply 命令解析和 lock model | `crates/eva-lifecycle`、`crates/eva-cli` | lock acquisition 和 conflict tests | Planned | pending |
| P6-004 | 增加 snapshot promote 命令解析和 release pointer plan | `crates/eva-backup`、`crates/eva-lifecycle`、`crates/eva-cli` | confirmation 和 audit tests | Planned | pending |

## 每次修改的更新规则

对每一次功能修改：

1. 在实现提交前或实现提交中，把对应进度行从 Planned 改为 In Progress。
2. 只实现该行范围内的内容。
3. 运行该行验收检查，以及直接影响到的测试。
4. 提交产生后，把该行改为 Done，并填写 commit hash。
5. 推送到 GitHub 后，才能开始下一行。

如果某行因为外部凭据无法完成，把状态改为 Blocked，并记录缺少的凭据或服务。

## 提交纪律

本计划内每个提交都必须使用中文 intent line，并携带 Lore trailers。
`Tested:` trailer 必须列出实际运行过的命令。`Not-tested:` trailer 必须明确跳过的平台
或凭据依赖验证。

## 验证矩阵

| 修改类型 | 必要验证 |
| --- | --- |
| 仅 docs/site | `scripts/build-site-i18n.ps1`；`scripts/validate-i18n.ps1`；`scripts/validate-version-management.ps1` |
| 仅 release workflow | workflow diff review；`scripts/validate-version-management.ps1`；文案变化时运行 docs/site validation |
| Rust storage/runtime | `cargo fmt --check`；目标 crate tests；共享契约变化时扩大到 `cargo test --workspace` |
| CLI command surface | 目标 CLI smoke；JSON envelope review；`cargo test --workspace` |
| Apply path | plan/apply/rollback tests；policy denial tests；durable artifact tests；CLI smoke |

## 回滚策略

- release workflow 变更可以 revert，不移动 release tag。
- public tag 不能为了修复 release 而移动；必须通过 patch release 修复。
- apply 命令在所有必要 evidence 验证前不得修改状态。
- 任意 apply-path failure 都必须留下足够 durable audit data，用于定位最后一个成功步骤。
