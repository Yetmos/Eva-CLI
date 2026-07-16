//! 产物存储契约、内存实现和带完整性校验的本地文件系统实现。
//! Artifact store contracts and the V0.4 in-memory implementation.

use eva_core::EvaError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：持久化不透明产物字节，并在读取边界校验 key、大小和 SHA-256。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "artifact store interfaces and integrity boundaries";

/// 已存产物字节及确定性 SHA-256、内容类型和保留策略 metadata。
/// Stored artifact bytes and deterministic SHA-256 digest metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// 仓库内稳定相对 key；文件系统实现将 `/` 分段映射为子目录。
    pub key: String,
    /// 原始不透明产物内容。
    pub bytes: Vec<u8>,
    /// 带 `sha256:` 前缀的小写十六进制摘要。
    pub digest: String,
    /// 写入时记录的字节数，读取时必须与实际文件长度一致。
    pub size_bytes: usize,
    /// 已校验的 MIME 风格内容类型。
    pub content_type: String,
    /// 已校验的保留策略标识；策略执行属于上层服务。
    pub retention_policy: String,
    /// 可选保留截止 epoch 毫秒，仅作为 metadata 保存。
    pub retain_until_ms: Option<u128>,
}

/// 上层依赖的最小产物存储行为。
/// Minimal artifact store behavior retained for V0.4 module completeness.
pub trait ArtifactStore {
    /// 保存字节并返回包含摘要的完整记录；无效 key 或 I/O 失败不得报告成功。
    fn put_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<ArtifactRecord, EvaError>;
    /// 获取产物的兼容便捷接口；实现可能把详细读取错误折叠为 None。
    /// 需要区分“缺失”和“损坏/I/O 失败”的调用方应使用文件系统实现的 `try_get_bytes`。
    fn get_bytes(&self, key: &str) -> Option<ArtifactRecord>;
}

/// 测试和无持久化路径使用的内存产物存储。
/// In-memory artifact store for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryArtifactStore {
    /// 按 key 有序保存记录，使测试枚举和比较结果稳定。
    records: BTreeMap<String, ArtifactRecord>,
}

/// 用于本地 durable 证据的文件系统产物存储。
/// Filesystem-backed artifact store for durable local evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemArtifactStore {
    /// Store 根目录；对象与 metadata 分别位于其 `objects` 和 `metadata` 子树。
    root: PathBuf,
}

impl InMemoryArtifactStore {
    /// 创建空的进程内产物存储。
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystemArtifactStore {
    /// 创建指向给定根目录的 store 句柄；目录延迟到首次写入时创建。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// 返回未经重新解释的 store 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 保存字节及显式内容/保留 metadata；所有 metadata 在写磁盘前校验。
    pub fn put_bytes_with_metadata(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        content_type: impl Into<String>,
        retention_policy: impl Into<String>,
        retain_until_ms: Option<u128>,
    ) -> Result<ArtifactRecord, EvaError> {
        self.put_bytes_inner(
            key.into(),
            bytes.into(),
            content_type.into(),
            retention_policy.into(),
            retain_until_ms,
        )
    }

    /// 读取产物并验证对象文件与 metadata 的 key、size 和 digest 一致性。
    ///
    /// 对象文件缺失返回 `Ok(None)`；metadata 缺失、字段损坏、摘要不匹配或其他 I/O 故障
    /// 返回结构化错误。这样半写文件或外部篡改不会被误报为有效产物。
    pub fn try_get_bytes(&self, key: &str) -> Result<Option<ArtifactRecord>, EvaError> {
        let key = validate_filesystem_artifact_key(key.to_owned())?;
        let artifact_path = keyed_path(&self.root.join("objects"), &key, "artifact");
        let metadata_path = keyed_path(&self.root.join("metadata"), &key, "metadata");

        let bytes = match fs::read(&artifact_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(filesystem_error(
                    "failed to read artifact bytes",
                    &key,
                    &artifact_path,
                    error,
                ));
            }
        };

        let metadata = read_metadata(&metadata_path, &key)?;
        let actual_digest = sha256_digest(&bytes);
        if metadata.key != key {
            return Err(EvaError::conflict("artifact metadata key mismatch")
                .with_context("artifact_key", key)
                .with_context("metadata_key", metadata.key));
        }
        if metadata.size_bytes != bytes.len() {
            return Err(EvaError::conflict("artifact size mismatch")
                .with_context("artifact_key", key)
                .with_context("expected_size", metadata.size_bytes.to_string())
                .with_context("actual_size", bytes.len().to_string()));
        }
        if metadata.digest != actual_digest {
            return Err(EvaError::conflict("artifact digest mismatch")
                .with_context("artifact_key", key)
                .with_context("expected_digest", metadata.digest)
                .with_context("actual_digest", actual_digest));
        }

        Ok(Some(ArtifactRecord {
            key,
            bytes,
            digest: actual_digest,
            size_bytes: metadata.size_bytes,
            content_type: metadata.content_type,
            retention_policy: metadata.retention_policy,
            retain_until_ms: metadata.retain_until_ms,
        }))
    }

