//! Memory and knowledge service boundary.

pub mod context_builder;
pub mod durable;
pub mod knowledge_service;
pub mod memory_service;
pub mod redaction;

pub use context_builder::{
    BuiltContext, ContextBudget, ContextBuilder, ContextRequest, LuaContextSnapshot,
};
pub use durable::{
    DurableIndexLockGuard, FileSystemKnowledgeStore, FileSystemMemoryStore,
    KnowledgeRebuildCheckpointReport, MemoryCompactionReport,
};
pub use knowledge_service::{
    ExternalKnowledgeRetrievalRequest, InMemoryKnowledgeService, KnowledgeId, KnowledgeItem,
    KnowledgeSearch, KnowledgeSearchResult, KnowledgeSource,
};
pub use memory_service::{
    InMemoryMemoryService, MemoryCompression, MemoryReadRequest, MemoryRecord, MemoryRetention,
    MemorySnapshot, MemoryVisibility, MemoryWrite,
};
pub use redaction::{redact_sensitive_text, RedactedText};
