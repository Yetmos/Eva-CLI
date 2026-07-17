//! Deterministic Homebrew formula generation.
use crate::CanonicalPackageMetadata;
use eva_core::EvaError;

pub fn generate_homebrew_formula(metadata: &CanonicalPackageMetadata) -> Result<String, EvaError> {
    let artifact = metadata
        .artifacts
        .iter()
        .find(|a| a.target == "aarch64-apple-darwin")
        .ok_or_else(|| EvaError::not_found("macOS package metadata is missing"))?;
    Ok(format!("class EvaCli < Formula\n  desc \"Local-first agent runtime CLI\"\n  homepage \"https://github.com/Yetmos/Eva-CLI\"\n  url \"{}\"\n  version \"{}\"\n  sha256 \"{}\"\n  license \"NOASSERTION\"\n\n  def install\n    bin.install \"eva\"\n  end\n\n  test do\n    assert_match version.to_s, shell_output(\"#{{bin}}/eva version\")\n  end\nend\n",artifact.download_url,metadata.version,artifact.sha256))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackageArtifactMetadata;
    fn metadata() -> CanonicalPackageMetadata {
        let targets = [
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
        ];
        CanonicalPackageMetadata::new(
            "1.11.5-alpha",
            "0123456789abcdef0123456789abcdef01234567",
            targets
                .into_iter()
                .map(|target| {
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
                        format!("https://example.test/{name}"),
                        "a".repeat(64),
                    )
                })
                .collect(),
        )
        .unwrap()
    }
    #[test]
    fn formula_is_deterministic_and_uses_macos_digest() {
        let formula = generate_homebrew_formula(&metadata()).unwrap();
        assert!(formula.contains("class EvaCli < Formula"));
        assert!(formula.contains(&"a".repeat(64)));
        assert_eq!(formula, generate_homebrew_formula(&metadata()).unwrap());
    }
}
