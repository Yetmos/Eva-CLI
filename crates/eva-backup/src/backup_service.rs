//! 备份服务的归档与持久化编排。
//! Backup service orchestration.

use crate::archive::{
    BackupArchiveCodec, BackupArchiveManifest, BackupEncryptionKey, BackupSigningKey,
    RemoteBackupTarget,
};
use crate::manifest_verifier::{ManifestVerifier, VerificationReport};
use eva_core::{EvaError, GenerationId, RequestId};
use eva_storage::{ArtifactRecord, ArtifactStore};
use std::fmt::Write as _;

/// 本模块的架构职责：编排备份归档生成、存储和写后校验。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "backup service orchestration";

/// 备份范围内的单个相对路径及其内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// 归档内使用的稳定相对路径。
    pub path: String,
    /// 需要写入归档的原始字节。
    pub bytes: Vec<u8>,
    /// 清单是否应将该条目标记为敏感或已脱敏。
    pub redacted: bool,
}

/// 一次备份覆盖的项目和条目集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupScope {
    /// 备份所属项目的稳定标识。
    pub project_id: String,
    /// 非空的备份条目列表。
    pub entries: Vec<BackupEntry>,
}

/// 创建备份归档所需的完整输入和安全选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPlan {
    /// 在备份命名空间内使用的稳定归档标识。
    pub artifact_id: String,
    /// 触发备份的请求标识。
    pub request_id: RequestId,
    /// 备份内容对应的运行时代际。
    pub runtime_generation: GenerationId,
    /// 发起备份的主体。
    pub created_by: String,
    /// 创建备份的业务原因。
    pub reason: String,
    /// 被归档的项目与条目。
    pub scope: BackupScope,
    /// 是否将本次计划标记为演练。
    pub dry_run: bool,
    /// 传递给调用方的恢复风险说明。
    pub risks: Vec<String>,
    /// 用于绑定归档内容和元数据的签名密钥。
    pub signing_key: BackupSigningKey,
    /// 可选的归档密封密钥。
    pub encryption_key: Option<BackupEncryptionKey>,
    /// 可选远端复制目标声明。
    pub remote_target: Option<RemoteBackupTarget>,
}

/// 备份清单中不包含内容本身的条目元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupManifestEntry {
    /// 条目在归档中的相对路径。
    pub path: String,
    /// 条目原始内容的字节数。
    pub size_bytes: usize,
    /// 条目是否标记为敏感或已脱敏。
    pub redacted: bool,
}

/// 将存储归档与请求、代际和条目元数据绑定的备份清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupManifest {
    /// 用户可见的备份标识。
    pub artifact_id: String,
    /// 工件类别，当前固定为 `backup`。
    pub artifact_type: String,
    /// 创建该备份的请求标识。
    pub request_id: RequestId,
    /// 内容对应的运行时代际。
    pub runtime_generation: GenerationId,
    /// 内容所属项目标识。
    pub project_id: String,
    /// 不暴露内容的条目清单。
    pub entries: Vec<BackupManifestEntry>,
    /// ArtifactStore 实际存储字节的摘要。
    pub digest: String,
    /// 签名、密封与远端目标元数据。
    pub archive: BackupArchiveManifest,
    /// 创建和验证过程的审计记录。
    pub audit: Vec<String>,
}

/// 备份创建、持久化及写后校验的聚合结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupResult {
    /// 实际执行的原始计划。
    pub plan: BackupPlan,
    /// 与存储工件绑定的清单。
    pub manifest: BackupManifest,
    /// ArtifactStore 返回的持久化记录。
    pub artifact: ArtifactRecord,
    /// 写入后立即执行的完整性与签名验证证据。
    pub verification: VerificationReport,
}

/// 创建并验证备份归档的无状态服务。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupService;

impl BackupEntry {
    /// 校验相对路径安全性后创建备份条目。
    ///
    /// 禁止遍历片段、反斜杠和控制字符，确保归档路径不被恢复方解释为越界目标。
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Result<Self, EvaError> {
        let path = path.into();
        if path.trim().is_empty()
            || path.contains("..")
            || path.contains('\\')
            || path.chars().any(char::is_control)
        {
            return Err(
                EvaError::invalid_argument("backup path must be a stable relative path")
                    .with_context("path", path),
            );
        }
        Ok(Self {
            path,
            bytes: bytes.into(),
            redacted: false,
        })
    }

    /// 将条目标记为敏感或已脱敏，供清单和审计展示。
    pub fn redacted(mut self) -> Self {
        self.redacted = true;
        self
    }
}