    /// 文件系统写入的共享实现。
    ///
    /// 当前格式使用独立 object/metadata 文件，并按“先字节、后 metadata”顺序直接写入，
    /// 不承诺跨两文件原子性；第二步失败可能留下孤立 object。读取端必须完成完整性校验，
    /// 上层若需要事务原子性应在更高层使用临时目录/提交协议，而不能假定这里已有 CAS。
    fn put_bytes_inner(
        &mut self,
        key: String,
        bytes: Vec<u8>,
        content_type: String,
        retention_policy: String,
        retain_until_ms: Option<u128>,
    ) -> Result<ArtifactRecord, EvaError> {
        let key = validate_filesystem_artifact_key(key)?;
        let content_type = validate_content_type(content_type)?;
        let retention_policy = validate_retention_policy(retention_policy)?;
        let digest = sha256_digest(&bytes);
        let size_bytes = bytes.len();
        let artifact_path = keyed_path(&self.root.join("objects"), &key, "artifact");
        let metadata_path = keyed_path(&self.root.join("metadata"), &key, "metadata");

        if let Some(parent) = artifact_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                filesystem_error("failed to create artifact directory", &key, parent, error)
            })?;
        }
        if let Some(parent) = metadata_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                filesystem_error(
                    "failed to create artifact metadata directory",
                    &key,
                    parent,
                    error,
                )
            })?;
        }

        fs::write(&artifact_path, &bytes).map_err(|error| {
            filesystem_error(
                "failed to write artifact bytes",
                &key,
                &artifact_path,
                error,
            )
        })?;
        let metadata = ArtifactMetadata {
            key: key.clone(),
            digest: digest.clone(),
            size_bytes,
            content_type: content_type.clone(),
            retention_policy: retention_policy.clone(),
            retain_until_ms,
        };
        fs::write(&metadata_path, metadata.to_storage()).map_err(|error| {
            filesystem_error(
                "failed to write artifact metadata",
                &key,
                &metadata_path,
                error,
            )
        })?;

        Ok(ArtifactRecord {
            key,
            bytes,
            digest,
            size_bytes,
            content_type,
            retention_policy,
            retain_until_ms,
        })
    }
}

impl ArtifactRecord {
    /// 从 key 与字节构造内存记录，并计算默认 metadata 和确定性摘要。
    pub fn new(key: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let key = key.into();
        let bytes = bytes.into();
        let digest = sha256_digest(&bytes);
        let size_bytes = bytes.len();
        Self {
            key,
            bytes,
            digest,
            size_bytes,
            content_type: default_content_type(),
            retention_policy: default_retention_policy(),
            retain_until_ms: None,
        }
    }
}

impl ArtifactStore for InMemoryArtifactStore {
    /// 校验非空 key 后覆盖同 key 记录；返回值与 map 中保存的记录完全一致。
    fn put_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<ArtifactRecord, EvaError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(EvaError::invalid_argument("artifact key cannot be empty"));
        }
        let bytes = bytes.into();
        let record = ArtifactRecord::new(key.clone(), bytes);
        self.records.insert(key, record.clone());
        Ok(record)
    }

    /// 克隆返回指定内存记录；缺失返回 None。
    fn get_bytes(&self, key: &str) -> Option<ArtifactRecord> {
        self.records.get(key).cloned()
    }
}

impl ArtifactStore for FileSystemArtifactStore {
    /// 使用默认内容类型和永久保留策略写入文件系统产物。
    fn put_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<ArtifactRecord, EvaError> {
        self.put_bytes_inner(
            key.into(),
            bytes.into(),
            default_content_type(),
            default_retention_policy(),
            None,
        )
    }

