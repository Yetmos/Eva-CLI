use eva_release::{EvidenceEnvelope, EvidenceKind};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PARENT_TIMEOUT: Duration = Duration::from_secs(20);
const PIPE_EOF_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_HARNESS_LEASE_TTL_MS: &str = "8000";
const REQUIRED_RUNTIME_SCENARIOS: [&str; 5] = [
    "background_owner",
    "forced_kill",
    "stale_lock_reclaim",
    "restart_recovery",
    "effect_dedup",
];

struct RuntimeScenarioEvidence {
    id: &'static str,
    facts: BTreeMap<&'static str, String>,
}

impl RuntimeScenarioEvidence {
    fn new(id: &'static str) -> Self {
        assert!(REQUIRED_RUNTIME_SCENARIOS.contains(&id));
        Self {
            id,
            facts: BTreeMap::new(),
        }
    }

    fn with_fact(mut self, name: &'static str, value: impl ToString) -> Self {
        assert!(self.facts.insert(name, value.to_string()).is_none());
        self
    }
}

struct DaemonFixture {
    root: PathBuf,
    project: PathBuf,
}

struct CapturedParent {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl DaemonFixture {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!(
            "eva-background-{name}-{}-{timestamp}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            project: Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf(),
        }
    }

    fn durable(&self) -> PathBuf {
        self.root.join("durable")
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn locks(&self) -> PathBuf {
        self.root.join("locks")
    }

    fn pids(&self) -> PathBuf {
        self.root.join("pids")
    }

    fn observability(&self) -> PathBuf {
        self.root.join("observability")
    }

    fn process_harness(&self) -> PathBuf {
        self.root.join("process-harness")
    }

    fn start_args(&self, startup_timeout_ms: u64) -> Vec<OsString> {
        let mut args = vec![
            "daemon".into(),
            "start".into(),
            "--background".into(),
            "--dev".into(),
            "--startup-timeout-ms".into(),
            startup_timeout_ms.to_string().into(),
        ];
        args.extend(self.path_args());
        args
    }

    fn control_args(&self, operation: &str) -> Vec<OsString> {
        let mut args = vec!["daemon".into(), operation.into()];
        args.extend(self.path_args());
        args.extend(["--control-timeout-ms".into(), "2000".into()]);
        args
    }

    fn path_args(&self) -> Vec<OsString> {
        vec![
            "--durable-backend".into(),
            self.durable().into_os_string(),
            "--state-dir".into(),
            self.state().into_os_string(),
            "--lock-dir".into(),
            self.locks().into_os_string(),
            "--pid-dir".into(),
            self.pids().into_os_string(),
            "--observability-backend".into(),
            self.observability().into_os_string(),
            "--project".into(),
            self.project.clone().into_os_string(),
            "--output".into(),
            "json".into(),
        ]
    }

    fn spawn_start(&self, startup_timeout_ms: u64, envs: &[(&str, &str)]) -> Child {
        let mut command = Command::new(env!("CARGO_BIN_EXE_eva"));
        command
            .args(self.start_args(startup_timeout_ms))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in envs {
            command.env(name, value);
        }
        command.spawn().unwrap()
    }

    fn capture_start(&self, startup_timeout_ms: u64, envs: &[(&str, &str)]) -> CapturedParent {
        let child = self.spawn_start(startup_timeout_ms, envs);
        self.capture_parent(child)
    }

    fn capture_parent(&self, mut child: Child) -> CapturedParent {
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (stdout_tx, stdout_rx) = mpsc::channel();
        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
            let _ = stdout_tx.send(result);
        });
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr.read_to_end(&mut bytes).map(|_| bytes);
            let _ = stderr_tx.send(result);
        });

        let deadline = Instant::now() + PARENT_TIMEOUT;
        let status = loop {
            match child.try_wait().unwrap() {
                Some(status) => break status,
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.try_shutdown();
                    panic!("background launcher did not exit within {PARENT_TIMEOUT:?}");
                }
            }
        };

        let stdout = stdout_rx.recv_timeout(PIPE_EOF_TIMEOUT);
        let stderr = stderr_rx.recv_timeout(PIPE_EOF_TIMEOUT);
        if stdout.is_err() || stderr.is_err() {
            self.try_shutdown();
        }
        let stdout = stdout
            .expect("background child kept launcher stdout open")
            .unwrap();
        let stderr = stderr
            .expect("background child kept launcher stderr open")
            .unwrap();
        CapturedParent {
            status,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
        }
    }

    fn run_control(&self, operation: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_eva"))
            .args(self.control_args(operation))
            .output()
            .unwrap()
    }

    fn run_control_with(&self, operation: &str, extra: &[&str]) -> std::process::Output {
        let mut args = self.control_args(operation);
        args.extend(extra.iter().map(OsString::from));
        Command::new(env!("CARGO_BIN_EXE_eva"))
            .args(args)
            .output()
            .unwrap()
    }

    fn task_status(&self, task_id: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_eva"))
            .args([
                OsString::from("task"),
                OsString::from("status"),
                OsString::from("--task"),
                OsString::from(task_id),
                OsString::from("--durable-backend"),
                self.durable().into_os_string(),
                OsString::from("--project"),
                self.project.clone().into_os_string(),
                OsString::from("--output"),
                OsString::from("json"),
            ])
            .output()
            .unwrap()
    }

    fn wait_for_task_status(&self, task_id: &str, expected: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let output = self.task_status(task_id);
            let stdout = text(&output.stdout);
            if output.status.success() && stdout.contains(&format!("\"status\":\"{expected}\"")) {
                return stdout;
            }
            assert!(
                Instant::now() < deadline,
                "task {task_id} did not reach {expected}; stdout={stdout} stderr={}",
                text(&output.stderr)
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn try_shutdown(&self) {
        let _ = self.run_control("shutdown");
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.pids().join("daemon.pid").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        self.try_shutdown();
        for _ in 0..20 {
            match fs::remove_dir_all(&self.root) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => thread::sleep(Duration::from_millis(25)),
            }
        }
    }
}

