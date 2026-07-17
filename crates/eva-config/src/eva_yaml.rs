//! 主 `eva.yaml` 的加载、默认值应用与规范化。
//! Main `eva.yaml` loading and normalization.

use crate::{
    invalid_config, read_yaml_file, require_non_empty, require_non_empty_path, with_field_context,
    EvaError,
};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// 本模块的架构职责：加载主 Eva YAML，并在交叉引用校验前建立强类型配置边界。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load and normalize the main Eva YAML configuration";

/// 错误上下文中使用的主配置类型名称。
const CONFIG_TYPE: &str = "eva.yaml";

/// 项目级 Eva 运行时配置。
/// Project-level Eva runtime configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaConfig {
    /// 主配置源文件路径。
    /// Path to the source file.
    pub path: PathBuf,
    /// 运行时路径和热重载设置。
    /// Runtime path and hot-reload settings.
    pub runtime: RuntimeConfig,
    /// 日志、追踪、指标、审计和导出设置。
    /// Logging, tracing, metrics, audit, and exporter settings.
    pub observability: ObservabilityConfig,
    /// 可选服务管理器集成设置。
    /// Optional service-manager integration settings.
    pub service_manager: Option<ServiceManagerConfig>,
    /// 拆分配置文件和目录的根路径。
    /// Split configuration roots.
    pub config: ConfigRoots,
    /// 由下游 crate 解释的额外顶层对象。
    /// Additional top-level objects owned by downstream crates.
    pub extra: Mapping,
}

/// 配置加载阶段使用的稳定 `runtime` 对象子集。
/// Stable subset of the `runtime` object used during configuration loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// 环境名称，必须为非空且已裁剪文本。
    pub env: String,
    /// 运行时工作区路径。
    pub workspace: PathBuf,
    /// 可选持久数据目录。
    pub data_dir: Option<PathBuf>,
    /// 可选 Lua 或其他脚本目录。
    pub script_dir: Option<PathBuf>,
    /// 可选运行时 Adapter 目录覆盖值。
    pub adapter_dir: Option<PathBuf>,
    /// 是否允许配置热重载。
    pub hot_reload: bool,
    /// Optional daemon-owned external knowledge retrieval worker.
    pub retrieval_worker: Option<KnowledgeRetrievalWorkerConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeRetrievalWorkerConfig {
    pub agent: String,
    pub capability: String,
    pub provider: String,
    pub query: String,
    pub interval_ms: u64,
}

/// 操作系统服务管理器的可选项目配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerConfig {
    /// 是否启用服务管理器集成。
    pub enabled: bool,
    /// 目标服务管理器类别。
    pub kind: ServiceManagerKind,
    /// 服务管理器中的稳定服务名称。
    pub service_name: String,
    /// 平台特定的可选 unit 名称。
    pub unit_name: Option<String>,
    /// 当前运行时二进制路径。
    pub runtime_binary: Option<PathBuf>,
    /// 候选运行时二进制路径。
    pub candidate_runtime_binary: Option<PathBuf>,
    /// 是否配置为随系统启动。
    pub start_on_boot: bool,
    /// 交接时是否重启 Supervisor。
    pub restart_supervisor: bool,
}

/// 主配置中的可观测性开关、导出和保留策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityConfig {
    /// 日志级别文本。
    pub log_level: String,
    /// 是否启用追踪。
    pub tracing: bool,
    /// 是否启用指标。
    pub metrics: bool,
    /// 是否启用审计事件。
    pub audit: bool,
    /// 旧式 OpenTelemetry 端点环境变量名。
    pub otel_endpoint_env: Option<String>,
    /// 结构化 OpenTelemetry 导出器设置。
    pub otel_exporter: Option<OpenTelemetryExporterConfig>,
    /// 可观测性记录保留设置。
    pub retention: Option<ObservabilityRetentionConfig>,
}

/// OpenTelemetry 批量导出与背压配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTelemetryExporterConfig {
    /// 可选固定 OTLP 端点。
    pub endpoint: Option<String>,
    /// 保存认证请求头的环境变量名，而非 secret 本身。
    pub auth_header_env: Option<String>,
    /// 单批最多导出的记录数。
    pub batch_size: usize,
    /// 单次导出超时毫秒数。
    pub timeout_ms: u64,
    /// 队列达到容量时的丢弃策略。
    pub drop_policy: OpenTelemetryDropPolicy,
    /// 每个指标允许的最大标签数。
    pub max_metric_labels: usize,
}

/// OpenTelemetry 导出队列满载时的记录丢弃策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTelemetryDropPolicy {
    /// 保留队列现有记录并丢弃新记录。
    DropNew,
    /// 丢弃最旧记录为新记录腾出空间。
    DropOldest,
}

