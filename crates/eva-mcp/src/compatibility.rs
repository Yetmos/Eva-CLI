//! 本模块提供 `compatibility` 相关实现。
//! MCP compatibility matrix and stream lifecycle evidence.

use crate::session::McpServerTransport;
use eva_core::EvaError;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP transport, schema, and stream lifecycle compatibility matrix";

/// MCP compatibility 数据来自受控 fixture 还是真实 server run。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCompatibilityEvidenceKind {
    /// 静态构造的兼容矩阵，只能支持 alpha 展示。
    Fixture,
    /// 由命名 MCP server 实际运行生成的兼容结果。
    Measurement,
}

impl McpCompatibilityEvidenceKind {
    /// 返回跨 crate 映射使用的稳定小写标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Measurement => "measurement",
        }
    }
}

/// 表示 `McpCompatibilityMatrix` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityMatrix {
    /// compatibility 数据的来源强度。
    evidence_kind: McpCompatibilityEvidenceKind,
    /// 记录 `protocol_version` 字段对应的值。
    pub protocol_version: String,
    /// 记录 `transports` 字段对应的值。
    pub transports: Vec<McpTransportCompatibility>,
    /// 记录 `tool_schemas` 字段对应的值。
    pub tool_schemas: Vec<McpToolSchemaCompatibility>,
    /// 记录 `stream_lifecycle` 字段对应的值。
    pub stream_lifecycle: McpStreamLifecycleCompatibility,
    /// 记录 `server_surface` 字段对应的值。
    pub server_surface: McpServerSurfaceCompatibility,
}

/// 表示 `McpTransportCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTransportCompatibility {
    /// 记录 `transport` 字段对应的值。
    pub transport: McpServerTransport,
    /// 记录 `json_rpc` 字段对应的值。
    pub json_rpc: bool,
    /// 记录 `auth_headers` 字段对应的值。
    pub auth_headers: bool,
    /// 记录 `timeout_and_output_limits` 字段对应的值。
    pub timeout_and_output_limits: bool,
    /// 记录 `stream_lifecycle` 字段对应的值。
    pub stream_lifecycle: bool,
}

/// 表示 `McpToolSchemaCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolSchemaCompatibility {
    /// 记录 `tool` 字段对应的值。
    pub tool: String,
    /// 记录 `input_schema` 字段对应的值。
    pub input_schema: String,
    /// 记录 `output_content` 字段对应的值。
    pub output_content: String,
    /// 记录 `schema_checked` 字段对应的值。
    pub schema_checked: bool,
}

/// 表示 `McpStreamLifecycleCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamLifecycleCompatibility {
    /// 记录 `start` 字段对应的值。
    pub start: bool,
    /// 记录 `abort` 字段对应的值。
    pub abort: bool,
    /// 记录 `cleanup` 字段对应的值。
    pub cleanup: bool,
    /// 记录 `dangling_sessions_after_abort` 字段对应的值。
    pub dangling_sessions_after_abort: u32,
}

/// 表示 `McpServerSurfaceCompatibility` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSurfaceCompatibility {
    /// 记录 `explicit_tool_gate` 字段对应的值。
    pub explicit_tool_gate: bool,
    /// 记录 `unlimited_proxy_exposed` 字段对应的值。
    pub unlimited_proxy_exposed: bool,
}

/// 表示 `McpCompatibilityReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityReport {
    /// verifier 保留的 fixture 或 measurement 来源分类。
    evidence_kind: McpCompatibilityEvidenceKind,
    /// 记录 `protocol_version` 字段对应的值。
    pub protocol_version: String,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `transport_count` 字段对应的值。
    pub transport_count: usize,
    /// 记录 `tool_schema_count` 字段对应的值。
    pub tool_schema_count: usize,
    /// 记录 `failures` 字段对应的值。
    pub failures: Vec<String>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

impl McpCompatibilityReport {
    /// 返回 verifier 从受控 matrix 保留的 evidence 来源分类。
    pub const fn evidence_kind(&self) -> McpCompatibilityEvidenceKind {
        self.evidence_kind
    }
}

