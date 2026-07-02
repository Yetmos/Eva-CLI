# eva-memory / 记忆与知识库

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-memory` 负责 Agent 私有记忆、系统总记忆库、知识库和 request-level context builder。它为 Agent 和 Lua 提供受 policy 限制的上下文入口，不改变 EventBus 的存储语义，不替代 durable event log。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| MemoryService | 骨架 | 提供 Agent 私有记忆和全局记忆的读写接口。 |
| KnowledgeService | 骨架 | 管理文档、代码片段、引用、摘要和检索结果。 |
| ContextBuilder | 骨架 | 根据 request、Agent、policy、budget 组装上下文。 |
| Policy-aware access | 未实现 | 读写 memory/knowledge 前必须检查 scope 和 allowlist。 |
| Storage integration | 未实现 | 使用 `eva-storage` 保存 memory record、knowledge artifact 和索引。 |
| Lua host API | 未实现 | V1.2 暴露 `ctx.memory`、`ctx.global_memory`、`ctx.knowledge`。 |

## 模块边界

`eva-memory` 做：

- 定义记忆和知识的记录、查询、引用、上下文预算。
- 给 Agent/Lua 提供受控 host API 后端。
- 记录 memory 读写的 audit 和 trace。

`eva-memory` 不做：

- 不保存事件递送日志。
- 不绕过 policy 读取其他 Agent 私有记忆。
- 不直接修改 Lua sandbox。
- 不替代 Adapter 或 MCP 的外部检索能力。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.2 | 定义 memory record、scope、owner、visibility、retention。 | `eva-core` | Agent 私有和全局 scope 可区分。 |
| 2 | V1.2 | 实现 MemoryService trait 和 in-memory/local store adapter。 | `eva-storage` | 私有记忆按 AgentId 隔离。 |
| 3 | V1.2 | 定义 KnowledgeItem、source ref、chunk、citation、digest。 | `eva-storage` artifact | 知识条目可追溯来源。 |
| 4 | V1.2 | 实现 ContextBuilder：request budget、policy filter、ranking 占位。 | `eva-policy` | 未授权记忆不会进入上下文。 |
| 5 | V1.2 | 接 Lua host API：`ctx.memory`、`ctx.global_memory`、`ctx.knowledge`。 | `eva-lua-host` | Lua 只能经受控 API 访问。 |
| 6 | V1.5 | 增加压缩、过期、索引重建、敏感信息扫描。 | storage/observability | 上下文规模可控且可审计。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export memory_service、knowledge_service、context_builder。 |
| `src/memory_service.rs` | Agent/global memory | `RESPONSIBILITY` 占位 | 定义 record、scope、read/write/list/delete API。 |
| `src/knowledge_service.rs` | 知识库 | `RESPONSIBILITY` 占位 | 定义 knowledge item、citation、index contract。 |
| `src/context_builder.rs` | policy-aware 上下文组装 | `RESPONSIBILITY` 占位 | 定义 context request、budget、filter、result。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | memory/context | 未开始 | 覆盖私有隔离、policy filter、budget 截断。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.2 | `cargo test -p eva-memory` | memory、knowledge、context builder 可测。 |
| V1.2 | `cargo test -p eva-lua-host` | Lua API 只能访问授权上下文。 |
| V1.5 | privacy regression tests | 私有记忆不跨 Agent 泄露。 |

## English

`eva-memory` owns private memory, global memory, knowledge storage, and policy-aware context assembly. It does not replace EventBus storage or bypass policy.
