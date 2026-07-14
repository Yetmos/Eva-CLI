//! 发布工件证据与来源证明验证契约。
//! Release artifact evidence and provenance verification contracts.

use eva_core::EvaError;
use eva_storage::ArtifactRecord;
use std::collections::BTreeMap;

/// 本模块的架构职责：把发布工件元数据、构建来源和签名证据绑定为稳定契约。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "signed release artifact and provenance evidence contract";

/// 当前支持的发布工件证据清单格式。
pub const ARTIFACT_EVIDENCE_FORMAT: &str = "eva.release.artifact_evidence.v1";
/// 当前证据签名载荷算法标识。
pub const RELEASE_SIGNATURE_ALGORITHM: &str = "sha256-keyed-v1";

/// 发布工件证据的签名密钥。
///
/// 自定义 `Debug` 会遮蔽 secret；当前 keyed SHA-256 是项目内证据协议，不应冒充标准
/// 数字签名或替代生产密钥管理和制品仓库签名验证。
#[derive(Clone, PartialEq, Eq)]
pub struct ReleaseArtifactSigningKey {
    /// 写入证据并用于选择验证密钥的稳定标识。
    key_id: String,
    /// 参与证据摘要计算的敏感密钥材料。
    secret: String,
}

/// 发布工件证据清单中的签名字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactSignature {
    /// 验证时应使用的密钥标识。
    pub key_id: String,
    /// 签名载荷算法标识。
    pub algorithm: String,
    /// 规范化证据载荷的 keyed 摘要。
    pub value: String,
}

/// 单个平台发布包的工件主题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactSubject {
    /// 发布包文件名。
    pub name: String,
    /// Rust target triple 或等价平台目标。
    pub target: String,
    /// tar.gz、zip 等包装格式。
    pub format: String,
    /// 包中主可执行文件名。
    pub binary: String,
    /// 发布包实际字节的 SHA-256 摘要声明。
    pub digest: String,
    /// 发布包实际字节数声明。
    pub size_bytes: u64,
    /// 工件是否声明已由发布流程签名。
    pub signed: bool,
}

/// 描述工件构建来源、命令、SBOM 和扫描状态的证明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseProvenanceEvidence {
    /// 构建系统或工作流标识。
    pub builder: String,
    /// 构建输入的完整提交哈希。
    pub source_commit: String,
    /// 生成发布工件的命令。
    pub build_command: String,
    /// 构建配置或 profile。
    pub build_profile: String,
    /// SBOM 工件位置或引用。
    pub sbom: String,
    /// 构建后安全扫描状态。
    pub scan_status: String,
}

/// 将版本、来源、工件、构建证明和签名聚合的证据清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactEvidence {
    /// 证据清单格式版本。
    pub format: String,
    /// 发布版本。
    pub version: String,
    /// 发布来源标签。
    pub source_tag: String,
    /// 发布来源的完整提交哈希。
    pub source_commit: String,
    /// 被发布的单个平台工件描述。
    pub artifact: ReleaseArtifactSubject,
    /// 工件的构建来源证明。
    pub provenance: ReleaseProvenanceEvidence,
    /// 绑定上述全部字段的签名证据。
    pub signature: ReleaseArtifactSignature,
}

/// 发布工件签名和来源证明的门禁验证结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactVerificationReport {
    /// `verified` 或 `blocked` 状态。
    pub status: String,
    /// 被验证发布版本。
    pub version: String,
    /// 被验证来源标签。
    pub source_tag: String,
    /// 被验证来源提交。
    pub source_commit: String,
    /// 被验证工件文件名。
    pub artifact_name: String,
    /// 证据声明的工件摘要。
    pub artifact_digest: String,
    /// 工件目标平台。
    pub target: String,
    /// signed 标志、密钥标识、算法和值是否全部匹配。
    pub signature_verified: bool,
    /// 构建提交、扫描状态和 SBOM 是否满足要求。
    pub provenance_verified: bool,
    /// 任一验证门禁失败时的具体原因。
    pub risks: Vec<String>,
    /// 工件、摘要、签名和构建来源审计记录。
    pub audit: Vec<String>,
}

