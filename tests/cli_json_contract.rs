//! CLI JSON output contract tests.
//!
//! These tests lock the initial stable-candidate script-facing JSON surfaces.
//! They intentionally exercise the compiled binary so stdout/stderr routing,
//! pretty-printing, and persisted config paths are covered together.

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::{mpsc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("CARGO_BIN_EXE_holon should be set for integration tests")
}

fn isolated_holon_command(home: &tempfile::TempDir) -> Command {
    let mut command = Command::new(holon_bin());
    command
        .env("HOLON_HOME", home.path())
        .env("HOLON_AGENT_ID", "main")
        .env("HOLON_MODEL", "openai/gpt-5.4")
        .env("HOLON_HTTP_ADDR", "127.0.0.1:9")
        .env(
            "HOLON_SOCKET_PATH",
            home.path().join("run").join("missing.sock"),
        )
        .env_remove("HOLON_CONTROL_TOKEN")
        .env_remove("HOLON_CONTROL_AUTH_MODE")
        .env_remove("RUST_LOG");
    command
}

struct ServeChild {
    child: Child,
    stderr: mpsc::Receiver<String>,
}

impl ServeChild {
    fn diagnostics(&mut self) -> String {
        let status = self.child.try_wait();
        let stderr = self
            .stderr
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or_else(|error| format!("stderr capture unavailable: {error}"));
        format!("child status: {status:?}\nstderr:\n{stderr}")
    }
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Serialize tests that spawn `holon serve`: concurrent instrumented servers
/// (for example under llvm-cov) compete for CPU on CI runners and can exceed
/// the startup deadline even though a single server starts well within it.
fn serve_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn spawn_local_serve(home: &tempfile::TempDir) -> (ServeChild, String) {
    let mut command = isolated_holon_command(home);
    command.args(["serve", "--listen", "127.0.0.1:0"]);
    spawn_serve_command(command)
}

fn forward_serve_stdout(reader: impl BufRead, line_tx: mpsc::Sender<std::io::Result<String>>) {
    for line in reader.lines() {
        // Keep the pipe open after the startup receiver has its listener address.
        let _ = line_tx.send(line);
    }
}

#[test]
fn serve_stdout_reader_drains_after_startup_receiver_is_dropped() {
    let output = b"Holon listening on 127.0.0.1:1234\nlocal listener\nUnix socket\n";
    let mut reader = std::io::Cursor::new(output);
    let (line_tx, line_rx) = mpsc::channel();
    drop(line_rx);
    forward_serve_stdout(&mut reader, line_tx);
    assert_eq!(reader.position(), output.len() as u64);
}

fn spawn_serve_command(mut command: Command) -> (ServeChild, String) {
    let mut child = command
        .env("HOLON_PRE_SERVER_RUNTIME_PREPARED", "1")
        .env("OPENAI_API_KEY", "test-openai-api-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn holon serve");
    let stdout = child.stdout.take().expect("serve stdout should be piped");
    let stderr = child.stderr.take().expect("serve stderr should be piped");
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        forward_serve_stdout(BufReader::new(stdout), line_tx);
    });
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut captured = String::new();
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    captured.push_str(&line);
                    captured.push('\n');
                }
                Err(error) => {
                    captured.push_str(&format!("failed to read serve stderr: {error}\n"));
                    break;
                }
            }
        }
        let _ = stderr_tx.send(captured);
    });
    let mut serve = ServeChild {
        child,
        stderr: stderr_rx,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut addr = None;
    while Instant::now() < deadline {
        if let Some(status) = serve.child.try_wait().expect("poll holon serve") {
            panic!(
                "holon serve exited before printing listening address: {status}\n{}",
                serve.diagnostics()
            );
        }
        match line_rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(line)) => {
                if line.starts_with("Holon listening on ") {
                    addr = Some(
                        line.trim()
                            .trim_start_matches("Holon listening on ")
                            .to_string(),
                    );
                }
                if line.starts_with("Holon listening on ") && addr.is_some() {
                    let Some(addr) = addr.clone() else {
                        unreachable!("addr should be set")
                    };
                    return (serve, addr);
                }
            }
            Ok(Err(error)) => panic!("read serve stdout: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "holon serve stdout closed before listening address was printed\n{}",
                    serve.diagnostics()
                )
            }
        };
    }
    let _ = serve.child.kill();
    panic!(
        "timed out waiting for holon serve to print listening address\n{}",
        serve.diagnostics()
    );
}