    /// 兼容 trait 的宽松读取：任何详细错误都折叠为 None；严格调用方应使用 `try_get_bytes`。
    fn get_bytes(&self, key: &str) -> Option<ArtifactRecord> {
        self.try_get_bytes(key).ok().flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 与 object 文件配对的磁盘 metadata；版本 2 增加内容类型和保留字段。
struct ArtifactMetadata {
    /// 必须与请求 key 完全一致。
    key: String,
    /// 写入时计算的 SHA-256。
    digest: String,
    /// 写入时的 object 字节数。
    size_bytes: usize,
    /// MIME 风格内容类型。
    content_type: String,
    /// 保留策略稳定标识。
    retention_policy: String,
    /// 可选保留截止时间。
    retain_until_ms: Option<u128>,
}

/// 计算带算法前缀的稳定 SHA-256 文本；字符串写入内存不可能失败。
pub fn sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();

    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

/// 校验可安全映射到文件系统的相对 artifact key。
/// 拒绝首尾空白、反斜线、空/`.`/`..` 分段和非稳定 ASCII 字符，防止目录穿越、
/// 平台分隔符歧义和同一逻辑 key 在不同系统落到不同路径。
pub(crate) fn validate_filesystem_artifact_key(key: String) -> Result<String, EvaError> {
    if key.trim().is_empty() || key.trim() != key || key.contains('\\') {
        return Err(
            EvaError::invalid_argument("artifact key must be a stable relative path")
                .with_context("artifact_key", key),
        );
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(
                EvaError::invalid_argument("artifact key must be a stable relative path")
                    .with_context("artifact_key", key),
            );
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(EvaError::invalid_argument(
                "artifact key contains unsupported filesystem characters",
            )
            .with_context("artifact_key", key));
        }
    }
    Ok(key)
}

/// 将已校验的 `/` 分段 key 映射到目录树，并只给最后一段添加存储扩展名。
/// 例如 `backup/run` 在 object 根下变为 `backup/run.artifact`。
fn keyed_path(root: &Path, key: &str, extension: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    let mut segments = key.split('/').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_some() {
            path.push(segment);
        } else {
            path.push(format!("{segment}.{extension}"));
        }
    }
    path
}

/// 返回旧 metadata 和普通 `put_bytes` 使用的二进制默认内容类型。
fn default_content_type() -> String {
    "application/octet-stream".to_owned()
}

/// 返回旧 metadata 和普通写入使用的默认永久保留标识。
fn default_retention_policy() -> String {
    "retain".to_owned()
}

/// 校验非空、无边界空白、含 `/` 且无控制字符的 MIME 风格内容类型。
fn validate_content_type(value: String) -> Result<String, EvaError> {
    if value.trim().is_empty()
        || value.trim() != value
        || !value.contains('/')
        || value.chars().any(char::is_control)
    {
        return Err(
            EvaError::invalid_argument("artifact content type is invalid")
                .with_context("content_type", value),
        );
    }
    Ok(value)
}

/// 校验可稳定写入逐行格式的保留策略标识，仅允许 ASCII 字母数字、短横线和下划线。
fn validate_retention_policy(value: String) -> Result<String, EvaError> {
    if value.trim().is_empty()
        || value.trim() != value
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            EvaError::invalid_argument("artifact retention policy is invalid")
                .with_context("retention_policy", value),
        );
    }
    Ok(value)
}

/// 读取并解析 metadata；缺失与其他 I/O 错误保留不同消息，同时附加 key/path 上下文。
fn read_metadata(path: &Path, key: &str) -> Result<ArtifactMetadata, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "artifact metadata is missing"
        } else {
            "failed to read artifact metadata"
        };
        filesystem_error(message, key, path, error)
    })?;
    parse_metadata(&data).map_err(|error| {
        error
            .with_context("artifact_key", key)
            .with_context("path", path.display().to_string())
    })
}

