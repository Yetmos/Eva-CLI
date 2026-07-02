# eva-cli/src / CLI 源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载 `eva-cli` 的命令入口和子命令模块。当前仍处于骨架阶段，V0.3 先实现开发闭环命令，V0.4 再接最小 runtime 运行命令。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 公共入口和模块导出 | 骨架 | V0.3 |
| `run.rs` | `eva run` 和 runtime 启动入口 | 骨架 | V0.3/V0.4 |
| `inspect.rs` | 配置、policy、routes、runtime 状态检查 | 骨架 | V0.3 |
| `emit.rs` | 构造并发布 ingress event | 骨架 | V0.4 |
| `agent.rs` | Agent 列表、状态、任务控制命令 | 骨架 | V0.4/V0.5 |
| `adapter.rs` | Adapter 列表、probe、调用计划命令 | 骨架 | V1.1 |
| `capability.rs` | Capability 列表、详情、dry-run 调用命令 | 骨架 | V0.4/V1.1 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 在 `lib.rs` 建立命令解析入口和输出 envelope。 | `eva --help` 可运行。 |
| 2 | 在 `inspect.rs` 和 `run.rs` 接 `eva-config` 与 no-op runtime。 | `config validate`、`inspect` 可输出 JSON。 |
| 3 | 在 `emit.rs` 接 EventBus，`agent.rs` 接任务状态。 | V0.4/V0.5 最小运行闭环。 |
| 4 | 在 `adapter.rs`、`capability.rs` 接扩展能力命令。 | V1.1 外部能力可诊断。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| 命令解析 | 子命令树、参数校验 | 未实现 | 先覆盖 V0.3 命令。 |
| 输出格式 | human/json、exit code | 未实现 | 统一 `EvaError` 映射。 |
| 配置诊断 | `config validate` | 未实现 | 调用 `load_project_config`。 |
| Runtime 接入 | no-op 和 basic run | 未实现 | 等 `eva-runtime` builder。 |
