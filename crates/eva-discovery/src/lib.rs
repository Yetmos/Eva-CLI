//! 可信来源发现与健康探测的边界。
//! Discovery boundary for trusted sources and health probing.

/// 保存最近一次发现结果的内存缓存。
pub mod cache;
/// 将候选项映射为只读健康状态。
pub mod health;
/// 统一候选项标识、类型与信任级别。
pub mod normalizer;
/// 编排多个发现来源并生成逐来源报告。
pub mod scanner;
/// 对外提供发现、缓存和健康查询服务。
pub mod service;
/// 内置的可信发现来源适配器。
pub mod sources;

pub use cache::DiscoveryCache;
pub use health::{DiscoveryHealth, DiscoveryHealthStatus};
pub use normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
pub use scanner::{
    DiscoveryScanContext, DiscoveryScanReport, DiscoverySource, DiscoverySourceReport,
    ProjectDiscoverySource,
};
pub use service::DiscoveryService;
