//! Hermetic compiled-binary fixtures for `holon daemon validate-startup`.
//!
//! The command must be able to prove a real runtime library without a daemon,
//! without network access, and without provider credentials. Read-only mode is
//! additionally proven to leave the whole data directory byte- and
//! mtime-identical.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

use holon::runtime_db::{RuntimeDb, RuntimeDbLock};
use holon::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
};
use serde_json::Value;

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

    fn socket_path(&self) -> PathBuf {
        self.home.path().join("run").join("control.sock")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(holon_bin())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("HOLON_HOME", self.home.path())
            .env("HOLON_WORKSPACE_DIR", self.home.path())
            .env("HOLON_SOCKET_PATH", self.socket_path())
            .env("HOLON_AGENT_ID", "main")
            .env("HOLON_MODEL", "openai/gpt-5.4")
            .env("OPENAI_API_KEY", "offline-validate-startup-test-key")
            .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
            .env("RUST_LOG", "off")
            .current_dir(self.home.path())
            .args(args)
            .output()
            .expect("run compiled holon")
    }

    fn db_path(&self) -> PathBuf {
        self.home.path().join("state").join("runtime.sqlite")
    }

    /// Build a complete library: migrated schema, every expected storage
    /// domain, and the configured default agent identity.
    fn create_library(&self) {
        let db_path = self.db_path();
        let lock_path = self.home.path().join("state").join("runtime.lock");
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap();
        {
            let connection = db.connection().unwrap();
            for expected in RuntimeDb::expected_storage_domains() {
                connection
                    .execute(
                        "INSERT INTO storage_domains(
                           domain, schema_version, import_status, canonical_source,
                           source_checkpoint_json, imported_at, updated_at
                         ) VALUES(?1, 1, 'complete', ?2, NULL, ?3, ?3)",
                        rusqlite::params![
                            expected.domain,
                            expected.canonical_source,
                            "2026-01-01T00:00:00.000Z"
                        ],
                    )
                    .unwrap();
            }
        }
        db.agent_identities()
            .upsert(&AgentIdentityRecord::new(
                "main",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))
            .unwrap();
        drop(db);
    }

    fn schema_version(&self) -> i64 {
        let connection = rusqlite::Connection::open_with_flags(
            self.db_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        connection
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

fn parse_envelope(output: &Output, expected_exit: i32) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let envelope: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not one JSON envelope: {error}\n{stdout}\n{stderr}")
    });
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["command"], "daemon.validate-startup");
    envelope
}

fn phase_status<'a>(data: &'a Value, phase: &str) -> Option<&'a str> {
    data["phases"]
        .as_array()?
        .iter()
        .find(|entry| entry["phase"] == phase)?["status"]
        .as_str()
}