impl ReleaseArtifactSigningKey {
    /// 创建具有稳定标识和非空 secret 的证据签名密钥。
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, EvaError> {
        let key_id = validate_token("release signing key id", key_id.into())?;
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "release signing key secret cannot be empty",
            ));
        }
        Ok(Self { key_id, secret })
    }

    /// 返回仅用于本地开发和测试的固定签名密钥。
    pub fn local_development() -> Self {
        Self {
            key_id: "eva-local-release-signing-key".to_owned(),
            secret: "eva-local-release-signing-secret".to_owned(),
        }
    }

    /// 返回非敏感密钥标识。
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl std::fmt::Debug for ReleaseArtifactSigningKey {
    /// 输出密钥标识但始终遮蔽敏感 secret。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReleaseArtifactSigningKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl ReleaseArtifactSignature {
    /// 校验三个签名字段均为稳定标记后创建签名描述。
    pub fn new(
        key_id: impl Into<String>,
        algorithm: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let key_id = validate_token("release artifact signature key id", key_id.into())?;
        let algorithm = validate_token("release artifact signature algorithm", algorithm.into())?;
        let value = validate_token("release artifact signature value", value.into())?;
        Ok(Self {
            key_id,
            algorithm,
            value,
        })
    }
}

impl ReleaseArtifactSubject {
    /// 校验文件名、目标、摘要和非零大小后创建工件主题。
    pub fn new(
        name: impl Into<String>,
        target: impl Into<String>,
        format: impl Into<String>,
        binary: impl Into<String>,
        digest: impl Into<String>,
        size_bytes: u64,
        signed: bool,
    ) -> Result<Self, EvaError> {
        let name = validate_artifact_name(name.into())?;
        let target = validate_token("release artifact target", target.into())?;
        let format = validate_token("release artifact format", format.into())?;
        let binary = validate_artifact_name(binary.into())?;
        let digest = validate_digest(digest.into())?;
        if size_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "release artifact size must be greater than zero",
            ));
        }
        Ok(Self {
            name,
            target,
            format,
            binary,
            digest,
            size_bytes,
            signed,
        })
    }
}

impl ReleaseProvenanceEvidence {
    /// 创建绑定完整来源提交、构建过程、SBOM 和扫描状态的来源证明。
    pub fn new(
        builder: impl Into<String>,
        source_commit: impl Into<String>,
        build_command: impl Into<String>,
        build_profile: impl Into<String>,
        sbom: impl Into<String>,
        scan_status: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let builder = validate_non_empty("release provenance builder", builder.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        let build_command = validate_non_empty("release build command", build_command.into())?;
        let build_profile = validate_token("release build profile", build_profile.into())?;
        let sbom = validate_non_empty("release SBOM evidence", sbom.into())?;
        let scan_status = validate_token("release scan status", scan_status.into())?;
        Ok(Self {
            builder,
            source_commit,
            build_command,
            build_profile,
            sbom,
            scan_status,
        })
    }
}

impl ReleaseArtifactEvidence {
    /// 创建格式固定的发布工件证据；签名可随后通过 `sign` 计算。
    pub fn new(
        version: impl Into<String>,
        source_tag: impl Into<String>,
        source_commit: impl Into<String>,
        artifact: ReleaseArtifactSubject,
        provenance: ReleaseProvenanceEvidence,
        signature: ReleaseArtifactSignature,
    ) -> Result<Self, EvaError> {
        let version = validate_version(version.into())?;
        let source_tag = validate_token("release source tag", source_tag.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        Ok(Self {
            format: ARTIFACT_EVIDENCE_FORMAT.to_owned(),
            version,
            source_tag,
            source_commit,
            artifact,
            provenance,
            signature,
        })
    }

    /// 从严格键值清单解析并重新校验全部嵌套证据。
    ///
    /// 重复或缺失字段、非法格式、无效摘要/提交都会失败关闭。解析只验证结构，不
    /// 验证签名，也不会读取工件字节；调用方仍需执行 `verify` 和制品仓库摘要校验。
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        let artifact = ReleaseArtifactSubject::new(
            required(&fields, "artifact.name")?,
            required(&fields, "artifact.target")?,
            required(&fields, "artifact.format")?,
            required(&fields, "artifact.binary")?,
            required(&fields, "artifact.digest")?,
            parse_size(required(&fields, "artifact.size_bytes")?)?,
            parse_bool(required(&fields, "artifact.signed")?, "artifact.signed")?,
        )?;
        let provenance = ReleaseProvenanceEvidence::new(
            required(&fields, "provenance.builder")?,
            required(&fields, "provenance.source_commit")?,
            required(&fields, "provenance.build_command")?,
            required(&fields, "provenance.build_profile")?,
            required(&fields, "provenance.sbom")?,
            required(&fields, "provenance.scan_status")?,
        )?;
        let signature = ReleaseArtifactSignature::new(
            required(&fields, "signature.key_id")?,
            required(&fields, "signature.algorithm")?,
            required(&fields, "signature.value")?,
        )?;
        let evidence = Self::new(
            required(&fields, "version")?,
            required(&fields, "source_tag")?,
            required(&fields, "source_commit")?,
            artifact,
            provenance,
            signature,
        )?;
        if required(&fields, "format")? != ARTIFACT_EVIDENCE_FORMAT {
            return Err(
                EvaError::invalid_argument("unsupported release artifact evidence format")
                    .with_context("format", required(&fields, "format")?),
            );
        }
        Ok(evidence)
    }