#[cfg(unix)]
#[test]
fn tailscale_serve_reenable_uses_bound_loopback_for_each_primary() {
    use std::{net::UdpSocket, os::unix::fs::PermissionsExt};

    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut primaries = vec!["127.0.0.1".to_owned(), "0.0.0.0".to_owned()];
    let route = UdpSocket::bind("0.0.0.0:0").unwrap();
    if route.connect("192.0.2.1:9").is_ok() {
        let ip = route.local_addr().unwrap().ip();
        if !ip.is_loopback() && !ip.is_unspecified() {
            primaries.push(ip.to_string());
        }
    }
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let stub_dir = tempfile::tempdir().unwrap();
    let stub = stub_dir.path().join("tailscale");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
set -eu
case "$*" in
  "status --json")
    printf '%s\n' '{"BackendState":"Running","Self":{"DNSName":"test.example.ts.net."}}' ;;
  "serve status --json")
    if [ -f "$TAILSCALE_TEST_HOME/serve.json" ]; then
      cat "$TAILSCALE_TEST_HOME/serve.json"
    else
      printf '%s\n' '{}'
    fi ;;
  "serve --https=443 --set-path=/ off")
    printf '%s\n' '{}' > "$TAILSCALE_TEST_HOME/serve.json"
    printf '%s\n' "$*" >> "$TAILSCALE_TEST_HOME/commands" ;;
  "serve --bg --https=443 --set-path=/"*)
    printf '{"Web":{"test.example.ts.net:443":{"Handlers":{"/":{"Proxy":"%s"}}}}}\n' "$5" > "$TAILSCALE_TEST_HOME/serve.json"
    printf '%s\n' "$*" >> "$TAILSCALE_TEST_HOME/commands" ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
    for primary in primaries {
        let home = tempfile::tempdir().unwrap();
        let listen = format!("{primary}:0");
        let mut command = isolated_holon_command(&home);
        command
            .args(["serve", "--listen", &listen])
            .env("HOLON_CONTROL_AUTH_MODE", "required")
            .env("HOLON_CONTROL_TOKEN", "serve-test-token")
            .env("TAILSCALE_BINARY_PATH", &stub)
            .env("TAILSCALE_TEST_HOME", home.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stub_dir.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        let (mut serve, address) = spawn_serve_command(command);
        let port = address.parse::<std::net::SocketAddr>().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let endpoint = format!("{base}/api/control/network/tailscale/serve");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if serve.child.try_wait().expect("poll holon serve").is_some() {
                panic!(
                    "daemon exited before becoming healthy: primary={listen}, bound={address}\n{}",
                    serve.diagnostics()
                );
            }
            match client.get(&endpoint).send() {
                Ok(response) if response.status() == 401 => break,
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                result => panic!(
                    "daemon did not become healthy: primary={listen}, bound={address}, result={result:?}\n{}",
                    serve.diagnostics()
                ),
            }
        }
        assert_eq!(
            client
                .post(format!("{endpoint}/enable"))
                .send()
                .unwrap()
                .status(),
            401
        );
        assert!(!home.path().join("commands").exists());
        for enabled in [true, false, true] {
            let action = if enabled { "enable" } else { "disable" };
            let response = client
                .post(format!("{endpoint}/{action}"))
                .bearer_auth("serve-test-token")
                // The mutation invokes the CLI to inspect, apply, and verify the rule.
                .timeout(Duration::from_secs(30))
                .send()
                .unwrap_or_else(|error| panic!("{primary}: {action}: {error:?}"));
            assert_eq!(response.status(), 200, "{primary}: {action}");
            let status: Value = response.json().unwrap();
            assert_eq!(status["desired_enabled"], enabled);
            assert_eq!(status["serving"], enabled);
            assert_eq!(status["conflict"], false);
        }
        let commands = std::fs::read_to_string(home.path().join("commands")).unwrap();
        assert_eq!(
            commands.lines().collect::<Vec<_>>(),
            vec![
                format!("serve --bg --https=443 --set-path=/ {base}"),
                "serve --https=443 --set-path=/ off".to_owned(),
                format!("serve --bg --https=443 --set-path=/ {base}"),
            ]
        );
        let stored: Value =
            serde_json::from_slice(&std::fs::read(home.path().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(stored["tailscale_serve_desired_enabled"], true);
    }
}

fn run_json(home: &tempfile::TempDir, args: &[&str]) -> Value {
    let output = isolated_holon_command(home)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run holon {args:?}: {error}"));
    assert_success(output, args)
}

fn run_json_with_env(home: &tempfile::TempDir, args: &[&str], envs: &[(&str, &str)]) -> Value {
    let mut command = isolated_holon_command(home);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run holon {args:?}: {error}"));
    assert_success(output, args)
}

fn assert_success(output: Output, args: &[&str]) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "holon {args:?} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.is_empty(), "stderr should stay empty: {stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout should be JSON for {args:?}: {error}\n{stdout}"))
}

fn run_json_with_stderr(home: &tempfile::TempDir, args: &[&str]) -> (Value, String) {
    let output = isolated_holon_command(home)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run holon {args:?}: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "holon {args:?} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout should be JSON for {args:?}: {error}\n{stdout}"));
    (value, stderr)
}

