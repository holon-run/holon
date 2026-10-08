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
    time::{Duration, Instant},
};

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("CARGO_BIN_EXE_holon should be set for integration tests")
}

fn write_json_response(stream: &mut TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("write mock response");
}

#[test]
fn skills_update_waits_for_job_and_prints_result() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock control plane");
    let addr = listener.local_addr().expect("mock control plane address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept update request");
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /api/skills/catalog/update "));
        let body = r#"{"ok":true,"job":{"id":"job_test","status":"queued"}}"#;
        write_json_response(&mut stream, body);

        let (mut stream, _) = listener.accept().expect("accept job request");
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("GET /api/jobs/job_test "));
        let body = r#"{"ok":true,"job":{"id":"job_test","status":"completed","result":{"statuses":[{"name":"demo","status":"updated"}]}}}"#;
        write_json_response(&mut stream, body);
    });

    let (mut command, _home) = isolated_holon_command();
    let output = command
        .env("HOLON_HTTP_ADDR", addr.to_string())
        .args(["skills", "update"])
        .output()
        .expect("run holon");
    handle.join().expect("mock control plane thread");

    let expected = serde_json::json!({
        "statuses": [{"name": "demo", "status": "updated"}]
    });
    let (stdout, stderr) = output_text(&output);
    assert_eq!(output.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr should stay empty: {stderr}");
    assert_eq!(
        stdout,
        format!("{}\n", serde_json::to_string_pretty(&expected).unwrap())
    );
}

#[test]
fn control_plane_post_commands_pretty_print_json_stdout() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["task", "run", "summary", "--cmd", "echo hi"],
            "/api/control/agents/main/tasks",
        ),
        (
            &["agent", "create", "worker"],
            "/api/control/agents/worker/create",
        ),
        (
            &["agent", "abort"],
            "/api/control/agents/main/current-run/abort",
        ),
        (
            &["agent", "timezone", "set", "Asia/Shanghai"],
            "/api/control/agents/main/timezone",
        ),
        (
            &["agent", "timezone", "clear", "worker"],
            "/api/control/agents/worker/timezone/clear",
        ),
        (
            &["skills", "install", "demo"],
            "/api/control/agents/main/skills/install",
        ),
        (
            &["skills", "uninstall", "demo"],
            "/api/control/agents/main/skills/uninstall",
        ),
        (&["skills", "catalog"], "/api/skills/catalog"),
        (
            &[
                "skills",
                "add",
                "holon-run/holon",
                "--remote",
                "--skill",
                "ghx",
            ],
            "/api/skills/catalog/add",
        ),
        (&["skills", "remove", "ghx"], "/api/skills/catalog/remove"),
        (&["skills", "refresh"], "/api/skills/catalog/refresh"),
        (&["skills", "reconcile"], "/api/skills/catalog/reconcile"),
        (&["skills", "check"], "/api/skills/catalog/check"),
        (
            &["skills", "enable", "ghx"],
            "/api/control/agents/main/skills/enable",
        ),
        (
            &["skills", "disable", "ghx"],
            "/api/control/agents/main/skills/disable",
        ),
    ];

    for (args, expected_path) in cases {
        let (output, actual_path) = run_with_mock_control_plane(args);
        assert_eq!(actual_path, *expected_path, "args: {args:?}");
        assert_pretty_json_stdout(output, expected_path);
    }
}

#[test]
fn timezone_commands_reject_agent_and_malformed_context_before_control_requests() {
    let cases: &[&[&str]] = &[
        &["agent", "timezone", "get"],
        &["agent", "timezone", "set", "Asia/Shanghai", "other-agent"],
        &["agent", "timezone", "clear", "other-agent"],
    ];
    for args in cases {
        for malformed in [false, true] {
            let (mut command, _home) = isolated_holon_command();
            if malformed {
                command.env("HOLON_CALLER_SOURCE_TASK_ID", "task-test");
            } else {
                command.env("HOLON_CALLER_AGENT_ID", "agent-test");
            }
            let output = command.args(*args).output().expect("run isolated holon");
            let (stdout, stderr) = output_text(&output);
            assert_eq!(output.status.code(), Some(1), "args: {args:?}, {stderr}");
            assert!(stdout.is_empty(), "args: {args:?}, {stdout}");
            let expected = if malformed {
                "HOLON_CALLER_AGENT_ID is required"
            } else {
                "agent timezone commands require operator mode"
            };
            assert!(stderr.contains(expected), "args: {args:?}, {stderr}");
        }
    }
}

