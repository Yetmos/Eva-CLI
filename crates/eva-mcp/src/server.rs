//! 本模块提供 `server` 相关实现。
//! Controlled Eva MCP server surface descriptors.

use crate::json_rpc::{parse_json_object_fields, parse_json_string, parse_json_string_array};
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "controlled MCP server exposure";

const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 16 * 1024;
const ADAPTER_LIST_SCHEMA: &str = r#"{"type":"object","additionalProperties":false}"#;
const ADAPTER_PROBE_SCHEMA: &str = r#"{"type":"object","properties":{"adapter_id":{"type":"string"}},"required":["adapter_id"],"additionalProperties":false}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolInputSchema {
    properties: BTreeSet<String>,
    required: BTreeSet<String>,
}

impl ToolInputSchema {
    fn parse(schema: &str) -> Result<Self, EvaError> {
        let fields = parse_json_object_fields(schema).map_err(|_| schema_error())?;
        if fields.keys().any(|name| {
            !matches!(
                name.as_str(),
                "type" | "properties" | "required" | "additionalProperties"
            )
        }) {
            return Err(schema_error());
        }
        if fields
            .get("type")
            .map(|value| parse_json_string(value))
            .transpose()
            .map_err(|_| schema_error())?
            .as_deref()
            != Some("object")
            || fields.get("additionalProperties").copied() != Some("false")
        {
            return Err(schema_error());
        }
        let property_fields = fields
            .get("properties")
            .map(|value| parse_json_object_fields(value))
            .transpose()
            .map_err(|_| schema_error())?
            .unwrap_or_default();
        let mut properties = BTreeSet::new();
        for (name, property_schema) in property_fields {
            if name.is_empty()
                || name.len() > MAX_TOOL_NAME_BYTES
                || name.chars().any(char::is_control)
            {
                return Err(schema_error());
            }
            let property = parse_json_object_fields(property_schema).map_err(|_| schema_error())?;
            if property.len() != 1
                || property
                    .get("type")
                    .map(|value| parse_json_string(value))
                    .transpose()
                    .map_err(|_| schema_error())?
                    .as_deref()
                    != Some("string")
            {
                return Err(schema_error());
            }
            properties.insert(name);
        }
        let required_values = fields
            .get("required")
            .map(|value| parse_json_string_array(value))
            .transpose()
            .map_err(|_| schema_error())?
            .unwrap_or_default();
        let mut required = BTreeSet::new();
        for name in required_values {
            if !properties.contains(&name) || !required.insert(name) {
                return Err(schema_error());
            }
        }
        Ok(Self {
            properties,
            required,
        })
    }

    fn validate_arguments(&self, arguments: &str) -> Result<(), EvaError> {
        let fields = parse_json_object_fields(arguments).map_err(|_| arguments_error())?;
        if fields.keys().any(|name| !self.properties.contains(name)) {
            return Err(EvaError::invalid_argument(
                "MCP server tool arguments contain an unknown field",
            )
            .with_provider_code("mcp_server_tool_argument_unknown"));
        }
        if self.required.iter().any(|name| !fields.contains_key(name)) {
            return Err(
                EvaError::invalid_argument("MCP server tool required argument is missing")
                    .with_provider_code("mcp_server_tool_argument_missing"),
            );
        }
        for name in &self.properties {
            if let Some(value) = fields.get(name) {
                parse_json_string(value).map_err(|_| {
                    EvaError::invalid_argument("MCP server tool string argument is invalid")
                        .with_provider_code("mcp_server_tool_argument_type_invalid")
                })?;
            }
        }
        Ok(())
    }
}

fn schema_error() -> EvaError {
    EvaError::invalid_argument("MCP server tool input schema is unsupported or invalid")
        .with_provider_code("mcp_server_tool_schema_invalid")
}

fn arguments_error() -> EvaError {
    EvaError::invalid_argument("MCP server tool arguments must be a JSON object")
        .with_provider_code("mcp_server_tool_arguments_invalid")
}

/// 表示 `McpServerTool` 数据结构。
/// Tool exposed by the controlled Eva MCP server surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerTool {
    /// 记录 `name` 字段对应的值。
    pub name: String,
    /// 记录 `description` 字段对应的值。
    pub description: String,
    /// A validated JSON object emitted as the MCP `inputSchema`.
    pub input_schema: String,
    /// 记录 `side_effects` 字段对应的值。
    pub side_effects: bool,
}

