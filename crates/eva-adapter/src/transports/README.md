# Adapter Transports / Adapter 传输实现

## 中文

本目录保存受控 transport 边界。具体 provider、MCP、shell、HTTP、skill 和硬件细节只能留在这里或对应 service crate 中，不能向内泄漏到 Scheduler、Lua 或 core types。

## English

This directory contains controlled transport boundaries. Provider, MCP, shell, HTTP, skill, and hardware details must stay here or in the corresponding service crates; they must not leak inward into Scheduler, Lua, or core types.
