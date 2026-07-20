//! Provider credential vault and per-invocation session boundaries.
//!
//! A manifest carries references only.  A transport receives bytes through a
//! short-lived [`CredentialSessionLease`], and the lease clears its in-memory
//! projection when the invocation finishes.  The production default is
//! deliberately fail-closed; callers must wire an OS/KMS-backed implementation
//! before a provider that declares credentials can run.

use crate::supervisor::{redact_provider_session_tokens, ProviderCredentialScope};
use eva_config::ProviderVaultSecretRef;
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::sync::{Arc, RwLock};

/// A vault implementation opens one session for one admitted provider scope.
pub trait CredentialVault: fmt::Debug + Send + Sync {
    /// Open a short-lived session.  Implementations must not put secret bytes
    /// in returned errors or debug output.
    fn open_session(
        &self,
        scope: &ProviderCredentialScope,
    ) -> Result<Box<dyn CredentialSession>, EvaError>;
}

/// Session-level secret access.  Implementations may fetch lazily, but must
/// reject every operation after [`CredentialSession::release`] succeeds.
pub trait CredentialSession: fmt::Debug + Send {
    /// Fetch one opaque vault reference.
    fn fetch(&mut self, secret_ref: &str) -> Result<SecretValue, EvaError>;
    /// Revoke/close the session and release provider-side material.
    fn release(&mut self) -> Result<(), EvaError>;
}

/// Cloneable runtime handle around a vault trait object.  Equality is pointer
/// identity so existing `AdapterRuntime` value semantics remain available
/// without ever comparing or formatting secret material.
#[derive(Clone)]
pub struct CredentialVaultHandle(Arc<dyn CredentialVault>);

impl CredentialVaultHandle {
    /// Wrap one vault implementation for runtime ownership.
    pub fn new(vault: impl CredentialVault + 'static) -> Self {
        Self(Arc::new(vault))
    }

    /// Wrap an already shared implementation.
    pub fn from_shared(vault: Arc<dyn CredentialVault>) -> Self {
        Self(vault)
    }

    /// Borrow the underlying authority.
    pub(crate) fn as_ref(&self) -> &dyn CredentialVault {
        self.0.as_ref()
    }
}

impl fmt::Debug for CredentialVaultHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialVaultHandle([REDACTED_AUTHORITY])")
    }
}

impl PartialEq for CredentialVaultHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CredentialVaultHandle {}

/// Secret bytes wrapped in a type whose formatting is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Construct a secret returned by an explicit vault implementation.
    /// Formatting this value is always redacted; callers should only expose
    /// it to the transport that owns the current credential lease.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Production-safe default.  It does not read the daemon environment or any
/// local file, so accidentally omitting the real vault wiring fails closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailClosedCredentialVault;

impl CredentialVault for FailClosedCredentialVault {
    fn open_session(
        &self,
        _scope: &ProviderCredentialScope,
    ) -> Result<Box<dyn CredentialSession>, EvaError> {
        Err(
            EvaError::permission_denied("provider credential vault is not configured")
                .with_provider_code("credential_vault_unconfigured"),
        )
    }
}

/// In-memory vault intended for explicit tests and controlled local fixtures.
/// It is never selected by production constructors implicitly.
#[derive(Clone, Default)]
pub struct MemoryCredentialVault {
    secrets: Arc<RwLock<BTreeMap<String, String>>>,
}

impl fmt::Debug for MemoryCredentialVault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.secrets.read().map(|map| map.len()).unwrap_or(0);
        formatter
            .debug_struct("MemoryCredentialVault")
            .field("secret_count", &count)
            .finish()
    }
}

impl MemoryCredentialVault {
    /// Create an empty memory vault.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace one reference without exposing it through `Debug`.
    pub fn insert(&self, secret_ref: impl Into<String>, value: impl Into<String>) {
        if let Ok(mut secrets) = self.secrets.write() {
            secrets.insert(secret_ref.into(), value.into());
        }
    }

    /// Builder form useful in tests.
    pub fn with_secret(self, secret_ref: impl Into<String>, value: impl Into<String>) -> Self {
        self.insert(secret_ref, value);
        self
    }
}

