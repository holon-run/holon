use super::super::load_state_on;
use super::*;
use holon_domain::execution_protocol::*;
use rusqlite::Connection;

const UNMETERED: ExecutionReadMaterial = ExecutionReadMaterial {
    charge_owned_metadata: |_, _| Ok(()),
    admit_typed_json_payload: |_| Ok(()),
};

fn state() -> Result<ExecutionProtocolState> {
    let mut state = ExecutionProtocolState::empty("agent-a");
    let attempt: ExecutionAttempt = serde_json::from_value(serde_json::json!({
        "attempt_id":"attempt-0000", "agent_id":"agent-a",
        "source":{"identity":{"kind":"queue_message","message_id":"source"},"generation":1},
        "binding":{"kind":"command"},
        "provenance":{"origin":"operator","trust":"operator_instruction","priority":"normal"},
        "admitted_fences":{"source_revision":1,"agent_control_revision":1,"host_registry_revision":1},
        "state":"settled","terminal_outcome_id":"outcome-0000","admitted_at":"at","terminal_at":"at"
    }))?;
    let outcome = ExecutionOutcomeRecord {
        outcome_id: "outcome-0000".into(),
        attempt_id: attempt.attempt_id.clone(),
        outcome: ExecutionOutcome::Conversation(ConversationOutcome::Replied),
        created_at: "at".into(),
    };
    state.attempts.insert(attempt.attempt_id.clone(), attempt);
    state.outcomes.insert(outcome.outcome_id.clone(), outcome);
    state.work_items.insert(
        "work".into(),
        WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            },
        },
    );
    Ok(state)
}
fn write(tx: &Transaction<'_>, state: &ExecutionProtocolState) -> Result<()> {
    // Include the repository's WorkItem marker columns while leaving the
    // fixture free to seed invalid records for invariant checks.
    tx.execute_batch("CREATE TABLE IF NOT EXISTS execution_protocol_partitions(agent_id TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS execution_protocol_work_items(agent_id TEXT,work_item_id TEXT,source_revision INTEGER,generation INTEGER,lifecycle_state TEXT,payload_json TEXT,PRIMARY KEY(agent_id,work_item_id));
        CREATE TABLE IF NOT EXISTS execution_protocol_attempts(agent_id TEXT,attempt_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,attempt_id));
        CREATE TABLE IF NOT EXISTS execution_protocol_outcomes(agent_id TEXT,outcome_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,outcome_id));
        DELETE FROM execution_protocol_work_items;DELETE FROM execution_protocol_attempts;DELETE FROM execution_protocol_outcomes;")?;
    tx.execute(
        "INSERT OR IGNORE INTO execution_protocol_partitions VALUES(?1)",
        [&state.agent_id],
    )?;
    for (id, work) in &state.work_items {
        let payload = serde_json::to_value(work)?;
        let lifecycle = payload["state"]["state"]
            .as_str()
            .context("closure fixture WorkItem must have a state tag")?;
        tx.execute(
            "INSERT INTO execution_protocol_work_items
             (agent_id,work_item_id,source_revision,generation,lifecycle_state,payload_json)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                state.agent_id,
                id,
                work.source_revision,
                work.generation(),
                lifecycle,
                payload.to_string()
            ],
        )?;
    }
    for (table, records) in [
        (
            "execution_protocol_attempts",
            state
                .attempts
                .iter()
                .map(|(id, r)| Ok((id, serde_json::to_string(r)?)))
                .collect::<Result<Vec<_>>>()?,
        ),
        (
            "execution_protocol_outcomes",
            state
                .outcomes
                .iter()
                .map(|(id, r)| Ok((id, serde_json::to_string(r)?)))
                .collect::<Result<Vec<_>>>()?,
        ),
    ] {
        for (id, raw) in records {
            tx.execute(
                &format!("INSERT INTO {table} VALUES(?1,?2,?3)"),
                params![state.agent_id, id, raw],
            )?;
        }
    }
    Ok(())
}
#[test]
fn streamed_closure_uses_the_aggregate_invariant_rules_for_every_history_row() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    let tx = connection.transaction()?;
    let valid = state()?;
    write(&tx, &valid)?;
    assert_eq!(
        read_closure_rows_tx(&tx, "agent-a", UNMETERED)?.work_items,
        valid.work_items
    );

    assert!(read_closure_rows_tx(&tx, "agent-a", UNMETERED)?
        .into_work_items("foreign")
        .is_err());
    for index in 0..13 {
        let mut bad = valid.clone();
        let attempt = bad.attempts.get_mut("attempt-0000").unwrap();
        match index {
            0 => attempt.agent_id = "foreign".into(),
            1 => attempt.attempt_id = "different".into(),
            2 => attempt.terminal_outcome_id = None,
            3 => attempt.terminal_at = None,
            4 => {
                bad.outcomes.clear();
            }
            5 => bad.outcomes.get_mut("outcome-0000").unwrap().attempt_id = "absent".into(),
            6 => bad.outcomes.get_mut("outcome-0000").unwrap().outcome_id = "different".into(),
            7 => bad.work_items.get_mut("work").unwrap().source_revision = 0,
            8 => {
                bad.work_items.get_mut("work").unwrap().state = WorkItemExecutionState::Runnable {
                    generation: 0,
                    recovery_ref: None,
                }
            }
            9 => {
                attempt.state = ExecutionAttemptState::Open;
                attempt.terminal_at = None;
                attempt.terminal_outcome_id = None;
                attempt.binding = ExecutionBinding::WorkItem {
                    work_item_id: "work".into(),
                };
                attempt.admitted_fences.work_item_generation = Some(2);
                bad.outcomes.clear();
            }
            10 => attempt.state = ExecutionAttemptState::Open,
            11 => {
                attempt.state = ExecutionAttemptState::Open;
                attempt.terminal_at = None;
                attempt.terminal_outcome_id = None;
                let mut other = attempt.clone();
                other.attempt_id = "second".into();
                bad.attempts.insert(other.attempt_id.clone(), other);
                bad.outcomes.clear();
            }
            12 => {
                attempt.state = ExecutionAttemptState::Open;
                attempt.terminal_at = None;
                attempt.terminal_outcome_id = None;
                attempt.binding = ExecutionBinding::WorkItem {
                    work_item_id: "work".into(),
                };
                bad.outcomes.clear();
            }
            _ => unreachable!(),
        }
        assert!(
            execution_protocol::assert_invariants(&bad).is_err(),
            "invalid fixture {index}"
        );
        write(&tx, &bad)?;
        assert!(
            read_closure_rows_tx(&tx, "agent-a", UNMETERED).is_err(),
            "stream accepted corruption {index}"
        );
    }
    write(&tx, &valid)?;
    tx.execute(
        "UPDATE execution_protocol_attempts SET payload_json='!'",
        [],
    )?;
    assert!(read_closure_rows_tx(&tx, "agent-a", UNMETERED).is_err());
    Ok(())
}
#[test]
fn closure_read_converges_the_retired_needs_repair_outcome_encoding() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    let tx = connection.transaction()?;
    let valid = state()?;
    write(&tx, &valid)?;
    // A pre-v115 database still stores the retired WorkItem encoding; the
    // closure read must decode it through the shared migration entry.
    tx.execute(
        "UPDATE execution_protocol_outcomes SET payload_json=?1
         WHERE agent_id=?2 AND outcome_id=?3",
        params![
            serde_json::json!({
                "outcome_id": "outcome-0000",
                "attempt_id": "attempt-0000",
                "outcome": {
                    "owner": "work_item",
                    "outcome": {
                        "kind": "needs_repair",
                        "repair_id": "provider_dispatch_rejected:turn-1"
                    }
                },
                "created_at": "at"
            })
            .to_string(),
            "agent-a",
            "outcome-0000"
        ],
    )?;
    assert_eq!(
        read_closure_rows_tx(&tx, "agent-a", UNMETERED)?.work_items,
        valid.work_items,
        "the retired needs_repair encoding must not break the closure read"
    );
    // Damaged retired rows still fail closed.
    tx.execute(
        "UPDATE execution_protocol_outcomes SET payload_json=?1
         WHERE agent_id=?2 AND outcome_id=?3",
        params![
            serde_json::json!({
                "outcome_id": "outcome-0000",
                "attempt_id": "attempt-0000",
                "outcome": {
                    "owner": "work_item",
                    "outcome": { "kind": "needs_repair" }
                },
                "created_at": "at"
            })
            .to_string(),
            "agent-a",
            "outcome-0000"
        ],
    )?;
    assert!(read_closure_rows_tx(&tx, "agent-a", UNMETERED).is_err());
    Ok(())
}
#[test]
fn retained_history_grows_inventory_without_growing_owned_row_material() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    let tx = connection.transaction()?;
    let mut state = state()?;
    write(&tx, &state)?;
    let baseline = closure_protocol_material_tx(&tx, "agent-a")?;
    let a = state.attempts.values().next().unwrap().clone();
    let o = state.outcomes.values().next().unwrap().clone();
    for index in 1..512 {
        let mut a = a.clone();
        let mut o = o.clone();
        a.attempt_id = format!("attempt-{index:04}");
        o.outcome_id = format!("outcome-{index:04}");
        a.terminal_outcome_id = Some(o.outcome_id.clone());
        o.attempt_id = a.attempt_id.clone();
        state.attempts.insert(a.attempt_id.clone(), a);
        state.outcomes.insert(o.outcome_id.clone(), o);
    }
    write(&tx, &state)?;
    assert_eq!(closure_protocol_material_tx(&tx, "agent-a")?, baseline);
    let expected = load_state_on(&tx, "agent-a", UNMETERED)?.context("initialized partition")?;
    assert_eq!(
        read_closure_rows_tx(&tx, "agent-a", UNMETERED)?.work_items,
        expected.work_items
    );
    assert_eq!(expected.attempts.len(), 512);
    Ok(())
}

