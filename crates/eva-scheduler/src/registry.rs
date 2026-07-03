//! Registered Agent mailbox metadata.

use crate::mailbox::AgentMailbox;
use eva_core::{AgentId, EvaError, Event};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "registered Agent mailbox metadata";

/// Registry of Agent mailboxes owned by the scheduler boundary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MailboxRegistry {
    mailboxes: BTreeMap<AgentId, AgentMailbox>,
}

impl MailboxRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, agent_id: AgentId, capacity: usize) -> Result<(), EvaError> {
        if self.mailboxes.contains_key(&agent_id) {
            return Err(EvaError::conflict("agent mailbox already registered")
                .with_context("agent_id", agent_id.as_str()));
        }
        self.mailboxes
            .insert(agent_id, AgentMailbox::new(capacity)?);
        Ok(())
    }

    pub fn deliver(&mut self, agent_id: &AgentId, event: Event) -> Result<(), EvaError> {
        let mailbox = self.mailboxes.get_mut(agent_id).ok_or_else(|| {
            EvaError::not_found("agent mailbox is not registered")
                .with_context("agent_id", agent_id.as_str())
        })?;
        mailbox.push(event)
    }

    pub fn drain_one(&mut self, agent_id: &AgentId) -> Result<Option<Event>, EvaError> {
        let mailbox = self.mailboxes.get_mut(agent_id).ok_or_else(|| {
            EvaError::not_found("agent mailbox is not registered")
                .with_context("agent_id", agent_id.as_str())
        })?;
        Ok(mailbox.pop())
    }

    pub fn mailbox(&self, agent_id: &AgentId) -> Option<&AgentMailbox> {
        self.mailboxes.get(agent_id)
    }

    pub fn len(&self) -> usize {
        self.mailboxes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mailboxes.is_empty()
    }
}
