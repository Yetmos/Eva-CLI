# eva-runtime / 运行时组合根

更新时间：2026-07-03

`eva-runtime` 是 Eva-CLI 的 composition root。它把已经通过 `eva-config` 校验的项目配置组装成 runtime 实例、服务摘要和生命周期入口。下层 crate 不反向依赖 `eva-runtime`；真实副作用必须通过 runtime 组合根统一进入。

## V0.3 已实现范围

| 能力 | 当前行为 | 副作用边界 |
| --- | --- | --- |
| `RuntimeBuilder` | 从 `ProjectConfig` 构造 V0.3 no-op `Runtime`。 | 不启动线程、事件循环、Lua、adapter 或外部进程。 |
| `RuntimeOptions` | 记录 runtime mode 和 generation id。默认 generation 为 `noop-v0.3`。 | 纯数据。 |
| `RuntimeSummary` | 汇总环境、project root、Agent/Adapter/Capability/Route/Policy 数量和 service summary。 | 纯数据。 |
| `RuntimeServices` | 保存 V0.3 服务边界摘要：config、policy、observability 为 ready，其余 runtime 服务为 planned。 | 只保存 summary，不持有真实服务句柄。 |
| `Runtime::shutdown()` | 标记 runtime 为 shutdown，返回 `ShutdownReport`。重复调用保持幂等。 | 不依赖真实运行循环。 |

## Runtime mode

当前只实现 `RuntimeMode::Noop`。该模式的含义是：配置已通过校验，runtime composition root 可以被构造和检查，但还不会处理事件、调度 Agent、执行 Lua 或调用 capability。

V0.4 会在这个组合根下继续接入 in-memory storage、EventBus、Scheduler、AgentRuntime、Lua host 和 builtin capability。V0.3 不提前伪造这些服务的行为，只在 `RuntimeServices` 中标记为 `planned`。

## 与 CLI 的关系

- `eva inspect` 调用 `RuntimeBuilder::new().build(project)` 读取 runtime summary。
- `eva doctor` 调用同一条 builder 路径，确认 no-op runtime summary 可构造。
- `eva run` 在 V0.3 会先构造 no-op runtime，再返回 `Unsupported`，提示真实事件循环属于 V0.4。

## 验证命令

| 命令 | 目标 |
| --- | --- |
| `cargo test -p eva-runtime` | 验证 no-op builder、summary 和幂等 shutdown。 |
| `cargo run -- inspect runtime --output json` | 通过 CLI 验证 runtime summary 可读。 |
| `cargo test --workspace` | 验证 runtime 组合根不破坏 workspace。 |

## 单元测试

| 测试 | 覆盖内容 |
| --- | --- |
| `noop_builder_summarizes_sample_project` | 样例项目可构造 no-op runtime，summary 数量与配置一致。 |
| `shutdown_is_idempotent` | `Runtime::shutdown()` 第一次记录停机，后续调用返回 already shutdown。 |

## 剩余限制

- `RuntimeServices` 当前只保存摘要，不持有真实 storage、eventbus、scheduler 或 agent handles。
- `RuntimeBuilder::build` 只检查 Agent 和 route 非空；更深层 service wiring 留给 V0.4/V0.5。
- effective policy 目前仍由下层 `eva-policy` 提供契约，V0.3 runtime summary 只展示 policy document 数量。
- shutdown 只记录状态切换，不执行 drain、cancel、audit flush 或 generation rollback。
