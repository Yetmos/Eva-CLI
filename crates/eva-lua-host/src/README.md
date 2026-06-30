# eva-lua-host/src / Lua 宿主源码

## 中文

这里保存 Lua 加载、沙箱、host binding 和热更新逻辑。Lua 可见能力必须通过 typed host API 暴露，不能直接打开文件、网络、shell、MCP 或硬件访问。

## English

This directory contains Lua loading, sandboxing, host bindings, and hot reload logic. Lua-visible capabilities must be exposed through typed host APIs and must not directly open filesystem, network, shell, MCP, or hardware access.
