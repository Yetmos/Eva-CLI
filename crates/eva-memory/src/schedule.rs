//! Restart-safe durable maintenance schedule state.
use eva_core::EvaError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRecord {
    pub id: String,
    pub next_run_at_ms: u128,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<u128>,
    pub generation: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemScheduleStore {
    root: PathBuf,
}
impl FileSystemScheduleStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, EvaError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| {
            EvaError::internal("create schedule directory").with_context("error", e.to_string())
        })?;
        Ok(Self { root })
    }
    pub fn upsert(&self, id: &str, next_run_at_ms: u128) -> Result<ScheduleRecord, EvaError> {
        validate_id(id)?;
        let _lock = self.lock(id)?;
        let generation = self
            .read(id)
            .map(|r| r.generation.saturating_add(1))
            .unwrap_or(1);
        let r = ScheduleRecord {
            id: id.to_owned(),
            next_run_at_ms,
            lease_owner: None,
            lease_expires_at_ms: None,
            generation,
        };
        self.write(&r)?;
        Ok(r)
    }
    pub fn claim(
        &self,
        id: &str,
        owner: &str,
        now_ms: u128,
        lease_ms: u128,
    ) -> Result<ScheduleRecord, EvaError> {
        validate_id(id)?;
        if owner.is_empty() {
            return Err(EvaError::invalid_argument("schedule owner cannot be empty"));
        }
        let _lock = self.lock(id)?;
        let mut r = self.read(id)?;
        if r.next_run_at_ms > now_ms {
            return Err(EvaError::unavailable("schedule is not due").with_retryable(true));
        }
        if r.lease_expires_at_ms.is_some_and(|e| e > now_ms)
            && r.lease_owner.as_deref() != Some(owner)
        {
            return Err(EvaError::unavailable("schedule lease is held").with_retryable(true));
        }
        r.lease_owner = Some(owner.to_owned());
        r.lease_expires_at_ms = Some(now_ms.saturating_add(lease_ms.max(1)));
        r.generation = r.generation.saturating_add(1);
        self.write(&r)?;
        Ok(r)
    }
    pub fn complete(
        &self,
        id: &str,
        owner: &str,
        next_run_at_ms: u128,
    ) -> Result<ScheduleRecord, EvaError> {
        let _lock = self.lock(id)?;
        let mut r = self.read(id)?;
        if r.lease_owner.as_deref() != Some(owner) {
            return Err(EvaError::conflict(
                "schedule completion owner does not match lease",
            ));
        }
        r.next_run_at_ms = next_run_at_ms;
        r.lease_owner = None;
        r.lease_expires_at_ms = None;
        r.generation = r.generation.saturating_add(1);
        self.write(&r)?;
        Ok(r)
    }
    pub fn read(&self, id: &str) -> Result<ScheduleRecord, EvaError> {
        validate_id(id)?;
        decode(&fs::read_to_string(self.path(id)).map_err(|e| {
            EvaError::not_found("schedule record not found").with_context("error", e.to_string())
        })?)
    }
    fn path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.schedule"))
    }
    fn lock(&self, id: &str) -> Result<ScheduleLock, EvaError> {
        let path = self.root.join(format!("{id}.schedule.lock"));
        for _ in 0..200 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(ScheduleLock { file, path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > Duration::from_secs(60))
                    {
                        let _ = fs::remove_file(&path);
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                Err(error) => {
                    return Err(EvaError::internal("acquire schedule lock")
                        .with_context("error", error.to_string()))
                }
            }
        }
        Err(EvaError::timeout("schedule lock acquisition timed out"))
    }
    fn write(&self, r: &ScheduleRecord) -> Result<(), EvaError> {
        let p = self.path(&r.id);
        let t = p.with_extension("schedule.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&t)
            .map_err(|e| {
                EvaError::internal("write schedule record").with_context("error", e.to_string())
            })?;
        f.write_all(encode(r).as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| {
                EvaError::internal("write schedule record").with_context("error", e.to_string())
            })?;
        fs::rename(t, p).map_err(|e| {
            EvaError::internal("publish schedule record").with_context("error", e.to_string())
        })
    }
}
#[derive(Debug)]
struct ScheduleLock {
    file: std::fs::File,
    path: PathBuf,
}
impl Drop for ScheduleLock {
    fn drop(&mut self) {
        let _ = self.file.sync_all();
        let _ = fs::remove_file(&self.path);
    }
}
fn validate_id(id: &str) -> Result<(), EvaError> {
    if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." {
        Err(EvaError::invalid_argument("schedule id is invalid"))
    } else {
        Ok(())
    }
}
fn encode(r: &ScheduleRecord) -> String {
    format!("version=1\nid={}\nnext_run_at_ms={}\nlease_owner={}\nlease_expires_at_ms={}\ngeneration={}\n",r.id,r.next_run_at_ms,r.lease_owner.as_deref().unwrap_or(""),r.lease_expires_at_ms.map(|v|v.to_string()).unwrap_or_default(),r.generation)
}
fn decode(t: &str) -> Result<ScheduleRecord, EvaError> {
    let mut id = None;
    let mut next = None;
    let mut owner = None;
    let mut exp = None;
    let mut gen = None;
    for l in t.lines() {
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        match k {
            "id" => id = Some(v.to_owned()),
            "next_run_at_ms" => {
                next = Some(
                    v.parse()
                        .map_err(|_| EvaError::invalid_argument("invalid schedule time"))?,
                )
            }
            "lease_owner" if !v.is_empty() => owner = Some(v.to_owned()),
            "lease_expires_at_ms" if !v.is_empty() => {
                exp = Some(
                    v.parse()
                        .map_err(|_| EvaError::invalid_argument("invalid schedule lease"))?,
                )
            }
            "generation" => {
                gen = Some(
                    v.parse()
                        .map_err(|_| EvaError::invalid_argument("invalid schedule generation"))?,
                )
            }
            _ => {}
        }
    }
    Ok(ScheduleRecord {
        id: id.ok_or_else(|| EvaError::invalid_argument("schedule id missing"))?,
        next_run_at_ms: next.ok_or_else(|| EvaError::invalid_argument("schedule time missing"))?,
        lease_owner: owner,
        lease_expires_at_ms: exp,
        generation: gen.ok_or_else(|| EvaError::invalid_argument("schedule generation missing"))?,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    #[test]
    fn claim_reclaims_expired_lease() {
        let root = std::env::temp_dir().join(format!("eva-schedule-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let s = FileSystemScheduleStore::new(&root).unwrap();
        s.upsert("memory-gc", 10).unwrap();
        assert!(s.claim("memory-gc", "a", 10, 10).is_ok());
        assert!(s.claim("memory-gc", "b", 15, 10).is_err());
        assert!(s.claim("memory-gc", "b", 21, 10).is_ok());
        s.complete("memory-gc", "b", 100).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_claim_has_one_owner() {
        let root = std::env::temp_dir().join(format!("eva-schedule-race-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = Arc::new(FileSystemScheduleStore::new(&root).unwrap());
        store.upsert("knowledge-rebuild", 1).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut joins = Vec::new();
        for owner in ["first", "second"] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            joins.push(thread::spawn(move || {
                barrier.wait();
                store.claim("knowledge-rebuild", owner, 1, 1_000).is_ok()
            }));
        }
        barrier.wait();
        let winners = joins
            .into_iter()
            .map(|join| join.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        let _ = fs::remove_dir_all(root);
    }
}
