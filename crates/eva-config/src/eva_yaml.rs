//! Main `eva.yaml` loading and normalization.

use crate::{
    invalid_config, read_yaml_file, require_non_empty, require_non_empty_path, with_field_context,
    EvaError,
};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load and normalize the main Eva YAML configuration";

const CONFIG_TYPE: &str = "eva.yaml";

/// Project-level Eva runtime configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaConfig {
    /// Path to the source file.
    pub path: PathBuf,
    /// Runtime path and hot-reload settings.
    pub runtime: RuntimeConfig,
    /// Logging, tracing, metrics, audit, and exporter settings.
    pub observability: ObservabilityConfig,
    /// Optional service-manager integration settings.
    pub service_manager: Option<ServiceManagerConfig>,
    /// Split configuration roots.
    pub config: ConfigRoots,
    /// Additional top-level objects owned by downstream crates.
    pub extra: Mapping,
}

/// Stable subset of the `runtime` object used during configuration loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub env: String,
    pub workspace: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub script_dir: Option<PathBuf>,
    pub adapter_dir: Option<PathBuf>,
    pub hot_reload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerConfig {
    pub enabled: bool,
    pub kind: ServiceManagerKind,
    pub service_name: String,
    pub unit_name: Option<String>,
    pub runtime_binary: Option<PathBuf>,
    pub candidate_runtime_binary: Option<PathBuf>,
    pub start_on_boot: bool,
    pub restart_supervisor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityConfig {
    pub log_level: String,
    pub tracing: bool,
    pub metrics: bool,
    pub audit: bool,
    pub otel_endpoint_env: Option<String>,
    pub otel_exporter: Option<OpenTelemetryExporterConfig>,
    pub retention: Option<ObservabilityRetentionConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTelemetryExporterConfig {
    pub endpoint: Option<String>,
    pub auth_header_env: Option<String>,
    pub batch_size: usize,
    pub timeout_ms: u64,
    pub drop_policy: OpenTelemetryDropPolicy,
    pub max_metric_labels: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTelemetryDropPolicy {
    DropNew,
    DropOldest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityRetentionConfig {
    pub sink: ObservabilityRetentionSink,
    pub max_file_bytes: u64,
    pub max_rotated_files: usize,
    pub retain_for_ms: u64,
    pub corrupt_record_policy: ObservabilityCorruptRecordPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityRetentionSink {
    JsonlFile,
    DurableAudit,
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityCorruptRecordPolicy {
    SkipAndReport,
    FailFast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerKind {
    Fake,
    WindowsService,
    Systemd,
    Launchd,
}

/// Paths to split configuration files and directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRoots {
    pub agent_dir: PathBuf,
    pub adapter_dir: PathBuf,
    pub capability_dir: PathBuf,
    pub policy_dir: PathBuf,
    pub route_file: PathBuf,
    pub schema_dir: PathBuf,
}

/// Loads and validates the main `eva.yaml` file.
pub fn load_eva_config(path: impl AsRef<Path>) -> Result<EvaConfig, EvaError> {
    let path = path.as_ref();
    let raw: RawEvaConfig = read_yaml_file(path, CONFIG_TYPE)?;
    EvaConfig::try_from_raw(path.to_path_buf(), raw)
}

impl EvaConfig {
    fn try_from_raw(path: PathBuf, raw: RawEvaConfig) -> Result<Self, EvaError> {
        let runtime = RuntimeConfig::try_from_raw(&path, raw.runtime)?;
        let observability = ObservabilityConfig::try_from_raw(&path, raw.observability)?;
        let service_manager = raw
            .service_manager
            .map(|config| ServiceManagerConfig::try_from_raw(&path, config))
            .transpose()?;
        let config = ConfigRoots::try_from_raw(&path, raw.config)?;
        Ok(Self {
            path,
            runtime,
            observability,
            service_manager,
            config,
            extra: raw.extra,
        })
    }
}

impl RuntimeConfig {
    fn try_from_raw(path: &Path, raw: RawRuntimeConfig) -> Result<Self, EvaError> {
        let env = require_non_empty(raw.env, CONFIG_TYPE, path, "runtime.env")?;
        let workspace =
            require_non_empty_path(raw.workspace, CONFIG_TYPE, path, "runtime.workspace")?;

        Ok(Self {
            env,
            workspace,
            data_dir: raw.data_dir,
            script_dir: raw.script_dir,
            adapter_dir: raw.adapter_dir,
            hot_reload: raw.hot_reload,
        })
    }
}

impl ObservabilityConfig {
    fn try_from_raw(path: &Path, raw: RawObservabilityConfig) -> Result<Self, EvaError> {
        let log_level = raw.log_level.unwrap_or_else(|| "info".to_owned());
        if log_level.trim().is_empty() || log_level.trim() != log_level {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.log_level",
                "log level cannot be empty or contain leading/trailing whitespace",
            ));
        }
        let otel_endpoint_env = raw
            .otel_endpoint_env
            .map(|value| {
                require_non_empty(value, CONFIG_TYPE, path, "observability.otel_endpoint_env")
            })
            .transpose()?;
        let otel_exporter = raw
            .otel_exporter
            .map(|config| OpenTelemetryExporterConfig::try_from_raw(path, config))
            .transpose()?;
        let retention = raw
            .retention
            .map(|config| ObservabilityRetentionConfig::try_from_raw(path, config))
            .transpose()?;

        Ok(Self {
            log_level,
            tracing: raw.tracing.unwrap_or(true),
            metrics: raw.metrics.unwrap_or(true),
            audit: raw.audit.unwrap_or(true),
            otel_endpoint_env,
            otel_exporter,
            retention,
        })
    }
}

impl OpenTelemetryExporterConfig {
    fn try_from_raw(path: &Path, raw: RawOpenTelemetryExporterConfig) -> Result<Self, EvaError> {
        let endpoint = raw
            .endpoint
            .map(|value| {
                require_non_empty(
                    value,
                    CONFIG_TYPE,
                    path,
                    "observability.otel_exporter.endpoint",
                )
            })
            .transpose()?;
        let auth_header_env = raw
            .auth_header_env
            .map(|value| {
                require_non_empty(
                    value,
                    CONFIG_TYPE,
                    path,
                    "observability.otel_exporter.auth_header_env",
                )
            })
            .transpose()?;
        let batch_size = raw.batch_size.unwrap_or(32);
        if batch_size == 0 {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.otel_exporter.batch_size",
                "OpenTelemetry exporter batch size must be greater than zero",
            ));
        }
        let timeout_ms = raw.timeout_ms.unwrap_or(5_000);
        if timeout_ms == 0 {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.otel_exporter.timeout_ms",
                "OpenTelemetry exporter timeout must be greater than zero",
            ));
        }
        let max_metric_labels = raw.max_metric_labels.unwrap_or(8);
        if max_metric_labels == 0 {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.otel_exporter.max_metric_labels",
                "OpenTelemetry exporter metric label limit must be greater than zero",
            ));
        }
        let drop_policy = raw
            .drop_policy
            .as_deref()
            .map(OpenTelemetryDropPolicy::parse)
            .transpose()
            .map_err(|error| {
                with_field_context(
                    error,
                    CONFIG_TYPE,
                    path,
                    "observability.otel_exporter.drop_policy",
                )
            })?
            .unwrap_or(OpenTelemetryDropPolicy::DropNew);

        Ok(Self {
            endpoint,
            auth_header_env,
            batch_size,
            timeout_ms,
            drop_policy,
            max_metric_labels,
        })
    }
}

