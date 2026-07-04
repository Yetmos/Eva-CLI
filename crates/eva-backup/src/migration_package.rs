//! Migration package construction and verification.

use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "migration package construction and verification";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPackageManifest {
    pub package_id: String,
    pub source_version: String,
    pub target_version: String,
    pub reversible: bool,
    pub affected_sections: Vec<String>,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPreflight {
    pub package_id: String,
    pub status: String,
    pub warnings: Vec<String>,
    pub audit: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationPackageService;

impl MigrationPackageManifest {
    pub fn new(
        package_id: impl Into<String>,
        source_version: impl Into<String>,
        target_version: impl Into<String>,
        affected_sections: Vec<String>,
    ) -> Result<Self, EvaError> {
        let package_id = package_id.into();
        let source_version = source_version.into();
        let target_version = target_version.into();
        if package_id.trim().is_empty()
            || source_version.trim().is_empty()
            || target_version.trim().is_empty()
        {
            return Err(EvaError::invalid_argument(
                "migration package id and versions are required",
            ));
        }
        if affected_sections.is_empty() {
            return Err(EvaError::invalid_argument(
                "migration package must declare affected sections",
            ));
        }
        let checksum = format!(
            "pkg:{}:{}:{}:{}",
            package_id,
            source_version,
            target_version,
            affected_sections.join(",")
        );
        Ok(Self {
            package_id,
            source_version,
            target_version,
            reversible: true,
            affected_sections,
            checksum,
        })
    }

    pub fn irreversible(mut self) -> Self {
        self.reversible = false;
        self
    }
}

impl MigrationPackageService {
    pub fn verify_preflight(
        &self,
        manifest: &MigrationPackageManifest,
        current_version: &str,
    ) -> Result<MigrationPreflight, EvaError> {
        if manifest.source_version != current_version {
            return Ok(MigrationPreflight {
                package_id: manifest.package_id.clone(),
                status: "blocked".to_owned(),
                warnings: vec![format!(
                    "package source version {} does not match current version {}",
                    manifest.source_version, current_version
                )],
                audit: vec!["migration:preflight_blocked".to_owned()],
            });
        }
        let mut warnings = Vec::new();
        if !manifest.reversible {
            warnings.push(
                "migration package is irreversible and requires explicit approval".to_owned(),
            );
        }
        Ok(MigrationPreflight {
            package_id: manifest.package_id.clone(),
            status: if warnings.is_empty() {
                "ready"
            } else {
                "planned"
            }
            .to_owned(),
            warnings,
            audit: vec!["migration:preflight_verified".to_owned()],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_preflight_blocks_wrong_source_version() {
        let manifest =
            MigrationPackageManifest::new("pkg-v14", "1.3.0", "1.4.0", vec!["state".to_owned()])
                .unwrap();

        let report = MigrationPackageService
            .verify_preflight(&manifest, "1.2.0")
            .unwrap();

        assert_eq!(report.status, "blocked");
    }
}
