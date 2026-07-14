//! 中文：权限数据契约、收窄规则和越权差异计算。
//! Permission data types and narrowing rules.

use eva_core::{AdapterId, CapabilityName};
use std::collections::BTreeSet;

/// 中文：请求或运行时使用的一组权限上界。
///
/// 中文：布尔字段是显式放行开关。Capability 与 Adapter 集合中，`None` 表示该维度
/// 不受限制，`Some(empty)` 表示明确不允许任何值；多层合并始终只取交集。
/// Boolean fields are explicit allow switches. Capability and adapter sets use
/// `None` as "not constrained by this dimension" and `Some(empty)` as
/// "explicitly allow no values". Narrowing always intersects permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSet {
    /// 中文：是否允许访问网络。
    pub network: bool,
    /// 中文：是否允许执行 shell 命令。
    pub shell: bool,
    /// 中文：是否允许读取工作区文件。
    pub read_workspace: bool,
    /// 中文：是否允许修改工作区文件。
    pub write_workspace: bool,
    /// 中文：单次操作的可选最大超时毫秒数；多层合并取最小上限。
    pub max_timeout_ms: Option<u64>,
    /// 中文：可调用的 capability 白名单；`None` 表示此维度不施加约束。
    pub capabilities: Option<BTreeSet<CapabilityName>>,
    /// 中文：可调用的 Adapter 白名单；`None` 表示此维度不施加约束。
    pub adapters: Option<BTreeSet<AdapterId>>,
}

/// 中文：请求相对权限上界发生扩张的稳定字段摘要。
/// Human-readable summary of permission differences.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionSetDiff {
    /// 中文：按固定检查顺序列出的越权字段名称。
    pub expanded: Vec<&'static str>,
}

impl Default for PermissionSet {
    /// 中文：默认采用拒绝广泛副作用的最小权限集合。
    fn default() -> Self {
        Self::deny_all()
    }
}

impl PermissionSet {
    /// 中文：返回不允许网络、shell 或工作区访问的权限集合，但不主动限定身份集合。
    /// Returns a permission set that allows no broad side effects and does not
    /// constrain capability/adapter identity.
    pub fn deny_all() -> Self {
        Self {
            network: false,
            shell: false,
            read_workspace: false,
            write_workspace: false,
            max_timeout_ms: None,
            capabilities: None,
            adapters: None,
        }
    }

    /// 中文：返回开发环境只读上界，允许读取工作区，仍拒绝 shell 和写入。
    /// Returns a development upper bound that allows common read-only runtime
    /// behavior while still denying shell and workspace writes.
    pub fn read_only_runtime() -> Self {
        Self {
            read_workspace: true,
            ..Self::deny_all()
        }
    }

    /// 中文：设置网络访问开关。
    pub fn with_network(mut self, value: bool) -> Self {
        self.network = value;
        self
    }

    /// 中文：设置 shell 执行开关。
    pub fn with_shell(mut self, value: bool) -> Self {
        self.shell = value;
        self
    }

    /// 中文：设置工作区读取开关。
    pub fn with_read_workspace(mut self, value: bool) -> Self {
        self.read_workspace = value;
        self
    }

    /// 中文：设置工作区写入开关。
    pub fn with_write_workspace(mut self, value: bool) -> Self {
        self.write_workspace = value;
        self
    }

    /// 中文：设置单次操作的最大超时毫秒数。
    pub fn with_max_timeout_ms(mut self, value: u64) -> Self {
        self.max_timeout_ms = Some(value);
        self
    }

    /// 中文：把 capability 加入显式白名单；首次调用会把“不受限”转换成具体白名单。
    pub fn allow_capability(mut self, capability: CapabilityName) -> Self {
        self.capabilities
            .get_or_insert_with(BTreeSet::new)
            .insert(capability);
        self
    }

    /// 中文：把 Adapter 加入显式白名单；首次调用会把“不受限”转换成具体白名单。
    pub fn allow_adapter(mut self, adapter: AdapterId) -> Self {
        self.adapters
            .get_or_insert_with(BTreeSet::new)
            .insert(adapter);
        self
    }

