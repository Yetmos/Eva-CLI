//! Permission data types and narrowing rules.

use eva_core::{AdapterId, CapabilityName};
use std::collections::BTreeSet;

/// A request/runtime permission set.
///
/// Boolean fields are explicit allow switches. Capability and adapter sets use
/// `None` as "not constrained by this dimension" and `Some(empty)` as
/// "explicitly allow no values". Narrowing always intersects permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSet {
    pub network: bool,
    pub shell: bool,
    pub read_workspace: bool,
    pub write_workspace: bool,
    pub max_timeout_ms: Option<u64>,
    pub capabilities: Option<BTreeSet<CapabilityName>>,
    pub adapters: Option<BTreeSet<AdapterId>>,
}

/// Human-readable summary of permission differences.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionSetDiff {
    pub expanded: Vec<&'static str>,
}

impl Default for PermissionSet {
    fn default() -> Self {
        Self::deny_all()
    }
}

impl PermissionSet {
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

    /// Returns a development upper bound that allows common read-only runtime
    /// behavior while still denying shell and workspace writes.
    pub fn read_only_runtime() -> Self {
        Self {
            read_workspace: true,
            ..Self::deny_all()
        }
    }

    pub fn with_network(mut self, value: bool) -> Self {
        self.network = value;
        self
    }

    pub fn with_shell(mut self, value: bool) -> Self {
        self.shell = value;
        self
    }

    pub fn with_read_workspace(mut self, value: bool) -> Self {
        self.read_workspace = value;
        self
    }

    pub fn with_write_workspace(mut self, value: bool) -> Self {
        self.write_workspace = value;
        self
    }

    pub fn with_max_timeout_ms(mut self, value: u64) -> Self {
        self.max_timeout_ms = Some(value);
        self
    }

    pub fn allow_capability(mut self, capability: CapabilityName) -> Self {
        self.capabilities
            .get_or_insert_with(BTreeSet::new)
            .insert(capability);
        self
    }

    pub fn allow_adapter(mut self, adapter: AdapterId) -> Self {
        self.adapters
            .get_or_insert_with(BTreeSet::new)
            .insert(adapter);
        self
    }

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

    /// Returns true when this permission set does not request anything outside
    /// `upper_bound`.
    pub fn is_subset_of(&self, upper_bound: &Self) -> bool {
        self.diff_against(upper_bound).expanded.is_empty()
    }

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

    pub fn allows_capability(&self, capability: &CapabilityName) -> bool {
        self.capabilities
            .as_ref()
            .map(|capabilities| capabilities.contains(capability))
            .unwrap_or(true)
    }

    pub fn explicitly_allows_capability(&self, capability: &CapabilityName) -> bool {
        self.capabilities
            .as_ref()
            .map(|capabilities| capabilities.contains(capability))
            .unwrap_or(false)
    }

    pub fn allows_adapter(&self, adapter: &AdapterId) -> bool {
        self.adapters
            .as_ref()
            .map(|adapters| adapters.contains(adapter))
            .unwrap_or(true)
    }

    pub fn explicitly_allows_adapter(&self, adapter: &AdapterId) -> bool {
        self.adapters
            .as_ref()
            .map(|adapters| adapters.contains(adapter))
            .unwrap_or(false)
    }
}

fn min_timeout(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn timeout_expands(requested: Option<u64>, upper_bound: Option<u64>) -> bool {
    match (requested, upper_bound) {
        (Some(requested), Some(upper_bound)) => requested > upper_bound,
        (Some(_), None) | (None, _) => false,
    }
}

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

    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[test]
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
    fn narrowing_uses_lowest_timeout() {
        let upper = PermissionSet::deny_all().with_max_timeout_ms(120_000);
        let request = PermissionSet::deny_all().with_max_timeout_ms(30_000);

        assert_eq!(upper.narrowed_by(&request).max_timeout_ms, Some(30_000));
    }

    #[test]
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
