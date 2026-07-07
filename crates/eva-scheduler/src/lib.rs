//! Scheduler boundary for routing events to Agent mailboxes.

pub mod generation;
pub mod mailbox;
pub mod matcher;
pub mod registry;
pub mod routing;
pub mod subscription;

pub use generation::{GenerationRouteCandidate, GenerationRouteGate};
pub use mailbox::AgentMailbox;
pub use matcher::matching_rules;
pub use registry::MailboxRegistry;
pub use routing::{DeliveryMode, RoutingRule};
pub use subscription::{DeliveryPlan, SubscriptionTable};
