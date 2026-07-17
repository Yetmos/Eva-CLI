//! Durable, cross-process provider admission reservations.

use eva_core::{AdapterId, EvaError};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

pub const DEFAULT_RESERVATION_TTL_MS: u128 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdmissionReservation {
    pub reservation_id: String,
    pub session_id: String,
    pub expires_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdmissionSnapshot {
    pub adapter_id: AdapterId,
    pub max_concurrency: usize,
    pub reservations: Vec<ProviderAdmissionReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemProviderAdmissionTable {
    root: PathBuf,
    lock_wait: Duration,
}

impl FileSystemProviderAdmissionTable {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, EvaError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| {
            EvaError::internal("create admission directory").with_context("error", e.to_string())
        })?;
        Ok(Self {
            root,
            lock_wait: Duration::from_millis(5),
        })
    }

    pub fn reserve(
        &self,
        adapter_id: &AdapterId,
        max_concurrency: usize,
        session_id: &str,
        now_ms: u128,
        ttl_ms: u128,
    ) -> Result<ProviderAdmissionReservation, EvaError> {
        if max_concurrency == 0 || session_id.is_empty() {
            return Err(EvaError::invalid_argument(
                "provider admission reservation is invalid",
            ));
        }
        let _lock = self.lock(adapter_id)?;
        let mut state = self.read(adapter_id)?;
        state.max_concurrency = max_concurrency;
        state.reservations.retain(|r| r.expires_at_ms > now_ms);
        if let Some(existing) = state
            .reservations
            .iter()
            .find(|r| r.session_id == session_id)
        {
            return Ok(existing.clone());
        }
        if state.reservations.len() >= max_concurrency {
            return Err(
                EvaError::unavailable("provider concurrency limit is exhausted")
                    .with_retryable(true)
                    .with_context("provider_code", "provider_concurrency_limited"),
            );
        }
        let reservation = ProviderAdmissionReservation {
            reservation_id: format!("{}-{}", session_id, now_ms),
            session_id: session_id.to_owned(),
            expires_at_ms: now_ms.saturating_add(ttl_ms.max(1)),
        };
        state.reservations.push(reservation.clone());
        self.write(adapter_id, &state)?;
        Ok(reservation)
    }

    pub fn release(&self, adapter_id: &AdapterId, session_id: &str) -> Result<(), EvaError> {
        let _lock = self.lock(adapter_id)?;
        let mut state = self.read(adapter_id)?;
        state.reservations.retain(|r| r.session_id != session_id);
        self.write(adapter_id, &state)
    }

    pub fn snapshot(
        &self,
        adapter_id: &AdapterId,
        now_ms: u128,
    ) -> Result<ProviderAdmissionSnapshot, EvaError> {
        let _lock = self.lock(adapter_id)?;
        let mut state = self.read(adapter_id)?;
        state.reservations.retain(|r| r.expires_at_ms > now_ms);
        self.write(adapter_id, &state)?;
        Ok(state)
    }

    fn path(&self, adapter_id: &AdapterId) -> PathBuf {
        self.root
            .join(format!("{}.admission", digest(adapter_id.as_str())))
    }
    fn lock_path(&self, adapter_id: &AdapterId) -> PathBuf {
        self.root
            .join(format!("{}.lock", digest(adapter_id.as_str())))
    }
    fn lock(&self, adapter_id: &AdapterId) -> Result<AdmissionLock, EvaError> {
        let path = self.lock_path(adapter_id);
        for _ in 0..200 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(AdmissionLock { file, path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    thread::sleep(self.lock_wait)
                }
                Err(e) => {
                    return Err(EvaError::internal("acquire provider admission lock")
                        .with_context("error", e.to_string()))
                }
            }
        }
        Err(EvaError::timeout(
            "provider admission lock acquisition timed out",
        ))
    }
    fn read(&self, adapter_id: &AdapterId) -> Result<ProviderAdmissionSnapshot, EvaError> {
        let path = self.path(adapter_id);
        if !path.exists() {
            return Ok(ProviderAdmissionSnapshot {
                adapter_id: adapter_id.clone(),
                max_concurrency: 0,
                reservations: Vec::new(),
            });
        }
        let mut text = String::new();
        File::open(&path)
            .and_then(|mut f| f.read_to_string(&mut text))
            .map_err(|e| {
                EvaError::internal("read provider admission").with_context("error", e.to_string())
            })?;
        decode(adapter_id, &text)
    }
    fn write(
        &self,
        adapter_id: &AdapterId,
        state: &ProviderAdmissionSnapshot,
    ) -> Result<(), EvaError> {
        let path = self.path(adapter_id);
        let tmp = path.with_extension("admission.tmp");
        let mut file = File::create(&tmp).map_err(|e| {
            EvaError::internal("write provider admission").with_context("error", e.to_string())
        })?;
        file.write_all(encode(state).as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| {
                EvaError::internal("write provider admission").with_context("error", e.to_string())
            })?;
        fs::rename(tmp, path).map_err(|e| {
            EvaError::internal("publish provider admission").with_context("error", e.to_string())
        })
    }
}