    /// 使用规范化证据载荷和提供的密钥计算签名字段。
    ///
    /// 载荷覆盖版本、标签、提交、工件元数据和全部来源证明，不包含已有签名字段，
    /// 因而重新签名结果可复现。
    pub fn sign(&self, signing_key: &ReleaseArtifactSigningKey) -> ReleaseArtifactSignature {
        ReleaseArtifactSignature {
            key_id: signing_key.key_id.clone(),
            algorithm: RELEASE_SIGNATURE_ALGORITHM.to_owned(),
            value: keyed_digest(&self.signature_payload(), signing_key),
        }
    }

    /// 验证证据签名及构建来源门禁并生成风险报告。
    ///
    /// 签名要求工件显式标记 signed 且 key id、算法和值均匹配；来源证明要求内部
    /// 提交与顶层提交一致、扫描状态为 passed 且 SBOM 非空。该方法只验证证据字段，
    /// 不获取或哈希实际发布包，外部发布流程必须另行把工件字节绑定到 digest/size。
    pub fn verify(
        &self,
        signing_key: &ReleaseArtifactSigningKey,
    ) -> ReleaseArtifactVerificationReport {
        let expected_signature = self.sign(signing_key);
        let signature_verified = self.artifact.signed
            && self.signature.key_id == expected_signature.key_id
            && self.signature.algorithm == expected_signature.algorithm
            && self.signature.value == expected_signature.value;
        let provenance_verified = self.provenance.source_commit == self.source_commit
            && self.provenance.scan_status == "passed"
            && !self.provenance.sbom.trim().is_empty();

        let mut risks = Vec::new();
        if !self.artifact.signed {
            risks.push("release artifact is marked unsigned".to_owned());
        }
        if self.signature.key_id != expected_signature.key_id {
            risks.push("release artifact signature key id mismatch".to_owned());
        }
        if self.signature.algorithm != expected_signature.algorithm {
            risks.push("release artifact signature algorithm mismatch".to_owned());
        }
        if self.signature.value != expected_signature.value {
            risks.push("release artifact signature value mismatch".to_owned());
        }
        if self.provenance.source_commit != self.source_commit {
            risks.push(
                "release provenance source commit does not match artifact evidence".to_owned(),
            );
        }
        if self.provenance.scan_status != "passed" {
            risks.push(format!(
                "release scan status is {}",
                self.provenance.scan_status
            ));
        }

        let status = if signature_verified && provenance_verified {
            "verified"
        } else {
            "blocked"
        }
        .to_owned();

        ReleaseArtifactVerificationReport {
            status,
            version: self.version.clone(),
            source_tag: self.source_tag.clone(),
            source_commit: self.source_commit.clone(),
            artifact_name: self.artifact.name.clone(),
            artifact_digest: self.artifact.digest.clone(),
            target: self.artifact.target.clone(),
            signature_verified,
            provenance_verified,
            risks,
            audit: vec![
                "release.artifact:manifest_parsed".to_owned(),
                format!("release.artifact:{}", self.artifact.name),
                format!("release.artifact.digest:{}", self.artifact.digest),
                format!("release.artifact.signature:{}", self.signature.key_id),
                format!("release.provenance.builder:{}", self.provenance.builder),
                format!("release.provenance.source_commit:{}", self.source_commit),
            ],
        }
    }