fn run_failure_with_env(
    home: &tempfile::TempDir,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (String, String) {
    let mut command = isolated_holon_command(home);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run holon {args:?}: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(1),
        "holon {args:?} should fail\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

#[test]
fn config_schema_json_entries_keep_stable_shape() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let value = run_json(&home, &["config", "schema"]);
    let entries = value.as_array().expect("schema output should be an array");
    assert!(
        entries.len() >= 40,
        "schema should expose the known config surface, got {} entries",
        entries.len()
    );

    let allowed_entry_keys =
        BTreeSet::from(["allowed_values", "default", "description", "key", "kind"]);
    let mut keys = BTreeSet::new();
    for entry in entries {
        let object = entry.as_object().expect("schema entry should be an object");
        for key in object.keys() {
            assert!(
                allowed_entry_keys.contains(key.as_str()),
                "unexpected config schema field {key:?} in {entry}"
            );
        }
        assert!(
            object
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "schema entry should include a non-empty key: {entry}"
        );
        assert!(
            object
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "schema entry should include a non-empty kind: {entry}"
        );
        assert!(
            object
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "schema entry should include a non-empty description: {entry}"
        );
        assert!(
            object.contains_key("default"),
            "schema entry should include default: {entry}"
        );
        keys.insert(object["key"].as_str().unwrap().to_string());
    }

    for expected in [
        "model.default",
        "model.fallbacks",
        "runtime.max_output_tokens",
        "tui.alternate_screen",
        "web.fetch.enabled",
        "web.search.provider",
    ] {
        assert!(
            keys.contains(expected),
            "schema output should include stable config key {expected}"
        );
    }
}

#[test]
fn config_provider_headers_updates_preserve_provider_fields_and_redact_values() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.json");
    let provider = json!({
        "transport": "openai_responses",
        "base_url": "https://proxy.example.invalid/v1",
        "auth": {"source":"env", "kind":"api_key", "env":"PROXY_API_KEY"},
        "reasoning_effort": "medium",
        "cache_capabilities": {"cache_control":false},
        "headers": {"x-existing":"keep"},
        "endpoints": {
            "coding": {
                "transport":"openai_chat_completions",
                "base_url":"https://coding.example.invalid/v1",
                "auth":{"source":"none", "kind":"none"},
                "headers":{"x-existing":"endpoint"}
            },
            "other": {"base_url":"https://other.example.invalid/v1"}
        },
        "plans": {"coding":{"endpoint":"coding"}}
    });
    let unrelated = json!({
        "transport":"anthropic_messages",
        "base_url":"https://unrelated.example.invalid",
        "auth":{"source":"none","kind":"none"},
        "headers":{"x-unrelated":"do-not-forward"}
    });
    std::fs::write(
        &path,
        json!({"providers":{"proxy":provider.clone(), "unrelated":unrelated.clone()}}).to_string(),
    )
    .unwrap();
    let secret = "random-secret-like-cli-header-value";
    let assignment = format!("X-Trace={secret}=suffix");
    let (output, stderr) = run_json_with_stderr(
        &home,
        &[
            "config",
            "providers",
            "headers",
            "set",
            "proxy",
            "--header",
            &assignment,
            "--header",
            "X-Empty=",
        ],
    );
    assert_eq!(
        output,
        json!({
            "applied_via":"offline_store",
            "provider":"proxy",
            "endpoint":null,
            "header_names":["x-empty","x-trace"],
            "status":"set"
        })
    );
    assert!(!output.to_string().contains(secret));
    assert!(!stderr.contains(secret));
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["providers"]["unrelated"], unrelated);
    assert_eq!(
        stored["providers"]["proxy"]["headers"]["x-trace"],
        format!("{secret}=suffix")
    );
    assert_eq!(stored["providers"]["proxy"]["headers"]["x-empty"], "");
    let mut retained = stored["providers"]["proxy"].clone();
    retained["headers"] = provider["headers"].clone();
    assert_eq!(retained, provider);

    run_json(
        &home,
        &[
            "config",
            "providers",
            "headers",
            "unset",
            "proxy",
            "--endpoint",
            "coding",
            "--name",
            "X-Existing",
        ],
    );
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut retained = stored["providers"]["proxy"].clone();
    assert!(retained["endpoints"]["coding"].get("headers").is_none());
    retained["headers"] = provider["headers"].clone();
    retained["endpoints"]["coding"]["headers"] = provider["endpoints"]["coding"]["headers"].clone();
    assert_eq!(retained, provider);
}

