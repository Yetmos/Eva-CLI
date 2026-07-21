//! Synchronous TCP/TLS connector for the MCP HTTP transport.
//!
//! This module deliberately owns connection establishment and TLS material
//! parsing, while HTTP framing remains in `json_rpc`. Certificate and key
//! bytes are consumed per transport construction and are never formatted.

use crate::session::{McpEndpoint, McpStreamableHttpConfig};
use eva_core::EvaError;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{CertificateError, ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pemfile::Item;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

const MAX_TLS_MATERIAL_BYTES: usize = 1024 * 1024;
const MAX_DNS_RESOLVER_THREADS: usize = 16;
const MAX_DNS_ADDRESSES: usize = 64;
static ACTIVE_DNS_RESOLVER_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Per-call TLS material supplied by the runtime authority.
///
/// The project root is used only for `file:` trust roots. Indirect roots are
/// keyed by the complete configured `pem:` reference so omitted or mismatched
/// material cannot be silently accepted.
#[derive(Default)]
pub struct McpTlsMaterial {
    project_root: Option<PathBuf>,
    indirect_trust_roots: BTreeMap<String, Zeroizing<Vec<u8>>>,
    client_certificate: Option<Zeroizing<Vec<u8>>>,
    client_private_key: Option<Zeroizing<Vec<u8>>>,
}

impl McpTlsMaterial {
    /// Create an empty material set. This is sufficient for plaintext HTTP or
    /// HTTPS configurations that use only platform trust roots and no mTLS.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `file:` references to an explicit controlled project root.
    pub fn with_project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        self.project_root = Some(project_root.into());
        self
    }

    /// Supply PEM bytes for one exact configured `pem:` indirect reference.
    pub fn with_indirect_trust_root(
        mut self,
        reference: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, EvaError> {
        let reference = reference.into();
        let mut bytes = bytes.into();
        if !reference.starts_with("pem:") || reference.len() <= "pem:".len() {
            bytes.zeroize();
            return Err(EvaError::invalid_argument(
                "MCP indirect TLS trust material requires a pem: reference",
            )
            .with_provider_code("mcp_tls_material_reference_invalid"));
        }
        if let Err(error) = validate_material_size(&bytes, "trust_root") {
            bytes.zeroize();
            return Err(error);
        }
        if self.indirect_trust_roots.contains_key(&reference) {
            bytes.zeroize();
            return Err(EvaError::conflict(
                "MCP indirect TLS trust material reference is duplicated",
            )
            .with_provider_code("mcp_tls_material_duplicate"));
        }
        self.indirect_trust_roots
            .insert(reference, Zeroizing::new(bytes));
        Ok(self)
    }

    /// Supply a client certificate chain and matching private key as PEM.
    pub fn with_client_auth(
        mut self,
        certificate_pem: impl Into<Vec<u8>>,
        private_key_pem: impl Into<Vec<u8>>,
    ) -> Result<Self, EvaError> {
        let mut certificate_pem = certificate_pem.into();
        let mut private_key_pem = private_key_pem.into();
        if self.client_certificate.is_some() || self.client_private_key.is_some() {
            certificate_pem.zeroize();
            private_key_pem.zeroize();
            return Err(EvaError::conflict(
                "MCP TLS client authentication material is already configured",
            )
            .with_provider_code("mcp_tls_client_auth_material_duplicate"));
        }
        if let Err(error) = validate_material_size(&certificate_pem, "client_certificate") {
            certificate_pem.zeroize();
            private_key_pem.zeroize();
            return Err(error);
        }
        if let Err(error) = validate_material_size(&private_key_pem, "client_private_key") {
            certificate_pem.zeroize();
            private_key_pem.zeroize();
            return Err(error);
        }
        self.client_certificate = Some(Zeroizing::new(certificate_pem));
        self.client_private_key = Some(Zeroizing::new(private_key_pem));
        Ok(self)
    }
}

impl fmt::Debug for McpTlsMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpTlsMaterial")
            .field("project_root_present", &self.project_root.is_some())
            .field(
                "indirect_trust_root_count",
                &self.indirect_trust_roots.len(),
            )
            .field(
                "client_certificate_present",
                &self.client_certificate.is_some(),
            )
            .field(
                "client_private_key_present",
                &self.client_private_key.is_some(),
            )
            .finish()
    }
}

/// Connector retained by an HTTP transport and reused across exchanges.
pub(crate) enum McpHttpConnector {
    Plaintext,
    Tls(Arc<ClientConfig>),
}

impl fmt::Debug for McpHttpConnector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpConnector")
            .field("tls", &matches!(self, Self::Tls(_)))
            .finish()
    }
}

/// A common I/O boundary for plaintext and rustls-backed sockets.
pub(crate) enum McpHttpStream {
    Plaintext(McpSharedTcpStream),
    Tls(Box<StreamOwned<ClientConnection, McpSharedTcpStream>>),
}

/// The exact TCP socket shared by the blocking reader and abort control
/// plane. Using the same socket object matters on Windows, where a duplicated
/// socket handle does not reliably wake a rustls read on another handle.
pub(crate) struct McpSharedTcpStream {
    socket: Arc<TcpStream>,
    timeout: McpSocketTimeout,
}

#[derive(Clone, Copy)]
enum McpSocketTimeout {
    Deadline(Instant),
    Idle(Duration),
}

/// Cloneable control plane for interrupting a blocking HTTP body read.
///
/// The handle shares only the TCP socket control reference. For TLS streams
/// it never touches rustls state from the aborting thread; shutting down the
/// underlying socket wakes the thread that exclusively owns rustls.
#[derive(Clone)]
pub(crate) struct McpHttpAbortHandle {
    inner: Arc<McpHttpAbortHandleInner>,
}

struct McpHttpAbortHandleInner {
    socket: Arc<TcpStream>,
    shutdown_requested: AtomicBool,
}

impl fmt::Debug for McpSharedTcpStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpSharedTcpStream")
            .finish_non_exhaustive()
    }
}

impl Deref for McpSharedTcpStream {
    type Target = TcpStream;

    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl Read for McpSharedTcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.socket
            .set_read_timeout(Some(self.remaining_timeout()?))?;
        self.finish_io(self.socket.as_ref().read(buffer))
    }
}

impl Write for McpSharedTcpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.socket
            .set_write_timeout(Some(self.remaining_timeout()?))?;
        self.finish_io(self.socket.as_ref().write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket
            .set_write_timeout(Some(self.remaining_timeout()?))?;
        self.finish_io(self.socket.as_ref().flush())
    }
}

impl McpSharedTcpStream {
    fn new(socket: TcpStream, deadline: Instant) -> io::Result<Self> {
        let stream = Self {
            socket: Arc::new(socket),
            timeout: McpSocketTimeout::Deadline(deadline),
        };
        let remaining = stream.remaining_timeout()?;
        stream.socket.set_read_timeout(Some(remaining))?;
        stream.socket.set_write_timeout(Some(remaining))?;
        Ok(stream)
    }

    fn use_idle_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        if timeout.is_zero() {
            return Err(deadline_io_error());
        }
        self.timeout = McpSocketTimeout::Idle(timeout);
        self.socket.set_read_timeout(Some(timeout))?;
        self.socket.set_write_timeout(Some(timeout))?;
        Ok(())
    }

    fn ensure_deadline(&self) -> io::Result<()> {
        self.remaining_timeout().map(|_| ())
    }

    fn remaining_timeout(&self) -> io::Result<Duration> {
        match self.timeout {
            McpSocketTimeout::Deadline(deadline) => deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(deadline_io_error),
            McpSocketTimeout::Idle(timeout) if !timeout.is_zero() => Ok(timeout),
            McpSocketTimeout::Idle(_) => Err(deadline_io_error()),
        }
    }

    fn finish_io<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if matches!(self.timeout, McpSocketTimeout::Deadline(deadline) if Instant::now() >= deadline)
        {
            Err(deadline_io_error())
        } else {
            result
        }
    }
}

fn deadline_io_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "MCP HTTP request deadline elapsed")
}

impl fmt::Debug for McpHttpAbortHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpAbortHandle")
            .field(
                "shutdown_requested",
                &self.inner.shutdown_requested.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for McpHttpStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpStream")
            .field("tls", &matches!(self, Self::Tls(_)))
            .finish_non_exhaustive()
    }
}