#[test]
fn owned_map_charges_and_admits_payload_before_decoding() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    let tx = connection.transaction()?;
    write(&tx, &state()?)?;
    for table in [
        "execution_protocol_attempts",
        "execution_protocol_work_items",
    ] {
        tx.execute(&format!("UPDATE {table} SET payload_json='!'"), [])?;
    }

    let reject_metadata = ExecutionReadMaterial {
        charge_owned_metadata: |_, _| Err(std::io::Error::other("metadata denied")),
        admit_typed_json_payload: |_| panic!("metadata must be admitted first"),
    };
    let reject_payload = ExecutionReadMaterial {
        charge_owned_metadata: |_, _| Ok(()),
        admit_typed_json_payload: |payload| {
            assert_eq!(payload, "!");
            anyhow::bail!("payload denied");
        },
    };
    for (material, expected) in [
        (reject_metadata, "metadata denied"),
        (reject_payload, "payload denied"),
    ] {
        let full = load_state_on(&tx, "agent-a", material)
            .err()
            .context("the full read must reject unadmitted material")?;
        assert_eq!(full.to_string(), expected);
        let streamed = read_closure_rows_tx(&tx, "agent-a", material)
            .err()
            .context("the streamed read must reject unadmitted material")?;
        assert_eq!(streamed.to_string(), expected);
    }
    Ok(())
}
