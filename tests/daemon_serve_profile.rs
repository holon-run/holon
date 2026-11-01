//! Linux compiled-binary regressions for durable daemon serve intent.
//!
//! Every invocation has its own home, socket, environment and process group.
//! No operator credentials or process-wide environment changes are used.

#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde_json::{json, Value};

const TOKEN: &str = "serve-profile-test-secret-not-an-operator-token";
const OTHER_TOKEN: &str = "serve-profile-replacement-secret-not-an-operator-token";
const DEADLINE: Duration = Duration::from_secs(90);

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("Cargo must provide the compiled holon binary")
}

// Instrumented daemon startup can be slow on CI. Serialize only this test
// executable; isolation never depends on changing the test process environment.
fn server_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ProcessGroup {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl ProcessGroup {
    fn kill(&mut self) {
        // SAFETY: Command::process_group(0) created this private group. Signal
        // the group, not a PID recovered from untrusted runtime metadata.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    fn diagnostics(&self) -> String {
        format!(
            "{}\n{}",
            fs::read_to_string(&self.stdout).unwrap_or_default(),
            fs::read_to_string(&self.stderr).unwrap_or_default()
        )
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Fixture {
    // Drop process groups before removing their home and log files.
    processes: Vec<ProcessGroup>,
    home: tempfile::TempDir,
    cwd: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated HOLON_HOME");
        let cwd = home.path().to_owned();
        fs::create_dir_all(home.path().join("ui")).unwrap();
        fs::create_dir_all(home.path().join("elsewhere")).unwrap();
        fs::write(home.path().join("ui/index.html"), "serve-profile-ui").unwrap();
        fs::write(home.path().join("control.token"), TOKEN).unwrap();
        fs::write(home.path().join("other.token"), OTHER_TOKEN).unwrap();
        Self {
            processes: Vec::new(),
            home,
            cwd,
        }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.home.path().join(path)
    }

    fn profile_bytes(&self) -> Vec<u8> {
        fs::read(self.path("daemon/serve_profile.json")).expect("persisted serve profile")
    }

    fn profile(&self) -> Value {
        serde_json::from_slice(&self.profile_bytes()).expect("valid serve profile JSON")
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(holon_bin());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("HOLON_HOME", self.home.path())
            .env("HOLON_WORKSPACE_DIR", self.home.path())
            .env("HOLON_SOCKET_PATH", self.path("run/control.sock"))
            .env("HOLON_AGENT_ID", "main")
            .env("HOLON_MODEL", "openai/gpt-5.4")
            .env("OPENAI_API_KEY", "offline-integration-test-key")
            .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
            .env("HOLON_DAEMON_START_TIMEOUT_SECS", "60")
            .env("RUST_LOG", "info")
            .envs(env.iter().copied())
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .process_group(0)
            .args(args);
        command
    }

    fn spawn(&mut self, args: &[&str], env: &[(&str, &str)]) -> usize {
        let index = self.processes.len();
        let stdout = self.path(&format!("command-{index}.stdout"));
        let stderr = self.path(&format!("command-{index}.stderr"));
        let child = self
            .command(args, env)
            .stdout(Stdio::from(File::create(&stdout).unwrap()))
            .stderr(Stdio::from(File::create(&stderr).unwrap()))
            .spawn()
            .expect("spawn isolated compiled holon");
        self.processes.push(ProcessGroup {
            child,
            stdout,
            stderr,
        });
        index
    }

    fn run_env(&mut self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let index = self.spawn(args, env);
        self.wait(index)
    }

    fn wait(&mut self, index: usize) -> Output {
        let process = &mut self.processes[index];
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = process.child.try_wait().expect("poll CLI") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI {index} timed out:\n{}",
                process.diagnostics()
            );
            thread::sleep(Duration::from_millis(25));
        };
        Output {
            status,
            stdout: fs::read(&process.stdout).unwrap(),
            stderr: fs::read(&process.stderr).unwrap(),
        }
    }

    fn wait_for_lifecycle_lock(&mut self, index: usize) {
        let process = &mut self.processes[index];
        let pid = process.child.id().to_string();
        let deadline = Instant::now() + DEADLINE;
        loop {
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "startup exited before acquiring lifecycle lock:\n{}",
                process.diagnostics()
            );
            assert!(
                !self.home.path().join("daemon/serve_profile.json").exists()
                    && !self.home.path().join("run/daemon.json").exists(),
                "startup published profile or identity while lifecycle lock was held"
            );
            // Linux exposes blocked flock requests, so both contenders can be
            // queued without relying on sleeps to hit the startup race.
            let locks = fs::read_to_string("/proc/locks").expect("Linux file lock table");
            if locks.lines().any(|line| {
                line.split_whitespace().any(|field| field == "->")
                    && line.split_whitespace().any(|field| field == pid)
            }) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "startup did not wait for lifecycle lock:\n{}",
                process.diagnostics()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn run(&mut self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn success_env(&mut self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let output = self.run_env(args, env);
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "expected JSON from {args:?}: {error}\n{}",
                output_text(&output)
            )
        })
    }

    fn success(&mut self, args: &[&str]) -> Value {
        self.success_env(args, &[])
    }

    fn start(&mut self, address: &str) -> Value {
        let advertise = format!("http://saved.example.test:{}", port(address));
        self.success(&[
            "daemon",
            "start",
            "--listen",
            address,
            "--advertise",
            &advertise,
            "--token-file",
            "control.token",
            "--web-dist",
            "ui",
            "--desktop-integration=false",
        ])
    }

    fn stop(&mut self) {
        let status = self.success(&["daemon", "stop"]);
        assert_eq!(status["state"], "stopped", "{status}");
        for name in ["run/daemon.json", "run/holon.pid", "run/control.sock"] {
            assert!(!self.path(name).exists(), "stop retained {name}");
        }
    }

    fn restore_stopped_legacy_metadata(&self, metadata: &[u8]) {
        let legacy: Value = serde_json::from_slice(metadata).unwrap();
        let pid = legacy["pid"].as_u64().unwrap();
        let process = PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + DEADLINE;
        // Shutdown may remove runtime files before the process exits. Restoring
        // its metadata earlier would make the live-PID fallback report Running.
        while process.exists() {
            assert!(
                Instant::now() < deadline,
                "stopped legacy daemon PID {pid} has not exited: {}",
                fs::read_to_string(process.join("stat")).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(25));
        }
        fs::write(self.path("run/daemon.json"), metadata).unwrap();
    }

    fn foreground(&mut self, args: &[&str]) -> usize {
        let index = self.spawn(args, &[]);
        let deadline = Instant::now() + DEADLINE;
        loop {
            let process = &mut self.processes[index];
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "foreground serve exited:\n{}",
                process.diagnostics()
            );
            if self.path("run/control.sock").exists() {
                let status = self.success(&["daemon", "status"]);
                if status["healthy"] == true {
                    return index;
                }
            }
            assert!(
                Instant::now() < deadline,
                "foreground serve readiness timeout:\n{}",
                self.processes[index].diagnostics()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn assert_no_secrets(&self) {
        let mut paths = vec![
            self.path("daemon/serve_profile.json"),
            self.path("run/daemon.json"),
            self.path("run/daemon.log"),
        ];
        for process in &self.processes {
            paths.push(process.stdout.clone());
            paths.push(process.stderr.clone());
        }
        for path in paths {
            let text = fs::read_to_string(&path).unwrap_or_default();
            for secret in [TOKEN, OTHER_TOKEN] {
                assert!(
                    !text.contains(secret),
                    "secret leaked into {}",
                    path.display()
                );
            }
        }
    }

    fn assert_menu_hint_not_persisted(&self) {
        let profile = String::from_utf8(self.profile_bytes()).unwrap();
        for hint in [
            "desktop-integration-if-unconfigured",
            "desktop_integration_if_unconfigured",
        ] {
            assert!(!profile.contains(hint), "menu hint persisted: {profile}");
        }
        let metadata: Value =
            serde_json::from_slice(&fs::read(self.path("run/daemon.json")).unwrap()).unwrap();
        let args = metadata["serve_args"]
            .as_array()
            .expect("normalized serve arguments");
        assert!(
            args.iter().all(|arg| !arg
                .as_str()
                .unwrap()
                .contains("desktop-integration-if-unconfigured")),
            "request-only menu hint became a serve argument: {metadata}"
        );
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed ({:?}):\n{}",
        output.status.code(),
        output_text(output)
    );
}

fn output_text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn fake_cloudflared(fixture: &Fixture, connect: bool) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = fixture.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = if connect {
        "#!/bin/sh\nif [ \"$1\" = --version ]; then exit 0; fi\nprintf '%s\\n' \"$$\" > \"$HOME/connector.pid\"\nprintf '%s\\n' \"$@\" > \"$HOME/connector.argv\"\nprintf '%s\\n' '{\"message\":\"Registered tunnel connection\",\"connIndex\":0}' >&2\nexec /bin/sleep 600\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = --version ]; then exit 0; fi\nprintf '%s\\n' 'secret-connector-error-must-not-be-published' >&2\nexit 8\n"
    };
    let file = bin.join("cloudflared");
    fs::write(&file, script).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.path("connector.token"), "fake-connector-secret").unwrap();
    bin.to_str().unwrap().to_owned()
}

