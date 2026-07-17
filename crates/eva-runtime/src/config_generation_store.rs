use eva_config::ConfigGeneration;
use eva_core::EvaError;
use eva_storage::{atomic_write, DurableBackendLayout};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationPhase {
    Stable,
    Prepared,
    Promoted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRecord {
    pub generation: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationManifest {
    pub phase: GenerationPhase,
    pub active: GenerationRecord,
    pub previous: Option<GenerationRecord>,
    pub candidate: Option<GenerationRecord>,
}

pub struct ConfigGenerationStore {
    path: PathBuf,
}

impl ConfigGenerationStore {
    pub fn new(layout: &DurableBackendLayout) -> Self {
        Self {
            path: layout.state_dir.join("config-generation.manifest"),
        }
    }

    pub fn initialize(&self, active: &ConfigGeneration) -> Result<GenerationManifest, EvaError> {
        if self.path.exists() {
            return self.recover();
        }
        let manifest = GenerationManifest {
            phase: GenerationPhase::Stable,
            active: active.into(),
            previous: None,
            candidate: None,
        };
        self.write(&manifest)?;
        Ok(manifest)
    }

    pub fn prepare(&self, candidate: &ConfigGeneration) -> Result<GenerationManifest, EvaError> {
        let mut manifest = self.recover()?;
        if manifest.phase != GenerationPhase::Stable
            || candidate.generation <= manifest.active.generation
        {
            return Err(EvaError::conflict(
                "config generation prepare is not monotonic",
            ));
        }
        manifest.phase = GenerationPhase::Prepared;
        manifest.candidate = Some(candidate.into());
        self.write(&manifest)?;
        Ok(manifest)
    }

    pub fn promote(&self) -> Result<GenerationManifest, EvaError> {
        let mut manifest = self.read()?;
        if manifest.phase != GenerationPhase::Prepared {
            return Err(EvaError::conflict("config generation is not prepared"));
        }
        let candidate = manifest
            .candidate
            .take()
            .ok_or_else(|| EvaError::conflict("prepared generation missing candidate"))?;
        manifest.previous = Some(manifest.active);
        manifest.active = candidate;
        manifest.phase = GenerationPhase::Promoted;
        self.write(&manifest)?;
        Ok(manifest)
    }

    pub fn retire(&self) -> Result<GenerationManifest, EvaError> {
        let mut manifest = self.read()?;
        if manifest.phase != GenerationPhase::Promoted {
            return Err(EvaError::conflict("config generation is not promoted"));
        }
        manifest.phase = GenerationPhase::Stable;
        manifest.previous = None;
        self.write(&manifest)?;
        Ok(manifest)
    }

    pub fn recover(&self) -> Result<GenerationManifest, EvaError> {
        let mut manifest = self.read()?;
        if manifest.phase == GenerationPhase::Prepared {
            manifest.phase = GenerationPhase::Stable;
            manifest.candidate = None;
            self.write(&manifest)?;
        } else if manifest.phase == GenerationPhase::Promoted {
            manifest.phase = GenerationPhase::Stable;
            manifest.previous = None;
            self.write(&manifest)?;
        }
        Ok(manifest)
    }

    fn write(&self, value: &GenerationManifest) -> Result<(), EvaError> {
        fs::create_dir_all(self.path.parent().unwrap()).map_err(|e| {
            EvaError::internal("create config generation state directory")
                .with_context("io_error", e.to_string())
        })?;
        atomic_write(&self.path, encode(value).as_bytes()).map_err(|e| {
            EvaError::internal("write config generation manifest")
                .with_context("io_error", e.to_string())
        })
    }

    fn read(&self) -> Result<GenerationManifest, EvaError> {
        let text = fs::read_to_string(&self.path).map_err(|e| {
            EvaError::not_found("read config generation manifest")
                .with_context("io_error", e.to_string())
        })?;
        decode(&text)
    }
}

impl From<&ConfigGeneration> for GenerationRecord {
    fn from(value: &ConfigGeneration) -> Self {
        Self {
            generation: value.generation,
            digest: value.digest.clone(),
        }
    }
}

fn encode(v: &GenerationManifest) -> String {
    let (pg, pd) = v
        .previous
        .as_ref()
        .map(|r| (r.generation.to_string(), r.digest.clone()))
        .unwrap_or(("none".into(), "none".into()));
    let (cg, cd) = v
        .candidate
        .as_ref()
        .map(|r| (r.generation.to_string(), r.digest.clone()))
        .unwrap_or(("none".into(), "none".into()));
    format!("version=1\nphase={}\nactive_generation={}\nactive_digest={}\nprevious_generation={}\nprevious_digest={}\ncandidate_generation={}\ncandidate_digest={}\n", match v.phase {GenerationPhase::Stable=>"stable",GenerationPhase::Prepared=>"prepared",GenerationPhase::Promoted=>"promoted"},v.active.generation,v.active.digest,pg,pd,cg,cd)
}

fn decode(text: &str) -> Result<GenerationManifest, EvaError> {
    let mut values = std::collections::BTreeMap::new();
    for line in text.lines() {
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| EvaError::conflict("invalid config generation manifest line"))?;
        if values.insert(k, v).is_some() {
            return Err(EvaError::conflict(
                "duplicate config generation manifest field",
            ));
        }
    }
    if values.len() != 8 || values.get("version") != Some(&"1") {
        return Err(EvaError::conflict(
            "invalid config generation manifest fields",
        ));
    }
    let phase = match values["phase"] {
        "stable" => GenerationPhase::Stable,
        "prepared" => GenerationPhase::Prepared,
        "promoted" => GenerationPhase::Promoted,
        _ => return Err(EvaError::conflict("invalid config generation phase")),
    };
    let record = |prefix: &str| -> Result<Option<GenerationRecord>, EvaError> {
        let g = values[&*format!("{prefix}_generation")];
        let d = values[&*format!("{prefix}_digest")];
        if g == "none" && d == "none" {
            return Ok(None);
        };
        let generation = g
            .parse()
            .map_err(|_| EvaError::conflict("invalid config generation number"))?;
        if generation == 0 || !d.starts_with("sha256:") {
            return Err(EvaError::conflict("invalid config generation record"));
        }
        Ok(Some(GenerationRecord {
            generation,
            digest: d.into(),
        }))
    };
    let active = record("active")?
        .ok_or_else(|| EvaError::conflict("config generation manifest missing active"))?;
    let previous = record("previous")?;
    let candidate = record("candidate")?;
    match phase {
        GenerationPhase::Stable if previous.is_some() || candidate.is_some() => {
            return Err(EvaError::conflict(
                "stable config generation has transient records",
            ))
        }
        GenerationPhase::Prepared if candidate.is_none() || previous.is_some() => {
            return Err(EvaError::conflict("invalid prepared config generation"))
        }
        GenerationPhase::Promoted if previous.is_none() || candidate.is_some() => {
            return Err(EvaError::conflict("invalid promoted config generation"))
        }
        _ => {}
    }
    Ok(GenerationManifest {
        phase,
        active,
        previous,
        candidate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "eva-generation-store-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn identity(generation: u64, digit: char) -> ConfigGeneration {
        ConfigGeneration {
            generation,
            digest: format!("sha256:{}", digit.to_string().repeat(64)),
            environment: "test".into(),
        }
    }

    #[test]
    fn crash_points_recover_exactly_one_active_generation() {
        let root = root();
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let store = ConfigGenerationStore::new(backend.layout());
        store.initialize(&identity(1, 'a')).unwrap();
        store.prepare(&identity(2, 'b')).unwrap();
        let recovered = ConfigGenerationStore::new(backend.layout())
            .recover()
            .unwrap();
        assert_eq!(recovered.phase, GenerationPhase::Stable);
        assert_eq!(recovered.active.generation, 1);
        assert!(recovered.previous.is_none());
        assert!(recovered.candidate.is_none());
        store.prepare(&identity(2, 'b')).unwrap();
        store.promote().unwrap();
        let recovered = ConfigGenerationStore::new(backend.layout())
            .recover()
            .unwrap();
        assert_eq!(recovered.phase, GenerationPhase::Stable);
        assert_eq!(recovered.active.generation, 2);
        assert!(recovered.previous.is_none());
        store.prepare(&identity(3, 'c')).unwrap();
        store.promote().unwrap();
        let stable = store.retire().unwrap();
        assert_eq!(stable.active.generation, 3);
        assert_eq!(store.recover().unwrap(), stable);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_manifest_fails_closed() {
        let root = root();
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let store = ConfigGenerationStore::new(backend.layout());
        store.initialize(&identity(1, 'a')).unwrap();
        fs::write(&store.path, "version=1\nphase=stable\n").unwrap();
        assert!(store.recover().is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
