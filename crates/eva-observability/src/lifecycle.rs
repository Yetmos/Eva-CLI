//! Runtime-owned observability lifecycle.
//! 中文：由运行时持有的可观测性生命周期，统一管理管线启动、刷新和关闭。
use crate::{BestEffortObservabilityPipeline, ObservabilitySmokeReport, TraceFields};
use eva_core::EvaError;
use std::path::{Path, PathBuf};

#[derive(Debug)]
/// 中文：封装可观测性管线及其存储根目录，并阻止关闭后继续刷新数据。
pub struct RuntimeObservabilityLifecycle {
    /// 中文：诊断数据和冒烟报告使用的持久化根目录。
    root: PathBuf,
    /// 中文：以尽力而为语义写入审计、指标和追踪数据的管线。
    pipeline: BestEffortObservabilityPipeline,
    /// 中文：记录管线是否仍可接受刷新请求。
    started: bool,
}

impl RuntimeObservabilityLifecycle {
    /// 中文：在指定根目录打开可观测性管线，并将生命周期置为已启动状态。
    pub fn start(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            pipeline: BestEffortObservabilityPipeline::open(&root),
            root,
            started: true,
        }
    }
    /// 中文：返回底层管线的可变引用，供运行时提交观测数据。
    pub fn pipeline_mut(&mut self) -> &mut BestEffortObservabilityPipeline {
        &mut self.pipeline
    }
    /// 中文：生成当前管线的冒烟报告；生命周期关闭后返回冲突错误。
    pub fn flush(&mut self, trace: &TraceFields) -> Result<ObservabilitySmokeReport, EvaError> {
        if !self.started {
            return Err(EvaError::conflict("observability lifecycle is stopped"));
        }
        Ok(self.pipeline.smoke_report(
            self.root.display().to_string(),
            trace.request_id.as_ref().map(|id| id.as_str().to_owned()),
        ))
    }
    /// 中文：在生成最后一份冒烟报告后关闭生命周期，后续刷新将被拒绝。
    pub fn shutdown(&mut self, trace: &TraceFields) -> Result<ObservabilitySmokeReport, EvaError> {
        let report = self.flush(trace)?;
        self.started = false;
        Ok(report)
    }
    /// 中文：返回可观测性数据的持久化根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 中文：验证关闭操作会先刷新报告，并阻止生命周期关闭后的再次刷新。
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
