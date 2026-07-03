//! Agent-private and global memory service contracts.

use eva_core::{AgentId, EvaError, RequestId};
use eva_storage::StateVersion;
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent and global memory service boundaries";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryVisibility {
    Private,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryRetention {
    Session,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    pub key: String,
    pub value: String,
    pub visibility: MemoryVisibility,
    pub owner_agent: Option<AgentId>,
    pub retention: MemoryRetention,
    pub version: StateVersion,
    pub request_id: Option<RequestId>,
    pub audit_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWrite {
    pub key: String,
    pub value: String,
    pub visibility: MemoryVisibility,
    pub owner_agent: Option<AgentId>,
    pub retention: MemoryRetention,
    pub request_id: Option<RequestId>,
    pub audit_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReadRequest {
    pub requester: AgentId,
    pub owner_agent: Option<AgentId>,
    pub visibility: MemoryVisibility,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemorySnapshot {
    pub private: Vec<MemoryRecord>,
    pub global: Vec<MemoryRecord>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryMemoryService {
    records: BTreeMap<MemoryIndexKey, MemoryRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MemoryIndexKey {
    visibility: MemoryVisibility,
    owner: Option<AgentId>,
    key: String,
}

impl MemoryVisibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Global => "global",
        }
    }
}

impl MemoryRetention {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Persistent => "persistent",
        }
    }
}

impl MemoryWrite {
    pub fn private(owner_agent: AgentId, key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            visibility: MemoryVisibility::Private,
            owner_agent: Some(owner_agent),
            retention: MemoryRetention::Session,
            request_id: None,
            audit_reason: "agent private memory write".to_owned(),
        }
    }

    pub fn global(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            visibility: MemoryVisibility::Global,
            owner_agent: None,
            retention: MemoryRetention::Persistent,
            request_id: None,
            audit_reason: "global memory write".to_owned(),
        }
    }

    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.audit_reason = reason.into();
        self
    }
}

impl MemoryReadRequest {
    pub fn private(requester: AgentId, key: impl Into<String>) -> Self {
        Self {
            owner_agent: Some(requester.clone()),
            requester,
            visibility: MemoryVisibility::Private,
            key: key.into(),
        }
    }

    pub fn global(requester: AgentId, key: impl Into<String>) -> Self {
        Self {
            requester,
            owner_agent: None,
            visibility: MemoryVisibility::Global,
            key: key.into(),
        }
    }

    pub fn with_owner_agent(mut self, owner_agent: AgentId) -> Self {
        self.owner_agent = Some(owner_agent);
        self
    }
}

impl InMemoryMemoryService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&mut self, write: MemoryWrite) -> Result<MemoryRecord, EvaError> {
        validate_memory_key(&write.key)?;
        if write.value.trim().is_empty() {
            return Err(EvaError::invalid_argument("memory value cannot be empty")
                .with_context("key", write.key));
        }
        if write.visibility == MemoryVisibility::Private && write.owner_agent.is_none() {
            return Err(EvaError::invalid_argument(
                "private memory requires an owner agent",
            ));
        }
        if write.visibility == MemoryVisibility::Global && write.owner_agent.is_some() {
            return Err(EvaError::invalid_argument(
                "global memory cannot carry a private owner agent",
            ));
        }

