//! 定义 MCP 会话配置及其进程监督边界。
//!
//! 会话管理器只负责启动 stdio 进程型会话，并验证监督器返回的句柄。关闭时先暂时取走句柄；
//! 若监督器关闭失败则把句柄放回，使调用方仍可重试且不会把仍运行的进程误报为已停止。
//! MCP process/session lifecycle boundary.

use eva_core::{AdapterId, EvaError};
use std::collections::BTreeSet;
use std::fmt;
use std::net::Ipv6Addr;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP process startup and session shutdown boundary";

/// 定义 `DEFAULT_STARTUP_TIMEOUT_MS` 常量。
const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 10_000;
/// 定义 `DEFAULT_SHUTDOWN_TIMEOUT_MS` 常量。
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;

/// 定义 `McpServerTransport` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerTransport {
    /// 表示 `Stdio` 枚举分支。
    Stdio,
    /// 表示 `Http` 枚举分支。
    Http,
    /// 表示规范化的 Streamable HTTP 传输。
    ///
    /// `Http` 保留为旧 manifest 的兼容拼写；新的 manifest 应使用
    /// `streamable_http`，两者共享同一个配置与安全策略边界。
    StreamableHttp,
}

/// MCP 客户端证书/私钥的间接引用配置。
///
/// 这里只保存 `env:` 或 `vault://` 引用，不保存证书或私钥字节；TLS
/// connector 的实际读取和使用属于 W4-L02。
#[derive(Clone, PartialEq, Eq)]
pub struct McpClientAuthConfig {
    /// 客户端证书引用。
    pub certificate_ref: String,
    /// 客户端私钥引用。
    pub private_key_ref: String,
}

/// Streamable HTTP redirect 处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRedirectPolicy {
    /// 不允许任何 redirect。
    Deny,
    /// 只允许保持 endpoint origin 的 redirect。
    SameOrigin {
        /// 最大 redirect 跳数。
        max_hops: u8,
    },
}

/// 已规范化的 MCP Streamable HTTP 配置。
///
/// 该类型只描述连接合同，不建立 socket，也不实现 TLS。它在所有网络 I/O
/// 之前执行 endpoint、trust root、client auth、redirect 和 origin policy 校验，
/// 供 W4-L02/W4-L03 的 connector/framing 实现消费。
#[derive(Clone, PartialEq, Eq)]
pub struct McpStreamableHttpConfig {
    /// Streamable HTTP endpoint。
    pub endpoint: String,
    /// 信任根引用；例如 `system`、`file:...` 或 `pem:...`。
    pub trust_roots: BTreeSet<String>,
    /// 可选的客户端证书认证引用。
    pub client_auth: Option<McpClientAuthConfig>,
    /// redirect policy。
    pub redirect_policy: McpRedirectPolicy,
    /// 允许的 canonical origin 集合；不支持 wildcard。
    pub allowed_origins: BTreeSet<String>,
}

/// 旧命名的兼容别名。
pub type McpHttpTransportConfig = McpStreamableHttpConfig;

/// 只包含非敏感 endpoint 组成部分的 canonical URL 视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpEndpoint {
    /// URL scheme，当前只允许 `http`/`https`。
    pub scheme: String,
    /// canonical authority。
    pub authority: String,
    /// canonical origin（不含 path/query/fragment）。
    pub origin: String,
}

impl fmt::Debug for McpClientAuthConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpClientAuthConfig")
            .field("certificate_ref_present", &!self.certificate_ref.is_empty())
            .field("private_key_ref_present", &!self.private_key_ref.is_empty())
            .finish()
    }
}

impl fmt::Debug for McpStreamableHttpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpConfig")
            .field("endpoint_origin", &self.endpoint_origin().ok())
            .field("trust_root_count", &self.trust_roots.len())
            .field("client_auth_configured", &self.client_auth.is_some())
            .field("redirect_policy", &self.redirect_policy)
            .field("allowed_origin_count", &self.allowed_origins.len())
            .finish()
    }
}

/// 表示 `McpProcessSpec` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessSpec {
    /// 记录 `command` 字段对应的值。
    pub command: String,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `allowed_commands` 字段对应的值。
    pub allowed_commands: BTreeSet<String>,
    /// 记录 `startup_timeout_ms` 字段对应的值。
    pub startup_timeout_ms: u64,
    /// 记录 `shutdown_timeout_ms` 字段对应的值。
    pub shutdown_timeout_ms: u64,
}

/// 表示 `McpSessionConfig` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionConfig {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `server_transport` 字段对应的值。
    pub server_transport: McpServerTransport,
    /// 记录 `process` 字段对应的值。
    pub process: McpProcessSpec,
}

/// 表示 `McpProcessStartRequest` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessStartRequest {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `server_transport` 字段对应的值。
    pub server_transport: McpServerTransport,
    /// 记录 `command` 字段对应的值。
    pub command: String,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `startup_timeout_ms` 字段对应的值。
    pub startup_timeout_ms: u64,
}

/// 表示 `McpProcessShutdownRequest` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessShutdownRequest {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `shutdown_timeout_ms` 字段对应的值。
    pub shutdown_timeout_ms: u64,
}

