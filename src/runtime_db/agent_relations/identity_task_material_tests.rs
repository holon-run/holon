use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentVisibility, TaskStatus},
};
use std::sync::Arc;

const AGENT: &str = "relation-identity-汉字";
const TASK: &str = "relation-task-汉字";
const WORK: &str = "work-汉字\0\"\\\n{}[]:,";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "relation-identity-task".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn text_cost(text: &str) -> (u64, usize) {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

// Independent punctuation count, excluding escaped punctuation in strings.
fn json_cost(payload: &str) -> (u64, usize) {
    let mut quoted = false;
    let mut escaped = false;
    let mut slots = 0;
    for byte in payload.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            slots += usize::from(matches!(
                byte,
                b'"' | b'{' | b'}' | b'[' | b']' | b':' | b','
            ));
            quoted = byte == b'"';
        }
    }
    (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

fn task() -> TaskRecord {
    let now = chrono::DateTime::from_timestamp_millis(1_760_000_000_123).unwrap();
    TaskRecord {
        id: TASK.into(),
        agent_id: AGENT.into(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: Some(WORK.into()),
        summary: Some("escaped-汉字\0\"\\\n{}[]:,".into()),
        detail: Some(serde_json::json!({
            "child_agent_id": AGENT,
            "created_new_subagent": true,
            "work_item_id": "fallback-汉字",
        })),
        recovery: None,
    }
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    fixture_with_task(&task())
}

fn fixture_with_task(
    record: &TaskRecord,
) -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        Some(TASK.into()),
    ))?;
    db.tasks().upsert(record)?;
    Ok((home, db))
}

#[derive(Debug, PartialEq, Eq)]
enum Evidence {
    Identity(Option<AgentIdentityRecord>),
    Task(Option<LegacySupervisionTaskEvidence>),
}

fn read(connection: &rusqlite::Connection, identity: bool, id: &str) -> Result<Evidence> {
    if identity {
        projection_identity(connection, id).map(Evidence::Identity)
    } else {
        legacy_task_evidence(connection, id).map(Evidence::Task)
    }
}

fn steps(connection: &rusqlite::Connection, identity: bool) -> Result<Vec<(u64, usize)>> {
    if identity {
        let payload: String = connection.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [AGENT],
            |row| row.get(0),
        )?;
        return Ok(vec![text_cost(&payload), json_cost(&payload)]);
    }
    let (owner, child, kind, payload): (String, Option<String>, String, String) = connection
        .query_row(
        "SELECT owner_agent_id, child_agent_id, kind, payload_json FROM tasks WHERE task_id = ?1",
        [TASK],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let mut costs = vec![text_cost(&owner)];
    if let Some(child) = child {
        costs.push(text_cost(&child));
    }
    costs.extend([
        text_cost(&kind),
        text_cost(&payload),
        json_cost(&payload),
        text_cost(TASK),
    ]);
    // The fixture explicitly stores this field; do not call the reader/helper.
    let decoded: serde_json::Value = serde_json::from_str(&payload)?;
    if let Some(work) = decoded["work_item_id"]
        .as_str()
        .or_else(|| decoded["detail"]["work_item_id"].as_str())
    {
        costs.push(text_cost(work));
    }
    Ok(costs)
}

#[test]
fn identity_task_exact_cost_and_every_allocation_denial() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    let prior = (13 * 8, 3 * 8);
    for identity in [true, false] {
        let id = if identity { AGENT } else { TASK };
        let baseline = read(&connection, identity, id)?;
        let costs = steps(&connection, identity)?;
        let exact = costs.iter().copied().fold(add(header(), prior), add);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(13, 3)?;
            assert_eq!(read(&connection, identity, id)?, baseline);
        }
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let mut before = add(header(), prior);
        for (index, cost) in costs.into_iter().enumerate() {
            let next = add(before, cost);
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    next.0 - u64::from(bytes),
                    next.1 - usize::from(!bytes),
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    charge_owned_metadata(13, 3)?;
                    let error = read(&connection, identity, id).unwrap_err();
                    assert!(
                        format!("{error:#}").contains("ResourceExhausted"),
                        "{identity}/{index}: {error:#}"
                    );
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
                assert!(phase.finish().is_err());
                assert_eq!(
                    ledger.report()?.observed_bytes,
                    before.0,
                    "{identity}/{index}"
                );
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, before.0);
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            before = next;
        }
    }
    Ok(())
}