impl CredentialVault for MemoryCredentialVault {
    fn open_session(
        &self,
        scope: &ProviderCredentialScope,
    ) -> Result<Box<dyn CredentialSession>, EvaError> {
        let secrets = self
            .secrets
            .read()
            .map_err(|_| EvaError::internal("credential vault lock is poisoned"))?
            .clone();
        Ok(Box::new(MemoryCredentialSession {
            scope_id: scope.session_id.clone(),
            secrets,
            released: false,
            fetched: Vec::new(),
        }))
    }
}

#[derive(Clone)]
struct MemoryCredentialSession {
    scope_id: String,
    secrets: BTreeMap<String, String>,
    released: bool,
    fetched: Vec<String>,
}

impl fmt::Debug for MemoryCredentialSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryCredentialSession")
            .field("scope_id", &self.scope_id)
            .field("secret_count", &self.secrets.len())
            .field("released", &self.released)
            .finish()
    }
}

impl CredentialSession for MemoryCredentialSession {
    fn fetch(&mut self, secret_ref: &str) -> Result<SecretValue, EvaError> {
        if self.released {
            return Err(EvaError::conflict(
                "provider credential session has been released",
            ));
        }
        let lookup = secret_ref
            .strip_prefix("env:")
            .unwrap_or(secret_ref)
            .to_owned();
        let value = self
            .secrets
            .get(secret_ref)
            .or_else(|| self.secrets.get(&lookup))
            .ok_or_else(|| {
                EvaError::unavailable("provider credential reference is unavailable")
                    .with_provider_code("missing_credential")
                    .with_context("secret_ref", secret_ref)
            })?;
        self.fetched.push(secret_ref.to_owned());
        Ok(SecretValue::new(value.clone()))
    }

    fn release(&mut self) -> Result<(), EvaError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        wipe_map_values(&mut self.secrets);
        self.fetched.clear();
        Ok(())
    }
}

/// A transport-facing projection of one vault session.
pub struct CredentialSessionLease {
    session: Option<Box<dyn CredentialSession>>,
    values: BTreeMap<String, String>,
    allowed_envs: BTreeSet<String>,
    redactions: Vec<String>,
    audit: Vec<String>,
    released: bool,
}

impl fmt::Debug for CredentialSessionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialSessionLease")
            .field("env_names", &self.values.keys().collect::<Vec<_>>())
            .field("secret_count", &self.values.len())
            .field("released", &self.released)
            .finish()
    }
}

impl CredentialSessionLease {
    /// Open and populate a session from canonical vault refs plus legacy
    /// allowlisted env names.  Legacy names are resolved by the vault too;
    /// this function never reads the parent process environment.
    pub fn open(
        vault: &dyn CredentialVault,
        scope: Option<&ProviderCredentialScope>,
        vault_refs: &[ProviderVaultSecretRef],
        legacy_env: &[String],
    ) -> Result<Self, EvaError> {
        Self::open_with_lazy_env(vault, scope, vault_refs, legacy_env, &[])
    }