fn wait_for_tunnel(fixture: &mut Fixture, phase: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let status = fixture.success(&["daemon", "status"]);
        if status["tunnel"]["phase"] == phase {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "missing tunnel phase {phase}: {status}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn assert_connector_gone(pid: u32) {
    let deadline = Instant::now() + DEADLINE;
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "connector {pid} survived shutdown"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn cloudflare_named_daemon_restart_and_mode_replacement_clean_up_connector() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let path = fake_cloudflared(&fixture, true);
    let address = free_address();
    let started = fixture.success_env(
        &[
            "daemon",
            "start",
            "--access",
            "cloudflare",
            "--listen",
            &address,
            "--advertise",
            "https://holon.example.com",
            "--cloudflare-token-file",
            "connector.token",
            "--token-file",
            "control.token",
            "--web-dist",
            "ui",
        ],
        &[("PATH", &path), ("HOLON_CONTROL_AUTH_MODE", "disabled")],
    );
    assert_eq!(started["healthy"], true, "{started}");
    let connected = wait_for_tunnel(&mut fixture, "connected");
    assert_eq!(
        connected["tunnel"]["public_url"],
        "https://holon.example.com"
    );
    assert_eq!(fixture.profile()["control_auth_mode"], "required");
    let first_pid: u32 = fs::read_to_string(fixture.path("connector.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let argv = fs::read_to_string(fixture.path("connector.argv")).unwrap();
    assert!(argv.contains("--output\njson\nrun\n--token-file\n"));
    assert!(argv.contains("--config\n/dev/null\n"));
    assert!(!argv.contains("fake-connector-secret"));
    let restarted = fixture.success_env(&["daemon", "restart"], &[("PATH", &path)]);
    assert_eq!(restarted["healthy"], true, "{restarted}");
    wait_for_tunnel(&mut fixture, "connected");
    assert_connector_gone(first_pid);
    let second_pid: u32 = fs::read_to_string(fixture.path("connector.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(first_pid, second_pid);
    fixture.success(&["daemon", "restart", "--access", "local"]);
    assert_connector_gone(second_pid);
    assert!(fixture.profile().get("cloudflare_token_file").is_none());
    assert_eq!(
        fixture.success(&["daemon", "status"])["tunnel"],
        Value::Null
    );
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn cloudflare_connector_failure_keeps_local_daemon_healthy() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let path = fake_cloudflared(&fixture, false);
    let address = free_address();
    fixture.success_env(
        &[
            "daemon",
            "start",
            "--access",
            "cloudflare",
            "--listen",
            &address,
            "--advertise",
            "https://holon.example.com",
            "--cloudflare-token-file",
            "connector.token",
            "--token-file",
            "control.token",
        ],
        &[("PATH", &path)],
    );
    let failed = wait_for_tunnel(&mut fixture, "failed");
    assert_eq!(failed["healthy"], true, "{failed}");
    assert_eq!(failed["state"], "running", "{failed}");
    assert!(!failed.to_string().contains("secret-connector-error"));
    assert!(!fs::read_to_string(fixture.path("run/daemon.log"))
        .unwrap_or_default()
        .contains("secret-connector-error"));
    fixture.stop();
}

#[test]
fn cloudflare_failed_preflight_preserves_healthy_pid_and_accepted_profile() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let path = fake_cloudflared(&fixture, true);
    let address = free_address();
    let original = fixture.start(&address);
    let original_profile = fixture.profile_bytes();
    let empty_path = fixture.path("empty-bin");
    fs::create_dir(&empty_path).unwrap();
    for (token_file, dependency_path, expected_error) in [
        ("missing-connector.token", path.as_str(), "token file"),
        (
            "connector.token",
            empty_path.to_str().unwrap(),
            "cloudflared",
        ),
    ] {
        let output = fixture.run_env(
            &[
                "daemon",
                "restart",
                "--access",
                "cloudflare",
                "--listen",
                &address,
                "--advertise",
                "https://holon.example.com",
                "--cloudflare-token-file",
                token_file,
            ],
            &[("PATH", dependency_path)],
        );
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(
            output_text(&output).contains(expected_error),
            "{}",
            output_text(&output)
        );
        let current = fixture.success(&["daemon", "status"]);
        assert_eq!(current["pid"], original["pid"], "{current}");
        assert_eq!(current["healthy"], true, "{current}");
        assert_eq!(fixture.profile_bytes(), original_profile);
        assert!(!fixture.path("connector.pid").exists());
    }
    fixture.stop();
}

#[test]
fn cloudflare_foreground_preflight_failure_does_not_accept_profile() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    let output = fixture.run(&[
        "serve",
        "--access",
        "cloudflare",
        "--listen",
        &address,
        "--advertise",
        "https://holon.example.com",
        "--cloudflare-token-file",
        "missing-connector.token",
        "--token-file",
        "control.token",
    ]);
    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(!fixture.path("daemon/serve_profile.json").exists());
    assert!(!fixture.path("run/daemon.json").exists());
    assert!(!fixture.path("runtime.db").exists());
}

fn free_address() -> String {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

#[test]
fn managed_daemon_logs_rotate_without_replacing_the_running_process() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    fs::create_dir_all(fixture.path("run")).unwrap();
    let log_path = fixture.path("run/daemon.log");
    let segment_bytes = 16 * 1024 * 1024;
    File::create(&log_path)
        .unwrap()
        .set_len(segment_bytes)
        .unwrap();
    let started = fixture.start(&free_address());
    let pid = started["pid"].clone();
    let logs = fixture.success(&["daemon", "logs", "--tail", "20"]);
    assert_eq!(logs["log_health"]["degraded"], false, "{logs}");
    assert!(
        fs::metadata(fixture.path("run/daemon.log.1"))
            .unwrap()
            .len()
            <= segment_bytes
    );
    assert!(fs::metadata(&log_path).unwrap().len() <= segment_bytes);
    assert!(!fs::read_to_string(&log_path).unwrap().contains('\u{1b}'));
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["pid"], pid, "rotation must not restart the daemon");
    fixture.stop();
    fixture.start(&free_address());
    let logs = fixture.success(&["daemon", "logs", "--tail", "5"]);
    assert_eq!(logs["log_health"]["degraded"], false, "{logs}");
    fixture.stop();
}

fn port(address: &str) -> String {
    address.parse::<SocketAddr>().unwrap().port().to_string()
}

fn assert_profile_options(profile: &Value, address: &str, home: &Path) {
    assert_eq!(profile["schema_version"], 1, "{profile}");
    assert_eq!(profile["listen"], address, "{profile}");
    assert_eq!(profile["desktop_integration"], false, "{profile}");
    assert_eq!(
        profile["web_dist"],
        json!(home.join("ui")),
        "web-dist must be independent of the next command's cwd"
    );
    assert_eq!(
        profile["authentication"],
        json!({"mode": "token_file", "path": home.join("control.token")}),
        "token-file path must also be independent of the next command's cwd"
    );
}

#[test]
fn startup_phase_logs_preserve_serving_order_and_daemon_handoff_correlation() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let assert_phases = |log: &str, expected: &[&str]| -> String {
        // tracing's terminal styling must not affect the semantic assertions.
        let mut plain = String::new();
        let mut escape = false;
        for c in log.chars() {
            if c == '\x1b' {
                escape = true;
            } else if escape {
                if c == 'm' {
                    escape = false;
                }
            } else {
                plain.push(c);
            }
        }
        assert!(
            !plain.contains("startup runtime state recovery completed"),
            "{plain}"
        );
        let mut cursor = 0;
        let mut startup_id = None;
        for phase in expected {
            for state in ["started", "finished"] {
                let line = plain[cursor..]
                    .lines()
                    .find(|line| {
                        line.contains(&format!("phase=\"{phase}\""))
                            && line.contains(&format!("state=\"{state}\""))
                    })
                    .unwrap_or_else(|| panic!("missing {phase}/{state}:\n{plain}"));
                if state == "finished" {
                    assert!(line.contains("outcome=\"ok\""), "{line}");
                    assert!(line.contains("elapsed_ms="), "{line}");
                }
                let id = line
                    .split("startup_id=")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_string();
                if let Some(expected_id) = &startup_id {
                    assert_eq!(&id, expected_id, "{line}");
                } else {
                    startup_id = Some(id);
                }
                cursor += plain[cursor..].find(line).unwrap() + line.len();
            }
        }
        startup_id.unwrap()
    };
    let foreground = fixture.foreground(&[
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--token-file",
        "control.token",
    ]);
    let foreground_log = fs::read_to_string(&fixture.processes[foreground].stderr).unwrap();
    assert_phases(
        &foreground_log,
        &[
            "storage_prepare",
            "host_init",
            "durable_reconcile",
            "candidate_discovery",
            "default_activation",
            "first_run",
            "bind",
            "router",
            "serving_ready",
        ],
    );
    fixture.stop();
    let started = fixture.run(&[
        "daemon",
        "start",
        "--listen",
        "127.0.0.1:0",
        "--token-file",
        "control.token",
    ]);
    assert_success(&started);
    let preparation_id = assert_phases(
        std::str::from_utf8(&started.stderr).unwrap(),
        &["storage_prepare"],
    );
    let daemon_log = fs::read_to_string(fixture.path("run/daemon.log")).unwrap();
    let serve_id = assert_phases(
        &daemon_log,
        &[
            "host_init",
            "durable_reconcile",
            "candidate_discovery",
            "default_activation",
            "first_run",
            "bind",
            "router",
            "serving_ready",
        ],
    );
    assert_eq!(
        preparation_id, serve_id,
        "daemon handoff lost startup correlation"
    );
    fixture.stop();
}

#[test]
fn startup_phase_logs_classify_real_bind_failure_as_error_not_cancellation() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = occupied.local_addr().unwrap().to_string();
    // The legacy marker bypasses parent preflight, so the compiled serve path
    // reaches a real bind failure without adding any production fault switch.
    let failed = fixture.run_env(
        &[
            "serve",
            "--listen",
            &address,
            "--token-file",
            "control.token",
        ],
        &[("HOLON_PRE_SERVER_RUNTIME_PREPARED", "1")],
    );
    assert!(!failed.status.success(), "{}", output_text(&failed));
    let log = String::from_utf8(failed.stderr).unwrap();
    for phase in ["bind", "serve_startup"] {
        let finished = log
            .lines()
            .find(|line| line.contains(&format!("\"{phase}\"")) && line.contains("\"finished\""))
            .unwrap_or_else(|| panic!("missing {phase} result:\n{log}"));
        assert!(finished.contains("\"error\""), "{finished}");
        assert!(!finished.contains("\"incomplete\""), "{finished}");
    }
    assert!(!log.contains("serving_ready"), "{log}");
    fixture.assert_no_secrets();
}

