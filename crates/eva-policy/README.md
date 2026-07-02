# eva-policy / 权限策略

更新时间：2026-07-02

`eva-policy` 负责把系统策略、manifest 声明、会话策略和 request 级约束合并成最终 effective policy。它只做纯数据计算和权限收紧，不扫描配置、不执行 I/O、不调用 Adapter、不读取密钥，也不替 Runtime 做副作用决策。

## 中文

### 当前实现状态

V0.2 已落地最小权限契约：

| 范围 | 状态 | 说明 |
| --- | --- | --- |
| `PermissionSet` | 已完成 | 表示网络、shell、workspace 读写、超时、capability allowlist、adapter allowlist |
| 权限收紧 | 已完成 | `narrowed_by` 对布尔权限取交集，对超时取更小值，对 allowlist 取交集 |
| 扩权检测 | 已完成 | `diff_against` 和 `is_subset_of` 可以判断 request 是否超过上限 |
| `SandboxPolicy` | 已完成 | 表示 Lua 禁用库、内存、超时、文件/网络/env 权限和 schema/topic 校验开关 |
| 沙箱收紧 | 已完成 | 禁用库取并集，资源限制取更小值，危险能力只能收紧 |
| `PolicyLayer` | 已完成 | 表示 system、manifest、session、request 等任意策略层 |
| `EffectivePolicy` | 已完成 | 从一个或多个 `PolicyLayer` 计算最终权限和沙箱策略 |
| request 校验 | 已完成 | `ensure_request_allowed` 会拒绝试图扩大 effective policy 的请求 |
| YAML policy 解析 | 不在本 crate | `eva-config` 只加载 extensible policy document，领域解释留给本 crate 后续切片 |

### 公开 API

| API | 输入 | 输出 | 用途 |
| --- | --- | --- | --- |
| `PermissionSet::deny_all` | 无 | `PermissionSet` | 构建默认拒绝权限 |
| `PermissionSet::read_only_runtime` | 无 | `PermissionSet` | 构建只读开发上限 |
| `PermissionSet::narrowed_by` | `&PermissionSet` | `PermissionSet` | 计算两个权限集合的交集 |
| `PermissionSet::diff_against` | `&PermissionSet` | `PermissionSetDiff` | 列出相对上限发生扩权的字段 |
| `PermissionSet::is_subset_of` | `&PermissionSet` | `bool` | 判断 request 是否未超过上限 |
| `SandboxPolicy::lua_default` | 无 | `SandboxPolicy` | 构建与 `config/policies/sandbox.yaml` 对齐的 Lua 安全基线 |
| `SandboxPolicy::narrowed_by` | `&SandboxPolicy` | `SandboxPolicy` | 合并沙箱策略并保持更严格结果 |
| `PolicyLayer::new` | 名称、权限、沙箱 | `PolicyLayer` | 构造一个策略层 |
| `EffectivePolicy::from_layers` | `IntoIterator<PolicyLayer>` | `Result<EffectivePolicy, EvaError>` | 计算最终策略，要求至少一层 |
| `EffectivePolicy::ensure_request_allowed` | `&PermissionSet` | `Result<(), EvaError>` | 拒绝扩权 request |

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
- 返回结构化 `EvaError`。

`eva-policy` 不做：

- 不扫描 `config/`。
- 不解析所有 policy YAML 领域字段。
- 不执行 shell、文件、网络、MCP、硬件或 Adapter。
- 不决定 CLI 展示格式和 exit code。
- 不保存审计日志，只能提供可被审计记录引用的结构化结果。

### 测试与验证

| 命令 | 当前结果 |
| --- | --- |
| `cargo test -p eva-policy` | 通过，10 个测试 |
| `cargo test --workspace` | 通过 |

关键测试覆盖：

| 测试 | 覆盖内容 |
| --- | --- |
| `narrowing_intersects_boolean_permissions` | 布尔权限只能收紧 |
| `narrowing_uses_lowest_timeout` | 超时取更小值 |
| `narrowing_intersects_capabilities_and_adapters` | capability/adapter allowlist 取交集 |
| `diff_reports_expansion` | 能检测 shell/capability 扩权 |
| `default_sandbox_matches_safe_lua_floor` | Lua 默认沙箱与配置样例一致 |
| `narrowing_unions_disabled_libs_and_uses_lower_limits` | 沙箱禁用库并集、资源限制取更小值 |
| `effective_policy_intersects_layers` | 多策略层合并结果正确 |
| `request_expansion_is_rejected` | 扩权请求返回 `PermissionDenied` |

### 后续版本移交

| 版本 | 工作 |
| --- | --- |
| V0.3 | CLI 将使用 `ensure_request_allowed` 转换成人类/JSON 诊断和 exit code |
| V0.4 | Runtime/Tool Layer 在 capability 调用前应用 effective policy |
| V1.1 | AdapterRegistry 解释 adapter policy 领域字段并生成 `PolicyLayer` |
| V1.3 | Hardware policy 领域字段生成设备访问 `PolicyLayer` |

## English

`eva-policy` owns side-effect-free permission and sandbox contracts. V0.2 implements `PermissionSet`, `SandboxPolicy`, `PolicyLayer`, and `EffectivePolicy`. Merging only narrows permissions: booleans are intersected, timeouts use the lower value, allowlists are intersected, disabled Lua libraries are unioned, and required validations stay enabled.

This crate does not scan configuration, execute I/O, invoke adapters, or write audit logs. It returns typed policy results and structured errors for runtime and CLI layers to consume.