    /// Open a session while deferring selected environment references until a
    /// transport resolves a header lazily.  This keeps missing-header errors
    /// at the transport boundary while still ensuring the value comes from
    /// the vault rather than the parent process environment.
    pub(crate) fn open_with_lazy_env(
        vault: &dyn CredentialVault,
        scope: Option<&ProviderCredentialScope>,
        vault_refs: &[ProviderVaultSecretRef],
        legacy_env: &[String],
        lazy_env: &[String],
    ) -> Result<Self, EvaError> {
        if vault_refs.is_empty() && legacy_env.is_empty() && lazy_env.is_empty() {
            return Ok(Self {
                session: None,
                values: BTreeMap::new(),
                allowed_envs: BTreeSet::new(),
                redactions: Vec::new(),
                audit: Vec::new(),
                released: false,
            });
        }
        let scope = scope.ok_or_else(|| {
            EvaError::permission_denied("provider credential session scope is required")
                .with_provider_code("credential_scope_required")
        })?;
        let mut session = vault.open_session(scope).map_err(|error| {
            sanitize_vault_error(
                error,
                "provider credential vault session unavailable",
                "credential_vault_error",
            )
        })?;
        let mut values = BTreeMap::new();
        let mut allowed_envs = BTreeSet::new();
        allowed_envs.extend(vault_refs.iter().map(|reference| reference.env.clone()));
        allowed_envs.extend(legacy_env.iter().cloned());
        allowed_envs.extend(lazy_env.iter().cloned());
        let mut redactions = Vec::new();
        let mut audit = vec!["credential.vault_session:opened".to_owned()];

        for reference in vault_refs {
            let secret = match session.fetch(&reference.secret_ref) {
                Ok(secret) => secret,
                Err(error) => {
                    let _ = session.release();
                    return Err(sanitize_vault_error(
                        error,
                        "provider credential reference is unavailable",
                        "missing_credential",
                    )
                    .with_context("credential_env", &reference.env));
                }
            };
            if secret.expose().is_empty() {
                let _ = session.release();
                return Err(EvaError::unavailable(
                    "provider credential reference resolved to an empty value",
                )
                .with_provider_code("empty_credential")
                .with_context("credential_env", &reference.env));
            }
            values.insert(reference.env.clone(), secret.expose().to_owned());
            redactions.push(secret.expose().to_owned());
            audit.push(format!("credential_vault:{}:redacted", reference.env));
        }

        for name in legacy_env {
            if values.contains_key(name) {
                continue;
            }
            let secret = match session.fetch(name) {
                Ok(secret) => secret,
                Err(error) => {
                    let _ = session.release();
                    return Err(sanitize_vault_error(
                        error,
                        "provider credential reference is unavailable",
                        "missing_credential",
                    )
                    .with_context("credential_env", name));
                }
            };
            if secret.expose().is_empty() {
                let _ = session.release();
                return Err(EvaError::unavailable(
                    "provider credential environment reference resolved to an empty value",
                )
                .with_provider_code("empty_credential")
                .with_context("credential_env", name));
            }
            values.insert(name.clone(), secret.expose().to_owned());
            redactions.push(secret.expose().to_owned());
            audit.push(format!("credential_env:{name}:redacted"));
        }

        Ok(Self {
            session: Some(session),
            values,
            allowed_envs,
            redactions,
            audit,
            released: false,
        })
    }

    /// Inject fetched values into an explicit child environment map.
    pub(crate) fn inject_env(&self, env_values: &mut BTreeMap<String, String>) {
        if !self.released {
            env_values.extend(self.values.clone());
        }
    }

    /// Resolve one allowlisted env reference for an HTTP header.
    pub(crate) fn resolve_env(&mut self, name: &str) -> Result<String, EvaError> {
        if self.released {
            return Err(EvaError::conflict(
                "provider credential session has been released",
            ));
        }
        if !self.allowed_envs.contains(name) {
            return Err(EvaError::permission_denied(
                "provider credential environment reference is not declared",
            )
            .with_provider_code("credential_env_not_allowlisted")
            .with_context("credential_env", name));
        }
        if let Some(value) = self.values.get(name) {
            return Ok(value.clone());
        }
        let Some(session) = self.session.as_mut() else {
            return Err(
                EvaError::permission_denied("provider credential session is unavailable")
                    .with_provider_code("credential_vault_unconfigured"),
            );
        };
        let secret = session.fetch(name).map_err(|error| {
            sanitize_vault_error(
                error,
                "provider credential reference is unavailable",
                "missing_credential",
            )
            .with_context("credential_env", name)
        })?;
        if secret.expose().is_empty() {
            return Err(EvaError::unavailable(
                "provider credential environment reference resolved to an empty value",
            )
            .with_provider_code("empty_credential")
            .with_context("credential_env", name));
        }
        self.values
            .insert(name.to_owned(), secret.expose().to_owned());
        self.redactions.push(secret.expose().to_owned());
        self.audit.push(format!("credential_env:{name}:redacted"));
        Ok(secret.expose().to_owned())
    }

    /// Values that must be removed from stream previews, artifacts, and errors.
    pub(crate) fn redaction_values(&self) -> Vec<String> {
        if self.released {
            Vec::new()
        } else {
            self.redactions.clone()
        }
    }

    /// Stable audit entries with names only; no bytes are returned.
    pub(crate) fn audit_entries(&self) -> Vec<String> {
        self.audit.clone()
    }

    /// Explicitly close the provider session and clear the local projection.
    pub fn release(&mut self) -> Result<(), EvaError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let result = self
            .session
            .take()
            .map(|mut session| {
                session.release().map_err(|error| {
                    sanitize_vault_error(
                        error,
                        "provider credential vault session release failed",
                        "credential_vault_release_error",
                    )
                })
            })
            .unwrap_or(Ok(()));
        wipe_map_values(&mut self.values);
        self.allowed_envs.clear();
        for value in &mut self.redactions {
            wipe_string(value);
        }
        self.redactions.clear();
        self.audit
            .push("credential.vault_session:released".to_owned());
        result
    }
}

