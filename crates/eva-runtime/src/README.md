# eva-runtime/src / 运行时源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载 runtime composition root 的实现。当前为可编译骨架，V0.3 先实现 no-op runtime builder，V0.4 再装配最小事件闭环。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出和公共入口 | 骨架 | V0.3 |
| `builder.rs` | 从 validated config 组装 runtime services | 骨架 | V0.3/V0.4 |
| `runtime.rs` | runtime instance、summary、生命周期入口 | 骨架 | V0.3 |
| `services.rs` | 保存本 generation 的服务句柄 | 骨架 | V0.3/V0.4 |
| `shutdown.rs` | shutdown、drain、幂等停止 | 骨架 | V0.3/V0.5 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 builder 输入、runtime summary 和 service summary。 | CLI 可 inspect no-op runtime。 |
| 2 | 实现幂等 shutdown token 和无副作用 runtime instance。 | V0.3 单元测试通过。 |
| 3 | 注入 storage、eventbus、scheduler、agent、lua-host、capability。 | V0.4 端到端运行。 |
| 4 | 接 adapter、memory、hardware、backup、lifecycle。 | V1.x 扩展能力可组合。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Builder | no-op build 和错误传播 | 未实现 | 定义 `RuntimeBuilder`。 |
| Runtime | start/shutdown/status | 未实现 | 定义 `Runtime` 状态。 |
| Services | service handle 容器 | 未实现 | 定义只读 summary。 |
| Shutdown | drain 和 cancel | 未实现 | 先实现幂等停止。 |