impl Read for McpHttpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plaintext(stream) => stream.read(buffer),
            Self::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for McpHttpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plaintext(stream) => stream.write(buffer),
            Self::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plaintext(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl McpHttpStream {
    /// Clone the control reference to the exact TCP socket before the stream
    /// is transferred to a blocking reader thread.
    pub(crate) fn abort_handle(&self) -> Result<McpHttpAbortHandle, EvaError> {
        let socket = match self {
            Self::Plaintext(stream) => stream.socket.clone(),
            Self::Tls(stream) => stream.sock.socket.clone(),
        };
        Ok(McpHttpAbortHandle {
            inner: Arc::new(McpHttpAbortHandleInner {
                socket,
                shutdown_requested: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn use_idle_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        match self {
            Self::Plaintext(stream) => stream.use_idle_timeout(timeout),
            Self::Tls(stream) => stream.sock.use_idle_timeout(timeout),
        }
    }

    pub(crate) fn ensure_deadline(&self) -> io::Result<()> {
        match self {
            Self::Plaintext(stream) => stream.ensure_deadline(),
            Self::Tls(stream) => stream.sock.ensure_deadline(),
        }
    }
}

impl McpHttpAbortHandle {
    /// Interrupt both directions of the shared socket. Concurrent and
    /// repeated requests are idempotent; a failed first attempt remains
    /// retryable instead of being mistaken for a completed shutdown.
    pub(crate) fn abort(&self) -> Result<(), EvaError> {
        match self.inner.socket.shutdown(Shutdown::Both) {
            Ok(()) => {
                self.inner.shutdown_requested.store(true, Ordering::Release);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                self.inner.shutdown_requested.store(true, Ordering::Release);
                Ok(())
            }
            Err(error) => Err(EvaError::unavailable("failed to abort MCP HTTP socket")
                .with_provider_code("mcp_http_abort_failed")
                .with_context("io_error_kind", format!("{:?}", error.kind()))),
        }
    }
}

impl McpHttpConnector {
    /// Build a connector while consuming all supplied secret material.
    pub(crate) fn from_config(
        config: &McpStreamableHttpConfig,
        material: McpTlsMaterial,
    ) -> Result<Self, EvaError> {
        config.validate_for_environment("dev")?;
        let endpoint = McpEndpoint::parse(&config.endpoint)?;
        match endpoint.scheme.as_str() {
            "http" => Ok(Self::Plaintext),
            "https" => build_tls_client_config(config, material).map(Self::Tls),
            _ => Err(
                EvaError::unsupported("unsupported MCP HTTP connector scheme")
                    .with_provider_code("mcp_http_scheme_unsupported")
                    .with_context("scheme", endpoint.scheme),
            ),
        }
    }

    /// Establish a socket and complete TLS before HTTP bytes are written.
    pub(crate) fn connect_until(
        &self,
        scheme: &str,
        host: &str,
        port: u16,
        origin: &str,
        deadline: Instant,
    ) -> Result<McpHttpStream, EvaError> {
        match (self, scheme) {
            (Self::Plaintext, "http") => {
                let stream = connect_tcp(host, port, origin, deadline)?;
                Ok(McpHttpStream::Plaintext(stream))
            }
            (Self::Tls(config), "https") => {
                let mut stream = connect_tcp(host, port, origin, deadline)?;
                let server_name = ServerName::try_from(host.to_owned()).map_err(|_| {
                    EvaError::invalid_argument("MCP TLS server name is invalid")
                        .with_provider_code("mcp_tls_server_name_invalid")
                        .with_context("origin", origin)
                })?;
                let mut connection =
                    ClientConnection::new(config.clone(), server_name).map_err(|_| {
                        EvaError::internal("failed to create MCP TLS client connection")
                            .with_provider_code("mcp_tls_client_config_invalid")
                            .with_context("origin", origin)
                    })?;
                connection
                    .complete_io(&mut stream)
                    .map_err(|error| map_tls_handshake_error(error, origin))?;
                stream
                    .ensure_deadline()
                    .map_err(|error| map_tls_handshake_error(error, origin))?;
                Ok(McpHttpStream::Tls(Box::new(StreamOwned::new(
                    connection, stream,
                ))))
            }
            _ => Err(
                EvaError::internal("MCP HTTP connector does not match endpoint scheme")
                    .with_provider_code("mcp_http_connector_scheme_mismatch")
                    .with_context("origin", origin),
            ),
        }
    }

    #[cfg(test)]
    fn connect(
        &self,
        scheme: &str,
        host: &str,
        port: u16,
        origin: &str,
        timeout: Duration,
    ) -> Result<McpHttpStream, EvaError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            EvaError::invalid_argument("MCP HTTP request timeout is out of range")
                .with_provider_code("mcp_http_timeout_invalid")
                .with_context("origin", origin)
        })?;
        self.connect_until(scheme, host, port, origin, deadline)
    }
}

fn build_tls_client_config(
    config: &McpStreamableHttpConfig,
    mut material: McpTlsMaterial,
) -> Result<Arc<ClientConfig>, EvaError> {
    let mut roots = RootCertStore::empty();
    if config.trust_roots.is_empty() {
        add_system_roots(&mut roots)?;
    } else {
        for reference in &config.trust_roots {
            if reference == "system" {
                add_system_roots(&mut roots)?;
            } else if let Some(relative_path) = reference.strip_prefix("file:") {
                let project_root = material.project_root.as_deref().ok_or_else(|| {
                    EvaError::permission_denied(
                        "MCP file trust root requires an explicit controlled project root",
                    )
                    .with_provider_code("mcp_tls_project_root_required")
                })?;
                let pem = read_controlled_trust_file(project_root, relative_path)?;
                add_pem_roots(&mut roots, &pem)?;
            } else if reference.starts_with("pem:") {
                let pem = material
                    .indirect_trust_roots
                    .remove(reference)
                    .ok_or_else(|| {
                        EvaError::permission_denied(
                            "MCP indirect trust root material resolver is unavailable",
                        )
                        .with_provider_code("mcp_tls_indirect_material_unavailable")
                    })?;
                add_pem_roots(&mut roots, &pem)?;
            }
        }
    }
    if !material.indirect_trust_roots.is_empty() {
        return Err(EvaError::invalid_argument(
            "MCP TLS material contains unconfigured indirect trust roots",
        )
        .with_provider_code("mcp_tls_material_unused"));
    }
    if roots.is_empty() {
        return Err(EvaError::permission_denied(
            "MCP TLS trust policy resolved to no usable certificates",
        )
        .with_provider_code("mcp_tls_trust_store_empty"));
    }

    let provider = rustls::crypto::ring::default_provider();
    let builder = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|_| {
            EvaError::internal("failed to configure MCP TLS protocol versions")
                .with_provider_code("mcp_tls_protocol_config_invalid")
        })?
        .with_root_certificates(roots);

    let mut client_config = match (
        config.client_auth.as_ref(),
        material.client_certificate.as_deref(),
        material.client_private_key.as_deref(),
    ) {
        (None, None, None) => builder.with_no_client_auth(),
        (Some(_), Some(certificate_pem), Some(private_key_pem)) => {
            let certificates = parse_certificate_pem(certificate_pem, "client_certificate")?;
            let private_key = parse_private_key_pem(private_key_pem)?;
            builder
                .with_client_auth_cert(certificates, private_key)
                .map_err(|_| {
                    EvaError::invalid_argument(
                        "MCP TLS client certificate and private key are invalid or mismatched",
                    )
                    .with_provider_code("mcp_tls_client_auth_invalid")
                })?
        }
        (Some(_), _, _) => {
            return Err(EvaError::permission_denied(
                "MCP TLS client authentication material is unavailable",
            )
            .with_provider_code("mcp_tls_client_auth_material_unavailable"));
        }
        (None, _, _) => {
            return Err(EvaError::invalid_argument(
                "MCP TLS material contains unconfigured client authentication bytes",
            )
            .with_provider_code("mcp_tls_material_unused"));
        }
    };
    client_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(client_config))
}

fn add_system_roots(roots: &mut RootCertStore) -> Result<(), EvaError> {
    let loaded = rustls_native_certs::load_native_certs();
    let (valid, _) = roots.add_parsable_certificates(loaded.certs);
    if valid == 0 {
        return Err(EvaError::unavailable(
            "MCP TLS platform trust store has no usable certificates",
        )
        .with_provider_code("mcp_tls_system_roots_unavailable"));
    }
    Ok(())
}

