//! 带签名且可选密封的备份归档契约。
//! Signed and optionally sealed backup archive contracts.

use eva_core::EvaError;
use eva_storage::ArtifactRecord;
use std::fmt;

/// 本模块的架构职责：定义签名备份归档与远端目标契约。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "signed backup archive and remote target contracts";

/// 当前支持的备份归档格式标识。
const ARCHIVE_FORMAT: &str = "eva.backup.archive.v1";
/// 当前归档签名载荷算法标识。
const SIGNATURE_ALGORITHM: &str = "sha256-keyed-v1";
/// 当前可逆异或摘要流密封算法标识。
const ENCRYPTION_ALGORITHM: &str = "xor-sha256-stream-v1";

/// 备份归档签名密钥。
///
/// 自定义 `Debug` 实现会隐藏密钥材料，但克隆值仍包含明文 secret，调用方必须避免
/// 序列化或长期留存在日志中。
#[derive(Clone, PartialEq, Eq)]
pub struct BackupSigningKey {
    /// 写入签名清单并用于选择验证密钥的稳定标识。
    key_id: String,
    /// 参与签名摘要计算的敏感密钥材料。
    secret: String,
}

/// 备份归档密封密钥。
///
/// 当前算法是项目内格式契约，不提供标准认证加密保证；生产环境应在受控存储或
/// 外层加密系统中管理归档与密钥。
#[derive(Clone, PartialEq, Eq)]
pub struct BackupEncryptionKey {
    /// 写入加密清单并用于选择解密密钥的稳定标识。
    key_id: String,
    /// 生成可逆摘要流的敏感密钥材料。
    secret: String,
}

/// 归档清单中绑定内容与元数据的签名描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSignatureManifest {
    /// 验证签名时应使用的密钥标识。
    pub key_id: String,
    /// 签名载荷算法标识。
    pub algorithm: String,
    /// 规范化签名载荷的摘要值。
    pub value: String,
}

/// 可选归档密封的解密元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEncryptionManifest {
    /// 解密时应使用的密钥标识。
    pub key_id: String,
    /// 密封算法标识。
    pub algorithm: String,
    /// 解密后必须匹配的明文摘要。
    pub plaintext_checksum: String,
}

/// 备份归档可声明的远端存储类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteBackupTargetKind {
    /// 文件系统镜像目录。
    FilesystemMirror,
    /// 通用对象存储。
    ObjectStore,
    /// 兼容 S3 API 的对象存储。
    S3Compatible,
}

/// 写入归档清单的远端复制目标描述。
///
/// 该类型只声明目标，本模块不会上传归档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBackupTarget {
    /// 远端目标类别。
    pub kind: RemoteBackupTargetKind,
    /// 目标端点或镜像根路径。
    pub endpoint: String,
    /// 目标内的稳定相对前缀。
    pub prefix: String,
    /// 后续编排是否必须成功完成远端复制。
    pub required: bool,
}

/// 校验和、签名、密封与远端复制元数据的归档清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupArchiveManifest {
    /// 归档格式版本。
    pub format: String,
    /// 归档在 ArtifactStore 中的稳定键。
    pub artifact_key: String,
    /// 实际存储字节的摘要。
    pub checksum: String,
    /// 密封前明文字节的摘要。
    pub plaintext_checksum: String,
    /// 是否存在密封元数据。
    pub encrypted: bool,
    /// 绑定归档内容及关键元数据的签名清单。
    pub signature: BackupSignatureManifest,
    /// 密封归档的解密元数据；明文归档为 `None`。
    pub encryption: Option<BackupEncryptionManifest>,
    /// 可选远端复制目标声明。
    pub remote_target: Option<RemoteBackupTarget>,
}

/// 已密封、可直接写入 ArtifactStore 的归档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedBackupArchive {
    /// 明文或经过可逆密封后的存储字节。
    pub bytes: Vec<u8>,
    /// 与存储字节和原始明文绑定的清单。
    pub manifest: BackupArchiveManifest,
}

/// 签名验证成功后的比对证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSignatureVerification {
    /// 实际使用的签名密钥标识。
    pub key_id: String,
    /// 已验证的签名算法。
    pub algorithm: String,
    /// 根据清单和密钥重新计算的签名。
    pub expected_signature: String,
    /// 清单携带的签名。
    pub actual_signature: String,
    /// 签名是否匹配；成功返回时为 `true`。
    pub verified: bool,
}

