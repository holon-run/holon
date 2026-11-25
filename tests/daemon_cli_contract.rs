//! Compiled-binary contract fixtures for the shared `holon daemon` envelope.
//!
//! These tests exercise the real binary because the contract covers argv
//! pre-parsing, stdout/stderr routing, exit codes, and secret filtering. Each
//! fixture asserts the parsed single-object stdout, the absence of mixed human
//! output, the stderr policy, and the expected exit code.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use serde_json::{json, Value};

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("CARGO_BIN_EXE_holon should be set for integration tests")
}

struct Fixture {
    home: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated HOLON_HOME");
        fs::create_dir_all(home.path().join("run")).unwrap();
        Self { home }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(holon_bin())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("HOLON_HOME", self.home.path())
            .env("HOLON_WORKSPACE_DIR", self.home.path())
            .env(
                "HOLON_SOCKET_PATH",
                self.home.path().join("run").join("control.sock"),
            )
            .env("HOLON_AGENT_ID", "main")
            .env("HOLON_MODEL", "openai/gpt-5.4")
            .env("OPENAI_API_KEY", "offline-contract-test-key")
            .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
            .env("RUST_LOG", "off")
            .current_dir(self.home.path())
            .args(args)
            .output()
            .expect("run compiled holon")
    }

    /// A persisted `desired_running = true` without a live runtime makes
    /// `daemon status` report the daemon as unavailable.
    fn mark_should_be_running(&self) {
        fs::write(
            self.home.path().join("run").join("desired_state.json"),
            r#"{"desired_running": true}"#,
        )
        .unwrap();
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Parse stdout as exactly one JSON envelope and pin the shared shape.
fn envelope(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stdout.contains("status: ok")
            && !stdout.contains("status: failed")
            && !stdout.contains("status: blocked"),
        "human status output leaked into JSON stdout:\n{stdout}"
    );
    let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not one JSON object: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert_eq!(value["schema_version"], json!(1), "{value}");
    let command = value["command"].as_str().unwrap_or_default();
    assert!(
        command == "daemon" || command.starts_with("daemon."),
        "unexpected command path: {value}"
    );
    assert!(
        matches!(value["status"].as_str(), Some("ok" | "failed" | "blocked")),
        "unexpected status: {value}"
    );
    assert!(
        !stderr.contains("schema_version"),
        "the JSON envelope leaked into stderr:\n{stderr}"
    );
    value
}

#[test]
fn status_and_logs_report_success_envelopes() {
    let fixture = Fixture::new();

    let status = fixture.run(&["daemon", "--json", "status"]);
    assert_eq!(status.status.code(), Some(0), "{}", output_text(&status));
    let status = envelope(&status);
    assert_eq!(status["command"], "daemon.status");
    assert_eq!(status["status"], "ok");
    assert!(status["error"].is_null());
    assert_eq!(status["data"]["state"], "stopped");
    assert_eq!(status["data"]["desired_running"], false);

    // The daemon-level flag is global, so it may follow the subcommand.
    let logs = fixture.run(&["daemon", "logs", "--json", "--tail", "5"]);
    assert_eq!(logs.status.code(), Some(0), "{}", output_text(&logs));
    let logs = envelope(&logs);
    assert_eq!(logs["command"], "daemon.logs");
    assert_eq!(logs["status"], "ok");
    assert!(
        logs["data"].get("tail").is_none() || logs["data"]["tail"] == json!([]),
        "{logs}"
    );
    assert!(logs["data"]["log_path"]
        .as_str()
        .unwrap()
        .ends_with("run/daemon.log"));

    let quarantine = fixture.run(&["daemon", "--json", "quarantine"]);
    assert_eq!(
        quarantine.status.code(),
        Some(0),
        "{}",
        output_text(&quarantine)
    );
    let quarantine = envelope(&quarantine);
    assert_eq!(quarantine["command"], "daemon.quarantine");
    assert_eq!(quarantine["status"], "ok");
    assert!(quarantine["data"].is_object(), "{quarantine}");
}

