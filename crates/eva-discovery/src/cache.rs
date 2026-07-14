//! 发现结果的进程内缓存。
//! In-memory discovery result cache.

use crate::normalizer::{dedupe, DiscoveryCandidate};

/// 本模块的架构职责：缓存发现结果，但绝不由缓存授予运行时句柄。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "cache discovery results without granting runtime handles";

/// 最近一次可用的发现快照及其刷新原因。
///
/// 缓存只保存描述性候选项，不拥有连接、进程或其他可执行句柄，因此读取缓存不会
/// 绕过后续授权边界。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryCache {
    /// 按候选项标识去重后的当前快照。
    snapshot: Vec<DiscoveryCandidate>,
    /// 最近一次完整替换或增量合并的可诊断原因。
    refresh_reason: Option<String>,
}

impl DiscoveryCache {
    /// 创建空的发现缓存。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用完整扫描结果原子地替换进程内快照，并记录刷新原因。
    pub fn replace(&mut self, snapshot: Vec<DiscoveryCandidate>, reason: impl Into<String>) {
        self.snapshot = snapshot;
        self.refresh_reason = Some(reason.into());
    }

    /// 仅替换指定来源的候选项，再对合并后的快照去重。
    ///
    /// 调用方应只在该来源本轮扫描成功时调用本方法；这样单个来源失败时仍可保留
    /// 其他来源以及该失败来源上一轮的有效结果，避免局部故障清空整个发现视图。
    pub fn merge_source(
        &mut self,
        source_id: &str,
        candidates: Vec<DiscoveryCandidate>,
        reason: impl Into<String>,
    ) {
        self.snapshot
            .retain(|candidate| candidate.source.as_str() != source_id);
        self.snapshot.extend(candidates);
        self.snapshot = dedupe(std::mem::take(&mut self.snapshot));
        self.refresh_reason = Some(reason.into());
    }

    /// 借用当前发现快照，不授予其中候选项对应的运行时能力。
    pub fn snapshot(&self) -> &[DiscoveryCandidate] {
        &self.snapshot
    }

    /// 返回最近一次刷新原因；从未刷新时返回 `None`。
    pub fn refresh_reason(&self) -> Option<&str> {
        self.refresh_reason.as_deref()
    }
}