    /// 中文：返回两组权限的交集，保证合并结果不会比任一输入更宽松。
    ///
    /// 布尔权限使用逻辑与，超时取更小约束；可选集合若只有一侧受限则保留该限制，
    /// 两侧都受限时取集合交集。
    /// Returns the intersection between two permission sets.
    pub fn narrowed_by(&self, other: &Self) -> Self {
        Self {
            network: self.network && other.network,
            shell: self.shell && other.shell,
            read_workspace: self.read_workspace && other.read_workspace,
            write_workspace: self.write_workspace && other.write_workspace,
            max_timeout_ms: min_timeout(self.max_timeout_ms, other.max_timeout_ms),
            capabilities: intersect_optional_sets(&self.capabilities, &other.capabilities),
            adapters: intersect_optional_sets(&self.adapters, &other.adapters),
        }
    }

    /// 中文：判断当前权限请求是否完全包含在给定上界内。
    /// Returns true when this permission set does not request anything outside
    /// `upper_bound`.
    pub fn is_subset_of(&self, upper_bound: &Self) -> bool {
        self.diff_against(upper_bound).expanded.is_empty()
    }

    /// 中文：返回会扩张给定上界的字段列表，顺序稳定以便测试和审计比较。
    /// Returns a stable list of fields that would expand `upper_bound`.
    pub fn diff_against(&self, upper_bound: &Self) -> PermissionSetDiff {
        let mut diff = PermissionSetDiff::default();

        if self.network && !upper_bound.network {
            diff.expanded.push("network");
        }
        if self.shell && !upper_bound.shell {
            diff.expanded.push("shell");
        }
        if self.read_workspace && !upper_bound.read_workspace {
            diff.expanded.push("read_workspace");
        }
        if self.write_workspace && !upper_bound.write_workspace {
            diff.expanded.push("write_workspace");
        }
        if timeout_expands(self.max_timeout_ms, upper_bound.max_timeout_ms) {
            diff.expanded.push("max_timeout_ms");
        }
        if optional_set_expands(&self.capabilities, &upper_bound.capabilities) {
            diff.expanded.push("capabilities");
        }
        if optional_set_expands(&self.adapters, &upper_bound.adapters) {
            diff.expanded.push("adapters");
        }

        diff
    }

    /// 中文：判断 capability 是否满足普通访问语义；未配置白名单时视为不受限。
    pub fn allows_capability(&self, capability: &CapabilityName) -> bool {
        self.capabilities
            .as_ref()
            .map(|capabilities| capabilities.contains(capability))
            .unwrap_or(true)
    }

    /// 中文：判断 capability 是否被显式列入白名单；未配置白名单时返回否。
    pub fn explicitly_allows_capability(&self, capability: &CapabilityName) -> bool {
        self.capabilities
            .as_ref()
            .map(|capabilities| capabilities.contains(capability))
            .unwrap_or(false)
    }

    /// 中文：判断 Adapter 是否满足普通访问语义；未配置白名单时视为不受限。
    pub fn allows_adapter(&self, adapter: &AdapterId) -> bool {
        self.adapters
            .as_ref()
            .map(|adapters| adapters.contains(adapter))
            .unwrap_or(true)
    }

    /// 中文：判断 Adapter 是否被显式列入白名单；未配置白名单时返回否。
    pub fn explicitly_allows_adapter(&self, adapter: &AdapterId) -> bool {
        self.adapters
            .as_ref()
            .map(|adapters| adapters.contains(adapter))
            .unwrap_or(false)
    }
}