#[test]
fn idle_lifecycle_commands_share_the_envelope() {
    let fixture = Fixture::new();
    for (args, command) in [
        (&["daemon", "--json", "stop"][..], "daemon.stop"),
        (
            &["daemon", "--json", "prepare-update"][..],
            "daemon.prepare-update",
        ),
    ] {
        let output = fixture.run(args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "args={args:?}\n{}",
            output_text(&output)
        );
        let value = envelope(&output);
        assert_eq!(value["command"], command, "args={args:?}");
        assert_eq!(value["status"], "ok", "args={args:?}");
        assert_eq!(value["data"]["state"], "stopped", "args={args:?}");
        assert!(value["error"].is_null(), "args={args:?}");
    }
}

#[test]
fn human_mode_is_bounded_and_not_machine_json() {
    let fixture = Fixture::new();
    let output = fixture.run(&["daemon", "status"]);
    assert_eq!(output.status.code(), Some(0), "{}", output_text(&output));
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().next(), Some("status: ok"), "{text}");
    assert!(!text.contains("schema_version"), "{text}");
    assert!(text.lines().count() <= 20, "{text}");
}

#[test]
fn blocked_status_reports_exit_21() {
    let fixture = Fixture::new();
    fixture.mark_should_be_running();
    let output = fixture.run(&["daemon", "--json", "status"]);
    assert_eq!(output.status.code(), Some(21), "{}", output_text(&output));
    let value = envelope(&output);
    assert_eq!(value["command"], "daemon.status");
    assert_eq!(value["status"], "blocked");
    assert!(value["data"].is_null(), "{value}");
    assert_eq!(value["error"]["kind"], "daemon_unavailable");
    assert_eq!(value["error"]["code"], "daemon_not_running");
    assert_eq!(value["error"]["retryable"], true);

    let human = fixture.run(&["daemon", "status"]);
    assert_eq!(human.status.code(), Some(21), "{}", output_text(&human));
    let text = String::from_utf8(human.stdout).unwrap();
    assert_eq!(text.lines().next(), Some("status: blocked"), "{text}");
    assert!(text.contains("next: "), "{text}");
}

#[test]
fn usage_failures_render_the_same_envelope() {
    let fixture = Fixture::new();
    for (args, command, code) in [
        (&["daemon", "--json"][..], "daemon", "missing_command"),
        (
            &["daemon", "--json", "bogus"][..],
            "daemon.bogus",
            "unknown_command",
        ),
        (
            &["daemon", "--json", "status", "--bogus"][..],
            "daemon.status",
            "unknown_argument",
        ),
        (
            &["daemon", "--json", "logs", "--tail", "abc"][..],
            "daemon.logs",
            "invalid_value",
        ),
    ] {
        let output = fixture.run(args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "args={args:?}\n{}",
            output_text(&output)
        );
        let value = envelope(&output);
        assert_eq!(value["command"], command, "args={args:?}");
        assert_eq!(value["status"], "failed", "args={args:?}");
        assert!(value["data"].is_null(), "args={args:?}: {value}");
        assert_eq!(value["error"]["kind"], "usage", "args={args:?}");
        assert_eq!(value["error"]["code"], code, "args={args:?}");
    }
}

#[test]
fn failures_never_leak_secrets_in_json_or_human_mode() {
    const CREDENTIAL_URL: &str = "https://user:sup3rsecret@example.test";
    const TOKEN: &str = "tok3n-secret-value";
    let fixture = Fixture::new();

    let json = fixture.run(&[
        "daemon",
        "--json",
        "start",
        "--advertise",
        CREDENTIAL_URL,
        "--token",
        TOKEN,
    ]);
    assert_eq!(json.status.code(), Some(1), "{}", output_text(&json));
    let value = envelope(&json);
    assert_eq!(value["error"]["kind"], "usage");
    assert_eq!(value["error"]["code"], "invalid_serve_options");
    let json_text = output_text(&json);
    assert!(!json_text.contains("sup3rsecret"), "{json_text}");
    assert!(!json_text.contains(TOKEN), "{json_text}");

    let human = fixture.run(&[
        "daemon",
        "start",
        "--advertise",
        CREDENTIAL_URL,
        "--token",
        TOKEN,
    ]);
    assert_eq!(human.status.code(), Some(1), "{}", output_text(&human));
    let text = String::from_utf8(human.stdout).unwrap();
    assert_eq!(text.lines().next(), Some("status: failed"), "{text}");
    assert!(text.contains("next: "), "{text}");
    assert!(!text.contains("sup3rsecret"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
}