#[test]
fn ephemeral_port_intent_survives_foreground_start_and_restart() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let assert_listener = |path: &Path| {
        let log = fs::read_to_string(path).unwrap();
        let address: SocketAddr = log
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix("Holon listening on "))
            .unwrap_or_else(|| panic!("missing bound listener address:\n{log}"))
            .parse()
            .unwrap();
        assert_ne!(address.port(), 0);
        TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .expect("ephemeral listener must be reachable");
    };
    let foreground = fixture.foreground(&[
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--token-file",
        "control.token",
    ]);
    let original = fixture.profile();
    assert_eq!(original["listen"], "127.0.0.1:0", "{original}");
    assert!(original["port"].is_null(), "{original}");
    let running = fixture.success(&["daemon", "status"]);
    assert_eq!(running["healthy"], true, "{running}");
    assert_listener(&fixture.processes[foreground].stdout);
    fixture.stop();
    for command in ["start", "restart"] {
        let running = fixture.success(&["daemon", command]);
        assert_eq!(running["healthy"], true, "{running}");
        assert_listener(&fixture.path("run/daemon.log"));
        assert_eq!(fixture.profile(), original);
    }
    fixture.stop();
}

#[test]
fn foreground_stop_start_and_restart_inherit_full_profile() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.foreground(&[
        "serve",
        "--access",
        "lan",
        "--host",
        "profile.example.test",
        "--listen",
        &address,
        "--token-file",
        "control.token",
        "--web-dist",
        "ui",
        "--desktop-integration=false",
    ]);
    let original = fixture.profile();
    assert_profile_options(&original, &address, fixture.home.path());
    assert_eq!(original["access"], "lan", "{original}");
    assert_eq!(original["host"], "profile.example.test", "{original}");

    fixture.stop();
    assert_eq!(fixture.profile(), original);
    let stopped = fixture.success(&["daemon", "status"]);
    assert!(
        stopped["desired_serve"].is_object(),
        "stopped status must expose non-secret desired serve intent: {stopped}"
    );
    assert_eq!(stopped["desired_serve"]["source"], "profile", "{stopped}");
    assert_eq!(stopped["desired_serve"]["listen"], address, "{stopped}");
    assert!(
        stopped.get("desired_serve_source").is_none(),
        "source belongs inside desired_serve, not beside it: {stopped}"
    );
    fixture.cwd = fixture.path("elsewhere");
    let started = fixture.success(&["daemon", "start"]);
    assert_eq!(started["http_addr"], address, "{started}");
    assert_eq!(started["process_created"], true, "{started}");
    assert_profile_options(&fixture.profile(), &address, fixture.home.path());
    let restarted = fixture.success(&["daemon", "restart"]);
    assert_eq!(restarted["http_addr"], address, "{restarted}");
    assert_eq!(fixture.profile(), original);

    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(format!("http://{address}/"))
        .header(reqwest::header::ACCEPT, "text/html")
        .bearer_auth(TOKEN)
        .send()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().unwrap(), "serve-profile-ui");
    let endpoint = format!("http://{address}/api/control/runtime/status");
    assert_eq!(client.get(&endpoint).send().unwrap().status(), 401);
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth(TOKEN)
            .send()
            .unwrap()
            .status(),
        200
    );
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn concurrent_foreground_and_start_keep_the_winning_profile_and_identity() {
    let _lock = server_lock();
    for foreground_first in [true, false] {
        let mut fixture = Fixture::new();
        fs::create_dir_all(fixture.path("run")).unwrap();
        let lifecycle_lock = File::create(fixture.path("run/lifecycle.lock")).unwrap();
        FileExt::lock_exclusive(&lifecycle_lock).unwrap();
        let foreground_address = free_address();
        let mut start_address = free_address();
        while start_address == foreground_address {
            start_address = free_address();
        }
        let foreground_args = [
            "serve",
            "--listen",
            &foreground_address,
            "--token-file",
            "control.token",
            "--web-dist",
            "ui",
            "--desktop-integration=false",
        ];
        let start_args = [
            "daemon",
            "start",
            "--listen",
            &start_address,
            "--token-file",
            "other.token",
            "--web-dist",
            "ui",
            "--desktop-integration=false",
        ];
        let commands: [&[&str]; 2] = if foreground_first {
            [&foreground_args, &start_args]
        } else {
            [&start_args, &foreground_args]
        };
        let first = fixture.spawn(commands[0], &[]);
        fixture.wait_for_lifecycle_lock(first);
        let second = fixture.spawn(commands[1], &[]);
        fixture.wait_for_lifecycle_lock(second);
        drop(lifecycle_lock);
        let (foreground, start) = if foreground_first {
            (first, second)
        } else {
            (second, first)
        };
        let deadline = Instant::now() + DEADLINE;
        let running = loop {
            if fixture.path("run/control.sock").exists() {
                let status = fixture.success(&["daemon", "status"]);
                if status["healthy"] == true {
                    break status;
                }
            }
            assert!(
                Instant::now() < deadline,
                "concurrent startup did not become healthy:\n{}\n{}",
                fixture.processes[first].diagnostics(),
                fixture.processes[second].diagnostics()
            );
            thread::sleep(Duration::from_millis(25));
        };
        let original = fixture.profile_bytes();
        let metadata = fs::read(fixture.path("run/daemon.json")).unwrap();
        let profile = fixture.profile();
        assert_eq!(profile["listen"], running["http_addr"], "{running}");
        let foreground_won = running["http_addr"] == foreground_address;
        assert!(
            foreground_won || running["http_addr"] == start_address,
            "unexpected concurrent winner: {running}"
        );
        let expected_token_file = if foreground_won {
            "control.token"
        } else {
            "other.token"
        };
        assert_eq!(
            profile["authentication"],
            json!({"mode": "token_file", "path": fixture.path(expected_token_file)})
        );
        let started = fixture.wait(start);
        if foreground_won {
            assert!(!started.status.success(), "{}", output_text(&started));
            assert_eq!(
                running["pid"],
                fixture.processes[foreground].child.id(),
                "{running}"
            );
            assert!(fixture.processes[foreground]
                .child
                .try_wait()
                .unwrap()
                .is_none());
        } else {
            assert_success(&started);
            let started: Value = serde_json::from_slice(&started.stdout).unwrap();
            assert_eq!(started["process_created"], true, "{started}");
            assert_eq!(started["pid"], running["pid"], "{started}");
            let rejected = fixture.wait(foreground);
            assert!(!rejected.status.success(), "{}", output_text(&rejected));
        }
        let after = fixture.success(&["daemon", "status"]);
        assert_eq!(after["healthy"], true, "{after}");
        assert_eq!(after["pid"], running["pid"], "{after}");
        assert_eq!(
            after["runtime_config_fingerprint"], running["runtime_config_fingerprint"],
            "{after}"
        );
        assert_eq!(after["desired_serve"]["listen"], running["http_addr"]);
        assert_eq!(fixture.profile_bytes(), original);
        assert_eq!(
            fs::read(fixture.path("run/daemon.json")).unwrap(),
            metadata,
            "losing startup must not replace the running identity"
        );
        fixture.stop();
        fixture.assert_no_secrets();
    }
}