    /// 以稳定字段顺序序列化完整证据和签名。
    pub fn to_manifest(&self) -> String {
        format!(
            "format={}\nversion={}\nsource_tag={}\nsource_commit={}\nartifact.name={}\nartifact.target={}\nartifact.format={}\nartifact.binary={}\nartifact.digest={}\nartifact.size_bytes={}\nartifact.signed={}\nprovenance.builder={}\nprovenance.source_commit={}\nprovenance.build_command={}\nprovenance.build_profile={}\nprovenance.sbom={}\nprovenance.scan_status={}\nsignature.key_id={}\nsignature.algorithm={}\nsignature.value={}\n",
            self.format,
            self.version,
            self.source_tag,
            self.source_commit,
            self.artifact.name,
            self.artifact.target,
            self.artifact.format,
            self.artifact.binary,
            self.artifact.digest,
            self.artifact.size_bytes,
            self.artifact.signed,
            self.provenance.builder,
            self.provenance.source_commit,
            self.provenance.build_command,
            self.provenance.build_profile,
            self.provenance.sbom,
            self.provenance.scan_status,
            self.signature.key_id,
            self.signature.algorithm,
            self.signature.value,
        )
    }

    /// 以固定顺序编码所有需要签名保护的非签名字段。
    fn signature_payload(&self) -> String {
        format!(
            "format={}\nversion={}\nsource_tag={}\nsource_commit={}\nartifact.name={}\nartifact.target={}\nartifact.format={}\nartifact.binary={}\nartifact.digest={}\nartifact.size_bytes={}\nartifact.signed={}\nprovenance.builder={}\nprovenance.source_commit={}\nprovenance.build_command={}\nprovenance.build_profile={}\nprovenance.sbom={}\nprovenance.scan_status={}\n",
            self.format,
            self.version,
            self.source_tag,
            self.source_commit,
            self.artifact.name,
            self.artifact.target,
            self.artifact.format,
            self.artifact.binary,
            self.artifact.digest,
            self.artifact.size_bytes,
            self.artifact.signed,
            self.provenance.builder,
            self.provenance.source_commit,
            self.provenance.build_command,
            self.provenance.build_profile,
            self.provenance.sbom,
            self.provenance.scan_status,
        )
    }
}

/// 解析允许 BOM、空行和注释的严格键值清单，并拒绝重复键。
fn parse_key_value_manifest(data: &str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "release artifact evidence line must use key=value format",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(EvaError::invalid_argument(
                "release artifact evidence key cannot be empty",
            ));
        }
        if fields
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(EvaError::invalid_argument(
                "release artifact evidence field is duplicated",
            )
            .with_context("field", key));
        }
    }
    Ok(fields)
}

/// 读取必填证据字段。
fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release artifact evidence is missing required field")
            .with_context("required_field", key)
    })
}

/// 严格解析 `true` 或 `false` 布尔字段。
fn parse_bool(value: String, field: &str) -> Result<bool, EvaError> {
    match value.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(EvaError::invalid_argument(
            "release artifact boolean field must be true or false",
        )
        .with_context("field", field)
        .with_context("value", value)),
    }
}

/// 解析工件字节数，零值由工件构造器进一步拒绝。
fn parse_size(value: String) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|error| {
        EvaError::invalid_argument("release artifact size must be an integer")
            .with_context("field", "artifact.size_bytes")
            .with_context("value", value)
            .with_context("parse_error", error.to_string())
    })
}

/// 校验发布版本为非空且不含空白的单个标记。
fn validate_version(value: String) -> Result<String, EvaError> {
    let value = validate_non_empty("release version", value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument("release version cannot contain whitespace")
                .with_context("version", value),
        );
    }
    Ok(value)
}

/// 校验不允许包含空白的稳定证据标记。
fn validate_token(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_non_empty(field, value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument(format!("{field} cannot contain whitespace"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验文本非空且首尾无空白。
fn validate_non_empty(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验工件名是不能含路径分隔符或遍历片段的文件名。
fn validate_artifact_name(value: String) -> Result<String, EvaError> {
    let value = validate_token("release artifact name", value)?;
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("release artifact name must be a stable file name")
                .with_context("artifact", value),
        );
    }
    Ok(value)
}

/// 校验工件摘要是带前缀的 64 位十六进制 SHA-256 值。
fn validate_digest(value: String) -> Result<String, EvaError> {
    let value = validate_token("release artifact digest", value)?;
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(
            EvaError::invalid_argument("release artifact digest must use sha256 prefix")
                .with_context("digest", value),
        );
    };
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(EvaError::invalid_argument(
            "release artifact digest must be a sha256 hex digest",
        )
        .with_context("digest", value));
    }
    Ok(value)
}

/// 校验来源提交为完整 40 字符十六进制哈希。
fn validate_commit(value: String) -> Result<String, EvaError> {
    let value = validate_token("release source commit", value)?;
    if value.len() != 40 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(EvaError::invalid_argument(
            "release source commit must be a full 40-character hex sha",
        )
        .with_context("source_commit", value));
    }
    Ok(value)
}