#[test]
fn config_provider_headers_serialize_with_other_config_writers() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.json");
    let provider = json!({
        "transport":"openai_chat_completions",
        "base_url":"https://proxy.example.invalid/v1",
        "auth":{"source":"none","kind":"none"},
        "headers":{"x-remove":"old"},
        "endpoints":{"coding":{"base_url":"https://coding.example.invalid/v1"}}
    });
    std::fs::write(
        &path,
        json!({"providers":{"alpha":provider,"beta":provider,"obsolete":provider}}).to_string(),
    )
    .unwrap();
    let transaction = holon::config::PersistedConfigTransaction::begin(&path).unwrap();
    let commands: &[&[&str]] = &[
        &[
            "config",
            "providers",
            "headers",
            "set",
            "alpha",
            "--header",
            "X-First=one",
        ],
        &[
            "config",
            "providers",
            "headers",
            "set",
            "beta",
            "--header",
            "X-Second=two",
        ],
        &[
            "config",
            "providers",
            "headers",
            "set",
            "alpha",
            "--endpoint",
            "coding",
            "--header",
            "X-Endpoint=three",
        ],
        &[
            "config",
            "providers",
            "headers",
            "unset",
            "alpha",
            "--name",
            "X-Remove",
        ],
        &["config", "set", "web.fetch.enabled", "false"],
        &["config", "providers", "remove", "obsolete"],
    ];
    let mut children = commands
        .iter()
        .map(|args| {
            isolated_holon_command(&home)
                .args(*args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    thread::sleep(Duration::from_millis(200));
    let completed_before_unlock = children
        .iter_mut()
        .any(|child| child.try_wait().unwrap().is_some());
    let mut config = transaction.load().unwrap();
    config.vision.default = Some("openai/gpt-5.4".into());
    transaction.save(&config).unwrap();
    drop(transaction);
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap();
    }
    assert!(
        !completed_before_unlock,
        "config writers must wait for the transaction"
    );
    let stored: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(stored["providers"]["alpha"]["headers"]["x-first"], "one");
    assert!(stored["providers"]["alpha"]["headers"]
        .get("x-remove")
        .is_none());
    assert_eq!(stored["providers"]["beta"]["headers"]["x-second"], "two");
    assert_eq!(
        stored["providers"]["alpha"]["endpoints"]["coding"]["headers"]["x-endpoint"],
        "three"
    );
    assert_eq!(stored["web"]["fetch"]["enabled"], false);
    assert_eq!(stored["vision"]["default"], "openai/gpt-5.4");
    assert!(stored["providers"].get("obsolete").is_none());
}

#[test]
fn config_provider_headers_reject_invalid_batches_without_writing() {
    let home = tempfile::tempdir().unwrap();
    run_json(
        &home,
        &[
            "config",
            "providers",
            "set",
            "proxy",
            "--transport",
            "openai_responses",
            "--base-url",
            "https://proxy.example.invalid/v1",
        ],
    );
    let path = home.path().join("config.json");
    let before = std::fs::read(&path).unwrap();
    let secret = "random-secret-like-rejected-header-value";
    let reserved = format!("Authorization={secret}");
    let injected = format!("X-Injected={secret}\r\nother: injected");
    let too_large = format!("X-Large={secret}{}", "v".repeat(4097));
    for extra in [
        vec!["--header", "x-valid=valid", "--header", reserved.as_str()],
        vec!["--header", "X-Duplicate=one", "--header", "x-duplicate=two"],
        vec!["--header", injected.as_str()],
        vec!["--header", too_large.as_str()],
        vec!["--header", "missing-delimiter"],
        vec!["--endpoint", "missing", "--header", "x-valid=valid"],
        vec![
            "--endpoint",
            "invalid@endpoint",
            "--header",
            "x-valid=valid",
        ],
    ] {
        let mut args = vec!["config", "providers", "headers", "set", "proxy"];
        args.extend(extra);
        let (stdout, stderr) = run_failure_with_env(&home, &args, &[]);
        assert!(stdout.is_empty());
        assert!(!stderr.contains(secret));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    for args in [
        vec![
            "config",
            "providers",
            "headers",
            "unset",
            "proxy",
            "--name",
            "X-One",
            "--name",
            "x-one",
        ],
        vec![
            "config",
            "providers",
            "headers",
            "set",
            "missing",
            "--header",
            "x-one=value",
        ],
        vec![
            "config",
            "providers",
            "headers",
            "set",
            "opencode-go",
            "--header",
            "x-opencode-session=not-allowed",
        ],
    ] {
        let (stdout, _) = run_failure_with_env(&home, &args, &[]);
        assert!(stdout.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn config_provider_headers_materialize_builtin_auth_and_only_return_names() {
    let home = tempfile::tempdir().unwrap();
    run_json(
        &home,
        &[
            "config",
            "providers",
            "headers",
            "set",
            "openai",
            "--header",
            "User-Agent=non-sensitive-custom-client",
        ],
    );
    let path = home.path().join("config.json");
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["providers"]["openai"]["auth"]["source"], "env");
    assert_eq!(stored["providers"]["openai"]["auth"]["kind"], "api_key");
    assert_eq!(
        stored["providers"]["openai"]["auth"]["env"],
        "OPENAI_API_KEY"
    );
    let view = run_json(&home, &["config", "providers", "get", "openai"]);
    assert_eq!(view["header_names"], json!(["user-agent"]));
    assert!(!view.to_string().contains("non-sensitive-custom-client"));
}

#[test]
fn config_provider_remove_json_contract_is_stable() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");

    let missing = run_json(&home, &["config", "providers", "remove", "script-test"]);
    assert_eq!(
        missing,
        json!({
            "applied_via": "offline_store",
            "provider": "script-test",
            "status": "not_configured"
        })
    );

    let _set = run_json(
        &home,
        &[
            "config",
            "providers",
            "set",
            "script-test",
            "--transport",
            "anthropic_messages",
            "--base-url",
            "https://example.invalid",
        ],
    );
    let removed = run_json(&home, &["config", "providers", "remove", "script-test"]);
    assert_eq!(
        removed,
        json!({
            "applied_via": "offline_store",
            "provider": "script-test",
            "status": "removed"
        })
    );
}

#[test]
fn config_credentials_json_contract_is_stable_and_redacted() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");

    let set = run_json(
        &home,
        &[
            "config",
            "credentials",
            "set",
            "demo",
            "--kind",
            "api_key",
            "--material",
            "super-secret",
        ],
    );
    assert_eq!(
        set,
        json!({
            "applied_via": "offline_store",
            "credential": {
                "configured": true,
                "kind": "api_key",
                "profile": "demo"
            }
        })
    );

    let list = run_json(&home, &["config", "credentials", "list"]);
    assert_eq!(
        list,
        json!([
            {
                "configured": true,
                "kind": "api_key",
                "profile": "demo"
            }
        ])
    );
    assert!(
        !list.to_string().contains("super-secret"),
        "credential list JSON must not expose credential material"
    );

    let removed = run_json(&home, &["config", "credentials", "remove", "demo"]);
    assert_eq!(
        removed,
        json!({
            "applied_via": "offline_store",
            "credential": {
                "configured": false,
                "kind": "api_key",
                "profile": "demo"
            }
        })
    );

    let empty_list = run_json(&home, &["config", "credentials", "list"]);
    assert_eq!(empty_list, json!([]));
}

#[test]
fn config_set_unset_reports_offline_application_path_on_stderr() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");

    let (set, set_stderr) =
        run_json_with_stderr(&home, &["config", "set", "model.default", "openai/gpt-4.1"]);
    assert_eq!(set, json!("openai@default/gpt-4.1"));
    assert_eq!(set_stderr, "applied_via=offline_store\n");

    let get = run_json(&home, &["config", "get", "model.default"]);
    assert_eq!(get, json!("openai@default/gpt-4.1"));

    let (unset, unset_stderr) = run_json_with_stderr(&home, &["config", "unset", "model.default"]);
    assert_eq!(
        unset,
        json!({
            "key": "model.default",
            "status": "unset"
        })
    );
    assert_eq!(unset_stderr, "applied_via=offline_store\n");
}

#[test]
fn config_set_prefers_running_daemon_runtime_config_api() {
    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let (_serve, addr) = spawn_local_serve(&home);

    let (set, set_stderr) = {
        let mut command = isolated_holon_command(&home);
        command.env("HOLON_HTTP_ADDR", &addr);
        let output = command
            .args(["config", "set", "model.default", "openai/gpt-4.1"])
            .output()
            .expect("run holon config set");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(0),
            "config set failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        (
            serde_json::from_str::<Value>(&stdout).expect("set stdout should be JSON"),
            stderr,
        )
    };
    assert_eq!(set, json!("openai@default/gpt-4.1"));
    assert!(
        set_stderr.contains("applied_via=daemon_api\n"),
        "stderr should report daemon application path: {set_stderr}"
    );

    let get = run_json_with_env(
        &home,
        &["config", "get", "model.default"],
        &[("HOLON_HTTP_ADDR", &addr)],
    );
    assert_eq!(get, json!("openai@default/gpt-4.1"));

    let (unset, unset_stderr) = {
        let mut command = isolated_holon_command(&home);
        command.env("HOLON_HTTP_ADDR", &addr);
        let output = command
            .args(["config", "unset", "model.default"])
            .output()
            .expect("run holon config unset");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(0),
            "config unset failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        (
            serde_json::from_str::<Value>(&stdout).expect("unset stdout should be JSON"),
            stderr,
        )
    };
    assert_eq!(
        unset,
        json!({
            "key": "model.default",
            "status": "unset"
        })
    );
    assert!(
        unset_stderr.contains("applied_via=daemon_api\n"),
        "stderr should report daemon application path: {unset_stderr}"
    );
}

