//! Provider process/session table contracts.

use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider process/session table snapshots";

/// Queryable provider execution state shared by supervisors and future recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProcessSnapshot {
    pub session_id: String,
    pub provider_process_id: String,
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub capability: CapabilityName,
    pub transport: String,
    pub manifest_digest: String,
    pub start_command: String,
    pub health: String,
    pub restart_policy: String,
    pub active: bool,
    pub last_error: Option<String>,
    pub started_at_ms: u128,
    pub updated_at_ms: u128,
    pub audit: Vec<String>,
}

/// Provider process/session table behavior required by V1.13 supervision.
pub trait ProviderProcessTable {
    fn upsert(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError>;
    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError>;
    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;
}

/// In-memory process table used by the first provider supervisor baseline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryProviderProcessTable {
    snapshots: BTreeMap<String, ProviderProcessSnapshot>,
}

impl ProviderProcessSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn running(
        session_id: impl Into<String>,
        provider_process_id: impl Into<String>,
        request_id: RequestId,
        adapter_id: AdapterId,
        capability: CapabilityName,
        transport: impl Into<String>,
        manifest_digest: impl Into<String>,
        start_command: impl Into<String>,
        restart_policy: impl Into<String>,
    ) -> Self {
        let now = now_ms();
        let session_id = session_id.into();
        let provider_process_id = provider_process_id.into();
        Self {
            audit: vec![
                "provider.supervisor.acquired".to_owned(),
                format!("provider.session:{session_id}"),
                format!("provider.process:{provider_process_id}"),
            ],
            session_id,
            provider_process_id,
            request_id,
            adapter_id,
            capability,
            transport: transport.into(),
            manifest_digest: manifest_digest.into(),
            start_command: start_command.into(),
            health: "running".to_owned(),
            restart_policy: restart_policy.into(),
            active: true,
            last_error: None,
            started_at_ms: now,
            updated_at_ms: now,
        }
    }

    pub fn release(
        &mut self,
        health: impl Into<String>,
        last_error: Option<String>,
    ) -> Result<(), EvaError> {
        let health = health.into();
        if health.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process health cannot be empty",
            ));
        }
        self.active = false;
        self.health = health.clone();
        self.last_error = last_error;
        self.updated_at_ms = now_ms();
        self.audit.push("provider.slot:released".to_owned());
        self.audit.push(format!("provider.health:{health}"));
        if self.last_error.is_some() {
            self.audit.push("provider.supervisor.failed".to_owned());
        } else {
            self.audit.push("provider.supervisor.completed".to_owned());
        }
        Ok(())
    }
}

impl InMemoryProviderProcessTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|snapshot| snapshot.active && &snapshot.adapter_id == adapter_id)
            .collect())
    }
}

impl ProviderProcessTable for InMemoryProviderProcessTable {
    fn upsert(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError> {
        if snapshot.session_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process session id cannot be empty",
            ));
        }
        self.snapshots.insert(snapshot.session_id.clone(), snapshot);
        Ok(())
    }

    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        self.snapshots.get(session_id).cloned().ok_or_else(|| {
            EvaError::not_found("provider process session does not exist")
                .with_context("session_id", session_id)
        })
    }

    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        Ok(self.snapshots.values().cloned().collect())
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(session: &str) -> ProviderProcessSnapshot {
        ProviderProcessSnapshot::running(
            session,
            format!("proc-{session}"),
            RequestId::parse("req-provider-table").unwrap(),
            AdapterId::parse("stdio-test").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "fnv64:0123456789abcdef",
            "stdio-test --run",
            "none",
        )
    }

    #[test]
    fn process_table_upserts_and_lists_active_sessions() {
        let mut table = InMemoryProviderProcessTable::new();
        let adapter_id = AdapterId::parse("stdio-test").unwrap();

        table.upsert(snapshot("session-1")).unwrap();

        let sessions = table.list().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(table.active_for_adapter(&adapter_id).unwrap().len(), 1);
    }

    #[test]
    fn process_table_release_records_last_error() {
        let mut table = InMemoryProviderProcessTable::new();
        let mut snapshot = snapshot("session-2");
        snapshot
            .release("failed", Some("provider exited before ready".to_owned()))
            .unwrap();

        table.upsert(snapshot).unwrap();
        let stored = table.read("session-2").unwrap();

        assert!(!stored.active);
        assert_eq!(stored.health, "failed");
        assert_eq!(
            stored.last_error.as_deref(),
            Some("provider exited before ready")
        );
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.slot:released"));
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.supervisor.failed"));
    }
}