fn add_pem_roots(roots: &mut RootCertStore, pem: &[u8]) -> Result<(), EvaError> {
    let certificates = parse_certificate_pem(pem, "trust_root")?;
    for certificate in certificates {
        roots.add(certificate).map_err(|_| {
            EvaError::invalid_argument("MCP TLS trust root certificate is invalid")
                .with_provider_code("mcp_tls_trust_root_invalid")
        })?;
    }
    Ok(())
}

fn parse_certificate_pem(
    pem: &[u8],
    purpose: &'static str,
) -> Result<Vec<CertificateDer<'static>>, EvaError> {
    validate_material_size(pem, purpose)?;
    let mut reader = BufReader::new(pem);
    let mut certificates = Vec::new();
    loop {
        let item = rustls_pemfile::read_one(&mut reader).map_err(|_| {
            EvaError::invalid_argument("MCP TLS certificate PEM is malformed")
                .with_provider_code("mcp_tls_certificate_pem_invalid")
                .with_context("material", purpose)
        })?;
        match item {
            Some(Item::X509Certificate(certificate)) => certificates.push(certificate),
            Some(_) => {
                return Err(EvaError::invalid_argument(
                    "MCP TLS certificate PEM contains a non-certificate block",
                )
                .with_provider_code("mcp_tls_certificate_pem_invalid")
                .with_context("material", purpose));
            }
            None => break,
        }
    }
    if certificates.is_empty() {
        return Err(
            EvaError::invalid_argument("MCP TLS certificate PEM contains no certificates")
                .with_provider_code("mcp_tls_certificate_pem_empty")
                .with_context("material", purpose),
        );
    }
    Ok(certificates)
}

fn parse_private_key_pem(pem: &[u8]) -> Result<PrivateKeyDer<'static>, EvaError> {
    validate_material_size(pem, "client_private_key")?;
    let mut reader = BufReader::new(pem);
    let mut private_key = None;
    loop {
        let item = rustls_pemfile::read_one(&mut reader).map_err(|_| {
            EvaError::invalid_argument("MCP TLS private key PEM is malformed")
                .with_provider_code("mcp_tls_private_key_pem_invalid")
        })?;
        let key = match item {
            Some(Item::Pkcs1Key(key)) => Some(PrivateKeyDer::Pkcs1(key)),
            Some(Item::Pkcs8Key(key)) => Some(PrivateKeyDer::Pkcs8(key)),
            Some(Item::Sec1Key(key)) => Some(PrivateKeyDer::Sec1(key)),
            Some(_) => {
                return Err(EvaError::invalid_argument(
                    "MCP TLS private key PEM contains a non-key block",
                )
                .with_provider_code("mcp_tls_private_key_pem_invalid"));
            }
            None => break,
        };
        if let Some(key) = key {
            if private_key.replace(key).is_some() {
                return Err(EvaError::invalid_argument(
                    "MCP TLS private key PEM contains multiple keys",
                )
                .with_provider_code("mcp_tls_private_key_pem_ambiguous"));
            }
        }
    }
    private_key.ok_or_else(|| {
        EvaError::invalid_argument("MCP TLS private key PEM contains no private key")
            .with_provider_code("mcp_tls_private_key_pem_empty")
    })
}

fn validate_material_size(bytes: &[u8], material: &'static str) -> Result<(), EvaError> {
    if bytes.is_empty() {
        return Err(EvaError::invalid_argument("MCP TLS material is empty")
            .with_provider_code("mcp_tls_material_empty")
            .with_context("material", material));
    }
    if bytes.len() > MAX_TLS_MATERIAL_BYTES {
        return Err(EvaError::conflict("MCP TLS material exceeded size limit")
            .with_provider_code("mcp_tls_material_too_large")
            .with_context("material", material)
            .with_context("material_limit_bytes", MAX_TLS_MATERIAL_BYTES.to_string()));
    }
    Ok(())
}

fn read_controlled_trust_file(
    project_root: &Path,
    relative_path: &str,
) -> Result<Vec<u8>, EvaError> {
    read_controlled_trust_file_with_hook(project_root, relative_path, |_| {})
}

fn read_controlled_trust_file_with_hook<F>(
    project_root: &Path,
    relative_path: &str,
    mut ancestor_opened: F,
) -> Result<Vec<u8>, EvaError>
where
    F: FnMut(usize),
{
    if !project_root.is_absolute() {
        return Err(
            EvaError::permission_denied("MCP TLS project root must be an absolute path")
                .with_provider_code("mcp_tls_project_root_invalid"),
        );
    }
    let relative = Path::new(relative_path);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative_path
            .split(['/', '\\'])
            .any(|segment| segment.contains(':'))
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EvaError::permission_denied(
            "MCP TLS trust root path escapes its controlled project root",
        )
        .with_provider_code("mcp_tls_trust_root_escape"));
    }

    let original_root_metadata = fs::symlink_metadata(project_root).map_err(|_| {
        EvaError::not_found("MCP TLS controlled project root is unavailable")
            .with_provider_code("mcp_tls_project_root_unavailable")
    })?;
    if metadata_is_link_or_reparse(&original_root_metadata) || !original_root_metadata.is_dir() {
        return Err(EvaError::permission_denied(
            "MCP TLS controlled project root is not a regular directory",
        )
        .with_provider_code("mcp_tls_project_root_invalid"));
    }
    let canonical_root = fs::canonicalize(project_root).map_err(|_| {
        EvaError::not_found("MCP TLS controlled project root is unavailable")
            .with_provider_code("mcp_tls_project_root_unavailable")
    })?;
    if canonical_root.as_os_str() != project_root.as_os_str() {
        return Err(
            EvaError::permission_denied("MCP TLS project root must already be canonical")
                .with_provider_code("mcp_tls_project_root_not_canonical"),
        );
    }
    let root_metadata = fs::symlink_metadata(&canonical_root).map_err(|_| {
        EvaError::not_found("MCP TLS controlled project root is unavailable")
            .with_provider_code("mcp_tls_project_root_unavailable")
    })?;
    if metadata_is_link_or_reparse(&root_metadata) || !root_metadata.is_dir() {
        return Err(EvaError::permission_denied(
            "MCP TLS controlled project root is not a regular directory",
        )
        .with_provider_code("mcp_tls_project_root_invalid"));
    }

    validate_trust_path_components(&canonical_root, relative)?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(component) => Ok(component.to_os_string()),
            _ => Err(EvaError::permission_denied(
                "MCP TLS trust root path escapes its controlled project root",
            )
            .with_provider_code("mcp_tls_trust_root_escape")),
        })
        .collect::<Result<Vec<OsString>, EvaError>>()?;
    let mut file = open_controlled_trust_file(&canonical_root, &components, &mut ancestor_opened)
        .map_err(|_| {
        EvaError::permission_denied("MCP TLS trust root file could not be opened safely")
            .with_provider_code("mcp_tls_trust_root_open_denied")
    })?;
    let metadata = file.metadata().map_err(|_| {
        EvaError::unavailable("MCP TLS trust root metadata is unavailable")
            .with_provider_code("mcp_tls_trust_root_unavailable")
    })?;
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(EvaError::permission_denied(
            "MCP TLS trust root must be a regular non-link file",
        )
        .with_provider_code("mcp_tls_trust_root_link"));
    }
    if metadata.len() > MAX_TLS_MATERIAL_BYTES as u64 {
        return Err(
            EvaError::conflict("MCP TLS trust root file exceeded size limit")
                .with_provider_code("mcp_tls_material_too_large"),
        );
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((MAX_TLS_MATERIAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            EvaError::unavailable("failed to read MCP TLS trust root file")
                .with_provider_code("mcp_tls_trust_root_unavailable")
        })?;
    validate_material_size(&bytes, "trust_root")?;
    Ok(bytes)
}