/// 解析 `name=value` metadata 磁盘格式并执行版本兼容。
///
/// 无 version 的旧格式及 v1/v2 均可读；旧记录缺少的新字段回退默认值。未知字段、未知版本、
/// 缺必填 key/digest/size 或字段格式损坏均返回 Conflict，表示磁盘事实不可信而非用户输入错误。
fn parse_metadata(data: &str) -> Result<ArtifactMetadata, EvaError> {
    let mut version = None;
    let mut key = None;
    let mut digest = None;
    let mut size_bytes = None;
    let mut content_type = None;
    let mut retention_policy = None;
    let mut retain_until_ms = None;
    for line in data.lines() {
        let Some((name, value)) = line.split_once('=') else {
            return Err(EvaError::conflict("artifact metadata is invalid"));
        };
        match name {
            "version" => version = Some(value.to_owned()),
            "key" => key = Some(value.to_owned()),
            "digest" => digest = Some(value.to_owned()),
            "size_bytes" => {
                size_bytes = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| EvaError::conflict("artifact metadata is invalid"))?,
                );
            }
            "content_type" => content_type = Some(value.to_owned()),
            "retention_policy" => retention_policy = Some(value.to_owned()),
            "retain_until_ms" => {
                retain_until_ms = if value.is_empty() {
                    None
                } else {
                    Some(
                        value
                            .parse::<u128>()
                            .map_err(|_| EvaError::conflict("artifact metadata is invalid"))?,
                    )
                };
            }
            _ => return Err(EvaError::conflict("artifact metadata is invalid")),
        }
    }
    if !matches!(version.as_deref(), None | Some("1") | Some("2")) {
        return Err(EvaError::conflict(
            "artifact metadata version is unsupported",
        ));
    }
    let content_type = validate_content_type(content_type.unwrap_or_else(default_content_type))
        .map_err(|_| {
            EvaError::conflict("artifact metadata is invalid").with_context("field", "content_type")
        })?;
    let retention_policy =
        validate_retention_policy(retention_policy.unwrap_or_else(default_retention_policy))
            .map_err(|_| {
                EvaError::conflict("artifact metadata is invalid")
                    .with_context("field", "retention_policy")
            })?;
    Ok(ArtifactMetadata {
        key: key.ok_or_else(|| EvaError::conflict("artifact metadata is invalid"))?,
        digest: digest.ok_or_else(|| EvaError::conflict("artifact metadata is invalid"))?,
        size_bytes: size_bytes.ok_or_else(|| EvaError::conflict("artifact metadata is invalid"))?,
        content_type,
        retention_policy,
        retain_until_ms,
    })
}

impl ArtifactMetadata {
    /// 序列化为 v2 逐行格式；可选截止时间以空值表示 None，并保留结尾换行。
    fn to_storage(&self) -> String {
        format!(
            "version=2\nkey={}\ndigest={}\nsize_bytes={}\ncontent_type={}\nretention_policy={}\nretain_until_ms={}\n",
            self.key,
            self.digest,
            self.size_bytes,
            self.content_type,
            self.retention_policy,
            self.retain_until_ms
                .map(|value| value.to_string())
                .unwrap_or_default()
        )
    }
}

