//! Bounded provider stream capture and artifact evidence.

use crate::supervisor::redact_provider_session_tokens;
use eva_core::EvaError;
use eva_storage::{ArtifactRecord, FileSystemArtifactStore};
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "bounded provider stream capture with redacted artifact evidence";

pub const DEFAULT_STREAM_CHUNK_SIZE_BYTES: usize = 8 * 1024;
pub const DEFAULT_STREAM_PREVIEW_LIMIT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStreamConfig {
    pub stream_name: String,
    pub output_limit_bytes: usize,
    pub preview_limit_bytes: usize,
    pub chunk_size_bytes: usize,
    pub artifact_root: Option<PathBuf>,
    pub artifact_key: Option<String>,
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStreamCapture {
    pub stream_name: String,
    pub preview: Vec<u8>,
    pub captured_bytes: usize,
    pub chunk_count: usize,
    pub truncated: bool,
    pub artifact: Option<ProviderStreamArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStreamArtifact {
    pub key: String,
    pub digest: String,
    pub size_bytes: usize,
    pub content_type: String,
}

impl ProviderStreamConfig {
    pub fn new(stream_name: impl Into<String>, output_limit_bytes: usize) -> Self {
        Self {
            stream_name: stream_name.into(),
            output_limit_bytes,
            preview_limit_bytes: DEFAULT_STREAM_PREVIEW_LIMIT_BYTES,
            chunk_size_bytes: DEFAULT_STREAM_CHUNK_SIZE_BYTES,
            artifact_root: None,
            artifact_key: None,
            content_type: "application/octet-stream".to_owned(),
        }
    }

    pub fn with_preview_limit(mut self, preview_limit_bytes: usize) -> Self {
        self.preview_limit_bytes = preview_limit_bytes;
        self
    }

    pub fn with_chunk_size(mut self, chunk_size_bytes: usize) -> Self {
        self.chunk_size_bytes = chunk_size_bytes;
        self
    }

    pub fn with_artifact(
        mut self,
        artifact_root: impl Into<PathBuf>,
        artifact_key: impl Into<String>,
        content_type: impl Into<String>,
    ) -> Self {
        self.artifact_root = Some(artifact_root.into());
        self.artifact_key = Some(artifact_key.into());
        self.content_type = content_type.into();
        self
    }
}

impl ProviderStreamCapture {
    pub fn empty(stream_name: impl Into<String>) -> Self {
        Self {
            stream_name: stream_name.into(),
            preview: Vec::new(),
            captured_bytes: 0,
            chunk_count: 0,
            truncated: false,
            artifact: None,
        }
    }

    pub fn preview_text(&self) -> String {
        String::from_utf8_lossy(&self.preview).into_owned()
    }
}

pub fn collect_provider_stream(
    mut reader: impl Read,
    config: ProviderStreamConfig,
    sensitive_values: &[String],
) -> Result<ProviderStreamCapture, EvaError> {
    validate_stream_config(&config)?;
    let mut captured = Vec::new();
    let mut buffer = vec![0_u8; config.chunk_size_bytes];
    let mut chunk_count = 0_usize;
    let mut truncated = false;

    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            EvaError::unavailable("failed to read provider stream")
                .with_context("stream", &config.stream_name)
                .with_context("io_error", error.to_string())
        })?;
        if read == 0 {
            break;
        }
        chunk_count = chunk_count.saturating_add(1);
        let remaining = config.output_limit_bytes.saturating_sub(captured.len());
        if read > remaining {
            captured.extend_from_slice(&buffer[..remaining]);
            truncated = true;
            break;
        }
        captured.extend_from_slice(&buffer[..read]);
    }

    capture_provider_bytes(config, captured, chunk_count, truncated, sensitive_values)
}

pub fn capture_provider_bytes(
    config: ProviderStreamConfig,
    mut bytes: Vec<u8>,
    chunk_count: usize,
    mut truncated: bool,
    sensitive_values: &[String],
) -> Result<ProviderStreamCapture, EvaError> {
    validate_stream_config(&config)?;
    if bytes.len() > config.output_limit_bytes {
        bytes.truncate(config.output_limit_bytes);
        truncated = true;
    }
    let captured_bytes = bytes.len();
    let redacted = redact_provider_stream_bytes(bytes, sensitive_values);
    let preview_limit = config.preview_limit_bytes.min(config.output_limit_bytes);
    let preview_len = redacted.len().min(preview_limit);
    let preview = redacted[..preview_len].to_vec();
    let artifact = persist_stream_artifact(&config, redacted)?;
    Ok(ProviderStreamCapture {
        stream_name: config.stream_name,
        preview,
        captured_bytes,
        chunk_count,
        truncated,
        artifact,
    })
}

pub fn provider_stream_audit(capture: &ProviderStreamCapture) -> Vec<String> {
    let prefix = format!("stream.{}", capture.stream_name);
    let mut audit = vec![
        format!("{prefix}.bytes:{}", capture.captured_bytes),
        format!("{prefix}.chunks:{}", capture.chunk_count),
        format!("{prefix}.truncated:{}", capture.truncated),
    ];
    if let Some(artifact) = &capture.artifact {
        audit.push(format!("{prefix}.artifact:{}", artifact.key));
        audit.push(format!("{prefix}.artifact_digest:{}", artifact.digest));
    }
    audit
}

pub fn provider_stream_summary_json(capture: &ProviderStreamCapture) -> String {
    format!(
        "{{\"stream\":{},\"bytes\":{},\"chunks\":{},\"truncated\":{},\"preview\":{},\"artifact\":{}}}",
        json_string(&capture.stream_name),
        capture.captured_bytes,
        capture.chunk_count,
        capture.truncated,
        json_string(&capture.preview_text()),
        capture
            .artifact
            .as_ref()
            .map(provider_stream_artifact_json)
            .unwrap_or_else(|| "null".to_owned())
    )
}

