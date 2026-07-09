# eva-policy / 权限策略

更新时间：2026-07-09

![Eva module implementation roadmap](../assets/eva-module-implementation-roadmap.svg)

`eva-policy` 负责把系统策略、manifest 声明、会话策略和 request 级约束合并成最终 effective policy。它只做纯数据计算和权限收紧，不扫描配置、不执行 I/O、不调用 Adapter、不读取密钥，也不替 Runtime 做副作用决策。

## 中文

### 当前实现状态

V0.2 已落地最小权限契约：

| 范围 | 状态 | 说明 |
| --- | --- | --- |
| `PermissionSet` | 已完成 | 表示网络、shell、workspace 读写、超时、capability allowlist、adapter allowlist |
| 显式身份授权查询 | 已完成 V1.8.5.2 | `explicitly_allows_capability` / `explicitly_allows_adapter` 供 runtime gate 采用默认拒绝语义 |
| 权限收紧 | 已完成 | `narrowed_by` 对布尔权限取交集，对超时取更小值，对 allowlist 取交集 |
| 扩权检测 | 已完成 | `diff_against` 和 `is_subset_of` 可以判断 request 是否超过上限 |
| `SandboxPolicy` | 已完成 | 表示 Lua 禁用库、内存、超时、文件/网络/env 权限和 schema/topic 校验开关 |
| 沙箱收紧 | 已完成 | 禁用库取并集，资源限制取更小值，危险能力只能收紧 |
| `PolicyLayer` | 已完成 | 表示 system、manifest、session、request 等任意策略层 |
| `EffectivePolicy` | 已完成 | 从一个或多个 `PolicyLayer` 计算最终权限和沙箱策略 |
| request 校验 | 已完成 | `ensure_request_allowed` 会拒绝试图扩大 effective policy 的请求 |
| YAML policy 解析 | 已完成 V1.9.2 | `PolicyDomainSet` 解释 `adapter_policy`、`hardware_policy`、`mcp_server`、`runtime_policy` 和 `lua_sandbox`，并生成 `PolicyLayer` |
| 运行时高风险门禁 | 已完成 V1.13.3 | `RuntimePolicyGate` 对 MCP tool/topic、Skill、Hardware、Restore/Upgrade/Release/Supervisor 和 provider credential session scope 等动作输出 allow/deny decision 和 audit，并为 provider admission 暴露 capability retry backoff |

### 公开 API

| API | 输入 | 输出 | 用途 |
| --- | --- | --- | --- |
| `PermissionSet::deny_all` | 无 | `PermissionSet` | 构建默认拒绝权限 |
| `PermissionSet::read_only_runtime` | 无 | `PermissionSet` | 构建只读开发上限 |
| `PermissionSet::narrowed_by` | `&PermissionSet` | `PermissionSet` | 计算两个权限集合的交集 |
| `PermissionSet::diff_against` | `&PermissionSet` | `PermissionSetDiff` | 列出相对上限发生扩权的字段 |
| `PermissionSet::is_subset_of` | `&PermissionSet` | `bool` | 判断 request 是否未超过上限 |
| `PermissionSet::explicitly_allows_capability` | `&CapabilityName` | `bool` | runtime gate 检查 capability 是否被显式 allow |
| `PermissionSet::explicitly_allows_adapter` | `&AdapterId` | `bool` | runtime gate 检查 adapter provider 是否被显式 allow |
| `SandboxPolicy::lua_default` | 无 | `SandboxPolicy` | 构建与 `config/policies/sandbox.yaml` 对齐的 Lua 安全基线 |
| `SandboxPolicy::narrowed_by` | `&SandboxPolicy` | `SandboxPolicy` | 合并沙箱策略并保持更严格结果 |
| `PolicyLayer::new` | 名称、权限、沙箱 | `PolicyLayer` | 构造一个策略层 |
| `EffectivePolicy::from_layers` | `IntoIterator<PolicyLayer>` | `Result<EffectivePolicy, EvaError>` | 计算最终策略，要求至少一层 |
| `EffectivePolicy::ensure_request_allowed` | `&PermissionSet` | `Result<(), EvaError>` | 拒绝扩权 request |
| `PolicyDomainSet::from_documents` | `&[PolicyDocument]` | `Result<PolicyDomainSet, EvaError>` | 将配置加载出的 policy YAML 转成 typed domain 和 policy layers |
| `PolicyDomainSet::effective_policy` | 无 | `Result<EffectivePolicy, EvaError>` | 合并 policy domain 生成的 layers |
| `RuntimePolicyGate::decide` | `RuntimePolicyRequest` | `PolicyDecision` | 对高风险动作和 provider credential session scope 执行 allow/deny 判定 |