#[test]
fn config_set_surfaces_daemon_rejection_reason() {
    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let (_serve, addr) = spawn_local_serve(&home);

    let (stdout, stderr) = run_failure_with_env(
        &home,
        &["config", "set", "home_dir", "/tmp/other-home"],
        &[("HOLON_HTTP_ADDR", &addr)],
    );

    assert!(
        stdout.is_empty(),
        "failed config set should not emit JSON stdout"
    );
    assert!(
        stderr.contains("daemon rejected runtime config update for home_dir"),
        "stderr should name the rejected key: {stderr}"
    );
    assert!(
        stderr.contains("unsupported or startup-only config key"),
        "stderr should surface daemon-provided reason: {stderr}"
    );
}

#[test]
fn config_set_rejects_runtime_scheduler_while_daemon_is_running() {
    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let (_serve, addr) = spawn_local_serve(&home);

    let (stdout, stderr) = run_failure_with_env(
        &home,
        &["config", "set", "runtime.scheduler", "legacy"],
        &[("HOLON_HTTP_ADDR", &addr)],
    );

    assert!(
        stdout.is_empty(),
        "failed config set should not emit JSON stdout"
    );
    assert!(
        stderr.contains("daemon rejected runtime config update for runtime.scheduler"),
        "stderr should name the startup-only key: {stderr}"
    );
    assert!(
        stderr.contains("unsupported or startup-only config key"),
        "stderr should explain that scheduler selection requires restart: {stderr}"
    );
}

