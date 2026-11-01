use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::transitions::agent_state_tx,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn cost(bytes: usize, slots: usize) -> (u64, usize) {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn wire_cost(value: &impl serde::Serialize) -> Result<(u64, usize)> {
    // Independent AST formula, not the production byte/quote scanner or ledger.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len().saturating_sub(1)
                    + fields.values().map(|value| 2 + slots(value)).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let payload = serde_json::to_string(value)?;
    Ok(cost(payload.len(), slots(&serde_json::from_str(&payload)?)))
}
fn state_read_cost(value: &AgentState) -> Result<(u64, usize)> {
    fn nodes(value: &serde_json::Value) -> usize {
        1 + match value {
            serde_json::Value::Object(fields) => fields.values().map(nodes).sum::<usize>(),
            serde_json::Value::Array(values) => values.iter().map(nodes).sum::<usize>(),
            _ => 0,
        }
    }
    let payload = serde_json::to_string(value)?;
    let nodes = nodes(&serde_json::from_str(&payload)?);
    Ok((
        (payload.len() as u64 + 256 + (nodes * std::mem::size_of::<serde_json::Value>()) as u64)
            * 8,
        (1 + nodes) * 8,
    ))
}
fn now() -> DateTime<Utc> {
    "2026-10-17T11:22:23.123456789Z".parse().unwrap()
}
fn record() -> AgentState {
    let mut state = AgentState::new("scheduling-agent");
    state.last_wake_reason = Some("{,:} \"x\" \\ 路".into());
    state.attached_workspaces = vec!["workspace-one".into(), "workspace-two".into()];
    state
}
fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| upsert_agent_state_at_tx(tx, &record(), now()))?;
    Ok((home, db))
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

