use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;

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

#[derive(Debug)]
struct RepairInspection {
    report: WaitFinalBriefPublicationRepairReport,
    candidates: Vec<RepairCandidate>,
}

impl RuntimeDb {
    pub fn create_wait_final_brief_publication_repair_backup(&self) -> Result<PathBuf> {
        self.create_verified_backup("wait-final-brief-publication")
    }

    pub fn repair_wait_final_brief_publications(
        &self,
        apply: bool,
        agent_id: Option<&str>,
        diagnostic_sample_limit: usize,
        backup_path: Option<PathBuf>,
    ) -> Result<WaitFinalBriefPublicationRepairReport> {
        if !apply {
            return inspect_repairs(
                &self.connection()?,
                agent_id,
                diagnostic_sample_limit,
                false,
                backup_path,
            )
            .map(|inspection| inspection.report);
        }

        self.transaction(|tx| {
            let mut inspection = inspect_repairs(
                tx,
                agent_id,
                diagnostic_sample_limit,
                true,
                backup_path.clone(),
            )?;
            for candidate in &inspection.candidates {
                repair_candidate(tx, candidate)?;
                inspection.report.repaired_briefs += 1;
            }
            Ok(inspection.report)
        })
    }
}

fn inspect_repairs(
    connection: &Connection,
    agent_id: Option<&str>,
    diagnostic_sample_limit: usize,
    apply: bool,
    backup_path: Option<PathBuf>,
) -> Result<RepairInspection> {
    let rows = load_candidate_rows(connection, agent_id)?;
    let scanned_briefs = rows.len();
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();

    for (_, (brief_row, turn_payloads)) in rows {
        match inspect_candidate(connection, brief_row, turn_payloads) {
            Ok(candidate) => {
                if diagnostics.len() < diagnostic_sample_limit {
                    diagnostics.push(WaitFinalBriefPublicationRepairDiagnostic {
                        brief_id: candidate.brief.id.clone(),
                        agent_id: candidate.brief.agent_id.clone(),
                        turn_id: Some(candidate.turn.turn_id.clone()),
                        status: "repairable",
                        reason: "unique completed waiting Turn and referenced WaitFor condition prove canonical Brief ownership".to_string(),
                    });
                }
                candidates.push(candidate);
            }
            Err(diagnostic) => {
                if diagnostics.len() < diagnostic_sample_limit {
                    diagnostics.push(diagnostic);
                }
            }
        }
    }

    let repairable_briefs = candidates.len();
    Ok(RepairInspection {
        report: WaitFinalBriefPublicationRepairReport {
            apply,
            agent_id: agent_id.map(ToOwned::to_owned),
            scanned_briefs,
            repairable_briefs,
            repaired_briefs: 0,
            skipped_briefs: scanned_briefs.saturating_sub(repairable_briefs),
            backup_path,
            diagnostics,
        },
        candidates,
    })
}

fn load_candidate_rows(
    connection: &Connection,
    agent_id: Option<&str>,
) -> Result<BTreeMap<String, (BriefRow, Vec<String>)>> {
    let mut statement = connection.prepare(
        "SELECT b.evidence_id, b.agent_id, b.turn_id, b.work_item_id, b.payload_json,
                refs.turn_payload_json
         FROM briefs AS b
         LEFT JOIN (
           SELECT t.agent_id,
                  CAST(produced.value AS TEXT) AS brief_id,
                  t.payload_json AS turn_payload_json
           FROM turn_records AS t,
                json_each(
                  CASE WHEN json_valid(t.payload_json) THEN t.payload_json ELSE '{}' END,
                  '$.produced_brief_ids'
                ) AS produced
         ) AS refs
           ON refs.agent_id = b.agent_id
          AND refs.brief_id = b.evidence_id
         WHERE b.kind = 'result'
           AND b.created_event_seq IS NULL
           AND json_extract(
                 CASE WHEN json_valid(b.payload_json) THEN b.payload_json ELSE '{}' END,
                 '$.finalizes_assistant_round_id'
               ) IS NOT NULL
           AND (?1 IS NULL OR b.agent_id = ?1)
         ORDER BY b.agent_id, b.created_at, b.evidence_id",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((
            BriefRow {
                brief_id: row.get(0)?,
                agent_id: row.get(1)?,
                turn_id: row.get(2)?,
                work_item_id: row.get(3)?,
                payload_json: row.get(4)?,
            },
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut grouped = BTreeMap::new();
    for row in rows {
        let (brief, turn_payload) = row?;
        grouped
            .entry(brief.brief_id.clone())
            .and_modify(|(_, turns): &mut (BriefRow, Vec<String>)| {
                if let Some(turn_payload) = turn_payload.as_ref() {
                    turns.push(turn_payload.clone());
                }
            })
            .or_insert_with(|| (brief, turn_payload.into_iter().collect()));
    }
    Ok(grouped)
}

fn inspect_candidate(
    connection: &Connection,
    row: BriefRow,
    turn_payloads: Vec<String>,
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
    if turn_payloads.len() != 1 {
        return Err(diagnostic(
            row.turn_id.clone(),
            format!(
                "Brief is referenced by {} Turn records; expected exactly one",
                turn_payloads.len()
            ),
        ));
    }
    let turn: TurnRecord = serde_json::from_str(&turn_payloads[0]).map_err(|error| {
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
    if let Err(error) = ensure_created_event_compatible(connection, &brief) {
        return Err(diagnostic(Some(turn.turn_id), error.to_string()));
    }
    Ok(RepairCandidate { brief, turn })
}

fn ensure_created_event_compatible(connection: &Connection, brief: &BriefRecord) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT data_json
         FROM audit_events
         WHERE agent_id = ?1
           AND kind = 'brief_created'
           AND json_extract(data_json, '$.data.brief_id') = ?2
         ORDER BY event_seq, audit_event_id",
    )?;
    let payloads = statement
        .query_map(params![&brief.agent_id, &brief.id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        payloads.len() <= 1,
        "multiple brief_created events already reference this Brief"
    );
    let Some(payload) = payloads.first() else {
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
    use tempfile::tempdir;

    use super::*;
    use crate::{
        runtime_db::RuntimeDb,
        types::{BriefKind, TurnTerminalSummary, WaitConditionKind, WaitConditionStatus},
    };

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

        let dry_run = db.repair_wait_final_brief_publications(false, None, 10, None)?;
        assert_eq!(dry_run.repairable_briefs, 1);
        assert_eq!(dry_run.repaired_briefs, 0);

        let applied = db.repair_wait_final_brief_publications(true, None, 10, None)?;
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

        let repeated = db.repair_wait_final_brief_publications(true, None, 10, None)?;
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

        let report = db.repair_wait_final_brief_publications(true, None, 10, None)?;
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
}
