//! 中文：Eva 各 crate 共享的结构化错误契约。
//! English: Structured error contracts shared across Eva crates.

use std::fmt;

/// 中文：Eva 错误的高层分类，用于路由重试、日志聚合和机器可读输出。
/// English: High-level Eva error category for retry routing, log aggregation, and machine-readable output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// 中文：调用方传入了无效输入。
    /// English: Caller supplied invalid input.
    InvalidArgument,
    /// 中文：请求的 Agent、Adapter、Capability 或资源不存在。
    /// English: The requested Agent, Adapter, Capability, or resource does not exist.
    NotFound,
    /// 中文：请求操作与当前状态冲突。
    /// English: The requested operation conflicts with current state.
    Conflict,
    /// 中文：策略或 sandbox 拒绝了该操作。
    /// English: Policy or sandbox denied the operation.
    PermissionDenied,
    /// 中文：操作超过了 timeout budget。
    /// English: The operation exceeded its timeout budget.
    Timeout,
    /// 中文：Provider 或 runtime 组件暂时不可用。
    /// English: A provider or runtime component is temporarily unavailable.
    Unavailable,
    /// 中文：发生了非预期内部错误。
    /// English: An unexpected internal failure occurred.
    Internal,
    /// 中文：请求的 capability 或操作不受支持。
    /// English: The requested capability or operation is unsupported.
    Unsupported,
}

impl ErrorKind {
    /// 中文：返回该错误分类的默认重试判断。
    /// English: Returns the default retry classification for this error kind.
    pub const fn default_retryable(self) -> bool {
        matches!(self, Self::Timeout | Self::Unavailable)
    }

    /// 中文：返回稳定 snake_case 错误码，供日志和机器可读输出使用。
    /// English: Returns the stable snake_case code used in logs and machine-readable output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::PermissionDenied => "permission_denied",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
            Self::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 中文：Provider 私有错误码，只作为数据透传，不解释其协议含义。
/// English: Provider-specific error code kept as data, without interpreting its protocol.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderCode(String);

impl ProviderCode {
    /// 中文：输入 trim 后非空时创建 provider code。
    /// English: Creates a provider code when the supplied value is non-empty after trim.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_owned()))
        }
    }

    /// 中文：以字符串切片返回 provider code。
    /// English: Returns the provider code as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 中文：附加在错误上的非敏感结构化上下文。
/// English: Non-sensitive structured context attached to an error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ErrorContext {
    // 中文：保留插入顺序，方便日志输出按调用方补充上下文的顺序呈现。
    // English: Preserve insertion order so logs reflect the caller's context-building order.
    entries: Vec<(String, String)>,
}

impl ErrorContext {
    /// 中文：创建空上下文。
    /// English: Creates an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：添加一条非敏感 key/value 上下文并返回新值。
    /// English: Adds one non-sensitive key/value context entry and returns the updated value.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.push(key, value);
        self
    }

    /// 中文：原地添加一条非敏感 key/value 上下文。
    /// English: Adds one non-sensitive key/value context entry in place.
    pub fn push(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        // 中文：忽略空 key，避免生成不可检索或不可聚合的上下文字段。
        // English: Ignore empty keys to avoid context fields that cannot be queried or aggregated.
        if !key.trim().is_empty() {
            self.entries.push((key, value));
        }
    }

    /// 中文：按插入顺序返回上下文条目。
    /// English: Returns context entries in insertion order.
    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }

    /// 中文：没有上下文条目时返回 true。
    /// English: Returns true when no context entries are present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 中文：跨 Eva crate 边界传递的标准错误值。
/// English: Standard error value crossing Eva crate boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaError {
    // 中文：错误分类必须稳定，供上层策略判断是否重试或降级。
    // English: Error kind must remain stable so upper policy can decide retry or fallback behavior.
    kind: ErrorKind,
    // 中文：面向人的错误消息；不要在这里放 secret、token 或大块 provider 原始响应。
    // English: Human-readable message; do not store secrets, tokens, or large raw provider responses here.
    message: String,
    // 中文：允许调用方覆盖默认重试分类，以表达 provider/runtime 的具体语义。
    // English: Allows callers to override the default retry classification with provider/runtime-specific semantics.
    retryable: bool,
    // 中文：保留 provider 私有码用于诊断，但 eva-core 不解释它。
    // English: Preserve provider-private codes for diagnostics, without interpretation in eva-core.
    provider_code: Option<ProviderCode>,
    // 中文：只允许非敏感上下文，便于日志和 telemetry 安全输出。
    // English: Context must stay non-sensitive so logs and telemetry can emit it safely.
    context: ErrorContext,
}