#[test]
fn work_source_column_errors_precede_vec_admission_without_extra_charge() -> Result<()> {
    let (_home, db) = fixture()?;
    let budget = ledger()?;
    let material =
        ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    let read = |sql: &str| {
        db.transaction(|tx| {
            let mut statement = tx.prepare(sql)?;
            work_queue_records(&mut statement, [])
        })
    };
    assert!(material
        .read(|| read("SELECT X'7b' AS payload_json WHERE 0"))?
        .is_empty());
    for sql in [
        "SELECT X'7b' AS payload_json",
        "SELECT 1 AS payload_json",
        "SELECT 1.5 AS payload_json",
        "SELECT NULL AS payload_json",
        "SELECT CAST(X'80' AS TEXT) AS payload_json",
    ] {
        let legacy = db
            .transaction(|tx| Ok(tx.query_row(sql, [], |row| row.get::<_, String>(0))?))
            .unwrap_err();
        let error = material.read(|| read(sql)).unwrap_err();
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        assert_eq!(format!("{error:#}"), format!("{legacy:#}"));
        assert_eq!(material.finish()?, header());
    }
    let error = material
        .read(|| read("SELECT '{' AS payload_json"))
        .unwrap_err();
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert!(material.finish().is_err());
    drop(material);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_source_column_error_precedes_vec_admission_without_extra_charge() -> Result<()> {
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO tasks(task_id, owner_agent_id, kind, status, revision,
                created_at, updated_at, payload_json)
             VALUES('invalid-task', ?1, 'command_task', 'running', 1, ?2, ?2, X'7b')",
            params![scope().agent_id, timestamp(now())],
        )?;
        Ok(())
    })?;
    let budget = ledger()?;
    let material =
        ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    let read = |limit| {
        db.transaction(|tx| {
            context_tasks_query_on(
                tx,
                &scope().agent_id,
                "owner_agent_id = ?1",
                [&scope().agent_id],
                limit,
            )
        })
    };
    assert!(material.read(|| read(0))?.is_empty());
    let error = material.read(|| read(1)).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::InvalidColumnType(0, name, rusqlite::types::Type::Blob))
            if name == "payload_json"
    ));
    assert_eq!(material.finish()?, header());
    assert_eq!(format!("{error:#}"), format!("{:#}", read(1).unwrap_err()));
    db.transaction(|tx| {
        tx.execute("UPDATE tasks SET payload_json='{'", [])?;
        Ok(())
    })?;
    let error = material.read(|| read(1)).unwrap_err();
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert!(material.finish().is_err());
    drop(material);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn latest_state_source_borrows_before_exact_admission_and_latches_rejection() -> Result<()> {
    let (_home, db) = fixture()?;
    let exact = add(header(), wire_cost(&record())?);
    for (bytes, records, admitted) in [
        (exact.0, exact.1, true),
        (exact.0 - 1, exact.1, false),
        (exact.0, exact.1 - 1, false),
    ] {
        let budget = ledger()?;
        let material = ResourceReadMaterial::with_test_limits(&budget, &scope(), bytes, records)?;
        let result = material.read(|| db.agent_states().latest(&scope().agent_id));
        if admitted {
            assert_eq!(result?, Some(record()));
            assert_eq!(material.finish()?, exact);
            assert_eq!(material.read(|| db.agent_states().latest("absent"))?, None);
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("ResourceExhausted"));
            assert!(material
                .read(|| db.agent_states().latest("absent"))
                .is_err());
        }
        drop(material);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn brief_and_event_sources_preserve_order_and_admit_raw_and_typed_material() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut expected = Vec::new();
    for index in 0..5 {
        let mut brief = BriefRecord::new(
            scope().agent_id,
            BriefKind::Result,
            format!("source {index} {{,:}} \"中文\""),
            None,
            None,
        );
        brief.created_at = now() + chrono::Duration::seconds(index);
        db.transaction(|tx| {
            insert_brief_evidence_tx(tx, &brief)?;
            insert_audit_event_tx(
                tx,
                Some(&scope().agent_id),
                &AuditEvent::legacy("source_event", serde_json::json!({"index": index})),
            )
        })?;
        expected.push(brief);
    }
    let events = db.audit_events().recent(Some(&scope().agent_id), 3)?;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].data["index"], 2);
    assert_eq!(events[2].data["index"], 4);
    let budget = ledger()?;
    let phase = ResourceReadMaterial::new(&budget, &scope())?;
    let (briefs, actual_events) = phase.read(|| {
        Ok((
            db.evidence().recent_briefs(&scope().agent_id, 3)?,
            db.audit_events().recent(Some(&scope().agent_id), 3)?,
        ))
    })?;
    assert_eq!(briefs, expected[2..]);
    assert_eq!(actual_events, events);
    let mut exact = add(
        header(),
        add(
            add(
                (4 * std::mem::size_of::<EvidencePayloadRow>() as u64 * 8, 32),
                (4 * std::mem::size_of::<BriefRecord>() as u64 * 8, 32),
            ),
            (4 * std::mem::size_of::<AuditEvent>() as u64 * 8, 32),
        ),
    );
    for brief in &expected[2..] {
        exact = add(exact, (serde_json::to_string(brief)?.len() as u64 * 8, 8));
        exact = add(exact, wire_cost(brief)?);
    }
    for event in &events {
        exact = add(exact, wire_cost(event)?);
    }
    assert_eq!(phase.finish()?, exact);
    let guard = phase.guard()?;
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(guard);
    assert_eq!(budget.report()?.observed_bytes, 0);
    for (bytes, records) in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), bytes, records)?;
        assert!(phase
            .read(|| {
                db.evidence().recent_briefs(&scope().agent_id, 3)?;
                db.audit_events().recent(Some(&scope().agent_id), 3)
            })
            .is_err());
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

fn snapshot(db: &crate::runtime_db::RuntimeDb) -> Result<(String, String, i64, String, i64)> {
    db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT payload_json,status,turn_index,updated_at,control_revision FROM agent_states",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?)
    })
}

