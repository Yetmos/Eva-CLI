//! 中文：事件发布、确认、死信和恢复能力的 EventBus 边界。
//! EventBus boundary for publication and recovery.

pub mod bus;
pub mod dead_letter;
pub mod durable;
pub mod in_memory;
pub mod recoverable;

pub use bus::{EventBus, EventReceipt};
pub use dead_letter::{DeadLetterQueue, DeadLetterRecord, RedrivePolicy, ReplayHandlerBinding};
pub use durable::{DurableEventBus, FileSystemDeadLetterStore};
pub use in_memory::InMemoryEventBus;