/// 表示 `McpProcessHandle` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessHandle {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `process_id` 字段对应的值。
    pub process_id: Option<u32>,
}

/// 表示 `McpSession` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSession {
    /// 记录 `config` 字段对应的值。
    config: McpSessionConfig,
    /// 记录 `handle` 字段对应的值。
    handle: Option<McpProcessHandle>,
}

/// 表示 `McpSessionStartReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionStartReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpSessionStatus,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `McpSessionShutdownReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionShutdownReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpSessionStatus,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 定义 `McpSessionStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSessionStatus {
    /// 表示 `Started` 枚举分支。
    Started,
    /// 表示 `Stopped` 枚举分支。
    Stopped,
}

/// 约定 `McpSessionSupervisor` 实现需要满足的接口。
pub trait McpSessionSupervisor {
    /// 执行 `start_process` 对应的受控流程。
    fn start_process(
        &mut self,
        request: &McpProcessStartRequest,
    ) -> Result<McpProcessHandle, EvaError>;

    /// 停止或释放 `shutdown_process` 管理的资源。
    fn shutdown_process(&mut self, request: &McpProcessShutdownRequest) -> Result<(), EvaError>;
}

/// 表示 `McpSessionManager` 数据结构。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct McpSessionManager;

impl McpServerTransport {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "stdio" => Ok(Self::Stdio),
            "http" => Ok(Self::Http),
            "streamable_http" => Ok(Self::StreamableHttp),
            _ => Err(EvaError::unsupported("unsupported MCP server transport")
                .with_context("server_transport", value)),
        }
    }

    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::StreamableHttp => "streamable_http",
        }
    }

    /// 返回规范化的 transport 拼写；旧 `http` alias 归一化为 Streamable HTTP。
    pub const fn canonical_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http | Self::StreamableHttp => "streamable_http",
        }
    }

    /// 判断当前 transport 是否属于 HTTP 系列。
    pub const fn is_http(self) -> bool {
        matches!(self, Self::Http | Self::StreamableHttp)
    }
}

impl McpClientAuthConfig {
    /// 创建客户端认证引用配置并执行引用格式校验。
    pub fn new(
        certificate_ref: impl Into<String>,
        private_key_ref: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let config = Self {
            certificate_ref: certificate_ref.into(),
            private_key_ref: private_key_ref.into(),
        };
        validate_credential_reference(&config.certificate_ref, "certificate_ref")?;
        validate_credential_reference(&config.private_key_ref, "private_key_ref")?;
        Ok(config)
    }
}

impl McpStreamableHttpConfig {
    /// 创建默认的 Streamable HTTP 配置。
    pub fn new(endpoint: impl Into<String>) -> Result<Self, EvaError> {
        let endpoint = McpEndpoint::canonicalize(&endpoint.into())?;
        let config = Self {
            endpoint,
            trust_roots: BTreeSet::new(),
            client_auth: None,
            redirect_policy: McpRedirectPolicy::Deny,
            allowed_origins: BTreeSet::new(),
        };
        config.validate_for_environment("dev")?;
        Ok(config)
    }