#[test]
fn state_sql_read_write_read_uses_original_allowance_and_last_ack_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let budget = ledger()?;
    let mut changed = record();
    changed.turn_index = 1;
    changed.status = AgentStatus::AwakeIdle;
    let body = add(
        add(state_read_cost(&record())?, state_read_cost(&changed)?),
        add(wire_cost(&changed)?, wire_cost(&now())?),
    );
    assert_eq!(
        wire_cost(&now())?,
        cost(r#""2026-10-17T11:22:23.123456789Z""#.len(), 1)
    );
    let exact = add(add(header(), header()), body);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        db.transaction(|tx| {
            assert_eq!(agent_state_tx(tx, "scheduling-agent")?, Some(record()));
            upsert_agent_state_at_tx(tx, &changed, now())?;
            assert_eq!(
                agent_state_tx(tx, "scheduling-agent")?,
                Some(changed.clone())
            );
            Ok(())
        })?;
    }
    assert_eq!(transaction.finish()?, add(header(), body));
    assert_eq!(phase.finish()?, exact);
    let first = transaction.guard()?;
    let last = first.clone();
    assert!(std::sync::Arc::ptr_eq(&first, &last));
    {
        let _entry = transaction.enter();
        let error = db
            .transaction(|tx| agent_state_tx(tx, "scheduling-agent"))
            .unwrap_err();
        assert!(format!("{error:#}")
            .to_lowercase()
            .contains("resourceexhausted"));
    }
    assert!(phase.finish().is_err());
    assert!(phase.transaction().is_err());
    drop((phase, transaction, changed));
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn state_decode_uses_exact_nodes_and_rejects_one_unit_short() -> Result<()> {
    let (_home, db) = fixture()?;
    let budget = ledger()?;
    let exact = add(header(), state_read_cost(&record())?);
    for (bytes_short, records_short) in [(0, 0), (1, 0), (0, 1)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &budget,
            &scope(),
            exact.0 - bytes_short,
            exact.1 - records_short,
        )?;
        let result = phase.read(|| db.transaction(|tx| agent_state_tx(tx, "scheduling-agent")));
        if bytes_short == 0 && records_short == 0 {
            assert_eq!(result?, Some(record()));
            assert_eq!(phase.finish()?, exact);
        } else {
            assert!(format!("{:#}", result.unwrap_err())
                .to_lowercase()
                .contains("resourceexhausted"));
            assert!(phase.finish().is_err());
            assert!(phase
                .read(|| db.transaction(|tx| agent_state_tx(tx, "scheduling-agent")))
                .is_err());
        }
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    assert_eq!(
        db.transaction(|tx| agent_state_tx(tx, "scheduling-agent"))?,
        Some(record())
    );
    Ok(())
}

#[test]
fn each_state_output_gate_rejects_before_sql_and_releases_failed_transaction() -> Result<()> {
    let (_home, db) = fixture()?;
    let before = snapshot(&db)?;
    let mut changed = record();
    changed.turn_index = 2;
    let mut prefix = add(header(), header());
    for next in [wire_cost(&changed)?, wire_cost(&now())?] {
        for bytes in [true, false] {
            let budget = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes {
                    prefix.0 + next.0 - 1
                } else {
                    64 * 1024 * 1024
                },
                if bytes { 65536 } else { prefix.1 + next.1 - 1 },
            )?;
            let transaction = phase.transaction()?;
            {
                let _entry = transaction.enter();
                let error = db
                    .transaction(|tx| upsert_agent_state_at_tx(tx, &changed, now()))
                    .unwrap_err();
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(db
                    .transaction(|tx| upsert_agent_state_at_tx(tx, &changed, now()))
                    .is_err());
            }
            assert_eq!(snapshot(&db)?, before);
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            assert!(transaction.finish().is_err());
            drop(transaction);
            assert_eq!(budget.report()?.observed_bytes, header().0);
            assert!(phase.finish().is_err());
            assert!(phase.transaction().is_err());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        prefix = add(prefix, next);
    }
    Ok(())
}

