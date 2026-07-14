//! 中文：已注册 Agent 邮箱的所有权与查找表。
//! Registered Agent mailbox metadata.

use crate::mailbox::AgentMailbox;
use eva_core::{AgentId, EvaError, Event};
use std::collections::BTreeMap;

/// 中文：本模块负责维护 Agent 标识到有界邮箱的一一映射。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "registered Agent mailbox metadata";

/// 中文：调度器独占的 Agent 邮箱注册表，使用有序映射保证诊断输出稳定。
/// Registry of Agent mailboxes owned by the scheduler boundary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MailboxRegistry {
    /// 中文：Agent 标识到其私有邮箱的映射。
    mailboxes: BTreeMap<AgentId, AgentMailbox>,
}

impl MailboxRegistry {
    /// 中文：创建空邮箱注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：为 Agent 注册指定容量的邮箱；重复标识会被拒绝，避免覆盖待处理事件。
    pub fn register(&mut self, agent_id: AgentId, capacity: usize) -> Result<(), EvaError> {
        if self.mailboxes.contains_key(&agent_id) {
            return Err(EvaError::conflict("agent mailbox already registered")
                .with_context("agent_id", agent_id.as_str()));
        }
        self.mailboxes
            .insert(agent_id, AgentMailbox::new(capacity)?);
        Ok(())
    }

    /// 中文：把事件投递到指定 Agent 邮箱；未注册和邮箱满载分别向上传递明确错误。
    pub fn deliver(&mut self, agent_id: &AgentId, event: Event) -> Result<(), EvaError> {
        let mailbox = self.mailboxes.get_mut(agent_id).ok_or_else(|| {
            EvaError::not_found("agent mailbox is not registered")
                .with_context("agent_id", agent_id.as_str())
        })?;
        mailbox.push(event)
    }

    /// 中文：从指定 Agent 邮箱取出一个最早事件；邮箱存在但为空时返回 `Ok(None)`。
    pub fn drain_one(&mut self, agent_id: &AgentId) -> Result<Option<Event>, EvaError> {
        let mailbox = self.mailboxes.get_mut(agent_id).ok_or_else(|| {
            EvaError::not_found("agent mailbox is not registered")
                .with_context("agent_id", agent_id.as_str())
        })?;
        Ok(mailbox.pop())
    }

    /// 中文：按 Agent 标识获取邮箱的只读视图。
    pub fn mailbox(&self, agent_id: &AgentId) -> Option<&AgentMailbox> {
        self.mailboxes.get(agent_id)
    }

    /// 中文：返回已注册邮箱数量。
    pub fn len(&self) -> usize {
        self.mailboxes.len()
    }

    /// 中文：判断注册表是否为空。
    pub fn is_empty(&self) -> bool {
        self.mailboxes.is_empty()
    }
}
