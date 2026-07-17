//! 项目配置加载、Schema 验证、规范化与跨文件一致性边界。
//! Configuration loading and normalization boundary.

/// 主 `eva.yaml` 强类型配置。
pub mod eva_yaml;
/// Agent、Adapter 和 Capability 清单模型。
pub mod manifest;
/// 策略文档加载模型。
pub mod policy;
/// 路由表加载模型。
pub mod routes;
/// 配置文件 Schema 验证器。
pub mod schema;

use crate::eva_yaml::{ConfigRoots, EvaConfig};
use crate::manifest::adapter::{load_adapter_manifest, AdapterManifest};
use crate::manifest::agent::{load_agent_manifest, AgentManifest};
use crate::manifest::capability::{load_capability_manifest, CapabilityManifest};
use crate::policy::{load_policy_document, PolicyDocument};
use crate::routes::{load_routes, RouteConfig};
use crate::schema::validate_config_files_with_schemas;
use eva_core::EvaError;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub use eva_yaml::{
    load_eva_config, ObservabilityConfig, ObservabilityCorruptRecordPolicy,
    ObservabilityRetentionConfig, ObservabilityRetentionSink, OpenTelemetryDropPolicy,
    OpenTelemetryExporterConfig, RuntimeConfig, ServiceManagerConfig, ServiceManagerKind,
};
pub use manifest::adapter::{
    AdapterTransport, HardwareAdapterConfig, HardwareBusKind, HardwareClaimMode,
    HardwareDriverConfig, HardwareDriverKind, HardwareHotplugConfig, HardwareIdentityConfig,
    HardwareMatchConfig, HardwareProtocolConfig, HardwareProtocolKind, ProviderConfig,
    ProviderRestartConfig, ProviderRestartMode, ProviderRunAsIdentity, ProviderVaultSecretRef,
    RawAdapterTransport,
};
pub use manifest::agent::AgentManifestPermissions;
pub use manifest::capability::{CapabilityKind, RawCapabilityKind};
pub use routes::{RawRouteDelivery, RouteDelivery, RouteRule};
pub use schema::{schema_paths, SchemaPaths};

/// 由 `eva.yaml` 与拆分清单组装的项目级配置。
/// Project-level configuration assembled from `eva.yaml` and split manifests.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectConfig {
    /// 用于解析拆分配置路径的规范化项目根。
    /// Canonical project root used for resolving split configuration roots.
    pub project_root: PathBuf,
    /// 已加载主配置文件路径。
    /// Path to the loaded `eva.yaml` file.
    pub eva_config_path: PathBuf,
    /// 解析并规范化后的主配置。
    /// Parsed main configuration.
    pub eva: EvaConfig,
    /// 已相对项目根解析的配置根路径。
    /// Config roots resolved against `project_root`.
    pub roots: ConfigRoots,
    /// 已加载的 Agent 清单。
    /// Loaded Agent manifests.
    pub agents: Vec<AgentManifest>,
    /// 已加载的 Adapter 清单。
    /// Loaded Adapter manifests.
    pub adapters: Vec<AdapterManifest>,
    /// 已加载的 Capability 清单。
    /// Loaded capability manifests.
    pub capabilities: Vec<CapabilityManifest>,
    /// 已加载的策略文档。
    /// Loaded policy documents.
    pub policies: Vec<PolicyDocument>,
    /// 已加载的路由表。
    /// Loaded route table.
    pub routes: RouteConfig,
}

