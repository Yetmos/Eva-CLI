# eva-runtime / 运行时组合根

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-runtime` 是 Eva-CLI workspace 的唯一组合根，负责把配置、policy、storage、eventbus、scheduler、agent、Lua host、capability、adapter、memory、hardware、backup 和 lifecycle 服务装配成一个 runtime generation。下层 crate 不应依赖 `eva-runtime`，只能通过各自公开 trait 或数据契约被 runtime 组合。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Runtime builder | 骨架 | 从 `ProjectConfig`、policy 和运行参数构造 runtime services。 |
| Service registry | 骨架 | 保存本 generation 的服务句柄、只读摘要和健康状态。 |
| Runtime instance | 骨架 | 拥有启动、运行、停止、drain 的生命周期入口。 |
| Shutdown | 骨架 | 协调 eventbus 停止接收、scheduler drain、agent cancel、audit flush。 |
| No-op runtime | 未实现 | V0.3 先构造不执行事件的 runtime，用于 CLI inspect 和配置闭环。 |
| 最小事件闭环 | 未实现 | V0.4 组合 storage、eventbus、scheduler、agent、lua-host、capability。 |
| 扩展服务组合 | 未实现 | V1.x 接 adapter、MCP、discovery、memory、hardware、backup、lifecycle。 |

## 模块边界

`eva-runtime` 做：

- 汇总已经验证过的配置和 policy。
- 选择具体服务实现并注入依赖。
- 管理 runtime generation 的启动、停止、drain、health summary。
- 统一发布运行时 audit、trace 和 metrics。

`eva-runtime` 不做：

- 不定义基础 ID、Topic、Event、Invoke 契约，这些属于 `eva-core`。
- 不解析 YAML 细节，这些属于 `eva-config`。
- 不实现业务 Agent 或 Lua 脚本逻辑。
- 不让下层模块反向调用 runtime。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.3 | 定义 `RuntimeOptions`、`RuntimeSummary`、`RuntimeBuilder` 输入输出。 | `eva-config`、`eva-policy` | builder 类型不启动任何副作用即可测试。 |
| 2 | V0.3 | 实现 no-op `RuntimeServices`，记录配置摘要、policy 摘要和观测 sink。 | `eva-observability` | `eva-cli inspect runtime` 可展示摘要。 |
| 3 | V0.3 | 定义 shutdown token 和 `Runtime::shutdown()` 语义。 | 标准库 | 多次 shutdown 幂等。 |
| 4 | V0.4 | 注入 in-memory storage 和 eventbus。 | `eva-storage`、`eva-eventbus` | publish/subscribe 可由 runtime 统一创建。 |
| 5 | V0.4 | 注入 scheduler、agent runtime、lua host、capability registry。 | `eva-scheduler`、`eva-agent`、`eva-lua-host`、`eva-capability` | `examples/basic/` 从 event 到 Lua 到 capability。 |
| 6 | V0.5 | 增加长任务、超时、取消、runtime status 查询。 | AgentRuntime、EventLog | CLI 可查询任务状态和日志。 |
| 7 | V1.1 | 接 adapter、MCP、discovery，并保持授权和发现分离。 | Adapter/MCP/Discovery | discovery candidate 不等于 executable handle。 |
| 8 | V1.2-V1.4 | 接 memory、hardware、backup、lifecycle。 | V1.x 服务模块 | 高风险操作先 plan 后 apply，全部可审计。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | runtime 模块导出 | 骨架 | re-export builder、runtime、services、shutdown 的公共类型。 |
| `src/builder.rs` | 从配置组装服务 | `RESPONSIBILITY` 占位 | 定义 builder 输入、输出、错误和 no-op build。 |
| `src/runtime.rs` | runtime instance 所有权 | `RESPONSIBILITY` 占位 | 实现 `Runtime` 状态、summary、start/shutdown。 |
| `src/services.rs` | 服务句柄容器 | `RESPONSIBILITY` 占位 | 定义 `RuntimeServices` 和只读 `ServiceSummary`。 |
| `src/shutdown.rs` | 停止和 drain | `RESPONSIBILITY` 占位 | 实现幂等 shutdown token 和阶段记录。 |
| `src/README.md` | 源码目录说明 | 简略 | 同步 builder/runtime/services 的实施状态。 |
| 单元测试 | builder 与 shutdown | 未开始 | 覆盖 no-op builder、重复 shutdown、错误传播。 |
| 集成测试 | CLI inspect runtime | 未开始 | 使用示例配置构造 no-op runtime 摘要。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.3 | `cargo test -p eva-runtime` | builder、summary、shutdown 单元测试通过。 |
| V0.3 | `cargo run -- inspect runtime --output json` | CLI 能读取 runtime 摘要。 |
| V0.4 | `cargo test --workspace` | 最小 runtime 服务组合不破坏 workspace。 |
| V0.4 | `cargo run -- run --example basic` | 端到端事件闭环可运行。 |

## English

`eva-runtime` is the only composition root. It wires validated configuration and policy into concrete services and owns runtime generation lifecycle. Lower crates must not depend back on it.
