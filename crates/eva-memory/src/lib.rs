//! Memory and knowledge service boundary.

pub mod context_builder;
pub mod knowledge_service;
pub mod memory_service;

pub use context_builder::{
    BuiltContext, ContextBudget, ContextBuilder, ContextRequest, LuaContextSnapshot,
};
pub use knowledge_service::{
    InMemoryKnowledgeService, KnowledgeId, KnowledgeItem, KnowledgeSearch, KnowledgeSearchResult,
    KnowledgeSource,
};
pub use memory_service::{
    InMemoryMemoryService, MemoryReadRequest, MemoryRecord, MemoryRetention, MemorySnapshot,
    MemoryVisibility, MemoryWrite,
};