#[test]
fn background_parent_returns_and_child_remains_controllable_with_bound_identity() {
    let fixture = DaemonFixture::new("success");
    let start = fixture.capture_start(10_000, &[]);
    assert!(start.status.success(), "{}", start.stderr);
    assert!(start.stderr.is_empty(), "{}", start.stderr);
    assert!(start.stdout.contains("\"command\":\"daemon.start\""));
    assert!(start.stdout.contains("\"mode\":\"background\""));
    assert!(start.stdout.contains("\"foreground\":false"));
    assert!(start.stdout.contains("\"durable_backend\":"));
    assert!(start.stdout.contains("\"recovery\":"));
    assert!(start.stdout.contains("\"memory_maintenance\":"));
    assert!(start.stdout.contains("\"spawn\":"));

    let child_pid = json_u64(&start.stdout, "child_pid") as u32;
    let launcher_pid = json_u64(&start.stdout, "launcher_pid") as u32;
    let nonce = json_string_value(&start.stdout, "nonce");
    assert_ne!(child_pid, launcher_pid);
    let ready = parse_fields(
        &fs::read_to_string(
            fixture
                .state()
                .join("startup")
                .join(format!("{nonce}.ready")),
        )
        .unwrap(),
    );
    let lease = parse_fields(&fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap());
    let pid = parse_fields(&fs::read_to_string(fixture.pids().join("daemon.pid")).unwrap());
    assert_eq!(ready.get("phase").map(String::as_str), Some("ready"));
    assert_eq!(ready.get("child_pid").unwrap(), &child_pid.to_string());
    assert_eq!(ready.get("child_pid"), lease.get("pid"));
    assert_eq!(ready.get("child_pid"), pid.get("pid"));
    assert_eq!(
        ready.get("process_start_token"),
        lease.get("process_start_token")
    );
    assert_eq!(
        ready.get("process_start_token"),
        pid.get("process_start_token")
    );
    assert_eq!(ready.get("generation"), lease.get("generation"));
    assert_eq!(ready.get("generation"), pid.get("generation"));
    assert_eq!(lease.get("state").map(String::as_str), Some("active"));
    assert!(start.stdout.contains(ready.get("report_digest").unwrap()));

    let status = fixture.run_control("status");
    assert!(status.status.success(), "{}", text(&status.stderr));
    let status_stdout = text(&status.stdout);
    assert!(status_stdout.contains("\"daemon_available\":true"));
    assert!(status_stdout.contains(&format!("\"pid\":{child_pid}")));

    let shutdown = fixture.run_control("shutdown");
    assert!(shutdown.status.success(), "{}", text(&shutdown.stderr));
    wait_until(Duration::from_secs(5), || {
        !fixture.pids().join("daemon.pid").exists()
            && fs::read_to_string(fixture.locks().join("daemon.lease"))
                .is_ok_and(|lease| lease.contains("state=released"))
    });
    assert!(fixture.locks().join("daemon.lock").exists());
}

