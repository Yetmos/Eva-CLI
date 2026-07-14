//! 中文：状态存储契约及 V0.4 进程内乐观并发实现。
//! State store contracts and the V0.4 in-memory implementation.

use eva_core::EvaError;
use std::collections::BTreeMap;

/// 中文：本模块拥有键值状态版本和比较后写入语义。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "state store traits and local state ownership";

/// 中文：比较后写入使用的单调状态版本；零表示键尚不存在。
/// Monotonic state version used for compare-and-set writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StateVersion(
    /// 中文：每次成功写入递增的原始版本号。
    pub u64,
);

/// 中文：包含键、值和读取时版本的完整状态记录。
/// Stored state value with a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRecord {
    /// 中文：状态记录的稳定查找键。
    pub key: String,
    /// 中文：由上层解释的状态文本。
    pub value: String,
    /// 中文：本次值对应的单调版本。
    pub version: StateVersion,
}

/// 中文：SQLite 后端实现前运行时所需的最小状态存储行为。
/// Minimal state store behavior required before the SQLite backend exists.
pub trait StateStore {
    /// 中文：读取键的当前记录快照；不存在时返回 `None`。
    fn get(&self, key: &str) -> Option<StateRecord>;
    /// 中文：无条件写入值并把现有版本递增一。
    fn put(&mut self, key: impl Into<String>, value: impl Into<String>) -> StateRecord;
    /// 中文：仅在当前版本等于调用方预期时写入，否则返回包含实际版本的冲突错误。
    fn compare_and_set(
        &mut self,
        key: &str,
        expected: StateVersion,
        value: impl Into<String>,
    ) -> Result<StateRecord, EvaError>;
}

/// 中文：测试和 V0.4 运行路径使用的内存状态存储。
/// In-memory state store for tests and the V0.4 runtime path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryStateStore {
    /// 中文：按键有序保存的最新状态记录。
    values: BTreeMap<String, StateRecord>,
}

impl InMemoryStateStore {
    /// 中文：创建空状态存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：返回当前不同状态键的数量。
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// 中文：判断存储中是否没有状态键。
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl StateStore for InMemoryStateStore {
    /// 中文：克隆并返回当前记录，使调用方不能绕过版本语义修改内部状态。
    fn get(&self, key: &str) -> Option<StateRecord> {
        self.values.get(key).cloned()
    }

    /// 中文：写入新值；已有键版本递增，新键从版本一开始。
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

    /// 中文：执行乐观并发写入。
    ///
    /// 不存在键的当前版本按零处理，因此调用方可用 `StateVersion(0)` 原子创建；版本不符
    /// 时保持存储不变并返回期望值与实际值，避免迟到写入覆盖较新的状态。
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
    /// 中文：验证同一键的无条件写入会单调递增版本并替换值。
    fn put_versions_state() {
        let mut store = InMemoryStateStore::new();

        let first = store.put("agent.root.last_event", "evt-1");
        let second = store.put("agent.root.last_event", "evt-2");

        assert_eq!(first.version, StateVersion(1));
        assert_eq!(second.version, StateVersion(2));
        assert_eq!(store.get("agent.root.last_event").unwrap().value, "evt-2");
    }

    #[test]
    /// 中文：验证过期版本的比较后写入被拒绝且不覆盖当前值。
    fn compare_and_set_rejects_stale_version() {
        let mut store = InMemoryStateStore::new();
        store.put("key", "old");

        let error = store
            .compare_and_set("key", StateVersion(0), "new")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }
}
