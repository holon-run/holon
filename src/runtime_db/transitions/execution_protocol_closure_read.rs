//! Full invariant validation with bounded row ownership for memory closure.
use super::*;
use crate::domain::execution_protocol::{ExecutionAttemptState, WorkItemExecutionRecord};

pub(crate) struct ValidatedClosureProtocol {
    agent_id: String,
    work_items: BTreeMap<String, WorkItemExecutionRecord>,
}
impl ValidatedClosureProtocol {
    pub(crate) fn into_work_items(
        self,
        agent_id: &str,
    ) -> Result<BTreeMap<String, WorkItemExecutionRecord>> {
        anyhow::ensure!(
            self.agent_id == agent_id,
            "closure protocol projection crossed Agent scope"
        );
        Ok(self.work_items)
    }
}

/// SQL sizes precede every decode. History remains subject to the original
/// inventory bound; only the Work map and current/reciprocal rows are owned.
pub(in crate::runtime_db) fn closure_protocol_material_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<(usize, usize)> {
    let (works, work_bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(work_item_id AS BLOB))),0) FROM execution_protocol_work_items WHERE agent_id=?1",
        [agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut inventory_records = works;
    let mut inventory_bytes = work_bytes;
    let mut bytes = work_bytes
        .checked_mul(4)
        .and_then(|n| {
            n.checked_add(agent_id.len() + std::mem::size_of::<ValidatedClosureProtocol>())
        })
        .context("closure Work material overflow")?;
    for (table, id) in [
        ("execution_protocol_attempts", "attempt_id"),
        ("execution_protocol_outcomes", "outcome_id"),
    ] {
        let (count, total): (usize, usize) = tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM {table} WHERE agent_id=?1"), [agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        inventory_records = inventory_records
            .checked_add(count)
            .context("closure inventory overflow")?;
        inventory_bytes = inventory_bytes
            .checked_add(total)
            .context("closure inventory overflow")?;
        anyhow::ensure!(
            inventory_records <= 65536 && inventory_bytes <= 64 * 1024 * 1024,
            "closure protocol inventory exceeds original bound"
        );
        let peak: usize = tx.query_row(&format!("SELECT COALESCE(MAX(length(CAST(payload_json AS BLOB))+length(CAST({id} AS BLOB))),0) FROM {table} WHERE agent_id=?1"), [agent_id], |r| r.get(0))?;
        bytes = bytes
            .checked_add(
                peak.checked_mul(2)
                    .context("closure row material overflow")?,
            )
            .context("closure material overflow")?;
    }
    Ok((
        bytes,
        works
            .checked_mul(4)
            .and_then(|n| n.checked_add(4))
            .context("closure record material overflow")?,
    ))
}

