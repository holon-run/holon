use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    domain::execution_protocol::{
        self, ExecutionAttempt, ExecutionAttemptState, InterruptExecution,
    },
    runtime_db::{
        evidence::{append_audit_event_tx, upsert_agent_state_tx},
        migrations::current_schema_version,
        repositories::{upsert_queue_entry_tx, upsert_turn_record_tx},
        transitions::{execution_protocol_repository::load_state_unchecked_tx, persist_state_tx},
        RuntimeDb, RUNTIME_DB_BUSY_TIMEOUT,
    },
    types::{
        AgentState, AgentStatus, AuditEvent, BriefKind, BriefRecord, MessageEnvelope,
        QueueEntryRecord, QueueEntryStatus, RuntimeFailurePhase, RuntimeFailureSummary,
        ToolExecutionRecord, ToolExecutionStatus, TurnNoBriefReason, TurnRecord, TurnTerminalKind,
        TurnTerminalRecord, TurnTerminalSummary,
    },
};

const PLAN_FORMAT: &str = "holon.turn-settlement-repair.v1";
const REPAIR_REASON: &str = "historical_terminal_settlement_reconciliation";
const PAGE_SIZE: i64 = 256;

#[derive(Debug, Clone, Serialize)]
pub struct TurnSettlementRepairReport {
    pub apply: bool,
    pub agent_id: Option<String>,
    pub turn_id: Option<String>,
    pub plan_path: Option<PathBuf>,
    pub scanned_turns: usize,
    pub repairable_turns: usize,
    pub repaired_turns: usize,
    pub already_settled_turns: usize,
    pub skipped_turns: usize,
    pub backup_path: Option<PathBuf>,
    pub diagnostics: Vec<TurnSettlementRepairDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSettlementRepairDiagnostic {
    pub agent_id: String,
    pub turn_id: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSettlementRepairPhase {
    Scanning,
    Applying,
    Complete,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnSettlementRepairProgress {
    pub phase: TurnSettlementRepairPhase,
    pub scanned_turns: usize,
    pub repairable_turns: usize,
    pub skipped_turns: usize,
}

impl fmt::Display for TurnSettlementRepairProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "phase={:?} scanned={} repairable={} skipped={}",
            self.phase, self.scanned_turns, self.repairable_turns, self.skipped_turns
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TurnSettlementRepairPlan {
    format: String,
    source_path: String,
    schema_version: i64,
    created_at: String,
    agent_id: Option<String>,
    turn_id: Option<String>,
    candidates: Vec<TurnSettlementRepairCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TurnSettlementRepairCandidate {
    agent_id: String,
    turn_id: String,
    message_id: String,
    attempt_id: String,
    terminal_tool_ids: Vec<String>,
    brief_ids: Vec<String>,
    wait_ids: Vec<String>,
    source_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
struct WaitEvidence {
    wait_id: String,
    status: String,
}

struct CandidateSnapshot {
    turn: TurnRecord,
    queue: QueueEntryRecord,
    attempt: ExecutionAttempt,
    agent_state: Option<AgentState>,
    briefs: Vec<BriefRecord>,
    tools: Vec<ToolExecutionRecord>,
    waits: Vec<WaitEvidence>,
    terminal_tool_ids: Vec<String>,
    source_fingerprint: String,
}

enum CandidateInspection {
    Repairable {
        candidate: TurnSettlementRepairCandidate,
        snapshot: CandidateSnapshot,
    },
    Skipped(TurnSettlementRepairDiagnostic),
}

enum ApplyDisposition {
    Applied,
    AlreadySettled,
}

impl RuntimeDb {
    pub fn create_turn_settlement_repair_backup(&self) -> Result<PathBuf> {
        self.create_verified_backup("turn-settlement")
    }

    pub fn prepare_turn_settlement_repair(
        &self,
        plan_path: Option<&Path>,
        agent_id: Option<&str>,
        turn_id: Option<&str>,
        diagnostic_sample_limit: usize,
        progress: impl FnMut(&TurnSettlementRepairProgress),
    ) -> Result<TurnSettlementRepairReport> {
        let connection = self.connection()?;
        prepare_turn_settlement_repair_with_connection(
            &connection,
            self.path(),
            plan_path,
            agent_id,
            turn_id,
            diagnostic_sample_limit,
            progress,
        )
    }

    pub fn prepare_turn_settlement_repair_read_only(
        path: &Path,
        plan_path: Option<&Path>,
        agent_id: Option<&str>,
        turn_id: Option<&str>,
        diagnostic_sample_limit: usize,
        progress: impl FnMut(&TurnSettlementRepairProgress),
    ) -> Result<TurnSettlementRepairReport> {
        let connection = open_existing_read_only(path)?;
        prepare_turn_settlement_repair_with_connection(
            &connection,
            path,
            plan_path,
            agent_id,
            turn_id,
            diagnostic_sample_limit,
            progress,
        )
    }

    pub fn preflight_turn_settlement_repair_plan_read_only(
        path: &Path,
        plan_path: &Path,
    ) -> Result<()> {
        let connection = open_existing_read_only(path)?;
        let plan = read_plan(plan_path)?;
        validate_plan(&connection, path, &plan)
    }

    pub fn preflight_turn_settlement_repair_plan(&self, plan_path: &Path) -> Result<()> {
        let connection = self.connection()?;
        let plan = read_plan(plan_path)?;
        validate_plan(&connection, self.path(), &plan)
    }

    pub fn apply_turn_settlement_repair_plan(
        &self,
        plan_path: &Path,
        backup_path: Option<PathBuf>,
        diagnostic_sample_limit: usize,
        mut progress: impl FnMut(&TurnSettlementRepairProgress),
    ) -> Result<TurnSettlementRepairReport> {
        let connection = self.connection()?;
        let plan = read_plan(plan_path)?;
        validate_plan(&connection, self.path(), &plan)?;
        let candidate_count = plan.candidates.len();
        let (repaired_turns, already_settled_turns, diagnostics) = self.transaction(|tx| {
            let mut repaired = 0;
            let mut already_settled = 0;
            let mut diagnostics = Vec::new();
            for candidate in &plan.candidates {
                match apply_candidate(tx, candidate)? {
                    ApplyDisposition::Applied => {
                        repaired += 1;
                        push_diagnostic(
                            &mut diagnostics,
                            diagnostic_sample_limit,
                            TurnSettlementRepairDiagnostic {
                                agent_id: candidate.agent_id.clone(),
                                turn_id: candidate.turn_id.clone(),
                                status: "repaired".into(),
                                reason: "turn, queue, and execution attempt interrupted".into(),
                            },
                        );
                    }
                    ApplyDisposition::AlreadySettled => {
                        already_settled += 1;
                        push_diagnostic(
                            &mut diagnostics,
                            diagnostic_sample_limit,
                            TurnSettlementRepairDiagnostic {
                                agent_id: candidate.agent_id.clone(),
                                turn_id: candidate.turn_id.clone(),
                                status: "already_settled".into(),
                                reason: "repair was already applied".into(),
                            },
                        );
                    }
                }
                progress(&TurnSettlementRepairProgress {
                    phase: TurnSettlementRepairPhase::Applying,
                    scanned_turns: repaired + already_settled,
                    repairable_turns: candidate_count,
                    skipped_turns: 0,
                });
            }
            Ok((repaired, already_settled, diagnostics))
        })?;

        progress(&TurnSettlementRepairProgress {
            phase: TurnSettlementRepairPhase::Complete,
            scanned_turns: candidate_count,
            repairable_turns: candidate_count,
            skipped_turns: 0,
        });
        Ok(TurnSettlementRepairReport {
            apply: true,
            agent_id: plan.agent_id,
            turn_id: plan.turn_id,
            plan_path: Some(plan_path.to_path_buf()),
            scanned_turns: candidate_count,
            repairable_turns: candidate_count,
            repaired_turns,
            already_settled_turns,
            skipped_turns: 0,
            backup_path,
            diagnostics,
        })
    }
}

fn prepare_turn_settlement_repair_with_connection(
    connection: &Connection,
    source_path: &Path,
    plan_path: Option<&Path>,
    agent_id: Option<&str>,
    turn_id: Option<&str>,
    diagnostic_sample_limit: usize,
    mut progress: impl FnMut(&TurnSettlementRepairProgress),
) -> Result<TurnSettlementRepairReport> {
    let effective_agent_id = resolve_agent_filter(connection, agent_id, turn_id)?;
    let mut report = TurnSettlementRepairReport {
        apply: false,
        agent_id: effective_agent_id.clone(),
        turn_id: turn_id.map(str::to_owned),
        plan_path: plan_path.map(Path::to_path_buf),
        scanned_turns: 0,
        repairable_turns: 0,
        repaired_turns: 0,
        already_settled_turns: 0,
        skipped_turns: 0,
        backup_path: None,
        diagnostics: Vec::new(),
    };
    let mut candidates = Vec::new();
    let mut checkpoint = 0_i64;

    loop {
        let page = load_turn_page(
            connection,
            effective_agent_id.as_deref(),
            turn_id,
            checkpoint,
        )?;
        if page.is_empty() {
            break;
        }
        for (rowid, agent_id, turn_id) in page {
            checkpoint = rowid;
            report.scanned_turns += 1;
            match inspect_candidate(connection, &agent_id, &turn_id)? {
                CandidateInspection::Repairable { candidate, .. } => {
                    report.repairable_turns += 1;
                    push_diagnostic(
                        &mut report.diagnostics,
                        diagnostic_sample_limit,
                        TurnSettlementRepairDiagnostic {
                            agent_id,
                            turn_id,
                            status: "repairable".into(),
                            reason: "unique orphaned terminal settlement evidence".into(),
                        },
                    );
                    candidates.push(candidate);
                }
                CandidateInspection::Skipped(diagnostic) => {
                    report.skipped_turns += 1;
                    push_diagnostic(&mut report.diagnostics, diagnostic_sample_limit, diagnostic);
                }
            }
        }
        progress(&TurnSettlementRepairProgress {
            phase: TurnSettlementRepairPhase::Scanning,
            scanned_turns: report.scanned_turns,
            repairable_turns: report.repairable_turns,
            skipped_turns: report.skipped_turns,
        });
    }

    if let Some(plan_path) = plan_path {
        write_plan(
            plan_path,
            &TurnSettlementRepairPlan {
                format: PLAN_FORMAT.into(),
                source_path: canonical_path(source_path),
                schema_version: current_schema_version(connection)?,
                created_at: Utc::now().to_rfc3339(),
                agent_id: effective_agent_id,
                turn_id: turn_id.map(str::to_owned),
                candidates,
            },
        )?;
    }
    progress(&TurnSettlementRepairProgress {
        phase: TurnSettlementRepairPhase::Complete,
        scanned_turns: report.scanned_turns,
        repairable_turns: report.repairable_turns,
        skipped_turns: report.skipped_turns,
    });
    Ok(report)
}

fn open_existing_read_only(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        bail!(
            "turn settlement reconciliation requires an existing runtime database: {}",
            path.display()
        );
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening runtime db read-only {}", path.display()))?;
    connection.busy_timeout(RUNTIME_DB_BUSY_TIMEOUT)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA query_only = ON;",
    )?;
    Ok(connection)
}

fn resolve_agent_filter(
    connection: &Connection,
    agent_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<Option<String>> {
    if let Some(agent_id) = agent_id {
        return Ok(Some(agent_id.to_string()));
    }
    let Some(turn_id) = turn_id else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT DISTINCT agent_id
         FROM turn_records
         WHERE turn_id = ?1
         ORDER BY agent_id",
    )?;
    let agents = statement
        .query_map([turn_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match agents.as_slice() {
        [] => Ok(None),
        [agent_id] => Ok(Some(agent_id.clone())),
        _ => bail!("turn_id {turn_id} is ambiguous across multiple agents; pass --agent"),
    }
}

fn load_turn_page(
    connection: &Connection,
    agent_id: Option<&str>,
    turn_id: Option<&str>,
    checkpoint: i64,
) -> Result<Vec<(i64, String, String)>> {
    let sql = match (agent_id, turn_id) {
        (Some(_), Some(_)) => {
            "SELECT rowid, agent_id, turn_id
             FROM turn_records
             WHERE rowid > ?1 AND agent_id = ?2 AND turn_id = ?3
               AND json_extract(payload_json, '$.terminal') IS NULL
             ORDER BY rowid
             LIMIT ?4"
        }
        (Some(_), None) => {
            "SELECT rowid, agent_id, turn_id
             FROM turn_records
             WHERE rowid > ?1 AND agent_id = ?2
               AND json_extract(payload_json, '$.terminal') IS NULL
             ORDER BY rowid
             LIMIT ?3"
        }
        (None, Some(_)) => {
            "SELECT rowid, agent_id, turn_id
             FROM turn_records
             WHERE rowid > ?1 AND turn_id = ?2
               AND json_extract(payload_json, '$.terminal') IS NULL
             ORDER BY rowid
             LIMIT ?3"
        }
        (None, None) => {
            "SELECT rowid, agent_id, turn_id
             FROM turn_records
             WHERE rowid > ?1
               AND json_extract(payload_json, '$.terminal') IS NULL
             ORDER BY rowid
             LIMIT ?2"
        }
    };
    let mut statement = connection.prepare(sql)?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    };
    let rows = match (agent_id, turn_id) {
        (Some(agent_id), Some(turn_id)) => statement
            .query_map(params![checkpoint, agent_id, turn_id, PAGE_SIZE], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        (Some(agent_id), None) => statement
            .query_map(params![checkpoint, agent_id, PAGE_SIZE], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        (None, Some(turn_id)) => statement
            .query_map(params![checkpoint, turn_id, PAGE_SIZE], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        (None, None) => statement
            .query_map(params![checkpoint, PAGE_SIZE], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(rows)
}

fn inspect_candidate(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
) -> Result<CandidateInspection> {
    let snapshot = match load_snapshot(connection, agent_id, turn_id) {
        Ok(snapshot) => snapshot,
        Err(error) => return Ok(skipped_inspection(agent_id, turn_id, error.to_string())),
    };
    let Some(snapshot) = snapshot else {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            "turn record disappeared during audit",
        ));
    };
    if snapshot.turn.terminal.is_some() {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            "turn is already terminal",
        ));
    }
    if snapshot.attempt.state != ExecutionAttemptState::Open {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            "execution attempt is not open",
        ));
    }
    if snapshot.attempt.recovery_of_attempt_id.is_some() {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            "execution attempt belongs to a recovery chain",
        ));
    }
    if snapshot.queue.status != QueueEntryStatus::Dequeued {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            format!(
                "queue entry is {:?}, expected dequeued",
                snapshot.queue.status
            ),
        ));
    }
    if let Some(agent_state) = snapshot.agent_state.as_ref() {
        let current_turn_conflicts = agent_state
            .current_turn_id
            .as_deref()
            .is_some_and(|current_turn_id| current_turn_id != turn_id);
        let execution_binding_conflicts = agent_state
            .current_execution_binding
            .as_ref()
            .is_some_and(|binding| binding.turn_id != turn_id);
        if current_turn_conflicts || execution_binding_conflicts {
            return Ok(skipped_inspection(
                agent_id,
                turn_id,
                "agent state belongs to a different turn; manual review is required",
            ));
        }
    }
    if snapshot.terminal_tool_ids.len() != 1 {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            format!(
                "found {} successful terminal intent tools; exact repair requires one",
                snapshot.terminal_tool_ids.len()
            ),
        ));
    }
    if !snapshot.waits.is_empty() {
        return Ok(skipped_inspection(
            agent_id,
            turn_id,
            "turn already has durable wait evidence; settlement is ambiguous",
        ));
    }
    let attempt_id = snapshot.attempt.attempt_id.clone();
    let message_id = snapshot.queue.message_id.clone();
    let brief_ids = snapshot
        .briefs
        .iter()
        .map(|brief| brief.id.clone())
        .collect();
    let wait_ids = snapshot
        .waits
        .iter()
        .map(|wait| wait.wait_id.clone())
        .collect();
    let candidate = TurnSettlementRepairCandidate {
        agent_id: agent_id.to_string(),
        turn_id: turn_id.to_string(),
        message_id,
        attempt_id,
        terminal_tool_ids: snapshot.terminal_tool_ids.clone(),
        brief_ids,
        wait_ids,
        source_fingerprint: snapshot.source_fingerprint.clone(),
    };
    Ok(CandidateInspection::Repairable {
        candidate,
        snapshot,
    })
}

