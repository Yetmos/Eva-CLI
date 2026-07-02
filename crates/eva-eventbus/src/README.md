# eva-eventbus/src / 事件总线源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载事件总线接口、内存实现、恢复日志集成和死信路径。当前为骨架，V0.4 先实现可测试的 in-memory event bus。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V0.4 |
| `bus.rs` | EventBus trait、publish、ack、fail | 骨架 | V0.4 |
| `in_memory.rs` | 内存事件总线 | 骨架 | V0.4 |
| `recoverable.rs` | Durable EventLog replay 集成 | 骨架 | V0.4 |
| `dead_letter.rs` | dead-letter 记录和查询 | 骨架 | V0.4/V0.5 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 EventBus trait 和 ack token。 | 调用方不绑定具体实现。 |
| 2 | 实现 in-memory publish/subscribe。 | 单元测试可覆盖事件投递。 |
| 3 | 接 `eva-storage::EventLog`。 | publish/ack/fail 可恢复。 |
| 4 | 实现 dead-letter 和重试边界。 | 失败事件可查询。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Bus | 发布、确认、失败接口 | 未实现 | 定义 trait 和错误。 |
| In-memory | 测试用队列实现 | 未实现 | 实现 bounded queue。 |
| Recoverable | replay 和 watermark | 未实现 | 接 EventLog trait。 |
| Dead letter | 失败归档 | 未实现 | 定义 record 和查询。 |
