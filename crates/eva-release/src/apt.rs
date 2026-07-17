//! Deterministic Debian/Apt metadata generation.
use crate::CanonicalPackageMetadata;
use eva_core::EvaError;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AptMetadata {
    pub control: String,
    pub packages: String,
}
pub fn generate_apt_metadata(
    metadata: &CanonicalPackageMetadata,
    installed_size_kib: u64,
) -> Result<AptMetadata, EvaError> {
    if installed_size_kib == 0 {
        return Err(EvaError::invalid_argument(
            "Apt installed size must be positive",
        ));
    }
    let a = metadata
        .artifacts
        .iter()
        .find(|a| a.target == "x86_64-unknown-linux-gnu")
        .ok_or_else(|| EvaError::not_found("Linux package metadata is missing"))?;
    let control=format!("Package: eva-cli\nVersion: {}\nArchitecture: amd64\nMaintainer: Yetmos\nInstalled-Size: {}\nSection: utils\nPriority: optional\nHomepage: https://github.com/Yetmos/Eva-CLI\nDescription: Local-first agent runtime CLI\n",metadata.version,installed_size_kib);
    let packages=format!("Package: eva-cli\nVersion: {}\nArchitecture: amd64\nFilename: pool/main/e/eva-cli/eva-cli_{}_amd64.deb\nSHA256: {}\nHomepage: https://github.com/Yetmos/Eva-CLI\nDescription: Local-first agent runtime CLI\nX-Upstream-Artifact: {}\n",metadata.version,metadata.version,a.sha256,a.download_url);
    Ok(AptMetadata { control, packages })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackageArtifactMetadata;
    #[test]
    fn apt_metadata_uses_linux_digest() {
        let targets = [
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
        ];
        let m = CanonicalPackageMetadata::new(
            "1.11.5-alpha",
            "0123456789abcdef0123456789abcdef01234567",
            targets
                .into_iter()
                .map(|t| {
                    let s = if t.contains("windows") {
                        "zip"
                    } else {
                        "tar.gz"
                    };
                    let n = format!("eva-cli-1.11.5-alpha-{t}.{s}");
                    PackageArtifactMetadata::new(
                        t,
                        n.clone(),
                        s,
                        format!("https://example.test/{n}"),
                        if t.contains("linux") {
                            "b".repeat(64)
                        } else {
                            "a".repeat(64)
                        },
                    )
                })
                .collect(),
        )
        .unwrap();
        let out = generate_apt_metadata(&m, 2048).unwrap();
        assert!(out.control.contains("Architecture: amd64"));
        assert!(out.packages.contains(&"b".repeat(64)));
    }
}
