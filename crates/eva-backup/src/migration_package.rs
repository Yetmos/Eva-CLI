//! 迁移包的构建与执行前验证。
//! Migration package construction and verification.

use eva_core::EvaError;

/// 本模块的架构职责：构建迁移包清单并验证版本前置条件。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "migration package construction and verification";

/// 描述一次版本迁移边界的包清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPackageManifest {
    /// 迁移包的稳定标识。
    pub package_id: String,
    /// 可以应用该迁移的唯一源版本。
    pub source_version: String,
    /// 迁移完成后的目标版本。
    pub target_version: String,
    /// 是否声明可通过配套步骤逆向恢复。
    pub reversible: bool,
    /// 迁移可能修改的配置或状态分区。
    pub affected_sections: Vec<String>,
    /// 由关键清单字段派生的确定性校验字符串。
    pub checksum: String,
}

/// 迁移包执行前的门禁结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPreflight {
    /// 被检查的迁移包标识。
    pub package_id: String,
    /// `ready`、`planned` 或 `blocked` 状态。
    pub status: String,
    /// 需要显式审批或阻止执行的风险说明。
    pub warnings: Vec<String>,
    /// 前置检查的审计记录。
    pub audit: Vec<String>,
}

/// 构建迁移前置检查结果的无状态服务。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationPackageService;

impl MigrationPackageManifest {
    /// 校验标识、版本和非空影响范围后创建默认可逆的清单。
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

    /// 将迁移显式标记为不可逆，以触发额外审批警告。
    pub fn irreversible(mut self) -> Self {
        self.reversible = false;
        self
    }
}

impl MigrationPackageService {
    /// 验证当前版本严格匹配源版本，并暴露不可逆迁移风险。
    ///
    /// 版本不匹配以正常的 blocked 报告返回，便于发布检查展示；清单不可逆不会自动
    /// 执行或硬失败，而是把状态降为 planned，要求更高层策略显式审批。
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
/// 迁移包版本门禁测试。
mod tests {
    use super::*;

    #[test]
    /// 验证当前版本与源版本不一致时迁移被阻塞。
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
