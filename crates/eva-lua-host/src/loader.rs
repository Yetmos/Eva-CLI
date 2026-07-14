//! 将 Lua 文件读取为待校验的源码数据。
//!
//! 加载阶段不执行脚本；I/O 失败会保留安全的路径与底层错误上下文，后续沙箱和 VM 边界仍
//! 必须独立完成权限与资源检查。
//! Lua script loading for the controlled V0.4 host contract.

use eva_core::EvaError;
use std::fs;
use std::path::{Path, PathBuf};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load Lua scripts into controlled state generations";

/// 表示 `LuaScript` 数据结构。
/// Loaded Lua source. V0.4 keeps source as data and does not embed a VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaScript {
    /// 记录 `path` 字段对应的值。
    pub path: Option<PathBuf>,
    /// 记录 `source` 字段对应的值。
    source: String,
}

impl LuaScript {
    /// 读取或解析 `load` 所需的数据，失败时保留错误语义。
    pub fn load(path: impl AsRef<Path>) -> Result<Self, EvaError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|error| {
            EvaError::not_found("failed to read Lua script")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            source,
        })
    }

    /// 根据输入构造当前类型，作为 `from_source` 的标准入口。
    pub fn from_source(source: impl Into<String>) -> Self {
        Self {
            path: None,
            source: source.into(),
        }
    }

    /// 返回 `source` 对应的数据视图。
    pub fn source(&self) -> &str {
        &self.source
    }
}
