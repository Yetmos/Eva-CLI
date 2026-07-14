//! Eva 可执行文件入口；具体 CLI 行为由 `eva-cli` crate 统一实现。

/// Eva 命令行二进制入口；所有解析、输出和退出码语义委托给 `eva-cli`，避免入口层复制逻辑。
fn main() {
    eva_cli::run();
}