impl OpenTelemetryDropPolicy {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "drop-new" | "drop_new" => Ok(Self::DropNew),
            "drop-oldest" | "drop_oldest" => Ok(Self::DropOldest),
            _ => Err(
                EvaError::invalid_argument("unsupported OpenTelemetry exporter drop policy")
                    .with_context("drop_policy", value)
                    .with_context("expected", "drop-new|drop-oldest"),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DropNew => "drop-new",
            Self::DropOldest => "drop-oldest",
        }
    }
}

impl ObservabilityRetentionConfig {
    fn try_from_raw(path: &Path, raw: RawObservabilityRetentionConfig) -> Result<Self, EvaError> {
        let sink = raw
            .sink
            .as_deref()
            .map(ObservabilityRetentionSink::parse)
            .transpose()
            .map_err(|error| {
                with_field_context(error, CONFIG_TYPE, path, "observability.retention.sink")
            })?
            .unwrap_or(ObservabilityRetentionSink::JsonlFile);
        let max_file_bytes = raw.max_file_bytes.unwrap_or(8 * 1024 * 1024);
        if max_file_bytes == 0 {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.retention.max_file_bytes",
                "observability retention max_file_bytes must be greater than zero",
            ));
        }
        let max_rotated_files = raw.max_rotated_files.unwrap_or(16);
        if max_rotated_files == 0 {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.retention.max_rotated_files",
                "observability retention max_rotated_files must be greater than zero",
            ));
        }
        let retain_for_ms = raw.retain_for_ms.unwrap_or(7 * 24 * 60 * 60 * 1000);
        if retain_for_ms == 0 {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "observability.retention.retain_for_ms",
                "observability retention retain_for_ms must be greater than zero",
            ));
        }
        let corrupt_record_policy = raw
            .corrupt_record_policy
            .as_deref()
            .map(ObservabilityCorruptRecordPolicy::parse)
            .transpose()
            .map_err(|error| {
                with_field_context(
                    error,
                    CONFIG_TYPE,
                    path,
                    "observability.retention.corrupt_record_policy",
                )
            })?
            .unwrap_or(ObservabilityCorruptRecordPolicy::SkipAndReport);

        Ok(Self {
            sink,
            max_file_bytes,
            max_rotated_files,
            retain_for_ms,
            corrupt_record_policy,
        })
    }
}