    /// 从 manifest 的规范化字段构造配置。
    pub fn from_parts(
        endpoint: impl Into<String>,
        trust_roots: impl IntoIterator<Item = impl Into<String>>,
        client_auth: Option<McpClientAuthConfig>,
        redirect_policy: McpRedirectPolicy,
        allowed_origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, EvaError> {
        let endpoint = McpEndpoint::canonicalize(&endpoint.into())?;
        let endpoint_view = McpEndpoint::parse(&endpoint)?;
        let roots = trust_roots.into_iter().map(Into::into).collect::<Vec<_>>();
        let trust_roots = roots.iter().cloned().collect::<BTreeSet<_>>();
        if trust_roots.len() != roots.len() {
            return Err(EvaError::invalid_argument(
                "MCP trust roots must not contain duplicates",
            ));
        }
        for root in &trust_roots {
            validate_trust_root(root)?;
        }

        let origins = allowed_origins
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        let allowed_origins = origins.iter().cloned().collect::<BTreeSet<_>>();
        if allowed_origins.len() != origins.len() {
            return Err(EvaError::invalid_argument(
                "MCP allowed origins must not contain duplicates",
            ));
        }
        for origin in &allowed_origins {
            let parsed = McpEndpoint::parse_origin(origin)?;
            if parsed.origin != *origin {
                return Err(EvaError::invalid_argument(
                    "MCP allowed origins must use canonical origin spelling",
                )
                .with_context("origin", origin));
            }
        }
        validate_redirect_policy(redirect_policy)?;
        if !allowed_origins.is_empty() && !allowed_origins.contains(&endpoint_view.origin) {
            return Err(EvaError::permission_denied(
                "MCP allowed origins must include the endpoint origin",
            )
            .with_context("endpoint_origin", endpoint_view.origin));
        }
        if let Some(auth) = &client_auth {
            validate_credential_reference(&auth.certificate_ref, "certificate_ref")?;
            validate_credential_reference(&auth.private_key_ref, "private_key_ref")?;
        }
        let config = Self {
            endpoint,
            trust_roots,
            client_auth,
            redirect_policy,
            allowed_origins,
        };
        config.validate_for_environment("dev")?;
        Ok(config)
    }

    /// 兼容旧 `http://` manifest 的最小配置构造器。
    pub fn legacy_http(endpoint: impl Into<String>) -> Result<Self, EvaError> {
        Self::new(endpoint)
    }

    /// 按运行环境验证配置；production HTTPS 必须显式声明 trust policy。
    pub fn validate_for_environment(&self, environment: &str) -> Result<(), EvaError> {
        let endpoint = self.validate_static()?;
        let production = matches!(
            environment.to_ascii_lowercase().as_str(),
            "prod" | "production"
        );
        if production && endpoint.scheme == "https" && self.trust_roots.is_empty() {
            return Err(EvaError::permission_denied(
                "production MCP HTTPS requires an explicit trust policy",
            )
            .with_provider_code("mcp_https_trust_policy_required")
            .with_context("endpoint_origin", endpoint.origin));
        }
        if production && endpoint.scheme == "https" && self.allowed_origins.is_empty() {
            return Err(EvaError::permission_denied(
                "production MCP HTTPS requires an explicit origin policy",
            )
            .with_provider_code("mcp_https_origin_policy_required")
            .with_context("endpoint_origin", endpoint.origin));
        }
        validate_redirect_policy(self.redirect_policy)
    }

    /// 验证一个收到的 redirect target 是否符合同源策略。
    pub fn validate_redirect_target(&self, target: &str) -> Result<(), EvaError> {
        let endpoint = self.validate_static()?;
        let target = McpEndpoint::parse(target)?;
        match self.redirect_policy {
            McpRedirectPolicy::Deny => Err(EvaError::permission_denied(
                "MCP redirects are disabled by policy",
            )
            .with_provider_code("mcp_redirect_denied")
            .with_context("target_origin", target.origin)),
            McpRedirectPolicy::SameOrigin { .. } if target.origin != endpoint.origin => Err(
                EvaError::permission_denied("MCP redirect crosses origin boundary")
                    .with_provider_code("mcp_cross_origin_redirect")
                    .with_context("from_origin", endpoint.origin)
                    .with_context("target_origin", target.origin),
            ),
            McpRedirectPolicy::SameOrigin { .. }
                if !self.allowed_origins.is_empty()
                    && !self.allowed_origins.contains(&target.origin) =>
            {
                Err(
                    EvaError::permission_denied("MCP redirect target is not in the origin policy")
                        .with_provider_code("mcp_origin_not_allowlisted")
                        .with_context("target_origin", target.origin),
                )
            }
            McpRedirectPolicy::SameOrigin { .. } => Ok(()),
        }
    }

    /// 返回 endpoint 的 canonical origin。
    pub fn endpoint_origin(&self) -> Result<String, EvaError> {
        Ok(McpEndpoint::parse(&self.endpoint)?.origin)
    }

    /// 返回 endpoint 是否为 HTTPS。
    pub fn is_https(&self) -> Result<bool, EvaError> {
        Ok(McpEndpoint::parse(&self.endpoint)?.scheme == "https")
    }

    /// Revalidate fields that can be changed after construction by callers of
    /// this public, intentionally data-oriented type.
    fn validate_static(&self) -> Result<McpEndpoint, EvaError> {
        let canonical_endpoint = McpEndpoint::canonicalize(&self.endpoint)?;
        if canonical_endpoint != self.endpoint {
            return Err(EvaError::invalid_argument(
                "MCP endpoint must use canonical URL spelling",
            ));
        }
        let endpoint = McpEndpoint::parse(&self.endpoint)?;
        for root in &self.trust_roots {
            validate_trust_root(root)?;
        }
        for origin in &self.allowed_origins {
            let parsed = McpEndpoint::parse_origin(origin)?;
            if parsed.origin != *origin {
                return Err(EvaError::invalid_argument(
                    "MCP allowed origins must use canonical origin spelling",
                )
                .with_context("origin", origin));
            }
        }
        if !self.allowed_origins.is_empty() && !self.allowed_origins.contains(&endpoint.origin) {
            return Err(EvaError::permission_denied(
                "MCP allowed origins must include the endpoint origin",
            )
            .with_context("endpoint_origin", endpoint.origin.clone()));
        }
        if let Some(auth) = &self.client_auth {
            validate_credential_reference(&auth.certificate_ref, "certificate_ref")?;
            validate_credential_reference(&auth.private_key_ref, "private_key_ref")?;
        }
        if endpoint.scheme == "http" && !self.trust_roots.is_empty() {
            return Err(EvaError::unsupported(
                "MCP trust roots require an HTTPS endpoint",
            ));
        }
        if endpoint.scheme == "http" && self.client_auth.is_some() {
            return Err(EvaError::unsupported(
                "MCP client certificate authentication requires an HTTPS endpoint",
            ));
        }
        validate_redirect_policy(self.redirect_policy)?;
        Ok(endpoint)
    }
}

impl McpRedirectPolicy {
    /// 解析 manifest 中的 redirect mode。
    pub fn parse(mode: &str, max_hops: Option<u64>) -> Result<Self, EvaError> {
        match mode {
            "deny" => {
                if max_hops.is_some_and(|value| value != 0) {
                    return Err(EvaError::invalid_argument(
                        "MCP deny redirect policy requires max_hops=0",
                    ));
                }
                Ok(Self::Deny)
            }
            "same_origin" => {
                let max_hops = max_hops.unwrap_or(3);
                let max_hops = u8::try_from(max_hops)
                    .map_err(|_| EvaError::invalid_argument("MCP redirect max_hops exceeds u8"))?;
                if max_hops == 0 || max_hops > 10 {
                    return Err(EvaError::invalid_argument(
                        "MCP same_origin redirect max_hops must be between 1 and 10",
                    ));
                }
                Ok(Self::SameOrigin { max_hops })
            }
            _ => {
                Err(EvaError::unsupported("unsupported MCP redirect policy")
                    .with_context("mode", mode))
            }
        }
    }
}

/// 表示已选择的 MCP 传输配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransportConfig {
    /// stdio process configuration。
    Stdio(McpProcessSpec),
    /// Streamable HTTP configuration。
    StreamableHttp(McpStreamableHttpConfig),
}