impl BackupScope {
    /// 创建项目的非空备份范围。
    pub fn new(project_id: impl Into<String>, entries: Vec<BackupEntry>) -> Result<Self, EvaError> {
        let project_id = project_id.into();
        if project_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup project id cannot be empty",
            ));
        }
        if entries.is_empty() {
            return Err(EvaError::invalid_argument(
                "backup scope must contain entries",
            ));
        }
        Ok(Self {
            project_id,
            entries,
        })
    }
}

impl BackupPlan {
    /// 校验工件标识、操作者和原因后创建默认签名的备份计划。
    ///
    /// 默认密钥仅用于本地开发，默认不密封、不声明远端复制；恢复始终保持 plan-first，
    /// 因此创建备份不会触发任何恢复或运行时变更。
    pub fn new(
        artifact_id: impl Into<String>,
        request_id: RequestId,
        runtime_generation: GenerationId,
        created_by: impl Into<String>,
        reason: impl Into<String>,
        scope: BackupScope,
    ) -> Result<Self, EvaError> {
        let artifact_id = artifact_id.into();
        let created_by = created_by.into();
        let reason = reason.into();
        if artifact_id.trim().is_empty() || artifact_id.contains('/') || artifact_id.contains('\\')
        {
            return Err(
                EvaError::invalid_argument("backup artifact id must be a stable slug")
                    .with_context("artifact_id", artifact_id),
            );
        }
        if created_by.trim().is_empty() || reason.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup actor and reason are required",
            ));
        }
        Ok(Self {
            artifact_id,
            request_id,
            runtime_generation,
            created_by,
            reason,
            scope,
            dry_run: false,
            risks: vec![
                "restore remains plan-first; no destructive mutation is performed".to_owned(),
            ],
            signing_key: BackupSigningKey::local_development(),
            encryption_key: None,
            remote_target: None,
        })
    }

    /// 将计划标记为演练；当前仍会生成并保存带该标记的归档。
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    /// 使用显式签名密钥替换本地开发默认值。
    pub fn signed_with(mut self, signing_key: BackupSigningKey) -> Self {
        self.signing_key = signing_key;
        self
    }

    /// 启用归档密封并记录解密所需的密钥标识。
    pub fn encrypted_with(mut self, encryption_key: BackupEncryptionKey) -> Self {
        self.encryption_key = Some(encryption_key);
        self
    }

    /// 在签名清单中绑定远端复制目标声明。
    pub fn with_remote_target(mut self, remote_target: RemoteBackupTarget) -> Self {
        self.remote_target = Some(remote_target);
        self
    }
}

impl BackupService {
    /// 生成规范化归档、写入 ArtifactStore，并在返回前重新验证摘要和签名。
    ///
    /// 顺序为：序列化条目、可选密封与签名、持久化、以存储返回摘要修正清单，最后
    /// 执行写后验证。存储或验证失败均不返回成功结果；接口不提供删除回滚，因此
    /// 验证失败时存储中可能留下不可接受工件，调用方不得仅凭存在性使用它。
    pub fn create(
        &self,
        plan: BackupPlan,
        store: &mut impl ArtifactStore,
    ) -> Result<BackupResult, EvaError> {
        let artifact_key = format!("backup/{}", plan.artifact_id);
        let payload = backup_archive_payload(&plan);
        let sealed = BackupArchiveCodec::seal(
            artifact_key.clone(),
            payload,
            &plan.signing_key,
            plan.encryption_key.as_ref(),
            plan.remote_target.clone(),
        );
        let artifact = store.put_bytes(artifact_key, sealed.bytes)?;
        let manifest = BackupManifest {
            artifact_id: plan.artifact_id.clone(),
            artifact_type: "backup".to_owned(),
            request_id: plan.request_id.clone(),
            runtime_generation: plan.runtime_generation.clone(),
            project_id: plan.scope.project_id.clone(),
            entries: plan
                .scope
                .entries
                .iter()
                .map(|entry| BackupManifestEntry {
                    path: entry.path.clone(),
                    size_bytes: entry.bytes.len(),
                    redacted: entry.redacted,
                })
                .collect(),
            digest: artifact.digest.clone(),
            archive: BackupArchiveManifest {
                checksum: artifact.digest.clone(),
                ..sealed.manifest
            },
            audit: vec![
                "backup:created".to_owned(),
                "backup:archive:sealed".to_owned(),
                "backup:signature:created".to_owned(),
                format!("artifact:{}", artifact.key),
                format!("dry_run:{}", plan.dry_run),
            ],
        };
        let verification =
            ManifestVerifier::verify_backup_archive(&artifact, &manifest, &plan.signing_key)?;
        Ok(BackupResult {
            plan,
            manifest,
            artifact,
            verification,
        })
    }
}