/// 负责归档密封和打开操作的无状态编解码器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupArchiveCodec;

/// 负责格式、密钥标识和签名载荷验证的无状态校验器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupArchiveVerifier;

impl BackupSigningKey {
    /// 创建具有合法稳定标识和非空 secret 的签名密钥。
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, EvaError> {
        let key_id = validate_key_id("backup signing key id", key_id.into())?;
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup signing key secret cannot be empty",
            ));
        }
        Ok(Self { key_id, secret })
    }

    /// 返回仅供本地开发和测试使用的固定签名密钥。
    pub fn local_development() -> Self {
        Self {
            key_id: "eva-local-dev-signing-key".to_owned(),
            secret: "eva-local-development-backup-signing-secret".to_owned(),
        }
    }

    /// 返回非敏感密钥标识。
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl BackupEncryptionKey {
    /// 创建具有合法稳定标识和非空 secret 的密封密钥。
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, EvaError> {
        let key_id = validate_key_id("backup encryption key id", key_id.into())?;
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup encryption key secret cannot be empty",
            ));
        }
        Ok(Self { key_id, secret })
    }

    /// 返回仅供本地开发和测试使用的固定密封密钥。
    pub fn local_development() -> Self {
        Self {
            key_id: "eva-local-dev-encryption-key".to_owned(),
            secret: "eva-local-development-backup-encryption-secret".to_owned(),
        }
    }

    /// 返回非敏感密钥标识。
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl fmt::Debug for BackupSigningKey {
    /// 输出可诊断密钥标识，同时始终遮蔽签名 secret。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackupSigningKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for BackupEncryptionKey {
    /// 输出可诊断密钥标识，同时始终遮蔽密封 secret。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackupEncryptionKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl RemoteBackupTargetKind {
    /// 返回写入签名载荷的稳定目标类别字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemMirror => "filesystem_mirror",
            Self::ObjectStore => "object_store",
            Self::S3Compatible => "s3_compatible",
        }
    }
}

impl RemoteBackupTarget {
    /// 校验端点和相对前缀后创建远端目标声明。
    pub fn new(
        kind: RemoteBackupTargetKind,
        endpoint: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let endpoint = endpoint.into();
        let prefix = prefix.into();
        if endpoint.trim().is_empty() || endpoint.trim() != endpoint {
            return Err(EvaError::invalid_argument(
                "remote backup target endpoint must be non-empty and trimmed",
            ));
        }
        if prefix.trim().is_empty()
            || prefix.trim() != prefix
            || prefix.contains("..")
            || prefix.contains('\\')
        {
            return Err(EvaError::invalid_argument(
                "remote backup target prefix must be a stable relative prefix",
            )
            .with_context("prefix", prefix));
        }
        Ok(Self {
            kind,
            endpoint,
            prefix,
            required: false,
        })
    }

    /// 标记后续备份编排必须完成该远端复制。
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

impl BackupArchiveCodec {
    /// 计算明文摘要、可选密封、计算存储摘要并最后签名全部关键元数据。
    ///
    /// 签名载荷同时绑定 artifact key、两类摘要、密封参数和远端目标，防止攻击者在
    /// 不破坏签名的情况下替换存储位置或降低远端复制要求。返回值尚未持久化。
    pub fn seal(
        artifact_key: impl Into<String>,
        plaintext: Vec<u8>,
        signing_key: &BackupSigningKey,
        encryption_key: Option<&BackupEncryptionKey>,
        remote_target: Option<RemoteBackupTarget>,
    ) -> SealedBackupArchive {
        let artifact_key = artifact_key.into();
        let plaintext_checksum = digest_bytes(&plaintext);
        let (bytes, encryption) = match encryption_key {
            Some(key) => {
                let sealed = xor_with_key(&plaintext, key);
                (
                    sealed,
                    Some(BackupEncryptionManifest {
                        key_id: key.key_id.clone(),
                        algorithm: ENCRYPTION_ALGORITHM.to_owned(),
                        plaintext_checksum: plaintext_checksum.clone(),
                    }),
                )
            }
            None => (plaintext, None),
        };
        let checksum = digest_bytes(&bytes);
        let signature = sign_archive(
            &artifact_key,
            &checksum,
            &plaintext_checksum,
            encryption.as_ref(),
            remote_target.as_ref(),
            signing_key,
        );
        let manifest = BackupArchiveManifest {
            format: ARCHIVE_FORMAT.to_owned(),
            artifact_key,
            checksum,
            plaintext_checksum,
            encrypted: encryption.is_some(),
            signature,
            encryption,
            remote_target,
        };
        SealedBackupArchive { bytes, manifest }
    }