impl McpServerTool {
    /// Construct one explicitly exposed tool descriptor.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: impl Into<String>,
        side_effects: bool,
    ) -> Result<Self, EvaError> {
        let tool = Self {
            name: name.into(),
            description: description.into(),
            input_schema: input_schema.into(),
            side_effects,
        };
        tool.validate()?;
        Ok(tool)
    }

    fn validate(&self) -> Result<(), EvaError> {
        if self.name.is_empty()
            || self.name.len() > MAX_TOOL_NAME_BYTES
            || !self.name.is_ascii()
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(EvaError::invalid_argument(
                "MCP server tool name must be bounded stable ASCII",
            )
            .with_provider_code("mcp_server_tool_name_invalid"));
        }
        if self.description.is_empty()
            || self.description.len() > MAX_TOOL_DESCRIPTION_BYTES
            || self.description.chars().any(char::is_control)
        {
            return Err(EvaError::invalid_argument(
                "MCP server tool description must be bounded display text",
            )
            .with_provider_code("mcp_server_tool_description_invalid"));
        }
        if self.input_schema.is_empty() || self.input_schema.len() > MAX_TOOL_SCHEMA_BYTES {
            return Err(EvaError::invalid_argument(
                "MCP server tool input schema must be a bounded JSON object",
            )
            .with_provider_code("mcp_server_tool_schema_invalid"));
        }
        ToolInputSchema::parse(&self.input_schema)?;
        Ok(())
    }
}

/// 表示 `EvaMcpServerSurface` 数据结构。
/// Explicit server surface consumed by a separately owned transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaMcpServerSurface {
    /// 记录 `tools` 字段对应的值。
    tools: Vec<McpServerTool>,
}

/// 表示 `McpServerToolGateReport` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerToolGateReport {
    /// The admitted name, omitted when the candidate was not exposed.
    pub exposed_tool: Option<String>,
    /// 记录 `allowed` 字段对应的值。
    pub allowed: bool,
    /// 记录 `reason` 字段对应的值。
    pub reason: Option<String>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

impl fmt::Debug for McpServerToolGateReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerToolGateReport")
            .field("tool_exposed", &self.exposed_tool.is_some())
            .field("allowed", &self.allowed)
            .field("reason", &self.reason)
            .field("audit", &self.audit)
            .finish()
    }
}

/// One validated tool call delivered to an explicitly configured handler.
pub struct McpServerToolCall<'a> {
    /// The stable name already admitted by the server surface.
    tool: &'a str,
    /// A syntactically validated JSON object. Its contents remain untrusted.
    arguments_json: &'a str,
}

impl<'a> McpServerToolCall<'a> {
    pub(crate) fn admitted(tool: &'a str, arguments_json: &'a str) -> Result<Self, EvaError> {
        parse_json_object_fields(arguments_json).map_err(|_| {
            EvaError::invalid_argument("MCP server tool arguments must be a JSON object")
                .with_provider_code("mcp_server_tool_arguments_invalid")
        })?;
        Ok(Self {
            tool,
            arguments_json,
        })
    }

    /// Return the explicitly admitted tool name.
    pub fn tool(&self) -> &str {
        self.tool
    }

    /// Return the complete validated arguments object.
    pub fn arguments_json(&self) -> &str {
        self.arguments_json
    }

    /// Read one direct optional string argument. Nested fields never match.
    pub fn string_argument(&self, name: &str) -> Result<Option<String>, EvaError> {
        self.arguments()?
            .get(name)
            .map(|value| {
                parse_json_string(value).map_err(|_| {
                    EvaError::invalid_argument("MCP server tool string argument is invalid")
                        .with_provider_code("mcp_server_tool_argument_type_invalid")
                })
            })
            .transpose()
    }

    /// Read one required direct string argument.
    pub fn required_string_argument(&self, name: &str) -> Result<String, EvaError> {
        self.string_argument(name)?.ok_or_else(|| {
            EvaError::invalid_argument("MCP server tool required argument is missing")
                .with_provider_code("mcp_server_tool_argument_missing")
        })
    }

    /// Reject direct arguments outside the handler's explicit contract.
    pub fn require_only_arguments(&self, allowed: &[&str]) -> Result<(), EvaError> {
        let fields = self.arguments()?;
        if fields
            .keys()
            .any(|name| !allowed.iter().any(|allowed| *allowed == name))
        {
            return Err(EvaError::invalid_argument(
                "MCP server tool arguments contain an unknown field",
            )
            .with_provider_code("mcp_server_tool_argument_unknown"));
        }
        Ok(())
    }

