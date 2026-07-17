//! 操作系统服务管理器的抽象边界。
//! OS service-manager abstraction boundary.

use crate::{RuntimeHealth, UpgradeApplyPlan};
use eva_config::ServiceManagerConfig;
use eva_core::EvaError;
use std::path::PathBuf;

pub use eva_config::ServiceManagerKind;

/// 本模块的架构职责：定义服务管理器适配器、模拟交接及回滚证据边界。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "OS service-manager typed lifecycle, fake state, handoff, and rollback evidence boundary";

/// 项目中声明的服务管理器配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerDefinition {
    /// 是否允许使用该服务管理器配置。
    pub enabled: bool,
    /// 目标服务管理器类别。
    pub kind: ServiceManagerKind,
    /// 服务管理器中的稳定服务名称。
    pub service_name: String,
    /// systemd、launchd 等平台使用的可选单元名称。
    pub unit_name: Option<String>,
    /// 当前活动运行时二进制路径。
    pub runtime_binary: Option<PathBuf>,
    /// 候选运行时二进制路径。
    pub candidate_runtime_binary: Option<PathBuf>,
    /// 是否配置为随系统启动。
    pub start_on_boot: bool,
    /// 交接时是否重启 Supervisor。
    pub restart_supervisor: bool,
}

/// Typed service state shared by status and mutation reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerState {
    /// No service definition is installed.
    NotInstalled,
    /// The service is installed but not running.
    Stopped,
    /// The service is installed and running.
    Running,
}

/// Mutating operations supported by every service-manager adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerOperation {
    /// Install the service definition.
    Install,
    /// Remove the service definition.
    Uninstall,
    /// Start an installed service.
    Start,
    /// Stop an installed service.
    Stop,
    /// Restart an installed service.
    Restart,
}

/// Stable evidence returned by one service-manager mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerMutationReport {
    /// Adapter kind that evaluated the request.
    pub kind: ServiceManagerKind,
    /// Stable service name from the validated definition.
    pub service_name: String,
    /// Requested mutation.
    pub operation: ServiceManagerOperation,
    /// State after the operation completed or was found to be unnecessary.
    pub state: ServiceManagerState,
    /// Whether this call changed adapter state.
    pub mutation_executed: bool,
    /// Whether the report came from a real host adapter.
    pub production_adapter: bool,
    /// Ordered, secret-free operation evidence.
    pub audit: Vec<String>,
}

/// 服务管理器当前配置和代际状态的检查报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerStatusReport {
    /// 被检查的服务管理器类别。
    pub kind: ServiceManagerKind,
    /// 被检查的服务名称。
    pub service_name: String,
    /// 服务配置是否启用。
    pub configured: bool,
    /// 是否为真实平台适配器而非模拟实现。
    pub production_adapter: bool,
    /// Typed installed/running state observed by the adapter.
    pub state: ServiceManagerState,
    /// 当前活动代际标识。
    pub active_generation: Option<String>,
    /// 当前活动发布引用。
    pub active_release: Option<String>,
    /// 正在验证的候选代际标识。
    pub candidate_generation: Option<String>,
    /// 检查操作的审计记录。
    pub audit: Vec<String>,
}

/// 服务管理器执行代际交接后的结果证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerHandoffReport {
    /// 对应的升级计划标识。
    pub plan_id: String,
    /// 实际执行交接的服务管理器类别。
    pub kind: ServiceManagerKind,
    /// 目标服务名称。
    pub service_name: String,
    /// 交接状态。
    pub status: String,
    /// 活动代际是否已经切换。
    pub handoff_executed: bool,
    /// 是否需要调用方执行回滚。
    pub rollback_required: bool,
    /// 报告结束时的活动代际。
    pub active_generation: String,
    /// 交接前的活动代际。
    pub previous_generation: String,
    /// 报告结束时的活动发布引用。
    pub release_ref: String,
    /// 候选启动、健康门禁与提交的审计记录。
    pub audit: Vec<String>,
}

/// 服务管理器执行回滚后的结果证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerRollbackReport {
    /// 对应的升级计划标识。
    pub plan_id: String,
    /// 实际执行回滚的服务管理器类别。
    pub kind: ServiceManagerKind,
    /// 目标服务名称。
    pub service_name: String,
    /// 回滚状态。
    pub status: String,
    /// 是否已恢复上一代际。
    pub rollback_executed: bool,
    /// 回滚后的活动代际。
    pub active_generation: String,
    /// 回滚后的活动发布引用。
    pub release_ref: String,
    /// 触发回滚的非空原因。
    pub reason: String,
    /// 回滚操作的审计记录。
    pub audit: Vec<String>,
}

