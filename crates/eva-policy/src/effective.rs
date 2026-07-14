//! 中文：通过只收窄不扩张的方式合并多层权限和沙箱策略。
//! Effective permission merging without expansion.

use crate::{PermissionSet, SandboxPolicy};
use eva_core::EvaError;

/// 中文：参与有效策略计算的一层具名策略。
/// One policy layer participating in effective policy calculation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLayer {
    /// 中文：用于审计和诊断的层名称。
    pub name: String,
    /// 中文：该层允许的权限上界。
    pub permissions: PermissionSet,
    /// 中文：该层施加的沙箱限制。
    pub sandbox: SandboxPolicy,
}

/// 中文：对全部策略层逐层取交集后得到的最终策略。
/// Final policy obtained by intersecting all layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    /// 中文：所有层共同允许的最终权限。
    pub permissions: PermissionSet,
    /// 中文：所有层合并后的最严格沙箱约束。
    pub sandbox: SandboxPolicy,
    /// 中文：按参与合并顺序保存的层名称，供解释决策来源。
    pub layer_names: Vec<String>,
}

impl PolicyLayer {
    /// 中文：从层名称、权限上界和沙箱约束创建策略层。
    pub fn new(
        name: impl Into<String>,
        permissions: PermissionSet,
        sandbox: SandboxPolicy,
    ) -> Self {
        Self {
            name: name.into(),
            permissions,
            sandbox,
        }
    }
}

impl EffectivePolicy {
    /// 中文：按输入顺序对策略层求交集；至少需要一层，避免空输入意外产生无限制策略。
    ///
    /// 布尔能力只能从允许变为拒绝，集合只能缩小，数值资源上限取更小值；因此后加入的
    /// 项目或请求层无法突破系统层建立的安全边界。
    /// Intersects policy layers in order. At least one layer is required so the
    /// caller cannot accidentally construct an all-unbounded policy.
    pub fn from_layers(layers: impl IntoIterator<Item = PolicyLayer>) -> Result<Self, EvaError> {
        let mut layers = layers.into_iter();
        let first = layers
            .next()
            .ok_or_else(|| EvaError::invalid_argument("at least one policy layer is required"))?;

        let mut permissions = first.permissions;
        let mut sandbox = first.sandbox;
        let mut layer_names = vec![first.name];

        for layer in layers {
            permissions = permissions.narrowed_by(&layer.permissions);
            sandbox = sandbox.narrowed_by(&layer.sandbox);
            layer_names.push(layer.name);
        }

        Ok(Self {
            permissions,
            sandbox,
            layer_names,
        })
    }

    /// 中文：把请求权限收窄到有效权限范围内，不返回越权部分。
    /// Returns a request narrowed by the effective permissions.
    pub fn narrow_request(&self, request: &PermissionSet) -> PermissionSet {
        self.permissions.narrowed_by(request)
    }

    /// 中文：拒绝任何超出有效策略的请求，并在错误上下文中列出扩张字段。
    /// Rejects requests that ask for permissions outside the effective policy.
    pub fn ensure_request_allowed(&self, request: &PermissionSet) -> Result<(), EvaError> {
        let diff = request.diff_against(&self.permissions);
        if diff.expanded.is_empty() {
            Ok(())
        } else {
            Err(
                EvaError::permission_denied("request expands effective policy")
                    .with_context("expanded_fields", diff.expanded.join(",")),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    #[test]
    /// 中文：验证空策略层集合不会产生默认放行策略。
    fn effective_policy_requires_at_least_one_layer() {
        let error = EffectivePolicy::from_layers([]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    /// 中文：验证权限、资源上限和层名称均按顺序正确合并。
    fn effective_policy_intersects_layers() {
        let system = PolicyLayer::new(
            "system",
            PermissionSet::deny_all()
                .with_network(true)
                .with_read_workspace(true)
                .with_max_timeout_ms(120_000),
            SandboxPolicy::default().with_memory_mb(128),
        );
        let manifest = PolicyLayer::new(
            "manifest",
            PermissionSet::deny_all()
                .with_read_workspace(true)
                .with_max_timeout_ms(30_000),
            SandboxPolicy::default().with_memory_mb(32),
        );

        let effective = EffectivePolicy::from_layers([system, manifest]).unwrap();

        assert!(!effective.permissions.network);
        assert!(effective.permissions.read_workspace);
        assert_eq!(effective.permissions.max_timeout_ms, Some(30_000));
        assert_eq!(effective.sandbox.memory_mb, Some(32));
        assert_eq!(effective.layer_names, vec!["system", "manifest"]);
    }

    #[test]
    /// 中文：验证请求扩张会被拒绝并报告具体越权字段。
    fn request_expansion_is_rejected() {
        let policy = EffectivePolicy::from_layers([PolicyLayer::new(
            "system",
            PermissionSet::deny_all().with_read_workspace(true),
            SandboxPolicy::default(),
        )])
        .unwrap();
        let request = PermissionSet::deny_all().with_write_workspace(true);

        let error = policy.ensure_request_allowed(&request).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "expanded_fields")
                .unwrap()
                .1,
            "write_workspace"
        );
    }
}