/// 从项目根加载、验证并交叉检查最小完整配置集合。
///
/// 顺序为：规范化项目根、加载主配置、枚举拆分文件、先用 Schema 验证原始 YAML，
/// 再构建强类型清单，最后检查跨文件引用。Schema 先行保证语义加载器只处理结构合法
/// 数据；任一阶段失败都不返回部分 `ProjectConfig`。文件列表排序后加载，使重复项错误
/// 与输出顺序跨平台稳定。
/// Loads the minimum project configuration set from a project root directory.
pub fn load_project_config(project_root: impl AsRef<Path>) -> Result<ProjectConfig, EvaError> {
    let project_root = normalize_existing_dir(project_root.as_ref(), "project root")?;
    let eva_config_path = project_root.join("config").join("eva.yaml");
    let eva = load_eva_config(&eva_config_path)?;
    let roots = eva.config.resolve_against(&project_root);

    let agent_paths = find_named_files(&roots.agent_dir, "agent.yaml")?;
    let adapter_paths = find_yaml_files(&roots.adapter_dir)?;
    let capability_paths = find_yaml_files(&roots.capability_dir)?;
    let policy_paths = find_yaml_files(&roots.policy_dir)?;

    validate_config_files_with_schemas(
        &roots,
        &eva_config_path,
        &agent_paths,
        &adapter_paths,
        &capability_paths,
        &policy_paths,
        &roots.route_file,
    )?;

    let agents = agent_paths
        .into_iter()
        .map(load_agent_manifest)
        .collect::<Result<Vec<_>, _>>()?;

    let adapters = adapter_paths
        .into_iter()
        .map(load_adapter_manifest)
        .collect::<Result<Vec<_>, _>>()?;

    let capabilities = capability_paths
        .into_iter()
        .map(load_capability_manifest)
        .collect::<Result<Vec<_>, _>>()?;
    let policies = policy_paths
        .into_iter()
        .map(load_policy_document)
        .collect::<Result<Vec<_>, _>>()?;
    let routes = load_routes(&roots.route_file)?;

    let project = ProjectConfig {
        project_root,
        eva_config_path,
        eva,
        roots,
        agents,
        adapters,
        capabilities,
        policies,
        routes,
    };
    validate_project_config(&project)?;
    Ok(project)
}

/// 检查单文件加载阶段不可见的目录、唯一性和交叉引用一致性。
/// Checks cross-file consistency that is not visible while loading one file.
pub fn validate_project_config(project: &ProjectConfig) -> Result<(), EvaError> {
    validate_roots(project)?;
    validate_unique_agents(&project.agents)?;
    validate_unique_adapters(&project.adapters)?;
    validate_unique_capabilities(&project.capabilities)?;
    validate_agent_references(&project.agents)?;
    validate_agent_permission_references(project)?;
    validate_agent_scripts(&project.agents)?;
    validate_hardware_adapter_configs(&project.adapters)?;
    validate_adapter_environments(project)?;
    validate_capability_providers(project)?;
    validate_route_agents(project)?;
    Ok(())
}

/// Applies environment-sensitive Adapter secret rules after eva.yaml and manifests are joined.
fn validate_adapter_environments(project: &ProjectConfig) -> Result<(), EvaError> {
    for adapter in &project.adapters {
        adapter.validate_for_environment(&project.eva.runtime.env)?;
    }
    Ok(())
}

/// 读取 UTF-8 YAML 文件并反序列化，统一映射 I/O 与位置化语法错误。
pub(crate) fn read_yaml_file<T>(
    path: impl AsRef<Path>,
    config_type: &'static str,
) -> Result<T, EvaError>
where
    T: DeserializeOwned,
{
    let path = path.as_ref();
    let content = fs::read_to_string(path).map_err(|error| io_error(error, path, config_type))?;
    serde_yaml::from_str(&content).map_err(|error| yaml_error(error, path, config_type))
}

/// 构造包含配置类型、路径和字段的参数错误。
pub(crate) fn invalid_config(
    config_type: &'static str,
    path: &Path,
    field: impl Into<String>,
    message: impl Into<String>,
) -> EvaError {
    EvaError::invalid_argument(message)
        .with_context("config_type", config_type)
        .with_context("path", path.display().to_string())
        .with_context("field", field.into())
}

/// 在保留原错误种类的同时附加配置类型、路径和字段上下文。
pub(crate) fn with_field_context(
    error: EvaError,
    config_type: &'static str,
    path: &Path,
    field: impl Into<String>,
) -> EvaError {
    error
        .with_context("config_type", config_type)
        .with_context("path", path.display().to_string())
        .with_context("field", field.into())
}

/// 要求字符串非空、首尾无空白，并返回原值。
pub(crate) fn require_non_empty(
    value: String,
    config_type: &'static str,
    path: &Path,
    field: &'static str,
) -> Result<String, EvaError> {
    if value.trim().is_empty() {
        Err(invalid_config(
            config_type,
            path,
            field,
            "required field cannot be empty",
        ))
    } else if value.trim() != value {
        Err(invalid_config(
            config_type,
            path,
            field,
            "field cannot contain leading or trailing whitespace",
        ))
    } else {
        Ok(value)
    }
}

