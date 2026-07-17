//! Controlled filesystem output for package-manager metadata.
use crate::{
    generate_apt_metadata, generate_homebrew_formula, generate_winget_manifests,
    CanonicalPackageMetadata,
};
use eva_core::EvaError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMetadataOutput {
    pub files: Vec<PathBuf>,
}
pub fn write_package_manager_metadata(
    metadata: &CanonicalPackageMetadata,
    output_root: impl AsRef<Path>,
    installed_size_kib: u64,
) -> Result<PackageMetadataOutput, EvaError> {
    let root = output_root.as_ref();
    fs::create_dir_all(root).map_err(|e| {
        EvaError::internal("create package metadata root").with_context("error", e.to_string())
    })?;
    let formula = generate_homebrew_formula(metadata)?;
    let winget = generate_winget_manifests(metadata)?;
    let apt = generate_apt_metadata(metadata, installed_size_kib)?;
    let files = [
        ("homebrew/eva-cli.rb", formula),
        ("winget/Yetmos.EvaCLI.yaml", winget.version),
        ("winget/Yetmos.EvaCLI.installer.yaml", winget.installer),
        ("winget/Yetmos.EvaCLI.locale.en-US.yaml", winget.locale),
        ("apt/control", apt.control),
        ("apt/Packages", apt.packages),
    ];
    let mut written = Vec::new();
    for (relative, content) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                EvaError::internal("create package metadata directory")
                    .with_context("error", e.to_string())
            })?;
        }
        atomic_write(&path, content.as_bytes())?;
        written.push(path);
    }
    Ok(PackageMetadataOutput { files: written })
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), EvaError> {
    let tmp = path.with_extension("metadata.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)
        .map_err(|e| {
            EvaError::internal("open package metadata temp file")
                .with_context("error", e.to_string())
        })?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| {
            EvaError::internal("write package metadata temp file")
                .with_context("error", e.to_string())
        })?;
    fs::rename(tmp, path).map_err(|e| {
        EvaError::internal("publish package metadata file").with_context("error", e.to_string())
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackageArtifactMetadata;
    #[test]
    fn writes_expected_six_files() {
        let root = std::env::temp_dir().join(format!("eva-package-output-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
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
                        "a".repeat(64),
                    )
                })
                .collect(),
        )
        .unwrap();
        let report = write_package_manager_metadata(&m, &root, 2048).unwrap();
        assert_eq!(report.files.len(), 6);
        assert!(root.join("apt/Packages").is_file());
        let _ = fs::remove_dir_all(root);
    }
}
