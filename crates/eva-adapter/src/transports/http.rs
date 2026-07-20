//! 执行受来源、方法、超时和响应大小约束的 HTTP 提供者调用。
//!
//! URL 在连接前解析并按 origin 白名单校验，请求头拒绝换行以阻断头注入。响应头和正文
//! 分别设置硬上限，正文到达上限后返回带截断标记的受控报告；环境凭据和会话令牌只进入
//! 实际请求，并作为敏感值参与输出脱敏。
//! HTTP transport runner contract.

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "HTTP transport with env allowlist-based credentials";

use crate::credential_vault::{
    default_credential_vault, sanitize_error_with_values, CredentialSessionLease, CredentialVault,
};
use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation as RuntimeAdapterInvocation, AdapterInvokeReport};
use crate::stream::{
    capture_provider_bytes, default_provider_artifact_root, provider_stream_audit,
    provider_stream_key, provider_stream_summary_json, ProviderStreamCapture, ProviderStreamConfig,
    DEFAULT_STREAM_CHUNK_SIZE_BYTES, DEFAULT_STREAM_PREVIEW_LIMIT_BYTES,
};
use crate::supervisor::validate_credential_scope_for_provider;
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// 定义 HTTP 调用允许的来源、方法、时间和响应证据边界。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRunnerConfig {
    /// 保存规范化的 `scheme://authority`，路径差异不会扩大来源权限。
    pub allowed_origins: BTreeSet<String>,
    /// 记录 `allowed_methods` 字段对应的值。
    pub allowed_methods: BTreeSet<HttpMethod>,
    /// 记录 `timeout_ms` 字段对应的值。
    pub timeout_ms: u64,
    /// 记录 `output_limit_bytes` 字段对应的值。
    pub output_limit_bytes: usize,
    /// 记录 `preview_limit_bytes` 字段对应的值。
    pub preview_limit_bytes: usize,
    /// 记录 `stream_chunk_size_bytes` 字段对应的值。
    pub stream_chunk_size_bytes: usize,
    /// 记录 `artifact_root` 字段对应的值。
    pub artifact_root: Option<std::path::PathBuf>,
    /// 记录 `artifact_key` 字段对应的值。
    pub artifact_key: Option<String>,
    /// 保存仅供响应脱敏使用的凭据原值，不写入审计报告。
    pub sensitive_values: Vec<String>,
}

impl fmt::Debug for HttpRunnerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRunnerConfig")
            .field("allowed_origin_count", &self.allowed_origins.len())
            .field("allowed_method_count", &self.allowed_methods.len())
            .field("timeout_ms", &self.timeout_ms)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .field("preview_limit_bytes", &self.preview_limit_bytes)
            .field("stream_chunk_size_bytes", &self.stream_chunk_size_bytes)
            .field("artifact_root", &self.artifact_root)
            .field("artifact_key", &self.artifact_key)
            .field("sensitive_value_count", &self.sensitive_values.len())
            .finish()
    }
}

/// 表示 `HttpInvocation` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpInvocation {
    /// 记录 `method` 字段对应的值。
    pub method: HttpMethod,
    /// 记录 `url` 字段对应的值。
    pub url: String,
    /// 记录 `headers` 字段对应的值。
    pub headers: BTreeMap<String, String>,
    /// 记录 `body` 字段对应的值。
    pub body: Vec<u8>,
}

impl fmt::Debug for HttpInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpInvocation")
            .field("method", &self.method)
            .field("url_present", &!self.url.is_empty())
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// 表示 `HttpRunReport` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRunReport {
    /// 记录 `method` 字段对应的值。
    pub method: HttpMethod,
    /// 记录 `url` 字段对应的值。
    pub url: String,
    /// 记录 `status_code` 字段对应的值。
    pub status_code: u16,
    /// 记录 `body` 字段对应的值。
    pub body: Vec<u8>,
    /// 记录 `body_stream` 字段对应的值。
    pub body_stream: ProviderStreamCapture,
    /// 记录 `duration_ms` 字段对应的值。
    pub duration_ms: u128,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