fn skipped_inspection(
    agent_id: &str,
    turn_id: &str,
    reason: impl Into<String>,
) -> CandidateInspection {
    CandidateInspection::Skipped(TurnSettlementRepairDiagnostic {
        agent_id: agent_id.to_string(),
        turn_id: turn_id.to_string(),
        status: "manual_review_required".into(),
        reason: reason.into(),
    })
}

fn load_snapshot(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<CandidateSnapshot>> {
    let turn_payload = connection
        .query_row(
            "SELECT payload_json
             FROM turn_records
             WHERE agent_id = ?1 AND turn_id = ?2",
            params![agent_id, turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(turn_payload) = turn_payload else {
        return Ok(None);
    };
    let turn: TurnRecord = serde_json::from_str(&turn_payload)?;

    let mut attempt_statement = connection.prepare(
        "SELECT payload_json
         FROM execution_protocol_attempts
         WHERE agent_id = ?1
           AND json_extract(payload_json, '$.turn_id') = ?2
         ORDER BY attempt_id",
    )?;
    let attempts = attempt_statement
        .query_map(params![agent_id, turn_id], |row| row.get::<_, String>(0))?
        .map(|payload| serde_json::from_str::<ExecutionAttempt>(&payload?).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    if attempts.len() != 1 {
        bail!(
            "turn {agent_id}/{turn_id} has {} execution attempts; exact repair requires one",
            attempts.len()
        );
    }
    let attempt = attempts.into_iter().next().expect("one attempt");
    let message_id = attempt
        .source_message_id
        .as_deref()
        .context("execution attempt has no source message")?;
    let message_payload = connection
        .query_row(
            "SELECT payload_json
             FROM messages
             WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .context("source message is missing")?;
    let message: MessageEnvelope = serde_json::from_str(&message_payload)?;
    if message.turn_id.as_deref() != Some(turn_id)
        && !turn.input_message_ids.iter().any(|id| id == message_id)
    {
        bail!("source message is not linked to the turn");
    }
    let queue_payload = connection
        .query_row(
            "SELECT payload_json
             FROM queue_entries
             WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .context("source queue entry is missing")?;
    let queue: QueueEntryRecord = serde_json::from_str(&queue_payload)?;

    let briefs = load_evidence::<BriefRecord>(connection, "briefs", agent_id, turn_id)?;
    let tools =
        load_evidence::<ToolExecutionRecord>(connection, "tool_executions", agent_id, turn_id)?;
    let waits = {
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, status
             FROM wait_conditions
             WHERE agent_id = ?1 AND last_turn_id = ?2
             ORDER BY wait_condition_id",
        )?;
        let waits = statement
            .query_map(params![agent_id, turn_id], |row| {
                Ok(WaitEvidence {
                    wait_id: row.get(0)?,
                    status: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        waits
    };
    let agent_state = connection
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<AgentState>(&payload))
        .transpose()?;
    let terminal_tool_ids = tools
        .iter()
        .filter(|tool| is_terminal_intent_tool(tool))
        .map(|tool| tool.id.clone())
        .collect::<Vec<_>>();
    let source_fingerprint = fingerprint(&serde_json::json!({
        "turn": turn,
        "queue": queue,
        "attempt": attempt,
        "agent_state": agent_state,
        "message": message,
        "briefs": briefs,
        "tools": tools,
        "waits": waits,
    }))?;
    Ok(Some(CandidateSnapshot {
        turn,
        queue,
        attempt,
        agent_state,
        briefs,
        tools,
        waits,
        terminal_tool_ids,
        source_fingerprint,
    }))
}

fn is_terminal_intent_tool(tool: &ToolExecutionRecord) -> bool {
    tool.status == ToolExecutionStatus::Success
        && tool
            .output
            .get("should_sleep")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && matches!(
            tool.tool_name.as_str(),
            "WaitFor" | "CompleteWorkItem" | "PickWorkItem"
        )
}

fn load_evidence<T: for<'de> Deserialize<'de>>(
    connection: &Connection,
    table: &str,
    agent_id: &str,
    turn_id: &str,
) -> Result<Vec<T>> {
    let sql = format!(
        "SELECT payload_json
         FROM {table}
         WHERE agent_id = ?1 AND turn_id = ?2
         ORDER BY created_at, evidence_id"
    );
    let mut statement = connection.prepare(&sql)?;
    let records = statement
        .query_map(params![agent_id, turn_id], |row| row.get::<_, String>(0))?
        .map(|payload| serde_json::from_str::<T>(&payload?).map_err(Into::into))
        .collect();
    records
}

fn apply_candidate(
    tx: &Transaction<'_>,
    candidate: &TurnSettlementRepairCandidate,
) -> Result<ApplyDisposition> {
    let current_turn = tx
        .query_row(
            "SELECT payload_json
             FROM turn_records
             WHERE agent_id = ?1 AND turn_id = ?2",
            params![candidate.agent_id, candidate.turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<TurnRecord>(&payload))
        .transpose()?
        .context("repair candidate turn no longer exists")?;
    if current_turn.terminal.as_ref().is_some_and(|terminal| {
        terminal.kind == TurnTerminalKind::Interrupted
            && terminal.reason.as_deref() == Some(REPAIR_REASON)
    }) {
        return Ok(ApplyDisposition::AlreadySettled);
    }

    let CandidateInspection::Repairable {
        candidate: current_candidate,
        snapshot,
    } = inspect_candidate(tx, &candidate.agent_id, &candidate.turn_id)?
    else {
        bail!(
            "turn settlement repair source changed for {}/{}",
            candidate.agent_id,
            candidate.turn_id
        );
    };
    if &current_candidate != candidate {
        bail!(
            "turn settlement repair fingerprint changed for {}/{}",
            candidate.agent_id,
            candidate.turn_id
        );
    }

    let now = Utc::now();
    let outcome_id = stable_id(
        "turn-settlement-outcome",
        &candidate.agent_id,
        &candidate.turn_id,
        &candidate.attempt_id,
    );
    let state = load_state_unchecked_tx(tx, &candidate.agent_id)?;
    let transition = execution_protocol::interrupt_execution(
        &state,
        &InterruptExecution {
            attempt_id: candidate.attempt_id.clone(),
            outcome_id: outcome_id.clone(),
            reason: REPAIR_REASON.into(),
            interrupted_at: now.to_rfc3339(),
        },
    )
    .map_err(|error| anyhow!("interrupting orphaned execution attempt: {error}"))?;
    persist_state_tx(tx, &transition.state)?;

    let mut queue = snapshot.queue.clone();
    queue.status = QueueEntryStatus::Interrupted;
    queue.updated_at = now;
    upsert_queue_entry_tx(tx, &queue)?;

    let terminal = TurnTerminalRecord {
        turn_id: candidate.turn_id.clone(),
        turn_index: snapshot.turn.turn_index,
        kind: TurnTerminalKind::Interrupted,
        reason: Some(REPAIR_REASON.into()),
        last_assistant_message: None,
        no_brief_reason: snapshot
            .briefs
            .is_empty()
            .then_some(TurnNoBriefReason::Interrupted),
        checkpoint: None,
        completed_at: now,
        duration_ms: 0,
    };
    let mut turn = snapshot.turn.clone();
    if !turn.input_message_ids.contains(&candidate.message_id) {
        turn.input_message_ids.push(candidate.message_id.clone());
    }
    turn.tool_execution_ids = snapshot.tools.iter().map(|tool| tool.id.clone()).collect();
    turn.produced_brief_ids = snapshot
        .briefs
        .iter()
        .map(|brief| brief.id.clone())
        .collect();
    turn.completed_work_item_ids = snapshot
        .briefs
        .iter()
        .filter(|brief| brief.kind == BriefKind::Result)
        .filter_map(|brief| brief.work_item_id.clone())
        .collect();
    turn.waiting_condition_ids = snapshot
        .waits
        .iter()
        .map(|wait| wait.wait_id.clone())
        .collect();
    turn.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    upsert_turn_record_tx(tx, &turn)?;

    if let Some(mut agent_state) = snapshot.agent_state {
        let current_turn_matches =
            agent_state.current_turn_id.as_deref() == Some(candidate.turn_id.as_str());
        let binding_turn_id = agent_state
            .current_execution_binding
            .as_ref()
            .map(|binding| binding.turn_id.as_str());
        let binding_matches = binding_turn_id == Some(candidate.turn_id.as_str());
        if current_turn_matches || binding_matches {
            agent_state.current_run_id = None;
            agent_state.current_turn_id = None;
            agent_state.current_turn_work_item_id = None;
            agent_state.current_execution_binding = None;
            agent_state.current_turn_operator_binding_id = None;
            agent_state.current_turn_operator_reply_route_id = None;
            if matches!(
                agent_state.status,
                AgentStatus::Booting | AgentStatus::AwakeRunning | AgentStatus::AwaitingTask
            ) {
                agent_state.status = AgentStatus::AwakeIdle;
            }
            agent_state.last_turn_terminal = Some(terminal.clone());
            agent_state.last_runtime_failure = Some(RuntimeFailureSummary {
                occurred_at: now,
                summary: "Historical terminal settlement was interrupted by offline reconciliation"
                    .into(),
                phase: RuntimeFailurePhase::RuntimeTurn,
                detail_hint: Some(REPAIR_REASON.into()),
                failure_artifact: None,
            });
            upsert_agent_state_tx(tx, &agent_state)?;
        }
    }

    let mut audit = AuditEvent::legacy(
        "turn_settlement_reconciliation_applied",
        serde_json::json!({
            "agent_id": candidate.agent_id,
            "turn_id": candidate.turn_id,
            "message_id": candidate.message_id,
            "attempt_id": candidate.attempt_id,
            "outcome_id": outcome_id,
            "terminal_tool_ids": candidate.terminal_tool_ids,
            "preserved_brief_ids": candidate.brief_ids,
            "preserved_wait_ids": candidate.wait_ids,
            "reason": REPAIR_REASON,
        }),
    );
    audit.id = stable_id(
        "turn-settlement-audit",
        &candidate.agent_id,
        &candidate.turn_id,
        &candidate.attempt_id,
    );
    audit.created_at = now;
    append_audit_event_tx(tx, Some(&candidate.agent_id), &audit)?;
    Ok(ApplyDisposition::Applied)
}

fn fingerprint(value: &serde_json::Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn stable_id(prefix: &str, agent_id: &str, turn_id: &str, attempt_id: &str) -> String {
    let digest = Sha256::digest(format!("{agent_id}\0{turn_id}\0{attempt_id}"));
    format!("{prefix}-{:x}", digest)
}

fn canonical_path(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn write_plan(path: &Path, plan: &TurnSettlementRepairPlan) -> Result<()> {
    if path.exists() {
        bail!("repair plan already exists: {}", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("json")
    ));
    fs::write(&temporary, serde_json::to_vec_pretty(plan)?)
        .with_context(|| format!("writing repair plan {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("publishing repair plan {}", path.display()))?;
    Ok(())
}

fn read_plan(path: &Path) -> Result<TurnSettlementRepairPlan> {
    serde_json::from_slice(
        &fs::read(path).with_context(|| format!("reading repair plan {}", path.display()))?,
    )
    .with_context(|| format!("decoding repair plan {}", path.display()))
}

fn validate_plan(
    connection: &Connection,
    source_path: &Path,
    plan: &TurnSettlementRepairPlan,
) -> Result<()> {
    if plan.format != PLAN_FORMAT {
        bail!("unsupported turn settlement repair plan format");
    }
    if plan.source_path != canonical_path(source_path) {
        bail!("turn settlement repair plan belongs to a different runtime database");
    }
    if plan.schema_version != current_schema_version(connection)? {
        bail!("turn settlement repair plan schema version is stale");
    }
    Ok(())
}

fn push_diagnostic(
    diagnostics: &mut Vec<TurnSettlementRepairDiagnostic>,
    limit: usize,
    diagnostic: TurnSettlementRepairDiagnostic,
) {
    if diagnostics.len() < limit {
        diagnostics.push(diagnostic);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        domain::execution_protocol::{
            AdmittedFences, ExecutionBinding, ExecutionOrigin, ExecutionPriority,
            ExecutionProtocolState, ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity,
            ExecutionTrust,
        },
        runtime_db::transitions::{
            execution_protocol_repository::load_state_unchecked_tx, persist_state_tx,
        },
        types::{
            AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
            MessageOrigin, Priority, ToolExecutionStatus, TurnOwner, WaitConditionKind,
            WaitConditionRecord, WaitConditionStatus, WorkItemExecutionBinding,
        },
    };

    fn fixture() -> Result<(tempfile::TempDir, RuntimeDb, PathBuf)> {
        let dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let plan_path = dir.path().join("turn-settlement-plan.json");
        Ok((dir, db, plan_path))
    }

    fn seed_orphaned_turn(db: &RuntimeDb) -> Result<(String, String, String)> {
        let agent_id = "agent-a".to_string();
        let turn_id = "turn-a".to_string();
        let attempt_id = "attempt-a".to_string();
        let mut message = MessageEnvelope::new(
            &agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "wait for review".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::HttpControlPrompt,
            AdmissionContext::ControlAuthenticated,
        );
        message.id = "message-a".into();
        message.turn_id = Some(turn_id.clone());
        db.messages().upsert(&message)?;
        db.queue_entries().upsert(&QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: agent_id.clone(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Dequeued,
            created_at: message.created_at,
            updated_at: message.created_at,
        })?;

        let mut turn = TurnRecord::new(&agent_id, &turn_id, 7);
        turn.owner = Some(TurnOwner::Conversation {
            interaction_id: "interaction-a".into(),
        });
        turn.input_message_ids = vec![message.id.clone()];
        db.turn_records().upsert(&turn)?;

        let now = Utc::now();
        db.evidence().append_tool_execution(&ToolExecutionRecord {
            id: "tool-wait-a".into(),
            agent_id: agent_id.clone(),
            work_item_id: None,
            turn_index: 7,
            turn_id: Some(turn_id.clone()),
            tool_name: "WaitFor".into(),
            created_at: now,
            completed_at: Some(now),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "wake": "external" }),
            output: serde_json::json!({
                "envelope": {
                    "tool_name": "WaitFor",
                    "status": "success",
                    "result": { "wait_condition": { "id": "wait-a" } }
                },
                "is_error": false,
                "should_sleep": true,
                "sleep_duration_ms": null,
                "error": null
            }),
            summary: "prepared external wait".into(),
            invocation_surface: None,
        })?;
        let mut brief = BriefRecord::new(
            &agent_id,
            BriefKind::Result,
            "completed work item report",
            Some(message.id.clone()),
            None,
        );
        brief.id = "brief-result-a".into();
        brief.work_item_id = Some("work-completed-a".into());
        brief.turn_index = Some(7);
        brief.turn_id = Some(turn_id.clone());
        db.evidence().append_brief(&brief)?;

        let attempt = ExecutionAttempt {
            attempt_id: attempt_id.clone(),
            agent_id: agent_id.clone(),
            source_message_id: Some(message.id.clone()),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: message.id.clone(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::Conversation {
                interaction_id: "interaction-a".into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::Operator,
                trust: ExecutionTrust::OperatorInstruction,
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
            run_id: Some("run-a".into()),
            turn_id: Some(turn_id.clone()),
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: "2026-09-18T08:00:00Z".into(),
            terminal_at: None,
        };
        let mut protocol = ExecutionProtocolState {
            agent_id: agent_id.clone(),
            attempts: BTreeMap::new(),
            work_items: BTreeMap::new(),
            outcomes: BTreeMap::new(),
        };
        protocol.attempts.insert(attempt_id.clone(), attempt);
        db.transaction(|tx| persist_state_tx(tx, &protocol))?;

        let mut agent_state = AgentState::new(&agent_id);
        agent_state.status = AgentStatus::AwakeRunning;
        agent_state.current_run_id = Some("run-a".into());
        agent_state.current_turn_id = Some(turn_id.clone());
        agent_state.current_execution_binding = Some(WorkItemExecutionBinding {
            activation_id: Some("activation-a".into()),
            admission_provenance: None,
            source_message_id: message.id.clone(),
            turn_id: turn_id.clone(),
            owner: Some(TurnOwner::Conversation {
                interaction_id: "interaction-a".into(),
            }),
            work_item_id: None,
            claimed_work_revision: None,
        });
        db.agent_states().upsert(&agent_state)?;
        Ok((agent_id, turn_id, attempt_id))
    }

    #[test]
    fn turn_settlement_repair_is_dry_run_fenced_and_idempotent() -> Result<()> {
        let (_dir, db, plan_path) = fixture()?;
        let (agent_id, turn_id, attempt_id) = seed_orphaned_turn(&db)?;

        let report = db.prepare_turn_settlement_repair(
            Some(&plan_path),
            Some(&agent_id),
            Some(&turn_id),
            20,
            |_| {},
        )?;
        assert!(!report.apply);
        assert_eq!(report.scanned_turns, 1);
        assert_eq!(report.repairable_turns, 1);
        assert_eq!(
            db.queue_entries()
                .latest("message-a")?
                .expect("queue entry")
                .status,
            QueueEntryStatus::Dequeued
        );
        assert!(db
            .turn_records()
            .by_id(Some(&agent_id), &turn_id)?
            .expect("turn")
            .terminal
            .is_none());

        db.preflight_turn_settlement_repair_plan(&plan_path)?;
        let backup_path = db.create_turn_settlement_repair_backup()?;
        assert!(backup_path.is_file());
        let applied = db.apply_turn_settlement_repair_plan(
            &plan_path,
            Some(backup_path.clone()),
            20,
            |_| {},
        )?;
        assert_eq!(applied.repaired_turns, 1);
        assert_eq!(applied.already_settled_turns, 0);
        assert_eq!(applied.backup_path.as_deref(), Some(backup_path.as_path()));
        let turn = db
            .turn_records()
            .by_id(Some(&agent_id), &turn_id)?
            .expect("turn");
        let terminal = turn.terminal.as_ref().expect("terminal");
        assert_eq!(terminal.kind, TurnTerminalKind::Interrupted);
        assert_eq!(terminal.no_brief_reason, None);
        assert_eq!(turn.produced_brief_ids, vec!["brief-result-a"]);
        assert_eq!(turn.completed_work_item_ids, vec!["work-completed-a"]);
        assert_eq!(
            db.queue_entries()
                .latest("message-a")?
                .expect("queue entry")
                .status,
            QueueEntryStatus::Interrupted
        );
        let protocol = db.transaction(|tx| load_state_unchecked_tx(tx, &agent_id))?;
        assert_eq!(
            protocol.attempts[&attempt_id].state,
            ExecutionAttemptState::Interrupted
        );
        assert!(protocol.attempts[&attempt_id].terminal_outcome_id.is_some());
        let agent_state = db.agent_states().latest(&agent_id)?.expect("agent state");
        assert_eq!(agent_state.status, AgentStatus::AwakeIdle);
        assert!(agent_state.current_turn_id.is_none());
        assert!(agent_state.current_execution_binding.is_none());
        assert!(db
            .evidence()
            .tool_execution_by_id(&agent_id, "tool-wait-a")?
            .is_some());
        assert!(db
            .evidence()
            .brief_by_id(&agent_id, "brief-result-a")?
            .is_some());

        let repeated = db.apply_turn_settlement_repair_plan(&plan_path, None, 20, |_| {})?;
        assert_eq!(repeated.repaired_turns, 0);
        assert_eq!(repeated.already_settled_turns, 1);
        Ok(())
    }

    #[test]
    fn turn_settlement_repair_rejects_changed_source_fingerprint() -> Result<()> {
        let (_dir, db, plan_path) = fixture()?;
        let (agent_id, turn_id, _) = seed_orphaned_turn(&db)?;
        db.prepare_turn_settlement_repair(
            Some(&plan_path),
            Some(&agent_id),
            Some(&turn_id),
            20,
            |_| {},
        )?;

        let mut queue = db
            .queue_entries()
            .latest("message-a")?
            .expect("queue entry");
        queue.updated_at = Utc::now() + chrono::Duration::seconds(1);
        db.queue_entries().upsert(&queue)?;

        let error = db
            .apply_turn_settlement_repair_plan(&plan_path, None, 20, |_| {})
            .expect_err("changed source must reject apply");
        assert!(error.to_string().contains("fingerprint changed"));
        assert!(db
            .turn_records()
            .by_id(Some(&agent_id), &turn_id)?
            .expect("turn")
            .terminal
            .is_none());
        Ok(())
    }

    #[test]
    fn turn_settlement_repair_rejects_ambiguous_terminal_evidence() -> Result<()> {
        let (_dir, db, _plan_path) = fixture()?;
        let (agent_id, turn_id, _) = seed_orphaned_turn(&db)?;
        let now = Utc::now();
        db.evidence().append_tool_execution(&ToolExecutionRecord {
            id: "tool-complete-a".into(),
            agent_id: agent_id.clone(),
            work_item_id: Some("work-a".into()),
            turn_index: 7,
            turn_id: Some(turn_id.clone()),
            tool_name: "CompleteWorkItem".into(),
            created_at: now,
            completed_at: Some(now),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "work_item_id": "work-a" }),
            output: serde_json::json!({
                "envelope": {
                    "tool_name": "CompleteWorkItem",
                    "status": "success",
                    "result": {
                        "completion_mode": "bound_execution",
                        "completion_phase": "prepared"
                    }
                },
                "is_error": false,
                "should_sleep": true,
                "sleep_duration_ms": null,
                "error": null
            }),
            summary: "completed work item".into(),
            invocation_surface: None,
        })?;

        let report =
            db.prepare_turn_settlement_repair(None, Some(&agent_id), Some(&turn_id), 20, |_| {})?;
        assert_eq!(report.repairable_turns, 0);
        assert_eq!(report.skipped_turns, 1);
        assert!(report.diagnostics[0]
            .reason
            .contains("exact repair requires one"));
        Ok(())
    }

    #[test]
    fn turn_settlement_repair_ignores_nonterminal_successful_tools() -> Result<()> {
        let (_dir, db, _plan_path) = fixture()?;
        let (agent_id, turn_id, _) = seed_orphaned_turn(&db)?;
        let now = Utc::now();
        db.evidence().append_tool_execution(&ToolExecutionRecord {
            id: "tool-detached-complete-a".into(),
            agent_id: agent_id.clone(),
            work_item_id: Some("work-a".into()),
            turn_index: 7,
            turn_id: Some(turn_id.clone()),
            tool_name: "CompleteWorkItem".into(),
            created_at: now,
            completed_at: Some(now),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "work_item_id": "work-a" }),
            output: serde_json::json!({
                "envelope": {
                    "tool_name": "CompleteWorkItem",
                    "status": "success",
                    "result": {
                        "completion_mode": "detached",
                        "completion_phase": "prepared"
                    }
                },
                "is_error": false,
                "should_sleep": false,
                "sleep_duration_ms": null,
                "error": null
            }),
            summary: "completed detached work item".into(),
            invocation_surface: None,
        })?;

        let report =
            db.prepare_turn_settlement_repair(None, Some(&agent_id), Some(&turn_id), 20, |_| {})?;
        assert_eq!(report.repairable_turns, 1);
        assert_eq!(report.skipped_turns, 0);
        Ok(())
    }

    #[test]
    fn turn_settlement_repair_rejects_existing_durable_wait() -> Result<()> {
        let (_dir, db, _plan_path) = fixture()?;
        let (agent_id, turn_id, _) = seed_orphaned_turn(&db)?;
        let now = Utc::now();
        db.wait_conditions().upsert(&WaitConditionRecord {
            id: "wait-a".into(),
            agent_id: agent_id.clone(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("WaitFor".into()),
            subject_ref: Some("github:holon-run/holon#3055".into()),
            waiting_for: "reviewer merge".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some(turn_id.clone()),
            trigger_message_id: None,
            triggered_at: None,
        })?;

        let report =
            db.prepare_turn_settlement_repair(None, Some(&agent_id), Some(&turn_id), 20, |_| {})?;
        assert_eq!(report.repairable_turns, 0);
        assert_eq!(report.skipped_turns, 1);
        assert!(report.diagnostics[0]
            .reason
            .contains("durable wait evidence"));
        Ok(())
    }

    #[test]
    fn turn_settlement_repair_openers_do_not_create_or_migrate_database() -> Result<()> {
        let dir = tempdir()?;
        let db_path = dir.path().join("legacy.db");
        let lock_path = dir.path().join("legacy.lock");
        Connection::open(&db_path)?.execute_batch("CREATE TABLE sentinel (id INTEGER);")?;

        RuntimeDb::prepare_turn_settlement_repair_read_only(&db_path, None, None, None, 20, |_| {})
            .expect_err("audit must reject an incompatible database");
        let connection = Connection::open(&db_path)?;
        let migration_table_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(migration_table_count, 0);
        drop(connection);

        RuntimeDb::open_for_turn_settlement_repair(&db_path, &lock_path)
            .expect_err("apply opener must reject an incompatible database");
        let connection = Connection::open(&db_path)?;
        let migration_table_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(migration_table_count, 0);
        Ok(())
    }

    #[test]
    fn turn_settlement_repair_rejects_newer_agent_execution_binding() -> Result<()> {
        let (_dir, db, plan_path) = fixture()?;
        let (agent_id, turn_id, attempt_id) = seed_orphaned_turn(&db)?;
        db.prepare_turn_settlement_repair(
            Some(&plan_path),
            Some(&agent_id),
            Some(&turn_id),
            20,
            |_| {},
        )?;

        let mut agent_state = db.agent_states().latest(&agent_id)?.expect("agent state");
        agent_state.current_execution_binding = Some(WorkItemExecutionBinding {
            activation_id: Some("activation-new".into()),
            admission_provenance: None,
            source_message_id: "message-new".into(),
            turn_id: "turn-new".into(),
            owner: Some(TurnOwner::Conversation {
                interaction_id: "interaction-new".into(),
            }),
            work_item_id: None,
            claimed_work_revision: None,
        });
        db.agent_states().upsert(&agent_state)?;

        let error = db
            .apply_turn_settlement_repair_plan(&plan_path, None, 20, |_| {})
            .expect_err("agent state fingerprint change must reject stale plan");
        assert!(error.to_string().contains("source changed"));

        let replacement_plan = plan_path.with_file_name("replacement-plan.json");
        let report = db.prepare_turn_settlement_repair(
            Some(&replacement_plan),
            Some(&agent_id),
            Some(&turn_id),
            20,
            |_| {},
        )?;
        assert_eq!(report.repairable_turns, 0);
        assert_eq!(report.skipped_turns, 1);
        assert!(report.diagnostics[0]
            .reason
            .contains("different turn; manual review is required"));

        let agent_state = db.agent_states().latest(&agent_id)?.expect("agent state");
        assert_eq!(
            agent_state
                .current_execution_binding
                .as_ref()
                .map(|binding| binding.turn_id.as_str()),
            Some("turn-new")
        );
        assert_eq!(
            agent_state.current_turn_id.as_deref(),
            Some(turn_id.as_str())
        );
        assert!(db
            .turn_records()
            .by_id(Some(&agent_id), &turn_id)?
            .expect("turn")
            .terminal
            .is_none());
        assert_eq!(
            db.queue_entries()
                .latest("message-a")?
                .expect("queue entry")
                .status,
            QueueEntryStatus::Dequeued
        );
        let protocol = db.transaction(|tx| load_state_unchecked_tx(tx, &agent_id))?;
        assert_eq!(
            protocol.attempts[&attempt_id].state,
            ExecutionAttemptState::Open
        );
        Ok(())
    }
}