/// 中文：合并可选超时上限；任一侧有限制时保留限制，两侧都有时取更小值。
fn min_timeout(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// 中文：判断请求超时是否突破上界；上界未限制超时时不构成扩张。
fn timeout_expands(requested: Option<u64>, upper_bound: Option<u64>) -> bool {
    match (requested, upper_bound) {
        (Some(requested), Some(upper_bound)) => requested > upper_bound,
        (Some(_), None) | (None, _) => false,
    }
}

/// 中文：对两个可选白名单求交集，同时保留 `None` 所代表的“不受该维度限制”语义。
fn intersect_optional_sets<T>(
    left: &Option<BTreeSet<T>>,
    right: &Option<BTreeSet<T>>,
) -> Option<BTreeSet<T>>
where
    T: Ord + Clone,
{
    match (left, right) {
        (Some(left), Some(right)) => Some(left.intersection(right).cloned().collect()),
        (Some(values), None) | (None, Some(values)) => Some(values.clone()),
        (None, None) => None,
    }
}

/// 中文：判断请求集合是否包含上界未允许的身份；无白名单请求会扩张有限白名单上界。
fn optional_set_expands<T>(
    requested: &Option<BTreeSet<T>>,
    upper_bound: &Option<BTreeSet<T>>,
) -> bool
where
    T: Ord,
{
    match (requested, upper_bound) {
        (Some(requested), Some(upper_bound)) => !requested.is_subset(upper_bound),
        (None, Some(_)) => true,
        (Some(_), None) | (None, None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中文：解析测试使用的 capability 名称。
    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    /// 中文：解析测试使用的 Adapter 标识。
    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[test]
    /// 中文：验证布尔权限通过逻辑与收窄。
    fn narrowing_intersects_boolean_permissions() {
        let upper = PermissionSet::deny_all()
            .with_network(true)
            .with_read_workspace(true);
        let request = PermissionSet::deny_all()
            .with_network(true)
            .with_write_workspace(true);

        let effective = upper.narrowed_by(&request);

        assert!(effective.network);
        assert!(!effective.read_workspace);
        assert!(!effective.write_workspace);
    }

    #[test]
    /// 中文：验证多层超时限制采用更小上限。
    fn narrowing_uses_lowest_timeout() {
        let upper = PermissionSet::deny_all().with_max_timeout_ms(120_000);
        let request = PermissionSet::deny_all().with_max_timeout_ms(30_000);

        assert_eq!(upper.narrowed_by(&request).max_timeout_ms, Some(30_000));
    }

    #[test]
    /// 中文：验证 capability 与 Adapter 白名单均取集合交集。
    fn narrowing_intersects_capabilities_and_adapters() {
        let upper = PermissionSet::deny_all()
            .allow_capability(capability("repo.analyze"))
            .allow_capability(capability("code.review"))
            .allow_adapter(adapter("codex-cli"));
        let request = PermissionSet::deny_all()
            .allow_capability(capability("code.review"))
            .allow_capability(capability("chat.reply"))
            .allow_adapter(adapter("codex-cli"))
            .allow_adapter(adapter("claude-api"));

        let effective = upper.narrowed_by(&request);

        assert!(effective.allows_capability(&capability("code.review")));
        assert!(!effective.allows_capability(&capability("repo.analyze")));
        assert!(effective.allows_adapter(&adapter("codex-cli")));
        assert!(!effective.allows_adapter(&adapter("claude-api")));
    }

    #[test]
    /// 中文：验证权限差异会稳定报告布尔和身份集合扩张。
    fn diff_reports_expansion() {
        let upper = PermissionSet::deny_all()
            .with_read_workspace(true)
            .allow_capability(capability("repo.analyze"));
        let request = PermissionSet::deny_all()
            .with_read_workspace(true)
            .with_shell(true)
            .allow_capability(capability("code.review"));

        let diff = request.diff_against(&upper);

        assert_eq!(diff.expanded, vec!["shell", "capabilities"]);
        assert!(!request.is_subset_of(&upper));
    }

    #[test]
    /// 中文：验证普通“不受限”与运行时门禁要求的“显式放行”语义不同。
    fn explicit_identity_allows_default_to_deny_for_runtime_gates() {
        let permissions = PermissionSet::deny_all();

        assert!(permissions.allows_capability(&capability("repo.analyze")));
        assert!(permissions.allows_adapter(&adapter("codex-cli")));
        assert!(!permissions.explicitly_allows_capability(&capability("repo.analyze")));
        assert!(!permissions.explicitly_allows_adapter(&adapter("codex-cli")));

        let permissions = permissions
            .allow_capability(capability("repo.analyze"))
            .allow_adapter(adapter("codex-cli"));

        assert!(permissions.explicitly_allows_capability(&capability("repo.analyze")));
        assert!(permissions.explicitly_allows_adapter(&adapter("codex-cli")));
    }
}
