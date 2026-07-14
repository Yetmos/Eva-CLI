//! Agent 私有与全局 Memory 的可见性、TTL 和版本契约。
//! Agent-private and global memory service contracts.

use crate::observability::{record_memory_observation, MemoryObservation, MemoryOperation};
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::{AuditSink, MetricSink, TraceFields};
use eva_storage::StateVersion;
use std::collections::BTreeMap;

/// 本模块的架构职责：隔离 Agent 私有记忆，并提供受版本、保留和 TTL 约束的全局记忆。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent and global memory service boundaries";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Memory 记录的读取可见性。
pub enum MemoryVisibility {
    /// 仅 owner Agent 可读取。
    Private,
    /// 任意 Agent 均可读取。
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Memory 记录的生命周期保留类别。
pub enum MemoryRetention {
    /// 仅当前会话需要保留。
    Session,
    /// 应由持久存储跨会话保存。
    Persistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
/// Memory 值在磁盘格式中的压缩方式。
pub enum MemoryCompression {
    /// 原样存储 UTF-8 值。
    #[default]
    None,
    /// 使用简单游程编码存储重复字符。
    RunLength,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 已写入、带版本和生命周期元数据的 Memory 记录。
pub struct MemoryRecord {
    /// 可见性分区内的稳定键。
    pub key: String,
    /// 解压后的逻辑值。
    pub value: String,
    /// 私有或全局可见性。
    pub visibility: MemoryVisibility,
    /// 私有记录所有者；全局记录必须为 `None`。
    pub owner_agent: Option<AgentId>,
    /// 会话或持久保留类别。
    pub retention: MemoryRetention,
    /// 同一索引槽位内单调递增的状态版本。
    pub version: StateVersion,
    /// 可选来源请求标识。
    pub request_id: Option<RequestId>,
    /// 写入原因审计文本。
    pub audit_reason: String,
    /// 记录创建时间，单位为 Unix epoch 毫秒。
    pub created_at_ms: u128,
    /// 可选到期时间；达到该时间即不可见。
    pub expires_at_ms: Option<u128>,
    /// 持久化时采用的压缩方式。
    pub compression: MemoryCompression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 创建或覆盖 Memory 记录的输入。
pub struct MemoryWrite {
    /// 目标索引键。
    pub key: String,
    /// 待保存逻辑值。
    pub value: String,
    /// 目标可见性。
    pub visibility: MemoryVisibility,
    /// 私有写入的所有者。
    pub owner_agent: Option<AgentId>,
    /// 保留类别。
    pub retention: MemoryRetention,
    /// 可选来源请求标识。
    pub request_id: Option<RequestId>,
    /// 写入原因。
    pub audit_reason: String,
    /// 创建时间毫秒值。
    pub created_at_ms: u128,
    /// 可选到期时间毫秒值。
    pub expires_at_ms: Option<u128>,
    /// 请求的磁盘压缩方式。
    pub compression: MemoryCompression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 包含请求者与目标 owner 的 Memory 读取请求。
pub struct MemoryReadRequest {
    /// 发起读取的 Agent。
    pub requester: AgentId,
    /// 可选请求追踪标识。
    pub request_id: Option<RequestId>,
    /// 私有记录的目标 owner。
    pub owner_agent: Option<AgentId>,
    /// 目标可见性分区。
    pub visibility: MemoryVisibility,
    /// 目标 Memory 键。
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// 提供给某 Agent 的有界私有与全局 Memory 快照。
pub struct MemorySnapshot {
    /// 仅属于请求 Agent 的私有记录。
    pub private: Vec<MemoryRecord>,
    /// 对所有 Agent 可见的全局记录。
    pub global: Vec<MemoryRecord>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
/// 按复合索引维护最新版本记录的进程内 Memory 服务。
pub struct InMemoryMemoryService {
    /// 可见性、owner 和逻辑键到最新记录的确定性映射。
    records: BTreeMap<MemoryIndexKey, MemoryRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
/// 隔离同名私有、全局及不同 Agent 私有记录的复合索引键。
struct MemoryIndexKey {
    /// 可见性分区。
    visibility: MemoryVisibility,
    /// 私有 owner；全局记录为空。
    owner: Option<AgentId>,
    /// 分区内逻辑键。
    key: String,
}

impl MemoryVisibility {
    /// 返回稳定磁盘拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Global => "global",
        }
    }

    /// 解析受支持可见性，未知值失败关闭。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "private" => Ok(Self::Private),
            "global" => Ok(Self::Global),
            _ => Err(EvaError::invalid_argument("unknown memory visibility")
                .with_context("visibility", value)),
        }
    }
}

impl MemoryRetention {
    /// 返回稳定磁盘拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Persistent => "persistent",
        }
    }

