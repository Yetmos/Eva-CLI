//! HTTP transport runner contract.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "HTTP transport with env allowlist-based credentials";

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
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunnerConfig {
    pub allowed_origins: BTreeSet<String>,
    pub allowed_methods: BTreeSet<HttpMethod>,
    pub timeout_ms: u64,
    pub output_limit_bytes: usize,
    pub preview_limit_bytes: usize,
    pub stream_chunk_size_bytes: usize,
    pub artifact_root: Option<std::path::PathBuf>,
    pub artifact_key: Option<String>,
    pub sensitive_values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpInvocation {
    pub method: HttpMethod,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunReport {
    pub method: HttpMethod,
    pub url: String,
    pub status_code: u16,
    pub body: Vec<u8>,
    pub body_stream: ProviderStreamCapture,
    pub duration_ms: u128,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

pub trait HttpClient {
    fn send(
        &self,
        invocation: &HttpInvocation,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> Result<HttpClientResponse, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpClientResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
    pub body_truncated: bool,
    pub body_chunk_count: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HttpRunner;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TcpHttpClient;

impl HttpRunnerConfig {
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

    pub fn with_artifact_sink(
        mut self,
        artifact_root: impl Into<std::path::PathBuf>,
        artifact_key: impl Into<String>,
    ) -> Self {
        self.artifact_root = Some(artifact_root.into());
        self.artifact_key = Some(artifact_key.into());
        self
    }

    pub fn with_sensitive_values(mut self, sensitive_values: Vec<String>) -> Self {
        self.sensitive_values = sensitive_values;
        self
    }
}

impl HttpInvocation {
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

impl HttpMethod {
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

pub fn invoke(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    invoke_with_client(handle, invocation, &TcpHttpClient)
}

pub fn invoke_with_client(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
    client: &impl HttpClient,
) -> Result<AdapterInvokeReport, EvaError> {
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
    let method = HttpMethod::parse(handle.method.as_deref().unwrap_or("POST"))?;
    let header_plan = http_headers(handle)?;
    let mut sensitive_values = header_plan.sensitive_values.clone();
    sensitive_values.extend(credential_env_values(&handle.credential_env).values);
    if let Some(scope) = &credential_scope {
        sensitive_values.extend(scope.redaction_values());
    }
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
    let run = HttpRunner.run(
        &config,
        client,
        HttpInvocation::new(method, endpoint)
            .with_headers(headers)
            .with_body(invocation.input.as_bytes().to_vec()),
    )?;

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
    audit.extend(credential_env_audit(&handle.credential_env));
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

fn has_scoped_http_credentials(handle: &AdapterHandle) -> bool {
    !handle.credential_env.is_empty()
        || handle
            .headers
            .values()
            .any(|value| value.strip_prefix("env:").is_some())
}

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
    pub fn with_headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.headers.extend(headers);
        self
    }
}

fn url_origin(url: &str) -> Result<String, EvaError> {
    Ok(ParsedHttpUrl::parse(url)?.origin)
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedHttpUrl {
    scheme: String,
    host: String,
    port: u16,
    path: String,
    authority: String,
    origin: String,
}

impl ParsedHttpUrl {
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

const HTTP_HEADER_LIMIT_BYTES: usize = 64 * 1024;

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

fn http_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeaderPlan {
    headers: BTreeMap<String, String>,
    audit: Vec<String>,
    sensitive_values: Vec<String>,
}

fn http_headers(handle: &AdapterHandle) -> Result<HeaderPlan, EvaError> {
    let mut headers = BTreeMap::new();
    let mut audit = Vec::new();
    let mut sensitive_values = Vec::new();
    for (name, value) in &handle.headers {
        if let Some(env_name) = value.strip_prefix("env:") {
            let env_value = std::env::var(env_name).map_err(|_| {
                EvaError::unavailable("HTTP credential environment variable is missing")
                    .with_provider_code("missing_credential")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("env", env_name)
            })?;
            headers.insert(name.clone(), env_value.clone());
            if !env_value.is_empty() {
                sensitive_values.push(env_value);
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

fn credential_env_values(names: &[String]) -> CredentialValues {
    let mut values = Vec::new();
    for name in names {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                values.push(value);
            }
        }
    }
    CredentialValues { values }
}

fn credential_env_audit(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| {
            if std::env::var(name).is_ok() {
                format!("credential_env:{name}:redacted")
            } else {
                format!("credential_env:{name}:missing")
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialValues {
    values: Vec<String>,
}

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

fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    #[derive(Debug, Clone)]
    struct FakeHttpClient {
        response: Result<HttpClientResponse, EvaError>,
    }

    impl HttpClient for FakeHttpClient {
        fn send(
            &self,
            _invocation: &HttpInvocation,
            _timeout: Duration,
            _output_limit_bytes: usize,
        ) -> Result<HttpClientResponse, EvaError> {
            self.response.clone()
        }
    }

    #[test]
    fn runner_denies_url_outside_allowlist() {
        let config = config();
        let invocation = HttpInvocation::new(HttpMethod::Post, "https://evil.example/v1/messages");

        let error = HttpRunner
            .run(&config, &client_with_body("ok"), invocation)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

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

    fn config() -> HttpRunnerConfig {
        HttpRunnerConfig::new(
            ["https://api.example.test"],
            [HttpMethod::Get, HttpMethod::Post],
            1_000,
            1024,
        )
    }

    fn client_with_body(body: &str) -> FakeHttpClient {
        FakeHttpClient {
            response: Ok(HttpClientResponse::new(200, body.as_bytes())),
        }
    }
}