/// Read-only service status request.
pub struct ServiceManagerStatusRequest<'a> {
    /// 待检查的只读服务配置。
    pub definition: &'a ServiceManagerDefinition,
}

/// Backward-compatible name for the original read-only inspection request.
pub type ServiceManagerInspectRequest<'a> = ServiceManagerStatusRequest<'a>;

/// Request shared by all typed service mutations.
pub struct ServiceManagerMutationRequest<'a> {
    /// Validated service definition that constrains the mutation.
    pub definition: &'a ServiceManagerDefinition,
}

/// 服务管理器代际交接请求。
pub struct ServiceManagerHandoffRequest<'a> {
    /// 约束目标平台和服务名称的配置。
    pub definition: &'a ServiceManagerDefinition,
    /// 提供源、目标代际及发布引用的升级计划。
    pub plan: &'a UpgradeApplyPlan,
    /// 必须属于目标代际的候选健康结果。
    pub candidate_health: RuntimeHealth,
}

/// 服务管理器回滚请求。
pub struct ServiceManagerRollbackRequest<'a> {
    /// 约束目标平台和服务名称的配置。
    pub definition: &'a ServiceManagerDefinition,
    /// 提供应恢复源代际及发布引用的升级计划。
    pub plan: &'a UpgradeApplyPlan,
    /// 触发回滚的原因。
    pub reason: &'a str,
}

/// 隔离平台服务管理器差异的适配器接口。
pub trait ServiceManagerAdapter {
    /// 返回适配器实际实现的服务管理器类别。
    fn kind(&self) -> ServiceManagerKind;

