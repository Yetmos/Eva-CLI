//! Daemon-owned scheduling and execution for external knowledge retrieval.

use eva_adapter::{AdapterBackedCapabilityHost, AdapterRuntime};
use eva_capability::{
    CapabilityDescriptor, CapabilityProviderPlan, CapabilityRegistry, CapabilityRouter,
};
use eva_config::{KnowledgeRetrievalWorkerConfig, ProjectConfig};
use eva_core::{AdapterId, AgentId, CapabilityName, EvaError, RequestId};
use eva_memory::{
    ExternalKnowledgeRetrievalReport, ExternalKnowledgeRetrievalRequest, FileSystemScheduleStore,
    InMemoryKnowledgeService,
};
use eva_policy::{PermissionSet, PolicyDomainSet, RuntimePolicyGate};

pub(crate) const RETRIEVAL_SCHEDULE_ID: &str = "knowledge-retrieval";
const RETRIEVAL_LEASE_MS: u128 = 30_000;

#[derive(Debug)]
pub(crate) struct DaemonRetrievalWorker {
    config: KnowledgeRetrievalWorkerConfig,
    gate: RuntimePolicyGate,
    redaction: eva_policy::RedactionPolicyDomain,
    host: AdapterBackedCapabilityHost,
}

impl DaemonRetrievalWorker {
    pub(crate) fn from_project(
        project: &ProjectConfig,
        adapter_runtime: AdapterRuntime,
    ) -> Result<Option<Self>, EvaError> {
        let Some(config) = project.eva.runtime.retrieval_worker.clone() else {
            return Ok(None);
        };
        let capability = CapabilityName::parse(&config.capability)?;
        let provider = AdapterId::parse(&config.provider)?;
        let router = CapabilityRouter::new(capability_registry_from_project(project)?);
        let plan = router
            .registry()
            .get(&capability)
            .ok_or_else(|| EvaError::not_found("retrieval capability is not registered"))?
            .provider_plan(Some(provider));
        let permissions = permissions_for_plan(&plan);
        let domains = PolicyDomainSet::from_project(project)?;
        Ok(Some(Self {
            config,
            gate: RuntimePolicyGate::new(domains.clone()),
            redaction: domains.memory.redaction,
            host: AdapterBackedCapabilityHost::new(router, adapter_runtime, permissions),
        }))
    }

    fn execute(&self, observed_at: u128) -> Result<ExternalKnowledgeRetrievalReport, EvaError> {
        let request = ExternalKnowledgeRetrievalRequest::new(
            RequestId::parse(&format!("retrieval-{observed_at}"))?,
            AgentId::parse(&self.config.agent)?,
            CapabilityName::parse(&self.config.capability)?,
            AdapterId::parse(&self.config.provider)?,
            self.config.query.clone(),
        );
        let mut pending = InMemoryKnowledgeService::new();
        request.execute_with_redaction_policy(&self.gate, &self.host, &mut pending, &self.redaction)
    }

    pub(crate) fn interval_ms(&self) -> u128 {
        u128::from(self.config.interval_ms)
    }

    /// Drain the daemon-owned provider supervisor before task ownership is
    /// released. The worker itself remains an immutable scheduling facade;
    /// lifecycle authority stays with its single AdapterRuntime instance.
    pub(crate) fn drain_providers(
        &self,
        timeout: std::time::Duration,
    ) -> Result<eva_adapter::ProviderDrainReport, EvaError> {
        self.host.runtime().drain_providers(timeout)
    }
}

pub(crate) fn ensure_retrieval_schedule(
    schedule: &FileSystemScheduleStore,
    worker: Option<&DaemonRetrievalWorker>,
) -> Result<(), EvaError> {
    if worker.is_some() && schedule.read(RETRIEVAL_SCHEDULE_ID).is_err() {
        schedule.upsert(RETRIEVAL_SCHEDULE_ID, 0)?;
    }
    Ok(())
}

pub(crate) fn run_scheduled_retrieval(
    schedule: &FileSystemScheduleStore,
    owner: &str,
    worker: Option<&DaemonRetrievalWorker>,
    observed_at: u128,
) -> Result<Option<ExternalKnowledgeRetrievalReport>, EvaError> {
    let Some(worker) = worker else {
        return Ok(None);
    };
    let claim = match schedule.claim(
        RETRIEVAL_SCHEDULE_ID,
        owner,
        observed_at,
        RETRIEVAL_LEASE_MS,
    ) {
        Ok(claim) => claim,
        Err(error) if error.kind() == eva_core::ErrorKind::Unavailable => return Ok(None),
        Err(error) => return Err(error),
    };
    let report = worker.execute(observed_at)?;
    if report.status != "indexed" {
        return Ok(Some(report));
    }
    schedule.complete(
        RETRIEVAL_SCHEDULE_ID,
        owner,
        claim.generation,
        observed_at.saturating_add(worker.interval_ms()),
    )?;
    Ok(Some(report))
}

fn capability_registry_from_project(
    project: &ProjectConfig,
) -> Result<CapabilityRegistry, EvaError> {
    let mut registry = CapabilityRegistry::new();
    for manifest in &project.capabilities {
        registry.register(CapabilityDescriptor::from_manifest(manifest))?;
    }
    Ok(registry)
}

fn permissions_for_plan(plan: &CapabilityProviderPlan) -> PermissionSet {
    let mut permissions = PermissionSet::deny_all().allow_capability(plan.capability.clone());
    for candidate in &plan.providers {
        permissions = permissions.allow_adapter(candidate.provider.clone());
    }
    permissions
}