#[test]
fn identity_task_missing_null_and_kind_work_fallback_are_unchanged() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    {
        let _entry = phase.enter();
        assert_eq!(projection_identity(&connection, "absent")?, None);
        assert_eq!(
            canonical_relations_from_connection(&connection, "absent")?,
            None
        );
        assert_eq!(legacy_task_evidence(&connection, "absent")?, None);
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for (kind, child, created, work, expected_child) in [
        (TaskKind::ActorInvocation, true, true, true, true),
        (TaskKind::ActorInvocation, true, false, false, false),
        (TaskKind::ActorInvocation, false, true, false, false),
        (TaskKind::ChildAgentTask, false, false, true, true),
        (TaskKind::CommandTask, false, false, false, false),
    ] {
        let mut record = task();
        record.kind = kind;
        record.work_item_id = None;
        record.detail = Some(serde_json::json!({"created_new_subagent": created}));
        if child {
            record.detail.as_mut().unwrap()["child_agent_id"] = serde_json::json!(AGENT);
        }
        if work {
            record.detail.as_mut().unwrap()["work_item_id"] = serde_json::json!(WORK);
        }
        let (_case_home, db) = fixture_with_task(&record)?;
        let connection = db.connection()?;
        let baseline = legacy_task_evidence(&connection, TASK)?.unwrap();
        assert_eq!(baseline.is_child_agent_task, expected_child);
        assert_eq!(baseline.child_agent_id.as_deref(), child.then_some(AGENT));
        assert_eq!(
            baseline.delegated_from_work_item_id.as_deref(),
            work.then_some(WORK)
        );
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        {
            let _entry = phase.enter();
            assert_eq!(legacy_task_evidence(&connection, TASK)?, Some(baseline));
        }
        let exact = steps(&connection, false)?.into_iter().fold(header(), add);
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn identity_task_corrupt_sql_json_and_kind_rollback_retry_on_same_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    connection.execute_batch("PRAGMA ignore_check_constraints = ON")?;
    for (identity, column, corrupt, label) in [
        (true, "payload_json", "x'ff'", "SQL"),
        (
            true,
            "payload_json",
            "'{'",
            "decoding agent identity for canonical projection",
        ),
        (false, "owner_agent_id", "x'ff'", "SQL"),
        (false, "child_agent_id", "x'ff'", "SQL"),
        (false, "kind", "x'ff'", "SQL"),
        (false, "payload_json", "x'ff'", "SQL"),
        (
            false,
            "payload_json",
            "'{'",
            "decoding legacy supervision task",
        ),
        (
            false,
            "kind",
            "'invalid-kind'",
            "decoding legacy supervision task kind",
        ),
    ] {
        let id = if identity { AGENT } else { TASK };
        let table = if identity {
            "agent_identities"
        } else {
            "tasks"
        };
        let baseline = read(&connection, identity, id)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let tx = connection.transaction()?;
            tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            tx.execute(&format!("UPDATE {table} SET {column} = {corrupt}"), [])?;
            let error = read(&tx, identity, id).unwrap_err();
            if label == "SQL" {
                assert!(
                    error.downcast_ref::<rusqlite::Error>().is_some(),
                    "{error:#}"
                );
            } else {
                assert!(format!("{error:#}").contains(label), "{error:#}");
            }
            let failed = phase.finish()?;
            tx.rollback()?;
            assert_eq!(
                connection
                    .query_row("PRAGMA defer_foreign_keys", [], |row| row.get::<_, i64>(0))?,
                0
            );
            assert_eq!(read(&connection, identity, id)?, baseline);
            let cost = steps(&connection, identity)?.into_iter().fold((0, 0), add);
            assert_eq!(phase.finish()?, add(failed, cost));
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        let used = phase.finish()?.0;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, used);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    connection.execute_batch("PRAGMA ignore_check_constraints = OFF")?;
    assert_eq!(
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        connection.query_row("PRAGMA ignore_check_constraints", [], |row| row
            .get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn canonical_projection_uses_original_phase_and_preserves_json_task_evidence() -> Result<()> {
    let (_home, db) = fixture()?;
    let baseline = db.agent_canonical_relations().latest(AGENT)?.unwrap();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        assert_eq!(
            db.agent_canonical_relations().latest(AGENT)?,
            Some(baseline)
        );
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(phase.finish()?.0 > header().0);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let used = phase.finish()?.0;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, used);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
