//! HTTP transport runner contract.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "HTTP transport with env allowlist-based credentials";

use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunnerConfig {
    pub allowed_origins: BTreeSet<String>,
    pub allowed_methods: BTreeSet<HttpMethod>,
    pub timeout_ms: u64,
    pub output_limit_bytes: usize,
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
    ) -> Result<HttpClientResponse, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpClientResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HttpRunner;

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
        }
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
        let response = client.send(&invocation, timeout)?;
        if response.body.len() > config.output_limit_bytes {
            return Err(EvaError::conflict("HTTP provider output exceeded limit")
                .with_context("url", &invocation.url)
                .with_context("output_limit_bytes", config.output_limit_bytes.to_string())
                .with_context("actual_bytes", response.body.len().to_string()));
        }
        if !timeout.is_zero() && started_at.elapsed() >= timeout {
            return Err(EvaError::timeout("HTTP provider timed out")
                .with_context("url", &invocation.url)
                .with_context("timeout_ms", config.timeout_ms.to_string()));
        }
        Ok(HttpRunReport {
            method: invocation.method,
            url: invocation.url,
            status_code: response.status_code,
            body: response.body,
            duration_ms: started_at.elapsed().as_millis(),
            audit: vec![
                "transport:http".to_owned(),
                format!("method:{}", invocation.method.as_str()),
                "url_allowlist:passed".to_owned(),
            ],
        })
    }
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

fn url_origin(url: &str) -> Result<String, EvaError> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| EvaError::invalid_argument("HTTP URL must include a scheme"))?;
    if !matches!(scheme, "http" | "https") {
        return Err(
            EvaError::invalid_argument("HTTP URL scheme is unsupported").with_context("url", url)
        );
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
    Ok(format!("{scheme}://{authority}"))
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
        ) -> Result<HttpClientResponse, EvaError> {
            self.response.clone()
        }
    }

    #[test]
    fn runner_denies_url_outside_allowlist() {
        let config = config();
        let invocation = HttpInvocation::new(HttpMethod::Post, "https://evil.example/v1/messages");

        let error = HttpRunner::default()
            .run(&config, &client_with_body("ok"), invocation)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn runner_denies_method_outside_allowlist() {
        let config = config();
        let invocation =
            HttpInvocation::new(HttpMethod::Delete, "https://api.example.test/v1/messages");

        let error = HttpRunner::default()
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

        let error = HttpRunner::default()
            .run(&config, &client, invocation)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    #[test]
    fn runner_rejects_oversized_output() {
        let config =
            HttpRunnerConfig::new(["https://api.example.test"], [HttpMethod::Post], 1_000, 2);
        let invocation =
            HttpInvocation::new(HttpMethod::Post, "https://api.example.test/v1/messages");

        let error = HttpRunner::default()
            .run(&config, &client_with_body("too-large"), invocation)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Conflict);
    }

    #[test]
    fn runner_completes_allowlisted_request() {
        let config = config();
        let invocation =
            HttpInvocation::new(HttpMethod::Post, "https://api.example.test/v1/messages")
                .with_header("content-type", "application/json")
                .with_body("{}");

        let report = HttpRunner::default()
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
            response: Ok(HttpClientResponse {
                status_code: 200,
                body: body.as_bytes().to_vec(),
            }),
        }
    }
}