/// 要求路径的 OS 字符串表示非空。
pub(crate) fn require_non_empty_path(
    value: PathBuf,
    config_type: &'static str,
    path: &Path,
    field: &'static str,
) -> Result<PathBuf, EvaError> {
    if value.as_os_str().is_empty() {
        Err(invalid_config(
            config_type,
            path,
            field,
            "path field cannot be empty",
        ))
    } else {
        Ok(value)
    }
}

/// 验证全部拆分配置目录和路由文件实际存在。
fn validate_roots(project: &ProjectConfig) -> Result<(), EvaError> {
    require_existing_dir(&project.roots.agent_dir, "agent_dir")?;
    require_existing_dir(&project.roots.adapter_dir, "adapter_dir")?;
    require_existing_dir(&project.roots.capability_dir, "capability_dir")?;
    require_existing_dir(&project.roots.policy_dir, "policy_dir")?;
    require_existing_dir(&project.roots.schema_dir, "schema_dir")?;
    require_existing_file(&project.roots.route_file, "route_file")?;
    Ok(())
}

/// 检查 Agent 标识唯一，并在冲突时同时报告两份源路径。
fn validate_unique_agents(agents: &[AgentManifest]) -> Result<(), EvaError> {
    let mut seen = BTreeMap::new();
    for agent in agents {
        let id = agent.id.as_str().to_owned();
        if let Some(first_path) = seen.insert(id.clone(), agent.path.clone()) {
            return Err(EvaError::conflict("duplicate Agent manifest id")
                .with_context("agent_id", id)
                .with_context("first_path", first_path.display().to_string())
                .with_context("second_path", agent.path.display().to_string()));
        }
    }
    Ok(())
}

/// 检查 Adapter 标识唯一，并在冲突时同时报告两份源路径。
fn validate_unique_adapters(adapters: &[AdapterManifest]) -> Result<(), EvaError> {
    let mut seen = BTreeMap::new();
    for adapter in adapters {
        let id = adapter.id.as_str().to_owned();
        if let Some(first_path) = seen.insert(id.clone(), adapter.path.clone()) {
            return Err(EvaError::conflict("duplicate Adapter manifest id")
                .with_context("adapter_id", id)
                .with_context("first_path", first_path.display().to_string())
                .with_context("second_path", adapter.path.display().to_string()));
        }
    }
    Ok(())
}

/// 检查 Capability 清单标识唯一。
fn validate_unique_capabilities(capabilities: &[CapabilityManifest]) -> Result<(), EvaError> {
    let mut seen = BTreeMap::new();
    for capability in capabilities {
        let id = capability.id.as_str().to_owned();
        if let Some(first_path) = seen.insert(id.clone(), capability.path.clone()) {
            return Err(EvaError::conflict("duplicate Capability manifest id")
                .with_context("capability_id", id)
                .with_context("first_path", first_path.display().to_string())
                .with_context("second_path", capability.path.display().to_string()));
        }
    }
    Ok(())
}