fn validate_trust_path_components(root: &Path, relative: &Path) -> Result<(), EvaError> {
    let components = relative.components().collect::<Vec<_>>();
    let mut cursor = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(EvaError::permission_denied(
                "MCP TLS trust root path escapes its controlled project root",
            )
            .with_provider_code("mcp_tls_trust_root_escape"));
        };
        cursor.push(component);
        let metadata = fs::symlink_metadata(&cursor).map_err(|_| {
            EvaError::not_found("MCP TLS trust root path component is unavailable")
                .with_provider_code("mcp_tls_trust_root_unavailable")
        })?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err(EvaError::permission_denied(
                "MCP TLS trust root path contains a symlink or reparse point",
            )
            .with_provider_code("mcp_tls_trust_root_link"));
        }
        let final_component = index + 1 == components.len();
        if (!final_component && !metadata.is_dir()) || (final_component && !metadata.is_file()) {
            return Err(EvaError::permission_denied(
                "MCP TLS trust root path has an invalid entry type",
            )
            .with_provider_code("mcp_tls_trust_root_entry_invalid"));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const UNIX_O_CLOEXEC: i32 = 0x0008_0000;
#[cfg(target_os = "linux")]
const UNIX_O_DIRECTORY: i32 = 0x0001_0000;
#[cfg(target_os = "linux")]
const UNIX_O_NOFOLLOW: i32 = 0x0002_0000;
#[cfg(target_os = "linux")]
const UNIX_O_NONBLOCK: i32 = 0x0000_0800;

#[cfg(target_os = "macos")]
const UNIX_O_CLOEXEC: i32 = 0x0100_0000;
#[cfg(target_os = "macos")]
const UNIX_O_DIRECTORY: i32 = 0x0010_0000;
#[cfg(target_os = "macos")]
const UNIX_O_NOFOLLOW: i32 = 0x0000_0100;
#[cfg(target_os = "macos")]
const UNIX_O_NONBLOCK: i32 = 0x0000_0004;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_controlled_trust_file<F>(
    root: &Path,
    components: &[OsString],
    ancestor_opened: &mut F,
) -> io::Result<fs::File>
where
    F: FnMut(usize),
{
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let expected_root = fs::metadata(root)?;
    let mut root_options = OpenOptions::new();
    root_options
        .read(true)
        .custom_flags(UNIX_O_CLOEXEC | UNIX_O_DIRECTORY | UNIX_O_NOFOLLOW);
    let mut current = root_options.open(root)?;
    let opened_root = current.metadata()?;
    if !opened_root.is_dir()
        || opened_root.dev() != expected_root.dev()
        || opened_root.ino() != expected_root.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "controlled root identity changed while opening",
        ));
    }

    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "controlled path component contains NUL",
            )
        })?;
        let final_component = index + 1 == components.len();
        let flags = if final_component {
            UNIX_O_CLOEXEC | UNIX_O_NOFOLLOW | UNIX_O_NONBLOCK
        } else {
            UNIX_O_CLOEXEC | UNIX_O_DIRECTORY | UNIX_O_NOFOLLOW
        };
        let descriptor = loop {
            let descriptor = unsafe { openat(current.as_raw_fd(), name.as_ptr(), flags) };
            if descriptor >= 0 {
                break descriptor;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        let next = unsafe { fs::File::from_raw_fd(descriptor) };
        let metadata = next.metadata()?;
        if (final_component && !metadata.is_file()) || (!final_component && !metadata.is_dir()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "controlled path component has an invalid type",
            ));
        }
        if final_component {
            return Ok(next);
        }
        ancestor_opened(index);
        current = next;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "controlled path has no final component",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe extern "C" {
    fn openat(directory: i32, path: *const std::ffi::c_char, flags: i32, ...) -> i32;
}

#[cfg(windows)]
fn open_controlled_trust_file<F>(
    root: &Path,
    components: &[OsString],
    ancestor_opened: &mut F,
) -> io::Result<fs::File>
where
    F: FnMut(usize),
{
    windows_trust_path::open(root, components, ancestor_opened)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn open_controlled_trust_file<F>(
    root: &Path,
    components: &[OsString],
    ancestor_opened: &mut F,
) -> io::Result<fs::File>
where
    F: FnMut(usize),
{
    let mut path = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        path.push(component);
        if index + 1 < components.len() {
            ancestor_opened(index);
        }
    }
    OpenOptions::new().read(true).open(path)
}

#[cfg(windows)]
mod windows_trust_path {
    use super::*;
    use std::ffi::{c_void, OsStr};
    use std::mem::size_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use std::ptr::{null_mut, NonNull};

    type Handle = *mut c_void;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_READ_DATA: u32 = 0x0000_0001;
    const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
    const FILE_TRAVERSE: u32 = 0x0000_0020;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_OPEN: u32 = 0x0000_0001;
    const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: Handle,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }

    #[repr(C)]
    struct IoStatusBlock {
        status_or_pointer: usize,
        information: usize,
    }

    pub(super) fn open<F>(
        root: &Path,
        components: &[OsString],
        ancestor_opened: &mut F,
    ) -> io::Result<fs::File>
    where
        F: FnMut(usize),
    {
        let mut root_options = OpenOptions::new();
        root_options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let root_handle = root_options.open(root)?;
        validate_handle_type(&root_handle, false)?;
        let opened_root = final_path(&root_handle)?;
        if !paths_equal(&opened_root, root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "controlled root final path changed while opening",
            ));
        }
        let mut current = root_handle.try_clone()?;

        for (index, component) in components.iter().enumerate() {
            let final_component = index + 1 == components.len();
            let next = open_relative(&current, component, final_component)?;
            validate_handle_type(&next, final_component)?;
            if final_component {
                let current_root = final_path(&root_handle)?;
                if !paths_equal(&opened_root, &current_root) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "controlled root handle moved while traversing",
                    ));
                }
                let opened_file = final_path(&next)?;
                if !path_is_below(&opened_root, &opened_file) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "controlled file final path escaped its root handle",
                    ));
                }
                return Ok(next);
            }
            ancestor_opened(index);
            current = next;
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controlled path has no final component",
        ))
    }

    fn open_relative(
        parent: &fs::File,
        component: &OsStr,
        final_component: bool,
    ) -> io::Result<fs::File> {
        let mut wide = component.encode_wide().collect::<Vec<_>>();
        if wide.is_empty()
            || wide.contains(&0)
            || wide.len() > usize::from(u16::MAX) / size_of::<u16>()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "controlled path component is invalid",
            ));
        }
        let byte_length = u16::try_from(wide.len() * size_of::<u16>()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "controlled path component is too long",
            )
        })?;
        let mut name = UnicodeString {
            length: byte_length,
            maximum_length: byte_length,
            buffer: wide.as_mut_ptr(),
        };
        let mut attributes = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as u32,
            root_directory: parent.as_raw_handle() as Handle,
            object_name: &mut name,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: null_mut(),
            security_quality_of_service: null_mut(),
        };
        let mut io_status = IoStatusBlock {
            status_or_pointer: 0,
            information: 0,
        };
        let mut handle: Handle = null_mut();
        let desired_access = FILE_READ_ATTRIBUTES
            | SYNCHRONIZE
            | if final_component {
                FILE_READ_DATA
            } else {
                FILE_LIST_DIRECTORY | FILE_TRAVERSE
            };
        let create_options = FILE_OPEN_REPARSE_POINT
            | FILE_SYNCHRONOUS_IO_NONALERT
            | if final_component {
                FILE_NON_DIRECTORY_FILE
            } else {
                FILE_DIRECTORY_FILE
            };
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                desired_access,
                &mut attributes,
                &mut io_status,
                null_mut(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                create_options,
                null_mut(),
                0,
            )
        };
        if status < 0 {
            return Err(ntstatus_error(status));
        }
        let handle = NonNull::new(handle)
            .ok_or_else(|| io::Error::other("NtCreateFile returned a null handle"))?;
        Ok(unsafe { fs::File::from_raw_handle(handle.as_ptr() as RawHandle) })
    }

    fn validate_handle_type(file: &fs::File, final_component: bool) -> io::Result<()> {
        use std::os::windows::fs::MetadataExt;

        let metadata = file.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (final_component && !metadata.is_file())
            || (!final_component && !metadata.is_dir())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "controlled path handle is a reparse point or has an invalid type",
            ));
        }
        Ok(())
    }

    fn final_path(file: &fs::File) -> io::Result<PathBuf> {
        let handle = file.as_raw_handle() as Handle;
        let required = unsafe { GetFinalPathNameByHandleW(handle, null_mut(), 0, 0) };
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0_u16; required as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0)
        };
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        if written as usize >= buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "controlled handle final path changed while reading",
            ));
        }
        buffer.truncate(written as usize);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }

    fn paths_equal(left: &Path, right: &Path) -> bool {
        let left = left.components().collect::<Vec<_>>();
        let right = right.components().collect::<Vec<_>>();
        left.len() == right.len()
            && left.iter().zip(right).all(|(left, right)| {
                os_string_eq_ignore_ascii_case(left.as_os_str(), right.as_os_str())
            })
    }

    fn path_is_below(root: &Path, candidate: &Path) -> bool {
        let root = root.components().collect::<Vec<_>>();
        let candidate = candidate.components().collect::<Vec<_>>();
        candidate.len() > root.len()
            && root.iter().zip(candidate.iter()).all(|(root, candidate)| {
                os_string_eq_ignore_ascii_case(root.as_os_str(), candidate.as_os_str())
            })
    }

    fn os_string_eq_ignore_ascii_case(left: &OsStr, right: &OsStr) -> bool {
        let left = left.encode_wide().collect::<Vec<_>>();
        let right = right.encode_wide().collect::<Vec<_>>();
        left.len() == right.len()
            && left.into_iter().zip(right).all(|(left, right)| {
                let left = if left <= u16::from(u8::MAX) {
                    u16::from((left as u8).to_ascii_lowercase())
                } else {
                    left
                };
                let right = if right <= u16::from(u8::MAX) {
                    u16::from((right as u8).to_ascii_lowercase())
                } else {
                    right
                };
                left == right
            })
    }

    fn ntstatus_error(status: i32) -> io::Error {
        let code = unsafe { RtlNtStatusToDosError(status) };
        if code == 0 {
            io::Error::other("NtCreateFile failed")
        } else {
            io::Error::from_raw_os_error(code as i32)
        }
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            file_handle: *mut Handle,
            desired_access: u32,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *mut c_void,
            ea_length: u32,
        ) -> i32;

        fn RtlNtStatusToDosError(status: i32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(
            file: Handle,
            file_path: *mut u16,
            file_path_length: u32,
            flags: u32,
        ) -> u32;
    }
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn connect_tcp(
    host: &str,
    port: u16,
    origin: &str,
    deadline: Instant,
) -> Result<McpSharedTcpStream, EvaError> {
    if Instant::now() >= deadline {
        return Err(EvaError::timeout("MCP HTTP connection timed out")
            .with_provider_code("mcp_http_connect_timeout")
            .with_context("origin", origin));
    }
    let addresses = resolve_addresses(host, port, origin, deadline)?;
    if addresses.is_empty() {
        return Err(
            EvaError::unavailable("MCP HTTP server host did not resolve")
                .with_provider_code("mcp_http_dns_empty")
                .with_context("origin", origin),
        );
    }

    let mut last_error = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => {
                return McpSharedTcpStream::new(stream, deadline).map_err(|error| {
                    if is_timeout_error(&error) {
                        EvaError::timeout("MCP HTTP connection timed out")
                            .with_provider_code("mcp_http_connect_timeout")
                            .with_context("origin", origin)
                    } else {
                        EvaError::unavailable("failed to configure MCP HTTP socket timeouts")
                            .with_provider_code("mcp_http_timeout_config_failed")
                            .with_context("origin", origin)
                    }
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    let timed_out = Instant::now() >= deadline || last_error.as_ref().is_some_and(is_timeout_error);
    if timed_out {
        Err(EvaError::timeout("MCP HTTP connection timed out")
            .with_provider_code("mcp_http_connect_timeout")
            .with_context("origin", origin))
    } else {
        Err(EvaError::unavailable("failed to connect MCP HTTP server")
            .with_provider_code("mcp_http_connect_failed")
            .with_context("origin", origin))
    }
}

fn resolve_addresses(
    host: &str,
    port: u16,
    origin: &str,
    deadline: Instant,
) -> Result<Vec<SocketAddr>, EvaError> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(address, port)]);
    }
    let host = host.to_owned();
    resolve_addresses_with(
        move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.take(MAX_DNS_ADDRESSES + 1).collect())
        },
        origin,
        deadline,
    )
}

