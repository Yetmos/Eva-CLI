# 备份、迁移包与 Release Snapshot 架构方案

> Language: 简体中文
> Canonical source: ../en/backup-migration-release-snapshot.md
> Translation status: current

更新日期：2026-06-17

## 1. 文档定位

本文定义 Eva-CLI 中备份、迁移包和 release snapshot 应该由 Agent 实现，
还是由 Runtime 实现。

核心结论是：**Runtime 负责可信执行层，Agent 只负责编排、解释、请求和
结果总结**。

备份、迁移包和 release snapshot 都是高副作用能力。它们可能覆盖文件、
改变持久化状态、移动 release pointer，或者影响回滚边界。因此它们必须
具备确定性、可重复性、可审计性和可恢复性，不能把 LLM prompt 当作文件
边界、覆盖策略、schema 兼容性或回滚逻辑的正确性来源。

## 2. 范围

本文覆盖三类相关操作：

| 操作 | Runtime 职责 | Agent 职责 |
| --- | --- | --- |
| 备份 | 创建、校验、列出、保留、恢复和审计可恢复 artifact。 | 建议范围、解释影响、请求备份、总结结果。 |
| 迁移包 | 构建、导入、校验、checksum/signature、dry-run、apply、拒绝和审计版本化迁移 artifact。 | 生成说明、把用户意图转换成 Runtime 请求、解释兼容性警告、分析失败日志。 |
| Release snapshot | 记录发布前后状态、比较 snapshot、关联健康检查证据、支撑回滚判断。 | 准备 release note、解释 snapshot diff、请求 Runtime 受控 snapshot 操作。 |

Runtime 负责 artifact 和状态转换。Agent 负责理解意图和面向人的解释。

## 3. 核心决策

Eva-CLI 应该把备份、迁移包和 release snapshot 作为 Runtime service：

```text
Agent
  -> 提出意图
  -> 调用受控 Runtime API
  -> 解释结果

Runtime
  -> 校验 policy 和 schema
  -> 获取锁
  -> 生成 manifest 和 checksum
  -> 区分 dry-run 与 apply
  -> 写入 audit 记录
  -> 拥有 restore 与 rollback 语义
```

Agent 不应该通过复制任意路径、创建临时 archive、编辑 release pointer、
或绕开 Runtime policy 恢复持久化状态来实现这些可信部分。

## 4. 职责边界

### 4.1 Runtime 负责

Runtime 负责：

- 解析备份范围。
- 路径 canonicalization 和 workspace 边界检查。
- 排除规则与脱敏规则。
- 密钥处理和加密 hook。
- 迁移包 schema 校验。
- package 兼容性校验。
- release snapshot 身份与来源。
- 文件锁和 operation lease。
- 原子写入或 staged 写入。
- dry-run / apply 分离。
- checksum、manifest 和可选 signature 校验。
- restore 与 rollback 权限。
- retention 与垃圾回收策略。
- audit 记录和 trace ID。
- 失败恢复与重试规则。

这些是正确性和安全性问题，必须由可测试的 Runtime 代码承担，而不是由
Agent 指令承担。

### 4.2 Agent 负责

Agent 可以：

- 判断用户正在请求哪类 Runtime 操作。
- 收集非破坏性的缺失上下文。
- 在高风险操作前建议创建备份或 snapshot。
- 生成 migration 描述和 release note。
- 通过显式 operation request 调用 Runtime API。
- 解释 preflight 警告和 policy 拒绝原因。
- 总结创建的 artifact 和验证证据。
- 在 Runtime 操作失败后分析日志。

Agent 不应该成为隐式 policy engine。Runtime policy 拒绝操作时，Agent 可以
解释原因，但不能绕过拒绝。

## 5. Runtime 服务划分

该能力应通过一组 Runtime-owned service 暴露。

| Service | 职责 |
| --- | --- |
| `OperationCoordinator` | 串行化高副作用操作，管理 operation ID、lease、取消和状态转换。 |
| `BackupService` | 创建、校验、恢复、列出和过期清理 backup artifact。 |
| `MigrationPackageService` | 构建、导入、校验、dry-run、apply 和拒绝 migration package。 |
| `ReleaseSnapshotService` | 在 release 激活前后捕获状态、比较 snapshot、记录 rollback 证据。 |
| `ArtifactStore` | 存储 artifact、manifest、checksum、metadata、retention 标记和 quarantine 记录。 |
| `ManifestVerifier` | 校验 artifact manifest、schema version、兼容性范围、checksum 和可选 signature。 |
| `PolicyEngine` | 判断 actor 是否可以 create、restore、apply、compare、export 或 delete artifact。 |
| `AuditLog` | 记录 actor、reason、scope、result、hash、warning、failure 和 recovery action。 |