/// Best-effort in-place clearing for transient secret strings.  The public
/// API never promises allocator-level zeroization, but release paths should
/// overwrite bytes before dropping their owned buffers whenever possible.
fn wipe_string(value: &mut String) {
    if value.is_empty() {
        return;
    }
    let zeros = "\0".repeat(value.len());
    value.replace_range(.., &zeros);
    value.clear();
}

fn wipe_map_values(values: &mut BTreeMap<String, String>) {
    for value in values.values_mut() {
        wipe_string(value);
    }
    values.clear();
}

/// Vault implementations are an authority boundary and may be supplied by
/// external code.  Never propagate their free-form message or context because
/// either can accidentally contain secret bytes; preserve only classification
/// and retryability plus a stable, non-sensitive provider code.
fn sanitize_vault_error(error: EvaError, message: &'static str, code: &'static str) -> EvaError {
    EvaError::new(error.kind(), message)
        .with_retryable(error.is_retryable())
        .with_provider_code(code)
}

/// Redact vault values and provider session tokens from an error before it is
/// returned to runtime, audit, or observability layers.  Provider transports
/// may receive errors from external clients, so their free-form text is not a
/// trusted boundary even when the transport itself never logs the request.
pub(crate) fn sanitize_error_with_values(error: EvaError, sensitive_values: &[String]) -> EvaError {
    let redact = |value: &str| {
        let mut redacted = value.to_owned();
        for secret in sensitive_values {
            if !secret.is_empty() {
                redacted = redacted.replace(secret, "[REDACTED]");
            }
        }
        redact_provider_session_tokens(&redacted)
    };
    let mut safe =
        EvaError::new(error.kind(), redact(error.message())).with_retryable(error.is_retryable());
    if let Some(code) = error.provider_code() {
        safe = safe.with_provider_code(redact(code.as_str()));
    }
    for (key, value) in error.context().entries() {
        safe = safe.with_context(redact(key), redact(value));
    }
    safe
}

impl Drop for CredentialSessionLease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Return the smallest inherited environment needed to locate a provider
/// executable.  Secret-bearing names are never copied from the parent.
pub(crate) fn minimal_process_env(explicit: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for name in ["PATH", "SystemRoot", "SYSTEMROOT", "WINDIR"] {
        if let Ok(value) = env::var(name) {
            values.insert(name.to_owned(), value);
        }
    }
    #[cfg(test)]
    for name in ["EVA_RESTART_COUNTER_FILE", "EVA_OUTPUT_LIMIT_COUNTER_FILE"] {
        if let Ok(value) = env::var(name) {
            values.insert(name.to_owned(), value);
        }
    }
    values.extend(explicit.clone());
    values
}