impl fmt::Debug for HttpRunReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRunReport")
            .field("method", &self.method)
            .field("url_present", &!self.url.is_empty())
            .field("status_code", &self.status_code)
            .field("body_len", &self.body.len())
            .field("body_stream", &"[REDACTED_STREAM]")
            .field("duration_ms", &self.duration_ms)
            .field("audit_count", &self.audit.len())
            .finish()
    }
}

/// 定义 `HttpMethod` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HttpMethod {
    /// 表示 `Get` 枚举分支。
    Get,
    /// 表示 `Post` 枚举分支。
    Post,
    /// 表示 `Put` 枚举分支。
    Put,
    /// 表示 `Patch` 枚举分支。
    Patch,
    /// 表示 `Delete` 枚举分支。
    Delete,
}

/// 抽象实际网络客户端，使策略校验和报告逻辑可在无网络副作用下测试。
pub trait HttpClient {
    /// 执行 `send` 对应的处理逻辑。
    fn send(
        &self,
        invocation: &HttpInvocation,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<HttpClientResponse, EvaError>;
}

/// 表示 `HttpClientResponse` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpClientResponse {
    /// 记录 `status_code` 字段对应的值。
    pub status_code: u16,
    /// 记录 `body` 字段对应的值。
    pub body: Vec<u8>,
    /// 记录 `body_truncated` 字段对应的值。
    pub body_truncated: bool,
    /// 记录 `body_chunk_count` 字段对应的值。
    pub body_chunk_count: usize,
}

impl fmt::Debug for HttpClientResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpClientResponse")
            .field("status_code", &self.status_code)
            .field("body_len", &self.body.len())
            .field("body_truncated", &self.body_truncated)
            .field("body_chunk_count", &self.body_chunk_count)
            .finish()
    }
}

/// 表示 `HttpRunner` 数据结构。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HttpRunner;

/// 表示 `TcpHttpClient` 数据结构。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TcpHttpClient;

impl HttpRunnerConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        allowed_origins: impl IntoIterator<Item = impl Into<String>>,
        allowed_methods: impl IntoIterator<Item = HttpMethod>,
        timeout_ms: u64,
        output_limit_bytes: usize,
    ) -> Self {
        Self {
            allowed_origins: allowed_origins
                .into_iter()
                .map(Into::into)
                .collect::<BTreeSet<_>>(),
            allowed_methods: allowed_methods.into_iter().collect::<BTreeSet<_>>(),
            timeout_ms,
            output_limit_bytes,
            preview_limit_bytes: DEFAULT_STREAM_PREVIEW_LIMIT_BYTES,
            stream_chunk_size_bytes: DEFAULT_STREAM_CHUNK_SIZE_BYTES,
            artifact_root: None,
            artifact_key: None,
            sensitive_values: Vec::new(),
        }
    }

    /// 设置 `artifact_sink` 并返回更新后的实例。
    pub fn with_artifact_sink(
        mut self,
        artifact_root: impl Into<std::path::PathBuf>,
        artifact_key: impl Into<String>,
    ) -> Self {
        self.artifact_root = Some(artifact_root.into());
        self.artifact_key = Some(artifact_key.into());
        self
    }

    /// 设置 `sensitive_values` 并返回更新后的实例。
    pub fn with_sensitive_values(mut self, sensitive_values: Vec<String>) -> Self {
        self.sensitive_values = sensitive_values;
        self
    }
}

impl HttpInvocation {
    /// 创建并初始化当前类型的实例。
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    /// 设置 `header` 并返回更新后的实例。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// 设置 `body` 并返回更新后的实例。
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

impl HttpMethod {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self::Get),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "PATCH" => Ok(Self::Patch),
            "DELETE" => Ok(Self::Delete),
            _ => {
                Err(EvaError::invalid_argument("unsupported HTTP method")
                    .with_context("method", value))
            }
        }
    }

    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

