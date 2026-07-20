//! 本模块提供 `client` 相关实现。
//! In-memory MCP client abstraction for V1.1 side-effect-free probing.

use crate::policy::McpAllowlist;
use eva_core::{AdapterId, EvaError, InvokeOutput, RequestId};
use std::fmt;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP client protocol integration";

/// 表示 `McpProbeReport` 数据结构。
/// Result of a side-effect-free MCP tool probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `status` 字段对应的值。
    pub status: McpProbeStatus,
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `message` 字段对应的值。
    pub message: String,
}

/// 定义 `McpProbeStatus` 可取的状态。
/// Stable status for MCP probe output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProbeStatus {
    /// 表示 `Allowed` 枚举分支。
    Allowed,
    /// 表示 `Blocked` 枚举分支。
    Blocked,
}

/// 表示 `McpCallReport` 数据结构。
/// Result of a controlled MCP tool call envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct McpCallReport {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `output` 字段对应的值。
    pub output: InvokeOutput,
}

impl fmt::Debug for McpCallReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpCallReport")
            .field("request_id", &self.request_id)
            .field("adapter_id", &self.adapter_id)
            .field("tool", &self.tool)
            .field("output", &"[REDACTED_OUTPUT]")
            .finish()
    }
}

/// 表示 `InMemoryMcpClient` 数据结构。
/// Minimal MCP client that enforces allowlists without starting a real server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryMcpClient {
    /// 记录 `adapter_id` 字段对应的值。
    adapter_id: AdapterId,
    /// 记录 `allowlist` 字段对应的值。
    allowlist: McpAllowlist,
}

impl McpProbeStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
        }
    }
}

impl InMemoryMcpClient {
    /// 创建并初始化当前类型的实例。
    pub fn new(adapter_id: AdapterId, allowlist: McpAllowlist) -> Self {
        Self {
            adapter_id,
            allowlist,
        }
    }

    /// 返回 `adapter_id` 对应的数据视图。
    pub fn adapter_id(&self) -> &AdapterId {
        &self.adapter_id
    }

    /// 返回 `allowlist` 对应的数据视图。
    pub fn allowlist(&self) -> &McpAllowlist {
        &self.allowlist
    }

    /// 执行 `probe_tool` 对应的处理逻辑。
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

    /// 执行 `call_tool` 对应的受控流程。
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

/// 按 `escape_json` 的协议约定生成输出。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `in_memory_client_blocks_unlisted_tool` 场景下的预期行为。
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
