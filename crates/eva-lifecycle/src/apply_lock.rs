//! 升级应用锁的获取边界。
//! Upgrade apply lock acquisition boundary.

use eva_core::{EvaError, GenerationId};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：建立升级应用命令的互斥锁模型。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "upgrade apply command lock model";

/// 从当前代际升级到目标代际的不可变计划描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeApplyPlan {
    /// 用于锁文件和审计关联的稳定计划标识。
    pub plan_id: String,
    /// 当前运行时代际。
    pub from_generation: GenerationId,
    /// 升级目标代际，必须与当前代际不同。
    pub to_generation: GenerationId,
    /// 当前发布引用。
    pub from_release: String,
    /// 目标发布引用。
    pub to_release: String,
}

/// 已获取的升级应用互斥锁及其审计证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeApplyLock {
    /// 由计划标识派生的锁标识。
    pub lock_id: String,
    /// 被锁定的升级计划标识。
    pub plan_id: String,
    /// 获取锁的稳定所有者标识。
    pub owner: String,
    /// 锁定时记录的当前代际。
    pub from_generation: GenerationId,
    /// 锁定时记录的目标代际。
    pub to_generation: GenerationId,
    /// 当前锁状态。
    pub status: String,
    /// 锁获取过程的有序审计事件。
    pub audit: Vec<String>,
}

/// 获取升级锁后的门禁报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeApplyReport {
    /// 本次升级计划标识。
    pub plan_id: String,
    /// 锁门禁状态。
    pub status: String,
    /// 是否已允许执行破坏性应用；仅获取锁时固定为 `false`。
    pub apply_allowed: bool,
    /// 已获取锁的完整证据。
    pub lock: UpgradeApplyLock,
    /// 后续必须执行的步骤说明。
    pub steps: Vec<String>,
    /// 当前阶段仍存在的风险与限制。
    pub risks: Vec<String>,
    /// 本次门禁操作的有序审计事件。
    pub audit: Vec<String>,
}

/// 升级应用锁存储的最小接口。
pub trait UpgradeApplyLockStore {
    /// 以原子冲突检测语义为计划获取锁。
    fn acquire_lock(
        &mut self,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyLock, EvaError>;
}

/// 供单进程协调和测试使用的内存锁存储。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryUpgradeApplyLockStore {
    /// 按计划标识保存且不会自动释放的锁。
    locks: BTreeMap<String, UpgradeApplyLock>,
}

/// 通过排他创建锁文件实现跨进程互斥的存储。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemUpgradeApplyLockStore {
    /// 锁文件所在根目录。
    root: PathBuf,
}

/// 将锁存储结果转换为升级门禁报告的无状态协调器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpgradeApplyCoordinator;

impl UpgradeApplyPlan {
    /// 校验计划标识、代际差异和发布引用后创建升级计划。
    pub fn new(
        plan_id: impl Into<String>,
        from_generation: GenerationId,
        to_generation: GenerationId,
        from_release: impl Into<String>,
        to_release: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let plan_id = validate_token("plan_id", plan_id.into())?;
        if from_generation == to_generation {
            return Err(EvaError::invalid_argument(
                "upgrade apply plan must target a different generation",
            )
            .with_context("plan_id", &plan_id)
            .with_context("generation", from_generation.as_str()));
        }
        Ok(Self {
            plan_id,
            from_generation,
            to_generation,
            from_release: validate_release_ref("from_release", from_release.into())?,
            to_release: validate_release_ref("to_release", to_release.into())?,
        })
    }

    /// 从计划标识派生稳定锁标识。
    pub fn lock_id(&self) -> String {
        format!("upgrade-apply-{}", self.plan_id)
    }
}