impl HttpClientResponse {
    /// 创建并初始化当前类型的实例。
    pub fn new(status_code: u16, body: impl Into<Vec<u8>>) -> Self {
        let body = body.into();
        let body_chunk_count = usize::from(!body.is_empty());
        Self {
            status_code,
            body,
            body_truncated: false,
            body_chunk_count,
        }
    }
}

impl HttpRunner {
    /// 在调用客户端前完成来源、方法、头部和输出上限校验，再对响应执行统一有界采集。
    pub fn run(
        &self,
        config: &HttpRunnerConfig,
        client: &impl HttpClient,
        invocation: HttpInvocation,
    ) -> Result<HttpRunReport, EvaError> {
        validate_invocation(config, &invocation)?;
        let timeout = Duration::from_millis(config.timeout_ms);
        let started_at = Instant::now();
        let response = client.send(&invocation, timeout, config.output_limit_bytes)?;
        let status_code = response.status_code;
        let body_chunk_count = response.body_chunk_count;
        let body_truncated = response.body_truncated;
        let body_stream = capture_provider_bytes(
            body_stream_config(config),
            response.body,
            body_chunk_count,
            body_truncated,
            &config.sensitive_values,
        )?;
        if !timeout.is_zero() && started_at.elapsed() >= timeout {
            return Err(EvaError::timeout("HTTP provider timed out")
                .with_context("url", &invocation.url)
                .with_context("timeout_ms", config.timeout_ms.to_string()));
        }
        Ok(HttpRunReport {
            method: invocation.method,
            url: invocation.url,
            status_code,
            body: body_stream.preview.clone(),
            body_stream: body_stream.clone(),
            duration_ms: started_at.elapsed().as_millis(),
            audit: {
                let mut audit = vec![
                    "transport:http".to_owned(),
                    format!("method:{}", invocation.method.as_str()),
                    "url_allowlist:passed".to_owned(),
                ];
                audit.extend(provider_stream_audit(&body_stream));
                audit
            },
        })
    }
}

impl HttpClient for TcpHttpClient {
    /// 通过裸 TCP 执行 HTTP/1.1 请求；当前实现明确拒绝未集成 TLS 的 HTTPS。
    fn send(
        &self,
        invocation: &HttpInvocation,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<HttpClientResponse, EvaError> {
        let parsed = ParsedHttpUrl::parse(&invocation.url)?;
        if parsed.scheme != "http" {
            return Err(EvaError::unsupported(
                "HTTPS provider execution requires a TLS HTTP client not bundled in this runtime",
            )
            .with_context("url", &invocation.url)
            .with_context("scheme", parsed.scheme));
        }

        let mut addrs = (parsed.host.as_str(), parsed.port)
            .to_socket_addrs()
            .map_err(|error| {
                EvaError::unavailable("failed to resolve HTTP provider")
                    .with_context("host", &parsed.host)
                    .with_context("io_error", error.to_string())
            })?;
        let addr = addrs.next().ok_or_else(|| {
            EvaError::unavailable("HTTP provider host did not resolve")
                .with_context("host", &parsed.host)
        })?;
        let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|error| {
            EvaError::unavailable("failed to connect HTTP provider")
                .with_context("origin", &parsed.origin)
                .with_context("io_error", error.to_string())
        })?;
        if !timeout.is_zero() {
            stream
                .set_read_timeout(Some(timeout))
                .and_then(|_| stream.set_write_timeout(Some(timeout)))
                .map_err(|error| {
                    EvaError::unavailable("failed to configure HTTP provider timeout")
                        .with_context("origin", &parsed.origin)
                        .with_context("io_error", error.to_string())
                })?;
        }

        let mut request = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            invocation.method.as_str(),
            parsed.path,
            parsed.authority,
            invocation.body.len()
        );
        for (name, value) in &invocation.headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).map_err(|error| {
            EvaError::unavailable("failed to write HTTP provider request")
                .with_context("origin", &parsed.origin)
                .with_context("io_error", error.to_string())
        })?;
        if !invocation.body.is_empty() {
            stream.write_all(&invocation.body).map_err(|error| {
                EvaError::unavailable("failed to write HTTP provider body")
                    .with_context("origin", &parsed.origin)
                    .with_context("io_error", error.to_string())
            })?;
        }

        read_http_response(&mut stream, &parsed.origin, output_limit_bytes)
    }
}