/// 将密钥标识、secret 和规范化载荷计算为项目内 keyed 摘要。
fn keyed_digest(payload: &str, signing_key: &ReleaseArtifactSigningKey) -> String {
    let signed_payload = format!(
        "eva-release-signature:v1\nkey_id={}\nsecret={}\n{}",
        signing_key.key_id, signing_key.secret, payload
    );
    ArtifactRecord::new("release/artifact/signature", signed_payload.into_bytes()).digest
}

#[cfg(test)]
/// 工件证据清单往返、签名和来源门禁测试。
mod tests {
    use super::*;

    /// 测试证据使用的完整来源提交。
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    /// 测试工件使用的合法 SHA-256 摘要。
    const DIGEST: &str = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";

    /// 构造签名和来源证明均有效的固定证据。
    fn signed_evidence() -> ReleaseArtifactEvidence {
        let key = ReleaseArtifactSigningKey::local_development();
        let artifact = ReleaseArtifactSubject::new(
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "eva",
            DIGEST,
            1024,
            true,
        )
        .unwrap();
        let provenance = ReleaseProvenanceEvidence::new(
            "github-actions",
            COMMIT,
            "cargo-build-release-locked-bin-eva",
            "release",
            "spdx:release-evidence/eva.spdx.json",
            "passed",
        )
        .unwrap();
        let signature =
            ReleaseArtifactSignature::new(key.key_id(), RELEASE_SIGNATURE_ALGORITHM, "pending")
                .unwrap();
        let mut evidence = ReleaseArtifactEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            COMMIT,
            artifact,
            provenance,
            signature,
        )
        .unwrap();
        evidence.signature = evidence.sign(&key);
        evidence
    }

    #[test]
    /// 验证有效证据可往返清单且两个门禁均通过。
    fn signed_artifact_evidence_round_trips_and_verifies() {
        let key = ReleaseArtifactSigningKey::local_development();
        let evidence =
            ReleaseArtifactEvidence::parse_manifest(&signed_evidence().to_manifest()).unwrap();

        let report = evidence.verify(&key);

        assert_eq!(report.status, "verified");
        assert!(report.signature_verified);
        assert!(report.provenance_verified);
        assert!(report.risks.is_empty());
    }

    #[test]
    /// 验证即使签名值匹配，工件声明未签名仍会阻塞。
    fn unsigned_artifact_blocks_verification() {
        let key = ReleaseArtifactSigningKey::local_development();
        let mut evidence = signed_evidence();
        evidence.artifact.signed = false;
        evidence.signature = evidence.sign(&key);

        let report = evidence.verify(&key);

        assert_eq!(report.status, "blocked");
        assert!(!report.signature_verified);
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "release artifact is marked unsigned"));
    }

    #[test]
    /// 验证签名值被篡改时证据失败关闭。
    fn signature_mismatch_blocks_verification() {
        let key = ReleaseArtifactSigningKey::local_development();
        let mut evidence = signed_evidence();
        evidence.signature.value = "sha256:bad".to_owned();

        let report = evidence.verify(&key);

        assert_eq!(report.status, "blocked");
        assert!(!report.signature_verified);
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "release artifact signature value mismatch"));
    }

    #[test]
    /// 验证来源证明提交与顶层提交不一致会阻塞发布。
    fn provenance_commit_mismatch_blocks_verification() {
        let key = ReleaseArtifactSigningKey::local_development();
        let mut evidence = signed_evidence();
        evidence.provenance.source_commit = "abcdef0123456789abcdef0123456789abcdef01".to_owned();
        evidence.signature = evidence.sign(&key);

        let report = evidence.verify(&key);

        assert_eq!(report.status, "blocked");
        assert!(report.signature_verified);
        assert!(!report.provenance_verified);
    }
}