### 权限收紧规则

| 字段 | 合并规则 | 说明 |
| --- | --- | --- |
| `network` | `&&` | 任一层拒绝则最终拒绝 |
| `shell` | `&&` | 默认必须保持关闭 |
| `read_workspace` | `&&` | 只在所有层都允许时可读 |
| `write_workspace` | `&&` | 高风险权限，后续 CLI 必须先 plan |
| `max_timeout_ms` | 取更小值 | 上层可继续缩短执行时间 |
| `capabilities` | allowlist 交集 | `None` 表示该层不约束，`Some(empty)` 表示显式不允许任何 capability |
| `adapters` | allowlist 交集 | 与 capability allowlist 语义一致 |

### 沙箱收紧规则

| 字段 | 合并规则 | 说明 |
| --- | --- | --- |
| `disabled_lua_libs` | 并集 | 任一层禁用的 Lua 库最终都禁用 |
| `memory_mb` | 取更小值 | 避免下层扩大内存预算 |
| `execution_timeout_ms` | 取更小值 | 避免下层扩大执行时间 |
| `filesystem_enabled` | `&&` | 任一层关闭则最终关闭 |
| `network_enabled` | `&&` | Lua 默认不直连网络 |
| `environment_enabled` | `&&` | Lua 默认不读环境变量 |
| `return_schema_validation` | `||` | 任一层要求校验则最终必须校验 |
| `emitted_topic_validation` | `||` | 任一层要求校验则最终必须校验 |

### 模块边界

`eva-policy` 做：

- 表示权限和沙箱策略。
- 计算 effective policy。
- 拒绝扩权 request。
- 解释当前已定义的 policy YAML domain。
- 为高风险 runtime action 生成可审计的 allow/deny decision。
- 返回结构化 `EvaError`。

`eva-policy` 不做：

- 不扫描 `config/`。
- 不执行 shell、文件、网络、MCP、硬件或 Adapter。
- 不决定 CLI 展示格式和 exit code。
- 不保存审计日志，只能提供可被审计记录引用的结构化结果。

### 测试与验证

| 命令 | 当前结果 |
| --- | --- |
| `cargo test -p eva-policy` | 通过，20 个测试 |
| `cargo test --workspace` | 通过 |

关键测试覆盖：

| 测试 | 覆盖内容 |
| --- | --- |
| `narrowing_intersects_boolean_permissions` | 布尔权限只能收紧 |
| `narrowing_uses_lowest_timeout` | 超时取更小值 |
| `narrowing_intersects_capabilities_and_adapters` | capability/adapter allowlist 取交集 |
| `diff_reports_expansion` | 能检测 shell/capability 扩权 |
| `explicit_identity_allows_default_to_deny_for_runtime_gates` | runtime gate 可用显式 allow 查询实现默认拒绝 |
| `default_sandbox_matches_safe_lua_floor` | Lua 默认沙箱与配置样例一致 |
| `narrowing_unions_disabled_libs_and_uses_lower_limits` | 沙箱禁用库并集、资源限制取更小值 |
| `effective_policy_intersects_layers` | 多策略层合并结果正确 |
| `request_expansion_is_rejected` | 扩权请求返回 `PermissionDenied` |
| `parses_sample_policy_domains` | 示例 policy YAML 可解析为 typed domain |
| `runtime_gate_requires_explicit_high_risk_allow` | restore/upgrade 等高风险动作默认拒绝 |
| `runtime_policy_can_explicitly_allow_high_risk_action` | 显式 allow path 生成允许 decision |
| `runtime_gate_allows_provider_credential_session_for_matching_provider` | 同 provider credential session scope 允许并输出 audit |
| `runtime_gate_denies_provider_credential_session_cross_provider` | 跨 provider credential session scope 被拒绝 |
| `runtime_gate_exposes_adapter_retry_backoff_by_capability` | V1.13.3 provider admission 可读取 capability retry backoff |