/// 将底层文件系统错误映射为带 artifact key、路径和原始 I/O 文本的内部错误。
fn filesystem_error(message: &str, key: &str, path: &Path, error: std::io::Error) -> EvaError {
    EvaError::internal(message)
        .with_context("artifact_key", key)
        .with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

#[cfg(test)]
/// ArtifactStore 完整性、兼容性、路径安全和损坏数据失败语义的回归测试。
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    /// 验证内存 store 往返保持确定性 SHA-256。
    fn artifact_round_trip_preserves_digest() {
        let mut store = InMemoryArtifactStore::new();

        let record = store.put_bytes("trace/basic", b"ok".as_slice()).unwrap();
        let loaded = store.get_bytes("trace/basic").unwrap();

        assert_eq!(
            record.digest,
            "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df"
        );
        assert_eq!(loaded.bytes, b"ok");
    }

    #[test]
    /// 验证文件系统 store 重开读取时保持字节、key、size 和摘要。
    fn filesystem_artifact_store_round_trips_bytes_and_digest() {
        let root = test_root("round-trip");
        let mut store = FileSystemArtifactStore::new(root.path());

        let record = store.put_bytes("backup/basic", b"ok".as_slice()).unwrap();
        let loaded = store.get_bytes("backup/basic").unwrap();

        assert_eq!(store.root(), root.path());
        assert_eq!(loaded, record);
        assert_eq!(loaded.bytes, b"ok");
        assert!(loaded.digest.starts_with("sha256:"));
    }

    #[test]
    /// 验证 v2 metadata 的内容类型、保留策略和截止时间完整往返。
    fn filesystem_artifact_store_round_trips_metadata() {
        let root = test_root("metadata-round-trip");
        let mut store = FileSystemArtifactStore::new(root.path());

        let record = store
            .put_bytes_with_metadata(
                "backup/metadata",
                b"ok".as_slice(),
                "application/json",
                "retain-until",
                Some(1_789_000_000_000),
            )
            .unwrap();
        let loaded = store.try_get_bytes("backup/metadata").unwrap().unwrap();
        let metadata_path =
            keyed_path(&root.path().join("metadata"), "backup/metadata", "metadata");
        let metadata = fs::read_to_string(metadata_path).unwrap();

        assert_eq!(loaded, record);
        assert_eq!(loaded.size_bytes, 2);
        assert_eq!(loaded.content_type, "application/json");
        assert_eq!(loaded.retention_policy, "retain-until");
        assert_eq!(loaded.retain_until_ms, Some(1_789_000_000_000));
        assert!(metadata.contains("version=2\n"));
        assert!(metadata.contains("content_type=application/json\n"));
        assert!(metadata.contains("retention_policy=retain-until\n"));
        assert!(metadata.contains("retain_until_ms=1789000000000\n"));
    }

    #[test]
    /// 验证无版本旧 metadata 可读，并为新增字段应用兼容默认值。
    fn filesystem_artifact_store_reads_legacy_metadata_defaults() {
        let root = test_root("legacy-metadata");
        let mut store = FileSystemArtifactStore::new(root.path());
        let record = store.put_bytes("backup/legacy", b"ok".as_slice()).unwrap();
        let metadata_path = keyed_path(&root.path().join("metadata"), "backup/legacy", "metadata");
        fs::write(
            metadata_path,
            format!(
                "key={}\ndigest={}\nsize_bytes={}\n",
                record.key, record.digest, record.size_bytes
            ),
        )
        .unwrap();

        let loaded = store.try_get_bytes("backup/legacy").unwrap().unwrap();

        assert_eq!(loaded.content_type, default_content_type());
        assert_eq!(loaded.retention_policy, default_retention_policy());
        assert_eq!(loaded.retain_until_ms, None);
    }

    #[test]
    /// 验证 object 被篡改后读取返回 Conflict 而非损坏字节。
    fn filesystem_artifact_store_reports_digest_mismatch() {
        let root = test_root("digest-mismatch");
        let mut store = FileSystemArtifactStore::new(root.path());
        store.put_bytes("backup/tamper", b"ok".as_slice()).unwrap();
        let artifact_path = keyed_path(&root.path().join("objects"), "backup/tamper", "artifact");
        fs::write(artifact_path, b"changed").unwrap();

        let error = store.try_get_bytes("backup/tamper").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证损坏 content_type 被视为磁盘冲突。
    fn filesystem_artifact_store_reports_corrupt_metadata_content_type() {
        let root = test_root("corrupt-content-type");
        let mut store = FileSystemArtifactStore::new(root.path());
        let record = store
            .put_bytes("backup/content-type", b"ok".as_slice())
            .unwrap();
        let metadata_path = keyed_path(
            &root.path().join("metadata"),
            "backup/content-type",
            "metadata",
        );
        fs::write(
            metadata_path,
            format!(
                "version=2\nkey={}\ndigest={}\nsize_bytes={}\ncontent_type=plain\nretention_policy=retain\nretain_until_ms=\n",
                record.key, record.digest, record.size_bytes
            ),
        )
        .unwrap();

        let error = store.try_get_bytes("backup/content-type").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证包含路径字符的 retention policy 被视为损坏 metadata。
    fn filesystem_artifact_store_reports_corrupt_metadata_retention_policy() {
        let root = test_root("corrupt-retention-policy");
        let mut store = FileSystemArtifactStore::new(root.path());
        let record = store
            .put_bytes("backup/retention-policy", b"ok".as_slice())
            .unwrap();
        let metadata_path = keyed_path(
            &root.path().join("metadata"),
            "backup/retention-policy",
            "metadata",
        );
        fs::write(
            metadata_path,
            format!(
                "version=2\nkey={}\ndigest={}\nsize_bytes={}\ncontent_type=application/octet-stream\nretention_policy=retain/now\nretain_until_ms=\n",
                record.key, record.digest, record.size_bytes
            ),
        )
        .unwrap();

        let error = store.try_get_bytes("backup/retention-policy").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证对象文件真正缺失时严格和兼容读取均返回 None。
    fn filesystem_artifact_store_returns_none_for_missing_artifacts() {
        let root = test_root("missing");
        let store = FileSystemArtifactStore::new(root.path());

        assert!(store.get_bytes("backup/missing").is_none());
        assert!(store.try_get_bytes("backup/missing").unwrap().is_none());
    }

    #[test]
    /// 验证目录穿越 key 在创建任何文件前被拒绝。
    fn filesystem_artifact_store_rejects_unsafe_keys() {
        let root = test_root("unsafe-key");
        let mut store = FileSystemArtifactStore::new(root.path());

        let error = store
            .put_bytes("../escape", b"nope".as_slice())
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    /// 测试专用临时根目录，Drop 时尽力清理。
    struct TestRoot {
        /// 唯一临时路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 返回临时根路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 测试结束时递归清理；清理失败不覆盖原测试结果。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 使用用例名、进程 ID 和纳秒时间创建并行安全的测试路径。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir()
                .join(format!("eva-storage-{name}-{}-{now}", std::process::id())),
        }
    }
}
