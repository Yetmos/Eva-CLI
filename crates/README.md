# Crates

这里用于维护 Rust workspace 中的子 crate。

建议在实现进入多模块阶段后再新增实际 crate，例如：

- `eva-runtime`
- `eva-eventbus`
- `eva-adapter`
- `eva-discovery`
- `eva-cli`

在只有一个 CLI 主程序前，可以先使用根目录 `src/`，避免过早拆分。