    fn arguments(&self) -> Result<BTreeMap<String, &str>, EvaError> {
        parse_json_object_fields(self.arguments_json).map_err(|_| {
            EvaError::invalid_argument("MCP server tool arguments must be a JSON object")
                .with_provider_code("mcp_server_tool_arguments_invalid")
        })
    }
}

impl fmt::Debug for McpServerToolCall<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerToolCall")
            .field("tool", &self.tool)
            .field("arguments", &"[REDACTED_ARGUMENTS]")
            .field("argument_bytes", &self.arguments_json.len())
            .finish()
    }
}

/// Bounded text returned by a controlled MCP tool handler.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerToolResult {
    text: String,
    is_error: bool,
}

impl McpServerToolResult {
    /// Build a text content result. The transport applies its response bound
    /// and JSON escaping before any bytes are written to the client.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    /// Build a controlled tool-level error result.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    pub(crate) fn text_content(&self) -> &str {
        &self.text
    }

    pub(crate) fn is_error_result(&self) -> bool {
        self.is_error
    }
}

impl fmt::Debug for McpServerToolResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerToolResult")
            .field("text", &"[REDACTED_OUTPUT]")
            .field("text_bytes", &self.text.len())
            .field("is_error", &self.is_error)
            .finish()
    }
}

/// Runtime boundary invoked only after transport, session, method, and tool
/// gates have all succeeded.
pub trait McpServerToolHandler: Send {
    /// Execute one admitted tool call.
    fn call_tool(
        &mut self,
        request: McpServerToolCall<'_>,
    ) -> Result<McpServerToolResult, EvaError>;
}

impl EvaMcpServerSurface {
    /// Build a deterministic explicit surface and reject duplicate or invalid
    /// descriptors before a server can accept connections.
    pub fn new(tools: Vec<McpServerTool>) -> Result<Self, EvaError> {
        if tools.is_empty() {
            return Err(EvaError::invalid_argument(
                "MCP server surface must expose at least one explicit tool",
            )
            .with_provider_code("mcp_server_surface_empty"));
        }
        let mut names = BTreeSet::new();
        for tool in &tools {
            tool.validate()?;
            if !names.insert(tool.name.clone()) {
                return Err(
                    EvaError::conflict("MCP server surface contains a duplicate tool")
                        .with_provider_code("mcp_server_tool_duplicate"),
                );
            }
        }
        Ok(Self { tools })
    }

    /// 执行 `v11_minimal` 对应的处理逻辑。
    pub fn v11_minimal() -> Self {
        Self::new(vec![
            McpServerTool::new(
                "adapter.list",
                "List authorized Eva adapter handles",
                ADAPTER_LIST_SCHEMA,
                false,
            )
            .expect("the built-in MCP tool descriptor is valid"),
            McpServerTool::new(
                "adapter.probe",
                "Probe an authorized Eva adapter without invoking it",
                ADAPTER_PROBE_SCHEMA,
                false,
            )
            .expect("the built-in MCP tool descriptor is valid"),
        ])
        .expect("the built-in MCP server surface is valid")
    }

    /// 执行 `tools` 对应的处理逻辑。
    pub fn tools(&self) -> &[McpServerTool] {
        &self.tools
    }

    pub(crate) fn tool(&self, name: &str) -> Option<&McpServerTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub(crate) fn validate_tool_arguments(
        &self,
        tool: &str,
        arguments: &str,
    ) -> Result<(), EvaError> {
        let descriptor = self.tool(tool).ok_or_else(|| {
            EvaError::permission_denied("MCP server tool is not explicitly exposed")
                .with_provider_code("mcp_server_tool_not_exposed")
        })?;
        ToolInputSchema::parse(&descriptor.input_schema)?.validate_arguments(arguments)
    }

    /// 执行 `gate_tool` 对应的处理逻辑。
    pub fn gate_tool(&self, tool: &str) -> McpServerToolGateReport {
        if self.tools.iter().any(|entry| entry.name == tool) {
            McpServerToolGateReport {
                exposed_tool: Some(tool.to_owned()),
                allowed: true,
                reason: None,
                audit: vec![
                    "mcp.server.tool:allowed".to_owned(),
                    "mcp.server.tool_name:explicit".to_owned(),
                    "proxy_boundary:explicit_tools_only".to_owned(),
                ],
            }
        } else {
            McpServerToolGateReport {
                exposed_tool: None,
                allowed: false,
                reason: Some("tool is not explicitly exposed".to_owned()),
                audit: vec![
                    "mcp.server.tool:blocked".to_owned(),
                    "mcp.server.tool_name:redacted".to_owned(),
                    "proxy_boundary:explicit_tools_only".to_owned(),
                ],
            }
        }
    }

