//! 对 Lua 源码实施宿主 API 的最低安全门禁。
//!
//! 这里的令牌拒绝列表是执行前的快速防线，用于产生清晰的权限错误，并不替代 VM 的标准库
//! 裁剪、只读表和资源限额。任何命中都整体拒绝脚本，不尝试删除或重写源码。
//! Sandbox checks for the controlled V0.4 Lua host contract.

use crate::loader::LuaScript;
use eva_core::EvaError;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "enforce Lua sandbox policy floors";

/// 表示 `LuaSandboxPolicy` 数据结构。
/// Minimal denylist gate before a real Lua VM sandbox exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaSandboxPolicy {
    /// 记录 `forbidden_tokens` 字段对应的值。
    forbidden_tokens: Vec<&'static str>,
}

impl Default for LuaSandboxPolicy {
    /// 创建启用最低宿主 API 拒绝列表的策略。
    fn default() -> Self {
        Self {
            forbidden_tokens: vec!["os.execute", "io.popen", "require", "dofile", "loadfile"],
        }
    }
}

impl LuaSandboxPolicy {
    /// 校验 `validate` 对应的约束，不满足时返回明确错误。
    pub fn validate(&self, script: &LuaScript) -> Result<(), EvaError> {
        for token in &self.forbidden_tokens {
            if script.source().contains(token) {
                return Err(
                    EvaError::permission_denied("Lua script uses a forbidden host API")
                        .with_context("token", *token),
                );
            }
        }
        Ok(())
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::LuaScript;

    /// 验证 `sandbox_rejects_forbidden_tokens` 场景下的预期行为。
    #[test]
    fn sandbox_rejects_forbidden_tokens() {
        let script = LuaScript::from_source("os.execute('rm -rf .')");

        let error = LuaSandboxPolicy::default().validate(&script).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }
}