#[test]
fn borrowed_state_decode_gates_before_parser_and_missing_rows_are_free() -> Result<()> {
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        tx.execute("UPDATE agent_states SET payload_json='{'", [])?;
        Ok(())
    })?;
    let budget = ledger()?;
    let prefix = add(header(), header());
    let next = ((1 + 256) * 8, 8);
    for bytes in [true, false] {
        let phase = ResourceReadMaterial::with_test_limits(
            &budget,
            &scope(),
            if bytes {
                prefix.0 + next.0 - 1
            } else {
                64 * 1024 * 1024
            },
            if bytes { 65536 } else { prefix.1 + next.1 - 1 },
        )?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            assert!(db
                .transaction(|tx| agent_state_tx(tx, "missing"))?
                .is_none());
            assert_eq!(phase.finish()?, prefix);
            let error = db
                .transaction(|tx| agent_state_tx(tx, "scheduling-agent"))
                .unwrap_err();
            assert!(format!("{error:#}")
                .to_lowercase()
                .contains("resourceexhausted"));
            assert!(db
                .transaction(|tx| agent_state_tx(tx, "scheduling-agent"))
                .is_err());
        }
        assert_eq!(budget.report()?.observed_bytes, prefix.0);
        assert!(transaction.finish().is_err());
        drop(transaction);
        assert_eq!(budget.report()?.observed_bytes, header().0);
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    let phase = ResourceReadMaterial::new(&budget, &scope())?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        let error = db
            .transaction(|tx| agent_state_tx(tx, "scheduling-agent"))
            .unwrap_err();
        assert!(error.downcast_ref::<serde_json::Error>().is_some());
    }
    assert_eq!(transaction.finish()?, add(add(header(), next), cost(1, 1)));
    drop(transaction);
    assert_eq!(budget.report()?.observed_bytes, header().0);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn state_sql_status_alias_turn_monotonicity_and_inspection_contract_are_unchanged() -> Result<()> {
    for status in [
        AgentStatus::Booting,
        AgentStatus::AwakeIdle,
        AgentStatus::AwakeRunning,
        AgentStatus::AwaitingTask,
        AgentStatus::Asleep,
        AgentStatus::Stopped,
    ] {
        let (_home, db) = fixture()?;
        let mut state = record();
        state.status = status;
        state.turn_index = 5;
        db.transaction(|tx| upsert_agent_state_at_tx(tx, &state, now()))?;
        let stored = snapshot(&db)?;
        assert_eq!(serde_json::from_str::<AgentState>(&stored.0)?, state);
        assert_eq!(
            serde_json::to_string(&state.status)?,
            format!("\"{}\"", stored.1)
        );
        assert_eq!(stored.2, 5);
        assert_eq!(stored.3, "2026-10-17T11:22:23.123Z");
        assert_eq!(stored.4, 2);
        let mut stale = state.clone();
        stale.turn_index = 4;
        db.transaction(|tx| upsert_agent_state_at_tx(tx, &stale, now()))?;
        assert_eq!(snapshot(&db)?, stored);
        db.transaction(|tx| upsert_agent_state_at_tx(tx, &state, now()))?;
        assert_eq!(snapshot(&db)?.4, 3);
        assert_eq!(
            db.transaction(|tx| agent_state_tx(tx, "scheduling-agent"))?,
            Some(state)
        );
    }
    let (_home, db) = fixture()?;
    let mut stopped = record();
    stopped.status = AgentStatus::Stopped;
    let alias = serde_json::to_string(&stopped)?.replace("\"stopped\"", "\"paused\"");
    db.transaction(|tx| {
        tx.execute("UPDATE agent_states SET payload_json=?1", [&alias])?;
        Ok(())
    })?;
    assert_eq!(
        db.transaction(|tx| agent_state_tx(tx, "scheduling-agent"))?,
        Some(stopped)
    );
    Ok(())
}