#[test]
fn timeout_reclaims_killed_child_and_fixed_anchor_is_reusable() {
    let fixture = DaemonFixture::new("timeout");
    let failed = fixture.capture_start(100, &[("EVA_DAEMON_TEST_REPORT_DELAY_MS", "5000")]);
    assert!(!failed.status.success(), "{}", failed.stdout);
    assert!(failed.stdout.is_empty());
    assert!(
        failed.stderr.contains("\"kind\":\"timeout\""),
        "{}",
        failed.stderr
    );
    assert!(
        failed.stderr.contains("cleanup_complete"),
        "{}",
        failed.stderr
    );
    assert!(fixture.locks().join("daemon.lock").exists());
    assert!(!fixture.pids().join("daemon.pid").exists());
    let lease = fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap();
    assert!(lease.contains("state=released"), "{lease}");
    match fs::read_to_string(fixture.state().join("daemon.state")) {
        Ok(state) => assert!(state.contains("status=stopped"), "{state}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("failed to inspect daemon state after timeout cleanup: {error}"),
    }
    let failed_frame = only_startup_file(&fixture.state(), "failed");
    assert!(fs::read_to_string(failed_frame)
        .unwrap()
        .contains("cleanup_complete=true"));

    let restarted = fixture.capture_start(10_000, &[]);
    assert!(restarted.status.success(), "{}", restarted.stderr);
    assert!(fixture.run_control("status").status.success());
    assert!(fixture.run_control("shutdown").status.success());
}

#[test]
fn kill_between_lease_and_claimed_frame_reclaims_probe_bound_identity() {
    let fixture = DaemonFixture::new("lease-claim-gap");
    let failed = fixture.capture_start(100, &[("EVA_DAEMON_TEST_LEASE_CLAIM_DELAY_MS", "5000")]);
    assert!(!failed.status.success(), "{}", failed.stdout);
    assert!(failed.stdout.is_empty());
    assert!(
        failed.stderr.contains("\"kind\":\"timeout\""),
        "{}",
        failed.stderr
    );
    assert!(failed.stderr.contains("lease_probe"), "{}", failed.stderr);
    assert!(fixture.locks().join("daemon.lock").exists());
    assert!(!fixture.pids().join("daemon.pid").exists());
    assert!(fs::read_to_string(fixture.locks().join("daemon.lease"))
        .unwrap()
        .contains("state=released"));
    let startup = fixture.state().join("startup");
    assert!(!fs::read_dir(&startup)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("claimed")));
    assert!(
        fs::read_to_string(only_startup_file(&fixture.state(), "failed"))
            .unwrap()
            .contains("cleanup_complete=true")
    );
}

#[test]
fn startup_stage_failure_releases_lease_without_pid_residue() {
    let fixture = DaemonFixture::new("stage-failure");
    fs::create_dir_all(fixture.state()).unwrap();
    fs::write(fixture.state().join("control"), b"blocks control directory").unwrap();

    let failed = fixture.capture_start(10_000, &[]);
    assert!(!failed.status.success(), "{}", failed.stdout);
    assert!(failed.stdout.is_empty());
    assert!(failed.stderr.contains("\"command\":\"daemon.start\""));
    assert!(fixture.locks().join("daemon.lock").exists());
    assert!(!fixture.pids().join("daemon.pid").exists());
    let lease = fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap();
    assert!(lease.contains("state=released"), "{lease}");
    let failed_frame = only_startup_file(&fixture.state(), "failed");
    assert!(fs::read_to_string(failed_frame)
        .unwrap()
        .contains("cleanup_complete=true"));
}

#[test]
fn concurrent_launchers_publish_exactly_one_background_owner() {
    let fixture = DaemonFixture::new("concurrent");
    let first = fixture.spawn_start(10_000, &[]);
    let second = fixture.spawn_start(10_000, &[]);
    let first = fixture.capture_parent(first);
    let second = fixture.capture_parent(second);
    let successes = [first.status.success(), second.status.success()]
        .into_iter()
        .filter(|success| *success)
        .count();
    assert_eq!(
        successes, 1,
        "first={} second={}",
        first.stderr, second.stderr
    );
    let loser = if first.status.success() {
        &second
    } else {
        &first
    };
    assert!(loser.stdout.is_empty());
    assert!(
        loser.stderr.contains("\"kind\":\"conflict\""),
        "{}",
        loser.stderr
    );
    assert!(fixture.run_control("status").status.success());
    assert!(fixture.run_control("shutdown").status.success());
}

