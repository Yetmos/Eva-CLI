//! 中文：Eva-CLI 各 crate 共享的无副作用基础契约。
//! English: Side-effect-free foundation contracts shared across Eva-CLI crates.
//!
//! 中文：`eva-core` 只定义 topic、identifier、capability、event、invoke request
//! 和结构化错误等稳定数据类型，不负责运行时装配或外部 I/O。
//! `eva-core` defines stable data types for topics, identifiers,
//! capabilities, events, invoke requests, and structured errors. It does not
//! perform filesystem, network, shell, database, Lua, MCP, hardware, provider,
//! or runtime orchestration work.

/// Capability 名称、引用和 provider hint 契约。
pub mod capability;
/// 结构化错误分类、上下文和 provider code 契约。
pub mod error;
/// 事件 payload、目标、metadata 和链路契约。
pub mod event;
/// 跨 crate 使用的强类型标识符契约。
pub mod ids;
/// Capability/Agent/Adapter 调用请求与响应契约。
pub mod invoke;
/// Topic 与订阅 pattern 解析和匹配契约。
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
