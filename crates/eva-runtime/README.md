# eva-runtime / 运行时组合根

## 中文

`eva-runtime` 是唯一组合根，负责把 EventBus、Scheduler、AgentRuntime、AdapterRegistry、MemoryService、MCP Server、硬件、备份和生命周期服务装配在一起。下层 crate 不应反向依赖它。

## English

`eva-runtime` is the only composition root. It wires together EventBus, Scheduler, AgentRuntime, AdapterRegistry, MemoryService, MCP Server, hardware, backup, and lifecycle services. Lower crates must not depend back on it.