/// 验证所有 Agent 父子引用指向已加载 Agent。
fn validate_agent_references(agents: &[AgentManifest]) -> Result<(), EvaError> {
    let known = agents
        .iter()
        .map(|agent| agent.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for agent in agents {
        if let Some(parent) = &agent.parent {
            if !known.contains(parent.as_str()) {
                return Err(EvaError::not_found("Agent parent reference does not exist")
                    .with_context("agent_id", agent.id.as_str())
                    .with_context("parent", parent.as_str())
                    .with_context("path", agent.path.display().to_string()));
            }
        }

        for child in &agent.children {
            if !known.contains(child.as_str()) {
                return Err(EvaError::not_found("Agent child reference does not exist")
                    .with_context("agent_id", agent.id.as_str())
                    .with_context("child", child.as_str())
                    .with_context("path", agent.path.display().to_string()));
            }
        }
    }

    Ok(())
}

/// 将相对脚本路径按各自清单目录解析，并要求目标是现有文件。
fn validate_agent_scripts(agents: &[AgentManifest]) -> Result<(), EvaError> {
    for agent in agents {
        let manifest_dir = agent.path.parent().unwrap_or_else(|| Path::new(""));
        let script_path = if agent.script.is_absolute() {
            agent.script.clone()
        } else {
            manifest_dir.join(&agent.script)
        };
        if !script_path.is_file() {
            return Err(EvaError::not_found("Agent script does not exist")
                .with_context("agent_id", agent.id.as_str())
                .with_context("script", agent.script.display().to_string())
                .with_context("resolved_path", script_path.display().to_string())
                .with_context("path", agent.path.display().to_string()));
        }
    }
    Ok(())
}

/// 验证 Agent 权限中的 Provider 与能力引用在项目内有声明。
///
/// 能力可以由 Adapter 直接暴露，也可以由 Capability 清单声明；任何未知引用都会
/// 附带建议并失败关闭，避免权限拼写错误在运行时悄然扩大或缩小能力面。
fn validate_agent_permission_references(project: &ProjectConfig) -> Result<(), EvaError> {
    let adapters = project
        .adapters
        .iter()
        .map(|adapter| adapter.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let adapter_capabilities = project
        .adapters
        .iter()
        .flat_map(|adapter| adapter.capabilities.iter())
        .map(|capability| capability.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let declared_capabilities = project
        .capabilities
        .iter()
        .map(|capability| capability.capability.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for agent in &project.agents {
        for provider in &agent.permissions.adapter_providers {
            if !adapters.contains(provider) {
                return Err(EvaError::not_found(
                    "Agent permission references unknown Adapter provider",
                )
                .with_context("agent_id", agent.id.as_str())
                .with_context("provider", provider)
                .with_context("path", agent.path.display().to_string())
                .with_context("field", "permissions.adapters.providers")
                .with_context(
                    "suggestion",
                    "add the Adapter manifest or remove the provider permission",
                ));
            }
        }

        for capability in &agent.permissions.adapter_capabilities {
            let capability = capability.as_str();
            if !adapter_capabilities.contains(capability)
                && !declared_capabilities.contains(capability)
            {
                return Err(EvaError::not_found(
                    "Agent permission references unknown adapter capability",
                )
                .with_context("agent_id", agent.id.as_str())
                .with_context("capability", capability)
                .with_context("path", agent.path.display().to_string())
                .with_context("field", "permissions.adapters.capabilities")
                .with_context(
                    "suggestion",
                    "add the Adapter/Capability manifest or remove the capability permission",
                ));
            }
        }
    }

    Ok(())
}

/// 验证每份 Capability 的所有 Provider 引用均指向已加载 Adapter。
fn validate_capability_providers(project: &ProjectConfig) -> Result<(), EvaError> {
    let adapters = project
        .adapters
        .iter()
        .map(|adapter| adapter.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for capability in &project.capabilities {
        for provider in capability.adapter_providers() {
            if !adapters.contains(provider.as_str()) {
                return Err(
                    EvaError::not_found("Capability provider Adapter does not exist")
                        .with_context("capability_id", capability.id.as_str())
                        .with_context("provider", provider.as_str())
                        .with_context("path", capability.path.display().to_string()),
                );
            }
        }
    }

    Ok(())
}

/// 强制解析所有 Hardware Adapter 扩展，使非法硬件字段在启动前暴露。
fn validate_hardware_adapter_configs(adapters: &[AdapterManifest]) -> Result<(), EvaError> {
    for adapter in adapters {
        adapter.hardware_config()?;
    }
    Ok(())
}

/// 验证每条路由的目标 Agent 均存在。
fn validate_route_agents(project: &ProjectConfig) -> Result<(), EvaError> {
    let agents = project
        .agents
        .iter()
        .map(|agent| agent.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for route in &project.routes.routes {
        for agent in &route.agents {
            if !agents.contains(agent.as_str()) {
                return Err(EvaError::not_found("Route target Agent does not exist")
                    .with_context("agent_id", agent.as_str())
                    .with_context("pattern", route.pattern.as_str())
                    .with_context("path", project.routes.path.display().to_string()));
            }
        }
    }

    Ok(())
}

/// 递归查找具有指定文件名的普通文件并按路径排序。
fn find_named_files(root: &Path, filename: &str) -> Result<Vec<PathBuf>, EvaError> {
    let mut files = Vec::new();
    collect_files(root, &mut files, &|path| {
        path.file_name().and_then(|name| name.to_str()) == Some(filename)
    })?;
    files.sort();
    Ok(files)
}

/// 递归查找 `.yaml` 或 `.yml` 普通文件并按路径排序。
fn find_yaml_files(root: &Path) -> Result<Vec<PathBuf>, EvaError> {
    let mut files = Vec::new();
    collect_files(root, &mut files, &is_yaml_file)?;
    files.sort();
    Ok(files)
}

/// 深度优先收集满足谓词的普通文件。
///
/// 只跟随 `read_dir` 返回的目录项类型，不把非文件节点加入结果；任何目录读取错误
/// 立即传播，避免以不完整配置集合继续启动。
fn collect_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    include: &dyn Fn(&Path) -> bool,
) -> Result<(), EvaError> {
    let entries = fs::read_dir(root).map_err(|error| io_error(error, root, "config root"))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(error, root, "config root"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| io_error(error, &path, "config root"))?;
        if file_type.is_dir() {
            collect_files(&path, files, include)?;
        } else if file_type.is_file() && include(&path) {
            files.push(path);
        }
    }
    Ok(())
}

/// 判断路径扩展名是否为受支持 YAML 拼写。
fn is_yaml_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("yaml") | Some("yml")
    )
}

/// 规范化现有目录路径并拒绝非目录目标。
fn normalize_existing_dir(path: &Path, field: &'static str) -> Result<PathBuf, EvaError> {
    let path = fs::canonicalize(path).map_err(|error| io_error(error, path, field))?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(EvaError::invalid_argument("path is not a directory")
            .with_context("field", field)
            .with_context("path", path.display().to_string()))
    }
}

/// 要求配置路径指向现有目录。
fn require_existing_dir(path: &Path, field: &'static str) -> Result<(), EvaError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(
            EvaError::not_found("configuration directory does not exist")
                .with_context("field", field)
                .with_context("path", path.display().to_string()),
        )
    }
}