/// 使用默认 TCP 客户端执行已经过适配器运行时授权的 HTTP 调用。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let vault = default_credential_vault();
    invoke_with_client_and_vault(handle, invocation, &TcpHttpClient, vault.as_ref())
}

/// 构造凭据、白名单与证据配置后调用可替换客户端；任何网络 I/O 都发生在校验之后。
pub fn invoke_with_client(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
    client: &impl HttpClient,
) -> Result<AdapterInvokeReport, EvaError> {
    let vault = default_credential_vault();
    invoke_with_client_and_vault(handle, invocation, client, vault.as_ref())
}

/// Invoke HTTP with an explicit credential authority.
pub fn invoke_with_vault(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
    vault: &dyn CredentialVault,
) -> Result<AdapterInvokeReport, EvaError> {
    invoke_with_client_and_vault(handle, invocation, &TcpHttpClient, vault)
}

/// Build and execute an HTTP request with an explicit client and vault.
pub fn invoke_with_client_and_vault(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
    client: &impl HttpClient,
    vault: &dyn CredentialVault,
) -> Result<AdapterInvokeReport, EvaError> {
    super::validate_process_free_identity(handle)?;
    let endpoint = handle.endpoint.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("HTTP adapter is missing endpoint")
            .with_context("adapter_id", handle.id.as_str())
    })?;
    validate_input_size(handle, &invocation.input)?;
    let trace = invocation.trace_for_adapter(&handle.id);
    let request_id = invocation.request_id.clone();
    let capability = invocation.capability.clone();
    let credential_scope = validate_credential_scope_for_provider(
        invocation.credential_scope(),
        &handle.id,
        &invocation.request_id,
        &invocation.capability,
        has_scoped_http_credentials(handle),
    )?
    .cloned();
    let lazy_credential_env = handle
        .headers
        .values()
        .filter_map(|value| value.strip_prefix("env:"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut credential_lease = CredentialSessionLease::open_with_lazy_env(
        vault,
        credential_scope.as_ref(),
        &handle.provider.vault_secrets,
        &handle.credential_env,
        &lazy_credential_env,
    )?;
    let method = HttpMethod::parse(handle.method.as_deref().unwrap_or("POST"))?;
    let header_plan = http_headers(handle, &mut credential_lease)?;
    let mut sensitive_values = header_plan.sensitive_values.clone();
    sensitive_values.extend(credential_lease.redaction_values());
    if let Some(scope) = &credential_scope {
        sensitive_values.extend(scope.redaction_values());
    }
    let error_redactions = sensitive_values.clone();
    let mut headers = header_plan.headers.clone();
    if let Some(scope) = &credential_scope {
        scope.apply_headers(&mut headers);
    }
    let artifact_root = default_provider_artifact_root(&handle.source_path);
    let artifact_key = provider_stream_key(
        "provider",
        handle.id.as_str(),
        request_id.as_str(),
        "http-body",
    );
    let config = HttpRunnerConfig::new(
        [url_origin(endpoint)?],
        [method],
        timeout_ms(handle),
        output_limit_bytes(handle),
    )
    .with_sensitive_values(sensitive_values)
    .with_artifact_sink(artifact_root, artifact_key);
    let mut http_invocation = HttpInvocation::new(method, endpoint)
        .with_headers(headers)
        .with_body(invocation.input.as_bytes().to_vec());
    let run_result = HttpRunner.run(&config, client, http_invocation.clone());
    http_invocation.headers.clear();
    http_invocation.body.clear();
    let run = match (run_result, credential_lease.release()) {
        (Err(error), _) => return Err(sanitize_error_with_values(error, &error_redactions)),
        (Ok(_), Err(error)) => {
            return Err(sanitize_error_with_values(error, &error_redactions));
        }
        (Ok(run), Ok(())) => run,
    };

    let status = if run.body_stream.truncated {
        "output_limit_exceeded"
    } else if (200..400).contains(&run.status_code) {
        "completed"
    } else {
        "failed"
    }
    .to_owned();
    let mut audit = vec![format!("adapter.invoked:{}", handle.id.as_str())];
    audit.extend(run.audit);
    audit.extend(header_plan.audit);
    audit.extend(credential_lease.audit_entries());
    if let Some(scope) = &credential_scope {
        audit.extend(scope.audit_entries());
    }
    Ok(AdapterInvokeReport {
        request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability,
        status,
        output: format!(
            "{{\"transport\":\"http\",\"method\":{},\"url\":{},\"status_code\":{},\"body\":{},\"duration_ms\":{}}}",
            escape_json(run.method.as_str()),
            escape_json(&run.url),
            run.status_code,
            provider_stream_summary_json(&run.body_stream),
            run.duration_ms
        ),
        audit,
        trace,
    })
}

/// 判断 `has_scoped_http_credentials` 对应的条件是否成立。
fn has_scoped_http_credentials(handle: &AdapterHandle) -> bool {
    !handle.credential_env.is_empty()
        || !handle.provider.vault_secrets.is_empty()
        || handle
            .headers
            .values()
            .any(|value| value.strip_prefix("env:").is_some())
}

/// 校验方法、输出上限、头部语法和精确来源；失败时客户端尚未被调用。
fn validate_invocation(
    config: &HttpRunnerConfig,
    invocation: &HttpInvocation,
) -> Result<(), EvaError> {
    if !config.allowed_methods.contains(&invocation.method) {
        return Err(
            EvaError::permission_denied("HTTP method is not allowlisted")
                .with_context("method", invocation.method.as_str()),
        );
    }
    if config.output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "HTTP output limit must be greater than zero",
        ));
    }
    for (name, value) in &invocation.headers {
        validate_header(name, value)?;
    }
    let origin = url_origin(&invocation.url)?;
    if !config.allowed_origins.contains(&origin) {
        return Err(
            EvaError::permission_denied("HTTP origin is not allowlisted")
                .with_context("origin", origin)
                .with_context("url", &invocation.url),
        );
    }
    Ok(())
}

