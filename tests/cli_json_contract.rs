//! CLI JSON output contract tests.
//!
//! These tests lock the initial stable-candidate script-facing JSON surfaces.
//! They intentionally exercise the compiled binary so stdout/stderr routing,
//! pretty-printing, and persisted config paths are covered together.

use std::{
    collections::BTreeSet,
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

fn isolated_holon_command(home: &tempfile::TempDir) -> Command {
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
    command
}

fn run_json(home: &tempfile::TempDir, args: &[&str]) -> Value {
    let output = isolated_holon_command(home)
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
    assert!(
        value["next_actions"].as_array().is_some(),
        "onboard report should include actionable next-step array: {value}"
    );
    assert!(
        !value.to_string().contains("super-secret"),
        "onboard JSON must not expose credential material"
    );
}
