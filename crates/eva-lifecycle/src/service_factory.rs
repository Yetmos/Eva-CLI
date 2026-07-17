//! Host-kind validation and bound command execution for service managers.

use crate::service_command::ValidatedServiceCommandTarget;
use crate::service_command::{ServiceCommand, ServiceCommandExecutor, ServiceCommandReport};
use crate::service_manager::ServiceManagerKind;
use eva_core::EvaError;

/// Host operating-system families relevant to production service adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceHostPlatform {
    /// Windows Service Control Manager host.
    Windows,
    /// Linux systemd host.
    Linux,
    /// macOS launchd host.
    Macos,
    /// A target with no production service-manager implementation.
    Unsupported,
}

impl ServiceHostPlatform {
    /// Detects the immutable compilation target.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Unsupported
        }
    }

    /// Returns the stable spelling used in errors and evidence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Unsupported => "unsupported",
        }
    }

    /// Returns the only production manager kind allowed on this host.
    pub const fn service_manager_kind(self) -> Option<ServiceManagerKind> {
        match self {
            Self::Windows => Some(ServiceManagerKind::WindowsService),
            Self::Linux => Some(ServiceManagerKind::Systemd),
            Self::Macos => Some(ServiceManagerKind::Launchd),
            Self::Unsupported => None,
        }
    }
}

/// Factory that releases an executor borrow only after exact host-kind validation.
pub struct ServiceManagerFactory<E> {
    host: ServiceHostPlatform,
    executor: E,
}

impl<E> ServiceManagerFactory<E>
where
    E: ServiceCommandExecutor,
{
    /// Creates a factory bound to the actual compilation target.
    pub const fn new(executor: E) -> Self {
        Self {
            host: ServiceHostPlatform::current(),
            executor,
        }
    }

    /// Returns the immutable host identity used by every bind operation.
    pub const fn host(&self) -> ServiceHostPlatform {
        self.host
    }

    /// Validates a kind before handing out a host-bound execution capability.
    pub fn bind(
        &mut self,
        kind: ServiceManagerKind,
    ) -> Result<HostBoundServiceCommandExecutor<'_, E>, EvaError> {
        ensure_host_kind(self.host, kind)?;
        Ok(HostBoundServiceCommandExecutor {
            target: ValidatedServiceCommandTarget::new(kind),
            executor: &mut self.executor,
        })
    }

    /// Convenience path that still performs the gate before the executor call.
    pub fn execute_command(
        &mut self,
        kind: ServiceManagerKind,
        command: &ServiceCommand,
    ) -> Result<ServiceCommandReport, EvaError> {
        self.bind(kind)?.execute(command)
    }

    #[cfg(test)]
    pub(crate) const fn for_host(host: ServiceHostPlatform, executor: E) -> Self {
        Self { host, executor }
    }
}

/// Execution capability created only for the production manager of its host.
pub struct HostBoundServiceCommandExecutor<'a, E> {
    target: ValidatedServiceCommandTarget,
    executor: &'a mut E,
}

impl<E> HostBoundServiceCommandExecutor<'_, E>
where
    E: ServiceCommandExecutor,
{
    /// Returns the manager kind already validated by the factory.
    pub const fn kind(&self) -> ServiceManagerKind {
        self.target.kind()
    }

    /// Executes one command and attaches kind, digest, and redacted audit evidence.
    pub fn execute(&mut self, command: &ServiceCommand) -> Result<ServiceCommandReport, EvaError> {
        let execution = self.executor.execute(&self.target, command)?;
        ServiceCommandReport::from_execution(self.target.kind(), command, execution)
    }
}

