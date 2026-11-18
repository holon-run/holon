//! Bounded direct-supervision discovery from committed scalar projections.
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::RuntimeDb;
use crate::types::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubagentCursor {
    version: u8,
    runtime_id: String,
    supervisor: String,
    supervisor_incarnation: u64,
    after: String,
    child_incarnation: u64,
    supervision_id: String,
    supervision_revision: u64,
}

// Match the canonical projection's latest-edge ordering, including closed edges.
// An older active edge must never resurrect visibility.
const CURRENT_EDGE: &str = "
    SELECT s.child_agent_id,
           COALESCE(json_extract(i.payload_json, '$.incarnation'), 1),
           s.state, i.status, json_extract(a.payload_json, '$.status'),
           s.delegated_from_task_id, t.status, s.supervision_id, s.revision
    FROM agent_supervisions s
    JOIN agent_identities i ON i.agent_id = s.child_agent_id
    LEFT JOIN agent_states a ON a.agent_id = s.child_agent_id
    LEFT JOIN tasks t ON t.task_id = s.delegated_from_task_id
         AND t.owner_agent_id = s.supervisor_agent_id
         AND t.child_agent_id = s.child_agent_id
    WHERE s.supervisor_agent_id = ?1
      AND s.state IN ('active', 'cleanup_required')
      AND i.status IN ('active', 'deleting')
      AND s.supervision_id = (
          SELECT latest.supervision_id FROM agent_supervisions latest
          WHERE latest.child_agent_id = s.child_agent_id
          ORDER BY latest.revision DESC, latest.updated_at DESC,
                   latest.supervision_id DESC LIMIT 1
      )";

struct Row {
    entry: SubagentListEntry,
    supervision_id: String,
    supervision_revision: u64,
}

fn enum_value<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(value))?)
}

fn cleanup_diagnostic(
    tx: &Transaction<'_>,
    parent: &str,
    row: &Row,
) -> Result<SubagentCleanupDiagnostic> {
    // Observations can lag activity. Bind the last observation to the current
    // identity and supervision without expanding its historical state evidence.
    let observed_since = tx
        .query_row(
            "SELECT observed_since FROM subagent_cleanup_observations
             WHERE child_agent_id = ?1 AND parent_agent_id = ?2
               AND json_extract(evidence, '$[1]') = ?3
               AND json_extract(evidence, '$[3]') = ?4
               AND json_extract(json_extract(evidence, '$[0]'), '$[0]') = ?5",
            params![
                row.entry.agent_id,
                parent,
                row.supervision_id,
                row.supervision_revision,
                row.entry.incarnation
            ],
            |r| r.get(0),
        )
        .optional()?;
    Ok(SubagentCleanupDiagnostic {
        blocker: super::reclamation::parent_cleanup_blocker_tx(tx, &row.entry.agent_id)?,
        observed_since,
    })
}

