//! Sandbox checks for the controlled V0.4 Lua host contract.

use crate::loader::LuaScript;
use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "enforce Lua sandbox policy floors";

/// Minimal denylist gate before a real Lua VM sandbox exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaSandboxPolicy {
    forbidden_tokens: Vec<&'static str>,
}

impl Default for LuaSandboxPolicy {
    fn default() -> Self {
        Self {
            forbidden_tokens: vec!["os.execute", "io.popen", "require", "dofile", "loadfile"],
        }
    }
}

impl LuaSandboxPolicy {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::LuaScript;

    #[test]
    fn sandbox_rejects_forbidden_tokens() {
        let script = LuaScript::from_source("os.execute('rm -rf .')");

        let error = LuaSandboxPolicy::default().validate(&script).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }
}