/// 按固定字段顺序编码计划和每个条目的完整内容。
///
/// 原始字节使用十六进制，避免换行或任意二进制破坏行式归档边界；条目顺序保持
/// 调用方输入顺序，因此相同计划可生成确定载荷。
fn backup_archive_payload(plan: &BackupPlan) -> Vec<u8> {
    let mut payload = format!(
        "eva-backup-archive:v1\nartifact={}\nrequest={}\ngeneration={}\nproject={}\ncreated_by={}\nreason={}\ndry_run={}\n",
        plan.artifact_id,
        plan.request_id.as_str(),
        plan.runtime_generation.as_str(),
        plan.scope.project_id,
        plan.created_by,
        plan.reason,
        plan.dry_run
    );
    for entry in &plan.scope.entries {
        let _ = writeln!(payload, "entry.path={}", entry.path);
        let _ = writeln!(payload, "entry.size={}", entry.bytes.len());
        let _ = writeln!(payload, "entry.redacted={}", entry.redacted);
        let _ = writeln!(payload, "entry.bytes.hex={}", hex_encode(&entry.bytes));
    }
    payload.into_bytes()
}

/// 将任意字节编码为小写十六进制文本。
fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
/// 备份创建、密封打开和签名失败关闭行为测试。
mod tests {
    use super::*;
    use crate::archive::{BackupArchiveCodec, BackupEncryptionKey, RemoteBackupTargetKind};
    use eva_storage::InMemoryArtifactStore;

    #[test]
    /// 验证服务会持久化归档并立即完成摘要和签名校验。
    fn backup_service_creates_and_verifies_artifact() {
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("config/eva.yaml", "runtime: basic").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-v14",
            RequestId::parse("req-backup-1").unwrap(),
            GenerationId::parse("gen-v14").unwrap(),
            "cli",
            "pre-upgrade safety checkpoint",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();

        let result = BackupService.create(plan, &mut store).unwrap();

        assert!(result.verification.verified);
        assert_eq!(result.manifest.archive.format, "eva.backup.archive.v1");
        assert_eq!(
            result.manifest.archive.signature.key_id,
            result.plan.signing_key.key_id()
        );
        assert_eq!(result.manifest.entries[0].path, "config/eva.yaml");
        assert_eq!(
            store.get_bytes(&result.artifact.key).unwrap().digest,
            result.manifest.digest
        );
        let archive =
            BackupArchiveCodec::open(&result.artifact, &result.manifest.archive, None).unwrap();
        let archive = String::from_utf8(archive).unwrap();
        assert!(archive.contains("entry.path=config/eva.yaml"));
        assert!(archive.contains("entry.bytes.hex=72756e74696d653a206261736963"));
    }

    #[test]
    /// 验证密封归档不泄露明文且保留远端目标声明。
    fn backup_service_can_encrypt_archive_and_record_remote_target() {
        let encryption_key = BackupEncryptionKey::new("archive-key", "secret").unwrap();
        let remote = RemoteBackupTarget::new(
            RemoteBackupTargetKind::ObjectStore,
            "s3://eva-backups",
            "daily/eva-cli",
        )
        .unwrap()
        .required();
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("state/release-pointer", "stable").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-v1103",
            RequestId::parse("req-backup-2").unwrap(),
            GenerationId::parse("gen-v1103").unwrap(),
            "cli",
            "pre-restore safety checkpoint",
            scope,
        )
        .unwrap()
        .encrypted_with(encryption_key.clone())
        .with_remote_target(remote);
        let mut store = InMemoryArtifactStore::new();

        let result = BackupService.create(plan, &mut store).unwrap();

        assert!(result.manifest.archive.encrypted);
        assert!(result.manifest.archive.remote_target.is_some());
        assert!(!String::from_utf8_lossy(&result.artifact.bytes).contains("stable"));
        let archive = BackupArchiveCodec::open(
            &result.artifact,
            &result.manifest.archive,
            Some(&encryption_key),
        )
        .unwrap();
        assert!(String::from_utf8(archive)
            .unwrap()
            .contains("entry.path=state/release-pointer"));
    }

    #[test]
    /// 验证相同 key id 下的错误 secret 仍会导致签名验证失败。
    fn backup_signature_mismatch_blocks_verification() {
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("config/eva.yaml", "runtime: basic").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-signature",
            RequestId::parse("req-backup-3").unwrap(),
            GenerationId::parse("gen-v1103").unwrap(),
            "cli",
            "signature verification",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();
        let result = BackupService.create(plan, &mut store).unwrap();
        let wrong_key =
            BackupSigningKey::new(result.plan.signing_key.key_id(), "wrong-secret").unwrap();

        let error =
            ManifestVerifier::verify_backup_archive(&result.artifact, &result.manifest, &wrong_key)
                .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(error.message().contains("signature mismatch"));
    }
}
