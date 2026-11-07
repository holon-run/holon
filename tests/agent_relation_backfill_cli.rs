//! Exercise report ownership through the actual offline CLI serializer.
use holon::{
    runtime_db::RuntimeDb,
    types::{AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use serde_json::Value;
use std::process::Command;

#[test]
fn backfill_cli_report_apply_backup_and_retry_keep_the_wire_contract() {
    let home = tempfile::tempdir().unwrap();
    let db_path = home.path().join("state/runtime.sqlite");
    let lock_path = home.path().join("state/runtime.lock");
    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap();
    db.agent_identities()
        .upsert(&AgentIdentityRecord::new(
            "worker-汉字",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))
        .unwrap();
    drop(db);

    let run = |options: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_holon"))
            .env("HOLON_HOME", home.path())
            .env("HOLON_AGENT_ID", "main")
            .env("HOLON_MODEL", "openai/gpt-5.4")
            .env_remove("RUST_LOG")
            .args(["debug", "runtime-db", "agent-relations"])
            .args(options)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let dry_run: Value = serde_json::from_str(&run(&["--json"])).unwrap();
    assert_eq!(dry_run["apply"], false);
    assert_eq!(dry_run["scanned_agents"], 1);
    assert_eq!(dry_run["changed_agents"], 1);
    assert_eq!(dry_run["migrated_axes"], 4);
    assert_eq!(dry_run["diagnostics"], serde_json::json!([]));
    assert!(dry_run.get("run_id").is_none());
    assert!(dry_run.get("backup_path").is_none());
    assert!(dry_run.get("report").is_none());
    assert!(dry_run.get("_material").is_none());
    assert!(run(&[]).contains("mode=report scanned=1 changed=1"));

    let applied: Value = serde_json::from_str(&run(&["--apply", "--json"])).unwrap();
    assert_eq!(applied["apply"], true);
    assert_eq!(applied["changed_agents"], 1);
    assert_eq!(applied["migrated_axes"], 4);
    assert!(applied["run_id"]
        .as_str()
        .unwrap()
        .starts_with("agent-relations-"));
    assert!(std::path::Path::new(applied["backup_path"].as_str().unwrap()).is_file());
    let retry: Value = serde_json::from_str(&run(&["--apply", "--no-backup", "--json"])).unwrap();
    assert_eq!(retry["changed_agents"], 0);
    assert_eq!(retry["unchanged_agents"], 1);
    assert_eq!(retry["migrated_axes"], 0);
    assert!(retry.get("backup_path").is_none());
}
