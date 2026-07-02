# eva-policy/src / 策略源码

## 中文

源码按三层组织：

| 文件 | 职责 |
| --- | --- |
| `permissions.rs` | `PermissionSet`、allowlist、超时和扩权检测 |
| `sandbox.rs` | `SandboxPolicy`、Lua 默认沙箱和沙箱收紧规则 |
| `effective.rs` | `PolicyLayer` 与 `EffectivePolicy` 合并入口 |

实现约束：

- 只使用纯数据结构和 `eva-core::EvaError`。
- 合并策略只能收紧，不能放宽。
- `None` allowlist 表示当前层不约束，`Some(empty)` 表示当前层显式允许空集合。
- request 是否可执行由 `EffectivePolicy::ensure_request_allowed` 判断。
- 后续新增 policy domain 时，应先保持领域解析和副作用执行在其它 crate 中，`eva-policy` 只接收已归一化的策略层。

## English

The source is split into permission sets, sandbox policy, and effective policy merging. Policy evaluation is pure data work and may only narrow permissions.