impl InMemoryUpgradeApplyLockStore {
    /// 创建空的内存锁存储。
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystemUpgradeApplyLockStore {
    /// 创建以指定目录为持久化边界的文件锁存储。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 返回锁文件根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl UpgradeApplyLockStore for InMemoryUpgradeApplyLockStore {
    /// 若计划尚未持锁则记录锁，否则返回冲突。
    fn acquire_lock(
        &mut self,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyLock, EvaError> {
        if self.locks.contains_key(&plan.plan_id) {
            return Err(lock_conflict(&plan.plan_id, None));
        }
        let lock = build_lock(plan, owner)?;
        self.locks.insert(plan.plan_id.clone(), lock.clone());
        Ok(lock)
    }
}

impl UpgradeApplyLockStore for FileSystemUpgradeApplyLockStore {
    /// 通过 `create_new` 排他创建锁文件，保证并发进程只有一个成功。
    ///
    /// 文件存在会映射为明确冲突；其他 I/O 错误保留路径上下文。锁文件创建成功后
    /// 写入审计载荷，写入失败会返回错误，但已创建文件仍保留，采用保守的失败关闭
    /// 语义，避免另一个进程在状态不明时重新进入升级流程。
    fn acquire_lock(
        &mut self,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyLock, EvaError> {
        let lock = build_lock(plan, owner)?;
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create upgrade apply lock store")
                .with_context("lock_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let lock_path = lock_path(&self.root, &plan.plan_id);
        // 排他创建是跨进程竞争的线性化点，不能先检查存在性再普通创建。
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    lock_conflict(&plan.plan_id, Some(&lock_path))
                } else {
                    EvaError::internal("failed to create upgrade apply lock")
                        .with_context("plan_id", &plan.plan_id)
                        .with_context("lock_path", lock_path.display().to_string())
                        .with_context("io_error", error.to_string())
                }
            })?;
        file.write_all(lock_payload(plan, &lock).as_bytes())
            .map_err(|error| {
                EvaError::internal("failed to write upgrade apply lock")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("lock_path", lock_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(lock)
    }
}

impl UpgradeApplyCoordinator {
    /// 获取互斥锁并返回仍禁止实际应用的阶段性报告。
    ///
    /// 锁只解决并发升级竞争；备份证据、策略审批和交接门禁尚未完成，因此
    /// `apply_allowed` 必须保持为 `false`。
    pub fn acquire_lock<S: UpgradeApplyLockStore>(
        &self,
        store: &mut S,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyReport, EvaError> {
        let lock = store.acquire_lock(plan, owner)?;
        Ok(UpgradeApplyReport {
            plan_id: plan.plan_id.clone(),
            status: "locked".to_owned(),
            apply_allowed: false,
            lock,
            steps: vec![
                "acquire upgrade apply lock".to_owned(),
                "verify backup evidence before destructive apply".to_owned(),
                "keep runtime mutation disabled until apply gates are complete".to_owned(),
            ],
            risks: vec![
                "upgrade apply is locked only; no runtime process is started".to_owned(),
                "destructive generation handoff still requires backup evidence and policy approval"
                    .to_owned(),
            ],
            audit: vec![
                "upgrade.apply:plan_parsed".to_owned(),
                "upgrade.apply:lock_acquired".to_owned(),
                "apply_allowed:false".to_owned(),
            ],
        })
    }
}

/// 校验锁所有者并构造与计划绑定的锁证据。
fn build_lock(plan: &UpgradeApplyPlan, owner: &str) -> Result<UpgradeApplyLock, EvaError> {
    let owner = validate_token("owner", owner.to_owned())?;
    Ok(UpgradeApplyLock {
        lock_id: plan.lock_id(),
        plan_id: plan.plan_id.clone(),
        owner,
        from_generation: plan.from_generation.clone(),
        to_generation: plan.to_generation.clone(),
        status: "acquired".to_owned(),
        audit: vec![
            "lock:acquired".to_owned(),
            format!("plan:{}", plan.plan_id),
            format!("from:{}", plan.from_generation.as_str()),
            format!("to:{}", plan.to_generation.as_str()),
        ],
    })
}

/// 校验将参与文件名或审计标识的稳定短标记。
///
/// 禁止空白、路径遍历片段和非 ASCII slug 字符，防止计划标识逃逸锁存储目录或
/// 生成含混的跨平台文件名。
fn validate_token(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(EvaError::invalid_argument(
            "upgrade apply token must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    if value.contains("..")
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(
            EvaError::invalid_argument("upgrade apply token must be a stable slug")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验发布引用为非空、已裁剪的单行文本。
fn validate_release_ref(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.contains('\n')
        || value.contains('\r')
    {
        return Err(EvaError::invalid_argument(
            "upgrade apply release ref must be non-empty and single-line",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    Ok(value)
}

/// 在受控根目录下生成计划锁文件路径。
fn lock_path(root: &Path, plan_id: &str) -> PathBuf {
    root.join(format!("{plan_id}.lock"))
}

/// 将计划与锁的关键字段序列化为可审计的行式载荷。
fn lock_payload(plan: &UpgradeApplyPlan, lock: &UpgradeApplyLock) -> String {
    format!(
        "lock_id={}\nplan_id={}\nowner={}\nfrom_generation={}\nto_generation={}\nfrom_release={}\nto_release={}\nstatus={}\n",
        lock.lock_id,
        plan.plan_id,
        lock.owner,
        plan.from_generation.as_str(),
        plan.to_generation.as_str(),
        plan.from_release,
        plan.to_release,
        lock.status
    )
}

/// 构造锁已存在时的冲突错误，并在可用时附带持久化路径。
fn lock_conflict(plan_id: &str, lock_path: Option<&Path>) -> EvaError {
    let mut error =
        EvaError::conflict("upgrade apply lock already exists").with_context("plan_id", plan_id);
    if let Some(lock_path) = lock_path {
        error = error.with_context("lock_path", lock_path.display().to_string());
    }
    error
}

#[cfg(test)]
/// 升级应用锁的门禁、冲突与跨实例持久化测试。
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 构造有效的固定升级计划。
    fn plan() -> UpgradeApplyPlan {
        UpgradeApplyPlan::new(
            "plan-1",
            GenerationId::parse("gen-v14").unwrap(),
            GenerationId::parse("gen-v15").unwrap(),
            "1.4.0",
            "1.5.1",
        )
        .unwrap()
    }

    /// 创建进程与时间戳隔离的临时测试目录。
    fn temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("eva-lifecycle-{name}-{}-{now}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    /// 验证获取锁本身不会放开实际应用门禁。
    fn upgrade_apply_acquires_lock_without_allowing_apply() {
        let mut store = InMemoryUpgradeApplyLockStore::new();
        let report = UpgradeApplyCoordinator
            .acquire_lock(&mut store, &plan(), "cli")
            .unwrap();

        assert_eq!(report.status, "locked");
        assert_eq!(report.lock.status, "acquired");
        assert!(!report.apply_allowed);
    }

    #[test]
    /// 验证同一计划不能在内存存储中重复持锁。
    fn upgrade_apply_rejects_conflicting_lock() {
        let mut store = InMemoryUpgradeApplyLockStore::new();
        let plan = plan();

        UpgradeApplyCoordinator
            .acquire_lock(&mut store, &plan, "cli")
            .unwrap();
        let error = UpgradeApplyCoordinator
            .acquire_lock(&mut store, &plan, "cli")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证锁文件使不同存储实例也能观察到冲突。
    fn filesystem_lock_persists_conflict() {
        let root = temp_dir("upgrade-lock");
        let plan = plan();
        let mut first = FileSystemUpgradeApplyLockStore::new(&root);
        let mut second = FileSystemUpgradeApplyLockStore::new(&root);

        UpgradeApplyCoordinator
            .acquire_lock(&mut first, &plan, "cli")
            .unwrap();
        let error = UpgradeApplyCoordinator
            .acquire_lock(&mut second, &plan, "cli")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        fs::remove_dir_all(root).unwrap();
    }
}