#[test]
fn onboard_json_contract_is_secret_safe_and_actionable() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let _set = run_json(
        &home,
        &[
            "config",
            "credentials",
            "set",
            "demo",
            "--kind",
            "api_key",
            "--material",
            "super-secret",
        ],
    );

    let value = run_json(&home, &["onboard", "--json"]);
    assert_eq!(value["schema_version"], json!(1));
    assert!(
        value["sections"].as_array().is_some_and(|sections| {
            sections
                .iter()
                .any(|section| section["id"] == "model_provider")
        }),
        "onboard report should include model provider diagnostics: {value}"
    );
    let model_provider = value["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["id"] == "model_provider")
        })
        .expect("onboard report should include a model provider section");
    assert_eq!(
        model_provider["actions"][0]["command"],
        json!(["holon", "onboard"]),
        "missing provider diagnostics should point back to the complete onboarding flow: {value}"
    );
    match value.get("next_actions") {
        Some(next_actions) => assert!(
            next_actions.as_array().is_some(),
            "onboard report next_actions should be an array when present: {value}"
        ),
        None => assert_eq!(
            value["status"],
            json!("configured"),
            "onboard report should omit next_actions only when fully configured: {value}"
        ),
    }
    assert!(
        !value.to_string().contains("super-secret"),
        "onboard JSON must not expose credential material"
    );
}