fn related<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
    id: &str,
) -> Result<Option<T>> {
    tx.query_row(sql, params![agent_id, id], |row| {
        let raw = crate::runtime_db::repositories::borrowed_sql_text(row, 0)?;
        serde_json::from_str(raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .optional()
    .map_err(Into::into)
}

pub(crate) fn read_validated_closure_protocol_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    admitted: &crate::runtime_db::dispatch_memory::AdmittedFinalWork<'_, '_>,
) -> Result<Option<ValidatedClosureProtocol>> {
    admitted.validate_protocol_read(tx, agent_id)?;
    if !partition_exists_tx(tx, agent_id)? {
        return Ok(None);
    }
    let (bytes, records) = closure_protocol_material_tx(tx, agent_id)?;
    admitted.validate_protocol_material(bytes, records)?;
    crate::diagnostics::resource_io::charge_owned_metadata(u64::try_from(bytes)?, records)?;
    read_protocol_rows_tx(tx, agent_id).map(Some)
}

fn read_protocol_rows_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<ValidatedClosureProtocol> {
    // One map survives the traversal. Each decoded history row and its one
    // reciprocal row die before advancing the cursor; no history Vec/map grows.
    let work_items = load_payload_map(
        tx,
        "SELECT work_item_id,payload_json FROM execution_protocol_work_items WHERE agent_id=?1",
        agent_id,
    )?;
    let mut attempts = tx.prepare(
        "SELECT attempt_id,payload_json FROM execution_protocol_attempts WHERE agent_id=?1",
    )?;
    let mut rows = attempts.query([agent_id])?;
    let mut open = 0usize;
    while let Some(row) = rows.next()? {
        let id = crate::runtime_db::repositories::borrowed_sql_text(row, 0)?;
        let attempt: ExecutionAttempt =
            serde_json::from_str(crate::runtime_db::repositories::borrowed_sql_text(row, 1)?)?;
        if attempt.state == ExecutionAttemptState::Open {
            open += 1;
            if open > 1 {
                bail!("execution partition contains more than one open attempt");
            }
        }
        let outcome: Option<ExecutionOutcomeRecord> = attempt.terminal_outcome_id.as_deref().map(|id|
            related(tx, "SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2", agent_id, id)
        ).transpose()?.flatten();
        execution_protocol::validate_attempt_invariants(
            agent_id,
            id,
            &attempt,
            &work_items,
            outcome.as_ref(),
        )
        .map_err(anyhow::Error::msg)?;
    }
    drop(rows);
    for work in work_items.values() {
        let attempt: Option<ExecutionAttempt> = match &work.state {
            WorkItemExecutionState::InFlight { attempt_id, .. } => related(tx,
                "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2", agent_id, attempt_id)?,
            _ => None,
        };
        execution_protocol::validate_work_invariants(work, attempt.as_ref())
            .map_err(anyhow::Error::msg)?;
    }
    let mut outcomes = tx.prepare(
        "SELECT outcome_id,payload_json FROM execution_protocol_outcomes WHERE agent_id=?1",
    )?;
    let mut rows = outcomes.query([agent_id])?;
    while let Some(row) = rows.next()? {
        let id = crate::runtime_db::repositories::borrowed_sql_text(row, 0)?;
        let outcome: ExecutionOutcomeRecord =
            serde_json::from_str(crate::runtime_db::repositories::borrowed_sql_text(row, 1)?)?;
        let attempt: Option<ExecutionAttempt> = related(tx,
            "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2", agent_id, &outcome.attempt_id)?;
        execution_protocol::validate_outcome_invariants(id, &outcome, attempt.as_ref())
            .map_err(anyhow::Error::msg)?;
    }
    Ok(ValidatedClosureProtocol {
        agent_id: agent_id.to_owned(),
        work_items,
    })
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod tests {
    use super::*;
    use crate::domain::execution_protocol::*;

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
        tx.execute_batch("CREATE TABLE IF NOT EXISTS execution_protocol_partitions(agent_id TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS execution_protocol_work_items(agent_id TEXT,work_item_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,work_item_id));
            CREATE TABLE IF NOT EXISTS execution_protocol_attempts(agent_id TEXT,attempt_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,attempt_id));
            CREATE TABLE IF NOT EXISTS execution_protocol_outcomes(agent_id TEXT,outcome_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,outcome_id));
            DELETE FROM execution_protocol_work_items;DELETE FROM execution_protocol_attempts;DELETE FROM execution_protocol_outcomes;")?;
        tx.execute(
            "INSERT OR IGNORE INTO execution_protocol_partitions VALUES(?1)",
            [&state.agent_id],
        )?;
        for (table, records) in [
            (
                "execution_protocol_work_items",
                state
                    .work_items
                    .iter()
                    .map(|(id, r)| Ok((id, serde_json::to_string(r)?)))
                    .collect::<Result<Vec<_>>>()?,
            ),
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
            read_protocol_rows_tx(&tx, "agent-a")?.work_items,
            valid.work_items
        );
        assert!(read_protocol_rows_tx(&tx, "agent-a")?
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
                    bad.work_items.get_mut("work").unwrap().state =
                        WorkItemExecutionState::Runnable {
                            generation: 0,
                            recovery_ref: None,
                        }
                }
                9 => {
                    bad.work_items.get_mut("work").unwrap().state =
                        WorkItemExecutionState::InFlight {
                            generation: 1,
                            attempt_id: "absent".into(),
                        }
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
                read_protocol_rows_tx(&tx, "agent-a").is_err(),
                "stream accepted corruption {index}"
            );
        }
        write(&tx, &valid)?;
        tx.execute(
            "UPDATE execution_protocol_attempts SET payload_json='!'",
            [],
        )?;
        assert!(read_protocol_rows_tx(&tx, "agent-a").is_err());
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
        let expected = load_state_tx(&tx, "agent-a")?;
        assert_eq!(
            read_protocol_rows_tx(&tx, "agent-a")?.work_items,
            expected.work_items
        );
        assert_eq!(expected.attempts.len(), 512);
        Ok(())
    }
}
