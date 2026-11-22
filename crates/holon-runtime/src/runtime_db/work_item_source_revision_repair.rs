//! Offline, evidence-fenced repair for #3816 stranded registrations.
//!
//! Audit lists every registered open WorkItem whose execution registration
//! source revision trails its durable revision, together with the
//! per-revision attribution evidence verdict. Apply adopts a drifted
//! registration back to the durable revision only when the WorkItem is
//! open, its agent has no open execution attempt, and every intermediate revision has a
//! `work_item_written` attribution event. The repair never rewrites
//! admitted fences, generations, wait state, or attempt history, and it is
//! idempotent: a consistent registration is reported and left untouched.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    domain::execution_protocol::{self, WorkItemExecutionState},
    runtime_db::{
        connection::ensure_runtime_db_sidecars_are_consistent,
        evidence::append_audit_event_tx,
        transitions::{
            execution_protocol_repository::{
                load_state_unchecked_tx, missing_revision_attribution_tx,
            },
            persist_state_tx,
        },
        RuntimeDb, RUNTIME_DB_BUSY_TIMEOUT,
    },
    types::{AuditEvent, WorkItemRecord, WorkItemState},
};

#[derive(Debug, Clone, Serialize)]
pub struct WorkItemSourceRevisionRepairEntry {
    pub agent_id: String,
    pub work_item_id: String,
    pub source_revision: u64,
    pub durable_revision: u64,
    pub registration_state: String,
    pub missing_revision_evidence: Vec<u64>,
    pub verdict: &'static str,
    pub repaired: bool,
    pub already_consistent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkItemSourceRevisionRepairReport {
    pub apply: bool,
    pub agent_id: Option<String>,
    pub work_item_id: Option<String>,
    pub scanned_registrations: usize,
    pub repaired: usize,
    pub backup_path: Option<PathBuf>,
    pub entries: Vec<WorkItemSourceRevisionRepairEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkItemSourceRevisionBatchFailure {
    pub agent_id: String,
    pub work_item_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkItemSourceRevisionBatchEntry {
    pub agent_id: String,
    pub work_item_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkItemSourceRevisionBatchReport {
    pub agent_id: Option<String>,
    pub scanned_registrations: usize,
    pub repairable: usize,
    pub repaired: usize,
    pub failed: Option<WorkItemSourceRevisionBatchFailure>,
    pub untried: Vec<WorkItemSourceRevisionBatchEntry>,
    pub backup_path: Option<PathBuf>,
    pub entries: Vec<WorkItemSourceRevisionRepairEntry>,
}

impl RuntimeDb {
    pub fn create_work_item_source_revision_repair_backup(&self) -> Result<PathBuf> {
        self.create_verified_backup("work-item-source-revision")
    }

    pub fn audit_work_item_source_revision_repair_read_only(
        path: &Path,
        agent_id: Option<&str>,
        work_item_id: Option<&str>,
    ) -> Result<WorkItemSourceRevisionRepairReport> {
        let mut connection = open_existing_read_only(path)?;
        audit_with_connection(&mut connection, agent_id, work_item_id)
    }

    /// Apply is offline-only: the caller must hold the maintenance and
    /// migration locks so `holon serve` cannot race the repair.
    pub fn apply_work_item_source_revision_repair(
        &self,
        agent_id: &str,
        work_item_id: &str,
        backup_path: Option<PathBuf>,
    ) -> Result<WorkItemSourceRevisionRepairReport> {
        let mut report = self.transaction(|tx| {
            let mut report = audit_tx(tx, Some(agent_id), Some(work_item_id))?;
            for entry in report.entries.iter_mut() {
                if entry.verdict != "repairable" {
                    continue;
                }
                apply_entry_tx(tx, entry)?;
            }
            Ok(report)
        })?;
        report.apply = true;
        report.backup_path = backup_path;
        report.repaired = report.entries.iter().filter(|entry| entry.repaired).count();
        Ok(report)
    }

    /// Offline batch variant of the single-item repair. The caller holds
    /// the maintenance and migration locks for the whole batch. The audit
    /// runs first: with nothing repairable, no backup is created and no
    /// write happens. Otherwise one verified backup is created before the
    /// first repair (unless `no_backup`) and reused for every item, each
    /// in its own fenced transaction. The first real error stops the
    /// batch; committed repairs stay committed and the report names the
    /// failure and the untried remainder. Re-running re-audits and is
    /// idempotent.
    pub fn apply_work_item_source_revision_repair_batch(
        &self,
        agent_id: Option<&str>,
        no_backup: bool,
    ) -> Result<WorkItemSourceRevisionBatchReport> {
        let audit = self.transaction(|tx| audit_tx(tx, agent_id, None))?;
        let plan: Vec<(String, String)> = audit
            .entries
            .iter()
            .filter(|entry| entry.verdict == "repairable")
            .map(|entry| (entry.agent_id.clone(), entry.work_item_id.clone()))
            .collect();
        let mut report = WorkItemSourceRevisionBatchReport {
            agent_id: agent_id.map(str::to_string),
            scanned_registrations: audit.scanned_registrations,
            repairable: plan.len(),
            repaired: 0,
            failed: None,
            untried: Vec::new(),
            backup_path: None,
            entries: audit.entries,
        };
        if plan.is_empty() {
            return Ok(report);
        }
        if !no_backup {
            report.backup_path = Some(self.create_work_item_source_revision_repair_backup()?);
        }
        let executed = execute_repair_plan(&plan, |agent, work_item| {
            self.apply_work_item_source_revision_repair(agent, work_item, None)
                .map(|applied| applied.repaired)
        });
        report.repaired = executed.repaired;
        report.failed = executed.failed;
        report.untried = executed.untried;
        // Refresh entries so the report reflects the state after whatever
        // prefix of the plan actually committed.
        report.entries = self.transaction(|tx| audit_tx(tx, agent_id, None))?.entries;
        Ok(report)
    }
}

fn open_existing_read_only(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        bail!(
            "WorkItem source revision repair requires an existing runtime database: {}",
            path.display()
        );
    }
    ensure_runtime_db_sidecars_are_consistent(path)?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening runtime db read-only {}", path.display()))?;
    connection.busy_timeout(RUNTIME_DB_BUSY_TIMEOUT)?;
    connection.execute_batch("PRAGMA query_only = ON;")?;
    ensure_runtime_db_sidecars_are_consistent(path)?;
    Ok(connection)
}

fn audit_with_connection(
    connection: &mut Connection,
    agent_id: Option<&str>,
    work_item_id: Option<&str>,
) -> Result<WorkItemSourceRevisionRepairReport> {
    let mut report = WorkItemSourceRevisionRepairReport {
        apply: false,
        agent_id: agent_id.map(str::to_string),
        work_item_id: work_item_id.map(str::to_string),
        scanned_registrations: 0,
        repaired: 0,
        backup_path: None,
        entries: Vec::new(),
    };
    let mut statement = connection.prepare(
        "SELECT w.agent_id, w.work_item_id, w.payload_json, e.payload_json
         FROM work_items w JOIN execution_protocol_work_items e
           ON e.agent_id = w.agent_id AND e.work_item_id = w.work_item_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    for (row_agent, row_work_item, work_payload, execution_payload) in rows {
        if agent_id.is_some_and(|filter| filter != row_agent)
            || work_item_id.is_some_and(|filter| filter != row_work_item)
        {
            continue;
        }
        report.scanned_registrations += 1;
        let Ok(work) = serde_json::from_str::<WorkItemRecord>(&work_payload) else {
            continue;
        };
        let Ok(execution) =
            serde_json::from_str::<execution_protocol::WorkItemExecutionRecord>(&execution_payload)
        else {
            continue;
        };
        if work.state != WorkItemState::Open {
            continue;
        }
        let entry = inspect_entry(connection, &row_agent, &row_work_item, &work, &execution)?;
        report.entries.push(entry);
    }
    Ok(report)
}

fn inspect_entry(
    connection: &mut Connection,
    agent_id: &str,
    work_item_id: &str,
    work: &WorkItemRecord,
    execution: &execution_protocol::WorkItemExecutionRecord,
) -> Result<WorkItemSourceRevisionRepairEntry> {
    let mut entry = WorkItemSourceRevisionRepairEntry {
        agent_id: agent_id.to_string(),
        work_item_id: work_item_id.to_string(),
        source_revision: execution.source_revision,
        durable_revision: work.revision,
        registration_state: registration_state_label(&execution.state),
        missing_revision_evidence: Vec::new(),
        verdict: "consistent",
        repaired: false,
        already_consistent: false,
    };
    if execution.source_revision == work.revision {
        entry.already_consistent = true;
        return Ok(entry);
    }
    if work.revision < execution.source_revision {
        // Registration ahead of durable state is not the repairable drift
        // shape; fail-closed and let an operator inspect the evidence.
        entry.verdict = "blocked_registration_ahead";
        return Ok(entry);
    }
    let tx = connection.transaction()?;
    if agent_has_open_attempt(&tx, agent_id)? {
        entry.verdict = "blocked_in_flight";
        return Ok(entry);
    }
    let missing = missing_revision_attribution_tx(
        &tx,
        agent_id,
        work_item_id,
        execution.source_revision + 1,
        work.revision,
    )?;
    tx.commit()?;
    entry.missing_revision_evidence = missing;
    entry.verdict = if entry.missing_revision_evidence.is_empty() {
        "repairable"
    } else {
        "blocked_missing_evidence"
    };
    Ok(entry)
}

pub(super) fn agent_has_open_attempt(connection: &Connection, agent_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts
         WHERE agent_id = ?1 AND lifecycle_state = 'open')",
        [agent_id],
        |row| row.get(0),
    )?)
}

fn registration_state_label(state: &WorkItemExecutionState) -> String {
    match state {
        WorkItemExecutionState::Runnable { .. } => "runnable".into(),
        WorkItemExecutionState::Waiting { .. } => "waiting".into(),
        WorkItemExecutionState::Paused { .. } => "paused".into(),
        WorkItemExecutionState::Terminal { .. } => "terminal".into(),
    }
}

fn audit_tx(
    tx: &rusqlite::Transaction<'_>,
    agent_id: Option<&str>,
    work_item_id: Option<&str>,
) -> Result<WorkItemSourceRevisionRepairReport> {
    let mut report = WorkItemSourceRevisionRepairReport {
        apply: false,
        agent_id: agent_id.map(str::to_string),
        work_item_id: work_item_id.map(str::to_string),
        scanned_registrations: 0,
        repaired: 0,
        backup_path: None,
        entries: Vec::new(),
    };
    let mut statement = tx.prepare(
        "SELECT w.agent_id, w.work_item_id, w.payload_json, e.payload_json
         FROM work_items w JOIN execution_protocol_work_items e
           ON e.agent_id = w.agent_id AND e.work_item_id = w.work_item_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (row_agent, row_work_item, work_payload, execution_payload) in rows {
        if agent_id.is_some_and(|filter| filter != row_agent)
            || work_item_id.is_some_and(|filter| filter != row_work_item)
        {
            continue;
        }
        report.scanned_registrations += 1;
        let Ok(work) = serde_json::from_str::<WorkItemRecord>(&work_payload) else {
            continue;
        };
        let Ok(execution) =
            serde_json::from_str::<execution_protocol::WorkItemExecutionRecord>(&execution_payload)
        else {
            continue;
        };
        if work.state != WorkItemState::Open {
            continue;
        }
        let entry = inspect_entry_tx(tx, &row_agent, &row_work_item, &work, &execution)?;
        report.entries.push(entry);
    }
    Ok(report)
}

fn inspect_entry_tx(
    tx: &rusqlite::Transaction<'_>,
    agent_id: &str,
    work_item_id: &str,
    work: &WorkItemRecord,
    execution: &execution_protocol::WorkItemExecutionRecord,
) -> Result<WorkItemSourceRevisionRepairEntry> {
    let mut entry = WorkItemSourceRevisionRepairEntry {
        agent_id: agent_id.to_string(),
        work_item_id: work_item_id.to_string(),
        source_revision: execution.source_revision,
        durable_revision: work.revision,
        registration_state: registration_state_label(&execution.state),
        missing_revision_evidence: Vec::new(),
        verdict: "consistent",
        repaired: false,
        already_consistent: false,
    };
    if execution.source_revision == work.revision {
        entry.already_consistent = true;
        return Ok(entry);
    }
    if work.revision < execution.source_revision {
        entry.verdict = "blocked_registration_ahead";
        return Ok(entry);
    }
    if agent_has_open_attempt(tx, agent_id)? {
        entry.verdict = "blocked_in_flight";
        return Ok(entry);
    }
    entry.missing_revision_evidence = missing_revision_attribution_tx(
        tx,
        agent_id,
        work_item_id,
        execution.source_revision + 1,
        work.revision,
    )?;
    entry.verdict = if entry.missing_revision_evidence.is_empty() {
        "repairable"
    } else {
        "blocked_missing_evidence"
    };
    Ok(entry)
}

fn apply_entry_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &mut WorkItemSourceRevisionRepairEntry,
) -> Result<()> {
    let state = load_state_unchecked_tx(tx, &entry.agent_id)?;
    let Some(execution) = state.work_items.get(&entry.work_item_id) else {
        bail!(
            "WorkItem source revision repair target vanished: {}/{}",
            entry.agent_id,
            entry.work_item_id
        );
    };
    if execution.source_revision != entry.source_revision {
        bail!(
            "WorkItem source revision repair fence changed for {}/{}",
            entry.agent_id,
            entry.work_item_id
        );
    }
    let fingerprint = evidence_fingerprint(tx, &entry.agent_id, &entry.work_item_id, entry)?;
    let command = execution_protocol::ReconcileWorkItemSourceRevision {
        command_id: format!(
            "work_item:reconcile_source_revision:{}:{}",
            entry.work_item_id, entry.durable_revision
        ),
        work_item_id: entry.work_item_id.clone(),
        expected_source_revision: entry.source_revision,
        source_revision: entry.durable_revision,
        evidence_fingerprint: fingerprint,
    };
    let transition = execution_protocol::reconcile_work_item_source_revision(&state, &command)
        .map_err(|error| {
            anyhow!(
                "WorkItem source revision repair rejected for {}/{}: {error}",
                entry.agent_id,
                entry.work_item_id
            )
        })?;
    persist_state_tx(tx, &transition.state)?;
    tx.execute(
        "INSERT OR IGNORE INTO execution_protocol_command_results (
           agent_id, command_kind, command_identity,
           payload_hash, references_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            entry.agent_id,
            "reconcile_work_item_source_revision",
            command.command_id,
            format!("{:x}", Sha256::digest(serde_json::to_vec(&command)?)),
            serde_json::to_string(&transition.references)?,
            Utc::now().to_rfc3339(),
        ],
    )?;
    append_audit_event_tx(
        tx,
        Some(&entry.agent_id),
        &AuditEvent::legacy(
            "work_item_source_revision_reconciled",
            serde_json::json!({
                "work_item_id": entry.work_item_id,
                "source_revision": entry.source_revision,
                "durable_revision": entry.durable_revision,
                "evidence_fingerprint": command.evidence_fingerprint,
                "repair": "work_item_source_revision_repair",
            }),
        ),
    )?;
    entry.repaired = true;
    Ok(())
}