/// 可观测性记录的落点、轮转周期和损坏处理配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityRetentionConfig {
    /// 记录持久化落点类别。
    pub sink: ObservabilityRetentionSink,
    /// 单个文件轮转前的最大字节数。
    pub max_file_bytes: u64,
    /// 最多保留的轮转文件数量。
    pub max_rotated_files: usize,
    /// 记录最长保留毫秒数。
    pub retain_for_ms: u64,
    /// 读取损坏记录时采用的处理策略。
    pub corrupt_record_policy: ObservabilityCorruptRecordPolicy,
}

/// 可观测性记录的持久化落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityRetentionSink {
    /// 本地 JSON Lines 文件。
    JsonlFile,
    /// Eva 持久审计后端。
    DurableAudit,
    /// 预留数据库后端标识。
    Database,
}

/// 读取损坏可观测性记录时的策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityCorruptRecordPolicy {
    /// 跳过损坏项并生成诊断。
    SkipAndReport,
    /// 首个损坏项立即使读取失败。
    FailFast,
}

/// 支持配置的服务管理器类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerKind {
    /// 仅用于开发和测试的内存实现。
    Fake,
    /// Windows 服务控制管理器。
    WindowsService,
    /// Linux systemd 服务管理器。
    Systemd,
    /// macOS launchd 服务管理器。
    Launchd,
}

/// 拆分配置文件和目录的位置。
/// Paths to split configuration files and directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRoots {
    /// Agent 清单目录。
    pub agent_dir: PathBuf,
    /// Adapter 清单目录。
    pub adapter_dir: PathBuf,
    /// Capability 清单目录。
    pub capability_dir: PathBuf,
    /// 策略配置目录。
    pub policy_dir: PathBuf,
    /// 路由配置文件。
    pub route_file: PathBuf,
    /// JSON/YAML Schema 目录。
    pub schema_dir: PathBuf,
}

/// 加载并验证主 `eva.yaml` 文件。
/// Loads and validates the main `eva.yaml` file.
pub fn load_eva_config(path: impl AsRef<Path>) -> Result<EvaConfig, EvaError> {
    let path = path.as_ref();
    let raw: RawEvaConfig = read_yaml_file(path, CONFIG_TYPE)?;
    EvaConfig::try_from_raw(path.to_path_buf(), raw)
}

impl EvaConfig {
    /// 按 runtime、observability、service_manager、config 顺序规范化原始配置。
    ///
    /// 每个分区独立附加精确字段上下文；可选分区使用 transpose 传播解析错误而非把
    /// 错误误当作缺失。未知顶层对象保持在 extra 中供下游所有者解释。
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
    /// 校验必填环境和工作区，保留可选路径与热重载开关。
    fn try_from_raw(path: &Path, raw: RawRuntimeConfig) -> Result<Self, EvaError> {
        let env = require_non_empty(raw.env, CONFIG_TYPE, path, "runtime.env")?;
        let workspace =
            require_non_empty_path(raw.workspace, CONFIG_TYPE, path, "runtime.workspace")?;

        let retrieval_worker = raw
            .retrieval_worker
            .map(|config| KnowledgeRetrievalWorkerConfig::try_from_raw(path, config))
            .transpose()?
            .flatten();
        Ok(Self {
            env,
            workspace,
            data_dir: raw.data_dir,
            script_dir: raw.script_dir,
            adapter_dir: raw.adapter_dir,
            hot_reload: raw.hot_reload,
            retrieval_worker,
        })
    }
}

impl KnowledgeRetrievalWorkerConfig {
    fn try_from_raw(
        path: &Path,
        raw: RawKnowledgeRetrievalWorkerConfig,
    ) -> Result<Option<Self>, EvaError> {
        if !raw.enabled {
            if raw.agent.is_some()
                || raw.capability.is_some()
                || raw.provider.is_some()
                || raw.query.is_some()
                || raw.interval_ms.is_some()
            {
                return Err(invalid_config(
                    CONFIG_TYPE,
                    path,
                    "runtime.retrieval_worker",
                    "disabled retrieval worker cannot carry execution configuration",
                ));
            }
            return Ok(None);
        }
        let required = |value: Option<String>, field: &'static str| {
            value
                .ok_or_else(|| invalid_config(CONFIG_TYPE, path, field, "field is required"))
                .and_then(|value| require_non_empty(value, CONFIG_TYPE, path, field))
        };
        let interval_ms = raw.interval_ms.filter(|value| *value > 0).ok_or_else(|| {
            invalid_config(
                CONFIG_TYPE,
                path,
                "runtime.retrieval_worker.interval_ms",
                "interval must be positive",
            )
        })?;
        Ok(Some(Self {
            agent: required(raw.agent, "runtime.retrieval_worker.agent")?,
            capability: required(raw.capability, "runtime.retrieval_worker.capability")?,
            provider: required(raw.provider, "runtime.retrieval_worker.provider")?,
            query: required(raw.query, "runtime.retrieval_worker.query")?,
            interval_ms,
        }))
    }
}