    /// 解析受支持保留类别。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "session" => Ok(Self::Session),
            "persistent" => Ok(Self::Persistent),
            _ => Err(EvaError::invalid_argument("unknown memory retention")
                .with_context("retention", value)),
        }
    }
}

impl MemoryCompression {
    /// 返回稳定磁盘拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RunLength => "run_length",
        }
    }

    /// 解析受支持压缩方式。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "none" => Ok(Self::None),
            "run_length" => Ok(Self::RunLength),
            _ => Err(EvaError::invalid_argument("unknown memory compression")
                .with_context("compression", value)),
        }
    }
}

impl MemoryWrite {
    /// 创建默认会话保留、无 TTL 的私有写入。
    pub fn private(owner_agent: AgentId, key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            visibility: MemoryVisibility::Private,
            owner_agent: Some(owner_agent),
            retention: MemoryRetention::Session,
            request_id: None,
            audit_reason: "agent private memory write".to_owned(),
            created_at_ms: 0,
            expires_at_ms: None,
            compression: MemoryCompression::None,
        }
    }

    /// 创建默认持久保留、无 owner 的全局写入。
    pub fn global(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            visibility: MemoryVisibility::Global,
            owner_agent: None,
            retention: MemoryRetention::Persistent,
            request_id: None,
            audit_reason: "global memory write".to_owned(),
            created_at_ms: 0,
            expires_at_ms: None,
            compression: MemoryCompression::None,
        }
    }

    /// 关联来源请求标识。
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// 覆盖写入审计原因。
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.audit_reason = reason.into();
        self
    }

    /// 覆盖记录保留类别。
    pub fn with_retention(mut self, retention: MemoryRetention) -> Self {
        self.retention = retention;
        self
    }

    /// 设置显式创建时间。
    pub fn with_created_at_ms(mut self, created_at_ms: u128) -> Self {
        self.created_at_ms = created_at_ms;
        self
    }

    /// 设置创建时间和 TTL，并用饱和加法避免溢出导致提前过期。
    pub fn with_ttl_ms(mut self, created_at_ms: u128, ttl_ms: u128) -> Self {
        self.created_at_ms = created_at_ms;
        self.expires_at_ms = Some(created_at_ms.saturating_add(ttl_ms));
        self
    }

    /// 设置磁盘持久化压缩方式；不改变内存中的逻辑值。
    pub fn with_compression(mut self, compression: MemoryCompression) -> Self {
        self.compression = compression;
        self
    }
}

impl MemoryReadRequest {
    /// 创建读取请求者自身私有记录的请求。
    pub fn private(requester: AgentId, key: impl Into<String>) -> Self {
        Self {
            owner_agent: Some(requester.clone()),
            request_id: None,
            requester,
            visibility: MemoryVisibility::Private,
            key: key.into(),
        }
    }

    /// 创建读取全局记录的请求。
    pub fn global(requester: AgentId, key: impl Into<String>) -> Self {
        Self {
            requester,
            request_id: None,
            owner_agent: None,
            visibility: MemoryVisibility::Global,
            key: key.into(),
        }
    }

    /// 显式指定私有目标 owner；授权检查仍要求其等于 requester。
    pub fn with_owner_agent(mut self, owner_agent: AgentId) -> Self {
        self.owner_agent = Some(owner_agent);
        self
    }

    /// 关联请求追踪标识。
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }
}

impl InMemoryMemoryService {
    /// 创建空 Memory 服务。
    pub fn new() -> Self {
        Self::default()
    }

