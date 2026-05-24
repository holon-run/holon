//! CLI exit-code contract tests.
//!
//! These tests exercise the compiled binary because the contract includes
//! clap's process exits, stdout/stderr routing, and anyhow's top-level error
//! rendering.

use std::{
    path::PathBuf,
    process::{Command, Output},
};

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("CARGO_BIN_EXE_holon should be set for integration tests")
}

fn isolated_holon_command() -> (Command, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("create isolated HOLON_HOME");
    let mut command = Command::new(holon_bin());
    command
        .env("HOLON_HOME", home.path())
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