impl McpTransportConfig {
    /// 返回配置对应的 server transport。
    pub const fn server_transport(&self) -> McpServerTransport {
        match self {
            Self::Stdio(_) => McpServerTransport::Stdio,
            Self::StreamableHttp(_) => McpServerTransport::StreamableHttp,
        }
    }
}

impl McpEndpoint {
    /// Return a canonical absolute HTTP(S) URL while preserving its path and query.
    pub fn canonicalize(url: &str) -> Result<String, EvaError> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| EvaError::invalid_argument("MCP endpoint must include a scheme"))?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(EvaError::unsupported("MCP endpoint scheme is unsupported")
                .with_context("scheme", scheme));
        }
        if url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(EvaError::invalid_argument(
                "MCP endpoint must not contain whitespace or control characters",
            ));
        }
        if rest.contains('#') {
            return Err(EvaError::invalid_argument(
                "MCP endpoint must not contain a fragment",
            ));
        }
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return Err(EvaError::invalid_argument(
                "MCP endpoint must include a host",
            ));
        }
        if authority.contains('@') {
            return Err(EvaError::invalid_argument(
                "MCP endpoint must not include userinfo",
            ));
        }
        let authority = canonical_authority(&scheme, authority)?;
        let suffix = &rest[authority_end..];
        let suffix = if suffix.starts_with('?') {
            format!("/{suffix}")
        } else {
            suffix.to_owned()
        };
        Ok(format!("{scheme}://{authority}{suffix}"))
    }

    /// 解析绝对 HTTP(S) endpoint，并生成 canonical origin。
    pub fn parse(url: &str) -> Result<Self, EvaError> {
        let canonical = Self::canonicalize(url)?;
        let (scheme, rest) = canonical
            .split_once("://")
            .expect("canonical endpoint always has a scheme");
        let authority = rest
            .split(['/', '?'])
            .next()
            .filter(|authority| !authority.is_empty())
            .ok_or_else(|| EvaError::invalid_argument("MCP endpoint must include a host"))?;
        let authority = authority.to_owned();
        Ok(Self {
            origin: format!("{scheme}://{authority}"),
            scheme: scheme.to_owned(),
            authority,
        })
    }

    /// 解析只允许 origin 的值，拒绝 path/query/fragment。
    pub fn parse_origin(origin: &str) -> Result<Self, EvaError> {
        if origin == "*" || origin.contains('*') {
            return Err(EvaError::permission_denied(
                "MCP origin policy does not support wildcard origins",
            )
            .with_context("origin", origin));
        }
        let parsed = Self::parse(origin)?;
        let rest = origin
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or_default();
        if rest.contains(['/', '?', '#']) {
            return Err(EvaError::invalid_argument(
                "MCP origin policy entries must not contain a path or query",
            )
            .with_context("origin", origin));
        }
        Ok(parsed)
    }
}

fn canonical_authority(scheme: &str, authority: &str) -> Result<String, EvaError> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if authority.starts_with('[') {
        let end = authority.find(']').ok_or_else(|| {
            EvaError::invalid_argument("MCP endpoint IPv6 authority is malformed")
        })?;
        let host = &authority[1..end];
        if host.is_empty() {
            return Err(EvaError::invalid_argument(
                "MCP endpoint host cannot be empty",
            ));
        }
        let host = host
            .parse::<Ipv6Addr>()
            .map_err(|_| EvaError::invalid_argument("MCP endpoint IPv6 address is malformed"))?;
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            let port = suffix.strip_prefix(':').ok_or_else(|| {
                EvaError::invalid_argument("MCP endpoint IPv6 authority is malformed")
            })?;
            Some(parse_port(port)?)
        };
        let authority = format!("[{host}]");
        return Ok(match port {
            Some(port) if port != default_port => format!("{authority}:{port}"),
            _ => authority,
        });
    }
    let (host, port) = {
        let colon_count = authority.bytes().filter(|byte| *byte == b':').count();
        if colon_count > 1 {
            return Err(EvaError::invalid_argument(
                "MCP endpoint IPv6 addresses must use brackets",
            ));
        }
        let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
            if host.is_empty() || port.is_empty() {
                return Err(EvaError::invalid_argument(
                    "MCP endpoint authority contains an empty port",
                ));
            }
            (host.to_owned(), Some(parse_port(port)?))
        } else {
            (authority.to_owned(), None)
        };
        (host, port)
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || host == "*"
        || host
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err(EvaError::invalid_argument("MCP endpoint host is malformed"));
    }
    Ok(match port {
        Some(port) if port != default_port => format!("{host}:{port}"),
        _ => host,
    })
}