/// 执行 `body_stream_config` 对应的处理逻辑。
fn body_stream_config(config: &HttpRunnerConfig) -> ProviderStreamConfig {
    let mut stream_config = ProviderStreamConfig::new("body", config.output_limit_bytes)
        .with_preview_limit(config.preview_limit_bytes)
        .with_chunk_size(config.stream_chunk_size_bytes);
    if let (Some(root), Some(key)) = (&config.artifact_root, &config.artifact_key) {
        stream_config = stream_config.with_artifact(root.clone(), key.clone(), "application/json");
    }
    stream_config
}

impl HttpInvocation {
    /// 设置 `headers` 并返回更新后的实例。
    pub fn with_headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.headers.extend(headers);
        self
    }
}

/// 执行 `url_origin` 对应的处理逻辑。
fn url_origin(url: &str) -> Result<String, EvaError> {
    Ok(ParsedHttpUrl::parse(url)?.origin)
}

/// 校验 `validate_header` 对应的约束，不满足时返回明确错误。
fn validate_header(name: &str, value: &str) -> Result<(), EvaError> {
    if name.trim().is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(
            EvaError::invalid_argument("HTTP header name is unsupported")
                .with_context("header", name),
        );
    }
    if value.contains('\r') || value.contains('\n') {
        return Err(
            EvaError::invalid_argument("HTTP header value must not contain newlines")
                .with_context("header", name),
        );
    }
    Ok(())
}

