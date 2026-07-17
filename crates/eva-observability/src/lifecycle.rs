//! Runtime-owned observability lifecycle.
use crate::{BestEffortObservabilityPipeline, ObservabilitySmokeReport, TraceFields};
use eva_core::EvaError;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct RuntimeObservabilityLifecycle {
    root: PathBuf,
    pipeline: BestEffortObservabilityPipeline,
    started: bool,
}

impl RuntimeObservabilityLifecycle {
    pub fn start(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            pipeline: BestEffortObservabilityPipeline::open(&root),
            root,
            started: true,
        }
    }
    pub fn pipeline_mut(&mut self) -> &mut BestEffortObservabilityPipeline {
        &mut self.pipeline
    }
    pub fn flush(&mut self, trace: &TraceFields) -> Result<ObservabilitySmokeReport, EvaError> {
        if !self.started {
            return Err(EvaError::conflict("observability lifecycle is stopped"));
        }
        Ok(self.pipeline.smoke_report(
            self.root.display().to_string(),
            trace.request_id.as_ref().map(|id| id.as_str().to_owned()),
        ))
    }
    pub fn shutdown(&mut self, trace: &TraceFields) -> Result<ObservabilitySmokeReport, EvaError> {
        let report = self.flush(trace)?;
        self.started = false;
        Ok(report)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_flushes_and_rejects_after_shutdown() {
        let root = std::env::temp_dir().join(format!("eva-observe-life-{}", std::process::id()));
        let mut l = RuntimeObservabilityLifecycle::start(&root);
        let t = TraceFields::default();
        l.shutdown(&t).unwrap();
        assert!(l.flush(&t).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
