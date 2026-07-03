//! In-memory MCP client abstraction for V1.1 side-effect-free probing.

use crate::policy::McpAllowlist;
use eva_core::{AdapterId, EvaError, InvokeOutput, RequestId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP client protocol integration";

/// Result of a side-effect-free MCP tool probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeReport {
    pub adapter_id: AdapterId,
    pub status: McpProbeStatus,
    pub tool: String,
    pub message: String,
}

/// Stable status for MCP probe output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProbeStatus {
    Allowed,
    Blocked,
}

/// Result of a controlled MCP tool call envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCallReport {
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub tool: String,
    pub output: InvokeOutput,
}

/// Minimal MCP client that enforces allowlists without starting a real server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryMcpClient {
    adapter_id: AdapterId,
    allowlist: McpAllowlist,
}

impl McpProbeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
        }
    }
}

impl InMemoryMcpClient {
    pub fn new(adapter_id: AdapterId, allowlist: McpAllowlist) -> Self {
        Self {
            adapter_id,
            allowlist,
        }
    }

    pub fn adapter_id(&self) -> &AdapterId {
        &self.adapter_id
    }

    pub fn allowlist(&self) -> &McpAllowlist {
        &self.allowlist
    }

    pub fn probe_tool(&self, tool: &str) -> McpProbeReport {
        if self.allowlist.permits_tool(tool) {
            McpProbeReport {
                adapter_id: self.adapter_id.clone(),
                status: McpProbeStatus::Allowed,
                tool: tool.to_owned(),
                message: "tool is allowlisted for controlled MCP invocation".to_owned(),
            }
        } else {
            McpProbeReport {
                adapter_id: self.adapter_id.clone(),
                status: McpProbeStatus::Blocked,
                tool: tool.to_owned(),
                message: "tool is not in the MCP allowlist".to_owned(),
            }
        }
    }

    pub fn call_tool(
        &self,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpCallReport, EvaError> {
        self.allowlist.require_tool(tool)?;
        let output = format!(
            "{{\"transport\":\"mcp\",\"adapter_id\":\"{}\",\"tool\":\"{}\",\"mode\":\"controlled-envelope\",\"input\":\"{}\"}}",
            escape_json(self.adapter_id.as_str()),
            escape_json(tool),
            escape_json(input)
        );
        Ok(McpCallReport {
            request_id,
            adapter_id: self.adapter_id.clone(),
            tool: tool.to_owned(),
            output: InvokeOutput::text(output),
        })
    }
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value => escaped.push(value),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_client_blocks_unlisted_tool() {
        let client = InMemoryMcpClient::new(
            AdapterId::parse("github-mcp").unwrap(),
            McpAllowlist::from_tools(["list_issues"]).unwrap(),
        );

        assert_eq!(
            client.probe_tool("list_issues").status,
            McpProbeStatus::Allowed
        );
        assert!(client
            .call_tool(RequestId::parse("req-1").unwrap(), "delete_repo", "{}")
            .is_err());
    }
}
