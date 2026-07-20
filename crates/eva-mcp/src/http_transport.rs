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
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

const MAX_TLS_MATERIAL_BYTES: usize = 1024 * 1024;

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
#[derive(Clone)]
pub(crate) struct McpSharedTcpStream {
    socket: Arc<TcpStream>,
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
        self.socket.as_ref().read(buffer)
    }
}

impl Write for McpSharedTcpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.socket.as_ref().write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.as_ref().flush()
    }
}

impl McpSharedTcpStream {
    fn new(socket: TcpStream) -> Self {
        Self {
            socket: Arc::new(socket),
        }
    }
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
    pub(crate) fn connect(
        &self,
        scheme: &str,
        host: &str,
        port: u16,
        origin: &str,
        timeout: Duration,
    ) -> Result<McpHttpStream, EvaError> {
        match (self, scheme) {
            (Self::Plaintext, "http") => {
                let stream = connect_tcp(host, port, origin, timeout)?;
                Ok(McpHttpStream::Plaintext(McpSharedTcpStream::new(stream)))
            }
            (Self::Tls(config), "https") => {
                let mut stream = connect_tcp(host, port, origin, timeout)?;
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
                Ok(McpHttpStream::Tls(Box::new(StreamOwned::new(
                    connection,
                    McpSharedTcpStream::new(stream),
                ))))
            }
            _ => Err(
                EvaError::internal("MCP HTTP connector does not match endpoint scheme")
                    .with_provider_code("mcp_http_connector_scheme_mismatch")
                    .with_context("origin", origin),
            ),
        }
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
    if canonical_root != project_root {
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

    let path = canonical_root.join(relative);
    validate_trust_path_components(&canonical_root, relative)?;
    let canonical_path = fs::canonicalize(&path).map_err(|_| {
        EvaError::not_found("MCP TLS trust root file is unavailable")
            .with_provider_code("mcp_tls_trust_root_unavailable")
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(EvaError::permission_denied(
            "MCP TLS trust root path escapes its controlled project root",
        )
        .with_provider_code("mcp_tls_trust_root_escape"));
    }

    let mut file = open_file_no_follow(&path).map_err(|_| {
        EvaError::permission_denied("MCP TLS trust root file could not be opened safely")
            .with_provider_code("mcp_tls_trust_root_open_denied")
    })?;
    validate_trust_path_components(&canonical_root, relative)?;
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

fn open_file_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000 | 0x0000_0800); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0000_0100 | 0x0000_0004); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    options.open(path)
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
    timeout: Duration,
) -> Result<TcpStream, EvaError> {
    if timeout.is_zero() {
        return Err(EvaError::timeout("MCP HTTP connection timed out")
            .with_provider_code("mcp_http_connect_timeout")
            .with_context("origin", origin));
    }
    let addresses = (host, port).to_socket_addrs().map_err(|_| {
        EvaError::unavailable("failed to resolve MCP HTTP server")
            .with_provider_code("mcp_http_dns_failed")
            .with_context("origin", origin)
    })?;
    let addresses = addresses.collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(
            EvaError::unavailable("MCP HTTP server host did not resolve")
                .with_provider_code("mcp_http_dns_empty")
                .with_context("origin", origin),
        );
    }

    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        EvaError::invalid_argument("MCP HTTP connection timeout is out of range")
            .with_provider_code("mcp_http_timeout_invalid")
            .with_context("origin", origin)
    })?;
    let mut last_error = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(timeout))
                    .and_then(|_| stream.set_write_timeout(Some(timeout)))
                    .map_err(|_| {
                        EvaError::unavailable("failed to configure MCP HTTP socket timeouts")
                            .with_provider_code("mcp_http_timeout_config_failed")
                            .with_context("origin", origin)
                    })?;
                return Ok(stream);
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
