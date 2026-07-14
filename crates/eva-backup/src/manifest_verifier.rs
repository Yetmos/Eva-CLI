//! 工件与清单的完整性验证。
//! Artifact and manifest integrity verification.

use crate::archive::{verify_record_checksum, BackupArchiveVerifier, BackupSigningKey};
use crate::backup_service::BackupManifest;
use eva_core::EvaError;
use eva_storage::ArtifactRecord;

/// 本模块的架构职责：验证工件字节、外部摘要和备份签名的一致性。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "artifact and manifest integrity verification";

/// 工件通过完整性验证后的可审计报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    /// 被验证工件的存储键。
    pub artifact_key: String,
    /// 外部清单声明的预期摘要。
    pub expected_digest: String,
    /// ArtifactRecord 保存并经字节重算确认的摘要。
    pub actual_digest: String,
    /// 所有验证是否通过；成功返回时为 `true`。
    pub verified: bool,
    /// 已完成验证步骤的审计记录。
    pub audit: Vec<String>,
}

/// 对工件和备份清单执行失败关闭校验的无状态服务。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ManifestVerifier;

impl ManifestVerifier {
    /// 验证实际字节、记录摘要和外部预期摘要三者一致。
    pub fn verify_artifact(
        record: &ArtifactRecord,
        expected_digest: &str,
    ) -> Result<VerificationReport, EvaError> {
        if expected_digest.trim().is_empty() {
            return Err(
                EvaError::invalid_argument("expected digest cannot be empty")
                    .with_context("artifact_key", &record.key),
            );
        }
        verify_record_checksum(record, expected_digest)?;
        Ok(VerificationReport {
            artifact_key: record.key.clone(),
            expected_digest: expected_digest.to_owned(),
            actual_digest: record.digest.clone(),
            verified: true,
            audit: vec![
                "manifest:verified".to_owned(),
                format!("artifact:{}", record.key),
            ],
        })
    }

    /// 按键、清单摘要、存储字节和签名的顺序验证备份归档。
    ///
    /// 任一层不一致都会立即返回错误，不生成部分成功报告。密封标记和远端目标只会
    /// 写入审计；明文解密与摘要校验由归档编解码器负责。
    pub fn verify_backup_archive(
        record: &ArtifactRecord,
        manifest: &BackupManifest,
        signing_key: &BackupSigningKey,
    ) -> Result<VerificationReport, EvaError> {
        if manifest.archive.artifact_key != record.key {
            return Err(EvaError::conflict("backup archive artifact key mismatch")
                .with_context("expected_artifact_key", &manifest.archive.artifact_key)
                .with_context("actual_artifact_key", &record.key));
        }
        if manifest.archive.checksum != manifest.digest {
            return Err(
                EvaError::conflict("backup archive manifest checksum mismatch")
                    .with_context("artifact_key", &record.key)
                    .with_context("manifest_digest", &manifest.digest)
                    .with_context("archive_checksum", &manifest.archive.checksum),
            );
        }
        let mut report = Self::verify_artifact(record, &manifest.digest)?;
        let signature = BackupArchiveVerifier::verify_signature(&manifest.archive, signing_key)?;
        report.audit.push("archive:verified".to_owned());
        report.audit.push(format!("signature:{}", signature.key_id));
        if manifest.archive.encrypted {
            report.audit.push("archive:encrypted".to_owned());
        }
        if manifest.archive.remote_target.is_some() {
            report.audit.push("remote_target:declared".to_owned());
        }
        Ok(report)
    }
}

#[cfg(test)]
/// 摘要不匹配与记录内部损坏的失败关闭测试。
mod tests {
    use super::*;

    #[test]
    /// 验证正确记录不能通过错误外部摘要。
    fn verifier_rejects_digest_mismatch() {
        let record = ArtifactRecord::new("backup/test", b"ok".as_slice());

        let error = ManifestVerifier::verify_artifact(&record, "sha256:wrong").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证记录字节被篡改时即使保存的摘要未变也会被拒绝。
    fn verifier_rejects_corrupt_record_bytes() {
        let mut record = ArtifactRecord::new("backup/test", b"ok".as_slice());
        let expected = record.digest.clone();
        record.bytes = b"tampered".to_vec();

        let error = ManifestVerifier::verify_artifact(&record, &expected).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(error.message().contains("bytes digest"));
    }
}
