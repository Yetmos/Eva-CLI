//! MCP tool/resource/prompt allowlist policy helpers.

use eva_core::EvaError;
use std::collections::BTreeSet;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP policy helper evaluation";

/// Side-effect-free allowlist used before an MCP client or Adapter may call a tool.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpAllowlist {
    tools: BTreeSet<String>,
    resources: BTreeSet<String>,
    prompts: BTreeSet<String>,
}

impl McpAllowlist {
    pub fn new() -> Self {
        Self::default()
    }

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

    pub fn allow_tool(&mut self, tool: impl Into<String>) -> Result<(), EvaError> {
        let tool = validate_token("tool", tool.into())?;
        self.tools.insert(tool);
        Ok(())
    }

    pub fn allow_resource(&mut self, resource: impl Into<String>) -> Result<(), EvaError> {
        let resource = validate_token("resource", resource.into())?;
        self.resources.insert(resource);
        Ok(())
    }

    pub fn allow_prompt(&mut self, prompt: impl Into<String>) -> Result<(), EvaError> {
        let prompt = validate_token("prompt", prompt.into())?;
        self.prompts.insert(prompt);
        Ok(())
    }

    pub fn tools(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(String::as_str)
    }

    pub fn resources(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(String::as_str)
    }

    pub fn prompts(&self) -> impl Iterator<Item = &str> {
        self.prompts.iter().map(String::as_str)
    }

    pub fn permits_tool(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    pub fn require_tool(&self, tool: &str) -> Result<(), EvaError> {
        if self.permits_tool(tool) {
            Ok(())
        } else {
            Err(EvaError::permission_denied("MCP tool is not allowlisted")
                .with_context("tool", tool))
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