impl ObservabilityConfig {
    /// 应用向后兼容默认值并验证可观测性配置。
    ///
    /// 旧配置缺少开关时默认启用 tracing/metrics/audit；结构化 exporter 和 retention
    /// 缺失时保持 None，不凭空声明外部后端。存在但非法的可选分区仍失败关闭。
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
    /// 校验端点引用和所有非零资源限制，并应用保守丢新默认策略。
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
    /// 解析连字符稳定拼写及下划线兼容拼写。
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

    /// 返回规范化清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DropNew => "drop-new",
            Self::DropOldest => "drop-oldest",
        }
    }
}

impl ObservabilityRetentionConfig {
    /// 应用保留策略默认值并拒绝所有零容量、零数量和零周期限制。
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
    /// 解析落点稳定拼写和兼容别名。
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

    /// 返回规范化清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonlFile => "jsonl-file",
            Self::DurableAudit => "durable-audit",
            Self::Database => "database",
        }
    }
}

impl ObservabilityCorruptRecordPolicy {
    /// 解析损坏记录策略及下划线兼容拼写。
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

    /// 返回规范化清单拼写。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkipAndReport => "skip-and-report",
            Self::FailFast => "fail-fast",
        }
    }
}

impl ServiceManagerConfig {
    /// 校验服务管理器启用条件并应用禁用状态默认值。
    ///
    /// 启用时 kind 和 service_name 均为必填；禁用时缺失 kind/name 会规范化为 Fake 和
    /// `disabled`，保持旧配置可读，但不会由此启用生产服务管理器。
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
    /// 解析服务管理器类别及 Windows 兼容别名。
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

    /// 返回规范化配置拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::WindowsService => "windows_service",
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
        }
    }

    /// 判断该类别是否代表真实平台适配器。
    pub fn production_adapter(self) -> bool {
        !matches!(self, Self::Fake)
    }
}

impl ConfigRoots {
    /// 校验所有拆分配置根路径非空。
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

