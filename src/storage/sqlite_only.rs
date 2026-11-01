//! Reject retired file storage without opening its bodies.

use std::path::Path;

use anyhow::{ensure, Context, Result};

use crate::runtime_db::RuntimeDb;

use super::AppStorage;

const LEGACY_FILES: &[(&str, &str)] = &[
    ("state/agent.json", "agent_states"),
    ("ledger/workspaces.jsonl", "workspace_entries"),
    (
        "ledger/workspace_occupancies.jsonl",
        "workspace_occupancies",
    ),
    ("ledger/agent_identities.jsonl", "agent_identities"),
    ("ledger/work_items.jsonl", "work_items"),
    (
        "ledger/work_item_delegations.jsonl",
        "work_item_delegations",
    ),
    (
        "ledger/work_item_continuations.jsonl",
        "work_item_continuations",
    ),
    ("ledger/context_episodes.jsonl", "context_episode_anchors"),
    ("ledger/tasks.jsonl", "tasks"),
    ("ledger/external_triggers.jsonl", "external_triggers"),
    ("ledger/wait_conditions.jsonl", "wait_conditions"),
    ("ledger/queue_entries.jsonl", "queue_entries"),
    ("ledger/timers.jsonl", "timers"),
    ("ledger/messages.jsonl", "messages"),
    ("ledger/transcript.jsonl", "transcript_entries"),
    ("ledger/turns.jsonl", "turn_records"),
    ("ledger/tools.jsonl", "evidence"),
    ("ledger/briefs.jsonl", "evidence"),
    ("ledger/delivery_summaries.jsonl", "evidence"),
    ("ledger/events.jsonl", "audit_events"),
];