fn parse_port(value: &str) -> Result<u16, EvaError> {
    let port = value.parse::<u16>().map_err(|_| {
        EvaError::invalid_argument("MCP endpoint port is invalid").with_context("port", value)
    })?;
    if port == 0 {
        return Err(EvaError::invalid_argument(
            "MCP endpoint port must be non-zero",
        ));
    }
    Ok(port)
}

fn validate_trust_root(root: &str) -> Result<(), EvaError> {
    if root.is_empty()
        || root != root.trim()
        || root
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(EvaError::invalid_argument(
            "MCP trust root reference is invalid",
        ));
    }
    if root != "system" && !root.starts_with("file:") && !root.starts_with("pem:") {
        return Err(
            EvaError::unsupported("unsupported MCP trust root reference")
                .with_context("trust_root", root),
        );
    }
    if root.starts_with("file:") {
        let path = root.trim_start_matches("file:");
        if path.is_empty()
            || path.starts_with(['/', '\\'])
            || path
                .split(['/', '\\'])
                .any(|segment| segment.is_empty() || segment == "..")
            || path
                .split(['/', '\\'])
                .next()
                .is_some_and(|segment| segment.contains(':'))
        {
            return Err(EvaError::permission_denied(
                "MCP trust root file reference escapes its configured root",
            ));
        }
    }
    if root.starts_with("pem:") {
        let reference = root.trim_start_matches("pem:");
        if reference.is_empty() || reference.contains("BEGIN ") || reference.starts_with('-') {
            return Err(EvaError::invalid_argument(
                "MCP PEM trust root must be an indirect reference",
            ));
        }
    }
    Ok(())
}

fn validate_credential_reference(value: &str, field: &str) -> Result<(), EvaError> {
    if value.is_empty()
        || value != value.trim()
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(
            EvaError::invalid_argument("MCP client auth reference is invalid")
                .with_context("field", field),
        );
    }
    if let Some(name) = value.strip_prefix("env:") {
        let mut bytes = name.bytes();
        let valid = name.len() <= 128
            && bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if valid {
            return Ok(());
        }
    }
    if let Some(body) = value.strip_prefix("vault://") {
        let mut parts = body.split('#');
        let secret_path = parts.next().unwrap_or_default();
        let key = parts.next();
        let valid_path = !secret_path.is_empty()
            && secret_path.len() <= 384
            && secret_path.split('/').all(is_valid_vault_segment);
        let valid_key = key.is_none_or(is_valid_vault_key);
        if value.len() <= 512 && valid_path && valid_key && parts.next().is_none() {
            return Ok(());
        }
    }
    Err(
        EvaError::permission_denied("MCP client auth must use a valid env: or vault:// reference")
            .with_context("field", field),
    )
}

fn is_valid_vault_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_vault_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_redirect_policy(policy: McpRedirectPolicy) -> Result<(), EvaError> {
    match policy {
        McpRedirectPolicy::Deny => Ok(()),
        McpRedirectPolicy::SameOrigin { max_hops } if (1..=10).contains(&max_hops) => Ok(()),
        McpRedirectPolicy::SameOrigin { .. } => Err(EvaError::invalid_argument(
            "MCP same_origin redirect max_hops must be between 1 and 10",
        )),
    }
}

impl McpProcessSpec {
    /// 创建并初始化当前类型的实例。
    pub fn new(command: impl Into<String>) -> Self {
        let command = command.into();
        Self {
            allowed_commands: [command.clone()].into_iter().collect(),
            command,
            args: Vec::new(),
            startup_timeout_ms: DEFAULT_STARTUP_TIMEOUT_MS,
            shutdown_timeout_ms: DEFAULT_SHUTDOWN_TIMEOUT_MS,
        }
    }

    /// 设置 `args` 并返回更新后的实例。
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// 设置 `allowed_commands` 并返回更新后的实例。
    pub fn with_allowed_commands(
        mut self,
        allowed_commands: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_commands = allowed_commands.into_iter().map(Into::into).collect();
        self
    }

    /// 设置 `startup_timeout_ms` 并返回更新后的实例。
    pub fn with_startup_timeout_ms(mut self, startup_timeout_ms: u64) -> Self {
        self.startup_timeout_ms = startup_timeout_ms;
        self
    }

    /// 设置 `shutdown_timeout_ms` 并返回更新后的实例。
    pub fn with_shutdown_timeout_ms(mut self, shutdown_timeout_ms: u64) -> Self {
        self.shutdown_timeout_ms = shutdown_timeout_ms;
        self
    }
}