#[test]
fn agent_delete_then_recreate_reports_incarnation_json() {
    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let (_serve, addr) = spawn_local_serve(&home);
    let envs = [("HOLON_HTTP_ADDR", addr.as_str())];

    // Preserve the legacy-install fixture: the configured `main` agent
    // already exists, so the created agent is not selected as the default.
    run_json_with_env(&home, &["agent", "create", "main"], &envs);
    let created = run_json_with_env(&home, &["agent", "create", "reborn-cli"], &envs);
    assert_eq!(created["identity"]["incarnation"], json!(1));

    let deleted = run_json_with_env(
        &home,
        &["agent", "delete", "reborn-cli", "-y", "--json"],
        &envs,
    );
    assert_eq!(deleted["job"]["status"], json!("pending"));
    // `--json` prints the initial job; poll the idempotent delete surface
    // until the background coordinator completes the job.
    let mut completed = false;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let polled = run_json_with_env(
            &home,
            &["agent", "delete", "reborn-cli", "-y", "--json"],
            &envs,
        );
        if polled["job"]["status"] == json!("completed") {
            assert_eq!(polled["identity"]["status"], json!("deleted"));
            completed = true;
            break;
        }
    }
    assert!(completed, "deletion job should complete: {deleted}");

    // The released id recreates as a new incarnation.
    let recreated = run_json_with_env(&home, &["agent", "create", "reborn-cli"], &envs);
    assert_eq!(recreated["identity"]["status"], json!("active"));
    assert_eq!(recreated["identity"]["incarnation"], json!(2));
    assert_eq!(recreated["receipt"]["created"], json!(true));
}

#[test]
fn onboard_defaults_to_scriptable_json_when_not_a_tty() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");

    let value = run_json(&home, &["onboard"]);

    assert_eq!(value["schema_version"], json!(1));
    assert!(
        value["sections"].as_array().is_some(),
        "non-TTY onboard output should remain scriptable JSON: {value}"
    );
}

#[test]
fn agent_rename_reports_detail_json_and_readable_errors() {
    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let (_serve, addr) = spawn_local_serve(&home);
    let envs = [("HOLON_HTTP_ADDR", addr.as_str())];

    // Preserve the legacy-install fixture so `main` remains the configured
    // default while the named-agent rename contract is exercised.
    run_json_with_env(&home, &["agent", "create", "main"], &envs);
    let created = run_json_with_env(&home, &["agent", "create", "rename-one"], &envs);
    assert_eq!(created["identity"]["agent_id"], json!("rename-one"));

    let renamed = run_json_with_env(
        &home,
        &["agent", "rename", "rename-one", "--name", "First Name"],
        &envs,
    );
    assert_eq!(renamed["identity"]["agent_id"], json!("rename-one"));
    assert_eq!(renamed["name"], json!("First Name"));
    assert_eq!(renamed["identity"]["name"], json!("First Name"));
    assert!(
        renamed["display_name"]
            .as_str()
            .is_some_and(|value| value.contains("First Name")),
        "rename should echo the updated agent detail: {renamed}"
    );

    // Duplicate names are rejected with the server's typed conflict message.
    run_json_with_env(&home, &["agent", "create", "rename-two"], &envs);
    let (conflict_stdout, conflict_stderr) = run_failure_with_env(
        &home,
        &["agent", "rename", "rename-two", "--name", "First Name"],
        &envs,
    );
    assert!(
        conflict_stdout.is_empty(),
        "failed rename should not emit JSON stdout: {conflict_stdout}"
    );
    assert!(
        conflict_stderr.contains("already in use"),
        "conflict stderr should stay readable: {conflict_stderr}"
    );

    // The configured default agent is renameable while keeping its stable id.
    let default_renamed = run_json_with_env(
        &home,
        &["agent", "rename", "main", "--name", "Default Renamed"],
        &envs,
    );
    assert_eq!(default_renamed["identity"]["agent_id"], json!("main"));
    assert_eq!(
        default_renamed["identity"]["name"],
        json!("Default Renamed")
    );
}