/// 保存连接、Host 头和来源校验共同使用的 URL 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedHttpUrl {
    /// 记录 `scheme` 字段对应的值。
    scheme: String,
    /// 记录 `host` 字段对应的值。
    host: String,
    /// 记录 `port` 字段对应的值。
    port: u16,
    /// 记录 `path` 字段对应的值。
    path: String,
    /// 记录 `authority` 字段对应的值。
    authority: String,
    /// 记录 `origin` 字段对应的值。
    origin: String,
}

impl ParsedHttpUrl {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    fn parse(url: &str) -> Result<Self, EvaError> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| EvaError::invalid_argument("HTTP URL must include a scheme"))?;
        if !matches!(scheme, "http" | "https") {
            return Err(EvaError::invalid_argument("HTTP URL scheme is unsupported")
                .with_context("url", url));
        }
        let authority = rest
            .split(['/', '?', '#'])
            .next()
            .filter(|authority| !authority.trim().is_empty())
            .ok_or_else(|| EvaError::invalid_argument("HTTP URL must include a host"))?;
        if authority.contains('@') {
            return Err(
                EvaError::invalid_argument("HTTP URL must not include userinfo")
                    .with_context("url", url),
            );
        }
        let (host, port) = parse_authority(scheme, authority)?;
        let path_start = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let path = if path_start < rest.len() {
            let value = &rest[path_start..];
            if value.starts_with('?') || value.starts_with('#') {
                format!("/{value}")
            } else {
                value.to_owned()
            }
        } else {
            "/".to_owned()
        };
        Ok(Self {
            scheme: scheme.to_owned(),
            host,
            port,
            path,
            authority: authority.to_owned(),
            origin: format!("{scheme}://{authority}"),
        })
    }
}

/// 读取或解析 `parse_authority` 所需的数据，失败时保留错误语义。
fn parse_authority(scheme: &str, authority: &str) -> Result<(String, u16), EvaError> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) {
            let port = port.parse::<u16>().map_err(|error| {
                EvaError::invalid_argument("HTTP URL port is invalid")
                    .with_context("port", port)
                    .with_context("parse_error", error.to_string())
            })?;
            return Ok((host.to_owned(), port));
        }
    }
    Ok((authority.to_owned(), default_port))
}

/// 定义 `HTTP_HEADER_LIMIT_BYTES` 常量。
const HTTP_HEADER_LIMIT_BYTES: usize = 64 * 1024;

/// 增量解析 HTTP 响应头并有界收集正文；头部超限和缺少终止符均视为协议失败。
fn read_http_response(
    stream: &mut TcpStream,
    origin: &str,
    output_limit_bytes: usize,
) -> Result<HttpClientResponse, EvaError> {
    if output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "HTTP output limit must be greater than zero",
        ));
    }
    let mut header_bytes = Vec::new();
    let mut status_code = None;
    let mut body = Vec::new();
    let mut body_truncated = false;
    let mut body_chunk_count = 0_usize;
    let mut buffer = vec![0_u8; DEFAULT_STREAM_CHUNK_SIZE_BYTES];

    loop {
        let read = stream.read(&mut buffer).map_err(|error| {
            EvaError::unavailable("failed to read HTTP provider response")
                .with_context("origin", origin)
                .with_context("io_error", error.to_string())
        })?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        if status_code.is_none() {
            header_bytes.extend_from_slice(chunk);
            if header_bytes.len() > HTTP_HEADER_LIMIT_BYTES {
                // 响应头不计入正文预算，因此单独设置上限，防止服务端用无终止头耗尽内存。
                return Err(
                    EvaError::conflict("HTTP provider response headers exceeded limit")
                        .with_context("origin", origin)
                        .with_context("header_limit_bytes", HTTP_HEADER_LIMIT_BYTES.to_string()),
                );
            }
            if let Some(header_end) = http_header_end(&header_bytes) {
                let head = String::from_utf8_lossy(&header_bytes[..header_end]).into_owned();
                status_code = Some(parse_http_status_code(&head)?);
                let body_start = header_end + 4;
                let pending_body = header_bytes[body_start..].to_vec();
                append_http_body_chunk(
                    &pending_body,
                    output_limit_bytes,
                    &mut body,
                    &mut body_chunk_count,
                    &mut body_truncated,
                );
                header_bytes.clear();
            }
        } else {
            append_http_body_chunk(
                chunk,
                output_limit_bytes,
                &mut body,
                &mut body_chunk_count,
                &mut body_truncated,
            );
        }
        if body_truncated {
            break;
        }
    }

    let status_code = status_code.ok_or_else(|| {
        EvaError::unavailable("HTTP provider returned malformed response")
            .with_context("response", "missing header terminator")
    })?;
    Ok(HttpClientResponse {
        status_code,
        body,
        body_truncated,
        body_chunk_count,
    })
}

