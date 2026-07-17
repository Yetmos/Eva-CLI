//! Joinable polling watcher for project configuration generations.

use eva_core::EvaError;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChangeBatch {
    pub paths: Vec<PathBuf>,
}

pub struct ConfigWatcher {
    stop: Sender<()>,
    changes: Receiver<ConfigChangeBatch>,
    join: Option<JoinHandle<Result<(), EvaError>>>,
}

impl ConfigWatcher {
    pub fn start(
        project_root: impl Into<PathBuf>,
        poll: Duration,
        debounce: Duration,
    ) -> Result<Self, EvaError> {
        if poll.is_zero() || debounce.is_zero() {
            return Err(EvaError::invalid_argument(
                "config watcher intervals must be positive",
            ));
        }
        let root = project_root.into().join("config");
        let baseline = snapshot(&root)?;
        let (stop, stop_rx) = mpsc::channel();
        let (change_tx, changes) = mpsc::channel();
        let join = thread::Builder::new()
            .name("eva-config-watcher".to_owned())
            .spawn(move || watch_loop(root, baseline, poll, debounce, stop_rx, change_tx))
            .map_err(|error| {
                EvaError::internal("spawn config watcher").with_context("error", error.to_string())
            })?;
        Ok(Self {
            stop,
            changes,
            join: Some(join),
        })
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<ConfigChangeBatch>, EvaError> {
        match self.changes.recv_timeout(timeout) {
            Ok(batch) => Ok(Some(batch)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(EvaError::unavailable("config watcher stopped"))
            }
        }
    }

    pub fn stop_and_join(&mut self) -> Result<(), EvaError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        let _ = self.stop.send(());
        join.join()
            .map_err(|_| EvaError::internal("config watcher thread panicked"))?
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn watch_loop(
    root: PathBuf,
    mut current: Snapshot,
    poll: Duration,
    debounce: Duration,
    stop: Receiver<()>,
    changes: Sender<ConfigChangeBatch>,
) -> Result<(), EvaError> {
    let mut pending = BTreeMap::<PathBuf, ()>::new();
    let mut last_change = None;
    loop {
        match stop.recv_timeout(poll) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        let next = snapshot(&root)?;
        for path in changed_paths(&current, &next) {
            pending.insert(path, ());
            last_change = Some(Instant::now());
        }
        current = next;
        if last_change.is_some_and(|observed| observed.elapsed() >= debounce) && !pending.is_empty()
        {
            let paths = std::mem::take(&mut pending).into_keys().collect();
            changes
                .send(ConfigChangeBatch { paths })
                .map_err(|_| EvaError::unavailable("config watcher consumer stopped"))?;
            last_change = None;
        }
    }
}

type Snapshot = BTreeMap<PathBuf, (u64, SystemTime)>;
fn snapshot(root: &Path) -> Result<Snapshot, EvaError> {
    let mut result = BTreeMap::new();
    collect(root, root, &mut result)?;
    Ok(result)
}
fn collect(root: &Path, path: &Path, output: &mut Snapshot) -> Result<(), EvaError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| {
            EvaError::not_found("read config watcher directory")
                .with_context("path", path.display().to_string())
                .with_context("error", error.to_string())
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            EvaError::internal("enumerate config watcher directory")
                .with_context("error", error.to_string())
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let metadata = entry.metadata().map_err(|error| {
            EvaError::internal("read config watcher metadata")
                .with_context("error", error.to_string())
        })?;
        let path = entry.path();
        if metadata.is_dir() {
            collect(root, &path, output)?;
        } else if metadata.is_file() && watched(&path) {
            output.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (
                    metadata.len(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ),
            );
        }
    }
    Ok(())
}
fn watched(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|v| v.to_str()),
        Some("yaml" | "yml" | "json" | "lua")
    )
}
fn changed_paths(old: &Snapshot, new: &Snapshot) -> Vec<PathBuf> {
    old.keys()
        .chain(new.keys())
        .filter(|path| old.get(*path) != new.get(*path))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn burst_changes_coalesce_and_shutdown_joins() {
        let root = std::env::temp_dir().join(format!("eva-watcher-{}", std::process::id()));
        let config = root.join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("eva.yaml"), "runtime: 1\n").unwrap();
        let mut watcher =
            ConfigWatcher::start(&root, Duration::from_millis(10), Duration::from_millis(30))
                .unwrap();
        fs::write(config.join("eva.yaml"), "runtime: 22\n").unwrap();
        fs::write(config.join("routes.yaml"), "routes: []\n").unwrap();
        let batch = watcher
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(
            batch.paths,
            vec![PathBuf::from("eva.yaml"), PathBuf::from("routes.yaml")]
        );
        assert!(watcher
            .recv_timeout(Duration::from_millis(80))
            .unwrap()
            .is_none());
        watcher.stop_and_join().unwrap();
        watcher.stop_and_join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