impl ObservabilityRetentionSink {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "jsonl" | "jsonl-file" | "jsonl_file" => Ok(Self::JsonlFile),
            "durable-audit" | "durable_audit" => Ok(Self::DurableAudit),
            "database" | "db" => Ok(Self::Database),
            _ => Err(
                EvaError::invalid_argument("unsupported observability retention sink")
                    .with_context("sink", value)
                    .with_context("expected", "jsonl-file|durable-audit|database"),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonlFile => "jsonl-file",
            Self::DurableAudit => "durable-audit",
            Self::Database => "database",
        }
    }
}

impl ObservabilityCorruptRecordPolicy {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "skip-and-report" | "skip_and_report" => Ok(Self::SkipAndReport),
            "fail-fast" | "fail_fast" => Ok(Self::FailFast),
            _ => Err(
                EvaError::invalid_argument("unsupported observability corrupt record policy")
                    .with_context("corrupt_record_policy", value)
                    .with_context("expected", "skip-and-report|fail-fast"),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkipAndReport => "skip-and-report",
            Self::FailFast => "fail-fast",
        }
    }
}

impl ServiceManagerConfig {
    fn try_from_raw(path: &Path, raw: RawServiceManagerConfig) -> Result<Self, EvaError> {
        let enabled = raw.enabled;
        let kind = if let Some(kind) = raw.kind {
            ServiceManagerKind::parse(&kind).map_err(|error| {
                with_field_context(error, CONFIG_TYPE, path, "service_manager.kind")
            })?
        } else if enabled {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "service_manager.kind",
                "enabled service manager requires kind",
            ));
        } else {
            ServiceManagerKind::Fake
        };

        let service_name = if let Some(service_name) = raw.service_name {
            require_non_empty(
                service_name,
                CONFIG_TYPE,
                path,
                "service_manager.service_name",
            )?
        } else if enabled {
            return Err(invalid_config(
                CONFIG_TYPE,
                path,
                "service_manager.service_name",
                "enabled service manager requires service_name",
            ));
        } else {
            "disabled".to_owned()
        };

        let unit_name = raw
            .unit_name
            .map(|value| require_non_empty(value, CONFIG_TYPE, path, "service_manager.unit_name"))
            .transpose()?;

        Ok(Self {
            enabled,
            kind,
            service_name,
            unit_name,
            runtime_binary: raw.runtime_binary,
            candidate_runtime_binary: raw.candidate_runtime_binary,
            start_on_boot: raw.start_on_boot.unwrap_or(false),
            restart_supervisor: raw.restart_supervisor.unwrap_or(false),
        })
    }
}