/// 追加预算内的正文前缀；首次超限后保持截断状态并忽略后续数据。
fn append_http_body_chunk(
    chunk: &[u8],
    output_limit_bytes: usize,
    body: &mut Vec<u8>,
    body_chunk_count: &mut usize,
    body_truncated: &mut bool,
) {
    if chunk.is_empty() || *body_truncated {
        return;
    }
    *body_chunk_count = (*body_chunk_count).saturating_add(1);
    let remaining = output_limit_bytes.saturating_sub(body.len());
    if chunk.len() > remaining {
        body.extend_from_slice(&chunk[..remaining]);
        *body_truncated = true;
        return;
    }
    body.extend_from_slice(chunk);
}

/// 执行 `http_header_end` 对应的处理逻辑。
fn http_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// 读取或解析 `parse_http_status_code` 所需的数据，失败时保留错误语义。
fn parse_http_status_code(head: &str) -> Result<u16, EvaError> {
    let status_line = head.lines().next().ok_or_else(|| {
        EvaError::unavailable("HTTP provider returned malformed response")
            .with_context("response", "missing status line")
    })?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| {
            EvaError::unavailable("HTTP provider returned malformed response")
                .with_context("response", "missing status code")
        })?
        .parse::<u16>()
        .map_err(|error| {
            EvaError::unavailable("HTTP provider returned invalid status code")
                .with_context("parse_error", error.to_string())
        })?;
    Ok(status_code)
}

/// 表示 `HeaderPlan` 数据结构。
#[derive(Clone, PartialEq, Eq)]
struct HeaderPlan {
    /// 记录 `headers` 字段对应的值。
    headers: BTreeMap<String, String>,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
    /// 记录 `sensitive_values` 字段对应的值。
    sensitive_values: Vec<String>,
}

/// 执行 `http_headers` 对应的处理逻辑。
fn http_headers(
    handle: &AdapterHandle,
    credentials: &mut CredentialSessionLease,
) -> Result<HeaderPlan, EvaError> {
    let mut headers = BTreeMap::new();
    let mut audit = Vec::new();
    let mut sensitive_values = Vec::new();
    for (name, value) in &handle.headers {
        if let Some(env_name) = value.strip_prefix("env:") {
            let env_value = credentials.resolve_env(env_name).map_err(|_| {
                EvaError::unavailable("HTTP credential environment variable is missing")
                    .with_provider_code("missing_credential")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("env", env_name)
            })?;
            headers.insert(name.clone(), env_value.clone());
            if !env_value.is_empty() {
                sensitive_values.push(env_value.clone());
            }
            audit.push(format!("credential_header:{name}:env:{env_name}:redacted"));
        } else {
            headers.insert(name.clone(), value.clone());
            audit.push(format!("http.header:{name}:literal"));
        }
    }
    Ok(HeaderPlan {
        headers,
        audit,
        sensitive_values,
    })
}

/// 校验 `validate_input_size` 对应的约束，不满足时返回明确错误。
fn validate_input_size(handle: &AdapterHandle, input: &str) -> Result<(), EvaError> {
    if let Some(limit) = handle.max_prompt_bytes {
        if input.len() > limit {
            return Err(
                EvaError::conflict("HTTP provider input exceeded prompt limit")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("max_prompt_bytes", limit.to_string())
                    .with_context("actual_bytes", input.len().to_string()),
            );
        }
    }
    Ok(())
}

