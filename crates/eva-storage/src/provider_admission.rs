//! Durable, cross-process provider admission reservations.
//! 中文：基于文件系统的跨进程 Provider 准入租约，用于统一限制并发会话数量。

use eva_core::{AdapterId, EvaError};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// 中文：未显式指定时使用的租约有效期，避免进程异常退出后永久占用并发名额。
pub const DEFAULT_RESERVATION_TTL_MS: u128 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq)]
/// 中文：一次 Provider 并发名额的有时限占用凭证。
pub struct ProviderAdmissionReservation {
    /// 中文：唯一租约标识，续租和所有权校验必须同时匹配该值。
    pub reservation_id: String,
    /// 中文：持有租约的运行时会话标识。
    pub session_id: String,
    /// 中文：租约失效的绝对毫秒时间戳。
    pub expires_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 中文：指定 Adapter 当前并发上限及全部有效租约的持久化快照。
pub struct ProviderAdmissionSnapshot {
    /// 中文：快照归属的 Adapter 标识。
    pub adapter_id: AdapterId,
    /// 中文：该 Provider 当前允许的最大并发会话数。
    pub max_concurrency: usize,
    /// 中文：尚未过期且仍占用并发名额的租约集合。
    pub reservations: Vec<ProviderAdmissionReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 中文：通过独占锁文件协调多个进程的 Provider 准入表。
pub struct FileSystemProviderAdmissionTable {
    /// 中文：准入快照和锁文件的存储目录。
    root: PathBuf,
    /// 中文：发现锁已被占用时两次重试之间的等待时间。
    lock_wait: Duration,
}

impl FileSystemProviderAdmissionTable {
    /// 中文：创建准入表并确保其持久化目录存在。
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

    /// 中文：原子清理过期租约并为会话申请名额；同一会话重复申请会返回原租约。
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
        // 中文：读、判断和写必须处于同一跨进程锁内，否则两个进程可能同时越过并发上限。
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

    /// 中文：按会话释放其全部租约；会话不存在时保持幂等成功。
    pub fn release(&self, adapter_id: &AdapterId, session_id: &str) -> Result<(), EvaError> {
        let _lock = self.lock(adapter_id)?;
        let mut state = self.read(adapter_id)?;
        state.reservations.retain(|r| r.session_id != session_id);
        self.write(adapter_id, &state)
    }

    /// 中文：仅当租约标识和会话标识同时匹配时释放名额，防止其他会话误删租约。
    pub fn release_owned(
        &self,
        adapter_id: &AdapterId,
        reservation_id: &str,
        session_id: &str,
    ) -> Result<(), EvaError> {
        let _lock = self.lock(adapter_id)?;
        let mut state = self.read(adapter_id)?;
        let index = state
            .reservations
            .iter()
            .position(|entry| {
                entry.reservation_id == reservation_id && entry.session_id == session_id
            })
            .ok_or_else(|| EvaError::conflict("provider admission reservation is not owned"))?;
        state.reservations.remove(index);
        self.write(adapter_id, &state)
    }

    /// 中文：在租约尚未过期且所有权匹配时延长有效期。
    pub fn renew(
        &self,
        adapter_id: &AdapterId,
        reservation_id: &str,
        session_id: &str,
        now_ms: u128,
        ttl_ms: u128,
    ) -> Result<ProviderAdmissionReservation, EvaError> {
        let _lock = self.lock(adapter_id)?;
        let mut state = self.read(adapter_id)?;
        let reservation = state
            .reservations
            .iter_mut()
            .find(|entry| entry.reservation_id == reservation_id && entry.session_id == session_id)
            .ok_or_else(|| EvaError::conflict("provider admission reservation is not owned"))?;
        if reservation.expires_at_ms <= now_ms {
            return Err(EvaError::conflict(
                "provider admission reservation has expired",
            ));
        }
        reservation.expires_at_ms = now_ms.saturating_add(ttl_ms.max(1));
        let renewed = reservation.clone();
        self.write(adapter_id, &state)?;
        Ok(renewed)
    }

    /// 中文：清理过期项并返回指定 Adapter 的最新持久化准入快照。
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

