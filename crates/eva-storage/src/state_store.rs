//! State store contracts and the V0.4 in-memory implementation.

use eva_core::EvaError;
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "state store traits and local state ownership";

/// Monotonic state version used for compare-and-set writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StateVersion(pub u64);

/// Stored state value with a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRecord {
    pub key: String,
    pub value: String,
    pub version: StateVersion,
}

/// Minimal state store behavior required before the SQLite backend exists.
pub trait StateStore {
    fn get(&self, key: &str) -> Option<StateRecord>;
    fn put(&mut self, key: impl Into<String>, value: impl Into<String>) -> StateRecord;
    fn compare_and_set(
        &mut self,
        key: &str,
        expected: StateVersion,
        value: impl Into<String>,
    ) -> Result<StateRecord, EvaError>;
}

/// In-memory state store for tests and the V0.4 runtime path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryStateStore {
    values: BTreeMap<String, StateRecord>,
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl StateStore for InMemoryStateStore {
    fn get(&self, key: &str) -> Option<StateRecord> {
        self.values.get(key).cloned()
    }

    fn put(&mut self, key: impl Into<String>, value: impl Into<String>) -> StateRecord {
        let key = key.into();
        let version = self
            .values
            .get(&key)
            .map(|record| StateVersion(record.version.0 + 1))
            .unwrap_or(StateVersion(1));
        let record = StateRecord {
            key: key.clone(),
            value: value.into(),
            version,
        };
        self.values.insert(key, record.clone());
        record
    }

    fn compare_and_set(
        &mut self,
        key: &str,
        expected: StateVersion,
        value: impl Into<String>,
    ) -> Result<StateRecord, EvaError> {
        let current = self
            .values
            .get(key)
            .map(|record| record.version)
            .unwrap_or_default();
        if current != expected {
            return Err(EvaError::conflict("state version conflict")
                .with_context("key", key)
                .with_context("expected", expected.0.to_string())
                .with_context("actual", current.0.to_string()));
        }
        Ok(self.put(key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_versions_state() {
        let mut store = InMemoryStateStore::new();

        let first = store.put("agent.root.last_event", "evt-1");
        let second = store.put("agent.root.last_event", "evt-2");

        assert_eq!(first.version, StateVersion(1));
        assert_eq!(second.version, StateVersion(2));
        assert_eq!(store.get("agent.root.last_event").unwrap().value, "evt-2");
    }

    #[test]
    fn compare_and_set_rejects_stale_version() {
        let mut store = InMemoryStateStore::new();
        store.put("key", "old");

        let error = store
            .compare_and_set("key", StateVersion(0), "new")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }
}