#[test]
fn local_required_auth_policy_survives_start_without_original_environment() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.success_env(
        &[
            "daemon",
            "start",
            "--access",
            "local",
            "--listen",
            &address,
            "--token-file",
            "control.token",
        ],
        &[("HOLON_CONTROL_AUTH_MODE", "required")],
    );
    let original = fixture.profile();
    assert_eq!(original["access"], "local", "{original}");
    assert_eq!(original["control_auth_mode"], "required", "{original}");
    assert_eq!(
        original["authentication"],
        json!({"mode": "token_file", "path": fixture.path("control.token")}),
        "{original}"
    );
    fixture.stop();
    assert_eq!(fixture.profile(), original);
    let stopped = fixture.success(&["daemon", "status"]);
    assert_eq!(stopped["desired_serve"]["source"], "profile", "{stopped}");
    assert_eq!(
        stopped["desired_serve"]["control_auth_mode"], "required",
        "{stopped}"
    );

    // Fixture commands clear their environment, so the original required
    // policy must now come from the profile, not the startup environment.
    let started = fixture.success(&["daemon", "start"]);
    assert_eq!(started["http_addr"], address, "{started}");
    assert_eq!(fixture.profile(), original);
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["desired_serve"]["source"], "profile", "{status}");
    assert_eq!(
        status["desired_serve"]["control_auth_mode"], "required",
        "{status}"
    );
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let endpoint = format!("http://{address}/api/control/runtime/status");
    assert_eq!(client.get(&endpoint).send().unwrap().status(), 401);
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth(TOKEN)
            .send()
            .unwrap()
            .status(),
        200
    );
    fixture.stop();
    assert_eq!(fixture.profile(), original);
    fixture.assert_no_secrets();
}