impl EvaError {
    /// 中文：使用 `kind` 的默认重试分类创建错误。
    /// English: Creates an error with the default retry classification for `kind`.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: kind.default_retryable(),
            provider_code: None,
            context: ErrorContext::new(),
        }
    }

    /// 中文：创建 `InvalidArgument` 错误。
    /// English: Creates an `InvalidArgument` error.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    /// 中文：创建 `NotFound` 错误。
    /// English: Creates a `NotFound` error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    /// 中文：创建 `Conflict` 错误。
    /// English: Creates a `Conflict` error.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }

    /// 中文：创建 `PermissionDenied` 错误。
    /// English: Creates a `PermissionDenied` error.
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PermissionDenied, message)
    }

    /// 中文：创建 `Timeout` 错误。
    /// English: Creates a `Timeout` error.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, message)
    }

    /// 中文：创建 `Unavailable` 错误。
    /// English: Creates an `Unavailable` error.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }

    /// 中文：创建 `Internal` 错误。
    /// English: Creates an `Internal` error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// 中文：创建 `Unsupported` 错误。
    /// English: Creates an `Unsupported` error.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    /// 中文：返回错误分类。
    /// English: Returns the error kind.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// 中文：返回面向人的错误消息。
    /// English: Returns the human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 中文：返回上层是否可以重试该失败。
    /// English: Returns whether upper layers may retry this failure.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// 中文：覆盖重试分类。
    /// English: Overrides retry classification.
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// 中文：附加可选 provider code；空 code 会被忽略。
    /// English: Attaches an optional provider code; empty codes are ignored.
    pub fn with_provider_code(mut self, code: impl Into<String>) -> Self {
        self.provider_code = ProviderCode::new(code);
        self
    }

    /// 中文：附加一条非敏感上下文。
    /// English: Attaches one non-sensitive context entry.
    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.push(key, value);
        self
    }

    /// 中文：整体替换上下文。
    /// English: Replaces context.
    pub fn with_error_context(mut self, context: ErrorContext) -> Self {
        self.context = context;
        self
    }

    /// 中文：存在 provider code 时返回它。
    /// English: Returns the provider code, when one is present.
    pub fn provider_code(&self) -> Option<&ProviderCode> {
        self.provider_code.as_ref()
    }

    /// 中文：返回非敏感上下文条目。
    /// English: Returns non-sensitive context entries.
    pub fn context(&self) -> &ErrorContext {
        &self.context
    }
}

impl fmt::Display for EvaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for EvaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_argument_is_not_retryable_by_default() {
        let error = EvaError::invalid_argument("bad topic");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(!error.is_retryable());
    }

    #[test]
    fn timeout_is_retryable_by_default() {
        let error = EvaError::timeout("provider timed out");
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(error.is_retryable());
    }

    #[test]
    fn error_display_contains_kind_and_message() {
        let error = EvaError::not_found("missing adapter");
        assert_eq!(error.to_string(), "not_found: missing adapter");
    }

    #[test]
    fn provider_code_is_optional() {
        let without_code = EvaError::unavailable("provider offline");
        assert!(without_code.provider_code().is_none());

        let with_code = without_code.with_provider_code("EHOSTDOWN");
        assert_eq!(with_code.provider_code().unwrap().as_str(), "EHOSTDOWN");
    }

    #[test]
    fn context_does_not_change_kind() {
        let error = EvaError::conflict("generation changed").with_context("generation", "gen-2");

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(
            error.context().entries(),
            &[("generation".to_owned(), "gen-2".to_owned())]
        );
    }

    #[test]
    fn retryable_can_be_overridden() {
        let error = EvaError::internal("transient executor failure").with_retryable(true);
        assert!(error.is_retryable());
    }
}