#[derive(Debug)]
struct AdmissionLock {
    file: File,
    path: PathBuf,
}
impl Drop for AdmissionLock {
    fn drop(&mut self) {
        let _ = self.file.sync_all();
        let _ = fs::remove_file(&self.path);
    }
}

fn digest(value: &str) -> String {
    let mut h = Sha256::new();
    h.update(value.as_bytes());
    format!("{:x}", h.finalize())
}
fn encode(s: &ProviderAdmissionSnapshot) -> String {
    let mut out = format!("version=1\nmax={}\n", s.max_concurrency);
    for r in &s.reservations {
        out.push_str(&format!(
            "reservation={}\t{}\t{}\n",
            r.reservation_id, r.session_id, r.expires_at_ms
        ));
    }
    out
}
fn decode(adapter: &AdapterId, text: &str) -> Result<ProviderAdmissionSnapshot, EvaError> {
    let mut max = 0;
    let mut reservations = Vec::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("max=") {
            max = v
                .parse()
                .map_err(|_| EvaError::invalid_argument("invalid admission max"))?;
        } else if let Some(v) = line.strip_prefix("reservation=") {
            let p: Vec<_> = v.split('\t').collect();
            if p.len() != 3 {
                return Err(EvaError::invalid_argument("invalid admission reservation"));
            }
            reservations.push(ProviderAdmissionReservation {
                reservation_id: p[0].to_owned(),
                session_id: p[1].to_owned(),
                expires_at_ms: p[2]
                    .parse()
                    .map_err(|_| EvaError::invalid_argument("invalid admission expiry"))?,
            });
        }
    }
    Ok(ProviderAdmissionSnapshot {
        adapter_id: adapter.clone(),
        max_concurrency: max,
        reservations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "eva-admission-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn capacity_release_and_expiry_are_durable() {
        let root = root();
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let adapter = AdapterId::parse("provider-admission").unwrap();
        assert!(table.reserve(&adapter, 1, "s1", 10, 100).is_ok());
        assert!(table.reserve(&adapter, 1, "s2", 11, 100).is_err());
        table.release(&adapter, "s1").unwrap();
        assert!(table.reserve(&adapter, 1, "s2", 12, 100).is_ok());
        assert!(table
            .snapshot(&adapter, 200)
            .unwrap()
            .reservations
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn same_session_reservation_is_idempotent() {
        let root = root();
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let adapter = AdapterId::parse("provider-admission-idempotent").unwrap();
        let first = table.reserve(&adapter, 1, "s1", 10, 100).unwrap();
        let second = table.reserve(&adapter, 1, "s1", 11, 100).unwrap();
        assert_eq!(first, second);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn two_processes_have_one_winner_for_capacity_one() {
        if let Ok(root) = std::env::var("EVA_ADMISSION_CHILD_ROOT") {
            let table = FileSystemProviderAdmissionTable::new(root).unwrap();
            let adapter = AdapterId::parse("provider-admission-process").unwrap();
            let result = table.reserve(
                &adapter,
                1,
                &format!("child-{}", std::process::id()),
                10,
                30_000,
            );
            std::process::exit(if result.is_ok() { 0 } else { 7 });
        }

        let root = root();
        let exe = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for _ in 0..2 {
            children.push(
                Command::new(&exe)
                    .arg(
                        "provider_admission::tests::two_processes_have_one_winner_for_capacity_one",
                    )
                    .arg("--exact")
                    .arg("--nocapture")
                    .env("EVA_ADMISSION_CHILD_ROOT", &root)
                    .spawn()
                    .unwrap(),
            );
        }
        let mut successes = 0;
        for mut child in children {
            let status = child.wait().unwrap();
            if status.success() {
                successes += 1;
            } else {
                assert_eq!(status.code(), Some(7));
            }
        }
        assert_eq!(successes, 1);
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let snapshot = table
            .snapshot(&AdapterId::parse("provider-admission-process").unwrap(), 20)
            .unwrap();
        assert_eq!(snapshot.reservations.len(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