fn resolve_addresses_with<F>(
    resolver: F,
    origin: &str,
    deadline: Instant,
) -> Result<Vec<SocketAddr>, EvaError>
where
    F: FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| dns_timeout_error(origin))?;
    let slot = DnsResolverSlot::acquire(origin)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("eva-mcp-dns".to_owned())
        .spawn(move || {
            let _slot = slot;
            let _ = sender.send(resolver());
        })
        .map_err(|_| {
            EvaError::unavailable("failed to start MCP HTTP DNS resolver")
                .with_provider_code("mcp_http_dns_worker_unavailable")
                .with_context("origin", origin)
        })?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| dns_timeout_error(origin))?;
    let resolved = match receiver.recv_timeout(remaining) {
        Ok(resolved) => resolved,
        Err(mpsc::RecvTimeoutError::Timeout) => return Err(dns_timeout_error(origin)),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(
                EvaError::unavailable("MCP HTTP DNS resolver stopped unexpectedly")
                    .with_provider_code("mcp_http_dns_failed")
                    .with_context("origin", origin),
            );
        }
    };
    if Instant::now() >= deadline {
        return Err(dns_timeout_error(origin));
    }
    let addresses = resolved.map_err(|_| {
        EvaError::unavailable("failed to resolve MCP HTTP server")
            .with_provider_code("mcp_http_dns_failed")
            .with_context("origin", origin)
    })?;
    if addresses.len() > MAX_DNS_ADDRESSES {
        return Err(
            EvaError::conflict("MCP HTTP server resolved to too many addresses")
                .with_provider_code("mcp_http_dns_address_limit")
                .with_context("origin", origin)
                .with_context("address_limit", MAX_DNS_ADDRESSES.to_string()),
        );
    }
    Ok(addresses)
}

#[derive(Debug)]
struct DnsResolverSlot {
    active: &'static AtomicUsize,
}

impl DnsResolverSlot {
    fn acquire(origin: &str) -> Result<Self, EvaError> {
        Self::acquire_from(
            &ACTIVE_DNS_RESOLVER_THREADS,
            MAX_DNS_RESOLVER_THREADS,
            origin,
        )
    }

    fn acquire_from(
        active_threads: &'static AtomicUsize,
        limit: usize,
        origin: &str,
    ) -> Result<Self, EvaError> {
        active_threads
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < limit).then_some(active + 1)
            })
            .map_err(|_| {
                EvaError::unavailable("MCP HTTP DNS resolver capacity is exhausted")
                    .with_provider_code("mcp_http_dns_capacity_exhausted")
                    .with_retryable(true)
                    .with_context("origin", origin)
            })?;
        Ok(Self {
            active: active_threads,
        })
    }
}

impl Drop for DnsResolverSlot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn dns_timeout_error(origin: &str) -> EvaError {
    EvaError::timeout("MCP HTTP DNS resolution timed out")
        .with_provider_code("mcp_http_dns_timeout")
        .with_context("origin", origin)
}

fn map_tls_handshake_error(error: io::Error, origin: &str) -> EvaError {
    if is_timeout_error(&error) {
        return EvaError::timeout("MCP TLS handshake timed out")
            .with_provider_code("mcp_tls_handshake_timeout")
            .with_context("origin", origin);
    }
    let rustls_error = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<rustls::Error>());
    if let Some(rustls::Error::InvalidCertificate(certificate_error)) = rustls_error {
        let (message, code) = classify_certificate_error(certificate_error);
        return EvaError::permission_denied(message)
            .with_provider_code(code)
            .with_context("origin", origin);
    }
    if let Some(rustls::Error::AlertReceived(alert)) = rustls_error {
        if matches!(
            alert,
            rustls::AlertDescription::BadCertificate
                | rustls::AlertDescription::UnknownCA
                | rustls::AlertDescription::CertificateRequired
                | rustls::AlertDescription::AccessDenied
        ) {
            return EvaError::permission_denied("MCP TLS peer rejected client authentication")
                .with_provider_code("mcp_tls_client_auth_rejected")
                .with_context("origin", origin);
        }
    }
    EvaError::unavailable("MCP TLS handshake failed")
        .with_provider_code("mcp_tls_handshake_failed")
        .with_context("origin", origin)
}