    /// Installs the service definition if it is not already installed.
    fn install(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError>;

    /// Removes the service definition if it is installed.
    fn uninstall(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError>;

    /// Reads the current typed service state without mutation.
    fn status(
        &self,
        request: ServiceManagerStatusRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError>;

    /// Starts an installed service.
    fn start(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError>;

    /// Stops the service when it is currently running.
    fn stop(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError>;

    /// Restarts an installed service, starting it when currently stopped.
    fn restart(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError>;

    /// 读取服务配置与当前代际状态，不执行交接。
    fn inspect(
        &self,
        request: ServiceManagerInspectRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError> {
        self.status(request)
    }

    /// 在候选健康门禁通过后执行代际交接。
    fn handoff(
        &mut self,
        request: ServiceManagerHandoffRequest<'_>,
    ) -> Result<ServiceManagerHandoffReport, EvaError>;

    /// 将活动代际恢复为升级计划的源代际。
    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError>;
}

/// 只在内存中模拟服务交接的适配器。
///
/// 它拒绝所有真实平台类别，避免测试实现被误当成生产控制面。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FakeServiceManagerAdapter {
    /// Whether a fake service definition has been installed.
    installed: bool,
    /// Whether the installed fake service is running.
    running: bool,
    /// 模拟的当前活动代际。
    active_generation: Option<String>,
    /// 模拟的当前活动发布引用。
    active_release: Option<String>,
    /// 已启动但尚未通过健康门禁的候选代际。
    candidate_generation: Option<String>,
}

impl ServiceManagerState {
    /// Returns the stable state spelling used in audit evidence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::Stopped => "stopped",
            Self::Running => "running",
        }
    }
}

impl ServiceManagerOperation {
    /// Returns the stable operation spelling used in audit evidence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

impl ServiceManagerDefinition {
    /// 创建具有稳定非空服务名称的基础定义。
    pub fn new(
        enabled: bool,
        kind: ServiceManagerKind,
        service_name: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let service_name = stable_non_empty(service_name.into(), "service_name")?;
        Ok(Self {
            enabled,
            kind,
            service_name,
            unit_name: None,
            runtime_binary: None,
            candidate_runtime_binary: None,
            start_on_boot: false,
            restart_supervisor: false,
        })
    }

    /// 判断配置是否启用了真实平台服务管理器。
    pub fn production_adapter_enabled(&self) -> bool {
        self.enabled && self.kind.production_adapter()
    }
}

impl From<ServiceManagerConfig> for ServiceManagerDefinition {
    /// Moves an already validated config into the lifecycle boundary without path conversion.
    fn from(config: ServiceManagerConfig) -> Self {
        Self {
            enabled: config.enabled,
            kind: config.kind,
            service_name: config.service_name,
            unit_name: config.unit_name,
            runtime_binary: config.runtime_binary,
            candidate_runtime_binary: config.candidate_runtime_binary,
            start_on_boot: config.start_on_boot,
            restart_supervisor: config.restart_supervisor,
        }
    }
}

impl From<&ServiceManagerConfig> for ServiceManagerDefinition {
    /// Clones a validated config while preserving both path values byte-for-byte.
    fn from(config: &ServiceManagerConfig) -> Self {
        config.clone().into()
    }
}

impl FakeServiceManagerAdapter {
    /// 创建尚无活动代际的模拟适配器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建预置活动代际与发布引用的模拟适配器。
    pub fn with_active_generation(
        generation: impl Into<String>,
        release: impl Into<String>,
    ) -> Self {
        Self {
            installed: true,
            running: true,
            active_generation: Some(generation.into()),
            active_release: Some(release.into()),
            candidate_generation: None,
        }
    }

    /// 拒绝把模拟适配器用于真实平台服务管理器类别。
    fn ensure_fake(definition: &ServiceManagerDefinition) -> Result<(), EvaError> {
        if definition.kind == ServiceManagerKind::Fake {
            Ok(())
        } else {
            Err(EvaError::unsupported(
                "fake service manager adapter cannot execute platform service manager kind",
            )
            .with_context("kind", definition.kind.as_str())
            .with_context("service_name", &definition.service_name))
        }
    }

    /// 确认服务管理器配置已显式启用。
    fn ensure_enabled(definition: &ServiceManagerDefinition) -> Result<(), EvaError> {
        if definition.enabled {
            Ok(())
        } else {
            Err(EvaError::invalid_argument("service manager is not enabled")
                .with_context("service_name", &definition.service_name))
        }
    }

    /// Returns the single state source used by status and every mutation report.
    fn service_state(&self) -> ServiceManagerState {
        if !self.installed {
            ServiceManagerState::NotInstalled
        } else if self.running {
            ServiceManagerState::Running
        } else {
            ServiceManagerState::Stopped
        }
    }

    /// Requires an installed service before start/restart operations.
    fn ensure_installed(&self, definition: &ServiceManagerDefinition) -> Result<(), EvaError> {
        if self.installed {
            Ok(())
        } else {
            Err(
                EvaError::not_found("service manager service is not installed")
                    .with_context("service_name", &definition.service_name),
            )
        }
    }

    /// Builds stable, secret-free evidence from the authoritative fake state.
    fn mutation_report(
        &self,
        definition: &ServiceManagerDefinition,
        operation: ServiceManagerOperation,
        mutation_executed: bool,
    ) -> ServiceManagerMutationReport {
        let state = self.service_state();
        ServiceManagerMutationReport {
            kind: ServiceManagerKind::Fake,
            service_name: definition.service_name.clone(),
            operation,
            state,
            mutation_executed,
            production_adapter: false,
            audit: vec![
                format!("service_manager.fake:{}", operation.as_str()),
                format!("service_manager.mutation_executed:{mutation_executed}"),
                format!("service_manager.state:{}", state.as_str()),
                format!("service_manager.service:{}", definition.service_name),
            ],
        }
    }
}

impl ServiceManagerAdapter for FakeServiceManagerAdapter {
    /// 返回模拟适配器类别。
    fn kind(&self) -> ServiceManagerKind {
        ServiceManagerKind::Fake
    }

    fn install(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        let mutation_executed = !self.installed;
        if mutation_executed {
            self.installed = true;
            self.running = false;
        }
        Ok(self.mutation_report(
            request.definition,
            ServiceManagerOperation::Install,
            mutation_executed,
        ))
    }

    fn uninstall(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        let mutation_executed = self.installed;
        self.installed = false;
        self.running = false;
        Ok(self.mutation_report(
            request.definition,
            ServiceManagerOperation::Uninstall,
            mutation_executed,
        ))
    }

    /// 返回当前模拟状态，不修改代际。
    fn status(
        &self,
        request: ServiceManagerStatusRequest<'_>,
    ) -> Result<ServiceManagerStatusReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Ok(ServiceManagerStatusReport {
            kind: ServiceManagerKind::Fake,
            service_name: request.definition.service_name.clone(),
            configured: request.definition.enabled,
            production_adapter: false,
            state: self.service_state(),
            active_generation: self.active_generation.clone(),
            active_release: self.active_release.clone(),
            candidate_generation: self.candidate_generation.clone(),
            audit: vec![
                "service_manager.fake:inspect".to_owned(),
                format!(
                    "service_manager.service:{}",
                    request.definition.service_name
                ),
            ],
        })
    }

    fn start(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        self.ensure_installed(request.definition)?;
        let mutation_executed = !self.running;
        self.running = true;
        Ok(self.mutation_report(
            request.definition,
            ServiceManagerOperation::Start,
            mutation_executed,
        ))
    }

    fn stop(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        let mutation_executed = self.installed && self.running;
        self.running = false;
        Ok(self.mutation_report(
            request.definition,
            ServiceManagerOperation::Stop,
            mutation_executed,
        ))
    }

    fn restart(
        &mut self,
        request: ServiceManagerMutationRequest<'_>,
    ) -> Result<ServiceManagerMutationReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        self.ensure_installed(request.definition)?;
        self.running = true;
        Ok(self.mutation_report(request.definition, ServiceManagerOperation::Restart, true))
    }

    /// 模拟候选启动、健康门禁及活动代际切换。
    ///
    /// 候选健康失败时保留原活动代际并保留候选标识，返回需要回滚的阻塞报告；
    /// 仅在健康通过后才同时更新活动代际与发布引用，并清除候选项。
    fn handoff(
        &mut self,
        request: ServiceManagerHandoffRequest<'_>,
    ) -> Result<ServiceManagerHandoffReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        self.candidate_generation = Some(request.plan.to_generation.as_str().to_owned());

        if !request.candidate_health.healthy {
            return Ok(ServiceManagerHandoffReport {
                plan_id: request.plan.plan_id.clone(),
                kind: ServiceManagerKind::Fake,
                service_name: request.definition.service_name.clone(),
                status: "blocked".to_owned(),
                handoff_executed: false,
                rollback_required: true,
                active_generation: request.plan.from_generation.as_str().to_owned(),
                previous_generation: request.plan.from_generation.as_str().to_owned(),
                release_ref: request.plan.from_release.clone(),
                audit: vec![
                    "service_manager.fake:candidate_started".to_owned(),
                    "service_manager.fake:candidate_health_failed".to_owned(),
                    format!(
                        "service_manager.health:{}",
                        request.candidate_health.message
                    ),
                ],
            });
        }

        self.active_generation = Some(request.plan.to_generation.as_str().to_owned());
        self.active_release = Some(request.plan.to_release.clone());
        self.candidate_generation = None;
        Ok(ServiceManagerHandoffReport {
            plan_id: request.plan.plan_id.clone(),
            kind: ServiceManagerKind::Fake,
            service_name: request.definition.service_name.clone(),
            status: "committed".to_owned(),
            handoff_executed: true,
            rollback_required: false,
            active_generation: request.plan.to_generation.as_str().to_owned(),
            previous_generation: request.plan.from_generation.as_str().to_owned(),
            release_ref: request.plan.to_release.clone(),
            audit: vec![
                "service_manager.fake:candidate_started".to_owned(),
                "service_manager.fake:candidate_health_passed".to_owned(),
                "service_manager.fake:handoff_committed".to_owned(),
            ],
        })
    }

    /// 模拟恢复升级计划中的源代际，并清除任何候选状态。
    fn rollback(
        &mut self,
        request: ServiceManagerRollbackRequest<'_>,
    ) -> Result<ServiceManagerRollbackReport, EvaError> {
        Self::ensure_fake(request.definition)?;
        Self::ensure_enabled(request.definition)?;
        let reason = stable_non_empty(request.reason.to_owned(), "reason")?;
        self.active_generation = Some(request.plan.from_generation.as_str().to_owned());
        self.active_release = Some(request.plan.from_release.clone());
        self.candidate_generation = None;
        Ok(ServiceManagerRollbackReport {
            plan_id: request.plan.plan_id.clone(),
            kind: ServiceManagerKind::Fake,
            service_name: request.definition.service_name.clone(),
            status: "rolled_back".to_owned(),
            rollback_executed: true,
            active_generation: request.plan.from_generation.as_str().to_owned(),
            release_ref: request.plan.from_release.clone(),
            reason: reason.clone(),
            audit: vec![
                "service_manager.fake:rollback_committed".to_owned(),
                format!("service_manager.rollback.reason:{reason}"),
            ],
        })
    }
}

/// 校验服务名称和回滚原因等字段为已裁剪的非空单行文本。
fn stable_non_empty(value: String, field: &'static str) -> Result<String, EvaError> {
    if value.trim().is_empty() {
        Err(
            EvaError::invalid_argument("service manager field cannot be empty")
                .with_context("field", field),
        )
    } else if value.trim() != value {
        Err(EvaError::invalid_argument(
            "service manager field cannot contain leading or trailing whitespace",
        )
        .with_context("field", field))
    } else if value.contains('\n') || value.contains('\r') {
        Err(
            EvaError::invalid_argument("service manager field cannot contain line breaks")
                .with_context("field", field),
        )
    } else {
        Ok(value)
    }
}

#[cfg(test)]
/// 模拟服务管理器的交接、失败门禁与类别隔离测试。
mod tests {
    use super::*;
    use eva_config::ServiceManagerConfig;
    use eva_core::GenerationId;

    /// 构造服务管理器测试使用的固定升级计划。
    fn plan() -> UpgradeApplyPlan {
        UpgradeApplyPlan::new(
            "plan-service",
            GenerationId::parse("gen-v14").unwrap(),
            GenerationId::parse("gen-v15").unwrap(),
            "1.14.0",
            "1.15.0",
        )
        .unwrap()
    }

    #[test]
    fn config_conversion_preserves_canonical_kind_and_paths() {
        #[cfg(unix)]
        let runtime_binary = {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            PathBuf::from(OsString::from_vec(b"runtime/eva-\xff".to_vec()))
        };
        #[cfg(not(unix))]
        let runtime_binary = PathBuf::from("runtime dir/eva-current");
        let candidate_runtime_binary = PathBuf::from("candidate dir/eva-next");
        let config = ServiceManagerConfig {
            enabled: true,
            kind: eva_config::ServiceManagerKind::Systemd,
            service_name: "eva-service".to_owned(),
            unit_name: Some("eva.service".to_owned()),
            runtime_binary: Some(runtime_binary.clone()),
            candidate_runtime_binary: Some(candidate_runtime_binary.clone()),
            start_on_boot: true,
            restart_supervisor: true,
        };

        let borrowed = ServiceManagerDefinition::from(&config);
        let owned = ServiceManagerDefinition::from(config);
        let canonical_kind: eva_config::ServiceManagerKind = borrowed.kind;

        assert_eq!(canonical_kind, ServiceManagerKind::Systemd);
        assert_eq!(borrowed, owned);
        assert_eq!(borrowed.runtime_binary, Some(runtime_binary));
        assert_eq!(
            borrowed.candidate_runtime_binary,
            Some(candidate_runtime_binary)
        );
        assert!(borrowed.production_adapter_enabled());
    }

    #[test]
    fn fake_service_manager_typed_operations_are_idempotent() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-dev").unwrap();
        let mut adapter = FakeServiceManagerAdapter::new();

        let initial = adapter
            .status(ServiceManagerStatusRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(initial.state, ServiceManagerState::NotInstalled);
        assert!(!initial.production_adapter);

        let installed = adapter
            .install(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(installed.operation, ServiceManagerOperation::Install);
        assert_eq!(installed.state, ServiceManagerState::Stopped);
        assert!(installed.mutation_executed);
        assert!(!installed.production_adapter);
        assert!(
            !adapter
                .install(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .unwrap()
                .mutation_executed
        );

        let started = adapter
            .start(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(started.state, ServiceManagerState::Running);
        assert!(started.mutation_executed);
        assert!(
            !adapter
                .start(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .unwrap()
                .mutation_executed
        );

        let stopped = adapter
            .stop(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(stopped.state, ServiceManagerState::Stopped);
        assert!(stopped.mutation_executed);
        assert!(
            !adapter
                .stop(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .unwrap()
                .mutation_executed
        );

        let restarted = adapter
            .restart(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(restarted.operation, ServiceManagerOperation::Restart);
        assert_eq!(restarted.state, ServiceManagerState::Running);
        assert!(restarted.mutation_executed);

        let uninstalled = adapter
            .uninstall(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(uninstalled.state, ServiceManagerState::NotInstalled);
        assert!(uninstalled.mutation_executed);
        assert!(
            !adapter
                .uninstall(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .unwrap()
                .mutation_executed
        );
        for report in [&installed, &started, &stopped, &restarted, &uninstalled] {
            assert!(!report.production_adapter);
            assert!(report
                .audit
                .iter()
                .any(|entry| entry == "service_manager.mutation_executed:true"));
        }

        let object: &mut dyn ServiceManagerAdapter = &mut adapter;
        assert_eq!(object.kind(), ServiceManagerKind::Fake);
        assert_eq!(
            object
                .inspect(ServiceManagerInspectRequest {
                    definition: &definition,
                })
                .unwrap()
                .state,
            ServiceManagerState::NotInstalled
        );
    }

    #[test]
    fn fake_service_manager_requires_installation_for_start_and_restart() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-dev").unwrap();
        let mut adapter = FakeServiceManagerAdapter::new();

        for error in [
            adapter
                .start(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .unwrap_err(),
            adapter
                .restart(ServiceManagerMutationRequest {
                    definition: &definition,
                })
                .unwrap_err(),
        ] {
            assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
        }
        let stopped = adapter
            .stop(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap();
        assert_eq!(stopped.state, ServiceManagerState::NotInstalled);
        assert!(!stopped.mutation_executed);
    }

    #[test]
    fn fake_service_manager_rejects_disabled_mutation() {
        let definition =
            ServiceManagerDefinition::new(false, ServiceManagerKind::Fake, "eva-disabled").unwrap();
        let mut adapter = FakeServiceManagerAdapter::new();

        let error = adapter
            .install(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        let status = adapter
            .status(ServiceManagerStatusRequest {
                definition: &definition,
            })
            .unwrap();
        assert!(!status.configured);
        assert_eq!(status.state, ServiceManagerState::NotInstalled);
    }

    #[test]
    /// 验证成功交接和显式回滚都留下可审计证据。
    fn fake_service_manager_handoff_and_rollback_are_auditable() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-dev").unwrap();
        let plan = plan();
        let mut adapter = FakeServiceManagerAdapter::with_active_generation("gen-v14", "1.14.0");

        let report = adapter
            .handoff(ServiceManagerHandoffRequest {
                definition: &definition,
                plan: &plan,
                candidate_health: RuntimeHealth::healthy(plan.to_generation.clone()),
            })
            .unwrap();

        assert_eq!(report.status, "committed");
        assert!(report.handoff_executed);
        assert!(!report.rollback_required);
        assert_eq!(report.active_generation, "gen-v15");
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "service_manager.fake:handoff_committed"));

        let rollback = adapter
            .rollback(ServiceManagerRollbackRequest {
                definition: &definition,
                plan: &plan,
                reason: "candidate validation failed after handoff",
            })
            .unwrap();

        assert_eq!(rollback.status, "rolled_back");
        assert!(rollback.rollback_executed);
        assert_eq!(rollback.active_generation, "gen-v14");
        assert!(rollback
            .audit
            .iter()
            .any(|entry| entry == "service_manager.fake:rollback_committed"));
    }

    #[test]
    /// 验证候选健康失败时不会切换活动代际。
    fn fake_service_manager_blocks_failed_candidate_without_switching_active() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Fake, "eva-dev").unwrap();
        let plan = plan();
        let mut adapter = FakeServiceManagerAdapter::with_active_generation("gen-v14", "1.14.0");

        let report = adapter
            .handoff(ServiceManagerHandoffRequest {
                definition: &definition,
                plan: &plan,
                candidate_health: RuntimeHealth {
                    generation_id: plan.to_generation.clone(),
                    healthy: false,
                    message: "health check failed".to_owned(),
                },
            })
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert!(!report.handoff_executed);
        assert!(report.rollback_required);
        assert_eq!(report.active_generation, "gen-v14");
    }

    #[test]
    /// 验证模拟适配器拒绝执行真实平台服务管理器配置。
    fn fake_adapter_rejects_platform_service_manager_kind() {
        let definition =
            ServiceManagerDefinition::new(true, ServiceManagerKind::Systemd, "eva-prod").unwrap();
        let mut adapter = FakeServiceManagerAdapter::new();

        let error = adapter
            .inspect(ServiceManagerInspectRequest {
                definition: &definition,
            })
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unsupported);
        let mutation_error = adapter
            .install(ServiceManagerMutationRequest {
                definition: &definition,
            })
            .unwrap_err();
        assert_eq!(mutation_error.kind(), eva_core::ErrorKind::Unsupported);
    }
}