/// 要求配置路径指向现有普通文件。
fn require_existing_file(path: &Path, field: &'static str) -> Result<(), EvaError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(EvaError::not_found("configuration file does not exist")
            .with_context("field", field)
            .with_context("path", path.display().to_string()))
    }
}

/// 将 NotFound 与其他 I/O 错误映射为稳定 EvaError 种类并附带路径。
fn io_error(error: std::io::Error, path: &Path, config_type: &'static str) -> EvaError {
    let message = format!("failed to read {config_type}");
    let base = if error.kind() == std::io::ErrorKind::NotFound {
        EvaError::not_found(message)
    } else {
        EvaError::internal(message)
    };
    base.with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

/// 将 YAML 语法错误映射为参数错误，并在可用时附加行列位置。
fn yaml_error(error: serde_yaml::Error, path: &Path, config_type: &'static str) -> EvaError {
    let mut eva_error = EvaError::invalid_argument("failed to parse YAML")
        .with_context("config_type", config_type)
        .with_context("path", path.display().to_string())
        .with_context("yaml_error", error.to_string());
    if let Some(location) = error.location() {
        eva_error = eva_error
            .with_context("line", location.line().to_string())
            .with_context("column", location.column().to_string());
    }
    eva_error
}

#[cfg(test)]
/// 项目配置聚合、Schema 和跨文件引用验证测试。
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 返回包含示例配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证所有配置根、清单、策略和路由可组成完整项目。
    fn project_config_loads_all_config_roots() {
        let project = load_project_config(workspace_root()).unwrap();

        assert!(!project.agents.is_empty());
        assert!(!project.adapters.is_empty());
        assert!(!project.capabilities.is_empty());
        assert!(!project.policies.is_empty());
        assert!(!project.routes.routes.is_empty());
        assert!(project.roots.route_file.is_file());
        assert!(project.roots.schema_dir.is_dir());
    }

    #[test]
    /// 验证重复 Agent 标识在项目级校验中被拒绝。
    fn validate_project_config_rejects_duplicate_agent_id() {
        let mut project = load_project_config(workspace_root()).unwrap();
        let duplicate = project.agents[0].clone();
        project.agents.push(duplicate);

        let error = validate_project_config(&project).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert!(error.message().contains("duplicate Agent"));
    }

    #[test]
    /// 验证路由引用未知 Agent 时失败关闭。
    fn validate_project_config_rejects_unknown_route_agent() {
        let mut project = load_project_config(workspace_root()).unwrap();
        project.routes.routes[0]
            .agents
            .push("missing-agent".try_into().unwrap());

        let error = validate_project_config(&project).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(error.message().contains("Route target"));
    }

    #[test]
    /// 验证 Schema 禁止的额外属性在语义加载前被拒绝。
    fn load_project_config_rejects_schema_additional_property() {
        let root = minimal_project("schema-additional", "codex-cli");
        fs::write(
            root.join("config/routes/topics.yaml"),
            r#"routes:
  - pattern: /sys
    delivery: fanout
    agents:
      - root-agent
    extra: denied
"#,
        )
        .unwrap();

        let error = load_project_config(&root).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_context(&error, "field", "routes[0].extra");
        assert_context(&error, "schema_rule", "additionalProperties");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证 Agent 权限引用未知 Provider 时返回稳定上下文。
    fn load_project_config_rejects_unknown_agent_permission_provider() {
        let root = minimal_project("missing-provider", "missing-adapter");

        let error = load_project_config(&root).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_context(&error, "field", "permissions.adapters.providers");
        assert_context(&error, "provider", "missing-adapter");
        assert_context(
            &error,
            "suggestion",
            "add the Adapter manifest or remove the provider permission",
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证未知硬件驱动类别在项目加载阶段被拒绝。
    fn load_project_config_rejects_unknown_hardware_driver_kind() {
        let root = minimal_project("bad-hardware-driver", "codex-cli");
        fs::write(
            root.join("config/adapters/bad-scale.yaml"),
            r#"id: bad-scale
name: Bad Scale
version: 1.0.0
enabled: true
transport: hardware
hardware:
  bus: usb
  identity:
    logical_name: bad-scale
    device_class: scale
  driver:
    kind: mystery
capabilities:
  - hardware.scale.read
permissions: {}
limits: {}
routing: {}
"#,
        )
        .unwrap();

        let error = load_project_config(&root).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_context(&error, "field", "hardware.driver.kind");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证项目级 production 环境会拒绝 Adapter 中的明文敏感 Header。
    fn load_project_config_rejects_production_plaintext_secret() {
        let root = minimal_project("production-plaintext-secret", "codex-cli");
        let eva_path = root.join("config/eva.yaml");
        let eva = fs::read_to_string(&eva_path).unwrap();
        fs::write(&eva_path, eva.replace("env: dev", "env: production")).unwrap();
        fs::write(
            root.join("config/adapters/codex-cli.yaml"),
            r#"id: codex-cli
name: Codex CLI
version: 1.0.0
enabled: true
transport: builtin
command: codex
headers:
  Authorization: Bearer plaintext-token
capabilities:
  - repo.analyze
permissions: {}
limits: {}
routing: {}
"#,
        )
        .unwrap();

        let error = load_project_config(&root).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_context(&error, "field", "headers.Authorization");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证 dev 保持兼容且 production allowlisted env 引用可通过项目级校验。
    fn load_project_config_preserves_dev_and_allowlisted_production_compatibility() {
        let root = minimal_project("production-allowlisted-secret", "codex-cli");
        let adapter_path = root.join("config/adapters/codex-cli.yaml");
        fs::write(
            &adapter_path,
            r#"id: codex-cli
name: Codex CLI
version: 1.0.0
enabled: true
transport: builtin
headers:
  Authorization: Bearer development-token
capabilities:
  - repo.analyze
permissions: {}
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(load_project_config(&root).is_ok());

        let eva_path = root.join("config/eva.yaml");
        let eva = fs::read_to_string(&eva_path).unwrap();
        fs::write(&eva_path, eva.replace("env: dev", "env: production")).unwrap();
        fs::write(
            &adapter_path,
            r#"id: codex-cli
name: Codex CLI
version: 1.0.0
enabled: true
transport: builtin
headers:
  Authorization: env:API_TOKEN
capabilities:
  - repo.analyze
permissions:
  env:
    - API_TOKEN
limits: {}
routing: {}
"#,
        )
        .unwrap();
        assert!(load_project_config(&root).is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    /// 断言错误上下文包含指定键值。
    fn assert_context(error: &EvaError, key: &str, value: &str) {
        assert!(
            error
                .context()
                .entries()
                .iter()
                .any(|(entry_key, entry_value)| entry_key == key && entry_value == value),
            "missing context {key}={value:?}: {:?}",
            error.context().entries()
        );
    }

    /// 创建带最小配置和可控 Agent Provider 引用的临时项目。
    fn minimal_project(name: &str, agent_provider: &str) -> PathBuf {
        let root = test_temp_dir(name);
        let config = root.join("config");
        for directory in [
            "agents/root-agent",
            "adapters",
            "capabilities",
            "policies",
            "routes",
            "schemas",
        ] {
            fs::create_dir_all(config.join(directory)).unwrap();
        }

        fs::write(
            config.join("eva.yaml"),
            r#"runtime:
  env: dev
  workspace: .
  hot_reload: true
eventbus: {}
scheduler: {}
observability: {}
config:
  agent_dir: config/agents
  adapter_dir: config/adapters
  capability_dir: config/capabilities
  policy_dir: config/policies
  route_file: config/routes/topics.yaml
  schema_dir: config/schemas
"#,
        )
        .unwrap();
        fs::write(config.join("agents/root-agent/main.lua"), "return {}\n").unwrap();
        fs::write(
            config.join("agents/root-agent/agent.yaml"),
            format!(
                r#"id: root-agent
enabled: true
script: main.lua
subscriptions:
  - /sys
permissions:
  emit:
    - /sys/**
  adapters:
    capabilities:
      - repo.analyze
    providers:
      - {agent_provider}
"#
            ),
        )
        .unwrap();
        fs::write(
            config.join("adapters/codex-cli.yaml"),
            r#"id: codex-cli
name: Codex CLI
version: 1.0.0
enabled: true
transport: builtin
capabilities:
  - repo.analyze
permissions: {}
limits: {}
routing: {}
"#,
        )
        .unwrap();
        fs::write(
            config.join("routes/topics.yaml"),
            r#"routes:
  - pattern: /sys
    delivery: fanout
    agents:
      - root-agent
"#,
        )
        .unwrap();
        write_minimal_schemas(&config.join("schemas"));
        root
    }

    /// 写入最小测试项目所需的 Schema 文件。
    fn write_minimal_schemas(schema_dir: &Path) {
        fs::write(
            schema_dir.join("eva.schema.json"),
            r#"{"type":"object","required":["runtime","eventbus","scheduler","observability","config"],"properties":{"runtime":{"type":"object","required":["env","workspace","hot_reload"],"properties":{"env":{"type":"string"},"workspace":{"type":"string"},"hot_reload":{"type":"boolean"}},"additionalProperties":true},"eventbus":{"type":"object"},"scheduler":{"type":"object"},"observability":{"type":"object"},"config":{"type":"object"}},"additionalProperties":true}"#,
        )
        .unwrap();
        fs::write(
            schema_dir.join("agent.schema.json"),
            r#"{"type":"object","required":["id","enabled","script","subscriptions","permissions"],"properties":{"id":{"type":"string","pattern":"^[a-zA-Z0-9][a-zA-Z0-9_.-]*$"},"enabled":{"type":"boolean"},"script":{"type":"string"},"subscriptions":{"type":"array","items":{"type":"string","pattern":"^/.+"}},"permissions":{"type":"object"}},"additionalProperties":true}"#,
        )
        .unwrap();
        fs::write(
            schema_dir.join("adapter.schema.json"),
            r#"{"type":"object","required":["id","name","version","enabled","transport","capabilities","permissions","limits","routing"],"properties":{"id":{"type":"string","pattern":"^[a-zA-Z0-9][a-zA-Z0-9_.-]*$"},"name":{"type":"string"},"version":{"type":"string"},"enabled":{"type":"boolean"},"transport":{"type":"string","enum":["builtin","stdio","http","eventbus","mcp","skill","hardware","lua_capability"]},"capabilities":{"type":"array","items":{"type":"string"}},"permissions":{"type":"object"},"limits":{"type":"object"},"routing":{"type":"object"}},"additionalProperties":true}"#,
        )
        .unwrap();
        fs::write(
            schema_dir.join("capability.schema.json"),
            r#"{"type":"object","required":["id","name","version","enabled","kind","capability"],"properties":{"id":{"type":"string"},"name":{"type":"string"},"version":{"type":"string"},"enabled":{"type":"boolean"},"kind":{"type":"string","enum":["adapter_capability","lua_capability","mcp_tool","skill"]},"capability":{"type":"string"}},"additionalProperties":true}"#,
        )
        .unwrap();
        fs::write(
            schema_dir.join("policy.schema.json"),
            r#"{"type":"object","minProperties":1,"additionalProperties":true}"#,
        )
        .unwrap();
        fs::write(
            schema_dir.join("routes.schema.json"),
            r#"{"type":"object","required":["routes"],"properties":{"routes":{"type":"array","items":{"type":"object","required":["pattern","delivery","agents"],"properties":{"pattern":{"type":"string","pattern":"^/.+"},"delivery":{"type":"string","enum":["fanout","compete"]},"agents":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}}},"additionalProperties":false}"#,
        )
        .unwrap();
    }

    /// 创建进程和时间戳隔离的临时目录路径。
    fn test_temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eva-config-project-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }
}
