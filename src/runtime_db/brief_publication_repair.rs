use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::{
    runtime_db::{
        evidence::{append_audit_event_tx, upsert_brief_with_created_event_seq_tx},
        RuntimeDb,
    },
    types::{
        brief_created_event_for, stable_brief_created_event_id, AuditEvent, BriefRecord,
        TurnRecord, TurnTerminalKind, WaitConditionRecord,
    },
};

#[derive(Debug, Clone, Serialize)]
pub struct WaitFinalBriefPublicationRepairReport {
    pub apply: bool,
    pub agent_id: Option<String>,
    pub plan_path: Option<PathBuf>,
    pub resumed: bool,
    pub scanned_briefs: usize,
    pub repairable_briefs: usize,
    pub repaired_briefs: usize,
    pub skipped_briefs: usize,
    pub backup_path: Option<PathBuf>,
    pub diagnostics: Vec<WaitFinalBriefPublicationRepairDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaitFinalBriefPublicationRepairDiagnostic {
    pub brief_id: String,
    pub agent_id: String,
    pub turn_id: Option<String>,
    pub status: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitFinalBriefPublicationRepairPhase {
    Briefs,
    Turns,
    Events,
    Finalize,
    Complete,
    Apply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitFinalBriefPublicationRepairProgress {
    pub phase: WaitFinalBriefPublicationRepairPhase,
    pub processed: usize,
    pub total: Option<usize>,
    pub checkpoint: Option<String>,
}

impl fmt::Display for WaitFinalBriefPublicationRepairProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.phase, self.processed)?;
        if let Some(total) = self.total {
            write!(formatter, "/{total}")?;
        }
        if let Some(checkpoint) = &self.checkpoint {
            write!(formatter, " ({checkpoint})")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitFinalBriefPublicationRepairPlanStatus {
    pub plan_path: PathBuf,
    pub complete: bool,
    pub agent_id: Option<String>,
    pub repairable_briefs: usize,
    pub skipped_briefs: usize,
}

#[derive(Debug, Clone)]
struct BriefRow {
    brief_id: String,
    agent_id: String,
    turn_id: Option<String>,
    work_item_id: Option<String>,
    payload_json: String,
}

#[derive(Debug, Clone)]
struct RepairCandidate {
    brief: BriefRecord,
    turn: TurnRecord,
}

impl RuntimeDb {
    pub fn create_wait_final_brief_publication_repair_backup(&self) -> Result<PathBuf> {
        self.create_verified_backup("wait-final-brief-publication")
    }

    pub fn prepare_wait_final_brief_publication_repair(
        &self,
        plan_path: Option<&Path>,
        resume: bool,
        agent_id: Option<&str>,
        diagnostic_sample_limit: usize,
        mut progress: impl FnMut(&WaitFinalBriefPublicationRepairProgress),
    ) -> Result<WaitFinalBriefPublicationRepairReport> {
        let Some(plan_path) = plan_path else {
            anyhow::ensure!(!resume, "--resume requires a repair plan path");
            let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
            let temp_plan_path = parent.join(format!(
                ".wait-final-brief-publication-{}.sqlite",
                uuid::Uuid::new_v4()
            ));
            let result = (|| {
                self.prepare_wait_final_brief_publication_repair_plan(
                    &temp_plan_path,
                    agent_id,
                    &mut progress,
                )?;
                let plan = Connection::open(&temp_plan_path)?;
                let mut report = report_from_plan(&plan, false, diagnostic_sample_limit, None)?;
                report.plan_path = None;
                Ok(report)
            })();
            remove_temporary_plan(&temp_plan_path);
            return result;
        };
        if resume {
            anyhow::ensure!(
                plan_path.exists(),
                "repair plan {} does not exist",
                plan_path.display()
            );
        } else {
            anyhow::ensure!(
                !plan_path.exists(),
                "repair plan {} already exists; use resume to continue it",
                plan_path.display()
            );
        }
        self.prepare_wait_final_brief_publication_repair_plan(plan_path, agent_id, &mut progress)?;
        let plan = Connection::open(plan_path)?;
        let mut report = report_from_plan(&plan, false, diagnostic_sample_limit, None)?;
        report.plan_path = Some(plan_path.to_path_buf());
        report.resumed = resume;
        Ok(report)
    }

    fn prepare_wait_final_brief_publication_repair_plan(
        &self,
        plan_path: &Path,
        agent_id: Option<&str>,
        progress: &mut dyn FnMut(&WaitFinalBriefPublicationRepairProgress),
    ) -> Result<WaitFinalBriefPublicationRepairPlanStatus> {
        const BATCH_SIZE: i64 = 256;
        let source_path = std::fs::canonicalize(&self.path)
            .unwrap_or_else(|_| self.path.clone())
            .to_string_lossy()
            .into_owned();
        let source_schema = self.current_schema_version()?;
        let source = self.connection()?;
        let mut plan = Connection::open(plan_path)
            .with_context(|| format!("opening repair plan {}", plan_path.display()))?;
        plan.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS candidates (
               brief_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, turn_id TEXT,
               status TEXT NOT NULL, reason TEXT NOT NULL, source_fingerprint TEXT
             );
             CREATE TABLE IF NOT EXISTS scan_briefs (
               brief_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, turn_id TEXT,
               work_item_id TEXT, payload_hash TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS turn_refs (
               brief_id TEXT PRIMARY KEY, ref_count INTEGER NOT NULL,
               turn_id TEXT
             );
             CREATE TABLE IF NOT EXISTS event_refs (
               brief_id TEXT PRIMARY KEY, ref_count INTEGER NOT NULL,
               audit_event_id TEXT
             );
             CREATE TABLE IF NOT EXISTS inflight_turns (
               turn_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL
             );",
        )?;
        let existing_format = plan_metadata(&plan, "format")?;
        let effective_agent_id = if let Some(format) = existing_format {
            anyhow::ensure!(
                format == "holon.wait-final-brief-publication-repair.v2",
                "unsupported repair plan format {format}"
            );
            let stored_agent_id = plan_metadata(&plan, "agent_id")?.unwrap_or_default();
            if let Some(agent_id) = agent_id {
                anyhow::ensure!(
                    stored_agent_id == agent_id,
                    "repair plan agent scope does not match"
                );
            }
            validate_plan_identity(
                &plan,
                &source_path,
                source_schema,
                (!stored_agent_id.is_empty()).then_some(stored_agent_id.as_str()),
            )?;
            if plan_metadata(&plan, "state")?.as_deref() == Some("complete") {
                return plan_status(plan_path, &plan);
            }
            (!stored_agent_id.is_empty()).then_some(stored_agent_id)
        } else {
            let tx = plan.transaction()?;
            for (key, value) in [
                (
                    "format",
                    "holon.wait-final-brief-publication-repair.v2".to_string(),
                ),
                ("source_path", source_path),
                ("source_schema", source_schema.to_string()),
                ("agent_id", agent_id.unwrap_or("").to_string()),
                ("state", "briefs".to_string()),
                (
                    "brief_high_water",
                    source
                        .query_row("SELECT COALESCE(MAX(rowid),0) FROM briefs", [], |r| {
                            r.get::<_, i64>(0)
                        })?
                        .to_string(),
                ),
                (
                    "turn_high_water",
                    source
                        .query_row("SELECT COALESCE(MAX(rowid),0) FROM turn_records", [], |r| {
                            r.get::<_, i64>(0)
                        })?
                        .to_string(),
                ),
                (
                    "event_high_water",
                    source
                        .query_row("SELECT COALESCE(MAX(rowid),0) FROM audit_events", [], |r| {
                            r.get::<_, i64>(0)
                        })?
                        .to_string(),
                ),
                ("brief_checkpoint", "0".to_string()),
                ("brief_processed", "0".to_string()),
                ("turn_checkpoint", "0".to_string()),
                ("turn_processed", "0".to_string()),
                ("event_checkpoint", "0".to_string()),
                ("event_processed", "0".to_string()),
                ("finalize_checkpoint", String::new()),
                ("finalize_processed", "0".to_string()),
            ] {
                tx.execute(
                    "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                    params![key, value],
                )?;
            }
            tx.commit()?;
            agent_id.map(str::to_owned)
        };
        let agent_id = effective_agent_id.as_deref();

        loop {
            let state = plan_metadata(&plan, "state")?.context("repair plan state is missing")?;
            match state.as_str() {
                "briefs" => {
                    let checkpoint = plan_metadata_i64(&plan, "brief_checkpoint")?;
                    let high_water = plan_metadata_i64(&plan, "brief_high_water")?;
                    let mut stmt = source.prepare(
                        "SELECT rowid, evidence_id, agent_id, turn_id, work_item_id, payload_json
                         FROM briefs
                         WHERE kind = 'result' AND created_event_seq IS NULL
                           AND (?1 IS NULL OR agent_id = ?1) AND rowid > ?2 AND rowid <= ?3
                         ORDER BY rowid LIMIT ?4",
                    )?;
                    let rows = stmt
                        .query_map(
                            params![agent_id, checkpoint, high_water, BATCH_SIZE],
                            |row| {
                                Ok((
                                    row.get::<_, i64>(0)?,
                                    BriefRow {
                                        brief_id: row.get(1)?,
                                        agent_id: row.get(2)?,
                                        turn_id: row.get(3)?,
                                        work_item_id: row.get(4)?,
                                        payload_json: row.get(5)?,
                                    },
                                ))
                            },
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if rows.is_empty() {
                        set_plan_metadata(&plan, "state", "turns")?;
                        continue;
                    }
                    let batch_len = rows.len();
                    let last = rows.last().unwrap().0;
                    let tx = plan.transaction()?;
                    for (_, row) in rows {
                        if serde_json::from_str::<BriefRecord>(&row.payload_json)
                            .is_ok_and(|brief| brief.finalizes_assistant_round_id.is_some())
                        {
                            tx.execute(
                                "INSERT OR IGNORE INTO scan_briefs VALUES (?1,?2,?3,?4,?5)",
                                params![
                                    row.brief_id,
                                    row.agent_id,
                                    row.turn_id,
                                    row.work_item_id,
                                    sha256_hex(row.payload_json.as_bytes())
                                ],
                            )?;
                        }
                    }
                    increment_plan_processed_tx(&tx, "brief_processed", batch_len)?;
                    set_plan_metadata_tx(&tx, "brief_checkpoint", &last.to_string())?;
                    tx.commit()?;
                    emit_plan_progress(
                        &plan,
                        progress,
                        WaitFinalBriefPublicationRepairPhase::Briefs,
                        Some(last.to_string()),
                    )?;
                }
                "turns" => {
                    let checkpoint = plan_metadata_i64(&plan, "turn_checkpoint")?;
                    let high_water = plan_metadata_i64(&plan, "turn_high_water")?;
                    let mut stmt = source.prepare(
                        "SELECT rowid, turn_id, payload_json FROM turn_records
                         WHERE (?1 IS NULL OR agent_id = ?1) AND rowid > ?2 AND rowid <= ?3
                         ORDER BY rowid LIMIT ?4",
                    )?;
                    let rows = stmt
                        .query_map(
                            params![agent_id, checkpoint, high_water, BATCH_SIZE],
                            |row| {
                                Ok((
                                    row.get::<_, i64>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, String>(2)?,
                                ))
                            },
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if rows.is_empty() {
                        set_plan_metadata(&plan, "state", "events")?;
                        continue;
                    }
                    let batch_len = rows.len();
                    let last = rows.last().unwrap().0;
                    let tx = plan.transaction()?;
                    for (_, turn_id, payload) in rows {
                        let Ok(turn) = serde_json::from_str::<TurnRecord>(&payload) else {
                            continue;
                        };
                        if turn.terminal.is_none() {
                            tx.execute(
                                "INSERT OR IGNORE INTO inflight_turns VALUES (?1,?2)",
                                params![turn_id, turn.agent_id],
                            )?;
                        }
                        for brief_id in &turn.produced_brief_ids {
                            tx.execute(
                                "INSERT INTO turn_refs(brief_id,ref_count,turn_id)
                                 SELECT brief_id,1,?2 FROM scan_briefs WHERE brief_id=?1 AND agent_id=?3
                                 ON CONFLICT(brief_id) DO UPDATE SET ref_count=ref_count+1, turn_id=NULL",
                                params![brief_id,turn_id,turn.agent_id])?;
                        }
                    }
                    increment_plan_processed_tx(&tx, "turn_processed", batch_len)?;
                    set_plan_metadata_tx(&tx, "turn_checkpoint", &last.to_string())?;
                    tx.commit()?;
                    emit_plan_progress(
                        &plan,
                        progress,
                        WaitFinalBriefPublicationRepairPhase::Turns,
                        Some(last.to_string()),
                    )?;
                }
                "events" => {
                    let checkpoint = plan_metadata_i64(&plan, "event_checkpoint")?;
                    let high_water = plan_metadata_i64(&plan, "event_high_water")?;
                    let mut stmt = source.prepare(
                        "SELECT rowid, audit_event_id, data_json FROM audit_events
                         WHERE kind='brief_created' AND (?1 IS NULL OR agent_id=?1)
                           AND rowid > ?2 AND rowid <= ?3 ORDER BY rowid LIMIT ?4",
                    )?;
                    let rows = stmt
                        .query_map(
                            params![agent_id, checkpoint, high_water, BATCH_SIZE],
                            |row| {
                                Ok((
                                    row.get::<_, i64>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, String>(2)?,
                                ))
                            },
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if rows.is_empty() {
                        set_plan_metadata(&plan, "state", "finalize")?;
                        continue;
                    }
                    let batch_len = rows.len();
                    let last = rows.last().unwrap().0;
                    let tx = plan.transaction()?;
                    for (_, event_id, payload) in rows {
                        let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                            continue;
                        };
                        let Some(brief_id) =
                            value.pointer("/data/brief_id").and_then(|v| v.as_str())
                        else {
                            continue;
                        };
                        tx.execute(
                            "INSERT INTO event_refs(brief_id,ref_count,audit_event_id)
                             SELECT brief_id,1,?2 FROM scan_briefs WHERE brief_id=?1
                             ON CONFLICT(brief_id) DO UPDATE SET ref_count=ref_count+1, audit_event_id=NULL",
                            params![brief_id,event_id])?;
                    }
                    increment_plan_processed_tx(&tx, "event_processed", batch_len)?;
                    set_plan_metadata_tx(&tx, "event_checkpoint", &last.to_string())?;
                    tx.commit()?;
                    emit_plan_progress(
                        &plan,
                        progress,
                        WaitFinalBriefPublicationRepairPhase::Events,
                        Some(last.to_string()),
                    )?;
                }
                "finalize" => {
                    let checkpoint =
                        plan_metadata(&plan, "finalize_checkpoint")?.unwrap_or_default();
                    let mut stmt = plan.prepare(
                        "SELECT b.brief_id,b.agent_id,b.turn_id,b.work_item_id,
                                COALESCE(t.ref_count,0),t.turn_id,COALESCE(e.ref_count,0),e.audit_event_id
                         FROM scan_briefs b LEFT JOIN turn_refs t USING(brief_id)
                         LEFT JOIN event_refs e USING(brief_id)
                         WHERE b.brief_id>?1 ORDER BY b.brief_id LIMIT ?2")?;
                    let rows = stmt
                        .query_map(params![checkpoint, BATCH_SIZE], |r| {
                            Ok((
                                BriefRow {
                                    brief_id: r.get(0)?,
                                    agent_id: r.get(1)?,
                                    turn_id: r.get(2)?,
                                    work_item_id: r.get(3)?,
                                    payload_json: String::new(),
                                },
                                r.get::<_, i64>(4)?,
                                r.get::<_, Option<String>>(5)?,
                                r.get::<_, i64>(6)?,
                                r.get::<_, Option<String>>(7)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    drop(stmt);
                    if rows.is_empty() {
                        let tx = plan.transaction()?;
                        set_plan_metadata_tx(&tx, "state", "complete")?;
                        tx.commit()?;
                        progress(&WaitFinalBriefPublicationRepairProgress {
                            phase: WaitFinalBriefPublicationRepairPhase::Complete,
                            processed: plan.query_row(
                                "SELECT COUNT(*) FROM candidates",
                                [],
                                |r| r.get::<_, i64>(0),
                            )? as usize,
                            total: None,
                            checkpoint: None,
                        });
                        continue;
                    }
                    let batch_len = rows.len();
                    let last = rows.last().unwrap().0.brief_id.clone();
                    let tx = plan.transaction()?;
                    for (mut row, turn_count, turn_id, event_count, event_id) in rows {
                        row.payload_json = source.query_row(
                            "SELECT payload_json FROM briefs WHERE evidence_id=?1",
                            [&row.brief_id],
                            |r| r.get(0),
                        )?;
                        let turn_json = turn_id
                            .map(|id| {
                                source.query_row(
                                    "SELECT payload_json FROM turn_records WHERE turn_id=?1",
                                    [id],
                                    |r| r.get(0),
                                )
                            })
                            .transpose()?;
                        let event_json = event_id
                            .map(|id| {
                                source.query_row(
                                    "SELECT data_json FROM audit_events WHERE audit_event_id=?1",
                                    [id],
                                    |r| r.get(0),
                                )
                            })
                            .transpose()?;
                        let result = inspect_candidate_scanned(
                            &source,
                            row.clone(),
                            turn_count,
                            turn_json,
                            event_count,
                            event_json,
                        );
                        match result {
                            Ok(candidate) => {
                                let fingerprint = candidate_fingerprint(&source, &candidate)?;
                                tx.execute(
                                "INSERT OR REPLACE INTO candidates VALUES (?1,?2,?3,'repairable',?4,?5)",params![candidate.brief.id,candidate.brief.agent_id,candidate.turn.turn_id,"unique completed waiting Turn and referenced WaitFor condition prove canonical Brief ownership",fingerprint])?;
                            }
                            Err(d) => {
                                tx.execute("INSERT OR REPLACE INTO candidates VALUES (?1,?2,?3,'skipped',?4,NULL)",params![d.brief_id,d.agent_id,d.turn_id,d.reason])?;
                            }
                        }
                    }
                    increment_plan_processed_tx(&tx, "finalize_processed", batch_len)?;
                    set_plan_metadata_tx(&tx, "finalize_checkpoint", &last)?;
                    tx.commit()?;
                    emit_plan_progress(
                        &plan,
                        progress,
                        WaitFinalBriefPublicationRepairPhase::Finalize,
                        Some(last),
                    )?;
                }
                "complete" => return plan_status(plan_path, &plan),
                other => anyhow::bail!("unsupported repair plan state {other}"),
            }
        }
    }

    pub fn preflight_wait_final_brief_publication_repair_plan(
        &self,
        plan_path: &Path,
    ) -> Result<()> {
        let plan = Connection::open(plan_path)?;
        validate_complete_plan(self, &plan)
    }

    pub fn apply_wait_final_brief_publication_repair_plan(
        &self,
        plan_path: &Path,
        diagnostic_sample_limit: usize,
        backup_path: Option<PathBuf>,
        mut progress: impl FnMut(&WaitFinalBriefPublicationRepairProgress),
    ) -> Result<WaitFinalBriefPublicationRepairReport> {
        let plan = Connection::open(plan_path)?;
        validate_complete_plan(self, &plan)?;
        let mut statement = plan.prepare(
            "SELECT c.brief_id, c.agent_id, c.turn_id, c.source_fingerprint,
                    COALESCE(e.ref_count, 0), e.audit_event_id
             FROM candidates c
             LEFT JOIN event_refs e USING (brief_id)
             WHERE c.status = 'repairable'
             ORDER BY c.agent_id, c.brief_id",
        )?;
        let planned = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut report = report_from_plan(&plan, true, diagnostic_sample_limit, backup_path)?;
        report.plan_path = Some(plan_path.to_path_buf());
        self.transaction(|tx| {
            let candidate_keys = planned
                .iter()
                .map(|(brief_id, agent_id, _, _, _, _)| {
                    (agent_id.clone(), brief_id.clone())
                })
                .collect::<BTreeSet<_>>();
            let turn_high_water = plan_metadata_i64(&plan, "turn_high_water")?;
            let event_high_water = plan_metadata_i64(&plan, "event_high_water")?;
            let inflight = plan
                .prepare("SELECT turn_id FROM inflight_turns")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            let mut turn_stmt = tx.prepare(
                "SELECT turn_id, agent_id, payload_json
                 FROM turn_records
                 WHERE rowid > ?1
                 ORDER BY rowid",
            )?;
            let turns = turn_stmt
                .query_map([turn_high_water], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(turn_stmt);
            for (turn_id, agent_id, payload) in turns {
                let Ok(turn) = serde_json::from_str::<TurnRecord>(&payload) else {
                    continue;
                };
                for brief_id in &turn.produced_brief_ids {
                    let key = (agent_id.clone(), brief_id.clone());
                    if candidate_keys.contains(&key) {
                        anyhow::bail!(
                            "repair plan is stale: Turn {turn_id} newly references Brief {brief_id}"
                        );
                    }
                }
            }

            let mut inflight_stmt =
                tx.prepare("SELECT agent_id, payload_json FROM turn_records WHERE turn_id = ?1")?;
            for turn_id in inflight {
                let row = inflight_stmt
                    .query_row([&turn_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .optional()?;
                let Some((agent_id, payload)) = row else {
                    continue;
                };
                let Ok(turn) = serde_json::from_str::<TurnRecord>(&payload) else {
                    continue;
                };
                for brief_id in &turn.produced_brief_ids {
                    if candidate_keys.contains(&(agent_id.clone(), brief_id.clone())) {
                        anyhow::bail!(
                            "repair plan is stale: inflight Turn {turn_id} references Brief {brief_id}"
                        );
                    }
                }
            }
            drop(inflight_stmt);

            let mut event_stmt = tx.prepare(
                "SELECT agent_id, audit_event_id, data_json
                 FROM audit_events
                 WHERE rowid > ?1 AND kind = 'brief_created'
                 ORDER BY rowid",
            )?;
            let events = event_stmt
                .query_map([event_high_water], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(event_stmt);
            for (agent_id, event_id, payload) in events {
                let Some(agent_id) = agent_id else { continue };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue;
                };
                let Some(brief_id) = value.pointer("/data/brief_id").and_then(|v| v.as_str())
                else {
                    continue;
                };
                let key = (agent_id, brief_id.to_string());
                if candidate_keys.contains(&key) {
                    anyhow::bail!(
                        "repair plan is stale: brief_created event {event_id} newly conflicts with Brief {brief_id}"
                    );
                }
            }

            for (
                index,
                (brief_id, agent_id, turn_id, fingerprint, event_count, event_id),
            ) in planned.iter().enumerate()
            {
                let row = tx
                    .query_row(
                        "SELECT evidence_id,agent_id,turn_id,work_item_id,payload_json
                         FROM briefs WHERE evidence_id=?1 AND agent_id=?2
                           AND kind='result' AND created_event_seq IS NULL",
                        params![brief_id, agent_id],
                        |row| Ok(BriefRow {
                            brief_id: row.get(0)?,
                            agent_id: row.get(1)?,
                            turn_id: row.get(2)?,
                            work_item_id: row.get(3)?,
                            payload_json: row.get(4)?,
                        }),
                    )
                    .with_context(|| format!("planned Brief {brief_id} is no longer repairable"))?;
                let turn_payload = tx
                    .query_row(
                        "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                        [turn_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                let event_payload = event_id
                    .as_ref()
                    .map(|event_id| {
                        tx.query_row(
                            "SELECT data_json FROM audit_events WHERE audit_event_id = ?1",
                            [event_id],
                            |row| row.get::<_, String>(0),
                        )
                    })
                    .transpose()?;
                let current = inspect_candidate_scanned(
                    tx,
                    row,
                    i64::from(turn_payload.is_some()),
                    turn_payload,
                    *event_count,
                    event_payload,
                )
                .map_err(|diagnostic| anyhow::anyhow!(diagnostic.reason))?;
                anyhow::ensure!(current.turn.turn_id == *turn_id, "Brief {brief_id} changed Turn ownership");
                anyhow::ensure!(
                    candidate_fingerprint(tx, &current)? == *fingerprint,
                    "Brief {} changed after repair preflight",
                    current.brief.id
                );
                repair_candidate(tx, &current)?;
                report.repaired_briefs += 1;
                progress(&WaitFinalBriefPublicationRepairProgress {
                    phase: WaitFinalBriefPublicationRepairPhase::Apply,
                    processed: index + 1,
                    total: Some(planned.len()),
                    checkpoint: Some(current.brief.id),
                });
            }
            Ok(())
        })?;
        Ok(report)
    }
}

fn plan_metadata(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map_err(Into::into)
}

fn plan_metadata_i64(connection: &Connection, key: &str) -> Result<i64> {
    plan_metadata(connection, key)?
        .with_context(|| format!("repair plan metadata {key} is missing"))?
        .parse()
        .with_context(|| format!("repair plan metadata {key} is invalid"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn set_plan_metadata(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection.execute("INSERT INTO metadata(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
    Ok(())
}
fn set_plan_metadata_tx(tx: &Transaction<'_>, key: &str, value: &str) -> Result<()> {
    tx.execute("INSERT INTO metadata(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
    Ok(())
}

fn increment_plan_processed_tx(tx: &Transaction<'_>, key: &str, delta: usize) -> Result<()> {
    tx.execute(
        "INSERT INTO metadata(key,value) VALUES (?1,?2)
         ON CONFLICT(key) DO UPDATE
         SET value = CAST(metadata.value AS INTEGER) + CAST(excluded.value AS INTEGER)",
        params![key, delta.to_string()],
    )?;
    Ok(())
}

fn remove_temporary_plan(plan_path: &Path) {
    for suffix in ["", "-journal", "-shm", "-wal"] {
        let path = PathBuf::from(format!("{}{}", plan_path.display(), suffix));
        let _ = std::fs::remove_file(path);
    }
}

fn emit_plan_progress(
    plan: &Connection,
    progress: &mut dyn FnMut(&WaitFinalBriefPublicationRepairProgress),
    phase: WaitFinalBriefPublicationRepairPhase,
    checkpoint: Option<String>,
) -> Result<()> {
    let processed_key = match phase {
        WaitFinalBriefPublicationRepairPhase::Briefs => "brief_processed",
        WaitFinalBriefPublicationRepairPhase::Turns => "turn_processed",
        WaitFinalBriefPublicationRepairPhase::Events => "event_processed",
        WaitFinalBriefPublicationRepairPhase::Finalize => "finalize_processed",
        _ => return Ok(()),
    };
    let processed = plan_metadata(plan, processed_key)?
        .unwrap_or_default()
        .parse::<usize>()
        .with_context(|| format!("repair plan {processed_key} is invalid"))?;
    progress(&WaitFinalBriefPublicationRepairProgress {
        phase,
        processed,
        total: None,
        checkpoint,
    });
    Ok(())
}

fn validate_plan_identity(
    plan: &Connection,
    source_path: &str,
    source_schema: i64,
    agent_id: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        plan_metadata(plan, "source_path")?.as_deref() == Some(source_path),
        "repair plan belongs to a different runtime database"
    );
    anyhow::ensure!(
        plan_metadata(plan, "source_schema")?.as_deref()
            == Some(source_schema.to_string().as_str()),
        "repair plan schema does not match the runtime database"
    );
    anyhow::ensure!(
        plan_metadata(plan, "agent_id")?.as_deref() == Some(agent_id.unwrap_or("")),
        "repair plan agent scope does not match"
    );
    Ok(())
}

fn validate_complete_plan(db: &RuntimeDb, plan: &Connection) -> Result<()> {
    let format = plan_metadata(plan, "format")?;
    anyhow::ensure!(
        format.as_deref() == Some("holon.wait-final-brief-publication-repair.v2"),
        "unsupported repair plan format"
    );
    let source_path = std::fs::canonicalize(&db.path)
        .unwrap_or_else(|_| db.path.clone())
        .to_string_lossy()
        .into_owned();
    let agent = plan_metadata(plan, "agent_id")?.unwrap_or_default();
    validate_plan_identity(
        plan,
        &source_path,
        db.current_schema_version()?,
        (!agent.is_empty()).then_some(agent.as_str()),
    )?;
    anyhow::ensure!(
        plan_metadata(plan, "state")?.as_deref() == Some("complete"),
        "repair plan is incomplete; resume prepare before applying it"
    );
    Ok(())
}

fn plan_status(
    plan_path: &Path,
    plan: &Connection,
) -> Result<WaitFinalBriefPublicationRepairPlanStatus> {
    let repairable_briefs = plan.query_row(
        "SELECT COUNT(*) FROM candidates WHERE status = 'repairable'",
        [],
        |row| row.get::<_, i64>(0),
    )? as usize;
    let skipped_briefs = plan.query_row(
        "SELECT COUNT(*) FROM candidates WHERE status = 'skipped'",
        [],
        |row| row.get::<_, i64>(0),
    )? as usize;
    let agent_id = plan_metadata(plan, "agent_id")?.filter(|value| !value.is_empty());
    Ok(WaitFinalBriefPublicationRepairPlanStatus {
        plan_path: plan_path.to_path_buf(),
        complete: plan_metadata(plan, "state")?.as_deref() == Some("complete"),
        agent_id,
        repairable_briefs,
        skipped_briefs,
    })
}

fn report_from_plan(
    plan: &Connection,
    apply: bool,
    diagnostic_sample_limit: usize,
    backup_path: Option<PathBuf>,
) -> Result<WaitFinalBriefPublicationRepairReport> {
    let mut statement = plan.prepare(
        "SELECT brief_id, agent_id, turn_id, status, reason
         FROM candidates ORDER BY agent_id, brief_id LIMIT ?1",
    )?;
    let diagnostics = statement
        .query_map([diagnostic_sample_limit as i64], |row| {
            let status: String = row.get(3)?;
            Ok(WaitFinalBriefPublicationRepairDiagnostic {
                brief_id: row.get(0)?,
                agent_id: row.get(1)?,
                turn_id: row.get(2)?,
                status: if status == "repairable" {
                    "repairable"
                } else {
                    "skipped"
                },
                reason: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let status = plan_status(Path::new(""), plan)?;
    Ok(WaitFinalBriefPublicationRepairReport {
        apply,
        agent_id: status.agent_id,
        plan_path: None,
        resumed: false,
        scanned_briefs: status.repairable_briefs + status.skipped_briefs,
        repairable_briefs: status.repairable_briefs,
        repaired_briefs: 0,
        skipped_briefs: status.skipped_briefs,
        backup_path,
        diagnostics,
    })
}

fn candidate_fingerprint(connection: &Connection, candidate: &RepairCandidate) -> Result<String> {
    let wait_id = candidate
        .turn
        .waiting_condition_ids
        .first()
        .context("repair candidate is missing WaitFor condition")?;
    let wait_payload: String = connection.query_row(
        "SELECT payload_json FROM wait_conditions WHERE wait_condition_id = ?1",
        [wait_id],
        |row| row.get(0),
    )?;
    let event_id = stable_brief_created_event_id(&candidate.brief.agent_id, &candidate.brief.id);
    let event_row = connection
        .query_row(
            "SELECT audit_event_id, COALESCE(event_seq, -1), data_json
             FROM audit_events
             WHERE audit_event_id = ?1 AND agent_id = ?2 AND kind = 'brief_created'",
            params![event_id, &candidate.brief.agent_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let bytes = serde_json::to_vec(&(&candidate.brief, &candidate.turn, wait_payload, event_row))?;
    Ok(format!("{:x}", sha2::Sha256::digest(bytes)))
}

fn inspect_candidate_scanned(
    connection: &Connection,
    row: BriefRow,
    turn_count: i64,
    turn_payload: Option<String>,
    event_count: i64,
    event_payload: Option<String>,
) -> std::result::Result<RepairCandidate, WaitFinalBriefPublicationRepairDiagnostic> {
    let diagnostic =
        |turn_id: Option<String>, reason: String| WaitFinalBriefPublicationRepairDiagnostic {
            brief_id: row.brief_id.clone(),
            agent_id: row.agent_id.clone(),
            turn_id,
            status: "skipped",
            reason,
        };
    let mut brief: BriefRecord = serde_json::from_str(&row.payload_json).map_err(|error| {
        diagnostic(
            row.turn_id.clone(),
            format!("invalid Brief payload: {error}"),
        )
    })?;
    if brief.id != row.brief_id || brief.agent_id != row.agent_id {
        return Err(diagnostic(
            row.turn_id.clone(),
            "Brief payload identity does not match table columns".to_string(),
        ));
    }
    if brief.turn_id != row.turn_id || brief.work_item_id != row.work_item_id {
        return Err(diagnostic(
            row.turn_id.clone(),
            "Brief payload ownership does not match table columns".to_string(),
        ));
    }
    if turn_count != 1 {
        return Err(diagnostic(
            row.turn_id.clone(),
            format!(
                "Brief is referenced by {} Turn records; expected exactly one",
                turn_count
            ),
        ));
    }
    let turn: TurnRecord =
        serde_json::from_str(turn_payload.as_deref().unwrap_or("")).map_err(|error| {
            diagnostic(
                row.turn_id.clone(),
                format!("invalid TurnRecord payload: {error}"),
            )
        })?;
    if turn.agent_id != brief.agent_id || !turn.produced_brief_ids.contains(&brief.id) {
        return Err(diagnostic(
            Some(turn.turn_id),
            "TurnRecord does not canonically own the Brief".to_string(),
        ));
    }
    if turn.waiting_condition_ids.len() != 1
        || turn.terminal.as_ref().is_none_or(|terminal| {
            terminal.kind != TurnTerminalKind::Completed || terminal.no_brief_reason.is_some()
        })
    {
        return Err(diagnostic(
            Some(turn.turn_id),
            "Turn is not a completed WaitFor final publication".to_string(),
        ));
    }
    let wait_condition_id = &turn.waiting_condition_ids[0];
    let wait_row = connection
        .query_row(
            "SELECT agent_id, work_item_id, payload_json
             FROM wait_conditions
             WHERE wait_condition_id = ?1",
            [wait_condition_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| {
            diagnostic(
                Some(turn.turn_id.clone()),
                format!("failed to load referenced WaitFor condition: {error}"),
            )
        })?
        .ok_or_else(|| {
            diagnostic(
                Some(turn.turn_id.clone()),
                "referenced WaitFor condition is missing".to_string(),
            )
        })?;
    let wait: WaitConditionRecord = serde_json::from_str(&wait_row.2).map_err(|error| {
        diagnostic(
            Some(turn.turn_id.clone()),
            format!("invalid referenced WaitFor condition payload: {error}"),
        )
    })?;
    if wait.id != *wait_condition_id
        || wait.agent_id != wait_row.0
        || wait.work_item_id != wait_row.1
        || wait.agent_id != brief.agent_id
        || wait.source.as_deref() != Some("WaitFor")
        || wait.turn_id.as_deref() != Some(turn.turn_id.as_str())
    {
        return Err(diagnostic(
            Some(turn.turn_id),
            "referenced wait condition does not uniquely prove WaitFor ownership".to_string(),
        ));
    }
    if brief
        .turn_id
        .as_deref()
        .is_some_and(|turn_id| turn_id != turn.turn_id)
    {
        return Err(diagnostic(
            Some(turn.turn_id),
            "Brief already names a different Turn".to_string(),
        ));
    }
    if brief.work_item_id.is_some() && brief.work_item_id != wait.work_item_id {
        return Err(diagnostic(
            Some(turn.turn_id),
            "Brief already names a different WorkItem than the referenced WaitFor condition"
                .to_string(),
        ));
    }

    brief.turn_id = Some(turn.turn_id.clone());
    brief.work_item_id = wait.work_item_id;
    if let Err(error) =
        ensure_created_event_payload_compatible(event_count, event_payload.as_deref(), &brief)
    {
        return Err(diagnostic(Some(turn.turn_id), error.to_string()));
    }
    Ok(RepairCandidate { brief, turn })
}

fn ensure_created_event_payload_compatible(
    event_count: i64,
    payload: Option<&str>,
    brief: &BriefRecord,
) -> Result<()> {
    anyhow::ensure!(
        event_count <= 1,
        "multiple brief_created events already reference this Brief"
    );
    let Some(payload) = payload else {
        return Ok(());
    };
    let existing: AuditEvent =
        serde_json::from_str(payload).context("invalid existing brief_created event")?;
    let expected = brief_created_event_for(brief)?;
    anyhow::ensure!(
        existing.id == stable_brief_created_event_id(&brief.agent_id, &brief.id)
            && existing.created_at == expected.created_at
            && existing.kind == expected.kind
            && existing.contract_version == expected.contract_version
            && existing.payload_schema == expected.payload_schema
            && existing.payload_schema_version == expected.payload_schema_version
            && existing.data == expected.data,
        "existing brief_created event conflicts with repaired Brief content"
    );
    Ok(())
}

fn repair_candidate(tx: &Transaction<'_>, candidate: &RepairCandidate) -> Result<()> {
    let brief = &candidate.brief;
    let changed = tx.execute(
        "UPDATE briefs
         SET turn_id = ?1,
             work_item_id = ?2,
             payload_json = ?3
         WHERE evidence_id = ?4
           AND agent_id = ?5
           AND created_event_seq IS NULL",
        params![
            brief.turn_id.as_deref(),
            brief.work_item_id.as_deref(),
            serde_json::to_string(brief)?,
            &brief.id,
            &brief.agent_id,
        ],
    )?;
    anyhow::ensure!(
        changed == 1,
        "Brief {} changed after repair preflight",
        brief.id
    );
    let event = brief_created_event_for(brief)?;
    let (event, _) = append_audit_event_tx(tx, Some(&brief.agent_id), &event)?;
    upsert_brief_with_created_event_seq_tx(tx, brief, event.event_seq)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use tempfile::tempdir;

    use super::*;
    use crate::{
        runtime_db::RuntimeDb,
        types::{BriefKind, TurnTerminalSummary, WaitConditionKind, WaitConditionStatus},
    };

    fn insert_repairable_candidate(db: &RuntimeDb, suffix: &str, turn_index: u64) -> Result<()> {
        let agent_id = "agent-a";
        let turn_id = format!("turn-{suffix}");
        let wait_id = format!("wait-{suffix}");
        let brief_id = format!("brief-{suffix}");
        let mut turn = TurnRecord::new(agent_id, &turn_id, turn_index);
        turn.waiting_condition_ids.push(wait_id.clone());
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 1,
        });
        let mut brief = BriefRecord::new(
            agent_id,
            BriefKind::Result,
            format!("result-{suffix}"),
            Some(format!("message-{suffix}")),
            None,
        );
        brief.id = brief_id;
        brief.turn_index = Some(turn_index);
        brief.finalizes_assistant_round_id = Some(format!("assistant-round-{suffix}"));
        turn.produced_brief_ids.push(brief.id.clone());
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO turn_records (
                    turn_id, turn_index, agent_id, terminal_kind, created_at, completed_at,
                    payload_json
                 ) VALUES (?1, ?2, ?3, 'completed', ?4, ?5, ?6)",
                params![
                    &turn.turn_id,
                    turn.turn_index as i64,
                    &turn.agent_id,
                    turn.created_at.to_rfc3339(),
                    turn.terminal
                        .as_ref()
                        .map(|terminal| terminal.completed_at.to_rfc3339()),
                    serde_json::to_string(&turn)?,
                ],
            )?;
            tx.execute(
                "INSERT INTO briefs (
                    evidence_id, agent_id, message_id, created_at, kind, preview, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, 'result', ?5, ?6)",
                params![
                    &brief.id,
                    &brief.agent_id,
                    brief.related_message_id.as_deref(),
                    brief.created_at.to_rfc3339(),
                    &brief.text,
                    serde_json::to_string(&brief)?,
                ],
            )?;
            Ok(())
        })?;
        db.wait_conditions().upsert(&WaitConditionRecord {
            id: wait_id,
            agent_id: agent_id.into(),
            work_item_id: Some(format!("work-{suffix}")),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("WaitFor".into()),
            subject_ref: Some(format!("github:holon-run/holon#{suffix}")),
            waiting_for: "reviewer merge".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some(turn_id),
            trigger_message_id: None,
            triggered_at: None,
        })?;
        Ok(())
    }

    #[test]
    fn repairs_wait_final_brief_publication_idempotently() -> Result<()> {
        let dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.sqlite.lock"),
        )?;
        let mut turn = TurnRecord::new("agent-a", "turn-a", 1);
        turn.current_work_item_id = Some("work-current".into());
        turn.waiting_condition_ids.push("wait-a".into());
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 1,
        });
        let mut brief = BriefRecord::new(
            "agent-a",
            BriefKind::Result,
            "result",
            Some("message-a".into()),
            None,
        );
        brief.id = "brief-a".into();
        brief.turn_index = Some(1);
        brief.finalizes_assistant_round_id = Some("assistant-round-a".into());
        turn.produced_brief_ids.push(brief.id.clone());
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO turn_records (
                    turn_id, turn_index, agent_id, terminal_kind, created_at, completed_at,
                    payload_json
                 ) VALUES (?1, ?2, ?3, 'completed', ?4, ?5, ?6)",
                params![
                    &turn.turn_id,
                    turn.turn_index as i64,
                    &turn.agent_id,
                    turn.created_at.to_rfc3339(),
                    turn.terminal
                        .as_ref()
                        .map(|terminal| terminal.completed_at.to_rfc3339()),
                    serde_json::to_string(&turn)?,
                ],
            )?;
            tx.execute(
                "INSERT INTO briefs (
                    evidence_id, agent_id, message_id, created_at, kind, preview, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, 'result', ?5, ?6)",
                params![
                    &brief.id,
                    &brief.agent_id,
                    brief.related_message_id.as_deref(),
                    brief.created_at.to_rfc3339(),
                    &brief.text,
                    serde_json::to_string(&brief)?,
                ],
            )?;
            tx.execute(
                "INSERT INTO turn_records (
                    turn_id, turn_index, agent_id, created_at, payload_json
                 ) VALUES ('turn-corrupt', 2, 'agent-b', ?1, 'not-json')",
                [chrono::Utc::now().to_rfc3339()],
            )?;
            tx.execute(
                "INSERT INTO briefs (
                    evidence_id, agent_id, created_at, kind, preview, payload_json
                 ) VALUES ('brief-corrupt', 'agent-b', ?1, 'result', 'corrupt', 'not-json')",
                [chrono::Utc::now().to_rfc3339()],
            )?;
            Ok(())
        })?;
        db.wait_conditions().upsert(&WaitConditionRecord {
            id: "wait-a".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some("work-explicit".into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("WaitFor".into()),
            subject_ref: Some("github:holon-run/holon#3034".into()),
            waiting_for: "reviewer merge".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("turn-a".into()),
            trigger_message_id: None,
            triggered_at: None,
        })?;

        let plan_path = dir.path().join("repair-plan.sqlite");
        let interrupted = catch_unwind(AssertUnwindSafe(|| {
            let mut batches = 0;
            db.prepare_wait_final_brief_publication_repair(
                Some(&plan_path),
                false,
                Some("agent-a"),
                10,
                |_| {
                    batches += 1;
                    if batches == 1 {
                        panic!("simulated interruption after committed batch");
                    }
                },
            )
        }));
        assert!(interrupted.is_err());
        let partial = Connection::open(&plan_path)?;
        assert_eq!(plan_metadata(&partial, "state")?.as_deref(), Some("briefs"));
        assert_eq!(
            partial.query_row("SELECT COUNT(*) FROM scan_briefs", [], |row| {
                row.get::<_, i64>(0)
            })?,
            1
        );
        assert_eq!(
            plan_metadata(&partial, "brief_processed")?.as_deref(),
            Some("1")
        );
        drop(partial);

        let resumed = db.prepare_wait_final_brief_publication_repair(
            Some(&plan_path),
            true,
            None,
            10,
            |_| {},
        )?;
        assert!(resumed.resumed);
        assert_eq!(resumed.agent_id.as_deref(), Some("agent-a"));
        assert_eq!(resumed.repairable_briefs, 1);
        assert_eq!(resumed.skipped_briefs, 0);
        let plan = Connection::open(&plan_path)?;
        for table in ["scan_briefs", "turn_refs", "event_refs", "inflight_turns"] {
            let columns = plan
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(!columns.iter().any(|column| {
                column == "payload_json"
                    || column.ends_with("_json")
                    || column.contains("body")
                    || column.contains("text")
            }));
        }
        drop(plan);
        db.preflight_wait_final_brief_publication_repair_plan(&plan_path)?;
        let applied =
            db.apply_wait_final_brief_publication_repair_plan(&plan_path, 10, None, |_| {})?;
        assert_eq!(applied.repaired_briefs, 1);
        let stored = db
            .evidence()
            .brief_by_id("agent-a", "brief-a")?
            .expect("repaired Brief");
        assert_eq!(stored.turn_id.as_deref(), Some("turn-a"));
        assert_eq!(stored.work_item_id.as_deref(), Some("work-explicit"));
        assert!(stored.created_event_seq.is_some());
        let conversation = db.conversation().summary_page("agent-a", 10, None, None)?;
        let repaired_turn = conversation
            .turns
            .iter()
            .find(|turn| turn.turn_id == "turn-a")
            .expect("repaired Turn in conversation");
        assert!(repaired_turn.brief_ids.contains(&"brief-a".to_string()));
        assert_eq!(
            repaired_turn.result,
            crate::domain::conversation::ResultState::Available
        );
        let events = db.audit_events().recent(Some("agent-a"), 10)?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "brief_created")
                .count(),
            1
        );

        let repeated =
            db.prepare_wait_final_brief_publication_repair(None, false, None, 10, |_| {})?;
        assert_eq!(repeated.repairable_briefs, 0);
        assert_eq!(repeated.repaired_briefs, 0);
        assert_eq!(
            db.audit_events()
                .recent(Some("agent-a"), 10)?
                .iter()
                .filter(|event| event.kind == "brief_created")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn skips_brief_without_referenced_wait_condition() -> Result<()> {
        let dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.sqlite.lock"),
        )?;
        let mut turn = TurnRecord::new("agent-a", "turn-orphan", 1);
        turn.waiting_condition_ids.push("wait-missing".into());
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 1,
        });
        let mut brief = BriefRecord::new("agent-a", BriefKind::Result, "orphan", None, None);
        brief.id = "brief-orphan".into();
        brief.finalizes_assistant_round_id = Some("assistant-round-a".into());
        turn.produced_brief_ids.push(brief.id.clone());
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO turn_records (
                    turn_id, turn_index, agent_id, terminal_kind, created_at, completed_at,
                    payload_json
                 ) VALUES (?1, ?2, ?3, 'completed', ?4, ?5, ?6)",
                params![
                    &turn.turn_id,
                    turn.turn_index as i64,
                    &turn.agent_id,
                    turn.created_at.to_rfc3339(),
                    turn.terminal
                        .as_ref()
                        .map(|terminal| terminal.completed_at.to_rfc3339()),
                    serde_json::to_string(&turn)?,
                ],
            )?;
            tx.execute(
                "INSERT INTO briefs (
                    evidence_id, agent_id, created_at, kind, preview, payload_json
                 ) VALUES (?1, ?2, ?3, 'result', ?4, ?5)",
                params![
                    &brief.id,
                    &brief.agent_id,
                    brief.created_at.to_rfc3339(),
                    &brief.text,
                    serde_json::to_string(&brief)?,
                ],
            )?;
            Ok(())
        })?;

        let report =
            db.prepare_wait_final_brief_publication_repair(None, false, None, 10, |_| {})?;
        assert_eq!(report.scanned_briefs, 1);
        assert_eq!(report.repairable_briefs, 0);
        assert_eq!(report.repaired_briefs, 0);
        assert_eq!(report.skipped_briefs, 1);
        assert_eq!(report.diagnostics[0].status, "skipped");
        assert!(report.diagnostics[0]
            .reason
            .contains("referenced WaitFor condition is missing"));
        assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        let stored = db
            .evidence()
            .brief_by_id("agent-a", "brief-orphan")?
            .expect("orphan Brief remains stored");
        assert!(stored.turn_id.is_none());
        assert!(stored.created_event_seq.is_none());
        Ok(())
    }

    #[test]
    fn stale_candidate_rolls_back_the_entire_plan() -> Result<()> {
        let dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.sqlite.lock"),
        )?;
        insert_repairable_candidate(&db, "a", 1)?;
        insert_repairable_candidate(&db, "b", 2)?;
        let plan_path = dir.path().join("repair-plan.sqlite");
        let prepared = db.prepare_wait_final_brief_publication_repair(
            Some(&plan_path),
            false,
            None,
            10,
            |_| {},
        )?;
        assert_eq!(prepared.repairable_briefs, 2);

        db.connection()?.execute(
            "UPDATE wait_conditions
             SET payload_json = json_set(payload_json, '$.waiting_for', 'changed after prepare')
             WHERE wait_condition_id = 'wait-b'",
            [],
        )?;
        let error = db
            .apply_wait_final_brief_publication_repair_plan(&plan_path, 10, None, |_| {})
            .expect_err("stale candidate must reject the complete apply transaction");
        assert!(error.to_string().contains("changed after repair preflight"));

        for brief_id in ["brief-a", "brief-b"] {
            let stored = db
                .evidence()
                .brief_by_id("agent-a", brief_id)?
                .expect("candidate Brief remains stored");
            assert!(stored.turn_id.is_none());
            assert!(stored.created_event_seq.is_none());
        }
        assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn turn_inserted_after_prepare_makes_plan_stale_without_writes() -> Result<()> {
        let dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.sqlite.lock"),
        )?;
        insert_repairable_candidate(&db, "a", 1)?;
        let plan_path = dir.path().join("repair-plan.sqlite");
        db.prepare_wait_final_brief_publication_repair(Some(&plan_path), false, None, 10, |_| {})?;
        let mut turn = TurnRecord::new("agent-a", "turn-second", 2);
        turn.produced_brief_ids.push("brief-a".into());
        db.connection()?.execute(
            "INSERT INTO turn_records(turn_id,turn_index,agent_id,created_at,payload_json)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                turn.turn_id,
                turn.turn_index as i64,
                turn.agent_id,
                turn.created_at.to_rfc3339(),
                serde_json::to_string(&turn)?,
            ],
        )?;
        let error = db
            .apply_wait_final_brief_publication_repair_plan(&plan_path, 10, None, |_| {})
            .expect_err("new Turn reference must stale the plan");
        assert!(error.to_string().contains("newly references"));
        let brief = db
            .evidence()
            .brief_by_id("agent-a", "brief-a")?
            .expect("Brief remains");
        assert!(brief.turn_id.is_none());
        assert!(brief.created_event_seq.is_none());
        Ok(())
    }

    #[test]
    fn non_stable_created_event_inserted_after_prepare_makes_plan_stale() -> Result<()> {
        let dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.sqlite.lock"),
        )?;
        insert_repairable_candidate(&db, "a", 1)?;
        let plan_path = dir.path().join("repair-plan.sqlite");
        db.prepare_wait_final_brief_publication_repair(Some(&plan_path), false, None, 10, |_| {})?;
        let brief = db
            .evidence()
            .brief_by_id("agent-a", "brief-a")?
            .expect("Brief exists");
        let mut event = brief_created_event_for(&brief)?;
        event.id = "non-stable-created-event".into();
        db.connection()?.execute(
            "INSERT INTO audit_events(audit_event_id,agent_id,kind,created_at,data_json)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                event.id,
                brief.agent_id,
                event.kind,
                event.created_at.to_rfc3339(),
                serde_json::to_string(&event)?,
            ],
        )?;
        let error = db
            .apply_wait_final_brief_publication_repair_plan(&plan_path, 10, None, |_| {})
            .expect_err("new conflicting event must stale the plan");
        assert!(error.to_string().contains("newly conflicts"));
        let brief = db
            .evidence()
            .brief_by_id("agent-a", "brief-a")?
            .expect("Brief remains");
        assert!(brief.turn_id.is_none());
        assert!(brief.created_event_seq.is_none());
        Ok(())
    }
}
