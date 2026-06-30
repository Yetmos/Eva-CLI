# eva-agent / Agent 运行边界

## 中文

`eva-agent` 负责 Agent 生命周期、私有队列和事件处理边界。它不拥有 Lua 沙箱内部实现，也不直接处理外部 provider transport。

## English

`eva-agent` owns Agent lifecycle, private queues, and event handling boundaries. It does not own Lua sandbox internals or external provider transports directly.
