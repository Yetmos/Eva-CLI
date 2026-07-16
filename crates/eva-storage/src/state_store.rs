//! 中文：状态存储契约、进程内实现与 fenced 文件 CAS 实现。
//! State store contracts plus in-memory and fenced filesystem CAS implementations.

use crate::durable_backend::{
    acquire_record_write_lock, atomic_write, DurableWriterGuard, FileSystemDurableBackend,
    WriterGeneration,
};
use crate::DurableBackendLayout;
use eva_core::EvaError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 中文：本模块拥有键值状态版本和比较后写入语义。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "state store contracts, record versions, and durable CAS";

/// 中文：比较后写入使用的单调状态版本；零表示键尚不存在。
/// Monotonic state version used for compare-and-set writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StateVersion(
    /// 中文：每次成功写入递增的原始版本号。
    pub u64,
);

impl StateVersion {
    /// 尚不存在记录时的比较版本。
    pub const ZERO: Self = Self(0);

    /// 返回下一版本；达到 u64 上限后 fail closed，避免版本回绕。
    pub fn checked_next(self) -> Result<Self, EvaError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| EvaError::conflict("state record version exhausted"))
    }
}

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
    /// 中文：提交该版本的 durable writer generation；内存记录为零。
    pub owner_generation: WriterGeneration,
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

/// 使用 durable writer ownership、持久 record version 和原子替换的文件状态存储。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemStateStore {
    root: PathBuf,
    state_dir: PathBuf,
    writer: Option<DurableWriterGuard>,
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
            owner_generation: WriterGeneration::ZERO,
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

const STATE_RECORD_FORMAT: &str = "eva.state-record.v1";

impl FileSystemStateStore {
    /// 创建只读 durable state 视图；mutation 会因缺少 writer ownership 而拒绝。
    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self {
            root: layout.root.clone(),
            state_dir: layout.state_dir.clone(),
            writer: None,
        }
    }

    /// 创建由指定 runtime writer generation fence 的可写 state store。
    pub fn from_runtime_writer(
        layout: &DurableBackendLayout,
        writer: DurableWriterGuard,
    ) -> Result<Self, EvaError> {
        if writer.root() != layout.root {
            return Err(EvaError::conflict(
                "durable state writer belongs to a different backend root",
            )
            .with_context("layout_root", layout.root.display().to_string())
            .with_context("writer_root", writer.root().display().to_string()));
        }
        writer.verify_current()?;
        Ok(Self {
            root: layout.root.clone(),
            state_dir: layout.state_dir.clone(),
            writer: Some(writer),
        })
    }

    /// 从读写 backend 获取 runtime ownership 并构造可写 state store。
    pub fn from_writable_backend(backend: &FileSystemDurableBackend) -> Result<Self, EvaError> {
        Self::from_runtime_writer(backend.layout(), backend.acquire_runtime_writer()?)
    }

    /// 返回 store 所属 durable backend 根。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 读取当前版本；不存在返回 None，损坏/I/O 错误不会降级成缺失。
    pub fn get(&self, key: &str) -> Result<Option<StateRecord>, EvaError> {
        validate_state_key(key)?;
        let path = self.record_path(key);
        let record = read_state_record(&path)?;
        if let Some(record) = &record {
            if record.key != key {
                return Err(EvaError::conflict("durable state key digest collision")
                    .with_context("path", path.display().to_string())
                    .with_context("expected_key", key)
                    .with_context("actual_key", &record.key));
            }
        }
        Ok(record)
    }

    /// 仅当键不存在时创建 version 1 记录。
    pub fn create(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<StateRecord, EvaError> {
        self.compare_and_set(key, StateVersion::ZERO, value)
    }

    /// 持久 CAS：在 writer fencing、进程 mutex 和稳定记录锁内比较并原子替换。
    pub fn compare_and_set(
        &mut self,
        key: impl Into<String>,
        expected: StateVersion,
        value: impl Into<String>,
    ) -> Result<StateRecord, EvaError> {
        let key = key.into();
        let value = value.into();
        validate_state_key(&key)?;
        let writer = self.writer.clone().ok_or_else(|| {
            EvaError::conflict("durable state mutation requires runtime writer ownership")
                .with_context("root", self.root.display().to_string())
        })?;
        let path = self.record_path(&key);
        let lock_path = path.with_extension("state.lock");
        writer.with_write_lock(|generation| {
            let _record_lock = acquire_record_write_lock(&lock_path)?;
            writer.verify_current()?;
            let current = read_state_record(&path)?;
            if let Some(record) = &current {
                if record.key != key {
                    return Err(EvaError::conflict("durable state key digest collision")
                        .with_context("path", path.display().to_string())
                        .with_context("expected_key", &key)
                        .with_context("actual_key", &record.key));
                }
            }
            let actual = current
                .as_ref()
                .map(|record| record.version)
                .unwrap_or(StateVersion::ZERO);
            if actual != expected {
                return Err(EvaError::conflict("state version conflict")
                    .with_context("key", &key)
                    .with_context("expected", expected.0.to_string())
                    .with_context("actual", actual.0.to_string()));
            }
            let record = StateRecord {
                key: key.clone(),
                value,
                version: actual.checked_next()?,
                owner_generation: generation,
            };
            atomic_write(&path, state_record_to_storage(&record).as_bytes()).map_err(|error| {
                EvaError::internal("failed to atomically write durable state record")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            Ok(record)
        })
    }

    fn record_path(&self, key: &str) -> PathBuf {
        let digest = Sha256::digest(key.as_bytes());
        self.state_dir
            .join(format!("{}.state", bytes_to_hex(digest.as_slice())))
    }
}

fn state_record_to_storage(record: &StateRecord) -> String {
    format!(
        "format={STATE_RECORD_FORMAT}\nrecord_version={}\nowner_generation={}\nkey_hex={}\nvalue_hex={}\n",
        record.version.0,
        record.owner_generation.0,
        bytes_to_hex(record.key.as_bytes()),
        bytes_to_hex(record.value.as_bytes())
    )
}

fn read_state_record(path: &Path) -> Result<Option<StateRecord>, EvaError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EvaError::internal("failed to read durable state record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string()))
        }
    };
    parse_state_record(&data)
        .map(Some)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

fn parse_state_record(data: &str) -> Result<StateRecord, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines().filter(|line| !line.is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::conflict("durable state record is invalid"));
        };
        if key.is_empty() || fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(EvaError::conflict(
                "durable state record contains a duplicate or empty field",
            )
            .with_context("field", key));
        }
    }
    if fields.len() != 5 || fields.get("format").map(String::as_str) != Some(STATE_RECORD_FORMAT) {
        return Err(EvaError::conflict(
            "durable state record format or fields are unsupported",
        ));
    }
    let version = parse_positive_u64(
        required_state_field(&fields, "record_version")?,
        "record_version",
    )?;
    let owner_generation = parse_positive_u64(
        required_state_field(&fields, "owner_generation")?,
        "owner_generation",
    )?;
    let key = decode_utf8_hex(required_state_field(&fields, "key_hex")?, "key_hex")?;
    validate_state_key(&key)?;
    let value = decode_utf8_hex(required_state_field(&fields, "value_hex")?, "value_hex")?;
    Ok(StateRecord {
        key,
        value,
        version: StateVersion(version),
        owner_generation: WriterGeneration(owner_generation),
    })
}