        let index = MemoryIndexKey::new(write.visibility, write.owner_agent.clone(), &write.key);
        let version = self
            .records
            .get(&index)
            .map(|record| StateVersion(record.version.0 + 1))
            .unwrap_or(StateVersion(1));
        let record = MemoryRecord {
            key: write.key,
            value: write.value,
            visibility: write.visibility,
            owner_agent: write.owner_agent,
            retention: write.retention,
            version,
            request_id: write.request_id,
            audit_reason: write.audit_reason,
        };
        self.records.insert(index, record.clone());
        Ok(record)
    }

    pub fn read(&self, request: &MemoryReadRequest) -> Result<Option<MemoryRecord>, EvaError> {
        validate_memory_key(&request.key)?;
        self.authorize_read(request)?;
        let index = MemoryIndexKey::new(
            request.visibility,
            request.owner_agent.clone(),
            request.key.as_str(),
        );
        Ok(self.records.get(&index).cloned())
    }

    pub fn list_private(&self, requester: &AgentId) -> Vec<MemoryRecord> {
        let mut records = self
            .records
            .values()
            .filter(|record| {
                record.visibility == MemoryVisibility::Private
                    && record.owner_agent.as_ref() == Some(requester)
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.key.cmp(&right.key));
        records
    }

    pub fn list_global(&self) -> Vec<MemoryRecord> {
        let mut records = self
            .records
            .values()
            .filter(|record| record.visibility == MemoryVisibility::Global)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.key.cmp(&right.key));
        records
    }

    pub fn snapshot_for_agent(
        &self,
        requester: &AgentId,
        private_limit: usize,
        global_limit: usize,
    ) -> MemorySnapshot {
        MemorySnapshot {
            private: take_records(self.list_private(requester), private_limit),
            global: take_records(self.list_global(), global_limit),
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn authorize_read(&self, request: &MemoryReadRequest) -> Result<(), EvaError> {
        if request.visibility == MemoryVisibility::Private
            && request.owner_agent.as_ref() != Some(&request.requester)
        {
            return Err(
                EvaError::permission_denied("private memory is isolated by agent_id")
                    .with_context("requester", request.requester.as_str())
                    .with_context(
                        "owner_agent",
                        request
                            .owner_agent
                            .as_ref()
                            .map(|agent| agent.as_str())
                            .unwrap_or(""),
                    ),
            );
        }
        Ok(())
    }
}

impl MemoryIndexKey {
    fn new(visibility: MemoryVisibility, owner: Option<AgentId>, key: &str) -> Self {
        Self {
            visibility,
            owner,
            key: key.to_owned(),
        }
    }
}

fn validate_memory_key(key: &str) -> Result<(), EvaError> {
    if key.trim().is_empty() {
        return Err(EvaError::invalid_argument("memory key cannot be empty"));
    }
    if key.trim() != key {
        return Err(EvaError::invalid_argument(
            "memory key cannot contain leading or trailing whitespace",
        ));
    }
    if key.len() > 128 {
        return Err(EvaError::invalid_argument("memory key is too long"));
    }
    Ok(())
}

fn take_records(records: Vec<MemoryRecord>, limit: usize) -> Vec<MemoryRecord> {
    records.into_iter().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    #[test]
    fn private_memory_is_isolated_by_agent_id() {
        let mut service = InMemoryMemoryService::new();
        let owner = agent("root-agent");
        let other = agent("agent-a");
        service
            .write(MemoryWrite::private(owner.clone(), "goal", "ship-v1.2"))
            .unwrap();

        let allowed = service
            .read(&MemoryReadRequest::private(owner.clone(), "goal"))
            .unwrap();
        assert_eq!(allowed.unwrap().value, "ship-v1.2");

        let denied = service
            .read(&MemoryReadRequest::private(other, "goal").with_owner_agent(owner))
            .unwrap_err();
        assert_eq!(denied.kind(), eva_core::ErrorKind::PermissionDenied);
    }

    #[test]
    fn global_memory_is_visible_to_any_agent() {
        let mut service = InMemoryMemoryService::new();
        service
            .write(MemoryWrite::global("release", "v1.2"))
            .unwrap();

        let record = service
            .read(&MemoryReadRequest::global(agent("agent-a"), "release"))
            .unwrap()
            .unwrap();

        assert_eq!(record.visibility, MemoryVisibility::Global);
        assert_eq!(record.value, "v1.2");
    }

    #[test]
    fn writes_increment_versions() {
        let mut service = InMemoryMemoryService::new();
        let owner = agent("root-agent");
        let first = service
            .write(MemoryWrite::private(owner.clone(), "topic", "first"))
            .unwrap();
        let second = service
            .write(MemoryWrite::private(owner, "topic", "second"))
            .unwrap();

        assert_eq!(first.version, StateVersion(1));
        assert_eq!(second.version, StateVersion(2));
    }
}