#[test]
fn exited_child_failed_frame_wins_over_generic_early_exit() {
    let fixture = DaemonFixture::new("failed-frame-after-poll");
    let owner = fixture.capture_start(10_000, &[]);
    assert!(owner.status.success(), "{}", owner.stderr);

    let failed = fixture.capture_start(
        10_000,
        &[
            ("EVA_DAEMON_TEST_BACKGROUND_CHILD_START_DELAY_MS", "100"),
            ("EVA_DAEMON_TEST_TERMINAL_POLL_DELAY_MS", "2000"),
        ],
    );
    assert!(!failed.status.success(), "{}", failed.stdout);
    assert!(failed.stdout.is_empty());
    assert!(
        failed.stderr.contains("\"kind\":\"conflict\""),
        "{}",
        failed.stderr
    );
    assert!(
        failed
            .stderr
            .contains("background daemon reported startup failure"),
        "{}",
        failed.stderr
    );
    assert!(
        !failed
            .stderr
            .contains("background daemon exited before ready handshake"),
        "{}",
        failed.stderr
    );

    assert!(fixture.run_control("status").status.success());
    assert!(fixture.run_control("shutdown").status.success());
}

#[test]
fn changed_ready_frame_is_rejected_and_exact_child_is_cleaned() {
    let fixture = DaemonFixture::new("frame-change");
    let child = fixture.spawn_start(
        10_000,
        &[("EVA_DAEMON_TEST_READY_VALIDATION_DELAY_MS", "1500")],
    );
    let ready_path = wait_for_startup_file(&fixture.state(), "ready", Duration::from_secs(10));
    let ready = fs::read_to_string(&ready_path).unwrap();
    let generation = parse_fields(&ready).get("generation").unwrap().clone();
    let changed = ready.replacen(
        &format!("generation={generation}\n"),
        &format!("generation={}\n", generation.parse::<u64>().unwrap() + 100),
        1,
    );
    assert_ne!(ready, changed);
    fs::write(&ready_path, changed).unwrap();

    let failed = fixture.capture_parent(child);
    assert!(!failed.status.success(), "{}", failed.stdout);
    assert!(
        failed.stderr.contains("\"kind\":\"conflict\""),
        "{}",
        failed.stderr
    );
    assert!(!fixture.pids().join("daemon.pid").exists());
    assert!(fs::read_to_string(fixture.locks().join("daemon.lease"))
        .unwrap()
        .contains("state=released"));
    assert!(fixture.locks().join("daemon.lock").exists());
}

#[test]
#[ignore = "W1-L12 process evidence is captured explicitly by the three-platform CI gate"]
fn w1_real_process_evidence_covers_required_scenarios() {
    let mut scenarios = vec![measure_background_owner()];
    scenarios.extend(measure_forced_kill_stale_reclaim_and_restart());
    scenarios.push(measure_effect_deduplication());
    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| scenario.id)
            .collect::<Vec<_>>(),
        REQUIRED_RUNTIME_SCENARIOS
    );
    write_runtime_process_evidence(&scenarios);
}

fn measure_background_owner() -> RuntimeScenarioEvidence {
    let fixture = DaemonFixture::new("w1-background-owner");
    let start = fixture.capture_start(10_000, &[]);
    assert!(start.status.success(), "{}", start.stderr);
    let child_pid = json_u64(&start.stdout, "child_pid");
    assert!(child_pid > 0);
    let status = fixture.run_control("status");
    assert!(status.status.success(), "{}", text(&status.stderr));
    assert!(text(&status.stdout).contains("\"daemon_available\":true"));

    let shutdown = fixture.run_control("shutdown");
    assert!(shutdown.status.success(), "{}", text(&shutdown.stderr));
    wait_until(Duration::from_secs(5), || {
        !fixture.pids().join("daemon.pid").exists()
            && fs::read_to_string(fixture.locks().join("daemon.lease"))
                .is_ok_and(|lease| lease.contains("state=released"))
    });

    RuntimeScenarioEvidence::new("background_owner")
        .with_fact("child_pid_positive", "true")
        .with_fact("controlled_shutdown", "true")
        .with_fact("released", "true")
}

