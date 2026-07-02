# eva-capability/src / 能力源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载 capability registry、router、generation swap 和 host API trait。当前为骨架，V0.4 先实现一个无副作用 builtin capability 以支撑端到端闭环。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V0.4 |
| `registry.rs` | capability 注册和查询 | 骨架 | V0.4 |
| `router.rs` | provider 路由和 policy gate | 骨架 | V0.4/V1.1 |
| `generation.rs` | generation swap 和回滚安全 handle | 骨架 | V0.4 |
| `host_api.rs` | Agent/Lua 可调用 trait | 骨架 | V0.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 descriptor、provider hint、schema ref。 | manifest 可转 registry entry。 |
| 2 | 实现 registry 和 generation handle。 | capability set 可替换。 |
| 3 | 实现 router 和 policy gate。 | 未授权调用被拒绝。 |
| 4 | 定义 host API trait 和 builtin capability。 | V0.4 示例可调用。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Registry | 注册、查询、索引 | 未实现 | 定义 descriptor。 |
| Router | provider 选择 | 未实现 | 接 policy allowlist。 |
| Generation | 激活和回滚 | 未实现 | 定义 generation handle。 |
| Host API | 受控调用 trait | 未实现 | 定义输入输出 envelope。 |