impl ServiceManagerKind {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "fake" => Ok(Self::Fake),
            "windows_service" | "windows-service" | "windows" => Ok(Self::WindowsService),
            "systemd" => Ok(Self::Systemd),
            "launchd" => Ok(Self::Launchd),
            _ => Err(
                EvaError::invalid_argument("unsupported service manager kind")
                    .with_context("kind", value),
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::WindowsService => "windows_service",
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
        }
    }

    pub fn production_adapter(self) -> bool {
        !matches!(self, Self::Fake)
    }
}

impl ConfigRoots {
    fn try_from_raw(path: &Path, raw: RawConfigRoots) -> Result<Self, EvaError> {
        Ok(Self {
            agent_dir: require_non_empty_path(
                raw.agent_dir,
                CONFIG_TYPE,
                path,
                "config.agent_dir",
            )?,
            adapter_dir: require_non_empty_path(
                raw.adapter_dir,
                CONFIG_TYPE,
                path,
                "config.adapter_dir",
            )?,
            capability_dir: require_non_empty_path(
                raw.capability_dir,
                CONFIG_TYPE,
                path,
                "config.capability_dir",
            )?,
            policy_dir: require_non_empty_path(
                raw.policy_dir,
                CONFIG_TYPE,
                path,
                "config.policy_dir",
            )?,
            route_file: require_non_empty_path(
                raw.route_file,
                CONFIG_TYPE,
                path,
                "config.route_file",
            )?,
            schema_dir: require_non_empty_path(
                raw.schema_dir,
                CONFIG_TYPE,
                path,
                "config.schema_dir",
            )?,
        })
    }

    /// Resolves relative config roots against a project root.
    pub fn resolve_against(&self, project_root: &Path) -> Self {
        Self {
            agent_dir: resolve_path(project_root, &self.agent_dir),
            adapter_dir: resolve_path(project_root, &self.adapter_dir),
            capability_dir: resolve_path(project_root, &self.capability_dir),
            policy_dir: resolve_path(project_root, &self.policy_dir),
            route_file: resolve_path(project_root, &self.route_file),
            schema_dir: resolve_path(project_root, &self.schema_dir),
        }
    }
}

fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[derive(Debug, Deserialize)]
struct RawEvaConfig {
    runtime: RawRuntimeConfig,
    #[serde(default)]
    observability: RawObservabilityConfig,
    #[serde(default)]
    service_manager: Option<RawServiceManagerConfig>,
    config: RawConfigRoots,
    #[serde(flatten)]
    extra: Mapping,
}

#[derive(Debug, Deserialize)]
struct RawRuntimeConfig {
    env: String,
    workspace: PathBuf,
    data_dir: Option<PathBuf>,
    script_dir: Option<PathBuf>,
    adapter_dir: Option<PathBuf>,
    hot_reload: bool,
}