    /// 校验 `require_tool` 对应的约束，不满足时返回明确错误。
    pub fn require_tool(&self, tool: &str) -> Result<McpServerToolGateReport, EvaError> {
        let report = self.gate_tool(tool);
        if report.allowed {
            Ok(report)
        } else {
            Err(
                EvaError::permission_denied("MCP server tool is not explicitly exposed")
                    .with_provider_code("mcp_server_tool_not_exposed")
                    .with_context("proxy_boundary", "explicit_tools_only"),
            )
        }
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `minimal_server_surface_exposes_only_side_effect_free_tools` 场景下的预期行为。
    #[test]
    fn minimal_server_surface_exposes_only_side_effect_free_tools() {
        let surface = EvaMcpServerSurface::v11_minimal();

        assert!(surface
            .tools()
            .iter()
            .any(|tool| tool.name == "adapter.list"));
        assert!(surface.tools().iter().all(|tool| !tool.side_effects));
    }

    /// 验证 `illegal_proxy_request_is_rejected_with_audit` 场景下的预期行为。
    #[test]
    fn illegal_proxy_request_is_rejected_with_audit() {
        let surface = EvaMcpServerSurface::v11_minimal();

        let gate = surface.gate_tool("topic.publish");
        let error = surface.require_tool("topic.publish").unwrap_err();

        assert!(!gate.allowed);
        assert!(gate
            .audit
            .contains(&"proxy_boundary:explicit_tools_only".to_owned()));
        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_server_tool_not_exposed")
        );
        assert!(!format!("{gate:?}").contains("topic.publish"));
        assert!(!format!("{error:?}").contains("topic.publish"));
    }

    #[test]
    fn server_surface_rejects_invalid_and_duplicate_tools() {
        assert!(McpServerTool::new("bad tool", "description", ADAPTER_LIST_SCHEMA, false).is_err());
        assert!(McpServerTool::new("valid", "description", "{}", false).is_err());
        assert!(McpServerTool::new(
            "valid",
            "description",
            r#"{"type":"object","additionalProperties":true}"#,
            false
        )
        .is_err());
        let tool =
            McpServerTool::new("adapter.list", "description", ADAPTER_LIST_SCHEMA, false).unwrap();
        let error = EvaMcpServerSurface::new(vec![tool.clone(), tool]).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_server_tool_duplicate")
        );
    }

    #[test]
    fn handler_envelopes_redact_arguments_and_output() {
        let call =
            McpServerToolCall::admitted("adapter.list", "{\"secret\":\"opaque-value\"}").unwrap();
        let result = McpServerToolResult::text("opaque-result");

        assert!(!format!("{call:?}").contains("opaque-value"));
        assert!(!format!("{result:?}").contains("opaque-result"));
    }

    #[test]
    fn tool_call_reads_only_direct_string_arguments() {
        let call = McpServerToolCall::admitted("adapter.probe", "{\"adapter_id\":\"github-mcp\"}")
            .unwrap();

        call.require_only_arguments(&["adapter_id"]).unwrap();
        assert_eq!(
            call.required_string_argument("adapter_id").unwrap(),
            "github-mcp"
        );
        assert!(McpServerToolCall::admitted(
            "adapter.probe",
            "{\"nested\":{\"adapter_id\":\"hidden\"}}"
        )
        .unwrap()
        .required_string_argument("adapter_id")
        .is_err());
    }

    #[test]
    fn advertised_schema_rejects_missing_wrong_and_extra_arguments() {
        let surface = EvaMcpServerSurface::v11_minimal();

        surface
            .validate_tool_arguments("adapter.list", "{}")
            .unwrap();
        assert!(surface
            .validate_tool_arguments("adapter.list", "{\"extra\":\"value\"}")
            .is_err());
        assert!(surface
            .validate_tool_arguments("adapter.probe", "{}")
            .is_err());
        assert!(surface
            .validate_tool_arguments("adapter.probe", "{\"adapter_id\":1}")
            .is_err());
        surface
            .validate_tool_arguments("adapter.probe", "{\"adapter_id\":\"github-mcp\"}")
            .unwrap();
    }
}
