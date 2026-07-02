//! Effective permission merging without expansion.

use crate::{PermissionSet, SandboxPolicy};
use eva_core::EvaError;

/// One policy layer participating in effective policy calculation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLayer {
    pub name: String,
    pub permissions: PermissionSet,
    pub sandbox: SandboxPolicy,
}

/// Final policy obtained by intersecting all layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    pub permissions: PermissionSet,
    pub sandbox: SandboxPolicy,
    pub layer_names: Vec<String>,
}

impl PolicyLayer {
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

    /// Returns a request narrowed by the effective permissions.
    pub fn narrow_request(&self, request: &PermissionSet) -> PermissionSet {
        self.permissions.narrowed_by(request)
    }

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
    fn effective_policy_requires_at_least_one_layer() {
        let error = EffectivePolicy::from_layers([]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
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
