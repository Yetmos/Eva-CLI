//! JSON Schema path and alignment helpers.

use crate::eva_yaml::ConfigRoots;
use std::path::PathBuf;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load schemas and validate parsed configuration structures";

/// Expected schema file paths under a schema root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaPaths {
    pub eva: PathBuf,
    pub agent: PathBuf,
    pub adapter: PathBuf,
    pub capability: PathBuf,
    pub policy: PathBuf,
    pub routes: PathBuf,
}

/// Returns the canonical schema file paths for a resolved config root.
pub fn schema_paths(roots: &ConfigRoots) -> SchemaPaths {
    SchemaPaths {
        eva: roots.schema_dir.join("eva.schema.json"),
        agent: roots.schema_dir.join("agent.schema.json"),
        adapter: roots.schema_dir.join("adapter.schema.json"),
        capability: roots.schema_dir.join("capability.schema.json"),
        policy: roots.schema_dir.join("policy.schema.json"),
        routes: roots.schema_dir.join("routes.schema.json"),
    }
}

/// Adapter transport values currently accepted by `eva-config`.
pub const ADAPTER_TRANSPORT_VALUES: &[&str] = &[
    "builtin",
    "stdio",
    "http",
    "eventbus",
    "mcp",
    "skill",
    "hardware",
    "lua_capability",
];

/// Capability kind values currently accepted by `eva-config`.
pub const CAPABILITY_KIND_VALUES: &[&str] =
    &["adapter_capability", "lua_capability", "mcp_tool", "skill"];

/// Topic route delivery values currently accepted by `eva-config`.
pub const ROUTE_DELIVERY_VALUES: &[&str] = &["fanout", "compete"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eva_yaml::load_eva_config;
    use crate::routes::RouteDelivery;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn schema_paths_point_to_sample_schemas() {
        let root = workspace_root();
        let config = load_eva_config(root.join("config").join("eva.yaml")).unwrap();
        let roots = config.config.resolve_against(&root);
        let paths = schema_paths(&roots);

        assert!(paths.eva.is_file());
        assert!(paths.agent.is_file());
        assert!(paths.adapter.is_file());
        assert!(paths.capability.is_file());
        assert!(paths.policy.is_file());
        assert!(paths.routes.is_file());
    }

    #[test]
    fn enum_values_match_supported_manifest_values() {
        assert!(ADAPTER_TRANSPORT_VALUES.contains(&"stdio"));
        assert!(ADAPTER_TRANSPORT_VALUES.contains(&"mcp"));
        assert!(CAPABILITY_KIND_VALUES.contains(&"adapter_capability"));
        assert!(CAPABILITY_KIND_VALUES.contains(&"mcp_tool"));
        assert_eq!(
            ROUTE_DELIVERY_VALUES
                .iter()
                .map(|value| RouteDelivery::parse(value).unwrap().as_str())
                .collect::<Vec<_>>(),
            ROUTE_DELIVERY_VALUES
        );
    }
}