#[test]
fn fresh_home_status_reports_defaults_without_creating_a_profile() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["state"], "stopped", "{status}");
    assert_eq!(status["desired_serve"]["source"], "defaults", "{status}");
    assert!(
        status.get("desired_serve_source").is_none(),
        "source belongs inside desired_serve, not beside it: {status}"
    );
    assert!(!fixture.path("daemon/serve_profile.json").exists());
    assert!(!fixture.path("run/daemon.json").exists());
}

#[test]
fn stale_menu_defaults_hint_inherits_a_profile_created_by_another_start_or_serve() {
    let _lock = server_lock();
    for foreground in [false, true] {
        for action in ["start", "restart"] {
            let mut fixture = Fixture::new();
            let observed = fixture.success(&["daemon", "status"]);
            assert_eq!(
                observed["desired_serve"]["source"], "defaults",
                "{observed}"
            );
            assert!(!fixture.path("daemon/serve_profile.json").exists());

            // Another launcher writes intent after the menu's status read.
            let address = free_address();
            if foreground {
                fixture.foreground(&[
                    "serve",
                    "--listen",
                    &address,
                    "--token-file",
                    "control.token",
                    "--web-dist",
                    "ui",
                    "--desktop-integration=false",
                ]);
            } else {
                fixture.start(&address);
            }
            let before = fixture.success(&["daemon", "status"]);
            assert_eq!(before["healthy"], true, "{before}");
            assert_eq!(before["desired_serve"]["source"], "profile", "{before}");
            assert_eq!(
                before["desired_serve"]["desktop_integration"], false,
                "{before}"
            );
            let original = fixture.profile_bytes();

            // This request is based on the now-stale defaults observation.
            let menu =
                fixture.success(&["daemon", action, "--desktop-integration-if-unconfigured"]);
            assert_eq!(menu["process_created"], action == "restart", "{menu}");
            if action == "start" {
                assert_eq!(menu["pid"], before["pid"], "{menu}");
            } else {
                assert_ne!(menu["pid"], before["pid"], "{menu}");
            }
            assert_eq!(menu["http_addr"], address, "{menu}");
            assert_eq!(
                menu["runtime_config_fingerprint"], before["runtime_config_fingerprint"],
                "{menu}"
            );
            assert_eq!(fixture.profile_bytes(), original);
            let after = fixture.success(&["daemon", "status"]);
            assert_eq!(after["healthy"], true, "{after}");
            assert_eq!(after["pid"], menu["pid"], "{after}");
            assert_eq!(
                after["desired_serve"]["desktop_integration"], false,
                "{after}"
            );
            fixture.assert_menu_hint_not_persisted();
            fixture.stop();
            fixture.assert_no_secrets();
        }
    }
}

