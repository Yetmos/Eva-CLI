//! Invoke request and response contracts.

use crate::capability::CapabilityName;
use crate::error::EvaError;
use crate::event::{EventPayload, TraceContext};
use crate::ids::{AdapterId, AgentId, GenerationId, RequestId};
use std::time::Duration;

/// Target of an invoke request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InvokeTarget {
    /// Invoke a specific Agent.
    Agent(AgentId),
    /// Invoke a named Capability.
    Capability(CapabilityName),
    /// Invoke a specific Adapter.
    Adapter(AdapterId),
}

/// Opaque invoke input.
pub type InvokeInput = EventPayload;

/// Opaque invoke output.
pub type InvokeOutput = EventPayload;

/// Non-business metadata controlling an invoke request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InvokeMetadata {
    timeout: Option<Duration>,
    trace: TraceContext,
    generation_id: Option<GenerationId>,
    caller: Option<AgentId>,
}

impl InvokeMetadata {
    /// Creates empty metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the timeout budget, when present.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Returns trace context.
    pub fn trace(&self) -> &TraceContext {
        &self.trace
    }

    /// Returns the generation id, when present.
    pub fn generation_id(&self) -> Option<&GenerationId> {
        self.generation_id.as_ref()
    }

    /// Returns the calling Agent id, when present.
    pub fn caller(&self) -> Option<&AgentId> {
        self.caller.as_ref()
    }

    /// Sets the timeout budget.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets trace context.
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        self.trace = trace;
        self
    }

    /// Sets generation id.
    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.generation_id = Some(generation_id);
        self
    }

    /// Sets the calling Agent id.
    pub fn with_caller(mut self, caller: AgentId) -> Self {
        self.caller = Some(caller);
        self
    }
}

/// A request to invoke an Agent, Capability, or Adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeRequest {
    request_id: RequestId,
    target: InvokeTarget,
    input: InvokeInput,
    metadata: InvokeMetadata,
}

impl InvokeRequest {
    /// Creates an invoke request with default metadata.
    pub fn new(request_id: RequestId, target: InvokeTarget, input: InvokeInput) -> Self {
        Self {
            request_id,
            target,
            input,
            metadata: InvokeMetadata::new(),
        }
    }

    /// Returns the request id.
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    /// Returns the invoke target.
    pub fn target(&self) -> &InvokeTarget {
        &self.target
    }

    /// Returns the opaque input payload.
    pub fn input(&self) -> &InvokeInput {
        &self.input
    }

    /// Returns metadata.
    pub fn metadata(&self) -> &InvokeMetadata {
        &self.metadata
    }

    /// Replaces metadata.
    pub fn with_metadata(mut self, metadata: InvokeMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Lifecycle status of an invoke response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InvokeStatus {
    /// Runtime accepted the request, with result to follow through another path.
    Accepted,
    /// Request completed successfully.
    Completed,
    /// Request failed with an error.
    Failed,
    /// Request was cancelled.
    Cancelled,
    /// Request exceeded its timeout.
    Timeout,
}

impl InvokeStatus {
    /// Returns true for terminal statuses.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Timeout
        )
    }
}

/// Result of an invoke request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeResponse {
    request_id: RequestId,
    status: InvokeStatus,
    output: Option<InvokeOutput>,
    error: Option<EvaError>,
    metadata: InvokeMetadata,
}

impl InvokeResponse {
    /// Creates an accepted response.
    pub fn accepted(request_id: RequestId) -> Self {
        Self::new(request_id, InvokeStatus::Accepted, None, None)
    }

    /// Creates a completed response with output.
    pub fn completed(request_id: RequestId, output: InvokeOutput) -> Self {
        Self::new(request_id, InvokeStatus::Completed, Some(output), None)
    }

    /// Creates a failed response with a structured error.
    pub fn failed(request_id: RequestId, error: EvaError) -> Self {
        Self::new(request_id, InvokeStatus::Failed, None, Some(error))
    }

    /// Creates a cancelled response. A reason may be supplied as a structured error.
    pub fn cancelled(request_id: RequestId, reason: Option<EvaError>) -> Self {
        let error = reason.or_else(|| Some(EvaError::conflict("invoke request was cancelled")));
        Self::new(request_id, InvokeStatus::Cancelled, None, error)
    }

    /// Creates a timeout response.
    pub fn timeout(request_id: RequestId, message: impl Into<String>) -> Self {
        Self::new(
            request_id,
            InvokeStatus::Timeout,
            None,
            Some(EvaError::timeout(message)),
        )
    }