impl RuntimeDb {
    pub(crate) fn list_subagents(
        &self,
        supervisor: &str,
        limit: usize,
        cursor: Option<&str>,
        include_cleanup: bool,
    ) -> Result<SubagentListPage> {
        ensure!(
            (1..=MAX_SUBAGENT_LIST_LIMIT).contains(&limit),
            "ListSubagents limit must be between 1 and {MAX_SUBAGENT_LIST_LIMIT}"
        );
        let cursor: Option<SubagentCursor> = cursor
            .map(|encoded| -> Result<_> {
                ensure!(
                    encoded.len() <= 4096,
                    "invalid ListSubagents cursor: too long"
                );
                let payload = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .context("invalid ListSubagents cursor encoding")?;
                serde_json::from_slice(&payload).context("invalid ListSubagents cursor")
            })
            .transpose()?;
        self.with_read(|connection| {
            let tx = connection.unchecked_transaction()?;
            let runtime_id = Self::runtime_id_on(&tx)?;
            let incarnation: Option<u64> = tx
                .query_row(
                    "SELECT COALESCE(json_extract(payload_json, '$.incarnation'), 1)
                     FROM agent_identities WHERE agent_id = ?1 AND status = 'active'",
                    [supervisor],
                    |r| r.get(0),
                )
                .optional()?;
            let incarnation =
                incarnation.context("ListSubagents caller identity is unavailable")?;
            if let Some(cursor) = &cursor {
                ensure!(
                    cursor.version == 1
                        && cursor.runtime_id == runtime_id
                        && cursor.supervisor == supervisor
                        && cursor.supervisor_incarnation == incarnation,
                    "invalid or foreign ListSubagents cursor"
                );
                let anchor: Option<(u64, String, u64)> = tx
                    .query_row(
                        &format!("{CURRENT_EDGE} AND s.child_agent_id = ?2"),
                        params![supervisor, cursor.after],
                        |r| Ok((r.get(1)?, r.get(7)?, r.get(8)?)),
                    )
                    .optional()?;
                ensure!(
                    anchor
                        == Some((
                            cursor.child_incarnation,
                            cursor.supervision_id.clone(),
                            cursor.supervision_revision
                        )),
                    "stale ListSubagents cursor: restart pagination without a cursor"
                );
            }
            let mut stmt = tx.prepare(&format!(
                "{CURRENT_EDGE} AND s.child_agent_id > ?2
                 ORDER BY s.child_agent_id LIMIT ?3"
            ))?;
            let rows = stmt.query_map(
                params![
                    supervisor,
                    cursor.as_ref().map_or("", |cursor| cursor.after.as_str()),
                    limit + 1
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, u64>(8)?,
                    ))
                },
            )?;
            let mut result = Vec::with_capacity(limit + 1);
            for row in rows {
                let (
                    agent_id,
                    incarnation,
                    supervision,
                    identity,
                    status,
                    task,
                    task_status,
                    id,
                    rev,
                ) = row?;
                result.push(Row {
                    entry: SubagentListEntry {
                        agent_id,
                        incarnation,
                        supervision_state: enum_value(supervision)?,
                        identity_status: enum_value(identity)?,
                        runtime_status: status.map(enum_value).transpose()?,
                        invocation_task: task
                            .map(|task_id| -> Result<_> {
                                Ok(SubagentInvocationTaskRef {
                                    task_id,
                                    status: task_status.map(enum_value).transpose()?,
                                })
                            })
                            .transpose()?,
                        cleanup: None,
                    },
                    supervision_id: id,
                    supervision_revision: rev,
                });
            }
            let has_more = result.len() > limit;
            result.truncate(limit);
            let next_cursor = if has_more {
                let last = result.last().expect("nonzero page limit");
                Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&SubagentCursor {
                    version: 1,
                    runtime_id,
                    supervisor: supervisor.into(),
                    supervisor_incarnation: incarnation,
                    after: last.entry.agent_id.clone(),
                    child_incarnation: last.entry.incarnation,
                    supervision_id: last.supervision_id.clone(),
                    supervision_revision: last.supervision_revision,
                })?))
            } else {
                None
            };
            if include_cleanup {
                for row in &mut result {
                    row.entry.cleanup = Some(cleanup_diagnostic(&tx, supervisor, row)?);
                }
            }
            Ok(SubagentListPage {
                subagents: result.into_iter().map(|row| row.entry).collect(),
                limit,
                next_cursor,
            })
        })
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn identity(id: &str) -> AgentIdentityRecord {
        AgentIdentityRecord::new(
            id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        )
    }

    fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        db.transaction(|tx| {
            for parent in ["parent", "other", "empty"] {
                super::super::evidence::upsert_agent_identity_tx(tx, &identity(parent))?;
            }
            Ok(())
        })?;
        Ok((dir, db))
    }

    fn child(db: &RuntimeDb, parent: &str, id: &str, status: AgentStatus) -> Result<()> {
        let task_id = format!("task:{id}");
        let identity = AgentIdentityRecord::new(
            id,
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent.into()),
            Some(task_id.clone()),
        );
        let records = super::super::agent_relations::supervised_creation_records(
            &identity, parent, &task_id, None,
        );
        let mut state = AgentState::new(id);
        state.status = status;
        let now = Utc::now();
        db.transaction(|tx| {
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)?;
            super::super::agent_relations::upsert_canonical_record_set_tx(tx, &records)?;
            super::super::evidence::upsert_agent_state_tx(tx, &state)?;
            holon_storage::task::upsert_task_tx(
                tx,
                &TaskRecord {
                    id: task_id.clone(),
                    agent_id: parent.into(),
                    kind: TaskKind::ChildAgentTask,
                    status: TaskStatus::Completed,
                    created_at: now,
                    updated_at: now,
                    parent_message_id: None,
                    work_item_id: None,
                    summary: None,
                    detail: Some(serde_json::json!({"child_agent_id": id})),
                    recovery: None,
                },
            )?;
            Ok(())
        })
    }

    fn close(db: &RuntimeDb, id: &str) -> Result<()> {
        db.transaction(|tx| {
            super::super::agent_relations::transition_supervision_state_tx(
                tx,
                id,
                AgentSupervisionState::Closed,
                0,
                Utc::now(),
            )
        })
    }

    #[test]
    fn direct_supervision_includes_running_retained_stopped_and_blocked_only() -> Result<()> {
        let (_dir, db) = fixture()?;
        child(&db, "parent", "running", AgentStatus::AwakeRunning)?;
        child(&db, "parent", "retained", AgentStatus::Asleep)?;
        child(&db, "parent", "stopped", AgentStatus::Stopped)?;
        child(&db, "parent", "blocked", AgentStatus::Asleep)?;
        child(&db, "other", "foreign", AgentStatus::Asleep)?;
        child(&db, "parent", "closed", AgentStatus::Asleep)?;
        close(&db, "closed")?;
        db.transaction(|tx| {
            let independent = identity("lineage-only");
            super::super::evidence::upsert_agent_identity_tx(tx, &independent)?;
            let mut records = super::super::agent_relations::supervised_creation_records(
                &independent, "parent", "historical-invocation", None,
            );
            records.supervision = None;
            super::super::agent_relations::upsert_canonical_record_set_tx(tx, &records)?;
            tx.execute(
                "UPDATE agent_supervisions SET state = 'cleanup_required' WHERE child_agent_id = 'blocked'",
                [],
            )?;
            Ok(())
        })?;
        let plain = db.list_subagents("parent", 100, None, false)?;
        assert_eq!(
            plain
                .subagents
                .iter()
                .map(|c| c.agent_id.as_str())
                .collect::<Vec<_>>(),
            ["blocked", "retained", "running", "stopped"]
        );
        assert!(plain.subagents.iter().all(|c| c.cleanup.is_none()));
        assert_eq!(
            plain.subagents[0].supervision_state,
            AgentSupervisionState::CleanupRequired
        );
        assert_eq!(
            plain.subagents[2].runtime_status,
            Some(AgentStatus::AwakeRunning)
        );
        assert_eq!(
            plain.subagents[3].runtime_status,
            Some(AgentStatus::Stopped)
        );
        assert_eq!(
            plain.subagents[1].invocation_task.as_ref().unwrap().status,
            Some(TaskStatus::Completed)
        );
        let diagnostic = db.list_subagents("parent", 100, None, true)?;
        assert_eq!(plain.next_cursor, diagnostic.next_cursor);
        for (a, mut b) in plain.subagents.into_iter().zip(diagnostic.subagents) {
            let cleanup = b.cleanup.take().expect("explicit diagnostics");
            if b.agent_id == "running" {
                assert!(cleanup.blocker.is_some());
            }
            assert_eq!(a, b);
        }
        Ok(())
    }

    #[test]
    fn latest_closed_edge_wins_and_missing_state_is_not_a_membership_filter() -> Result<()> {
        let (_dir, db) = fixture()?;
        child(&db, "parent", "historical-active", AgentStatus::Asleep)?;
        child(&db, "parent", "no-runtime-state", AgentStatus::Asleep)?;
        let now = Utc::now();
        db.agent_canonical_relations()
            .upsert_supervision(&AgentSupervisionRecord {
                supervision_id: "newer-closed".into(),
                supervisor_agent_id: "parent".into(),
                child_agent_id: "historical-active".into(),
                delegated_from_work_item_id: None,
                delegated_from_task_id: None,
                state: AgentSupervisionState::Closed,
                revision: 99,
                created_at: now,
                updated_at: now,
            })?;
        db.transaction(|tx| {
            tx.execute(
                "DELETE FROM agent_states WHERE agent_id = 'no-runtime-state'",
                [],
            )?;
            Ok(())
        })?;
        let page = db.list_subagents("parent", 20, None, false)?;
        assert_eq!(page.subagents.len(), 1);
        assert_eq!(page.subagents[0].agent_id, "no-runtime-state");
        assert!(page.subagents[0].runtime_status.is_none());
        Ok(())
    }

    #[test]
    fn pagination_over_sixteen_is_complete_and_limits_are_strict() -> Result<()> {
        let (_dir, db) = fixture()?;
        for n in 0..37 {
            child(&db, "parent", &format!("child-{n:02}"), AgentStatus::Asleep)?;
        }
        for limit in [0, MAX_SUBAGENT_LIST_LIMIT + 1, usize::MAX] {
            assert!(db.list_subagents("parent", limit, None, false).is_err());
        }
        let empty = db.list_subagents("empty", 1, None, false)?;
        assert!(empty.subagents.is_empty());
        assert!(empty.next_cursor.is_none());
        for limit in [1, 7, 20, 100] {
            let mut cursor = None;
            let mut found = Vec::new();
            loop {
                let page = db.list_subagents("parent", limit, cursor.as_deref(), false)?;
                assert_eq!(page.limit, limit);
                assert!(page.subagents.len() <= limit);
                found.extend(page.subagents.into_iter().map(|c| c.agent_id));
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
                assert!(found.len() < 37);
            }
            assert_eq!(
                found,
                (0..37).map(|n| format!("child-{n:02}")).collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn cursors_reject_malformed_foreign_and_stale_anchors() -> Result<()> {
        let (_dir, db) = fixture()?;
        for id in ["a", "b", "c"] {
            child(&db, "parent", id, AgentStatus::Asleep)?;
        }
        for bad in ["", "not a cursor", "e30", &"a".repeat(4097)] {
            assert!(db.list_subagents("parent", 1, Some(bad), false).is_err());
        }
        let cursor = db
            .list_subagents("parent", 1, None, false)?
            .next_cursor
            .unwrap();
        assert!(db.list_subagents("other", 1, Some(&cursor), false).is_err());
        let (_foreign_dir, foreign_db) = fixture()?;
        for id in ["a", "b", "c"] {
            child(&foreign_db, "parent", id, AgentStatus::Asleep)?;
        }
        assert!(foreign_db
            .list_subagents("parent", 1, Some(&cursor), false)
            .is_err());
        let mut decoded: SubagentCursor =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&cursor)?)?;
        decoded.version = 99;
        let bad_version = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&decoded)?);
        assert!(db
            .list_subagents("parent", 1, Some(&bad_version), false)
            .is_err());
        close(&db, "a")?;
        assert!(db
            .list_subagents("parent", 1, Some(&cursor), false)
            .unwrap_err()
            .to_string()
            .contains("stale"));
        Ok(())
    }

    #[test]
    fn continuation_rechecks_removed_children_and_identity_incarnations() -> Result<()> {
        let (_dir, db) = fixture()?;
        for id in ["a", "b", "c"] {
            child(&db, "parent", id, AgentStatus::Asleep)?;
        }
        let cursor = db
            .list_subagents("parent", 1, None, false)?
            .next_cursor
            .unwrap();
        close(&db, "b")?;
        let next = db.list_subagents("parent", 1, Some(&cursor), false)?;
        assert_eq!(next.subagents[0].agent_id, "c");
        assert!(next.next_cursor.is_none());
        db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json = json_set(payload_json, '$.incarnation', 2)
                 WHERE agent_id = 'a'", [],
            )?;
            Ok(())
        })?;
        assert!(db
            .list_subagents("parent", 1, Some(&cursor), false)
            .is_err());
        db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json = json_set(payload_json, '$.incarnation', 2)
                 WHERE agent_id = 'parent'", [],
            )?;
            Ok(())
        })?;
        assert!(db
            .list_subagents("parent", 1, Some(&cursor), false)
            .is_err());
        Ok(())
    }

    #[test]
    fn default_query_uses_scalar_projections_not_task_or_state_evidence() -> Result<()> {
        let (_dir, db) = fixture()?;
        child(&db, "parent", "child", AgentStatus::Asleep)?;
        db.transaction(|tx| {
            tx.execute("UPDATE tasks SET payload_json = 'not task evidence'", [])?;
            tx.execute(
                "UPDATE agent_supervisions SET payload_json = 'not relationship evidence'",
                [],
            )?;
            tx.execute(
                "UPDATE agent_states SET payload_json =
                 json_object('status', 'asleep', 'last_turn_terminal', 'not state evidence')",
                [],
            )?;
            Ok(())
        })?;
        let page = db.list_subagents("parent", 20, None, false)?;
        assert_eq!(page.subagents.len(), 1);
        assert_eq!(page.subagents[0].runtime_status, Some(AgentStatus::Asleep));
        assert_eq!(
            page.subagents[0].invocation_task.as_ref().unwrap().status,
            Some(TaskStatus::Completed)
        );
        db.with_read(|c| {
            assert_eq!(
                c.query_row("SELECT payload_json FROM tasks", [], |r| r
                    .get::<_, String>(0))?,
                "not task evidence"
            );
            assert_eq!(
                c.query_row("SELECT state FROM agent_supervisions", [], |r| r
                    .get::<_, String>(0))?,
                "active"
            );
            Ok(())
        })
    }
}