fn ensure_host_kind(host: ServiceHostPlatform, kind: ServiceManagerKind) -> Result<(), EvaError> {
    let expected = host.service_manager_kind();
    if expected == Some(kind) {
        return Ok(());
    }

    let message = if host == ServiceHostPlatform::Unsupported {
        "host platform has no supported service manager"
    } else if kind == ServiceManagerKind::Fake {
        "fake service manager cannot execute host commands"
    } else {
        "service manager kind does not match host platform"
    };
    Err(EvaError::unsupported(message)
        .with_context("host_platform", host.as_str())
        .with_context("requested_kind", kind.as_str())
        .with_context(
            "expected_kind",
            expected.map_or("none", ServiceManagerKind::as_str),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_command::{ServiceCommandArg, ServiceCommandExecution};
    use std::ffi::OsStr;
    use std::path::Path;

    struct RecordingExecutor {
        calls: Vec<ServiceCommand>,
        execution: ServiceCommandExecution,
    }

    impl RecordingExecutor {
        fn successful(stdout: impl Into<Vec<u8>>, stderr: impl Into<Vec<u8>>) -> Self {
            Self {
                calls: Vec::new(),
                execution: ServiceCommandExecution::exited(Some(0), stdout, stderr),
            }
        }
    }

    impl ServiceCommandExecutor for RecordingExecutor {
        fn execute(
            &mut self,
            _target: &ValidatedServiceCommandTarget,
            command: &ServiceCommand,
        ) -> Result<ServiceCommandExecution, EvaError> {
            self.calls.push(command.clone());
            Ok(self.execution.clone())
        }
    }

    fn command_with_secret() -> ServiceCommand {
        ServiceCommand::new(
            "manager-program",
            [
                ServiceCommandArg::public("query-state"),
                ServiceCommandArg::public("eva service;literal|argument"),
                ServiceCommandArg::secret("api-key=top-secret"),
            ],
        )
        .unwrap()
    }

    #[test]
    fn matching_host_passes_exact_path_and_argv_without_secret_audit() {
        let executor = RecordingExecutor::successful(b"active\n".to_vec(), Vec::new());
        let mut factory = ServiceManagerFactory::for_host(ServiceHostPlatform::Linux, executor);
        let command = command_with_secret();

        let report = factory
            .execute_command(ServiceManagerKind::Systemd, &command)
            .unwrap();

        assert_eq!(factory.executor.calls, vec![command]);
        let call = &factory.executor.calls[0];
        assert_eq!(call.executable(), Path::new("manager-program"));
        assert_eq!(call.arguments()[0].value(), OsStr::new("query-state"));
        assert_eq!(
            call.arguments()[1].value(),
            OsStr::new("eva service;literal|argument")
        );
        assert_eq!(
            call.arguments()[2].value(),
            OsStr::new("api-key=top-secret")
        );
        assert!(report.success);
        assert_eq!(report.stdout.bytes, b"active\n");
        assert!(report.result_digest.starts_with("sha256:"));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "service_command.argv_count:4"));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "service_command.argv.2:[REDACTED]"));
        let audit = report.audit.join("\n");
        assert!(!audit.contains("top-secret"));
        assert!(!audit.contains("api-key"));
        assert!(!audit.contains("literal|argument"));
    }

    #[test]
    fn wrong_platform_fake_and_unsupported_fail_before_executor_invocation() {
        let cases = [
            (ServiceHostPlatform::Windows, ServiceManagerKind::Systemd),
            (ServiceHostPlatform::Linux, ServiceManagerKind::Launchd),
            (
                ServiceHostPlatform::Macos,
                ServiceManagerKind::WindowsService,
            ),
            (ServiceHostPlatform::Linux, ServiceManagerKind::Fake),
            (
                ServiceHostPlatform::Unsupported,
                ServiceManagerKind::Systemd,
            ),
        ];

        for (host, kind) in cases {
            let executor = RecordingExecutor::successful(Vec::new(), Vec::new());
            let mut factory = ServiceManagerFactory::for_host(host, executor);
            let command = command_with_secret();

            let error = factory.execute_command(kind, &command).unwrap_err();

            assert_eq!(error.kind(), eva_core::ErrorKind::Unsupported);
            assert!(factory.executor.calls.is_empty());
            let diagnostics = format!("{error:?} {command:?}");
            assert!(!diagnostics.contains("top-secret"));
            assert!(error
                .context()
                .entries()
                .iter()
                .all(|(_, value)| !value.contains("top-secret")));
        }
    }

    #[test]
    fn host_kind_mapping_is_exact() {
        assert_eq!(
            ServiceHostPlatform::Windows.service_manager_kind(),
            Some(ServiceManagerKind::WindowsService)
        );
        assert_eq!(
            ServiceHostPlatform::Linux.service_manager_kind(),
            Some(ServiceManagerKind::Systemd)
        );
        assert_eq!(
            ServiceHostPlatform::Macos.service_manager_kind(),
            Some(ServiceManagerKind::Launchd)
        );
        assert_eq!(
            ServiceHostPlatform::Unsupported.service_manager_kind(),
            None
        );
    }

    #[test]
    fn bound_executor_cannot_change_its_validated_kind() {
        let executor = RecordingExecutor::successful(Vec::new(), Vec::new());
        let mut factory = ServiceManagerFactory::for_host(ServiceHostPlatform::Windows, executor);
        let runner = factory.bind(ServiceManagerKind::WindowsService).unwrap();

        assert_eq!(runner.kind(), ServiceManagerKind::WindowsService);
    }

    #[test]
    fn factory_rejects_executor_with_forged_stream_evidence() {
        struct ForgingExecutor;

        impl ServiceCommandExecutor for ForgingExecutor {
            fn execute(
                &mut self,
                _target: &ValidatedServiceCommandTarget,
                _command: &ServiceCommand,
            ) -> Result<ServiceCommandExecution, EvaError> {
                Ok(ServiceCommandExecution {
                    termination: crate::service_command::ServiceCommandTermination::Exited,
                    exit_code: Some(0),
                    stdout: crate::service_command::ServiceCommandStream {
                        bytes: b"active\n".to_vec(),
                        observed_size_bytes: 7,
                        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                            .to_owned(),
                        truncated: false,
                    },
                    stderr: crate::service_command::ServiceCommandStream {
                        bytes: Vec::new(),
                        observed_size_bytes: 1,
                        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                            .to_owned(),
                        truncated: false,
                    },
                })
            }
        }

        let mut factory =
            ServiceManagerFactory::for_host(ServiceHostPlatform::Linux, ForgingExecutor);
        let command = command_with_secret();

        let error = factory
            .execute_command(ServiceManagerKind::Systemd, &command)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(!format!("{error:?}").contains("top-secret"));
    }

    #[test]
    fn current_supported_host_maps_to_a_production_kind() {
        let host = ServiceHostPlatform::current();
        if let Some(kind) = host.service_manager_kind() {
            assert!(kind.production_adapter());
            assert_ne!(kind, ServiceManagerKind::Fake);
        }
    }
}
