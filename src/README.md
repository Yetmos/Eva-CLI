# Eva-CLI Source / 主程序源码

## 中文

根目录 `src/main.rs` 是很薄的 binary shim，只负责把进程入口转交给 `crates/eva-cli`。核心运行时、配置、调度、Adapter、Lua host、MCP、记忆和生命周期逻辑都应放在 `crates/` 下对应 crate 中。

## English

The root `src/main.rs` is a thin binary shim that delegates process startup to `crates/eva-cli`. Core runtime, configuration, scheduling, Adapter, Lua host, MCP, memory, and lifecycle logic should live in the corresponding crates under `crates/`.