impl McpSessionConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        adapter_id: AdapterId,
        server_transport: McpServerTransport,
        process: McpProcessSpec,
    ) -> Result<Self, EvaError> {
        let config = Self {
            adapter_id,
            server_transport,
            process,
        };
        validate_config(&config)?;
        Ok(config)
    }

    /// 执行 `stdio` 对应的处理逻辑。
    pub fn stdio(adapter_id: AdapterId, command: impl Into<String>) -> Result<Self, EvaError> {
        Self::new(
            adapter_id,
            McpServerTransport::Stdio,
            McpProcessSpec::new(command),
        )
    }
}

impl McpProcessHandle {
    /// 创建并初始化当前类型的实例。
    pub fn new(session_id: impl Into<String>, process_id: Option<u32>) -> Self {
        Self {
            session_id: session_id.into(),
            process_id,
        }
    }
}

impl McpSession {
    /// 返回 `adapter_id` 对应的数据视图。
    pub fn adapter_id(&self) -> &AdapterId {
        &self.config.adapter_id
    }

    /// 执行 `session_id` 对应的处理逻辑。
    pub fn session_id(&self) -> Option<&str> {
        self.handle
            .as_ref()
            .map(|handle| handle.session_id.as_str())
    }

    /// 判断 `is_running` 对应的条件是否成立。
    pub fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    /// 执行 `process_id` 对应的处理逻辑。
    pub fn process_id(&self) -> Option<u32> {
        self.handle.as_ref().and_then(|handle| handle.process_id)
    }

    /// 执行 `server_transport` 对应的处理逻辑。
    pub fn server_transport(&self) -> McpServerTransport {
        self.config.server_transport
    }

    /// 执行 `start_report` 对应的受控流程。
    pub fn start_report(&self) -> Result<McpSessionStartReport, EvaError> {
        let handle = self.handle.as_ref().ok_or_else(|| {
            EvaError::conflict("MCP session is not running")
                .with_context("adapter_id", self.config.adapter_id.as_str())
        })?;
        Ok(McpSessionStartReport {
            adapter_id: self.config.adapter_id.clone(),
            session_id: handle.session_id.clone(),
            status: McpSessionStatus::Started,
            audit: start_audit(&self.config, handle),
        })
    }
}

impl McpSessionStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
        }
    }
}

impl McpSessionManager {
    /// 执行 `start` 对应的受控流程。
    pub fn start(
        &self,
        supervisor: &mut impl McpSessionSupervisor,
        config: McpSessionConfig,
    ) -> Result<McpSession, EvaError> {
        validate_config(&config)?;
        if config.server_transport != McpServerTransport::Stdio {
            return Err(EvaError::unsupported(
                "MCP session manager only starts stdio process sessions",
            )
            .with_context("adapter_id", config.adapter_id.as_str())
            .with_context("server_transport", config.server_transport.as_str()));
        }
        let request = McpProcessStartRequest {
            adapter_id: config.adapter_id.clone(),
            server_transport: config.server_transport,
            command: config.process.command.clone(),
            args: config.process.args.clone(),
            startup_timeout_ms: config.process.startup_timeout_ms,
        };
        let handle = supervisor.start_process(&request)?;
        if handle.session_id.trim().is_empty() {
            return Err(
                EvaError::internal("MCP supervisor returned empty session id")
                    .with_context("adapter_id", config.adapter_id.as_str()),
            );
        }
        Ok(McpSession {
            config,
            handle: Some(handle),
        })
    }

    /// 停止或释放 `shutdown` 管理的资源。
    pub fn shutdown(
        &self,
        supervisor: &mut impl McpSessionSupervisor,
        session: &mut McpSession,
    ) -> Result<McpSessionShutdownReport, EvaError> {
        let handle = session.handle.take().ok_or_else(|| {
            EvaError::conflict("MCP session is already stopped")
                .with_context("adapter_id", session.config.adapter_id.as_str())
        })?;
        let request = McpProcessShutdownRequest {
            adapter_id: session.config.adapter_id.clone(),
            session_id: handle.session_id.clone(),
            shutdown_timeout_ms: session.config.process.shutdown_timeout_ms,
        };

        match supervisor.shutdown_process(&request) {
            Ok(()) => Ok(McpSessionShutdownReport {
                adapter_id: session.config.adapter_id.clone(),
                session_id: handle.session_id,
                status: McpSessionStatus::Stopped,
                audit: vec![
                    "transport:mcp".to_owned(),
                    "mcp.session:shutdown_requested".to_owned(),
                    "mcp.session:stopped".to_owned(),
                ],
            }),
            Err(error) => {
                session.handle = Some(handle);
                Err(error)
            }
        }
    }
}

