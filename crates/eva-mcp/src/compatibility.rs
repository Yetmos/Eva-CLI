//! MCP compatibility matrix and stream lifecycle evidence.

use crate::session::McpServerTransport;
use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP transport, schema, and stream lifecycle compatibility matrix";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityMatrix {
    pub protocol_version: String,
    pub transports: Vec<McpTransportCompatibility>,
    pub tool_schemas: Vec<McpToolSchemaCompatibility>,
    pub stream_lifecycle: McpStreamLifecycleCompatibility,
    pub server_surface: McpServerSurfaceCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTransportCompatibility {
    pub transport: McpServerTransport,
    pub json_rpc: bool,
    pub auth_headers: bool,
    pub timeout_and_output_limits: bool,
    pub stream_lifecycle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolSchemaCompatibility {
    pub tool: String,
    pub input_schema: String,
    pub output_content: String,
    pub schema_checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamLifecycleCompatibility {
    pub start: bool,
    pub abort: bool,
    pub cleanup: bool,
    pub dangling_sessions_after_abort: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSurfaceCompatibility {
    pub explicit_tool_gate: bool,
    pub unlimited_proxy_exposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCompatibilityReport {
    pub protocol_version: String,
    pub status: String,
    pub transport_count: usize,
    pub tool_schema_count: usize,
    pub failures: Vec<String>,
    pub audit: Vec<String>,
}

impl McpCompatibilityMatrix {
    pub fn v1137_fixture() -> Self {
        Self {
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
            .any(|entry| entry.transport == McpServerTransport::Http && entry.json_rpc)
        {
            failures.push("transport:http_json_rpc_missing".to_owned());
        }
        if !self
            .transports
            .iter()
            .any(|entry| entry.transport == McpServerTransport::Http && entry.auth_headers)
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
            protocol_version: self.protocol_version.clone(),
            status,
            transport_count: self.transports.len(),
            tool_schema_count: self.tool_schemas.len(),
            failures,
            audit: self.audit_entries(),
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1137_fixture_verifies_mcp_compatibility_matrix() {
        let report = McpCompatibilityMatrix::v1137_fixture().verify().unwrap();

        assert_eq!(report.status, "compatible");
        assert_eq!(report.transport_count, 2);
        assert_eq!(report.tool_schema_count, 1);
        assert!(report.failures.is_empty());
        assert!(report.audit.contains(
            &"mcp.transport:http:json_rpc=true,auth_headers=true,limits=true,stream_lifecycle=true"
                .to_owned()
        ));
    }

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