#[test]
fn timer_commands_use_lifecycle_routes_and_pretty_print_records() {
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["timer", "--after-ms", "1"],
            "/api/control/agents/main/timers",
            "POST",
        ),
        (
            &["timer", "create", "--after-ms", "1"],
            "/api/control/agents/main/timers",
            "POST",
        ),
        (
            &["timer", "cancel", "timer-1"],
            "/api/control/agents/main/timers/timer-1/cancel",
            "POST",
        ),
        (
            &["timer", "list"],
            "/api/agents/main/timers?limit=50",
            "GET",
        ),
    ];

    for (args, expected_path, expected_method) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock control plane");
        let addr = listener.local_addr().expect("mock control plane address");
        let expected_path = expected_path.to_string();
        let expected_method = expected_method.to_string();
        let list = expected_method == "GET";
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept timer request");
            let request = read_http_request(&mut stream);
            assert!(
                request.starts_with(&format!("{expected_method} {expected_path} ")),
                "unexpected request: {request}"
            );
            let timer = serde_json::json!({
                "id": "timer-1",
                "agent_id": "main",
                "created_at": "2026-08-25T00:00:00Z",
                "duration_ms": 1,
                "interval_ms": null,
                "repeat": false,
                "status": "active",
                "summary": null,
                "next_fire_at": "2026-08-25T00:00:00.001Z",
                "last_fired_at": null,
                "fire_count": 0
            });
            let body = if list {
                serde_json::to_string(&vec![timer]).unwrap()
            } else {
                serde_json::to_string(&timer).unwrap()
            };
            write_json_response(&mut stream, &body);
        });

        let (mut command, _home) = isolated_holon_command();
        let output = command
            .env("HOLON_HTTP_ADDR", addr.to_string())
            .args(*args)
            .output()
            .expect("run holon");
        handle.join().expect("mock timer control plane thread");

        let (stdout, stderr) = output_text(&output);
        assert_eq!(output.status.code(), Some(0), "stderr:\n{stderr}");
        assert!(stderr.is_empty(), "stderr should stay empty: {stderr}");
        assert!(
            stdout.contains("\"id\": \"timer-1\""),
            "stdout should contain timer record: {stdout}"
        );
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
        .env_remove("HOLON_CALLER_AGENT_ID")
        .env_remove("HOLON_CALLER_SOURCE_TASK_ID")
        .env_remove("HOLON_CALLER_SOURCE_TURN_ID")
        .env_remove("HOLON_CALLER_SOURCE_WORK_ITEM_ID")
        .env_remove("HOLON_CALLER_SOURCE_ACTIVATION_ID")
        .env_remove("HOLON_CALLER_AUTHORITY_CLASS")
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
        listener
            .set_nonblocking(true)
            .expect("configure mock listener blocking mode");
        let deadline = Instant::now() + Duration::from_secs(30);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for CLI request"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept CLI request: {error}"),
            }
        };
        stream
            .set_nonblocking(false)
            .expect("configure mock stream blocking mode");
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
        .arg("agent")
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
fn remote_skill_errors_render_codes_and_recovery_hints_and_exit_one() {
    let cases = [
        (
            429,
            "remote_skill_rate_limited",
            "GitHub API rate limit exceeded",
            "Wait 17 seconds before retrying. For anonymous access, configure daemon-visible GITHUB_TOKEN / GH_TOKEN or authenticated gh.",
        ),
        (
            429,
            "remote_skill_rate_limited",
            "GitHub API rate limit exceeded",
            "Wait until 2026-10-06T12:00:00Z (UTC) before retrying.",
        ),
        (
            400,
            "remote_skill_not_found",
            "remote skill was not found",
            "browse the repository's skills/ directory and install one concrete skill",
        ),
    ];
    for subcommand in ["add", "install"] {
        for (status, code, error, hint) in cases {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing CLI request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                let expected = if subcommand == "add" {
                    "/api/skills/catalog/add"
                } else {
                    "/api/control/agents/main/skills/install"
                };
                assert!(
                    request.starts_with(&format!("POST {expected} ")),
                    "{request}"
                );
                let body = serde_json::json!({
                    "ok": false,
                    "error": error,
                    "code": code,
                    "hint": hint,
                    "retryable": status == 429,
                    "upstream_status": 403
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let (mut command, _home) = isolated_holon_command();
            let output = command
                .env("HOLON_HTTP_ADDR", addr.to_string())
                .args([
                    "skills",
                    subcommand,
                    "owner/repo",
                    "--remote",
                    "--skill",
                    "demo",
                ])
                .output()
                .unwrap();
            handle.join().unwrap();
            let (stdout, stderr) = output_text(&output);
            assert_eq!(output.status.code(), Some(1), "{subcommand}: {stderr}");
            assert!(stdout.is_empty(), "{stdout}");
            assert!(stderr.contains(code), "{stderr}");
            assert!(stderr.contains(error), "{stderr}");
            assert!(stderr.contains(hint), "{stderr}");
            assert!(!stderr.contains("exit status 403"), "{stderr}");
        }
    }
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
