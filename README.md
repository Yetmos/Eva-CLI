# Eva-CLI

Eva-CLI 当前处于架构方案整理阶段，仓库内主要内容是 `doc/` 下的设计文档，还不是可运行实现。

## 文档入口

建议先按以下顺序阅读：

1. [doc/总体架构方案.md](doc/总体架构方案.md)
2. [doc/Rust与Lua事件总线智能体调度架构方案.md](doc/Rust与Lua事件总线智能体调度架构方案.md)
3. [doc/Lua调用外部Agent动态Adapter架构方案.md](doc/Lua调用外部Agent动态Adapter架构方案.md)
4. [doc/Lua承载Skill-MCP-Tool热更新架构方案.md](doc/Lua承载Skill-MCP-Tool热更新架构方案.md)
5. [doc/Agent扫描与发现架构方案.md](doc/Agent扫描与发现架构方案.md)
6. [doc/外接硬件接入与热插拔架构方案.md](doc/外接硬件接入与热插拔架构方案.md)
7. [doc/项目配置方案.md](doc/项目配置方案.md)
8. [doc/进程级停机升级架构方案.md](doc/进程级停机升级架构方案.md)

补充评审：

- [doc/方案设计风险评审.md](doc/方案设计风险评审.md)

## 当前方案定位

当前方案目标是设计一套 Rust 托管运行时、Lua 热更新 Agent、Topic EventBus、动态 Adapter、MCP 双向集成、HardwareAdapter 和进程级恢复机制组合的多 Agent 调度系统。

核心边界：

- Rust 管系统边界、权限、schema、沙箱、密钥、进程生命周期、审计、超时和恢复。
- Lua 管可热更新业务逻辑、Agent 局部状态、工具调用编排和结果转换。
- Topic EventBus 管 Agent 间协作，不承担隐式全局业务状态。
- Adapter 管外部能力接入，包括 CLI、HTTP、MCP、Skill、本地模型、内部 Agent 和硬件。

## 当前主要缺口

方案已经覆盖目标架构，但仍需把 Bot 行为语义、状态一致性、权限合并、capability 注册、Lua binding、schema、错误恢复和验证不变量继续固化为可执行规格。详细风险见 [方案设计风险评审](doc/方案设计风险评审.md)。