    /// 将所有相对配置路径解析到项目根，绝对路径保持不变。
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

/// 相对路径与项目根拼接，绝对路径原样保留。
fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// 主配置尚未经过语义验证的反序列化结构。
#[derive(Debug, Deserialize)]
struct RawEvaConfig {
    /// 必填运行时分区。
    runtime: RawRuntimeConfig,
    /// 缺失时使用兼容默认值的可观测性分区。
    #[serde(default)]
    observability: RawObservabilityConfig,
    /// 可选服务管理器分区。
    #[serde(default)]
    service_manager: Option<RawServiceManagerConfig>,
    /// 必填拆分配置根路径。
    config: RawConfigRoots,
    /// 下游所有者使用的未知顶层扩展字段。
    #[serde(flatten)]
    extra: Mapping,
}

/// 原始运行时分区。
#[derive(Debug, Deserialize)]
struct RawRuntimeConfig {
    /// 环境名称。
    env: String,
    /// 工作区路径。
    workspace: PathBuf,
    /// 可选数据目录。
    data_dir: Option<PathBuf>,
    /// 可选脚本目录。
    script_dir: Option<PathBuf>,
    /// 可选 Adapter 目录。
    adapter_dir: Option<PathBuf>,
    /// 热重载开关。
    hot_reload: bool,
    #[serde(default)]
    retrieval_worker: Option<RawKnowledgeRetrievalWorkerConfig>,
}

#[derive(Debug, Deserialize)]
struct RawKnowledgeRetrievalWorkerConfig {
    #[serde(default)]
    enabled: bool,
    agent: Option<String>,
    capability: Option<String>,
    provider: Option<String>,
    query: Option<String>,
    interval_ms: Option<u64>,
}

/// 原始服务管理器分区。
#[derive(Debug, Deserialize)]
struct RawServiceManagerConfig {
    /// 是否启用。
    enabled: bool,
    /// 可选类别拼写。
    kind: Option<String>,
    /// 可选服务名称。
    service_name: Option<String>,
    /// 可选平台 unit 名称。
    unit_name: Option<String>,
    /// 可选当前二进制路径。
    runtime_binary: Option<PathBuf>,
    /// 可选候选二进制路径。
    candidate_runtime_binary: Option<PathBuf>,
    /// 可选开机启动开关。
    start_on_boot: Option<bool>,
    /// 可选 Supervisor 重启开关。
    restart_supervisor: Option<bool>,
}

/// 原始可观测性分区。
#[derive(Debug, Default, Deserialize)]
struct RawObservabilityConfig {
    /// 可选日志级别。
    log_level: Option<String>,
    /// 可选追踪开关。
    tracing: Option<bool>,
    /// 可选指标开关。
    metrics: Option<bool>,
    /// 可选审计开关。
    audit: Option<bool>,
    /// 可选旧式端点环境变量名。
    otel_endpoint_env: Option<String>,
    /// 可选结构化导出器配置。
    otel_exporter: Option<RawOpenTelemetryExporterConfig>,
    /// 可选保留策略。
    retention: Option<RawObservabilityRetentionConfig>,
}

/// 原始 OpenTelemetry 导出器配置。
#[derive(Debug, Deserialize)]
struct RawOpenTelemetryExporterConfig {
    /// 可选固定端点。
    endpoint: Option<String>,
    /// 可选认证环境变量名。
    auth_header_env: Option<String>,
    /// 可选批大小。
    batch_size: Option<usize>,
    /// 可选超时毫秒数。
    timeout_ms: Option<u64>,
    /// 可选丢弃策略拼写。
    drop_policy: Option<String>,
    /// 可选指标标签上限。
    max_metric_labels: Option<usize>,
}

/// 原始可观测性保留配置。
#[derive(Debug, Deserialize)]
struct RawObservabilityRetentionConfig {
    /// 可选落点拼写。
    sink: Option<String>,
    /// 可选单文件字节上限。
    max_file_bytes: Option<u64>,
    /// 可选轮转文件上限。
    max_rotated_files: Option<usize>,
    /// 可选保留周期。
    retain_for_ms: Option<u64>,
    /// 可选损坏记录策略拼写。
    corrupt_record_policy: Option<String>,
}

/// 原始拆分配置根路径。
#[derive(Debug, Deserialize)]
struct RawConfigRoots {
    /// Agent 目录。
    agent_dir: PathBuf,
    /// Adapter 目录。
    adapter_dir: PathBuf,
    /// Capability 目录。
    capability_dir: PathBuf,
    /// Policy 目录。
    policy_dir: PathBuf,
    /// Route 文件。
    route_file: PathBuf,
    /// Schema 目录。
    schema_dir: PathBuf,
}

impl TryFrom<Value> for EvaConfig {
    /// 值转换失败时使用的统一配置错误类型。
    type Error = EvaError;

    /// 从内存 YAML 值解析主配置，供测试与组合加载复用。
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawEvaConfig::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse eva.yaml")
                .with_context("yaml_error", error.to_string())
        })?;
        Self::try_from_raw(PathBuf::new(), raw)
    }
}

#[cfg(test)]
/// 主配置默认值、字段约束、服务管理器和路径解析测试。
mod tests {
    use super::*;
    use eva_core::ErrorKind;
    use serde_yaml::Value;

    /// 返回包含示例配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证示例主配置及可观测性默认值可加载。
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
        assert!(config.runtime.retrieval_worker.is_none());
    }

    #[test]
    fn retrieval_worker_requires_complete_enabled_configuration() {
        let value = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
  retrieval_worker:
    enabled: true
    agent: root-agent
    capability: knowledge.retrieve
    provider: retrieval-provider
    query: refresh project knowledge
    interval_ms: 60000
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
        let config = EvaConfig::try_from(value).unwrap();
        let worker = config.runtime.retrieval_worker.unwrap();
        assert_eq!(worker.agent, "root-agent");
        assert_eq!(worker.interval_ms, 60_000);

        let invalid = serde_yaml::from_str::<Value>(
            r#"
runtime:
  env: dev
  workspace: .
  hot_reload: true
  retrieval_worker:
    enabled: true
    agent: root-agent
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
        assert!(EvaConfig::try_from(invalid).is_err());
    }

    #[test]
    /// 验证缺失必填 runtime 分区会失败。
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
    /// 验证启用服务管理器时必须同时提供类别和名称。
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
    /// 验证未知服务管理器类别失败关闭。
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
    /// 验证 OpenTelemetry 的零资源限制被拒绝。
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
    /// 验证未知导出队列丢弃策略被拒绝。
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
    /// 验证可观测性保留策略的零限制被拒绝。
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
    /// 验证相对配置根统一解析到项目根目录。
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
    /// 验证空白环境名称附带字段上下文并失败。
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
