//! Canonical configuration generation and provenance digest.
use crate::ProjectConfig;
use eva_core::EvaError;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigGeneration {
    pub generation: u64,
    pub digest: String,
    pub environment: String,
}

impl ConfigGeneration {
    pub fn from_project(project: &ProjectConfig, generation: u64) -> Result<Self, EvaError> {
        if generation == 0 {
            return Err(EvaError::invalid_argument(
                "config generation must be positive",
            ));
        }
        let environment = project.eva.runtime.env.clone();
        let canonical = format!("env={environment}\nproject_root={}\neva={:?}\nroots={:?}\nagents={:?}\nadapters={:?}\ncapabilities={:?}\npolicies={:?}\nroutes={:?}\n", project.project_root.display(), project.eva, project.roots, project.agents, project.adapters, project.capabilities, project.policies, project.routes);
        let mut parts = [0u64; 4];
        for (index, part) in parts.iter_mut().enumerate() {
            let mut hasher = DefaultHasher::new();
            index.hash(&mut hasher);
            canonical.hash(&mut hasher);
            *part = hasher.finish();
        }
        Ok(Self {
            generation,
            digest: parts.iter().map(|part| format!("{part:016x}")).collect(),
            environment,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_project_config;
    #[test]
    fn digest_is_stable_for_same_project() {
        let root = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let p = load_project_config(root).unwrap();
        let a = ConfigGeneration::from_project(&p, 1).unwrap();
        let b = ConfigGeneration::from_project(&p, 1).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.digest.len(), 64);
    }

    #[test]
    fn digest_changes_when_runtime_environment_changes() {
        let root = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let project = load_project_config(root).unwrap();
        let baseline = ConfigGeneration::from_project(&project, 1).unwrap();
        let mut changed = project.clone();
        changed.eva.runtime.env.push_str("-override");
        let overridden = ConfigGeneration::from_project(&changed, 1).unwrap();
        assert_ne!(baseline.environment, overridden.environment);
        assert_ne!(baseline.digest, overridden.digest);
    }
}