fn classify_certificate_error(error: &CertificateError) -> (&'static str, &'static str) {
    match error {
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => (
            "MCP TLS server certificate is expired",
            "mcp_tls_certificate_expired",
        ),
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => (
            "MCP TLS server certificate is not valid yet",
            "mcp_tls_certificate_not_valid_yet",
        ),
        CertificateError::UnknownIssuer => (
            "MCP TLS server certificate chain has an unknown issuer",
            "mcp_tls_unknown_ca",
        ),
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => (
            "MCP TLS server certificate does not match the endpoint hostname",
            "mcp_tls_hostname_mismatch",
        ),
        CertificateError::Revoked => (
            "MCP TLS server certificate is revoked",
            "mcp_tls_certificate_revoked",
        ),
        _ => (
            "MCP TLS server certificate validation failed",
            "mcp_tls_certificate_invalid",
        ),
    }
}

pub(crate) fn map_http_read_error(error: io::Error, origin: &str) -> EvaError {
    if is_timeout_error(&error) {
        EvaError::timeout("MCP HTTP response read timed out")
            .with_provider_code("mcp_http_read_timeout")
            .with_context("origin", origin)
    } else {
        EvaError::unavailable("failed to read MCP HTTP response")
            .with_provider_code("mcp_http_read_failed")
            .with_context("origin", origin)
    }
}

pub(crate) fn map_http_write_error(error: io::Error, origin: &str) -> EvaError {
    if is_timeout_error(&error) {
        EvaError::timeout("MCP HTTP request write timed out")
            .with_provider_code("mcp_http_write_timeout")
            .with_context("origin", origin)
    } else {
        EvaError::unavailable("failed to write MCP HTTP request")
            .with_provider_code("mcp_http_write_failed")
            .with_context("origin", origin)
    }
}

