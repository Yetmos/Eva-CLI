# Crates / Rust 子模块

## 中文

本目录按照 `docs/模块划分方案.md` 承载 Eva-CLI 的 Rust workspace 边界。每个 crate 代表一个稳定职责域，先提供可编译的空骨架和模块占位，后续实现应保持文档中的依赖方向。

关键规则：

- `eva-core` 保存跨模块基础契约，不依赖运行时、Adapter、Lua、MCP 或 CLI。
- `eva-runtime` 是唯一组合根，负责装配具体服务。
- `eva-cli` 是用户入口，不拥有核心运行时状态。
- 下层 crate 不反向依赖 `eva-runtime`。

## English

This directory follows `docs/en/module-partitioning.md` and hosts the Eva-CLI Rust workspace boundaries. Each crate represents a stable responsibility domain. The current files are compileable scaffolding and module placeholders; future implementation should preserve the documented dependency direction.

Key rules:

- `eva-core` contains cross-module foundation contracts and must not depend on runtime, Adapter, Lua, MCP, or CLI crates.
- `eva-runtime` is the only composition root for concrete services.
- `eva-cli` is the user entry point and does not own core runtime state.
- Lower crates must not depend back on `eva-runtime`.