fn required_state_field<'a>(
    fields: &'a BTreeMap<String, String>,
    field: &'static str,
) -> Result<&'a str, EvaError> {
    fields.get(field).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("durable state record is incomplete").with_context("field", field)
    })
}

fn parse_positive_u64(value: &str, field: &'static str) -> Result<u64, EvaError> {
    let parsed = value.parse::<u64>().map_err(|_| {
        EvaError::conflict("durable state record field is invalid")
            .with_context("field", field)
            .with_context("value", value)
    })?;
    if parsed == 0 {
        return Err(
            EvaError::conflict("durable state record version fields must be positive")
                .with_context("field", field),
        );
    }
    Ok(parsed)
}

fn validate_state_key(key: &str) -> Result<(), EvaError> {
    if key.is_empty() {
        return Err(EvaError::invalid_argument("state key cannot be empty"));
    }
    Ok(())
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_utf8_hex(value: &str, field: &'static str) -> Result<String, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict("durable state hex field has odd length")
            .with_context("field", field));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = decode_hex_nibble(pair[0]).ok_or_else(|| {
            EvaError::conflict("durable state hex field is invalid").with_context("field", field)
        })?;
        let low = decode_hex_nibble(pair[1]).ok_or_else(|| {
            EvaError::conflict("durable state hex field is invalid").with_context("field", field)
        })?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| {
        EvaError::conflict("durable state hex field is not UTF-8").with_context("field", field)
    })
}

fn decode_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurableBackendOptions, FileSystemDurableBackend};
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    /// 两个 store 读取同一版本后，迟到 CAS 被拒绝且不会覆盖首个提交。
    fn filesystem_state_store_persists_and_rejects_stale_cas() {
        let root = test_root("stale-cas");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut first = FileSystemStateStore::from_writable_backend(&backend).unwrap();
        let mut second = first.clone();
        let created = first.create("agent.root.last_event", "evt-1").unwrap();

        let committed = first
            .compare_and_set("agent.root.last_event", created.version, "evt-2")
            .unwrap();
        let error = second
            .compare_and_set("agent.root.last_event", created.version, "evt-stale")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(committed.version, StateVersion(2));
        assert_eq!(first.get("agent.root.last_event").unwrap(), Some(committed));
    }

    #[test]
    /// Backend 重开后 generation 单调增加，新提交保留 record version 并 stamp 新 owner。
    fn filesystem_state_store_stamps_persistent_writer_generation() {
        let root = test_root("writer-generation");
        let first_record = {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemStateStore::from_writable_backend(&backend).unwrap();
            store.create("runtime.status", "starting").unwrap()
        };
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemStateStore::from_writable_backend(&backend).unwrap();

        let second_record = store
            .compare_and_set("runtime.status", first_record.version, "running")
            .unwrap();

        assert_eq!(first_record.owner_generation, WriterGeneration(1));
        assert_eq!(second_record.owner_generation, WriterGeneration(2));
        assert_eq!(second_record.version, StateVersion(2));
    }

    #[test]
    /// Layout-only store 可读取但不能绕过 runtime ownership 写入。
    fn filesystem_state_layout_only_store_is_read_only() {
        let root = test_root("read-only");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemStateStore::from_durable_layout(backend.layout());

        let error = store.create("runtime.status", "running").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-storage-state-store-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
