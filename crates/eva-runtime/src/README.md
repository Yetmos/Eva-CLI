# eva-runtime/src / 运行时源码边界

更新时间：2026-07-03

本目录实现 V0.3 no-op runtime composition root。它已经可以从 validated config 构造 runtime summary，并提供幂等 shutdown 状态；真实事件闭环仍留给 V0.4。

## 文件职责

| 文件 | 当前职责 | 当前状态 | 下一步 |
| --- | --- | --- | --- |
| `lib.rs` | 导出 runtime 模块和公共类型。 | 完成 V0.3 re-export。 | 保持组合根公共入口稳定。 |
| `builder.rs` | 定义 `RuntimeMode`、`RuntimeOptions`、`RuntimeBuilder`，从 `ProjectConfig` 构造 no-op runtime。 | 完成 V0.3。 | V0.4 注入真实服务实现。 |
| `runtime.rs` | 定义 `RuntimeStatus`、`RuntimeSummary`、`Runtime`，持有 summary、services 和 shutdown state。 | 完成 V0.3。 | 增加 start/drain/status 的真实生命周期。 |
| `services.rs` | 定义 `ServiceState`、`ServiceSummary`、`RuntimeServices`，记录服务边界状态。 | 完成 V0.3。 | 将 planned 服务替换为真实 handle。 |
| `shutdown.rs` | 定义 `ShutdownState` 和 `ShutdownReport`。 | 完成 V0.3 幂等记录。 | V0.5 接 drain、cancel、audit flush。 |

## Builder contract

- 输入必须是已经由 `eva-config::load_project_config` 校验过的 `ProjectConfig`。
- V0.3 build 过程必须 side-effect free。
- build 会拒绝空 Agent 集合和空 routes，避免构造不可检查 runtime。
- build 输出 `Runtime`，CLI 通过 `runtime.summary()` 和 service summary 做展示。
- 默认 generation id 为 `noop-v0.3`，只用于诊断，不代表真实 runtime generation 切换。

## Service summary 约定

| Service | V0.3 状态 | 说明 |
| --- | --- | --- |
| `config` | `ready` | 配置已加载，摘要含 Agent 和 route 数量。 |
| `policy` | `ready` | policy document 已加载，后续接 effective policy 展示。 |
| `observability` | `ready` | trace/audit/metric 契约可供上层使用。 |
| `storage`、`eventbus`、`scheduler`、`agent_runtime`、`lua_host`、`capability_router` | `planned` | V0.4 最小闭环接入。 |
| `adapter_router`、`mcp`、`discovery`、`memory`、`hardware`、`backup_lifecycle` | `planned` | V1.x 扩展能力接入。 |

## Shutdown contract

`Runtime::shutdown()` 当前只调用 `ShutdownState::request()`，并把 runtime status 标记为 `shutdown`。第一次调用返回 `already_shutdown = false`，后续调用返回 `already_shutdown = true`。这个契约先锁定幂等语义，避免 V0.5 接真实 drain 时改变调用方预期。

## 测试入口

| 命令 | 目标 |
| --- | --- |
| `cargo test -p eva-runtime` | 验证 no-op builder 和 shutdown。 |
| `cargo run -- inspect runtime --output json` | 验证 CLI 可读取 runtime summary。 |
| `cargo test --workspace` | 验证组合根与 workspace 其他 crate 兼容。 |