pub fn provider_stream_artifact_json(artifact: &ProviderStreamArtifact) -> String {
    format!(
        "{{\"key\":{},\"digest\":{},\"size_bytes\":{},\"content_type\":{}}}",
        json_string(&artifact.key),
        json_string(&artifact.digest),
        artifact.size_bytes,
        json_string(&artifact.content_type)
    )
}

pub fn provider_stream_key(
    namespace: &str,
    adapter_id: &str,
    request_id: &str,
    stream_name: &str,
) -> String {
    format!(
        "{}/{}/{}/{}",
        safe_segment(namespace),
        safe_segment(adapter_id),
        safe_segment(request_id),
        safe_segment(stream_name)
    )
}

pub fn default_provider_artifact_root(source_path: &str) -> PathBuf {
    let source_path = PathBuf::from(source_path);
    if let Some(project_root) = project_root_from_manifest_path(&source_path) {
        return project_root.join(".eva").join("artifacts");
    }
    env::temp_dir().join("eva-provider-artifacts")
}

pub fn json_string(value: &str) -> String {
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

fn validate_stream_config(config: &ProviderStreamConfig) -> Result<(), EvaError> {
    if config.stream_name.trim().is_empty() {
        return Err(EvaError::invalid_argument(
            "provider stream name cannot be empty",
        ));
    }
    if config.output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "provider stream output limit must be greater than zero",
        ));
    }
    if config.preview_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "provider stream preview limit must be greater than zero",
        ));
    }
    if config.chunk_size_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "provider stream chunk size must be greater than zero",
        ));
    }
    match (&config.artifact_root, &config.artifact_key) {
        (Some(_), Some(key)) if key.trim().is_empty() => Err(EvaError::invalid_argument(
            "provider stream artifact key cannot be empty",
        )),
        (Some(_), Some(_)) | (None, None) => Ok(()),
        _ => Err(EvaError::invalid_argument(
            "provider stream artifact root and key must be provided together",
        )),
    }
}

fn persist_stream_artifact(
    config: &ProviderStreamConfig,
    bytes: Vec<u8>,
) -> Result<Option<ProviderStreamArtifact>, EvaError> {
    let (Some(root), Some(key)) = (&config.artifact_root, &config.artifact_key) else {
        return Ok(None);
    };
    let mut store = FileSystemArtifactStore::new(root);
    let record = store.put_bytes_with_metadata(
        key.clone(),
        bytes,
        config.content_type.clone(),
        "retain",
        None,
    )?;
    Ok(Some(ProviderStreamArtifact::from(record)))
}

pub(crate) fn redact_provider_stream_bytes(bytes: Vec<u8>, sensitive_values: &[String]) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    for value in sensitive_values {
        if !value.is_empty() {
            text = text.replace(value, "[REDACTED]");
        }
    }
    redact_provider_session_tokens(&text).into_bytes()
}

fn project_root_from_manifest_path(path: &Path) -> Option<PathBuf> {
    let config_dir = path.parent()?.parent()?;
    if config_dir.file_name().and_then(|value| value.to_str()) == Some("config") {
        return config_dir.parent().map(Path::to_path_buf);
    }
    None
}

fn safe_segment(value: &str) -> String {
    let mut segment = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect::<String>();
    if segment.trim_matches('_').is_empty() {
        segment = "unknown".to_owned();
    }
    segment
}

impl From<ArtifactRecord> for ProviderStreamArtifact {
    fn from(record: ArtifactRecord) -> Self {
        Self {
            key: record.key,
            digest: record.digest,
            size_bytes: record.size_bytes,
            content_type: record.content_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn provider_stream_truncates_preview_and_writes_redacted_artifact() {
        let root = test_root("truncated");
        let secret = "stream-secret";
        let config = ProviderStreamConfig::new("stdout", 4)
            .with_preview_limit(2)
            .with_chunk_size(3)
            .with_artifact(root.path.clone(), "provider/test/req/stdout", "text/plain");

        let capture = collect_provider_stream(
            "abcdefstream-secret".as_bytes(),
            config,
            &[secret.to_owned()],
        )
        .unwrap();

        assert!(capture.truncated);
        assert_eq!(capture.captured_bytes, 4);
        assert_eq!(capture.preview, b"ab");
        assert_eq!(
            capture.artifact.as_ref().unwrap().key,
            "provider/test/req/stdout"
        );
        assert!(root
            .path
            .join("objects/provider/test/req/stdout.artifact")
            .exists());
    }

    #[test]
    fn provider_stream_redacts_artifact_bytes() {
        let root = test_root("redaction");
        let secret = "secret-value";
        let config = ProviderStreamConfig::new("body", 128).with_artifact(
            root.path.clone(),
            "provider/http/req/body",
            "text/plain",
        );

        let capture = collect_provider_stream(
            "before secret-value after".as_bytes(),
            config,
            &[secret.to_owned()],
        )
        .unwrap();
        let artifact_path = root.path.join("objects/provider/http/req/body.artifact");
        let artifact = fs::read_to_string(artifact_path).unwrap();

        assert_eq!(capture.preview_text(), "before [REDACTED] after");
        assert!(!artifact.contains(secret));
        assert!(artifact.contains("[REDACTED]"));
    }

    struct TestRoot {
        path: PathBuf,
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: env::temp_dir().join(format!(
                "eva-adapter-stream-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