#[test]
fn menu_defaults_hint_is_ignored_for_stopped_profile_and_legacy_metadata() {
    let _lock = server_lock();
    for legacy in [false, true] {
        for action in ["start", "restart"] {
            let mut fixture = Fixture::new();
            let address = free_address();
            let started = fixture.start(&address);
            let original = fixture.profile_bytes();
            let metadata = fs::read(fixture.path("run/daemon.json")).unwrap();
            fixture.stop();
            if legacy {
                fs::remove_file(fixture.path("daemon/serve_profile.json")).unwrap();
                fixture.restore_stopped_legacy_metadata(&metadata);
            }
            let before = fixture.success(&["daemon", "status"]);
            assert_eq!(
                before["state"],
                if legacy { "stale" } else { "stopped" },
                "{before}"
            );
            assert_eq!(before["control_connectivity"], false, "{before}");
            assert_eq!(
                before["desired_serve"]["source"],
                if legacy { "legacy" } else { "profile" },
                "{before}"
            );
            let menu =
                fixture.success(&["daemon", action, "--desktop-integration-if-unconfigured"]);
            assert_eq!(menu["process_created"], true, "{menu}");
            assert_ne!(menu["pid"], started["pid"], "{menu}");
            assert_eq!(menu["http_addr"], address, "{menu}");
            assert_eq!(fixture.profile_bytes(), original);
            let after = fixture.success(&["daemon", "status"]);
            assert_eq!(after["healthy"], true, "{after}");
            assert_eq!(after["desired_serve"]["source"], "profile", "{after}");
            assert_eq!(
                after["desired_serve"]["desktop_integration"], false,
                "{after}"
            );
            fixture.assert_menu_hint_not_persisted();
            fixture.stop();
            fixture.assert_no_secrets();
        }
    }
}

#[test]
fn first_menu_defaults_hint_still_requires_macos_for_desktop_integration() {
    let _lock = server_lock();
    for action in ["start", "restart"] {
        let mut fixture = Fixture::new();
        let address = free_address();
        let output = fixture.run(&[
            "daemon",
            action,
            "--listen",
            &address,
            "--token-file",
            "control.token",
            "--desktop-integration-if-unconfigured",
        ]);
        assert!(
            !output.status.success(),
            "Linux enabled desktop integration"
        );
        assert!(
            output_text(&output).contains("desktop integration requires macOS"),
            "first-use hint must reach the platform validation: {}",
            output_text(&output)
        );
        assert!(!fixture.path("daemon/serve_profile.json").exists());
        assert!(!fixture.path("run/daemon.json").exists());
        let status = fixture.success(&["daemon", "status"]);
        assert_eq!(status["state"], "stopped", "{status}");
        assert_eq!(status["desired_serve"]["source"], "defaults", "{status}");
        fixture.assert_no_secrets();
    }
}

#[test]
fn legacy_metadata_status_reports_migration_source_and_start_saves_profile() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    let mut legacy: Value =
        serde_json::from_slice(&fs::read(fixture.path("run/daemon.json")).unwrap()).unwrap();
    fixture.stop();
    fs::remove_file(fixture.path("daemon/serve_profile.json")).unwrap();
    // Keep identity fields generated by the real binary, but use the old
    // metadata contract rather than relying on its current serve_args shape.
    legacy["serve_args"] = json!([
        "--listen",
        address,
        "--token-file",
        fixture.path("control.token"),
        "--web-dist",
        fixture.path("ui"),
        "--desktop-integration=false"
    ]);
    legacy["control_token_env_configured"] = json!(false);
    fixture.restore_stopped_legacy_metadata(&serde_json::to_vec(&legacy).unwrap());
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["state"], "stale", "{status}");
    assert_eq!(status["control_connectivity"], false, "{status}");
    assert_eq!(status["desired_serve"]["source"], "legacy", "{status}");
    assert_eq!(status["desired_serve"]["listen"], address, "{status}");
    assert!(!fixture.path("daemon/serve_profile.json").exists());
    fixture.cwd = fixture.path("elsewhere");
    let started = fixture.success(&["daemon", "start"]);
    assert_eq!(started["process_created"], true, "{started}");
    assert_eq!(started["http_addr"], address, "{started}");
    assert_profile_options(&fixture.profile(), &address, fixture.home.path());
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["desired_serve"]["source"], "profile", "{status}");
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn running_legacy_metadata_without_explicit_defaults_is_adopted_without_stop() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    let started = fixture.start(&address);
    let original = fixture.profile();
    let metadata_path = fixture.path("run/daemon.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    // Old launchers omitted these default arguments, even though the new
    // canonical launch plan spells out --access local and desktop=false.
    legacy["serve_args"] = json!([
        "--listen",
        address,
        "--advertise",
        original["advertise"],
        "--token-file",
        fixture.path("control.token"),
        "--web-dist",
        fixture.path("ui")
    ]);
    fs::write(&metadata_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    fs::remove_file(fixture.path("daemon/serve_profile.json")).unwrap();
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["healthy"], true, "{status}");
    assert_eq!(status["pid"], started["pid"], "{status}");
    assert_eq!(status["desired_serve"]["source"], "legacy", "{status}");
    assert!(!fixture.path("daemon/serve_profile.json").exists());

    fixture.cwd = fixture.path("elsewhere");
    let adopted = fixture.success(&["daemon", "start"]);
    assert_eq!(adopted["process_created"], false, "{adopted}");
    assert_eq!(adopted["pid"], started["pid"], "{adopted}");
    assert_eq!(adopted["http_addr"], address, "{adopted}");
    assert_eq!(
        adopted["runtime_config_fingerprint"], started["runtime_config_fingerprint"],
        "{adopted}"
    );
    assert_profile_options(&fixture.profile(), &address, fixture.home.path());
    assert_eq!(fixture.profile(), original);
    let after: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    for field in ["pid", "started_at", "config_fingerprint", "http_addr"] {
        assert_eq!(after[field], legacy[field], "adoption changed {field}");
    }
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["healthy"], true, "{status}");
    assert_eq!(status["desired_serve"]["source"], "profile", "{status}");
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn crash_and_loss_of_run_directory_do_not_lose_serve_intent() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    let original = fixture.profile_bytes();
    // The daemon inherits the private group of its originating start command.
    fixture.processes[0].kill();
    let deadline = Instant::now() + DEADLINE;
    loop {
        if TcpListener::bind(&address).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "crashed daemon retained its port"
        );
        thread::sleep(Duration::from_millis(25));
    }
    fs::remove_dir_all(fixture.path("run")).unwrap();
    assert_eq!(fixture.profile_bytes(), original);
    let started = fixture.success(&["daemon", "start"]);
    assert_eq!(started["http_addr"], address, "{started}");
    assert_eq!(fixture.profile_bytes(), original);
    fixture.stop();
}