fn measure_forced_kill_stale_reclaim_and_restart() -> Vec<RuntimeScenarioEvidence> {
    const TASK_ID: &str = "req-w1-process-restart";
    let fixture = DaemonFixture::new("w1-forced-kill-restart");
    fs::create_dir_all(fixture.process_harness()).unwrap();
    let harness = fixture.process_harness().to_string_lossy().into_owned();
    let envs = [
        ("EVA_DAEMON_TEST_PROCESS_HARNESS_DIR", harness.as_str()),
        ("EVA_DAEMON_TEST_LEASE_TTL_MS", PROCESS_HARNESS_LEASE_TTL_MS),
    ];

    let start = fixture.capture_start(10_000, &envs);
    assert!(start.status.success(), "{}", start.stderr);
    let child_pid = json_u64(&start.stdout, "child_pid") as u32;
    let submit = fixture.run_control_with(
        "submit",
        &[
            "--task",
            TASK_ID,
            "--kind",
            "runtime.process-restart",
            "--agent",
            "root-agent",
            "--input",
            "restart-payload",
            "--idempotency-key",
            "w1-process-restart-key",
            "--max-attempts",
            "3",
            "--retry-backoff-ms",
            "0",
        ],
    );
    assert!(submit.status.success(), "{}", text(&submit.stderr));
    wait_for_file(
        &fixture.process_harness().join("restart.started.1"),
        Duration::from_secs(10),
    );
    let old_lease =
        parse_fields(&fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap());
    let old_generation = field_u64(&old_lease, "generation");
    let old_expiry = field_u128(&old_lease, "expires_at_ms");
    assert_eq!(field_u64(&old_lease, "pid"), u64::from(child_pid));

    force_kill_process(child_pid);
    assert!(fixture.pids().join("daemon.pid").is_file());
    assert!(fixture.locks().join("daemon.lock").is_file());
    let killed_lease = fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap();
    assert!(killed_lease.contains("state=active"), "{killed_lease}");
    let dead_status = fixture.run_control("status");
    let dead_status = format!("{}{}", text(&dead_status.stdout), text(&dead_status.stderr));
    assert!(
        dead_status.contains("\"key\":\"lease_owner_live\",\"value\":\"false\""),
        "{dead_status}"
    );
    assert!(
        dead_status.contains("\"key\":\"lease_expired\",\"value\":\"false\""),
        "{dead_status}"
    );

    let immediate = fixture.capture_start(10_000, &envs);
    assert!(!immediate.status.success(), "{}", immediate.stdout);
    assert!(
        immediate
            .stderr
            .contains("\"key\":\"child_error_kind\",\"value\":\"conflict\""),
        "{}",
        immediate.stderr
    );
    wait_until_epoch_ms(old_expiry + 150, Duration::from_secs(12));
    write_synced(
        &fixture.process_harness().join("restart.release"),
        b"release=true\n",
    );

    let restarted = fixture.capture_start(10_000, &envs);
    assert!(restarted.status.success(), "{}", restarted.stderr);
    let new_lease =
        parse_fields(&fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap());
    let new_generation = field_u64(&new_lease, "generation");
    assert!(new_generation > old_generation);
    let task = fixture.wait_for_task_status(TASK_ID, "completed");
    let attempts = json_u64(&task, "attempts");
    assert!(attempts >= 2, "{task}");
    wait_for_file(
        &fixture.process_harness().join("restart.started.2"),
        Duration::from_secs(5),
    );
    let shutdown = fixture.run_control("shutdown");
    assert!(shutdown.status.success(), "{}", text(&shutdown.stderr));

    vec![
        RuntimeScenarioEvidence::new("forced_kill")
            .with_fact("termination", "forced")
            .with_fact("mechanism", forced_kill_mechanism())
            .with_fact("owner_dead", "true")
            .with_fact("stale_pid_preserved", "true"),
        RuntimeScenarioEvidence::new("stale_lock_reclaim")
            .with_fact("immediate_restart_blocked", "true")
            .with_fact("new_generation_gt_old", "true")
            .with_fact("old_generation", old_generation)
            .with_fact("new_generation", new_generation),
        RuntimeScenarioEvidence::new("restart_recovery")
            .with_fact("status", "completed")
            .with_fact("attempts_min", attempts)
            .with_fact("generation_increased", "true")
            .with_fact(
                "result_digest_present",
                task.contains("\"result_digest\":\"sha256:"),
            ),
    ]
}

