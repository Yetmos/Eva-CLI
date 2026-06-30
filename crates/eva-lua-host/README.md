# eva-lua-host / Lua 执行宿主

## 中文

`eva-lua-host` 负责 Lua State 加载、沙箱、host binding 和热更新。它只能调用 typed host API trait，不能直接访问文件、网络、shell、MCP 或硬件实现。

## English

`eva-lua-host` owns Lua state loading, sandboxing, host bindings, and hot reload. It may call typed host API traits only and must not directly access file, network, shell, MCP, or hardware implementations.
