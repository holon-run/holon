//! CLI exit-code contract tests.
//!
//! These tests exercise the compiled binary because the contract includes
//! clap's process exits, stdout/stderr routing, and anyhow's top-level error
//! rendering.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Output},
    thread,
};

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("CARGO_BIN_EXE_holon should be set for integration tests")
}

#[test]
fn control_plane_post_commands_pretty_print_json_stdout() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["task", "summary", "--cmd", "echo hi"],
            "/control/agents/main/tasks",
        ),
        (&["timer", "--after-ms", "1"], "/control/agents/main/timers"),
        (
            &["agent", "create", "worker"],
            "/control/agents/worker/create",
        ),
        (
            &["agent", "abort"],
            "/control/agents/main/current-run/abort",
        ),
        (
            &["skills", "install", "demo"],
            "/control/agents/main/skills/install",
        ),
        (
            &["skills", "uninstall", "demo"],
            "/control/agents/main/skills/uninstall",
        ),
    ];

    for (args, expected_path) in cases {
        let (output, actual_path) = run_with_mock_control_plane(args);
        assert_eq!(actual_path, *expected_path, "args: {args:?}");
        assert_pretty_json_stdout(output, expected_path);
    }
}

fn isolated_holon_command() -> (Command, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let mut command = Command::new(holon_bin());
    command
        .env("HOLON_HOME", home.path())
        .env("HOLON_AGENT_ID", "main")
        .env("HOLON_MODEL", "openai/gpt-5.4")
        .env(
            "HOLON_SOCKET_PATH",
            home.path().join("run").join("missing.sock"),
        )
        .env_remove("HOLON_CONTROL_TOKEN")
        .env_remove("HOLON_CONTROL_AUTH_MODE")
        .env_remove("RUST_LOG");
    (command, home)
}

fn output_text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run_with_mock_control_plane(args: &[&str]) -> (Output, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock control plane");
    let addr = listener.local_addr().expect("mock control plane address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept CLI request");
        let request = read_http_request(&mut stream);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("<missing-path>")
            .to_string();
        let body = format!(r#"{{"ok":true,"path":"{path}","nested":{{"value":1}}}}"#);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write mock response");
        path
    });

    let (mut command, _home) = isolated_holon_command();
    let output = command
        .env("HOLON_HTTP_ADDR", addr.to_string())
        .args(args)
        .output()
        .expect("run holon");
    let path = handle.join().expect("mock control plane thread");
    (output, path)
}

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let n = stream.read(&mut buffer).expect("read CLI request");
        assert_ne!(n, 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
            let content_length = headers
                .lines()
                .find_map(|line| line.split_once(':'))
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while bytes.len().saturating_sub(header_end + 4) < content_length {
                let n = stream.read(&mut buffer).expect("read CLI request body");
                assert_ne!(n, 0, "request ended before declared body");
                bytes.extend_from_slice(&buffer[..n]);
            }
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}

fn assert_pretty_json_stdout(output: Output, expected_path: &str) {
    let (stdout, stderr) = output_text(&output);
    assert_eq!(output.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr should stay empty: {stderr}");
    let expected = serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "path": expected_path,
        "nested": {
            "value": 1
        }
    }))
    .expect("serialize expected JSON");
    assert_eq!(stdout, format!("{expected}\n"));
}

#[test]
fn invalid_arguments_exit_with_clap_usage_code() {
    let (mut command, _home) = isolated_holon_command();
    let output = command
        .arg("--definitely-not-a-holon-flag")
        .output()
        .expect("run holon");

    let (stdout, stderr) = output_text(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout should stay empty: {stdout}");
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("Usage:"),
        "stderr should be a clap usage error:\n{stderr}"
    );
}

#[test]
fn unreachable_control_plane_exits_nonzero_without_machine_stdout() {
    let (mut command, _home) = isolated_holon_command();
    let output = command
        .env("HOLON_HTTP_ADDR", "127.0.0.1:9")
        .arg("status")
        .output()
        .expect("run holon");

    let (stdout, stderr) = output_text(&output);
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout should stay empty: {stdout}");
    assert!(
        stderr.contains("failed to send /agents/main/status")
            || stderr.contains("error sending request"),
        "stderr should explain the failed control-plane request:\n{stderr}"
    );
}

#[test]
fn invalid_provider_configuration_exits_nonzero_without_machine_stdout() {
    let (mut command, _home) = isolated_holon_command();
    let output = command
        .args([
            "config",
            "providers",
            "set",
            "script-test",
            "--transport",
            "openai_responses",
            "--base-url",
            "not-a-url",
        ])
        .output()
        .expect("run holon");

    let (stdout, stderr) = output_text(&output);
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout should stay empty: {stdout}");
    assert!(
        stderr.contains("providers.<id>.base_url") || stderr.contains("not-a-url"),
        "stderr should explain the invalid provider setup:\n{stderr}"
    );
}