fn measure_effect_deduplication() -> RuntimeScenarioEvidence {
    const COMMITTED_TASK: &str = "req-w1-effect-committed";
    const PREPARED_TASK: &str = "req-w1-effect-prepared";
    let fixture = DaemonFixture::new("w1-effect-dedup");
    fs::create_dir_all(fixture.process_harness()).unwrap();
    let harness = fixture.process_harness().to_string_lossy().into_owned();
    let envs = [
        ("EVA_DAEMON_TEST_PROCESS_HARNESS_DIR", harness.as_str()),
        ("EVA_DAEMON_TEST_LEASE_TTL_MS", PROCESS_HARNESS_LEASE_TTL_MS),
    ];
    let start = fixture.capture_start(10_000, &envs);
    assert!(start.status.success(), "{}", start.stderr);

    write_synced(
        &fixture
            .process_harness()
            .join(format!("effect.pause-after-commit.{COMMITTED_TASK}")),
        b"enabled=true\n",
    );
    submit_effect_task(&fixture, COMMITTED_TASK, "w1-effect-committed-key");
    wait_for_file(
        &fixture
            .process_harness()
            .join(format!("effect.started.{COMMITTED_TASK}")),
        Duration::from_secs(10),
    );
    write_synced(
        &fixture
            .process_harness()
            .join(format!("effect.release.{COMMITTED_TASK}")),
        b"release=true\n",
    );
    wait_for_file(
        &fixture
            .process_harness()
            .join(format!("effect.committed.{COMMITTED_TASK}")),
        Duration::from_secs(10),
    );
    let first_lease =
        parse_fields(&fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap());
    let first_expiry = field_u128(&first_lease, "expires_at_ms");
    force_kill_process(field_u64(&first_lease, "pid") as u32);
    wait_until_epoch_ms(first_expiry + 150, Duration::from_secs(12));

    let first_restart = fixture.capture_start(10_000, &envs);
    assert!(first_restart.status.success(), "{}", first_restart.stderr);
    let committed = fixture.wait_for_task_status(COMMITTED_TASK, "completed");
    assert_eq!(json_u64(&committed, "attempts"), 1, "{committed}");
    let committed_ledger = parse_fields(&effect_ledger_record(&fixture, "w1-effect-committed-key"));
    let committed_digest = committed_ledger.get("result_digest").unwrap();
    let committed_size = field_u64(&committed_ledger, "result_size_bytes");
    assert!(
        committed.contains(&format!("\"result_digest\":\"{committed_digest}\"")),
        "{committed}"
    );
    assert_eq!(json_u64(&committed, "result_size_bytes"), committed_size);
    assert!(!fixture
        .process_harness()
        .join(format!("effect.duplicate.{COMMITTED_TASK}"))
        .exists());

    submit_effect_task(&fixture, PREPARED_TASK, "w1-effect-prepared-key");
    wait_for_file(
        &fixture
            .process_harness()
            .join(format!("effect.started.{PREPARED_TASK}")),
        Duration::from_secs(10),
    );
    let second_lease =
        parse_fields(&fs::read_to_string(fixture.locks().join("daemon.lease")).unwrap());
    let second_expiry = field_u128(&second_lease, "expires_at_ms");
    force_kill_process(field_u64(&second_lease, "pid") as u32);
    wait_until_epoch_ms(second_expiry + 150, Duration::from_secs(12));

    let second_restart = fixture.capture_start(10_000, &envs);
    assert!(second_restart.status.success(), "{}", second_restart.stderr);
    let prepared = fixture.wait_for_task_status(PREPARED_TASK, "interrupted");
    assert_eq!(json_u64(&prepared, "attempts"), 1, "{prepared}");
    assert!(
        prepared.contains("operator reconciliation required"),
        "{prepared}"
    );
    assert!(!fixture
        .process_harness()
        .join(format!("effect.duplicate.{PREPARED_TASK}"))
        .exists());
    assert!(fixture
        .process_harness()
        .join(format!("effect.applied.{COMMITTED_TASK}"))
        .is_file());
    assert!(fixture
        .process_harness()
        .join(format!("effect.applied.{PREPARED_TASK}"))
        .is_file());
    assert_eq!(effect_ledger_state_count(&fixture, "committed"), 1);
    assert_eq!(effect_ledger_state_count(&fixture, "prepared"), 1);
    let shutdown = fixture.run_control("shutdown");
    assert!(shutdown.status.success(), "{}", text(&shutdown.stderr));

    let stable_restart = fixture.capture_start(10_000, &envs);
    assert!(stable_restart.status.success(), "{}", stable_restart.stderr);
    let stable_prepared = fixture.wait_for_task_status(PREPARED_TASK, "interrupted");
    assert_eq!(json_u64(&stable_prepared, "attempts"), 1);
    assert!(stable_prepared.contains("operator reconciliation required"));
    assert!(!fixture
        .process_harness()
        .join(format!("effect.duplicate.{PREPARED_TASK}"))
        .exists());
    let stable_shutdown = fixture.run_control("shutdown");
    assert!(
        stable_shutdown.status.success(),
        "{}",
        text(&stable_shutdown.stderr)
    );

    RuntimeScenarioEvidence::new("effect_dedup")
        .with_fact("applied_count", 1)
        .with_fact("committed_applied_count", 1)
        .with_fact("duplicate", "false")
        .with_fact("status", "interrupted")
        .with_fact("operator_block", "true")
        .with_fact("committed_reused", "true")
        .with_fact("committed_status", "completed")
        .with_fact("prepared_attempts", 1)
        .with_fact("prepared_applied_count", 1)
        .with_fact("stable_restart", "true")
}