impl McpCompatibilityMatrix {
    /// 返回由受控构造路径固定的 evidence 来源分类。
    pub const fn evidence_kind(&self) -> McpCompatibilityEvidenceKind {
        self.evidence_kind
    }

    /// 执行 `v1137_fixture` 对应的处理逻辑。
    pub fn v1137_fixture() -> Self {
        Self {
            evidence_kind: McpCompatibilityEvidenceKind::Fixture,
            protocol_version: "2025-11-25".to_owned(),
            transports: vec![
                McpTransportCompatibility {
                    transport: McpServerTransport::Stdio,
                    json_rpc: true,
                    auth_headers: false,
                    timeout_and_output_limits: true,
                    stream_lifecycle: true,
                },
                McpTransportCompatibility {
                    transport: McpServerTransport::Http,
                    json_rpc: true,
                    auth_headers: true,
                    timeout_and_output_limits: true,
                    stream_lifecycle: true,
                },
            ],
            tool_schemas: vec![McpToolSchemaCompatibility {
                tool: "list_issues".to_owned(),
                input_schema: "object".to_owned(),
                output_content: "content[].text".to_owned(),
                schema_checked: true,
            }],
            stream_lifecycle: McpStreamLifecycleCompatibility {
                start: true,
                abort: true,
                cleanup: true,
                dangling_sessions_after_abort: 0,
            },
            server_surface: McpServerSurfaceCompatibility {
                explicit_tool_gate: true,
                unlimited_proxy_exposed: false,
            },
        }
    }