fn is_timeout_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{McpClientAuthConfig, McpRedirectPolicy};
    use rustls::server::WebPkiClientVerifier;
    use rustls::{ServerConfig, ServerConnection, StreamOwned};
    use std::net::TcpListener;
    use std::thread;

    const TEST_CA_PEM: &[u8] = include_bytes!("../testdata/tls/ca.pem");
    const TEST_SERVER_PEM: &[u8] = include_bytes!("../testdata/tls/server.pem");
    const TEST_SERVER_KEY: &[u8] = include_bytes!("../testdata/tls/server.key");
    const TEST_WRONG_HOST_PEM: &[u8] = include_bytes!("../testdata/tls/wrong.pem");
    const TEST_EXPIRED_PEM: &[u8] = include_bytes!("../testdata/tls/expired.pem");
    const TEST_CLIENT_PEM: &[u8] = include_bytes!("../testdata/tls/client.pem");
    const TEST_CLIENT_KEY: &[u8] = include_bytes!("../testdata/tls/client.key");
    const TEST_UNKNOWN_CA_PEM: &[u8] = include_bytes!("../testdata/tls/unknown-ca.pem");

    #[test]
    fn tls_material_debug_never_contains_paths_references_or_bytes() {
        let material = McpTlsMaterial::new()
            .with_project_root(PathBuf::from("C:/secret/project"))
            .with_indirect_trust_root("pem:secret-root", b"secret-cert".to_vec())
            .unwrap()
            .with_client_auth(b"secret-client-cert".to_vec(), b"secret-key".to_vec())
            .unwrap();

        let debug = format!("{material:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("C:/secret/project"));
        assert!(!debug.contains("pem:secret-root"));
        assert!(!debug.contains("secret-client-cert"));
        assert!(debug.contains("indirect_trust_root_count: 1"));
        assert!(debug.contains("client_certificate_present: true"));
    }

    #[test]
    fn certificate_error_classification_is_stable() {
        assert_eq!(
            classify_certificate_error(&CertificateError::Expired).1,
            "mcp_tls_certificate_expired"
        );
        assert_eq!(
            classify_certificate_error(&CertificateError::UnknownIssuer).1,
            "mcp_tls_unknown_ca"
        );
        assert_eq!(
            classify_certificate_error(&CertificateError::NotValidForName).1,
            "mcp_tls_hostname_mismatch"
        );
    }

    #[test]
    fn abort_handle_interrupts_plaintext_and_tls_reads_and_is_redacted() {
        let plaintext_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let plaintext_port = plaintext_listener.local_addr().unwrap().port();
        let plaintext_server = thread::spawn(move || {
            let (mut socket, _) = plaintext_listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut byte = [0_u8; 1];
            socket.read(&mut byte).map_err(|error| error.kind())
        });
        let plaintext = McpHttpConnector::Plaintext
            .connect(
                "http",
                "127.0.0.1",
                plaintext_port,
                "http://127.0.0.1",
                Duration::from_secs(1),
            )
            .unwrap();
        let plaintext_abort = plaintext.abort_handle().unwrap();
        assert!(!format!("{plaintext_abort:?}").contains("127.0.0.1"));
        let (plaintext_ready_sender, plaintext_ready_receiver) = std::sync::mpsc::sync_channel(1);
        let plaintext_reader = thread::spawn(move || {
            let mut stream = plaintext;
            let mut byte = [0_u8; 1];
            plaintext_ready_sender.send(()).unwrap();
            stream.read(&mut byte).map_err(|error| error.kind())
        });
        plaintext_ready_receiver.recv().unwrap();
        plaintext_abort.abort().unwrap();
        plaintext_abort.abort().unwrap();
        assert_reader_was_interrupted(plaintext_reader.join().unwrap());
        assert_eq!(plaintext_server.join().unwrap().unwrap(), 0);

        let tls_listener = TcpListener::bind(("localhost", 0)).unwrap();
        let tls_port = tls_listener.local_addr().unwrap().port();
        let server_config = test_server_config(TEST_SERVER_PEM, false);
        let tls_server = thread::spawn(move || {
            let (socket, _) = tls_listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let connection = ServerConnection::new(server_config).unwrap();
            let mut stream = StreamOwned::new(connection, socket);
            let mut byte = [0_u8; 1];
            stream.read(&mut byte).map_err(|error| error.kind())
        });
        let config = tls_config(tls_port, "localhost", false);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();
        let tls = connector
            .connect(
                "https",
                "localhost",
                tls_port,
                &format!("https://localhost:{tls_port}"),
                Duration::from_secs(1),
            )
            .unwrap();
        let tls_abort = tls.abort_handle().unwrap();
        assert!(!format!("{tls_abort:?}").contains("localhost"));
        let (tls_ready_sender, tls_ready_receiver) = std::sync::mpsc::sync_channel(1);
        let tls_reader = thread::spawn(move || {
            let mut stream = tls;
            let mut byte = [0_u8; 1];
            tls_ready_sender.send(()).unwrap();
            stream.read(&mut byte).map_err(|error| error.kind())
        });
        tls_ready_receiver.recv().unwrap();
        tls_abort.abort().unwrap();
        tls_abort.abort().unwrap();
        assert_reader_was_interrupted(tls_reader.join().unwrap());
        match tls_server.join().unwrap() {
            Ok(0) => {}
            Err(
                io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected,
            ) => {}
            outcome => panic!("TLS reader was not interrupted by socket shutdown: {outcome:?}"),
        }
    }

    fn assert_reader_was_interrupted(outcome: Result<usize, io::ErrorKind>) {
        match outcome {
            Ok(0) => {}
            Err(
                io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected,
            ) => {}
            // Darwin can surface EINVAL when socket shutdown wins the race
            // with the timeout setsockopt immediately before the blocking read.
            Err(io::ErrorKind::InvalidInput) if cfg!(target_os = "macos") => {}
            outcome => panic!("reader was not interrupted by socket shutdown: {outcome:?}"),
        }
    }

    #[test]
    fn local_ca_handshake_validates_chain_hostname_and_sni() {
        let server = spawn_tls_server(TEST_SERVER_PEM, false);
        let config = tls_config(server.port, "localhost", false);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();

        let stream = connector
            .connect(
                "https",
                "localhost",
                server.port,
                &format!("https://localhost:{}", server.port),
                Duration::from_secs(2),
            )
            .unwrap();
        drop(stream);

        assert_eq!(server.join().unwrap().as_deref(), Some("localhost"));
    }

    #[test]
    fn hostname_mismatch_has_stable_error_code() {
        let server = spawn_tls_server(TEST_WRONG_HOST_PEM, false);
        let config = tls_config(server.port, "localhost", false);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();

        let error = connector
            .connect(
                "https",
                "localhost",
                server.port,
                &format!("https://localhost:{}", server.port),
                Duration::from_secs(2),
            )
            .unwrap_err();

        assert_provider_code(&error, "mcp_tls_hostname_mismatch");
        let _ = server.join();
    }

    #[test]
    fn unknown_ca_has_stable_error_code() {
        let server = spawn_tls_server(TEST_SERVER_PEM, false);
        let config = tls_config(server.port, "localhost", false);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_UNKNOWN_CA_PEM.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();

        let error = connector
            .connect(
                "https",
                "localhost",
                server.port,
                &format!("https://localhost:{}", server.port),
                Duration::from_secs(2),
            )
            .unwrap_err();

        assert_provider_code(&error, "mcp_tls_unknown_ca");
        let _ = server.join();
    }

    #[test]
    fn expired_certificate_has_stable_error_code() {
        let server = spawn_tls_server(TEST_EXPIRED_PEM, false);
        let config = tls_config(server.port, "localhost", false);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();

        let error = connector
            .connect(
                "https",
                "localhost",
                server.port,
                &format!("https://localhost:{}", server.port),
                Duration::from_secs(2),
            )
            .unwrap_err();

        assert_provider_code(&error, "mcp_tls_certificate_expired");
        let _ = server.join();
    }

    #[test]
    fn mutual_tls_uses_supplied_client_certificate_and_key() {
        let server = spawn_tls_server(TEST_SERVER_PEM, true);
        let config = tls_config(server.port, "localhost", true);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap()
            .with_client_auth(TEST_CLIENT_PEM.to_vec(), TEST_CLIENT_KEY.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();

        let stream = connector
            .connect(
                "https",
                "localhost",
                server.port,
                &format!("https://localhost:{}", server.port),
                Duration::from_secs(2),
            )
            .unwrap();
        drop(stream);

        assert_eq!(server.join().unwrap().as_deref(), Some("localhost"));
    }

    #[test]
    fn missing_and_bad_client_auth_material_fail_before_network_io() {
        let config = tls_config(443, "localhost", true);
        let missing = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();
        let error = McpHttpConnector::from_config(&config, missing).unwrap_err();
        assert_provider_code(&error, "mcp_tls_client_auth_material_unavailable");

        let mismatched = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap()
            .with_client_auth(TEST_CLIENT_PEM.to_vec(), TEST_SERVER_KEY.to_vec())
            .unwrap();
        let error = McpHttpConnector::from_config(&config, mismatched).unwrap_err();
        assert_provider_code(&error, "mcp_tls_client_auth_invalid");

        let malformed = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", b"not a certificate".to_vec())
            .unwrap()
            .with_client_auth(TEST_CLIENT_PEM.to_vec(), TEST_CLIENT_KEY.to_vec())
            .unwrap();
        let error = McpHttpConnector::from_config(&config, malformed).unwrap_err();
        assert_provider_code(&error, "mcp_tls_certificate_pem_empty");

        let duplicate = McpTlsMaterial::new()
            .with_client_auth(TEST_CLIENT_PEM.to_vec(), TEST_CLIENT_KEY.to_vec())
            .unwrap()
            .with_client_auth(TEST_CLIENT_PEM.to_vec(), TEST_CLIENT_KEY.to_vec())
            .unwrap_err();
        assert_provider_code(&duplicate, "mcp_tls_client_auth_material_duplicate");
    }

    #[test]
    fn indirect_root_without_material_fails_closed() {
        let config = tls_config(443, "localhost", false);
        let error = McpHttpConnector::from_config(&config, McpTlsMaterial::new()).unwrap_err();
        assert_provider_code(&error, "mcp_tls_indirect_material_unavailable");
    }

    #[test]
    fn file_root_is_read_only_below_explicit_canonical_project_root() {
        let temp = TestDir::new("file-root");
        let project_root = temp.path().join("project");
        fs::create_dir_all(project_root.join("certs")).unwrap();
        fs::write(project_root.join("certs/ca.pem"), TEST_CA_PEM).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let config = file_tls_config("file:certs/ca.pem");

        let connector = McpHttpConnector::from_config(
            &config,
            McpTlsMaterial::new().with_project_root(project_root.clone()),
        )
        .unwrap();

        assert!(matches!(connector, McpHttpConnector::Tls(_)));

        for relative in ["certs/ca.pem:stream", "certs/ca.pem::$DATA"] {
            let error = read_controlled_trust_file(&project_root, relative).unwrap_err();
            assert_provider_code(&error, "mcp_tls_trust_root_escape");
        }
    }

    #[test]
    fn file_root_rejects_symlink_or_reparse_escape() {
        let temp = TestDir::new("file-link");
        let project_root = temp.path().join("project");
        fs::create_dir_all(project_root.join("certs")).unwrap();
        let outside = temp.path().join("outside.pem");
        fs::write(&outside, TEST_CA_PEM).unwrap();
        let link = project_root.join("certs/ca.pem");
        if !create_file_symlink(&outside, &link) {
            return;
        }
        let project_root = fs::canonicalize(project_root).unwrap();
        let config = file_tls_config("file:certs/ca.pem");

        let error = McpHttpConnector::from_config(
            &config,
            McpTlsMaterial::new().with_project_root(project_root),
        )
        .unwrap_err();

        assert_provider_code(&error, "mcp_tls_trust_root_link");
    }

    #[test]
    fn handle_traversal_stays_bound_when_an_ancestor_name_is_replaced() {
        let temp = TestDir::new("ancestor-replacement");
        let project_root = temp.path().join("project");
        fs::create_dir_all(project_root.join("certs")).unwrap();
        fs::write(project_root.join("certs/ca.pem"), TEST_CA_PEM).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let hook_root = project_root.clone();
        let mut replacement_completed = false;

        let bytes = read_controlled_trust_file_with_hook(
            &project_root,
            "certs/ca.pem",
            |component_index| {
                assert_eq!(component_index, 0);
                assert!(!replacement_completed);
                fs::rename(hook_root.join("certs"), hook_root.join("certs-original")).unwrap();
                fs::create_dir(hook_root.join("certs")).unwrap();
                fs::write(hook_root.join("certs/ca.pem"), TEST_UNKNOWN_CA_PEM).unwrap();
                replacement_completed = true;
            },
        )
        .unwrap();

        assert!(replacement_completed);
        assert_eq!(bytes, TEST_CA_PEM);
        assert_eq!(
            fs::read(project_root.join("certs/ca.pem")).unwrap(),
            TEST_UNKNOWN_CA_PEM
        );
    }

    #[cfg(windows)]
    #[test]
    fn handle_traversal_rejects_an_open_ancestor_moved_outside_the_project_root() {
        let temp = TestDir::new("ancestor-moved-out");
        let project_root = temp.path().join("project");
        fs::create_dir_all(project_root.join("certs")).unwrap();
        fs::write(project_root.join("certs/ca.pem"), TEST_CA_PEM).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let hook_root = project_root.clone();
        let moved_ancestor = temp.path().join("moved-certs");

        let error = read_controlled_trust_file_with_hook(
            &project_root,
            "certs/ca.pem",
            |component_index| {
                assert_eq!(component_index, 0);
                fs::rename(hook_root.join("certs"), &moved_ancestor).unwrap();
            },
        )
        .unwrap_err();

        assert_provider_code(&error, "mcp_tls_trust_root_open_denied");
        assert_eq!(
            fs::read(moved_ancestor.join("ca.pem")).unwrap(),
            TEST_CA_PEM
        );
    }

    #[test]
    fn noncanonical_or_missing_project_root_fails_closed() {
        let config = file_tls_config("file:certs/ca.pem");
        let missing = McpHttpConnector::from_config(&config, McpTlsMaterial::new()).unwrap_err();
        assert_provider_code(&missing, "mcp_tls_project_root_required");

        let temp = TestDir::new("root-canonical");
        let project_root = temp.path().join("project");
        fs::create_dir_all(project_root.join("certs")).unwrap();
        fs::write(project_root.join("certs/ca.pem"), TEST_CA_PEM).unwrap();
        let noncanonical = project_root.join(".");
        let error = McpHttpConnector::from_config(
            &config,
            McpTlsMaterial::new().with_project_root(noncanonical),
        )
        .unwrap_err();
        assert_provider_code(&error, "mcp_tls_project_root_not_canonical");
    }

    #[test]
    fn connect_handshake_read_and_write_timeouts_are_classified() {
        let listener = TcpListener::bind(("localhost", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(150));
        });
        let config = tls_config(port, "localhost", false);
        let material = McpTlsMaterial::new()
            .with_indirect_trust_root("pem:test-ca", TEST_CA_PEM.to_vec())
            .unwrap();
        let connector = McpHttpConnector::from_config(&config, material).unwrap();
        let error = connector
            .connect(
                "https",
                "localhost",
                port,
                &format!("https://localhost:{port}"),
                Duration::from_millis(30),
            )
            .unwrap_err();
        assert_provider_code(&error, "mcp_tls_handshake_timeout");
        server.join().unwrap();

        let zero_connect = McpHttpConnector::Plaintext
            .connect("http", "127.0.0.1", 9, "http://127.0.0.1:9", Duration::ZERO)
            .unwrap_err();
        assert_provider_code(&zero_connect, "mcp_http_connect_timeout");

        let listener = TcpListener::bind(("localhost", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(150));
        });
        let origin = format!("http://localhost:{port}");
        let mut stream = McpHttpConnector::Plaintext
            .connect(
                "http",
                "localhost",
                port,
                &origin,
                Duration::from_millis(30),
            )
            .unwrap();
        let socket = match &stream {
            McpHttpStream::Plaintext(socket) => socket,
            McpHttpStream::Tls(_) => unreachable!(),
        };
        assert!(socket.read_timeout().unwrap().is_some());
        assert!(socket.write_timeout().unwrap().is_some());
        let mut byte = [0_u8; 1];
        let read = stream.read(&mut byte).unwrap_err();
        let read = map_http_read_error(read, &origin);
        assert_provider_code(&read, "mcp_http_read_timeout");
        server.join().unwrap();

        let read = map_http_read_error(
            io::Error::new(io::ErrorKind::TimedOut, "secret read detail"),
            "https://localhost",
        );
        assert_provider_code(&read, "mcp_http_read_timeout");
        assert!(!read.to_string().contains("secret read detail"));
        let write = map_http_write_error(
            io::Error::new(io::ErrorKind::WouldBlock, "secret write detail"),
            "https://localhost",
        );
        assert_provider_code(&write, "mcp_http_write_timeout");
        assert!(!write.to_string().contains("secret write detail"));
    }

    #[test]
    fn dns_resolution_obeys_the_caller_deadline_and_address_bound() {
        let resolver_baseline = ACTIVE_DNS_RESOLVER_THREADS.load(Ordering::Acquire);
        let (release_sender, release_receiver) = mpsc::channel();
        let started = Instant::now();
        let error = resolve_addresses_with(
            move || {
                release_receiver.recv().unwrap();
                Ok(vec!["127.0.0.1:9".parse().unwrap()])
            },
            "http://dns-timeout.invalid",
            started + Duration::from_millis(40),
        )
        .unwrap_err();
        assert_provider_code(&error, "mcp_http_dns_timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
        release_sender.send(()).unwrap();
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        while ACTIVE_DNS_RESOLVER_THREADS.load(Ordering::Acquire) > resolver_baseline
            && Instant::now() < cleanup_deadline
        {
            thread::yield_now();
        }
        assert!(ACTIVE_DNS_RESOLVER_THREADS.load(Ordering::Acquire) <= resolver_baseline);

        let error = resolve_addresses_with(
            || Ok(vec!["127.0.0.1:9".parse().unwrap(); MAX_DNS_ADDRESSES + 1]),
            "http://dns-limit.invalid",
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        assert_provider_code(&error, "mcp_http_dns_address_limit");

        static LOCAL_ACTIVE: AtomicUsize = AtomicUsize::new(0);
        let slot =
            DnsResolverSlot::acquire_from(&LOCAL_ACTIVE, 1, "http://dns-busy.invalid").unwrap();
        let error =
            DnsResolverSlot::acquire_from(&LOCAL_ACTIVE, 1, "http://dns-busy.invalid").unwrap_err();
        assert_provider_code(&error, "mcp_http_dns_capacity_exhausted");
        assert!(error.is_retryable());
        drop(slot);
        assert_eq!(LOCAL_ACTIVE.load(Ordering::Acquire), 0);
    }

    #[test]
    fn real_blocked_socket_write_cannot_outlive_the_request_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_sender, accepted_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (_socket, _) = listener.accept().unwrap();
            accepted_sender.send(()).unwrap();
            release_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
        });
        let socket = TcpStream::connect(address).unwrap();
        accepted_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let started = Instant::now();
        let mut stream =
            McpSharedTcpStream::new(socket, started + Duration::from_millis(100)).unwrap();
        let block = [0_u8; 64 * 1024];
        let mut written = 0_usize;
        let error = loop {
            match stream.write(&block) {
                Ok(0) => panic!("blocked TCP write returned zero bytes"),
                Ok(count) => {
                    written = written.saturating_add(count);
                    assert!(written <= 128 * 1024 * 1024, "socket never blocked");
                }
                Err(error) => break error,
            }
        };
        let error = map_http_write_error(error, "http://blocked-write.test");
        assert_provider_code(&error, "mcp_http_write_timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
        release_sender.send(()).unwrap();
        server.join().unwrap();
    }

    fn tls_config(port: u16, host: &str, client_auth: bool) -> McpStreamableHttpConfig {
        let client_auth = client_auth.then(|| {
            McpClientAuthConfig::new("env:MCP_CLIENT_CERT", "env:MCP_CLIENT_KEY").unwrap()
        });
        McpStreamableHttpConfig::from_parts(
            format!("https://{host}:{port}/mcp"),
            ["pem:test-ca"],
            client_auth,
            McpRedirectPolicy::Deny,
            Vec::<String>::new(),
        )
        .unwrap()
    }

    fn file_tls_config(reference: &str) -> McpStreamableHttpConfig {
        McpStreamableHttpConfig::from_parts(
            "https://localhost/mcp",
            [reference],
            None,
            McpRedirectPolicy::Deny,
            Vec::<String>::new(),
        )
        .unwrap()
    }

    fn assert_provider_code(error: &EvaError, expected: &str) {
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some(expected),
            "unexpected error: {error}"
        );
    }

    struct TestTlsServer {
        port: u16,
        thread: thread::JoinHandle<Result<Option<String>, String>>,
    }

    impl TestTlsServer {
        fn join(self) -> Result<Option<String>, String> {
            self.thread
                .join()
                .map_err(|_| "TLS fixture thread panicked".to_owned())?
        }
    }

    fn spawn_tls_server(
        certificate_pem: &'static [u8],
        require_client_auth: bool,
    ) -> TestTlsServer {
        let listener = TcpListener::bind(("localhost", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = test_server_config(certificate_pem, require_client_auth);
        let thread = thread::spawn(move || {
            let (mut socket, _) = listener.accept().map_err(|error| error.to_string())?;
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .and_then(|_| socket.set_write_timeout(Some(Duration::from_secs(2))))
                .map_err(|error| error.to_string())?;
            let mut connection =
                ServerConnection::new(config).map_err(|error| error.to_string())?;
            let handshake = connection.complete_io(&mut socket);
            let server_name = connection.server_name().map(str::to_owned);
            handshake.map_err(|error| error.to_string())?;
            Ok(server_name)
        });
        TestTlsServer { port, thread }
    }

    fn test_server_config(certificate_pem: &[u8], require_client_auth: bool) -> Arc<ServerConfig> {
        let certificates = parse_certificate_pem(certificate_pem, "test_server").unwrap();
        let private_key = parse_private_key_pem(TEST_SERVER_KEY).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap();
        let config = if require_client_auth {
            let mut client_roots = RootCertStore::empty();
            for certificate in parse_certificate_pem(TEST_CA_PEM, "test_client_ca").unwrap() {
                client_roots.add(certificate).unwrap();
            }
            let verifier =
                WebPkiClientVerifier::builder_with_provider(Arc::new(client_roots), provider)
                    .build()
                    .unwrap();
            builder
                .with_client_cert_verifier(verifier)
                .with_single_cert(certificates, private_key)
                .unwrap()
        } else {
            builder
                .with_no_client_auth()
                .with_single_cert(certificates, private_key)
                .unwrap()
        };
        Arc::new(config)
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "eva-mcp-w4l02-{name}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn create_file_symlink(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }

    #[cfg(windows)]
    fn create_file_symlink(target: &Path, link: &Path) -> bool {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => false,
            Err(error) => panic!("failed to create TLS fixture symlink: {error}"),
        }
    }
}
