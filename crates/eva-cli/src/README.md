# eva-cli/src / CLI 源码

## 中文

这里保存用户可见命令入口。CLI 负责解析命令并调用 runtime，不直接拥有调度、Agent、Adapter 或状态存储的核心所有权。

## English

This directory contains user-facing command entry points. The CLI parses commands and calls the runtime; it does not directly own scheduling, Agent, Adapter, or state storage core ownership.
