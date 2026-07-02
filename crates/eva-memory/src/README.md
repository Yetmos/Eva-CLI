# eva-memory/src / 记忆源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载私有记忆、全局记忆、知识库和上下文构建。当前为骨架，V1.2 先实现 policy-aware context assembly。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.2 |
| `memory_service.rs` | Agent/global memory service | 骨架 | V1.2 |
| `knowledge_service.rs` | 项目知识存储和检索 | 骨架 | V1.2 |
| `context_builder.rs` | policy-aware context assembly | 骨架 | V1.2 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 memory record、scope、owner、retention。 | 私有和全局记忆可区分。 |
| 2 | 定义 KnowledgeItem、citation、digest。 | 知识来源可追溯。 |
| 3 | 实现 ContextBuilder policy filter 和 budget。 | 未授权内容不进入上下文。 |
| 4 | 接 Lua host API。 | `ctx.memory` 等受控可用。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| MemoryService | 读写/list/delete | 未实现 | 定义 scope 和权限。 |
| KnowledgeService | item/chunk/citation | 未实现 | 定义来源引用。 |
| ContextBuilder | filter/ranking/budget | 未实现 | 接 `eva-policy`。 |
