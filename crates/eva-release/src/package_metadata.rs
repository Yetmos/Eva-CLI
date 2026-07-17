//! Credential-free canonical package metadata.
use eva_core::EvaError;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageArtifactMetadata {
    pub target: String,
    pub artifact: String,
    pub format: String,
    pub download_url: String,
    pub sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPackageMetadata {
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub artifacts: Vec<PackageArtifactMetadata>,
}

impl CanonicalPackageMetadata {
    pub fn new(
        version: impl Into<String>,
        source_commit: impl Into<String>,
        artifacts: Vec<PackageArtifactMetadata>,
    ) -> Result<Self, EvaError> {
        let version = version.into();
        let source_commit = source_commit.into();
        if version.trim().is_empty() || version.contains(char::is_whitespace) {
            return Err(EvaError::invalid_argument(
                "package metadata version is invalid",
            ));
        }
        if source_commit.len() != 40 || !source_commit.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(EvaError::invalid_argument(
                "package metadata source commit is invalid",
            ));
        }
        let mut artifacts = artifacts;
        artifacts.sort();
        let mut targets = BTreeSet::new();
        for artifact in &artifacts {
            artifact.validate(&version)?;
            if !targets.insert(artifact.target.as_str()) {
                return Err(EvaError::conflict("package metadata target is duplicated"));
            }
        }
        let required = [
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
        ];
        if required.iter().any(|target| !targets.contains(target))
            || artifacts.len() != required.len()
        {
            return Err(EvaError::invalid_argument(
                "package metadata must cover the canonical three targets",
            ));
        }
        Ok(Self {
            source_tag: format!("v{version}"),
            version,
            source_commit: source_commit.to_ascii_lowercase(),
            artifacts,
        })
    }
    pub fn to_manifest(&self) -> String {
        let mut lines = vec![
            "format=eva.package-metadata.v1".to_owned(),
            format!("version={}", self.version),
            format!("source_tag={}", self.source_tag),
            format!("source_commit={}", self.source_commit),
        ];
        for (i, a) in self.artifacts.iter().enumerate() {
            lines.extend([
                format!("artifact.{i}.target={}", a.target),
                format!("artifact.{i}.name={}", a.artifact),
                format!("artifact.{i}.format={}", a.format),
                format!("artifact.{i}.url={}", a.download_url),
                format!("artifact.{i}.sha256={}", a.sha256),
            ]);
        }
        format!("{}\n", lines.join("\n"))
    }
}
impl PackageArtifactMetadata {
    pub fn new(
        target: impl Into<String>,
        artifact: impl Into<String>,
        format: impl Into<String>,
        download_url: impl Into<String>,
        sha256: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            artifact: artifact.into(),
            format: format.into(),
            download_url: download_url.into(),
            sha256: sha256.into(),
        }
    }
    fn validate(&self, version: &str) -> Result<(), EvaError> {
        let (suffix, format) = match self.target.as_str() {
            "x86_64-pc-windows-msvc" => ("zip", "zip"),
            "x86_64-unknown-linux-gnu" | "aarch64-apple-darwin" => ("tar.gz", "tar.gz"),
            _ => ("", ""),
        };
        if suffix.is_empty() {
            return Err(EvaError::invalid_argument(
                "package metadata target is unsupported",
            ));
        }
        let expected = format!("eva-cli-{version}-{}.{}", self.target, suffix);
        if self.artifact != expected || self.format != format {
            return Err(EvaError::invalid_argument(
                "package metadata artifact mapping is invalid",
            ));
        }
        if !self.download_url.starts_with("https://")
            || !self.download_url.ends_with(&self.artifact)
        {
            return Err(EvaError::invalid_argument(
                "package metadata download URL is invalid",
            ));
        }
        if self.sha256.len() != 64
            || !self.sha256.chars().all(|c| {
                c.is_ascii_hexdigit() && (!c.is_ascii_alphabetic() || c.is_ascii_lowercase())
            })
        {
            return Err(EvaError::invalid_argument(
                "package metadata sha256 is invalid",
            ));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn artifact(target: &str) -> PackageArtifactMetadata {
        let suffix = if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let name = format!("eva-cli-1.11.5-alpha-{target}.{suffix}");
        PackageArtifactMetadata::new(
            target,
            name.clone(),
            suffix,
            format!("https://github.com/Yetmos/Eva-CLI/releases/download/v1.11.5-alpha/{name}"),
            "a".repeat(64),
        )
    }
    #[test]
    fn metadata_is_sorted_and_deterministic() {
        let metadata = CanonicalPackageMetadata::new(
            "1.11.5-alpha",
            "0123456789abcdef0123456789abcdef01234567",
            vec![
                artifact("x86_64-unknown-linux-gnu"),
                artifact("x86_64-pc-windows-msvc"),
                artifact("aarch64-apple-darwin"),
            ],
        )
        .unwrap();
        assert_eq!(metadata.artifacts[0].target, "aarch64-apple-darwin");
        assert_eq!(metadata.to_manifest(), metadata.to_manifest());
    }
    #[test]
    fn missing_target_and_digest_fail_closed() {
        assert!(CanonicalPackageMetadata::new(
            "1.11.5-alpha",
            "0123456789abcdef0123456789abcdef01234567",
            vec![artifact("x86_64-unknown-linux-gnu")]
        )
        .is_err());
        let mut bad = artifact("x86_64-pc-windows-msvc");
        bad.sha256 = "ABC".to_owned();
        assert!(bad.validate("1.11.5-alpha").is_err());
    }
}