fn evidence_fingerprint(
    tx: &rusqlite::Transaction<'_>,
    agent_id: &str,
    work_item_id: &str,
    entry: &WorkItemSourceRevisionRepairEntry,
) -> Result<String> {
    let mut statement = tx.prepare(
        "SELECT audit_event_id FROM audit_events
         WHERE agent_id = ?1 AND kind = 'work_item_written'
         ORDER BY audit_event_id",
    )?;
    let ids = statement
        .query_map([agent_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut hasher = Sha256::new();
    for id in ids {
        hasher.update(id.as_bytes());
    }
    hasher.update(work_item_id.as_bytes());
    hasher.update(entry.source_revision.to_le_bytes());
    hasher.update(entry.durable_revision.to_le_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

struct RepairPlanOutcome {
    repaired: usize,
    failed: Option<WorkItemSourceRevisionBatchFailure>,
    untried: Vec<WorkItemSourceRevisionBatchEntry>,
}

/// Applies the plan one item at a time, stopping at the first real error.
/// Blocked or already-consistent items never reach this loop; audit
/// verdicts are outcomes, not errors.
fn execute_repair_plan(
    plan: &[(String, String)],
    mut apply_one: impl FnMut(&str, &str) -> Result<usize>,
) -> RepairPlanOutcome {
    let mut repaired = 0;
    let mut failed = None;
    let mut untried = Vec::new();
    for (agent_id, work_item_id) in plan {
        if failed.is_some() {
            untried.push(WorkItemSourceRevisionBatchEntry {
                agent_id: agent_id.clone(),
                work_item_id: work_item_id.clone(),
            });
            continue;
        }
        match apply_one(agent_id, work_item_id) {
            Ok(count) => repaired += count,
            Err(error) => {
                failed = Some(WorkItemSourceRevisionBatchFailure {
                    agent_id: agent_id.clone(),
                    work_item_id: work_item_id.clone(),
                    error: error.to_string(),
                });
            }
        }
    }
    RepairPlanOutcome {
        repaired,
        failed,
        untried,
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution_protocol::{
        ExecutionProtocolState, WorkItemExecutionRecord, WorkItemExecutionState,
    };
    use crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx;

    fn runtime_db() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        Ok((dir, db))
    }

    fn drifted_fixture(db: &RuntimeDb, work_item_id: &str, with_evidence: bool) -> Result<()> {
        let mut work = WorkItemRecord::new("agent-a", "drifted", WorkItemState::Open);
        work.id = work_item_id.to_string();
        db.work_items().insert_new(&work)?;
        let mut protocol = ExecutionProtocolState::empty("agent-a");
        protocol.work_items.insert(
            work.id.clone(),
            WorkItemExecutionRecord {
                source_revision: 1,
                state: WorkItemExecutionState::Waiting {
                    generation: 1,
                    wait: execution_protocol::WaitReference {
                        wait_id: "wait-drift-repair".into(),
                    },
                },
            },
        );
        db.transaction(|tx| persist_state_tx(tx, &protocol))?;
        // Strand the durable revision ahead of the registration.
        let mut drifted = work.clone();
        drifted.revision = 2;
        drifted.updated_at = Utc::now();
        db.transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET revision = ?1, payload_json = ?2 WHERE work_item_id = ?3",
                rusqlite::params![2, serde_json::to_string(&drifted)?, work.id],
            )?;
            if with_evidence {
                append_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::legacy(
                        "work_item_written",
                        serde_json::json!({
                            "work_item_id": work.id,
                            "revision": 2,
                        }),
                    ),
                )?;
            }
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn an_open_conversation_attempt_blocks_unrelated_work_item_revision_repair() -> Result<()> {
        use crate::domain::execution_protocol::{
            AdmitExecution, AdmittedFences, ExecutionAttempt, ExecutionAttemptState,
            ExecutionBinding, ExecutionOrigin, ExecutionPriority, ExecutionProvenance,
            ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
        };

        let (_dir, db) = runtime_db()?;
        drifted_fixture(&db, "work-repairable", true)?;
        db.transaction(|tx| {
            let baseline = load_state_unchecked_tx(tx, "agent-a")?;
            let admitted = execution_protocol::admit_execution(
                &baseline,
                &AdmitExecution {
                    attempt: ExecutionAttempt {
                        attempt_id: "attempt-a".into(),
                        agent_id: "agent-a".into(),
                        source_message_id: Some("message-a".into()),
                        input_projection: None,
                        source: ExecutionSource {
                            identity: ExecutionSourceIdentity::QueueMessage {
                                message_id: "message-a".into(),
                            },
                            generation: 1,
                        },
                        binding: ExecutionBinding::Conversation {
                            interaction_id: "interaction-a".into(),
                        },
                        provenance: ExecutionProvenance {
                            origin: ExecutionOrigin::System,
                            trust: ExecutionTrust::RuntimeInstruction,
                            priority: ExecutionPriority::Normal,
                            correlation_id: None,
                            causation_id: None,
                        },
                        admitted_fences: AdmittedFences {
                            source_revision: 1,
                            work_item_source_revision: None,
                            work_item_generation: None,
                            rejoin: None,
                            agent_control_revision: 1,
                            host_registry_revision: 1,
                        },
                        state: ExecutionAttemptState::Open,
                        run_id: None,
                        turn_id: None,
                        recovery_of_attempt_id: None,
                        terminal_outcome_id: None,
                        admitted_at: "2026-08-01T00:00:00Z".into(),
                        terminal_at: None,
                    },
                },
            )
            .map_err(anyhow::Error::msg)?;
            persist_state_tx(tx, &admitted.state)
        })?;

        let audit = db.transaction(|tx| audit_tx(tx, Some("agent-a"), Some("work-repairable")))?;
        assert_eq!(audit.entries[0].verdict, "blocked_in_flight");
        let applied =
            db.apply_work_item_source_revision_repair("agent-a", "work-repairable", None)?;
        assert_eq!(applied.repaired, 0);
        assert_eq!(applied.entries[0].verdict, "blocked_in_flight");
        db.transaction(|tx| {
            let state = load_state_unchecked_tx(tx, "agent-a")?;
            assert_eq!(state.work_items["work-repairable"].source_revision, 1);
            assert_eq!(state.open_attempt().unwrap().attempt_id, "attempt-a");
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn audit_reports_evidence_gaps_and_apply_is_fenced_and_idempotent() -> Result<()> {
        let (dir, db) = runtime_db()?;
        drifted_fixture(&db, "work-repairable", true)?;
        drifted_fixture(&db, "work-missing-evidence", false)?;
        let report = db.transaction(|tx| audit_tx(tx, None, None))?;
        assert_eq!(report.entries.len(), 2);
        let repairable = report
            .entries
            .iter()
            .find(|entry| entry.work_item_id == "work-repairable")
            .unwrap();
        assert_eq!(repairable.verdict, "repairable");
        assert!(repairable.missing_revision_evidence.is_empty());
        let blocked = report
            .entries
            .iter()
            .find(|entry| entry.work_item_id == "work-missing-evidence")
            .unwrap();
        assert_eq!(blocked.verdict, "blocked_missing_evidence");
        assert_eq!(blocked.missing_revision_evidence, vec![2]);

        let applied =
            db.apply_work_item_source_revision_repair("agent-a", "work-repairable", None)?;
        assert_eq!(applied.repaired, 1);
        let state = db
            .transitions()
            .load_execution_protocol_state_if_initialized("agent-a")?
            .unwrap();
        assert_eq!(state.work_items["work-repairable"].source_revision, 2);
        // The waiting registration state and generation are preserved.
        assert!(matches!(
            state.work_items["work-repairable"].state,
            WorkItemExecutionState::Waiting { generation: 1, .. }
        ));
        // Re-applying is a consistent no-op.
        let repeated =
            db.apply_work_item_source_revision_repair("agent-a", "work-repairable", None)?;
        assert_eq!(repeated.repaired, 0);
        assert!(repeated.entries[0].already_consistent);
        // Evidence-less drift stays blocked.
        let blocked_apply =
            db.apply_work_item_source_revision_repair("agent-a", "work-missing-evidence", None)?;
        assert_eq!(blocked_apply.repaired, 0);
        assert_eq!(blocked_apply.entries[0].verdict, "blocked_missing_evidence");

        // The repair survives a database reopen.
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        let state = reopened
            .transitions()
            .load_execution_protocol_state_if_initialized("agent-a")?
            .unwrap();
        assert_eq!(state.work_items["work-repairable"].source_revision, 2);
        Ok(())
    }

    fn repair_backups(dir: &tempfile::TempDir) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir.path().join("state"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension().is_some_and(|ext| ext == "bak")
                            && path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| name.contains("work-item-source-revision"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn batch_apply_repairs_all_with_one_shared_backup_and_is_idempotent() -> Result<()> {
        let (dir, db) = runtime_db()?;
        for id in ["batch-1", "batch-2", "batch-3"] {
            drifted_fixture(&db, id, true)?;
        }
        drifted_fixture(&db, "batch-blocked", false)?;
        let report = db.apply_work_item_source_revision_repair_batch(None, false)?;
        assert_eq!(report.repairable, 3);
        assert_eq!(report.repaired, 3);
        let backup = report.backup_path.as_ref().expect("one shared backup");
        assert!(backup.is_file());
        assert_eq!(repair_backups(&dir).len(), 1);
        // The shared backup is a complete, openable database.
        let integrity: String =
            rusqlite::Connection::open(backup)?
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        assert_eq!(integrity, "ok");
        // Evidence gaps stay blocked instead of being forced through.
        assert!(report
            .entries
            .iter()
            .any(|entry| entry.work_item_id == "batch-blocked"
                && entry.verdict == "blocked_missing_evidence"));
        // Re-running re-audits: nothing repairable, so no second backup.
        let repeated = db.apply_work_item_source_revision_repair_batch(None, false)?;
        assert_eq!(repeated.repaired, 0);
        assert!(repeated.backup_path.is_none());
        assert_eq!(repair_backups(&dir).len(), 1);
        Ok(())
    }

    #[test]
    fn batch_apply_without_repairable_entries_creates_no_backup() -> Result<()> {
        let (dir, db) = runtime_db()?;
        drifted_fixture(&db, "batch-blocked", false)?;
        let report = db.apply_work_item_source_revision_repair_batch(None, false)?;
        assert_eq!(report.repairable, 0);
        assert_eq!(report.repaired, 0);
        assert!(report.backup_path.is_none());
        assert_eq!(repair_backups(&dir).len(), 0);
        Ok(())
    }

    #[test]
    fn batch_apply_respects_agent_filter() -> Result<()> {
        let (dir, db) = runtime_db()?;
        drifted_fixture(&db, "batch-1", true)?;
        let report = db.apply_work_item_source_revision_repair_batch(Some("agent-b"), false)?;
        assert_eq!(report.scanned_registrations, 0);
        assert_eq!(report.repairable, 0);
        assert!(report.backup_path.is_none());
        assert_eq!(repair_backups(&dir).len(), 0);
        let report = db.apply_work_item_source_revision_repair_batch(Some("agent-a"), false)?;
        assert_eq!(report.repaired, 1);
        assert_eq!(repair_backups(&dir).len(), 1);
        Ok(())
    }

    #[test]
    fn batch_apply_no_backup_flag_repairs_without_artifact() -> Result<()> {
        let (dir, db) = runtime_db()?;
        drifted_fixture(&db, "batch-1", true)?;
        let report = db.apply_work_item_source_revision_repair_batch(None, true)?;
        assert_eq!(report.repaired, 1);
        assert!(report.backup_path.is_none());
        assert_eq!(repair_backups(&dir).len(), 0);
        Ok(())
    }

    #[test]
    fn batch_apply_backup_failure_leaves_repairs_unapplied() -> Result<()> {
        let (dir, db) = runtime_db()?;
        drifted_fixture(&db, "batch-1", true)?;
        drifted_fixture(&db, "batch-2", true)?;
        let _guard = holon_storage::maintenance::test_hooks::install(
            holon_storage::maintenance::test_hooks::Faults {
                vacuum_failure: Some(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "injected vacuum failure",
                )),
                ..Default::default()
            },
        );
        let result = db.apply_work_item_source_revision_repair_batch(None, false);
        drop(_guard);
        assert!(result.is_err());
        // Zero business writes: both entries stay repairable and no backup
        // artifact survives.
        assert_eq!(repair_backups(&dir).len(), 0);
        let audit = db.transaction(|tx| audit_tx(tx, None, None))?;
        assert_eq!(
            audit
                .entries
                .iter()
                .filter(|entry| entry.verdict == "repairable")
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn repair_plan_stops_at_first_failure_and_reports_remainder() -> Result<()> {
        let plan = vec![
            ("agent-a".to_string(), "work-1".to_string()),
            ("agent-a".to_string(), "work-2".to_string()),
            ("agent-b".to_string(), "work-3".to_string()),
        ];
        let mut calls = 0;
        let outcome = execute_repair_plan(&plan, |agent, work| {
            calls += 1;
            match calls {
                1 => Ok(1),
                2 => Err(anyhow!("injected apply failure for {agent}/{work}")),
                _ => Ok(1),
            }
        });
        assert_eq!(outcome.repaired, 1);
        let failed = outcome.failed.expect("failure recorded");
        assert_eq!(failed.work_item_id, "work-2");
        assert!(failed.error.contains("injected apply failure"));
        assert_eq!(
            outcome
                .untried
                .iter()
                .map(|entry| entry.work_item_id.as_str())
                .collect::<Vec<_>>(),
            vec!["work-3"]
        );
        Ok(())
    }
}
