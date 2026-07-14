//! 为 MCP 工具、资源和提示提供默认拒绝的显式允许列表。
//!
//! 名称在进入集合前必须是去除空白后的稳定非空标识；查询与拒绝均无副作用，因此调用方可
//! 保证未授权请求在任何进程启动或网络 I/O 之前被阻断。
//! MCP tool/resource/prompt allowlist policy helpers.

use eva_core::EvaError;
use std::collections::BTreeSet;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP policy helper evaluation";

/// 表示 `McpAllowlist` 数据结构。
/// Side-effect-free allowlist used before an MCP client or Adapter may call a tool.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpAllowlist {
    /// 记录 `tools` 字段对应的值。
    tools: BTreeSet<String>,
    /// 记录 `resources` 字段对应的值。
    resources: BTreeSet<String>,
    /// 记录 `prompts` 字段对应的值。
    prompts: BTreeSet<String>,
}

impl McpAllowlist {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 根据输入构造当前类型，作为 `from_tools` 的标准入口。
    pub fn from_tools<I, S>(tools: I) -> Result<Self, EvaError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut allowlist = Self::new();
        for tool in tools {
            allowlist.allow_tool(tool)?;
        }
        Ok(allowlist)
    }

    /// 执行 `allow_tool` 对应的处理逻辑。
    pub fn allow_tool(&mut self, tool: impl Into<String>) -> Result<(), EvaError> {
        let tool = validate_token("tool", tool.into())?;
        self.tools.insert(tool);
        Ok(())
    }

    /// 执行 `allow_resource` 对应的处理逻辑。
    pub fn allow_resource(&mut self, resource: impl Into<String>) -> Result<(), EvaError> {
        let resource = validate_token("resource", resource.into())?;
        self.resources.insert(resource);
        Ok(())
    }

    /// 执行 `allow_prompt` 对应的处理逻辑。
    pub fn allow_prompt(&mut self, prompt: impl Into<String>) -> Result<(), EvaError> {
        let prompt = validate_token("prompt", prompt.into())?;
        self.prompts.insert(prompt);
        Ok(())
    }

    /// 执行 `tools` 对应的处理逻辑。
    pub fn tools(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(String::as_str)
    }

    /// 执行 `resources` 对应的处理逻辑。
    pub fn resources(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(String::as_str)
    }

    /// 执行 `prompts` 对应的处理逻辑。
    pub fn prompts(&self) -> impl Iterator<Item = &str> {
        self.prompts.iter().map(String::as_str)
    }

    /// 判断 `permits_tool` 对应的条件是否成立。
    pub fn permits_tool(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    /// 校验 `require_tool` 对应的约束，不满足时返回明确错误。
    pub fn require_tool(&self, tool: &str) -> Result<(), EvaError> {
        if self.permits_tool(tool) {
            Ok(())
        } else {
            Err(EvaError::permission_denied("MCP tool is not allowlisted")
                .with_context("tool", tool))
        }
    }
}

/// 校验 `validate_token` 对应的约束，不满足时返回明确错误。
fn validate_token(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.is_empty() || value.trim() != value {
        return Err(EvaError::invalid_argument(
            "MCP allowlist token must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument("MCP allowlist token cannot contain whitespace")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `allowlist_rejects_unknown_tool` 场景下的预期行为。
    #[test]
    fn allowlist_rejects_unknown_tool() {
        let allowlist = McpAllowlist::from_tools(["list_issues"]).unwrap();

        assert!(allowlist.require_tool("list_issues").is_ok());
        assert_eq!(
            allowlist.require_tool("delete_repo").unwrap_err().kind(),
            eva_core::ErrorKind::PermissionDenied
        );
    }
}