    /// 执行 `verify` 对应的处理逻辑。
    pub fn verify(&self) -> Result<McpCompatibilityReport, EvaError> {
        if self.protocol_version.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "MCP compatibility protocol version must be non-empty",
            ));
        }

        let mut failures = Vec::new();
        if !self
            .transports
            .iter()
            .any(|entry| entry.transport == McpServerTransport::Stdio && entry.json_rpc)
        {
            failures.push("transport:stdio_json_rpc_missing".to_owned());
        }
        if !self
            .transports
            .iter()
            .any(|entry| entry.transport.is_http() && entry.json_rpc)
        {
            failures.push("transport:http_json_rpc_missing".to_owned());
        }
        if !self
            .transports
            .iter()
            .any(|entry| entry.transport.is_http() && entry.auth_headers)
        {
            failures.push("transport:http_auth_headers_missing".to_owned());
        }
        failures.extend(
            self.transports
                .iter()
                .filter(|entry| !entry.timeout_and_output_limits)
                .map(|entry| format!("transport:{}_limits_missing", entry.transport.as_str())),
        );
        failures.extend(
            self.transports
                .iter()
                .filter(|entry| !entry.stream_lifecycle)
                .map(|entry| {
                    format!(
                        "transport:{}_stream_lifecycle_missing",
                        entry.transport.as_str()
                    )
                }),
        );
        if self.tool_schemas.is_empty() {
            failures.push("tool_schema:matrix_empty".to_owned());
        }
        failures.extend(
            self.tool_schemas
                .iter()
                .filter(|entry| !entry.schema_checked)
                .map(|entry| format!("tool_schema:{}_unchecked", entry.tool)),
        );
        if !self.stream_lifecycle.start {
            failures.push("stream_lifecycle:start_missing".to_owned());
        }
        if !self.stream_lifecycle.abort {
            failures.push("stream_lifecycle:abort_missing".to_owned());
        }
        if !self.stream_lifecycle.cleanup {
            failures.push("stream_lifecycle:cleanup_missing".to_owned());
        }
        if self.stream_lifecycle.dangling_sessions_after_abort > 0 {
            failures.push(format!(
                "stream_lifecycle:dangling_sessions:{}",
                self.stream_lifecycle.dangling_sessions_after_abort
            ));
        }
        if !self.server_surface.explicit_tool_gate {
            failures.push("server_surface:explicit_tool_gate_missing".to_owned());
        }
        if self.server_surface.unlimited_proxy_exposed {
            failures.push("server_surface:unlimited_proxy_exposed".to_owned());
        }

        let status = if failures.is_empty() {
            "compatible"
        } else {
            "blocked"
        }
        .to_owned();
        Ok(McpCompatibilityReport {
            evidence_kind: self.evidence_kind,
            protocol_version: self.protocol_version.clone(),
            status,
            transport_count: self.transports.len(),
            tool_schema_count: self.tool_schemas.len(),
            failures,
            audit: self.audit_entries(),
        })
    }

    /// 执行 `audit_entries` 对应的处理逻辑。
    fn audit_entries(&self) -> Vec<String> {
        let mut audit = vec![
            format!("mcp.protocol_version:{}", self.protocol_version),
            format!("mcp.transport_count:{}", self.transports.len()),
            format!("mcp.tool_schema_count:{}", self.tool_schemas.len()),
            format!(
                "mcp.stream_lifecycle:start={},abort={},cleanup={}",
                self.stream_lifecycle.start,
                self.stream_lifecycle.abort,
                self.stream_lifecycle.cleanup
            ),
            format!(
                "mcp.server_surface:explicit_tool_gate={}",
                self.server_surface.explicit_tool_gate
            ),
        ];
        audit.extend(self.transports.iter().map(|entry| {
            format!(
                "mcp.transport:{}:json_rpc={},auth_headers={},limits={},stream_lifecycle={}",
                entry.transport.as_str(),
                entry.json_rpc,
                entry.auth_headers,
                entry.timeout_and_output_limits,
                entry.stream_lifecycle
            )
        }));
        audit
    }
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `v1137_fixture_verifies_mcp_compatibility_matrix` 场景下的预期行为。
    #[test]
    fn v1137_fixture_verifies_mcp_compatibility_matrix() {
        let matrix = McpCompatibilityMatrix::v1137_fixture();
        assert_eq!(
            matrix.evidence_kind(),
            McpCompatibilityEvidenceKind::Fixture
        );
        let report = matrix.verify().unwrap();

        assert_eq!(
            report.evidence_kind(),
            McpCompatibilityEvidenceKind::Fixture
        );
        assert_eq!(report.status, "compatible");
        assert_eq!(report.transport_count, 2);
        assert_eq!(report.tool_schema_count, 1);
        assert!(report.failures.is_empty());
        assert!(report.audit.contains(
            &"mcp.transport:http:json_rpc=true,auth_headers=true,limits=true,stream_lifecycle=true"
                .to_owned()
        ));
    }

    /// 验证 `compatibility_matrix_blocks_missing_stream_cleanup` 场景下的预期行为。
    #[test]
    fn compatibility_matrix_blocks_missing_stream_cleanup() {
        let mut matrix = McpCompatibilityMatrix::v1137_fixture();
        matrix.stream_lifecycle.cleanup = false;

        let report = matrix.verify().unwrap();

        assert_eq!(report.status, "blocked");
        assert!(report
            .failures
            .contains(&"stream_lifecycle:cleanup_missing".to_owned()));
    }

    /// 验证 `compatibility_matrix_blocks_missing_transport_stream_lifecycle` 场景下的预期行为。
    #[test]
    fn compatibility_matrix_blocks_missing_transport_stream_lifecycle() {
        let mut matrix = McpCompatibilityMatrix::v1137_fixture();
        matrix.transports[1].stream_lifecycle = false;

        let report = matrix.verify().unwrap();

        assert_eq!(report.status, "blocked");
        assert!(report
            .failures
            .contains(&"transport:http_stream_lifecycle_missing".to_owned()));
    }

    /// 验证 `compatibility_matrix_blocks_unlimited_server_proxy` 场景下的预期行为。
    #[test]
    fn compatibility_matrix_blocks_unlimited_server_proxy() {
        let mut matrix = McpCompatibilityMatrix::v1137_fixture();
        matrix.server_surface.unlimited_proxy_exposed = true;

        let report = matrix.verify().unwrap();

        assert_eq!(report.status, "blocked");
        assert!(report
            .failures
            .contains(&"server_surface:unlimited_proxy_exposed".to_owned()));
    }
}