### 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.2 | 定义 `PermissionSet` 和默认拒绝/只读上限。 | `eva-core::EvaError` | 权限字段可被测试覆盖。 |
| 2 | V0.2 | 实现权限收紧、allowlist 交集和扩权 diff。 | 标准库集合类型 | request 扩权可被拒绝。 |
| 3 | V0.2 | 定义 `SandboxPolicy` 和 Lua 默认安全基线。 | config sample policy | 默认禁用危险 Lua 能力。 |
| 4 | V0.2 | 实现 `PolicyLayer` 和 `EffectivePolicy::from_layers`。 | 权限/沙箱模型 | 多层策略只会收紧。 |
| 5 | V0.3 | 将 policy 错误映射到 CLI human/json 诊断。 | `eva-cli` | 用户能看懂被拒绝字段。 |
| 6 | V0.4 | 在 capability/Agent/Lua 调用前应用 effective policy。 | runtime/capability/lua-host | 未授权调用无法执行。 |
| 7 | V1.9.2 | 解释 Adapter、MCP、Hardware、Runtime、Lua policy domain。 | `eva-config::PolicyDocument` | 高风险权限有明确策略层和 audit decision。 |

### 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 公共导出和 crate 入口 | 已完成 | 随新增 policy domain re-export。 |
| `src/permissions.rs` | `PermissionSet`、收紧、diff、subset | 已完成 | V0.3 增加 CLI 友好的 diff 展示。 |
| `src/sandbox.rs` | `SandboxPolicy`、Lua 默认沙箱、收紧 | 已完成 | V0.4 接 `eva-lua-host` 限制映射。 |
| `src/effective.rs` | `PolicyLayer`、`EffectivePolicy`、request 校验 | 已完成 | V0.4 接 runtime/capability gate。 |
| `src/domains.rs` | YAML policy domain parser、`RuntimePolicyGate`、高风险 action decision | 已完成 V1.13.3 | 已接 provider credential session scope 和 retry backoff 查询；后续接生产 runtime/supervisor/hardware apply。 |
| `src/README.md` | 源码目录说明 | 简略 | 同步文件职责和后续阶段。 |
| policy domain parser | YAML domain 到策略层 | 已完成 V1.9.2 | 扩展真实 provider/hardware/backup/lifecycle 细粒度策略。 |

### 后续版本移交

| 版本 | 工作 |
| --- | --- |
| V0.3 | CLI 将使用 `ensure_request_allowed` 转换成人类/JSON 诊断和 exit code |
| V0.4 | Runtime/Tool Layer 在 capability 调用前应用 effective policy |
| V1.9.2 | Adapter/MCP/Hardware/Runtime/Lua policy domain 解释并输出 runtime gate audit |
| V1.13.2 | Provider credential session scope 复用 `RuntimePolicyGate` 输出 allow/deny audit |
| V1.13.3 | Provider admission gate 可通过 `RuntimePolicyGate::adapter_retry_backoff_ms` 获取 capability backoff hint |
| V1.10+ | Hardware、restore、upgrade 等真实 apply 路径复用 runtime gate 并写入生产 audit sink |

## English

`eva-policy` owns side-effect-free permission and sandbox contracts. V0.2 implements `PermissionSet`, `SandboxPolicy`, `PolicyLayer`, and `EffectivePolicy`. Merging only narrows permissions: booleans are intersected, timeouts use the lower value, allowlists are intersected, disabled Lua libraries are unioned, and required validations stay enabled.

This crate does not scan configuration, execute I/O, invoke adapters, or write audit logs. It returns typed policy results and structured errors for runtime and CLI layers to consume.