fn submit_effect_task(fixture: &DaemonFixture, task_id: &str, idempotency_key: &str) {
    let submit = fixture.run_control_with(
        "submit",
        &[
            "--task",
            task_id,
            "--kind",
            "runtime.process-effect",
            "--agent",
            "root-agent",
            "--input",
            "effect-payload",
            "--idempotency-key",
            idempotency_key,
            "--max-attempts",
            "2",
            "--retry-backoff-ms",
            "0",
        ],
    );
    assert!(submit.status.success(), "{}", text(&submit.stderr));
}

fn effect_ledger_state_count(fixture: &DaemonFixture, expected_state: &str) -> usize {
    fs::read_dir(fixture.durable().join("state").join("effects"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("effect"))
        .filter(|entry| {
            fs::read_to_string(entry.path())
                .unwrap()
                .contains(&format!("state={expected_state}\n"))
        })
        .count()
}

fn effect_ledger_record(fixture: &DaemonFixture, idempotency_key: &str) -> String {
    fs::read_dir(fixture.durable().join("state").join("effects"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("effect"))
        .map(|entry| fs::read_to_string(entry.path()).unwrap())
        .find(|record| record.contains(&format!("idempotency_key={idempotency_key}\n")))
        .unwrap()
}

fn write_runtime_process_evidence(scenarios: &[RuntimeScenarioEvidence]) {
    const ENVIRONMENT: [&str; 8] = [
        "EVA_W1_EVIDENCE_DIR",
        "EVA_W1_SOURCE_COMMIT",
        "EVA_W1_EXECUTOR",
        "EVA_W1_RUN_ID",
        "EVA_W1_RUN_ATTEMPT",
        "EVA_W1_JOB",
        "EVA_W1_OS",
        "EVA_W1_ARCH",
    ];
    let configured = ENVIRONMENT
        .iter()
        .filter(|name| std::env::var_os(name).is_some())
        .count();
    if configured == 0 {
        return;
    }
    assert_eq!(
        configured,
        ENVIRONMENT.len(),
        "W1 evidence environment must be configured atomically"
    );
    let value = |name: &str| {
        let value = std::env::var(name).unwrap_or_else(|_| panic!("{name} must be valid UTF-8"));
        assert_manifest_value(name, &value);
        value
    };
    let directory = PathBuf::from(value("EVA_W1_EVIDENCE_DIR"));
    fs::create_dir_all(&directory).unwrap();
    let metadata = fs::symlink_metadata(&directory).unwrap();
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
    let source_commit = value("EVA_W1_SOURCE_COMMIT");
    let executor = value("EVA_W1_EXECUTOR");
    let run_id = value("EVA_W1_RUN_ID");
    let run_attempt = value("EVA_W1_RUN_ATTEMPT");
    let job = value("EVA_W1_JOB");
    let os = value("EVA_W1_OS");
    let arch = value("EVA_W1_ARCH");
    assert_eq!(
        executor,
        format!("github-actions:{run_id}:{run_attempt}:{job}:{os}:{arch}")
    );

    let mut subject = format!(
        "format=eva.runtime-process.subject.v1\nsource_commit={source_commit}\nos={os}\narch={arch}\nexecutor={executor}\nrun_id={run_id}\nrun_attempt={run_attempt}\njob={job}\nscenario_count={}\n",
        scenarios.len()
    );
    for (scenario_index, scenario) in scenarios.iter().enumerate() {
        subject.push_str(&format!(
            "scenario.{scenario_index}.id={}\nscenario.{scenario_index}.status=passed\nscenario.{scenario_index}.fact_count={}\n",
            scenario.id,
            scenario.facts.len()
        ));
        for (fact_index, (name, value)) in scenario.facts.iter().enumerate() {
            assert_manifest_value(name, value);
            subject.push_str(&format!(
                "scenario.{scenario_index}.fact.{fact_index}.name={name}\nscenario.{scenario_index}.fact.{fact_index}.value={value}\n"
            ));
        }
    }
    let subject_path = directory.join("runtime-process.subject");
    write_synced(&subject_path, subject.as_bytes());
    let subject_bytes = fs::read(&subject_path).unwrap();
    assert_eq!(subject_bytes, subject.as_bytes());

    let environment =
        format!("os={os};arch={arch};run_id={run_id};run_attempt={run_attempt};job={job}");
    let envelope = EvidenceEnvelope::from_subject_bytes(
        EvidenceKind::Measurement,
        "w1-runtime-process-suite",
        &source_commit,
        environment,
        executor,
        epoch_ms(),
        &subject_bytes,
    )
    .unwrap();
    let envelope_path = directory.join("runtime-process.envelope");
    write_synced(&envelope_path, envelope.to_manifest().as_bytes());
    let reparsed =
        EvidenceEnvelope::parse_manifest(&fs::read_to_string(&envelope_path).unwrap()).unwrap();
    assert_eq!(reparsed, envelope);
    assert!(reparsed
        .verify_subject(&source_commit, &fs::read(&subject_path).unwrap())
        .unwrap()
        .is_verified());
}

fn assert_manifest_value(name: &str, value: &str) {
    assert!(
        !value.is_empty()
            && value.trim() == value
            && !value.chars().any(char::is_control)
            && !value.contains('='),
        "{name} is not a canonical manifest value"
    );
}

fn write_synced(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn wait_for_file(path: &Path, timeout: Duration) {
    wait_until(timeout, || path.is_file());
}

fn wait_until_epoch_ms(target_ms: u128, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while epoch_ms() < target_ms {
        assert!(
            Instant::now() < deadline,
            "epoch deadline {target_ms} did not arrive"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn field_u64(fields: &BTreeMap<String, String>, name: &str) -> u64 {
    fields.get(name).unwrap().parse().unwrap()
}

fn field_u128(fields: &BTreeMap<String, String>, name: &str) -> u128 {
    fields.get(name).unwrap().parse().unwrap()
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

#[cfg(windows)]
fn force_kill_process(pid: u32) {
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[cfg(unix)]
fn force_kill_process(pid: u32) {
    let output = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[cfg(windows)]
fn forced_kill_mechanism() -> &'static str {
    "terminate_process"
}

#[cfg(unix)]
fn forced_kill_mechanism() -> &'static str {
    "sigkill"
}

fn wait_for_startup_file(state: &Path, suffix: &str, timeout: Duration) -> PathBuf {
    let deadline = Instant::now() + timeout;
    loop {
        let startup = state.join("startup");
        if let Ok(entries) = fs::read_dir(&startup) {
            if let Some(path) = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some(suffix))
            {
                return path;
            }
        }
        assert!(
            Instant::now() < deadline,
            "startup .{suffix} file did not appear"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn only_startup_file(state: &Path, suffix: &str) -> PathBuf {
    wait_for_startup_file(state, suffix, Duration::from_secs(2))
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::sleep(Duration::from_millis(20));
    }
}

fn parse_fields(input: &str) -> BTreeMap<String, String> {
    input
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn json_u64(input: &str, field: &str) -> u64 {
    let marker = format!("\"{field}\":");
    let value = input.split_once(&marker).unwrap().1;
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap()
}

fn json_string_value(input: &str, field: &str) -> String {
    let marker = format!("\"{field}\":\"");
    input
        .split_once(&marker)
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0
        .to_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