impl AppStorage {
    pub(crate) fn validate_sqlite_layout(
        data_dir: &Path,
        agent_id: Option<&str>,
        runtime_db: &RuntimeDb,
    ) -> Result<()> {
        for (relative_path, domain) in LEGACY_FILES {
            let path = data_dir.join(super::RUNTIME_DIR).join(relative_path);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("checking retired storage {}", path.display()));
                }
            }
            let canonical_scope = match agent_id {
                Some(agent_id) => runtime_db.has_sqlite_agent_scope(agent_id)?,
                None => true,
            };
            ensure!(
                canonical_scope && runtime_db.storage_domain_is_complete(domain, "db")?,
                "legacy JSON/JSONL storage is no longer supported: {}; \
                 this scope has no completed SQLite cutover; legacy data was not imported",
                path.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::storage_domain::upsert_storage_domain;
    use crate::types::AgentState;
    use tempfile::{tempdir, TempDir};

    fn database(dir: &TempDir) -> RuntimeDb {
        RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )
        .unwrap()
    }

    fn legacy_file(dir: &TempDir, relative: &str) -> std::path::PathBuf {
        let path = dir.path().join(".holon").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    #[test]
    fn new_sqlite_domains_initialize_directly_and_reopen() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        assert!(db.storage_domains().unwrap().is_empty());
        db.initialize_sqlite_storage_domains().unwrap();
        db.validate_expected_storage_domains(RuntimeDb::expected_storage_domains())
            .unwrap();
        let before = db.storage_domains().unwrap();
        drop(db);
        let reopened = database(&dir);
        reopened.initialize_sqlite_storage_domains().unwrap();
        assert_eq!(reopened.storage_domains().unwrap(), before);
        assert!(!dir.path().join(".holon/ledger").exists());
    }

    #[test]
    fn unfinished_import_rejected_without_partial_domain_initialization() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        for status in ["pending", "importing", "failed"] {
            db.transaction(|tx| {
                upsert_storage_domain(
                    tx,
                    "tasks",
                    status,
                    "jsonl",
                    Some(serde_json::json!({"original": status})),
                )
            })
            .unwrap();
            let before = db.storage_domains().unwrap();
            let error = db.initialize_sqlite_storage_domains().unwrap_err();
            assert!(
                error.to_string().contains("no longer supported"),
                "{error:#}"
            );
            assert_eq!(db.storage_domains().unwrap(), before);
        }
    }

    #[test]
    fn completed_checkpoint_is_preserved() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        db.transaction(|tx| {
            upsert_storage_domain(
                tx,
                "tasks",
                "complete",
                "db",
                Some(serde_json::json!({"original": "checkpoint"})),
            )
        })
        .unwrap();
        let before = db.storage_domain("tasks").unwrap();
        db.initialize_sqlite_storage_domains().unwrap();
        assert_eq!(db.storage_domain("tasks").unwrap(), before);
    }

    #[test]
    fn unfinished_episode_import_rejected_even_with_complete_domains() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        db.initialize_sqlite_storage_domains().unwrap();
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO episode_continuity_seeds
                 (seed_id, agent_id, policy_version, boundary_id, source_kind,
                  roots_json, candidate_payloads_json, import_required, captured_at)
                 VALUES ('old-seed', 'old-agent', 1, 'old-boundary', 'legacy_state_import',
                         '{}', '{}', 1, '2026-10-18T00:00:00Z')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let before = db.storage_domains().unwrap();
        let error = db.initialize_sqlite_storage_domains().unwrap_err();
        assert!(
            error.to_string().contains("no longer supported"),
            "{error:#}"
        );
        assert_eq!(db.storage_domains().unwrap(), before);
        assert_eq!(
            db.pending_legacy_episode_import_agent_ids().unwrap(),
            ["old-agent"]
        );
    }

    #[test]
    fn file_only_agent_rejected_even_after_global_domains_initialize() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        db.initialize_sqlite_storage_domains().unwrap();
        let path = legacy_file(&dir, "state/agent.json");
        let original = b"\xffnot valid JSON or UTF-8";
        std::fs::write(&path, original).unwrap();
        let error =
            AppStorage::validate_sqlite_layout(dir.path(), Some("old-agent"), &db).unwrap_err();
        assert!(
            error.to_string().contains("no longer supported"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert!(db.agent_states().latest("old-agent").unwrap().is_none());
        assert!(db.agent_identities().latest("old-agent").unwrap().is_none());
    }

    #[test]
    fn host_file_only_layout_rejected_before_domain_initialization() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        let path = legacy_file(&dir, "ledger/workspaces.jsonl");
        std::fs::write(&path, "not JSONL").unwrap();
        let error = AppStorage::validate_sqlite_layout(dir.path(), None, &db).unwrap_err();
        assert!(
            error.to_string().contains("no longer supported"),
            "{error:#}"
        );
        assert!(db.storage_domains().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not JSONL");
    }

    #[test]
    fn existing_sqlite_scope_with_unfinished_file_cutover_rejected() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        db.agent_states()
            .upsert_for_fixture(&AgentState::new("agent-a"))
            .unwrap();
        let path = legacy_file(&dir, "ledger/messages.jsonl");
        std::fs::write(path, "unimported").unwrap();
        let error =
            AppStorage::validate_sqlite_layout(dir.path(), Some("agent-a"), &db).unwrap_err();
        assert!(
            error.to_string().contains("no longer supported"),
            "{error:#}"
        );
        assert!(db.storage_domains().unwrap().is_empty());
        assert!(db.agent_states().latest("agent-a").unwrap().is_some());
    }

    #[test]
    fn completed_sqlite_scope_ignores_old_file_bodies() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        let state = AgentState::new("agent-a");
        db.agent_states().upsert_for_fixture(&state).unwrap();
        db.initialize_sqlite_storage_domains().unwrap();
        let path = legacy_file(&dir, "state/agent.json");
        std::fs::write(&path, b"\xffnot valid JSON or UTF-8").unwrap();
        AppStorage::validate_sqlite_layout(dir.path(), Some("agent-a"), &db).unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "agent-a", db).unwrap();
        assert_eq!(storage.read_agent().unwrap(), Some(state));
        assert_eq!(std::fs::read(path).unwrap(), b"\xffnot valid JSON or UTF-8");
    }

    #[cfg(unix)]
    #[test]
    fn file_detection_and_completed_scope_never_open_legacy_fifo() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempdir().unwrap();
        let db = database(&dir);
        let path = legacy_file(&dir, "state/agent.json");
        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        // A body read would block: this FIFO has no writer.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let error =
            AppStorage::validate_sqlite_layout(dir.path(), Some("agent-a"), &db).unwrap_err();
        assert!(
            error.to_string().contains("no longer supported"),
            "{error:#}"
        );
        db.agent_states()
            .upsert_for_fixture(&AgentState::new("agent-a"))
            .unwrap();
        db.initialize_sqlite_storage_domains().unwrap();
        AppStorage::validate_sqlite_layout(dir.path(), Some("agent-a"), &db).unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "agent-a", db).unwrap();
        assert_eq!(storage.read_agent().unwrap().unwrap().id, "agent-a");
    }

    #[test]
    fn read_only_storage_construction_does_not_probe_retired_layout() {
        let dir = tempdir().unwrap();
        let db = database(&dir);
        let path = legacy_file(&dir, "state/agent.json");
        std::fs::write(path, "retired file-only scope").unwrap();
        // Inspection remains a SQLite read; only startup owns format admission.
        let storage = AppStorage::open_read_only_for_agent(dir.path(), "agent-a", db).unwrap();
        assert!(storage.read_agent().unwrap().is_none());
    }
}