/// Select the default vault for a direct transport call.  The default is
/// always fail-closed; tests and daemon hosts must wire an explicit vault.
pub(crate) fn default_credential_vault() -> Box<dyn CredentialVault> {
    Box::new(FailClosedCredentialVault)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{AdapterId, CapabilityName, ErrorKind, RequestId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scope() -> ProviderCredentialScope {
        ProviderCredentialScope::new_for_session(
            "vault-test-session",
            AdapterId::parse("vault-test-provider").unwrap(),
            RequestId::parse("req-vault-test").unwrap(),
            CapabilityName::parse("vault.read").unwrap(),
        )
    }

    #[test]
    fn fail_closed_vault_never_reads_parent_environment() {
        let vault = FailClosedCredentialVault;
        let error = vault.open_session(&scope()).unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_vault_unconfigured"
        );
    }

    #[test]
    fn memory_session_fetch_inject_release_and_debug_redact() {
        let vault = MemoryCredentialVault::new().with_secret("vault://tests/token", "top-secret");
        let refs = vec![ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://tests/token".to_owned(),
        }];
        let mut lease = CredentialSessionLease::open(&vault, Some(&scope()), &refs, &[]).unwrap();
        let mut env_values = BTreeMap::new();
        lease.inject_env(&mut env_values);
        assert_eq!(
            env_values.get("API_TOKEN").map(String::as_str),
            Some("top-secret")
        );
        assert!(!format!("{lease:?}").contains("top-secret"));
        assert!(lease.release().is_ok());
        assert!(lease.resolve_env("API_TOKEN").is_err());
        assert!(lease.redaction_values().is_empty());
        drop(env_values);
    }

    #[test]
    fn lazy_environment_resolution_is_allowlisted() {
        let vault = MemoryCredentialVault::new().with_secret("API_TOKEN", "top-secret");
        let mut lease = CredentialSessionLease::open_with_lazy_env(
            &vault,
            Some(&scope()),
            &[],
            &[],
            &["API_TOKEN".to_owned()],
        )
        .unwrap();
        assert_eq!(lease.resolve_env("API_TOKEN").unwrap(), "top-secret");
        let error = lease.resolve_env("UNDECLARED_TOKEN").unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_env_not_allowlisted"
        );
        lease.release().unwrap();
    }

    #[test]
    fn missing_scope_is_rejected_before_vault_access() {
        let vault = MemoryCredentialVault::new().with_secret("vault://tests/token", "top-secret");
        let refs = vec![ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://tests/token".to_owned(),
        }];
        let error = CredentialSessionLease::open(&vault, None, &refs, &[]).unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_scope_required"
        );
    }

    #[test]
    fn secret_value_formatting_is_always_redacted() {
        let secret = SecretValue::new("vault-test-secret");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
        assert_eq!(secret.to_string(), "[REDACTED]");
        let error = EvaError::internal(
            "vault-test-secret eva-provider-session:sess:digest and vault-test-secret",
        );
        let safe = sanitize_error_with_values(error, &["vault-test-secret".to_owned()]);
        assert!(!format!("{safe:?}").contains("vault-test-secret"));
        assert!(!safe.message().contains("eva-provider-session:"));
    }

    #[test]
    fn vault_errors_are_sanitized_and_release_runs_once() {
        let releases = Arc::new(AtomicUsize::new(0));
        let vault = CountingVault {
            releases: releases.clone(),
            secret: "external-vault-secret".to_owned(),
            fail_fetch: true,
            fail_release: false,
        };
        let refs = vec![ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://tests/missing".to_owned(),
        }];
        let error = CredentialSessionLease::open(&vault, Some(&scope()), &refs, &[]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(!format!("{error:?}").contains("external-vault-secret"));
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        let releases = Arc::new(AtomicUsize::new(0));
        let vault = CountingVault {
            releases: releases.clone(),
            secret: "external-vault-secret".to_owned(),
            fail_fetch: false,
            fail_release: true,
        };
        let mut lease =
            CredentialSessionLease::open(&vault, Some(&scope()), &[], &["API_TOKEN".to_owned()])
                .unwrap();
        let error = lease.release().unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_vault_release_error"
        );
        assert!(!format!("{error:?}").contains("external-vault-secret"));
        assert!(lease.release().is_ok());
        drop(lease);
        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }

    #[derive(Debug, Clone)]
    struct CountingVault {
        releases: Arc<AtomicUsize>,
        secret: String,
        fail_fetch: bool,
        fail_release: bool,
    }

    impl CredentialVault for CountingVault {
        fn open_session(
            &self,
            _scope: &ProviderCredentialScope,
        ) -> Result<Box<dyn CredentialSession>, EvaError> {
            Ok(Box::new(CountingSession {
                releases: self.releases.clone(),
                secret: self.secret.clone(),
                fail_fetch: self.fail_fetch,
                fail_release: self.fail_release,
                released: false,
            }))
        }
    }

    #[derive(Clone)]
    struct CountingSession {
        releases: Arc<AtomicUsize>,
        secret: String,
        fail_fetch: bool,
        fail_release: bool,
        released: bool,
    }

    impl fmt::Debug for CountingSession {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("CountingSession")
                .field("released", &self.released)
                .finish()
        }
    }

    impl CredentialSession for CountingSession {
        fn fetch(&mut self, _secret_ref: &str) -> Result<SecretValue, EvaError> {
            if self.fail_fetch {
                return Err(EvaError::unavailable(format!(
                    "vault backend rejected {}",
                    self.secret
                )));
            }
            Ok(SecretValue::new(self.secret.clone()))
        }

        fn release(&mut self) -> Result<(), EvaError> {
            if self.released {
                return Ok(());
            }
            self.released = true;
            self.releases.fetch_add(1, Ordering::SeqCst);
            if self.fail_release {
                return Err(EvaError::internal(format!(
                    "vault release failed for {}",
                    self.secret
                )));
            }
            Ok(())
        }
    }
}