fn snapshot_dir(root: &Path) -> Vec<(String, u64, SystemTime)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64, SystemTime)>) {
        let mut entries = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            let metadata = entry.metadata().unwrap();
            out.push((
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                metadata.len(),
                metadata.modified().unwrap(),
            ));
            if metadata.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

#[test]
fn healthy_library_read_only_is_zero_write_and_names_every_phase() {
    let fixture = Fixture::new();
    fixture.create_library();
    let before = snapshot_dir(fixture.home.path());

    let output = fixture.run(&["daemon", "validate-startup", "--json"]);
    let after = snapshot_dir(fixture.home.path());
    let envelope = parse_envelope(&output, 0);

    assert_eq!(
        before, after,
        "read-only validation must not write the data directory"
    );
    assert_eq!(envelope["status"], "ok");
    assert!(envelope["error"].is_null());
    let data = &envelope["data"];
    assert_eq!(data["schema_version"], 1);
    assert_eq!(data["mode"], "read_only");
    for field in [
        "home_dir",
        "data_dir",
        "runtime_db",
        "maintenance_lock",
        "control_socket",
    ] {
        assert!(
            data["paths"][field].as_str().is_some(),
            "paths.{field} must be present"
        );
    }
    assert_eq!(data["db"]["mode"], "read_only");
    assert_eq!(data["db"]["schema_version"], fixture.schema_version());
    assert_eq!(data["db"]["max_known"], fixture.schema_version());
    assert_eq!(data["agents"]["total"], 1);
    assert_eq!(data["agents"]["default_resolution"], "resolved");
    assert_eq!(data["quarantine"]["status"], "unavailable");
    assert_eq!(data["eligibility"]["snapshot"], "snapshot_missing");
    assert_eq!(data["phases"].as_array().unwrap().len(), 10);
    for phase in [
        "paths",
        "daemon_gate",
        "db_presence",
        "schema_verify",
        "migrate",
        "storage_domains",
        "agent_registry",
        "quarantine",
        "eligibility",
        "summary",
    ] {
        assert!(phase_status(data, phase).is_some(), "phase {phase} missing");
    }
    assert_eq!(phase_status(data, "daemon_gate"), Some("skipped"));
    assert_eq!(phase_status(data, "migrate"), Some("skipped"));
    assert!(data["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning["code"] == "quarantine_unavailable"));
}

#[test]
fn missing_database_is_a_domain_failure_without_creating_one() {
    let fixture = Fixture::new();
    let output = fixture.run(&["daemon", "validate-startup", "--json"]);
    let envelope = parse_envelope(&output, 20);
    assert_eq!(envelope["status"], "failed");
    assert_eq!(envelope["error"]["code"], "db_missing");
    assert_eq!(
        phase_status(&envelope["data"], "db_presence"),
        Some("failed")
    );
    assert!(
        !fixture.db_path().exists(),
        "validation must not create a db"
    );
}

#[test]
fn newer_schema_is_rejected() {
    let fixture = Fixture::new();
    fixture.create_library();
    let newest = fixture.schema_version();
    let connection = rusqlite::Connection::open(fixture.db_path()).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES(?1, 'future', ?2)",
            rusqlite::params![newest + 1, "2026-01-01T00:00:00.000Z"],
        )
        .unwrap();
    drop(connection);

    let output = fixture.run(&["daemon", "validate-startup", "--json"]);
    let envelope = parse_envelope(&output, 20);
    assert_eq!(envelope["error"]["code"], "schema_newer_than_binary");
    assert_eq!(
        phase_status(&envelope["data"], "schema_verify"),
        Some("failed")
    );
}

#[test]
fn older_schema_requires_migration_in_read_only_mode() {
    let fixture = Fixture::new();
    fixture.create_library();
    let newest = fixture.schema_version();
    let connection = rusqlite::Connection::open(fixture.db_path()).unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version = ?1", [newest])
        .unwrap();
    drop(connection);

    let output = fixture.run(&["daemon", "validate-startup", "--json"]);
    let envelope = parse_envelope(&output, 22);
    assert_eq!(envelope["status"], "failed");
    assert_eq!(envelope["error"]["code"], "migration_required");
    assert_eq!(
        phase_status(&envelope["data"], "schema_verify"),
        Some("failed")
    );

    let output = fixture.run(&["daemon", "validate-startup"]);
    assert_eq!(output.status.code(), Some(22));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().count() <= 20,
        "human output must stay bounded:\n{stdout}"
    );
    assert!(stdout.starts_with("status: failed"));
    assert!(stdout.contains("next:"));
}

#[test]
fn migrate_mode_refuses_while_the_maintenance_lease_is_held() {
    let fixture = Fixture::new();
    fixture.create_library();
    let lease = RuntimeDbLock::try_lock(fixture.home.path().join("state/runtime-maintenance.lock"))
        .unwrap();

    let output = fixture.run(&["daemon", "validate-startup", "--migrate", "--json"]);
    let envelope = parse_envelope(&output, 21);
    assert_eq!(envelope["status"], "blocked");
    assert_eq!(envelope["error"]["code"], "daemon_lease_held");
    assert!(envelope["error"]["retryable"].as_bool().unwrap());
    assert_eq!(
        phase_status(&envelope["data"], "daemon_gate"),
        Some("failed")
    );
    assert_eq!(
        phase_status(&envelope["data"], "db_presence"),
        Some("skipped")
    );
    drop(lease);
}

#[test]
fn migrate_mode_refuses_when_the_control_socket_is_live() {
    let fixture = Fixture::new();
    fixture.create_library();
    let _listener = std::os::unix::net::UnixListener::bind(fixture.socket_path()).unwrap();

    let output = fixture.run(&["daemon", "validate-startup", "--migrate", "--json"]);
    let envelope = parse_envelope(&output, 21);
    assert_eq!(envelope["error"]["code"], "daemon_running");
    assert_eq!(
        phase_status(&envelope["data"], "daemon_gate"),
        Some("failed")
    );
}

#[test]
fn migrate_mode_accepts_a_healthy_stopped_library() {
    let fixture = Fixture::new();
    fixture.create_library();

    let output = fixture.run(&["daemon", "validate-startup", "--migrate", "--json"]);
    let envelope = parse_envelope(&output, 0);
    assert_eq!(envelope["status"], "ok");
    let data = &envelope["data"];
    assert_eq!(data["mode"], "migrate");
    assert_eq!(data["db"]["mode"], "migrate");
    assert_eq!(phase_status(data, "daemon_gate"), Some("ok"));
    assert_eq!(phase_status(data, "migrate"), Some("skipped"));
    assert_eq!(phase_status(data, "storage_domains"), Some("ok"));
}
