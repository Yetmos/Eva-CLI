# eva-policy/src / 策略源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录承载权限集合、沙箱策略、effective policy 计算、V1.9.2 policy domain runtime gate、V1.13.2 provider credential session scope decision、V1.13.3 provider admission retry backoff 查询和 V1.15.8 memory redaction policy domain。`eva-config` 负责加载 YAML，当前目录负责解释已定义的策略领域并返回可审计 decision。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 公共导出 | 已完成 | V0.2 |
| `permissions.rs` | `PermissionSet`、收紧、diff、subset、显式 capability/adapter allow 查询 | 已完成 | V0.2/V0.3/V1.8.5.2 |
| `sandbox.rs` | `SandboxPolicy`、Lua 默认安全基线、收紧 | 已完成 | V0.2/V0.4 |
| `effective.rs` | `PolicyLayer`、`EffectivePolicy`、request gate | 已完成 | V0.2/V0.4 |
| `domains.rs` | typed policy domain parser、`RuntimePolicyGate`、高风险 action / provider credential session allow/deny audit / retry backoff 查询 / memory redaction policy | 已完成 | V1.15.8 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义权限字段和 deny/read-only 默认值。 | 权限上限可表达。 |
| 2 | 实现权限收紧、扩权 diff 和 subset 检查。 | request gate 可用。 |
| 3 | 定义沙箱资源限制和 Lua 默认安全基线。 | Lua host 可映射限制。 |
| 4 | 将多层策略合并成 effective policy。 | Runtime 调用前可统一授权。 |
| 5 | 将 policy YAML domain 解析为 typed domain 和 `PolicyLayer`。 | Adapter/MCP/Hardware/Runtime/Lua/Memory policy 可 round trip。 |
| 6 | 对高风险 runtime action 执行默认拒绝和显式 allow 判定。 | Skill/Hardware/Restore/Upgrade 等路径可写 audit evidence。 |
| 7 | 对 provider credential session scope 执行 provider 匹配判定。 | 跨 provider 复用 session scope 在 runner 启动前拒绝。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Permissions | 权限集合、扩权检测和显式 allow 查询 | 已完成 | CLI 展示 diff；runtime/capability gate 复用。 |
| Sandbox | 沙箱限制 | 已完成 | Lua host 接入。 |
| Effective | 多层合并和 request gate | 已完成 | Runtime/capability gate 接入。 |
| Domain parser | YAML policy domain 解释 | 已完成 V1.15.8 | 已包含 `memory_policy.redaction`；真实 provider/hardware/backup/lifecycle apply 继续复用。 |
| Runtime gate | 高风险 action 默认拒绝、显式 allow、provider credential session scope、retry backoff hint 和 audit decision | 已完成 V1.13.3 | 接生产 audit sink 和常驻 runtime。 |