#[test]
fn explicit_cli_and_env_overrides_preserve_unrelated_options_and_log_sources() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    let original = fixture.profile();
    fixture.success(&["daemon", "restart", "--token-file", "other.token"]);
    let auth_only = fixture.profile();
    assert_eq!(
        auth_only["authentication"],
        json!({"mode": "token_file", "path": fixture.path("other.token")})
    );
    for field in [
        "listen",
        "port",
        "access",
        "host",
        "advertise",
        "web_dist",
        "desktop_integration",
    ] {
        assert_eq!(
            auth_only[field], original[field],
            "auth-only changed {field}"
        );
    }
    let port_address = free_address();
    let new_port = port(&port_address);
    fixture.success(&["daemon", "restart", "--port", &new_port]);
    let profile = fixture.profile();
    assert!(profile["listen"].is_null(), "{profile}");
    assert_eq!(profile["port"], new_port.parse::<u16>().unwrap());

    let cli_address = free_address();
    let env_address = free_address();
    let overridden = fixture.success_env(
        &["daemon", "restart", "--listen", &cli_address],
        &[("HOLON_HTTP_ADDR", &env_address)],
    );
    assert_eq!(overridden["http_addr"], cli_address, "{overridden}");
    assert_eq!(fixture.profile()["listen"], cli_address);
    assert!(fixture.profile()["port"].is_null());
    let overridden =
        fixture.success_env(&["daemon", "restart"], &[("HOLON_HTTP_ADDR", &env_address)]);
    assert_eq!(overridden["http_addr"], env_address, "{overridden}");
    assert_eq!(fixture.profile()["listen"], env_address);
    assert_eq!(fixture.profile()["web_dist"], original["web_dist"]);
    assert_eq!(fixture.profile()["desktop_integration"], false);
    let logs = fixture
        .processes
        .iter()
        .map(ProcessGroup::diagnostics)
        .collect::<Vec<_>>()
        .join("\n")
        + &fs::read_to_string(fixture.path("run/daemon.log")).unwrap();
    assert!(
        logs.contains("base=profile")
            && logs.contains("CLI overrides=listen")
            && logs.contains("env overrides=HOLON_HTTP_ADDR(listen)"),
        "override diagnostics must identify explicit CLI and env sources:\n{logs}"
    );
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn reset_discards_previous_options_instead_of_merging_them() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    fixture.stop();
    let new_address = free_address();
    let status = fixture.success(&[
        "daemon",
        "start",
        "--reset-serve-options",
        "--listen",
        &new_address,
    ]);
    assert_eq!(status["http_addr"], new_address, "{status}");
    let reset = fixture.profile();
    assert_eq!(reset["listen"], new_address, "{reset}");
    assert!(reset["web_dist"].is_null(), "{reset}");
    assert!(reset["advertise"].is_null(), "{reset}");
    assert_eq!(reset["authentication"], json!({"mode": "none"}));
    assert!(
        !String::from_utf8(fixture.profile_bytes())
            .unwrap()
            .contains("control.token"),
        "reset must not inherit the old authentication source"
    );
    fixture.stop();
    assert_eq!(fixture.profile(), reset);
    let restarted = fixture.success(&["daemon", "start"]);
    assert_eq!(restarted["http_addr"], new_address, "{restarted}");
    assert_eq!(
        fixture.profile(),
        reset,
        "ordinary start must preserve the accepted reset instead of restoring old options"
    );
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn corrupt_or_unsupported_profile_is_rejected_unchanged_and_reset_recovers() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    fixture.stop();
    let mut unsupported = fixture.profile();
    unsupported["schema_version"] = json!(999);
    let mut invalid_binding = fixture.profile();
    invalid_binding["port"] = json!(0);
    for invalid in [
        b"{not-json".to_vec(),
        serde_json::to_vec(&unsupported).unwrap(),
        serde_json::to_vec(&invalid_binding).unwrap(),
    ] {
        fs::write(fixture.path("daemon/serve_profile.json"), &invalid).unwrap();
        let status = fixture.run(&["daemon", "status"]);
        assert!(
            !status.status.success(),
            "invalid profile must not be reported as defaults: {}",
            output_text(&status)
        );
        let output = fixture.run(&["daemon", "start"]);
        assert!(!output.status.success(), "{}", output_text(&output));
        assert_eq!(fixture.profile_bytes(), invalid);
        assert!(!fixture.path("run/daemon.json").exists());
    }
    let new_address = free_address();
    fixture.success(&[
        "daemon",
        "start",
        "--reset-serve-options",
        "--listen",
        &new_address,
    ]);
    assert_eq!(fixture.profile()["schema_version"], 1);
    assert_eq!(fixture.profile()["listen"], new_address);
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn corrupt_profile_does_not_prevent_stop_or_prepare_update_of_a_healthy_runtime() {
    let _lock = server_lock();
    for action in ["stop", "prepare-update"] {
        let mut fixture = Fixture::new();
        let address = free_address();
        let started = fixture.start(&address);
        assert_eq!(started["healthy"], true, "{started}");
        let invalid = b"{invalid-profile-written-while-running";
        fs::write(fixture.path("daemon/serve_profile.json"), invalid).unwrap();
        let status = fixture.run(&["daemon", "status"]);
        assert!(
            !status.status.success(),
            "status must reject the corrupt profile: {}",
            output_text(&status)
        );
        assert_eq!(fixture.profile_bytes(), invalid);
        let stopped = fixture.success(&["daemon", action]);
        assert_eq!(stopped["state"], "stopped", "{stopped}");
        assert_eq!(stopped["healthy"], false, "{stopped}");
        assert_eq!(
            stopped["desired_running"],
            action == "prepare-update",
            "{stopped}"
        );
        for name in ["run/daemon.json", "run/holon.pid", "run/control.sock"] {
            assert!(!fixture.path(name).exists(), "{action} retained {name}");
        }
        assert!(
            TcpListener::bind(&address).is_ok(),
            "{action} left the daemon listening"
        );
        assert_eq!(
            fixture.profile_bytes(),
            invalid,
            "{action} must not repair or overwrite the corrupt profile"
        );
        fixture.assert_no_secrets();
    }
}

