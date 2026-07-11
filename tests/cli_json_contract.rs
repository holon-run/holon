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
    sync::mpsc,
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
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_local_serve(home: &tempfile::TempDir) -> (ServeChild, String) {
    let mut child = isolated_holon_command(home)
        .args(["serve", "--listen", "127.0.0.1:0"])
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
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
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
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut addr = None;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll holon serve") {
            let stderr = stderr_rx
                .recv_timeout(Duration::from_millis(100))
                .unwrap_or_default();
            panic!(
                "holon serve exited before printing listening address: {status}\nstderr:\n{stderr}"
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
                } else if line.starts_with("Holon control socket on ") {
                    let addr = addr.expect("serve should print TCP listener before control socket");
                    return (ServeChild { child }, addr);
                }
                if line.starts_with("Holon listening on ") && addr.is_some() {
                    let Some(addr) = addr.clone() else {
                        unreachable!("addr should be set")
                    };
                    if !cfg!(unix) {
                        return (ServeChild { child }, addr);
                    }
                }
            }
            Ok(Err(error)) => panic!("read serve stdout: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let stderr = stderr_rx
                    .recv_timeout(Duration::from_millis(100))
                    .unwrap_or_default();
                panic!("holon serve stdout closed before listening address was printed\nstderr:\n{stderr}")
            }
        };
    }
    let _ = child.kill();
    let stderr = stderr_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap_or_default();
    panic!("timed out waiting for holon serve to print listening address\nstderr:\n{stderr}");
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
fn onboard_defaults_to_scriptable_json_when_not_a_tty() {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");

    let value = run_json(&home, &["onboard"]);

    assert_eq!(value["schema_version"], json!(1));
    assert!(
        value["sections"].as_array().is_some(),
        "non-TTY onboard output should remain scriptable JSON: {value}"
    );
}