这些 service 可以位于 Rust Runtime 边界内，再通过窄 host API 暴露给 Lua 或
外部 Agent。

## 6. Artifact 模型

每个 backup、migration package 和 release snapshot 都应该有 manifest。
manifest 是记录捕获内容、创建原因、请求者和验证方式的持久化契约。

必要字段：

| 字段 | 含义 |
| --- | --- |
| `artifact_id` | artifact 的稳定 ID。 |
| `artifact_type` | `backup`、`migration_package` 或 `release_snapshot`。 |
| `created_at` | UTC 时间戳。 |
| `created_by` | human、Agent、Supervisor 或 Runtime actor 身份。 |
| `request_id` | Agent 或 CLI 调用传入的因果请求 ID。 |
| `runtime_generation` | 创建或校验 artifact 的 Runtime generation。 |
| `project_id` | 项目或 workspace 身份。 |
| `scope` | Runtime 解析后的范围，不是用户输入的原始路径。 |
| `schema_version` | manifest schema version。 |
| `compatibility` | Runtime、config、state 和 package 兼容性约束。 |
| `entries` | 被捕获的文件、状态分区、checksum、size 和 redaction 标记。 |
| `policy` | policy 版本和决策记录。 |
| `verification` | checksum、signature、dry-run、health 或 restore verification 结果。 |
| `audit_id` | 指向 Runtime audit 记录。 |

manifest 应由 Runtime 代码生成。Agent 可以提出 reason 这类元数据，但不能
被信任来提供最终 checksum、解析后的路径或兼容性结果。

## 7. 备份语义

备份是 Runtime 识别的项目状态在某个时间点上的可恢复 artifact。它可以包含：

- 配置和 manifest。
- Lua Agent 与 capability 代码。
- AdapterRegistry metadata。
- 选定的 State Store 记录。
- Durable Event Log watermark 或有界 segment。
- policy 允许的 memory 与 knowledge metadata。
- release pointer metadata。
- restore safety 所需的 operation journal。

备份不应该包含：

- 原始 secret。
- 未脱敏 credential。
- 不受支持的临时 cache。
- 未 ack 的纯内存事件。
- 解析后 workspace 范围之外的任意用户文件。
- 无法在本地恢复的外部 provider 状态。

restore 应比 create 更严格。Runtime policy 应要求 validated manifest、兼容
target、明确 actor 权限和 audit 记录，才能让恢复操作修改状态。

## 8. 迁移包语义

迁移包是版本化、可检查、可验证的 artifact，用来描述状态或布局转换。它
应该携带足够 metadata，让 Runtime 能判断是否可以安全 apply。

必要属性：

- source 与 target schema version。
- compatibility range。
- package format version。
- 受影响的 state section。
- preflight requirement。
- reversible / irreversible 标记。
- idempotency key。
- checksum manifest。
- 可选 signature 或 trusted publisher identity。
- 预期 post-apply invariant。

migration package apply 必须由 Runtime 控制，因为它可能改变 state layout、
config 兼容性、index、release pointer 或 event replay 语义。Agent 可以解释
package 想做什么，但不应直接执行 migration logic。

## 9. Release Snapshot 语义

Release snapshot 不只是 backup。它是 release control record，用来关联
artifact、Runtime generation、配置、健康状态和 rollback 证据。

Runtime 至少应支持两种 snapshot 角色：

| Snapshot role | 含义 |
| --- | --- |
| `pre_release` | 在 activation 或 upgrade 前捕获当前 known-good 状态。 |
| `post_release` | 捕获激活后的状态，并附带 health 与兼容性证据。 |

有价值的 snapshot 内容包括：

- active release pointer。
- Runtime generation ID。
- binary 或 package digest。
- config digest。
- policy digest。
- manifest registry digest。
- Lua capability generation。
- AdapterRegistry generation。
- database 或 State Store schema version。
- EventBus durable watermark。
- health-check result。
- release 期间 apply 的 migration package ID。
- 与 release 关联的 backup artifact ID。
- rollback eligibility 与限制。

Supervisor 和 Runtime generation switching 模型应把 release snapshot 当作证据。
回滚决策可以由 Agent 辅助，但 snapshot 捕获、比较、pointer 移动和 rollback
操作必须由 Runtime 控制。