    /// 验证可见性/owner 不变量并写入同一复合索引的下一版本。
    ///
    /// 私有写入必须有 owner，全局写入禁止 owner。版本只在完全相同的 visibility、owner、
    /// key 槽位递增，因此不同 Agent 的同名私有记录互不影响；校验失败时不修改映射。
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
            created_at_ms: write.created_at_ms,
            expires_at_ms: write.expires_at_ms,
            compression: write.compression,
        };
        self.records.insert(index, record.clone());
        Ok(record)
    }

    /// 完成写入后记录审计和指标；观察写入失败会返回错误但不撤销已写 Memory。
    pub fn write_observed<S>(
        &mut self,
        write: MemoryWrite,
        sink: &mut S,
        trace: &TraceFields,
    ) -> Result<MemoryRecord, EvaError>
    where
        S: AuditSink + MetricSink,
    {
        let request_id = write.request_id.clone();
        let agent_id = write.owner_agent.clone();
        let visibility = write.visibility;
        let key = write.key.clone();
        let record = self.write(write)?;
        let mut observation = MemoryObservation::new(MemoryOperation::Write, trace.clone())
            .with_visibility(visibility)
            .with_key(key)
            .with_item_count(1);
        if let Some(request_id) = request_id {
            observation = observation.with_request_id(request_id);
        }
        if let Some(agent_id) = agent_id {
            observation = observation.with_agent_id(agent_id);
        }
        record_memory_observation(sink, observation)?;
        Ok(record)
    }

    /// 以兼容时间零读取记录。
    pub fn read(&self, request: &MemoryReadRequest) -> Result<Option<MemoryRecord>, EvaError> {
        self.read_at(request, 0)
    }

    /// 授权后读取记录，并将到期记录视为不存在。
    ///
    /// TTL 过滤是只读操作，不在此删除持久记录；物理清理由显式 compaction 完成，避免
    /// 普通读取产生隐藏写副作用。
    pub fn read_at(
        &self,
        request: &MemoryReadRequest,
        now_ms: u128,
    ) -> Result<Option<MemoryRecord>, EvaError> {
        validate_memory_key(&request.key)?;
        self.authorize_read(request)?;
        let index = MemoryIndexKey::new(
            request.visibility,
            request.owner_agent.clone(),
            request.key.as_str(),
        );
        Ok(self
            .records
            .get(&index)
            .filter(|record| !record.is_expired_at(now_ms))
            .cloned())
    }

    /// 读取后记录审计和指标；观察失败不改变读取结果或存储。
    pub fn read_observed<S>(
        &self,
        request: &MemoryReadRequest,
        now_ms: u128,
        sink: &mut S,
        trace: &TraceFields,
    ) -> Result<Option<MemoryRecord>, EvaError>
    where
        S: AuditSink + MetricSink,
    {
        let record = self.read_at(request, now_ms)?;
        let mut observation = MemoryObservation::new(MemoryOperation::Read, trace.clone())
            .with_agent_id(request.requester.clone())
            .with_visibility(request.visibility)
            .with_key(request.key.clone())
            .with_item_count(usize::from(record.is_some()));
        if let Some(request_id) = &request.request_id {
            observation = observation.with_request_id(request_id.clone());
        }
        record_memory_observation(sink, observation)?;
        Ok(record)
    }

    /// 以时间零列出请求 Agent 的全部私有记录。
    pub fn list_private(&self, requester: &AgentId) -> Vec<MemoryRecord> {
        self.list_private_at(requester, 0)
    }

    /// 按键排序列出请求 Agent 尚未过期的私有记录。
    pub fn list_private_at(&self, requester: &AgentId, now_ms: u128) -> Vec<MemoryRecord> {
        let mut records = self
            .records
            .values()
            .filter(|record| {
                record.visibility == MemoryVisibility::Private
                    && record.owner_agent.as_ref() == Some(requester)
                    && !record.is_expired_at(now_ms)
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.key.cmp(&right.key));
        records
    }

    /// 以时间零列出全部全局记录。
    pub fn list_global(&self) -> Vec<MemoryRecord> {
        self.list_global_at(0)
    }

    /// 按键排序列出尚未过期的全局记录。
    pub fn list_global_at(&self, now_ms: u128) -> Vec<MemoryRecord> {
        let mut records = self
            .records
            .values()
            .filter(|record| {
                record.visibility == MemoryVisibility::Global && !record.is_expired_at(now_ms)
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.key.cmp(&right.key));
        records
    }

    /// 以时间零创建有界 Agent 快照。
    pub fn snapshot_for_agent(
        &self,
        requester: &AgentId,
        private_limit: usize,
        global_limit: usize,
    ) -> MemorySnapshot {
        self.snapshot_for_agent_at(requester, private_limit, global_limit, 0)
    }

    /// 分别限制私有/全局条目数并排除到期记录。
    pub fn snapshot_for_agent_at(
        &self,
        requester: &AgentId,
        private_limit: usize,
        global_limit: usize,
        now_ms: u128,
    ) -> MemorySnapshot {
        MemorySnapshot {
            private: take_records(self.list_private_at(requester, now_ms), private_limit),
            global: take_records(self.list_global_at(now_ms), global_limit),
        }
    }

    /// 返回包含到期记录在内的当前索引槽位数。
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// 判断索引是否完全为空。
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// 强制私有读取的 requester 与 owner 完全一致。
    ///
    /// 即使调用者手工覆盖 `owner_agent`，也无法跨 Agent 读取；全局读取不使用 owner。
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

    /// 插入从持久存储恢复的完整记录并保留磁盘版本。
    pub fn insert_record(&mut self, record: MemoryRecord) -> Result<(), EvaError> {
        validate_memory_key(&record.key)?;
        let index = MemoryIndexKey::new(
            record.visibility,
            record.owner_agent.clone(),
            record.key.as_str(),
        );
        self.records.insert(index, record);
        Ok(())
    }
}

impl MemoryRecord {
    /// 判断到期时间是否已到；无 TTL 的记录永不过期。
    pub fn is_expired_at(&self, now_ms: u128) -> bool {
        self.expires_at_ms
            .map(|expires_at_ms| expires_at_ms <= now_ms)
            .unwrap_or(false)
    }
}

impl MemoryIndexKey {
    /// 从可见性、owner 和逻辑键构造复合索引。
    fn new(visibility: MemoryVisibility, owner: Option<AgentId>, key: &str) -> Self {
        Self {
            visibility,
            owner,
            key: key.to_owned(),
        }
    }
}

/// 校验 Memory 键非空、已裁剪且不超过 128 字节。
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

/// 从已排序记录中取至多指定数量；零限制返回空集合。
fn take_records(records: Vec<MemoryRecord>, limit: usize) -> Vec<MemoryRecord> {
    records.into_iter().take(limit).collect()
}

#[cfg(test)]
/// 可见性隔离、版本、TTL 和可观测性测试。
mod tests {
    use super::*;
    use eva_observability::{
        AuditEvent, AuditSink, InMemoryAuditSink, InMemoryMetricSink, MetricPoint, MetricSink,
    };

    #[derive(Debug, Default)]
    /// 同时收集测试审计事件和指标点的 Sink。
    struct TestSink {
        /// 内存审计 Sink。
        audit: InMemoryAuditSink,
        /// 内存指标 Sink。
        metrics: InMemoryMetricSink,
    }

    impl AuditSink for TestSink {
        /// 转发审计事件。
        fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
            self.audit.record(event)
        }
    }

    impl MetricSink for TestSink {
        /// 转发指标点。
        fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
            self.metrics.record(point)
        }
    }

    /// 解析测试 Agent 标识。
    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    #[test]
    /// 验证私有记录不能被其他 Agent 读取。
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
    /// 验证任意 Agent 均可读取全局记录。
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
    /// 验证同一复合索引的覆盖写递增版本。
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

    #[test]
    /// 验证快照排除已到期记录而保留未到期记录。
    fn expired_memory_is_omitted_from_snapshots() {
        let mut service = InMemoryMemoryService::new();
        let owner = agent("root-agent");
        service
            .write(MemoryWrite::private(owner.clone(), "fresh", "keep").with_ttl_ms(100, 100))
            .unwrap();
        service
            .write(MemoryWrite::private(owner.clone(), "expired", "drop").with_ttl_ms(100, 1))
            .unwrap();

        let snapshot = service.snapshot_for_agent_at(&owner, 8, 8, 150);

        assert_eq!(snapshot.private.len(), 1);
        assert_eq!(snapshot.private[0].key, "fresh");
    }

    #[test]
    /// 验证带观察的读写记录请求、Agent、审计和指标。
    fn memory_write_and_read_observed_record_request_agent_audit_and_metrics() {
        let mut service = InMemoryMemoryService::new();
        let mut sink = TestSink::default();
        let owner = agent("root-agent");
        let request_id = RequestId::parse("req-memory-observed").unwrap();

        service
            .write_observed(
                MemoryWrite::private(owner.clone(), "goal", "ship")
                    .with_request_id(request_id.clone()),
                &mut sink,
                &TraceFields::default(),
            )
            .unwrap();
        let record = service
            .read_observed(
                &MemoryReadRequest::private(owner.clone(), "goal")
                    .with_request_id(request_id.clone()),
                0,
                &mut sink,
                &TraceFields::default(),
            )
            .unwrap()
            .unwrap();

        assert_eq!(record.value, "ship");
        assert_eq!(sink.audit.events.len(), 2);
        assert_eq!(sink.audit.events[0].action.as_str(), "memory.write");
        assert_eq!(sink.audit.events[1].action.as_str(), "memory.read");
        assert_eq!(
            sink.audit.events[0].trace.request_id.as_ref(),
            Some(&request_id)
        );
        assert_eq!(sink.audit.events[0].trace.agent_id.as_ref(), Some(&owner));
        assert_eq!(sink.metrics.points.len(), 4);
    }
}