#[test]
fn agent_model_get_uses_the_dedicated_model_endpoint() {
    let _serve_guard = serve_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let (_serve, addr) = spawn_local_serve(&home);
    let envs = [("HOLON_HTTP_ADDR", addr.as_str())];

    // The configured default agent and a named agent both exist.
    run_json_with_env(&home, &["agent", "create", "main"], &envs);
    let created = run_json_with_env(&home, &["agent", "create", "model-cli"], &envs);
    assert_eq!(created["receipt"]["created"], json!(true));

    let metrics = || {
        reqwest::blocking::Client::new()
            .get(format!("http://{addr}/api/control/runtime/metrics"))
            .send()
            .expect("fetch runtime metrics")
            .text()
            .expect("read runtime metrics")
    };
    let counter = |body: &str, name: &str| -> u64 {
        body.lines()
            .find(|line| line.starts_with(name) && !line.starts_with(&format!("# {name}")))
            .expect(&format!("metric {name} should be rendered"))
            .rsplit(' ')
            .next()
            .expect("metric value")
            .parse()
            .expect("metric value should be numeric")
    };
    const MODEL_ROUTE: &str = "holon_http_json_agents_agent_id_model_count_total";
    const STATUS_ROUTE: &str = "holon_http_json_agents_agent_id_status_count_total";

    let before = metrics();
    let status_before = counter(&before, STATUS_ROUTE);

    // Real CLI reads: default agent selection and an explicit target.
    let default_model = run_json_with_env(&home, &["agent", "model", "get"], &envs);
    let named_model = run_json_with_env(&home, &["agent", "model", "get", "model-cli"], &envs);

    let after = metrics();
    assert_eq!(
        counter(&after, MODEL_ROUTE),
        counter(&before, MODEL_ROUTE) + 2,
        "both CLI reads should hit the dedicated model endpoint"
    );
    assert_eq!(
        counter(&after, STATUS_ROUTE),
        status_before,
        "model get must not request any full or compact status endpoint"
    );

    // Output stays deep-equal to the full summary `.model` object.
    assert!(default_model["effective_model"].is_string());
    assert!(default_model["resolved_policy"].is_object());
    assert_eq!(
        default_model, named_model,
        "both agents share the runtime default model"
    );
    let summary: Value = reqwest::blocking::Client::new()
        .get(format!("http://{addr}/api/agents/main/status?detail=full"))
        .send()
        .expect("fetch full agent status")
        .json()
        .expect("decode full agent status");
    assert_eq!(default_model, summary["model"]);

    // Durable set/clear through the real write surface reads back through the
    // dedicated endpoint.
    let set = run_json_with_env(
        &home,
        &[
            "agent",
            "model",
            "set",
            "openai@default/gpt-5.4",
            "model-cli",
        ],
        &envs,
    );
    assert_eq!(set["ok"], json!(true));
    let overridden = run_json_with_env(&home, &["agent", "model", "get", "model-cli"], &envs);
    assert_eq!(overridden["source"], json!("agent_override"));
    assert_eq!(
        overridden["override_model"],
        json!("openai@default/gpt-5.4")
    );
    let cleared = run_json_with_env(&home, &["agent", "model", "clear", "model-cli"], &envs);
    assert_eq!(cleared["ok"], json!(true));
    let restored = run_json_with_env(&home, &["agent", "model", "get", "model-cli"], &envs);
    assert_eq!(restored["source"], json!("runtime_default"));
    assert!(restored.get("override_model").is_none());
}