/// 校验 `validate_config` 对应的约束，不满足时返回明确错误。
fn validate_config(config: &McpSessionConfig) -> Result<(), EvaError> {
    if config.process.command.trim().is_empty() {
        return Err(
            EvaError::invalid_argument("MCP process command cannot be empty")
                .with_context("adapter_id", config.adapter_id.as_str()),
        );
    }
    if config.process.command.trim() != config.process.command {
        return Err(
            EvaError::invalid_argument("MCP process command must be trimmed")
                .with_context("adapter_id", config.adapter_id.as_str())
                .with_context("command", &config.process.command),
        );
    }
    if !config
        .process
        .allowed_commands
        .contains(&config.process.command)
    {
        return Err(
            EvaError::permission_denied("MCP process command is not allowlisted")
                .with_context("adapter_id", config.adapter_id.as_str())
                .with_context("command", &config.process.command),
        );
    }
    if config.process.startup_timeout_ms == 0 {
        return Err(EvaError::invalid_argument(
            "MCP process startup timeout must be greater than zero",
        )
        .with_context("adapter_id", config.adapter_id.as_str()));
    }
    if config.process.shutdown_timeout_ms == 0 {
        return Err(EvaError::invalid_argument(
            "MCP process shutdown timeout must be greater than zero",
        )
        .with_context("adapter_id", config.adapter_id.as_str()));
    }
    for arg in &config.process.args {
        if arg.contains('\0') {
            return Err(
                EvaError::invalid_argument("MCP process argument cannot contain NUL")
                    .with_context("adapter_id", config.adapter_id.as_str()),
            );
        }
    }
    Ok(())
}

