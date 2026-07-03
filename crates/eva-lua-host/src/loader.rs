//! Lua script loading for the controlled V0.4 host contract.

use eva_core::EvaError;
use std::fs;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load Lua scripts into controlled state generations";

/// Loaded Lua source. V0.4 keeps source as data and does not embed a VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaScript {
    pub path: Option<PathBuf>,
    source: String,
}

impl LuaScript {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, EvaError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|error| {
            EvaError::not_found("failed to read Lua script")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            source,
        })
    }

    pub fn from_source(source: impl Into<String>) -> Self {
        Self {
            path: None,
            source: source.into(),
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}
