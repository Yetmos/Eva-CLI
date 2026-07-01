//! 中文：Eva-CLI 各 crate 共享的无副作用基础契约。
//! English: Side-effect-free foundation contracts shared across Eva-CLI crates.
//!
//! 中文：`eva-core` 只定义 topic、identifier、capability、event、invoke request
//! 和结构化错误等稳定数据类型，不负责运行时装配或外部 I/O。
//! `eva-core` defines stable data types for topics, identifiers,
//! capabilities, events, invoke requests, and structured errors. It does not
//! perform filesystem, network, shell, database, Lua, MCP, hardware, provider,
//! or runtime orchestration work.

// 中文：模块保持公开，方便下游按边界导入具体契约。
// English: Modules stay public so downstream crates can import contracts by boundary.
pub mod capability;
pub mod error;
pub mod event;
pub mod ids;
pub mod invoke;
pub mod topic;

// 中文：高频公共类型在 crate 根重新导出，作为下游的稳定入口。
// English: Common contract types are re-exported at the crate root as the stable downstream entrypoint.
pub use capability::{CapabilityName, CapabilityRef, ProviderHint};
pub use error::{ErrorContext, ErrorKind, EvaError, ProviderCode};
pub use event::{Event, EventMetadata, EventPayload, EventTarget, TraceContext};
pub use ids::{AdapterId, AgentId, CapabilityId, EventId, GenerationId, RequestId};
pub use invoke::{
    InvokeInput, InvokeMetadata, InvokeOutput, InvokeRequest, InvokeResponse, InvokeStatus,
    InvokeTarget,
};
pub use topic::{Topic, TopicPattern, TopicPatternSegment};
