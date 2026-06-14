# Eva-CLI 源码

这里用于维护 Eva-CLI 的主程序源码。

当前仓库仍处于架构方案整理阶段，尚未放入可运行实现。开始实现时，建议优先补齐：

- `Cargo.toml`
- `src/main.rs`
- `src/lib.rs`
- 与 `docs/` 中设计边界对应的模块目录

如果后续拆成 Rust workspace，公共库和子 crate 放入 `crates/`，主 CLI 入口仍保留在这里或迁移到 `crates/eva-cli/`。