/// 执行 `start_audit` 对应的受控流程。
fn start_audit(config: &McpSessionConfig, handle: &McpProcessHandle) -> Vec<String> {
    let mut audit = vec![
        "transport:mcp".to_owned(),
        format!("mcp.server_transport:{}", config.server_transport.as_str()),
        "mcp.session:start_requested".to_owned(),
        "mcp.session:started".to_owned(),
        "shell:false".to_owned(),
        "command_allowlist:passed".to_owned(),
    ];
    if let Some(process_id) = handle.process_id {
        audit.push(format!("process_id:{process_id}"));
    }
    audit
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    /// 表示 `FakeSupervisor` 数据结构。
    #[derive(Debug, Clone)]
    struct FakeSupervisor {
        /// 记录 `start_response` 字段对应的值。
        start_response: Result<McpProcessHandle, EvaError>,
        /// 记录 `shutdown_response` 字段对应的值。
        shutdown_response: Result<(), EvaError>,
        /// 记录 `start_calls` 字段对应的值。
        start_calls: usize,
        /// 记录 `shutdown_calls` 字段对应的值。
        shutdown_calls: usize,
        /// 记录 `last_shutdown_session_id` 字段对应的值。
        last_shutdown_session_id: Option<String>,
    }

    impl FakeSupervisor {
        /// 执行 `started` 对应的受控流程。
        fn started() -> Self {
            Self {
                start_response: Ok(McpProcessHandle::new("session-1", Some(42))),
                shutdown_response: Ok(()),
                start_calls: 0,
                shutdown_calls: 0,
                last_shutdown_session_id: None,
            }
        }

        /// 执行 `startup_failure` 对应的受控流程。
        fn startup_failure() -> Self {
            Self {
                start_response: Err(EvaError::unavailable("MCP process failed to start")
                    .with_context("command", "github-mcp-server")),
                shutdown_response: Ok(()),
                start_calls: 0,
                shutdown_calls: 0,
                last_shutdown_session_id: None,
            }
        }
    }

    impl McpSessionSupervisor for FakeSupervisor {
        /// 执行 `start_process` 对应的受控流程。
        fn start_process(
            &mut self,
            request: &McpProcessStartRequest,
        ) -> Result<McpProcessHandle, EvaError> {
            self.start_calls += 1;
            assert_eq!(request.command, "github-mcp-server");
            self.start_response.clone()
        }

        /// 停止或释放 `shutdown_process` 管理的资源。
        fn shutdown_process(
            &mut self,
            request: &McpProcessShutdownRequest,
        ) -> Result<(), EvaError> {
            self.shutdown_calls += 1;
            self.last_shutdown_session_id = Some(request.session_id.clone());
            self.shutdown_response.clone()
        }
    }

    /// 验证 `session_start_reports_startup_failure` 场景下的预期行为。
    #[test]
    fn session_start_reports_startup_failure() {
        let mut supervisor = FakeSupervisor::startup_failure();
        let manager = McpSessionManager;

        let error = manager.start(&mut supervisor, config()).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(supervisor.start_calls, 1);
        assert_eq!(supervisor.shutdown_calls, 0);
    }

    /// 验证 `session_shutdown_stops_running_session` 场景下的预期行为。
    #[test]
    fn session_shutdown_stops_running_session() {
        let mut supervisor = FakeSupervisor::started();
        let manager = McpSessionManager;
        let mut session = manager.start(&mut supervisor, config()).unwrap();

        let start_report = session.start_report().unwrap();
        assert_eq!(start_report.status, McpSessionStatus::Started);
        assert!(start_report.audit.contains(&"shell:false".to_owned()));
        assert!(session.is_running());

        let shutdown_report = manager.shutdown(&mut supervisor, &mut session).unwrap();

        assert_eq!(shutdown_report.status, McpSessionStatus::Stopped);
        assert_eq!(
            supervisor.last_shutdown_session_id.as_deref(),
            Some("session-1")
        );
        assert!(!session.is_running());
    }

    /// 验证 `session_rejects_non_allowlisted_command` 场景下的预期行为。
    #[test]
    fn session_rejects_non_allowlisted_command() {
        let process = McpProcessSpec::new("github-mcp-server")
            .with_allowed_commands(["different-mcp-server"]);
        let error = McpSessionConfig::new(
            AdapterId::parse("github-mcp").unwrap(),
            McpServerTransport::Stdio,
            process,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn streamable_http_transport_and_unknown_transport_are_explicit() {
        assert_eq!(
            McpServerTransport::parse("streamable_http").unwrap(),
            McpServerTransport::StreamableHttp
        );
        assert_eq!(
            McpServerTransport::StreamableHttp.canonical_str(),
            "streamable_http"
        );
        assert!(McpServerTransport::parse("ftp").is_err());
    }

    #[test]
    fn endpoint_canonicalization_rejects_ambiguous_authorities() {
        assert_eq!(
            McpEndpoint::canonicalize("HTTP://Example.COM:80/mcp").unwrap(),
            "http://example.com/mcp"
        );
        assert_eq!(
            McpEndpoint::canonicalize("HTTPS://[2001:0DB8:0:0:0:0:0:1]:443/mcp").unwrap(),
            "https://[2001:db8::1]/mcp"
        );
        for endpoint in [
            "ftp://example.com/mcp",
            "http://user:password@example.com/mcp",
            "http://example.com/mcp#fragment",
            "http://example.com:bad/mcp",
            "http://example.com:0/mcp",
            "http://[]/mcp",
            "http://[example.com]/mcp",
            "http://2001:db8::1/mcp",
        ] {
            assert!(McpEndpoint::canonicalize(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn streamable_http_policy_checks_origins_redirects_and_environment() {
        let same_origin = McpStreamableHttpConfig::from_parts(
            "HTTPS://Example.COM:443/mcp",
            ["system"],
            None,
            McpRedirectPolicy::SameOrigin { max_hops: 2 },
            ["https://example.com"],
        )
        .unwrap();
        assert_eq!(same_origin.endpoint, "https://example.com/mcp");
        assert!(same_origin.validate_for_environment("production").is_ok());
        assert!(same_origin
            .validate_redirect_target("https://example.com/other")
            .is_ok());
        assert!(same_origin
            .validate_redirect_target("https://other.example/other")
            .is_err());

        let deny = McpStreamableHttpConfig::new("http://example.com/mcp").unwrap();
        assert!(deny
            .validate_redirect_target("http://example.com/other")
            .is_err());
        assert!(McpRedirectPolicy::parse("same_origin", Some(0)).is_err());
        assert!(McpRedirectPolicy::parse("same_origin", Some(11)).is_err());

        for origins in [
            vec!["*".to_owned()],
            vec!["https://other.example".to_owned()],
        ] {
            assert!(McpStreamableHttpConfig::from_parts(
                "https://example.com/mcp",
                ["system"],
                None,
                McpRedirectPolicy::Deny,
                origins,
            )
            .is_err());
        }
        assert!(McpStreamableHttpConfig::new("https://example.com/mcp")
            .unwrap()
            .validate_for_environment("production")
            .is_err());
    }

    #[test]
    fn client_auth_and_mutable_policy_fields_fail_closed() {
        for (certificate, key) in [
            ("literal", "env:CLIENT_KEY"),
            ("env:", "env:CLIENT_KEY"),
            ("env:1BAD", "env:CLIENT_KEY"),
            ("env:CLIENT_CERT", "vault://"),
            ("env:CLIENT_CERT", "vault://providers/../key"),
            ("env:CLIENT_CERT", "vault://providers/key#one#two"),
        ] {
            assert!(McpClientAuthConfig::new(certificate, key).is_err());
        }
        let auth =
            McpClientAuthConfig::new("env:CLIENT_CERT", "vault://providers/client/key#value")
                .unwrap();
        let config = McpStreamableHttpConfig::from_parts(
            "https://example.com/mcp",
            ["system"],
            Some(auth),
            McpRedirectPolicy::Deny,
            ["https://example.com"],
        )
        .unwrap();
        let mut mutated = config.clone();
        mutated.trust_roots.insert("file:/outside.pem".to_owned());
        assert!(mutated.validate_for_environment("dev").is_err());

        let mut mutated = config.clone();
        mutated.client_auth.as_mut().unwrap().certificate_ref = "plaintext".to_owned();
        assert!(mutated.validate_for_environment("dev").is_err());

        let mut mutated = config.clone();
        mutated.endpoint = "HTTPS://EXAMPLE.COM:443/mcp".to_owned();
        assert!(mutated.validate_for_environment("dev").is_err());

        let mut mutated = config.clone();
        mutated
            .allowed_origins
            .insert("https://other.example".to_owned());
        mutated.allowed_origins.remove("https://example.com");
        assert!(mutated.validate_for_environment("dev").is_err());

        let mut mutated = config;
        mutated.redirect_policy = McpRedirectPolicy::SameOrigin { max_hops: 0 };
        assert!(mutated.validate_for_environment("dev").is_err());

        let query =
            McpStreamableHttpConfig::new("https://example.com/mcp?access_token=do-not-log-this")
                .unwrap();
        let debug = format!("{query:?}");
        assert!(!debug.contains("access_token"));
        assert!(!debug.contains("do-not-log-this"));
    }

    /// 执行 `config` 对应的处理逻辑。
    fn config() -> McpSessionConfig {
        McpSessionConfig::stdio(AdapterId::parse("github-mcp").unwrap(), "github-mcp-server")
            .unwrap()
    }
}