    /// 验证存储字节、按清单可选解密，并验证恢复出的明文摘要。
    ///
    /// 校验顺序不可交换：先根据 ArtifactRecord 和清单拒绝损坏的密文，再检查 artifact
    /// key，之后才选择匹配 key id 的解密密钥，最后用明文摘要检测错误密钥或篡改。
    /// 本方法不验证签名，调用方须在信任清单前单独执行 `verify_signature`。
    pub fn open(
        record: &ArtifactRecord,
        manifest: &BackupArchiveManifest,
        encryption_key: Option<&BackupEncryptionKey>,
    ) -> Result<Vec<u8>, EvaError> {
        verify_record_checksum(record, &manifest.checksum)?;
        if manifest.artifact_key != record.key {
            return Err(EvaError::conflict("backup archive artifact key mismatch")
                .with_context("expected_artifact_key", &manifest.artifact_key)
                .with_context("actual_artifact_key", &record.key));
        }
        let plaintext = match &manifest.encryption {
            Some(encryption) => {
                let key = encryption_key.ok_or_else(|| {
                    EvaError::permission_denied("encrypted backup archive requires decryption key")
                        .with_context("artifact_key", &record.key)
                })?;
                if encryption.key_id != key.key_id {
                    return Err(EvaError::permission_denied(
                        "backup archive decryption key id mismatch",
                    )
                    .with_context("expected_key_id", &encryption.key_id)
                    .with_context("actual_key_id", key.key_id()));
                }
                xor_with_key(&record.bytes, key)
            }
            None => record.bytes.clone(),
        };
        let actual_plaintext_checksum = digest_bytes(&plaintext);
        if actual_plaintext_checksum != manifest.plaintext_checksum {
            return Err(
                EvaError::conflict("backup archive plaintext checksum mismatch")
                    .with_context("artifact_key", &record.key)
                    .with_context("expected_digest", &manifest.plaintext_checksum)
                    .with_context("actual_digest", actual_plaintext_checksum),
            );
        }
        Ok(plaintext)
    }
}

impl BackupArchiveVerifier {
    /// 验证格式、签名密钥标识、算法和规范化签名载荷。
    ///
    /// 任一不匹配均失败关闭；成功报告只证明清单与提供密钥一致，不替代存储字节和
    /// 明文摘要验证。
    pub fn verify_signature(
        manifest: &BackupArchiveManifest,
        signing_key: &BackupSigningKey,
    ) -> Result<BackupSignatureVerification, EvaError> {
        if manifest.format != ARCHIVE_FORMAT {
            return Err(EvaError::unsupported("unsupported backup archive format")
                .with_context("format", &manifest.format));
        }
        if manifest.signature.key_id != signing_key.key_id {
            return Err(
                EvaError::permission_denied("backup archive signing key id mismatch")
                    .with_context("expected_key_id", &manifest.signature.key_id)
                    .with_context("actual_key_id", signing_key.key_id()),
            );
        }
        if manifest.signature.algorithm != SIGNATURE_ALGORITHM {
            return Err(
                EvaError::unsupported("unsupported backup archive signature algorithm")
                    .with_context("algorithm", &manifest.signature.algorithm),
            );
        }
        let expected_signature = sign_archive(
            &manifest.artifact_key,
            &manifest.checksum,
            &manifest.plaintext_checksum,
            manifest.encryption.as_ref(),
            manifest.remote_target.as_ref(),
            signing_key,
        )
        .value;
        if expected_signature != manifest.signature.value {
            return Err(EvaError::conflict("backup archive signature mismatch")
                .with_context("artifact_key", &manifest.artifact_key)
                .with_context("expected_signature", expected_signature)
                .with_context("actual_signature", &manifest.signature.value));
        }
        Ok(BackupSignatureVerification {
            key_id: manifest.signature.key_id.clone(),
            algorithm: manifest.signature.algorithm.clone(),
            expected_signature,
            actual_signature: manifest.signature.value.clone(),
            verified: true,
        })
    }
}

/// 同时验证 ArtifactRecord 自身摘要和调用方期望摘要。
///
/// 第一层从实际字节重算摘要以发现记录内部损坏；第二层把记录绑定到外部清单，
/// 避免一份内部自洽但并非预期归档的记录被接受。
pub fn verify_record_checksum(
    record: &ArtifactRecord,
    expected_digest: &str,
) -> Result<(), EvaError> {
    let actual_from_bytes = digest_bytes(&record.bytes);
    if actual_from_bytes != record.digest {
        return Err(EvaError::conflict("artifact bytes digest mismatch")
            .with_context("artifact_key", &record.key)
            .with_context("expected_digest", &record.digest)
            .with_context("actual_digest", actual_from_bytes));
    }
    if record.digest != expected_digest {
        return Err(EvaError::conflict("artifact digest mismatch")
            .with_context("artifact_key", &record.key)
            .with_context("expected_digest", expected_digest)
            .with_context("actual_digest", &record.digest));
    }
    Ok(())
}

/// 使用 ArtifactRecord 的统一 SHA-256 格式计算任意字节摘要。
pub fn digest_bytes(bytes: &[u8]) -> String {
    ArtifactRecord::new("backup/archive/checksum", bytes.to_vec()).digest
}

/// 规范化并摘要所有需要防篡改的归档元数据。
fn sign_archive(
    artifact_key: &str,
    checksum: &str,
    plaintext_checksum: &str,
    encryption: Option<&BackupEncryptionManifest>,
    remote_target: Option<&RemoteBackupTarget>,
    signing_key: &BackupSigningKey,
) -> BackupSignatureManifest {
    let mut payload = format!(
        "eva-backup-signature:v1\nartifact_key={artifact_key}\nchecksum={checksum}\nplaintext_checksum={plaintext_checksum}\n"
    );
    match encryption {
        Some(encryption) => {
            payload.push_str(&format!(
                "encryption_algorithm={}\nencryption_key_id={}\n",
                encryption.algorithm, encryption.key_id
            ));
        }
        None => payload.push_str("encryption_algorithm=none\nencryption_key_id=none\n"),
    }
    match remote_target {
        Some(target) => {
            payload.push_str(&format!(
                "remote_kind={}\nremote_endpoint={}\nremote_prefix={}\nremote_required={}\n",
                target.kind.as_str(),
                target.endpoint,
                target.prefix,
                target.required
            ));
        }
        None => payload.push_str(
            "remote_kind=none\nremote_endpoint=none\nremote_prefix=none\nremote_required=false\n",
        ),
    }
    payload.push_str(&format!(
        "key_id={}\nsecret={}\n",
        signing_key.key_id, signing_key.secret
    ));
    BackupSignatureManifest {
        key_id: signing_key.key_id.clone(),
        algorithm: SIGNATURE_ALGORITHM.to_owned(),
        value: digest_bytes(payload.as_bytes()),
    }
}

/// 使用派生摘要流对归档字节执行对称异或变换。
///
/// 同一函数用于密封和打开；该项目内算法不是标准认证加密方案，完整性依赖独立
/// 签名与摘要校验。
fn xor_with_key(bytes: &[u8], key: &BackupEncryptionKey) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ stream_byte(key, index))
        .collect()
}

/// 从密钥材料、块编号和字节位置派生一个流字节。
fn stream_byte(key: &BackupEncryptionKey, index: usize) -> u8 {
    let seed = format!(
        "eva-backup-encryption-stream:v1\nkey_id={}\nsecret={}\nblock={}\n",
        key.key_id,
        key.secret,
        index / 71
    );
    let digest = digest_bytes(seed.as_bytes());
    digest.as_bytes()[index % digest.len()]
}

/// 校验密钥标识不会包含路径分隔符或遍历片段。
fn validate_key_id(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument("backup key id must be non-empty and trimmed")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("backup key id must be a stable slug")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}