    /// 中文：以 Adapter 标识摘要生成固定长度且不泄露原值的快照文件路径。
    fn path(&self, adapter_id: &AdapterId) -> PathBuf {
        self.root
            .join(format!("{}.admission", digest(adapter_id.as_str())))
    }
    /// 中文：返回与指定 Adapter 准入状态一一对应的锁文件路径。
    fn lock_path(&self, adapter_id: &AdapterId) -> PathBuf {
        self.root
            .join(format!("{}.lock", digest(adapter_id.as_str())))
    }
    /// 中文：通过原子创建锁文件获取跨进程互斥权，超过有限重试次数后返回超时。
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
    /// 中文：读取并解码准入快照；文件尚不存在时返回空状态。
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
    /// 中文：先同步临时文件再原子替换正式快照，避免读到部分写入的数据。
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
/// 中文：独占锁文件的 RAII 守卫，离开临界区时同步并删除锁文件。
struct AdmissionLock {
    /// 中文：保持打开状态的锁文件句柄。
    file: File,
    /// 中文：守卫释放时需要删除的锁文件路径。
    path: PathBuf,
}
impl Drop for AdmissionLock {
    /// 中文：尽力同步锁文件并释放跨进程互斥权；析构路径不传播清理错误。
    fn drop(&mut self) {
        let _ = self.file.sync_all();
        let _ = fs::remove_file(&self.path);
    }
}

/// 中文：计算 Adapter 标识的 SHA-256 十六进制摘要，用作安全文件名。
fn digest(value: &str) -> String {
    let mut h = Sha256::new();
    h.update(value.as_bytes());
    format!("{:x}", h.finalize())
}
/// 中文：把准入快照编码为带版本头的稳定逐行格式。
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
/// 中文：解析逐行准入格式，并拒绝字段数量或数值类型不合法的租约。
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

    /// 中文：为每个测试创建互不冲突的临时准入目录。
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "eva-admission-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// 中文：验证并发容量、显式释放和过期清理都会持久生效。
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

    /// 中文：验证同一会话重复申请时复用原租约而不额外占用名额。
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

    /// 中文：验证续租同时受租约标识、会话所有权和过期时间约束。
    #[test]
    fn renew_is_fenced_by_reservation_and_session_identity() {
        let root = root();
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let adapter = AdapterId::parse("provider-admission-renew").unwrap();
        let reservation = table.reserve(&adapter, 1, "owner", 10, 100).unwrap();
        let renewed = table
            .renew(&adapter, &reservation.reservation_id, "owner", 50, 200)
            .unwrap();
        assert_eq!(renewed.expires_at_ms, 250);
        assert!(table
            .renew(&adapter, &reservation.reservation_id, "other", 60, 200)
            .is_err());
        assert!(table
            .renew(&adapter, &reservation.reservation_id, "owner", 250, 200)
            .is_err());
        let _ = fs::remove_dir_all(root);
    }

    /// 中文：验证旧租约持有者无法释放同一会话后来创建的继任租约。
    #[test]
    fn release_owned_cannot_remove_a_successor_reservation() {
        let root = root();
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let adapter = AdapterId::parse("provider-admission-release-fence").unwrap();
        let first = table.reserve(&adapter, 1, "owner", 10, 10).unwrap();
        let successor = table.reserve(&adapter, 1, "owner", 20, 100).unwrap();
        assert_ne!(first.reservation_id, successor.reservation_id);
        assert!(table
            .release_owned(&adapter, &first.reservation_id, "owner")
            .is_err());
        assert_eq!(
            table.snapshot(&adapter, 21).unwrap().reservations,
            vec![successor.clone()]
        );
        table
            .release_owned(&adapter, &successor.reservation_id, "owner")
            .unwrap();
        assert!(table
            .snapshot(&adapter, 21)
            .unwrap()
            .reservations
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// 中文：验证两个独立进程竞争唯一名额时只有一个申请成功。
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

    #[test]
    fn crashed_process_reservation_is_reclaimed_only_after_expiry() {
        if let Ok(root) = std::env::var("EVA_ADMISSION_CRASH_CHILD_ROOT") {
            let table = FileSystemProviderAdmissionTable::new(root).unwrap();
            let adapter = AdapterId::parse("provider-admission-crash").unwrap();
            table
                .reserve(&adapter, 1, "crashed-owner", 10, 100)
                .unwrap();
            std::process::exit(0);
        }

        let root = root();
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("provider_admission::tests::crashed_process_reservation_is_reclaimed_only_after_expiry")
            .arg("--exact")
            .arg("--nocapture")
            .env("EVA_ADMISSION_CRASH_CHILD_ROOT", &root)
            .status()
            .unwrap();
        assert!(status.success());

        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let adapter = AdapterId::parse("provider-admission-crash").unwrap();
        let crashed = table.snapshot(&adapter, 50).unwrap().reservations[0].clone();
        assert!(table.reserve(&adapter, 1, "successor", 109, 100).is_err());
        let successor = table.reserve(&adapter, 1, "successor", 110, 100).unwrap();
        assert!(table
            .release_owned(&adapter, &crashed.reservation_id, "crashed-owner")
            .is_err());
        assert_eq!(
            table.snapshot(&adapter, 111).unwrap().reservations,
            vec![successor]
        );
        let _ = fs::remove_dir_all(root);
    }
}