/// 执行 `timeout_ms` 对应的处理逻辑。
fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

/// 执行 `output_limit_bytes` 对应的处理逻辑。
fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
}

/// 按 `escape_json` 的协议约定生成输出。
fn escape_json(value: &str) -> String {
    let mut escaped = String::from("\"");
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
    escaped.push('"');
    escaped
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    /// 表示 `FakeHttpClient` 数据结构。
    #[derive(Debug, Clone)]
    struct FakeHttpClient {
        /// 记录 `response` 字段对应的值。
        response: Result<HttpClientResponse, EvaError>,
    }

    impl HttpClient for FakeHttpClient {
        /// 执行 `send` 对应的处理逻辑。
        fn send(
            &self,
            _invocation: &HttpInvocation,
            _timeout: Duration,
            _output_limit_bytes: usize,
        ) -> Result<HttpClientResponse, EvaError> {
            self.response.clone()
        }
    }

    /// 验证 `runner_denies_url_outside_allowlist` 场景下的预期行为。
    #[test]
    fn runner_denies_url_outside_allowlist() {
        let config = config();
        let invocation = HttpInvocation::new(HttpMethod::Post, "https://evil.example/v1/messages");

        let error = HttpRunner
            .run(&config, &client_with_body("ok"), invocation)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    /// 验证 `runner_denies_method_outside_allowlist` 场景下的预期行为。
    #[test]
    fn runner_denies_method_outside_allowlist() {
        let config = config();
        let invocation =
            HttpInvocation::new(HttpMethod::Delete, "https://api.example.test/v1/messages");

        let error = HttpRunner
            .run(&config, &client_with_body("ok"), invocation)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    /// 验证 `runner_maps_client_timeout` 场景下的预期行为。
    #[test]
    fn runner_maps_client_timeout() {
        let config = config();
        let client = FakeHttpClient {
            response: Err(EvaError::timeout("fake timeout")),
        };
        let invocation =
            HttpInvocation::new(HttpMethod::Post, "https://api.example.test/v1/messages");

        let error = HttpRunner.run(&config, &client, invocation).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    /// 验证 `runner_truncates_oversized_output` 场景下的预期行为。
    #[test]
    fn runner_truncates_oversized_output() {
        let config =
            HttpRunnerConfig::new(["https://api.example.test"], [HttpMethod::Post], 1_000, 2);
        let invocation =
            HttpInvocation::new(HttpMethod::Post, "https://api.example.test/v1/messages");

        let report = HttpRunner
            .run(&config, &client_with_body("too-large"), invocation)
            .unwrap();

        assert!(report.body_stream.truncated);
        assert_eq!(report.body, b"to");
    }

    /// 验证 `runner_completes_allowlisted_request` 场景下的预期行为。
    #[test]
    fn runner_completes_allowlisted_request() {
        let config = config();
        let invocation =
            HttpInvocation::new(HttpMethod::Post, "https://api.example.test/v1/messages")
                .with_header("content-type", "application/json")
                .with_body("{}");

        let report = HttpRunner
            .run(&config, &client_with_body("{\"ok\":true}"), invocation)
            .unwrap();

        assert_eq!(report.status_code, 200);
        assert_eq!(report.method, HttpMethod::Post);
        assert_eq!(report.body, br#"{"ok":true}"#);
        assert!(report.audit.contains(&"url_allowlist:passed".to_owned()));
    }

    /// 执行 `config` 对应的处理逻辑。
    fn config() -> HttpRunnerConfig {
        HttpRunnerConfig::new(
            ["https://api.example.test"],
            [HttpMethod::Get, HttpMethod::Post],
            1_000,
            1024,
        )
    }

    /// 执行 `client_with_body` 对应的处理逻辑。
    fn client_with_body(body: &str) -> FakeHttpClient {
        FakeHttpClient {
            response: Ok(HttpClientResponse::new(200, body.as_bytes())),
        }
    }
}