#[derive(Debug, Deserialize)]
struct RawServiceManagerConfig {
    enabled: bool,
    kind: Option<String>,
    service_name: Option<String>,
    unit_name: Option<String>,
    runtime_binary: Option<PathBuf>,
    candidate_runtime_binary: Option<PathBuf>,
    start_on_boot: Option<bool>,
    restart_supervisor: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawObservabilityConfig {
    log_level: Option<String>,
    tracing: Option<bool>,
    metrics: Option<bool>,
    audit: Option<bool>,
    otel_endpoint_env: Option<String>,
    otel_exporter: Option<RawOpenTelemetryExporterConfig>,
    retention: Option<RawObservabilityRetentionConfig>,
}

#[derive(Debug, Deserialize)]
struct RawOpenTelemetryExporterConfig {
    endpoint: Option<String>,
    auth_header_env: Option<String>,
    batch_size: Option<usize>,
    timeout_ms: Option<u64>,
    drop_policy: Option<String>,
    max_metric_labels: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RawObservabilityRetentionConfig {
    sink: Option<String>,
    max_file_bytes: Option<u64>,
    max_rotated_files: Option<usize>,
    retain_for_ms: Option<u64>,
    corrupt_record_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawConfigRoots {
    agent_dir: PathBuf,
    adapter_dir: PathBuf,
    capability_dir: PathBuf,
    policy_dir: PathBuf,
    route_file: PathBuf,
    schema_dir: PathBuf,
}

impl TryFrom<Value> for EvaConfig {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawEvaConfig::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse eva.yaml")
                .with_context("yaml_error", error.to_string())
        })?;
        Self::try_from_raw(PathBuf::new(), raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use serde_yaml::Value;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn load_eva_config_accepts_sample_config() {
        let config = load_eva_config(workspace_root().join("config").join("eva.yaml")).unwrap();

        assert_eq!(config.runtime.env, "dev");
        assert_eq!(config.observability.log_level, "info");
        assert_eq!(
            config
                .observability
                .otel_exporter
                .as_ref()
                .unwrap()
                .drop_policy
                .as_str(),
            "drop-new"
        );
        assert_eq!(
            config
                .observability
                .retention
                .as_ref()
                .unwrap()
                .corrupt_record_policy
                .as_str(),
            "skip-and-report"
        );
        let service_manager = config
            .service_manager
            .as_ref()
            .expect("sample config should declare service manager boundary");
        assert!(service_manager.enabled);
        assert_eq!(service_manager.kind, ServiceManagerKind::Fake);
        assert_eq!(service_manager.service_name, "eva-dev");
        assert_eq!(config.config.agent_dir, PathBuf::from("config/agents"));
        assert!(config
            .extra
            .contains_key(Value::String("eventbus".to_owned())));
    }

    #[test]
    fn load_eva_config_rejects_missing_required_runtime() {
        let value = serde_yaml::from_str::<Value>(
            r#"
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn service_manager_requires_kind_and_name_when_enabled() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
service_manager:
  enabled: true
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "field" && value == "service_manager.kind"));
    }

    #[test]
    fn service_manager_rejects_unknown_kind() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
service_manager:
  enabled: true
  kind: smf
  service_name: eva-prod
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "kind" && value == "smf"));
    }

    #[test]
    fn observability_exporter_rejects_invalid_limits() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
observability:
  otel_exporter:
    endpoint: http://localhost:4318
    batch_size: 0
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error.context().entries().iter().any(|(key, value)| {
            key == "field" && value == "observability.otel_exporter.batch_size"
        }));
    }

    #[test]
    fn observability_exporter_rejects_unknown_drop_policy() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
observability:
  otel_exporter:
    endpoint: http://localhost:4318
    drop_policy: random
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error.context().entries().iter().any(|(key, value)| {
            key == "field" && value == "observability.otel_exporter.drop_policy"
        }));
    }

    #[test]
    fn observability_retention_rejects_invalid_limits() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
observability:
  retention:
    sink: jsonl-file
    retain_for_ms: 0
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(error.context().entries().iter().any(|(key, value)| {
            key == "field" && value == "observability.retention.retain_for_ms"
        }));
    }

    #[test]
    fn config_roots_resolve_relative_paths() {
        let config = load_eva_config(workspace_root().join("config").join("eva.yaml")).unwrap();
        let roots = config.config.resolve_against(Path::new("C:/workspace"));

        assert_eq!(roots.agent_dir, PathBuf::from("C:/workspace/config/agents"));
        assert_eq!(
            roots.route_file,
            PathBuf::from("C:/workspace/config/routes/topics.yaml")
        );
    }

    #[test]
    fn load_eva_config_rejects_blank_env() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: ""
  workspace: .
  hot_reload: true
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

        let error = EvaConfig::try_from(value).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "field")
                .unwrap()
                .1,
            "runtime.env"
        );
    }
}