    fn new(
        request_id: RequestId,
        status: InvokeStatus,
        output: Option<InvokeOutput>,
        error: Option<EvaError>,
    ) -> Self {
        Self {
            request_id,
            status,
            output,
            error,
            metadata: InvokeMetadata::new(),
        }
    }

    /// Returns the request id this response belongs to.
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    /// Returns response status.
    pub fn status(&self) -> InvokeStatus {
        self.status
    }

    /// Returns output, when present.
    pub fn output(&self) -> Option<&InvokeOutput> {
        self.output.as_ref()
    }

    /// Returns error, when present.
    pub fn error(&self) -> Option<&EvaError> {
        self.error.as_ref()
    }

    /// Returns metadata.
    pub fn metadata(&self) -> &InvokeMetadata {
        &self.metadata
    }

    /// Replaces metadata.
    pub fn with_metadata(mut self, metadata: InvokeMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns true only for completed responses.
    pub fn is_success(&self) -> bool {
        self.status == InvokeStatus::Completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_id(value: &str) -> RequestId {
        RequestId::parse(value).unwrap()
    }

    #[test]
    fn invoke_request_requires_target() {
        let target = InvokeTarget::Agent(AgentId::parse("agent-root").unwrap());
        let request = InvokeRequest::new(request_id("req-1"), target, InvokeInput::text("run"));

        assert_eq!(request.request_id().as_str(), "req-1");
        assert!(matches!(request.target(), InvokeTarget::Agent(_)));
        assert_eq!(request.input().as_text(), Some("run"));
    }

    #[test]
    fn invoke_request_accepts_metadata() {
        let target = InvokeTarget::Capability(CapabilityName::parse("repo.summary").unwrap());
        let metadata = InvokeMetadata::new()
            .with_timeout(Duration::from_secs(5))
            .with_generation_id(GenerationId::parse("gen-1").unwrap())
            .with_caller(AgentId::parse("agent-root").unwrap());
        let request = InvokeRequest::new(request_id("req-1"), target, InvokeInput::empty())
            .with_metadata(metadata);

        assert_eq!(request.metadata().timeout(), Some(Duration::from_secs(5)));
        assert_eq!(
            request.metadata().generation_id().unwrap().as_str(),
            "gen-1"
        );
        assert_eq!(request.metadata().caller().unwrap().as_str(), "agent-root");
    }

    #[test]
    fn completed_response_is_success() {
        let response = InvokeResponse::completed(request_id("req-1"), InvokeOutput::text("ok"));

        assert_eq!(response.status(), InvokeStatus::Completed);
        assert!(response.is_success());
        assert!(response.status().is_terminal());
        assert_eq!(response.output().unwrap().as_text(), Some("ok"));
    }

    #[test]
    fn accepted_response_is_not_terminal() {
        let response = InvokeResponse::accepted(request_id("req-1"));

        assert_eq!(response.status(), InvokeStatus::Accepted);
        assert!(!response.is_success());
        assert!(!response.status().is_terminal());
        assert!(response.output().is_none());
    }

    #[test]
    fn failed_response_carries_error() {
        let error = EvaError::not_found("missing agent");
        let response = InvokeResponse::failed(request_id("req-1"), error.clone());

        assert_eq!(response.status(), InvokeStatus::Failed);
        assert_eq!(response.error(), Some(&error));
        assert!(response.status().is_terminal());
    }

    #[test]
    fn timeout_response_is_terminal() {
        let response = InvokeResponse::timeout(request_id("req-1"), "agent timed out");

        assert_eq!(response.status(), InvokeStatus::Timeout);
        assert!(response.status().is_terminal());
        assert!(response.error().unwrap().is_retryable());
    }

    #[test]
    fn cancelled_response_carries_default_reason() {
        let response = InvokeResponse::cancelled(request_id("req-1"), None);

        assert_eq!(response.status(), InvokeStatus::Cancelled);
        assert!(response.status().is_terminal());
        assert!(response.error().is_some());
    }

    #[test]
    fn invoke_target_supports_agent_capability_and_adapter() {
        let agent = InvokeTarget::Agent(AgentId::parse("agent-root").unwrap());
        let capability = InvokeTarget::Capability(CapabilityName::parse("repo.summary").unwrap());
        let adapter = InvokeTarget::Adapter(AdapterId::parse("adapter-cli").unwrap());

        assert!(matches!(agent, InvokeTarget::Agent(_)));
        assert!(matches!(capability, InvokeTarget::Capability(_)));
        assert!(matches!(adapter, InvokeTarget::Adapter(_)));
    }
}