## 10. Operation 状态模型

高副作用操作需要显式状态，才能安全重试和恢复。状态模型归 Runtime 所有。

```text
requested
  -> admitted
  -> locked
  -> preflighted
  -> staged
  -> verified
  -> committed
  -> audited

failure paths:
  -> rejected
  -> quarantined
  -> rolled_back
  -> failed_with_recovery_required
```

具体实现可以演进，但架构不变量是：Runtime 必须能说明某个操作是从未开始、
在修改前被拒绝、已经 staged 但未 committed、已经成功 committed，还是需要
人工或自动 recovery。

## 11. Agent API 形态

Agent 应只拿到明确表达权限边界的窄 API。示例：

```text
ctx.runtime.backup.request(...)
ctx.runtime.backup.status(operation_id)
ctx.runtime.backup.list(...)

ctx.runtime.migration.verify(package_ref)
ctx.runtime.migration.dry_run(package_ref, target)
ctx.runtime.migration.apply(package_ref, target)

ctx.runtime.release_snapshot.create(role, release_ref)
ctx.runtime.release_snapshot.compare(left, right)
ctx.runtime.release_snapshot.status(operation_id)
```

API 应返回结构化值，包括 operation ID、artifact ID、policy decision、
warning、verification result 和 audit ID。它不应该暴露原始文件系统修改能力。

## 12. Policy 规则

Runtime policy 至少应支持：

- actor class：human、Agent、Supervisor、Runtime、CI。
- operation type：create、verify、restore、apply、compare、delete、export。
- workspace 或 project scope。
- 允许的 path class。
- secret redaction 与 encryption requirement。
- retention class。
- 最大 artifact size。
- offline / online requirement。
- release generation 约束。
- package trust requirement。
- destructive restore 或 irreversible migration 的 approval requirement。

Agent 可以帮助用户理解这些规则，但必须由 Runtime 执行规则。

## 13. 审计与可观测

每个高副作用操作都应该产生持久化证据：

- operation ID。
- request ID 与 trace ID。
- actor identity。
- requested reason。
- policy decision。
- resolved scope。
- artifact ID。
- manifest hash。
- warning。
- start / finish timestamp。
- failure category。
- rollback 或 recovery action。

日志必须脱敏 secret，并且应足够结构化，让 Agent 可以解释失败原因，而不需要
无限制读取文件系统。

## 14. 与现有架构的关系

该设计延续 Eva-CLI 现有边界：

- Rust 继续负责 authority、recovery、audit、schema 和 process lifecycle。
- Lua 与外部 Agent 继续只通过受控 host API 负责 orchestration 与 explanation。
- 进程级升级应在 Runtime generation activation 前后调用
  `ReleaseSnapshotService`。
- 当 hot reload 可能改变持久化状态或 rollback eligibility 时，应使用 backup
  或 snapshot artifact。
- migration package 影响 State Store、EventBus replay、MemoryService、
  KnowledgeService 或 AdapterRegistry layout 前，必须先通过 Runtime 校验。
- artifact manifest 应纳入与其他 Runtime-managed capability 相同的 policy
  和 audit 模型。

## 15. 安全不变量

- 原始 secret 不能写入 backup、package、snapshot 或 log。
- Agent 不能通过直接编辑文件来 restore 或 apply artifact。
- 导入的 package 在 verify 阶段不能执行代码。
- artifact 跨 trust boundary 前必须通过 manifest verification。
- release pointer 移动必须有 Runtime audit record。
- rollback 不能在缺少 post-rollback verification evidence 时声称成功。
- operation 不应依赖 manifest、policy 和 audit record 之外的隐藏全局状态。

## 16. 待定设计问题

以下细节应在实现规格阶段补齐：

- artifact storage layout 与默认 retention。
- 敏感备份分区的 encryption key management。
- 第三方 migration package 的 signature 要求。
- memory 与 knowledge data 的精确 restore policy。
- event log segment 捕获与 replay 兼容规则。
- CI release snapshot format。
- destructive restore 与 irreversible migration 的人工确认 UX。

这些是实现层细节，不改变“可信执行归 Runtime”的架构决策。

## 17. 总结

备份、迁移包和 release snapshot 不是 Agent 功能，而是 Runtime 安全能力。

Agent 可以通过理解意图、生成说明、请求操作和解释结果让这些能力更好用。
但 scope、policy、verification、mutation、rollback 和 audit 的事实来源必须
始终是 Runtime。