#[test]
fn corrupt_profile_does_not_allow_stop_or_prepare_update_of_a_foreign_runtime() {
    let _lock = server_lock();
    let mut runtime = Fixture::new();
    let address = free_address();
    let started = runtime.start(&address);
    let runtime_profile = runtime.profile_bytes();
    let metadata = fs::read(runtime.path("run/daemon.json")).unwrap();
    let mut requester = Fixture::new();
    fs::create_dir_all(requester.path("daemon")).unwrap();
    let invalid = b"{invalid-profile";
    fs::write(requester.path("daemon/serve_profile.json"), invalid).unwrap();
    let runtime_socket = runtime.path("run/control.sock");
    let wrong_socket = requester.path("run/missing.sock");

    for action in ["stop", "prepare-update"] {
        // A real compatible Holon at another home must not become our target.
        let foreign_home = requester.run_env(
            &["daemon", action],
            &[("HOLON_SOCKET_PATH", runtime_socket.to_str().unwrap())],
        );
        // Even matching home/PID metadata must not override the requested socket.
        let foreign_socket = runtime.run_env(
            &["daemon", action],
            &[("HOLON_SOCKET_PATH", wrong_socket.to_str().unwrap())],
        );
        for output in [foreign_home, foreign_socket] {
            assert!(!output.status.success(), "{}", output_text(&output));
            assert!(
                output_text(&output).contains("different home or control socket"),
                "{}",
                output_text(&output)
            );
        }
        let status = runtime.success(&["daemon", "status"]);
        assert_eq!(status["pid"], started["pid"], "{status}");
        assert_eq!(status["healthy"], true, "{status}");
        assert_eq!(runtime.profile_bytes(), runtime_profile);
        assert_eq!(requester.profile_bytes(), invalid);
        assert_eq!(fs::read(runtime.path("run/daemon.json")).unwrap(), metadata);
        assert!(runtime_socket.exists());
        assert!(!requester.path("run/daemon.json").exists());
    }
    runtime.stop();
    runtime.assert_no_secrets();
    requester.assert_no_secrets();
}

#[test]
fn missing_token_file_and_external_token_dependency_never_downgrade_loopback() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    fixture.stop();
    let original = fixture.profile_bytes();
    fs::remove_file(fixture.path("control.token")).unwrap();
    let output = fixture.run(&["daemon", "start"]);
    assert!(!output.status.success(), "{}", output_text(&output));
    assert_eq!(fixture.profile_bytes(), original);
    assert!(!fixture.path("run/daemon.json").exists());
    fixture.success(&["daemon", "start", "--token", TOKEN]);
    fixture.stop();
    let external = fixture.profile_bytes();
    assert!(
        String::from_utf8_lossy(&external).contains("external_required"),
        "inline token must leave a dependency, never a persisted secret"
    );
    let status = fixture.success(&["daemon", "status"]);
    assert_eq!(status["desired_serve"]["source"], "profile", "{status}");
    for args in [
        vec!["daemon", "start"],
        vec!["daemon", "restart", "--access", "local"],
        vec!["serve"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{}", output_text(&output));
        assert_eq!(fixture.profile_bytes(), external);
        assert!(!fixture.path("run/daemon.json").exists());
    }
    fixture.success_env(
        &["daemon", "start"],
        &[("HOLON_CONTROL_TOKEN", OTHER_TOKEN)],
    );
    fixture.success(&["daemon", "status"]);
    fixture.stop();
    let output = fixture.run(&["daemon", "start"]);
    assert!(!output.status.success(), "{}", output_text(&output));
    assert_eq!(fixture.profile_bytes(), external);
    fixture.assert_no_secrets();
}

#[test]
fn bind_failure_retains_accepted_intent_for_the_next_start() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    fixture.start(&address);
    fixture.stop();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let desired_address = occupied.local_addr().unwrap().to_string();
    let output = fixture.run(&[
        "daemon",
        "start",
        "--listen",
        &desired_address,
        "--start-timeout",
        "3",
    ]);
    assert!(!output.status.success(), "{}", output_text(&output));
    assert_eq!(fixture.profile()["listen"], desired_address);
    assert_eq!(fixture.profile()["desktop_integration"], false);
    drop(occupied);
    let status = fixture.success(&["daemon", "start"]);
    assert_eq!(status["http_addr"], desired_address, "{status}");
    fixture.stop();
    fixture.assert_no_secrets();
}

#[test]
fn idempotent_start_and_rejected_mismatch_or_foreign_runtime_do_not_rewrite_profile() {
    let _lock = server_lock();
    let mut fixture = Fixture::new();
    let address = free_address();
    let started = fixture.start(&address);
    let original = fixture.profile_bytes();
    let adopted = fixture.success(&["daemon", "start"]);
    assert_eq!(adopted["process_created"], false, "{adopted}");
    assert_eq!(adopted["pid"], started["pid"], "{adopted}");
    assert_eq!(fixture.profile_bytes(), original);
    let other_address = free_address();
    let output = fixture.run(&["daemon", "start", "--listen", &other_address]);
    assert!(!output.status.success(), "{}", output_text(&output));
    assert_eq!(fixture.profile_bytes(), original);
    assert_eq!(
        fixture.success(&["daemon", "status"])["pid"],
        started["pid"]
    );
    for args in [
        vec!["daemon", "restart", "--token-file", "missing.token"],
        vec!["daemon", "restart", "--listen", "invalid-listen-address"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{}", output_text(&output));
        assert_eq!(
            fixture.profile_bytes(),
            original,
            "rejected restart must not change the accepted profile"
        );
        let status = fixture.success(&["daemon", "status"]);
        assert_eq!(
            status["pid"], started["pid"],
            "rejected restart must not stop the existing process: {status}"
        );
        assert_eq!(status["healthy"], true, "{status}");
    }

    let mut foreign = Fixture::new();
    foreign.start(&other_address);
    foreign.stop();
    let foreign_profile = foreign.profile_bytes();
    let socket = fixture.path("run/control.sock");
    let output = foreign.run_env(
        &["daemon", "start", "--listen", &address],
        &[("HOLON_SOCKET_PATH", socket.to_str().unwrap())],
    );
    assert!(!output.status.success(), "{}", output_text(&output));
    assert_eq!(foreign.profile_bytes(), foreign_profile);
    assert_eq!(fixture.profile_bytes(), original);
    assert_eq!(
        fixture.success(&["daemon", "status"])["pid"],
        started["pid"]
    );
    fixture.stop();
    fixture.assert_no_secrets();
    foreign.assert_no_secrets();
}
