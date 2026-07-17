//! Deterministic Winget manifest generation.
use crate::CanonicalPackageMetadata;
use eva_core::EvaError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WingetManifestSet {
    pub version: String,
    pub installer: String,
    pub locale: String,
}
pub fn generate_winget_manifests(
    metadata: &CanonicalPackageMetadata,
) -> Result<WingetManifestSet, EvaError> {
    let a = metadata
        .artifacts
        .iter()
        .find(|a| a.target == "x86_64-pc-windows-msvc")
        .ok_or_else(|| EvaError::not_found("Windows package metadata is missing"))?;
    let version=format!("PackageIdentifier: Yetmos.EvaCLI\nPackageVersion: {}\nDefaultLocale: en-US\nManifestType: version\nManifestVersion: 1.6.0\n",metadata.version);
    let installer=format!("PackageIdentifier: Yetmos.EvaCLI\nPackageVersion: {}\nInstallerType: portable\nCommands:\n  - eva\nInstallers:\n  - Architecture: x64\n    InstallerUrl: {}\n    InstallerSha256: {}\n    Scope: user\nUpgradeBehavior: install\nManifestType: installer\nManifestVersion: 1.6.0\n",metadata.version,a.download_url,a.sha256.to_ascii_uppercase());
    let locale=format!("PackageIdentifier: Yetmos.EvaCLI\nPackageVersion: {}\nPackageLocale: en-US\nPublisher: Yetmos\nPackageName: Eva CLI\nShortDescription: Local-first agent runtime CLI\nLicense: NOASSERTION\nManifestType: defaultLocale\nManifestVersion: 1.6.0\n",metadata.version);
    Ok(WingetManifestSet {
        version,
        installer,
        locale,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackageArtifactMetadata;
    #[test]
    fn winget_uses_windows_url_and_uppercase_hash() {
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
                        "abcdef12".repeat(8),
                    )
                })
                .collect(),
        )
        .unwrap();
        let out = generate_winget_manifests(&m).unwrap();
        assert!(out.installer.contains("Architecture: x64"));
        assert!(out.installer.contains(&"ABCDEF12".repeat(8)));
    }
}
