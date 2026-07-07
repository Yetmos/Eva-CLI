# eva-policy/src / 策略源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录承载权限集合、沙箱策略和 effective policy 计算。V0.2 已完成最小权限契约，后续版本主要把它接入 CLI、Runtime、Adapter、Hardware 和 Backup。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 公共导出 | 已完成 | V0.2 |
| `permissions.rs` | `PermissionSet`、收紧、diff、subset、显式 capability/adapter allow 查询 | 已完成 | V0.2/V0.3/V1.8.5.2 |
| `sandbox.rs` | `SandboxPolicy`、Lua 默认安全基线、收紧 | 已完成 | V0.2/V0.4 |
| `effective.rs` | `PolicyLayer`、`EffectivePolicy`、request gate | 已完成 | V0.2/V0.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义权限字段和 deny/read-only 默认值。 | 权限上限可表达。 |
| 2 | 实现权限收紧、扩权 diff 和 subset 检查。 | request gate 可用。 |
| 3 | 定义沙箱资源限制和 Lua 默认安全基线。 | Lua host 可映射限制。 |
| 4 | 将多层策略合并成 effective policy。 | Runtime 调用前可统一授权。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Permissions | 权限集合、扩权检测和显式 allow 查询 | 已完成 | CLI 展示 diff；runtime/capability gate 复用。 |
| Sandbox | 沙箱限制 | 已完成 | Lua host 接入。 |
| Effective | 多层合并和 request gate | 已完成 | Runtime/capability gate 接入。 |
| Domain parser | YAML policy domain 解释 | 未实现 | V0.4/V1.x 分模块扩展。 |
