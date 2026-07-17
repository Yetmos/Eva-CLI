//! Credential-free canonical package metadata.
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

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
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let mut fields = BTreeMap::new();
        for line in data.lines() {
            let line = line.trim_start_matches('\u{feff}');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::invalid_argument(
                    "package metadata line must use key=value",
                ));
            };
            if fields.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err(
                    EvaError::invalid_argument("package metadata field is duplicated")
                        .with_context("field", key),
                );
            }
        }
        if fields.remove("format").as_deref() != Some("eva.package-metadata.v1") {
            return Err(EvaError::invalid_argument(
                "package metadata format is invalid",
            ));
        }
        let version = fields
            .remove("version")
            .ok_or_else(|| EvaError::invalid_argument("package metadata version is missing"))?;
        let tag = fields
            .remove("source_tag")
            .ok_or_else(|| EvaError::invalid_argument("package metadata source tag is missing"))?;
        let commit = fields.remove("source_commit").ok_or_else(|| {
            EvaError::invalid_argument("package metadata source commit is missing")
        })?;
        let mut artifacts = Vec::new();
        for index in 0..3 {
            let take = |fields: &mut BTreeMap<String, String>, name: &str| {
                fields
                    .remove(&format!("artifact.{index}.{name}"))
                    .ok_or_else(|| {
                        EvaError::invalid_argument("package metadata artifact field is missing")
                            .with_context("field", format!("artifact.{index}.{name}"))
                    })
            };
            artifacts.push(PackageArtifactMetadata::new(
                take(&mut fields, "target")?,
                take(&mut fields, "name")?,
                take(&mut fields, "format")?,
                take(&mut fields, "url")?,
                take(&mut fields, "sha256")?,
            ));
        }
        if !fields.is_empty() {
            return Err(
                EvaError::invalid_argument("package metadata contains unknown field")
                    .with_context("field", fields.keys().next().unwrap()),
            );
        }
        let metadata = Self::new(version, commit, artifacts)?;
        if metadata.source_tag != tag {
            return Err(EvaError::invalid_argument(
                "package metadata source tag does not match version",
            ));
        }
        let normalized_input = data.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        if metadata.to_manifest() != normalized_input {
            return Err(EvaError::invalid_argument(
                "package metadata manifest is not canonical",
            ));
        }
        Ok(metadata)
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
    #[test]
    fn canonical_manifest_round_trips_and_rejects_unknown_fields() {
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
        let manifest = metadata.to_manifest();
        assert_eq!(
            CanonicalPackageMetadata::parse_manifest(&manifest).unwrap(),
            metadata
        );
        assert!(CanonicalPackageMetadata::parse_manifest(&(manifest + "unknown=value\n")).is_err());
    }
}
