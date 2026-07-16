use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PARENT_TIMEOUT: Duration = Duration::from_secs(20);
const PIPE_EOF_TIMEOUT: Duration = Duration::from_secs(2);

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
