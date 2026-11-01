//! Canonical turn allocation and its state/input projection commit.

use anyhow::{anyhow, ensure, Result};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::domain::execution_protocol::{
    ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
};
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::types::{
    AgentIdentityRecord, AgentRegistryStatus, AgentState, ExecutionAdmissionProvenance, TurnOwner,
    TurnRecord,
};

#[cfg(test)]
use super::evidence::upsert_agent_state_tx;
#[cfg(test)]
use super::repositories::{upsert_turn_record_tx, validate_turn_record_identity};
use super::{RuntimeStateTransitionConflict, TurnRecordRepository};

impl TurnRecordRepository<'_> {
    /// Only the allocation scalar is read; retained Turn bodies are not inputs.
    pub(crate) fn prepare_next_index(
        &self,
        scope: &InputEvidenceScope,
        current_index: u64,
    ) -> Result<u64> {
        self.db.read_for_preparation(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &scope.agent_id,
            )?;
            next_turn_index_tx(tx, &scope.agent_id, current_index)
        })
    }

    #[cfg(test)]
    pub(crate) fn start(
        &self,
        expected: Option<&AgentState>,
        state: &AgentState,
        record: &TurnRecord,
        incarnation: u64,
    ) -> Result<AgentState> {
        ensure!(
            state.id == record.agent_id
                && expected.is_none_or(|expected| expected.id == state.id)
                && state.current_turn_id.as_deref() == Some(record.turn_id.as_str())
                && state.current_run_id == record.run_id
                && record.terminal.is_none(),
            "turn start requires matching state and nonterminal turn identity"
        );
        self.db.transaction(|tx| {
            validate_start_admission_tx(tx, state, record, incarnation)?;
            let actual = tx
                .query_row(
                    "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                    [&state.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| serde_json::from_str::<AgentState>(&payload))
                .transpose()?;
            let existing = tx
                .query_row(
                    "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                    [&record.turn_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| serde_json::from_str::<TurnRecord>(&payload))
                .transpose()?;
            let mut state = state.clone();
            let mut record = record.clone();
            if let Some(existing) = existing {
                record.turn_index = existing.turn_index;
                record.created_at = existing.created_at;
                validate_turn_record_identity(&existing, &record)?;
                state.turn_index = existing.turn_index;
                ensure!(
                    existing.terminal.is_none(),
                    "cannot restart a terminal turn"
                );
                if actual.as_ref() != Some(&state) {
                    return Err(RuntimeStateTransitionConflict::concurrent_mutation(
                        "turn start",
                        &record.turn_id,
                    )
                    .into());
                }
                // Do not replace evidence accumulated since the original start.
                return Ok(state);
            }
            if actual.as_ref() != expected {
                return Err(RuntimeStateTransitionConflict::concurrent_mutation(
                    "agent_state",
                    &state.id,
                )
                .into());
            }
            let next = next_turn_index_tx(tx, &state.id, 0)?;
            ensure!(
                actual.as_ref().is_none_or(|state| state.turn_index < next),
                "persisted turn counter is outside its canonical integer range"
            );
            state.turn_index = next;
            record.turn_index = state.turn_index;
            upsert_agent_state_tx(tx, &state)?;
            // Includes input assignment and conversation revisions in this transaction.
            upsert_turn_record_tx(tx, &record)?;
            Ok(state)
        })
    }
}

fn next_turn_index_tx(tx: &Transaction<'_>, agent_id: &str, current_index: u64) -> Result<u64> {
    let current = i64::try_from(current_index)
        .map_err(|_| anyhow!("turn index exceeds SQLite integer range"))?;
    let retained: Option<i64> = tx.query_row(
        "SELECT MAX(turn_index) FROM turn_records WHERE agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    let persisted: Option<i64> = tx
        .query_row(
            "SELECT turn_index FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    retained
        .unwrap_or(0)
        .max(persisted.unwrap_or(0))
        .max(current)
        .checked_add(1)
        .filter(|next| *next > 0)
        .map(|next| next as u64)
        .ok_or_else(|| anyhow!("turn index exceeds SQLite integer range"))
}

/// The prepared State, Turn and dispatch memory share an index before commit.
/// A concurrent history advance must reject that command, never renumber it.
pub(super) fn validate_prepared_turn_index_tx(
    tx: &Transaction<'_>,
    record: &TurnRecord,
) -> Result<()> {
    let existing: Option<(String, i64)> = tx
        .query_row(
            "SELECT agent_id, turn_index FROM turn_records WHERE turn_id = ?1",
            [&record.turn_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let matches = match existing {
        Some((agent_id, index)) => {
            agent_id == record.agent_id && u64::try_from(index) == Ok(record.turn_index)
        }
        None => record.turn_index == next_turn_index_tx(tx, &record.agent_id, 0)?,
    };
    ensure!(
        matches,
        RuntimeStateTransitionConflict::concurrent_mutation("turn index", &record.turn_id)
    );
    Ok(())
}

pub(super) fn validate_start_admission_tx(
    tx: &Transaction<'_>,
    state: &AgentState,
    record: &TurnRecord,
    incarnation: u64,
) -> Result<()> {
    let identity = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [&state.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<AgentIdentityRecord>(&payload))
        .transpose()?;
    ensure!(
        identity.as_ref().map_or(incarnation == 1, |identity| {
            identity.incarnation == incarnation && identity.status == AgentRegistryStatus::Active
        }),
        "turn start agent incarnation is no longer active"
    );
    let Some(binding) = state.current_execution_binding.as_ref() else {
        return Ok(());
    };
    ensure!(
        binding.turn_id == record.turn_id
            && binding.work_item_id == record.current_work_item_id
            && record
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                == Some(binding.source_message_id.as_str()),
        "turn start execution binding does not match its input"
    );
    let Some(provenance) = binding.admission_provenance.as_ref() else {
        return Ok(());
    };
    let ExecutionAdmissionProvenance::Canonical { activation_id, .. } = provenance else {
        return Err(anyhow!("turn start requires canonical execution admission"));
    };
    ensure!(
        binding.activation_id.as_ref() == Some(activation_id),
        "turn start activation mismatch"
    );
    let payload: String = tx.query_row(
        "SELECT payload_json FROM execution_protocol_attempts
         WHERE agent_id = ?1 AND attempt_id = ?2",
        params![state.id, activation_id],
        |row| row.get(0),
    )?;
    let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
    let owner = match attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem { work_item_id },
        ExecutionBinding::Conversation { interaction_id } => {
            TurnOwner::Conversation { interaction_id }
        }
        ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle { agent_id },
        ExecutionBinding::Command => TurnOwner::Command,
    };
    // An attempt retains its ingress turn across recovery; the replay execution
    // gets a new turn identity, so those two turn ids must not be equated here.
    ensure!(
        attempt.agent_id == state.id
            && attempt.attempt_id == *activation_id
            && attempt.state == ExecutionAttemptState::Open
            && attempt.source_message_id.as_ref() == Some(&binding.source_message_id)
            && attempt
                .run_id
                .as_ref()
                .is_none_or(|id| Some(id) == state.current_run_id.as_ref())
            && record.owner.as_ref() == Some(&owner)
            && binding.claimed_work_revision == attempt.admitted_fences.work_item_source_revision,
        "turn start canonical execution admission is stale or conflicting"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::tests::test_support::TempRuntimeDb;
    use crate::types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility};

    fn candidate(agent: &str, turn: &str) -> (AgentState, TurnRecord) {
        let mut state = AgentState::new(agent);
        state.current_turn_id = Some(turn.into());
        (state, TurnRecord::new(agent, turn, 0))
    }

    #[test]
    fn canonical_turn_start_reopens_with_persisted_high_water() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state/runtime.sqlite");
        let lock_path = dir.path().join("state/runtime.lock");
        let db = super::super::RuntimeDb::open_and_migrate(&path, &lock_path)?;
        db.turn_records()
            .upsert(&TurnRecord::new("a", "retained", 22))?;
        let (state, record) = candidate("a", "one");
        let first = db.turn_records().start(None, &state, &record, 1)?;
        assert_eq!(first.turn_index, 23);
        drop(db);

        let db = super::super::RuntimeDb::open_and_migrate(&path, &lock_path)?;
        assert_eq!(db.agent_states().latest("a")?, Some(first.clone()));
        let (state, record) = candidate("a", "two");
        let second = db.turn_records().start(Some(&first), &state, &record, 1)?;
        assert_eq!(second.turn_index, 24);
        assert_eq!(db.agent_states().latest("a")?, Some(second));
        assert_eq!(
            db.turn_records()
                .by_id(Some("a"), "one")?
                .unwrap()
                .turn_index,
            23
        );
        Ok(())
    }

    #[test]
    fn canonical_turn_start_rejects_stale_or_inactive_incarnation() -> Result<()> {
        let fixture = TempRuntimeDb::new()?;
        let db = &fixture.db;
        let mut identity = AgentIdentityRecord::new(
            "a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.incarnation = 2;
        db.agent_identities().upsert(&identity)?;
        let (state, record) = candidate("a", "one");
        assert!(db.turn_records().start(None, &state, &record, 1).is_err());
        assert!(db.agent_states().latest("a")?.is_none());
        assert!(db.turn_records().by_id(Some("a"), "one")?.is_none());
        let first = db.turn_records().start(None, &state, &record, 2)?;
        identity.status = AgentRegistryStatus::Deleting;
        db.agent_identities().upsert(&identity)?;
        assert!(db.turn_records().start(None, &state, &record, 2).is_err());
        identity.status = AgentRegistryStatus::Active;
        identity.incarnation = 3;
        db.agent_identities().upsert(&identity)?;
        assert!(db.turn_records().start(None, &state, &record, 2).is_err());
        assert_eq!(db.agent_states().latest("a")?, Some(first));
        Ok(())
    }

    #[test]
    fn canonical_turn_start_counter_matrix_preserves_legacy_ties_and_agent_scope() -> Result<()> {
        for (counter, history, next) in [
            (None, vec![], 1),
            (None, vec![22], 23),
            (Some(2), vec![22], 23),
            (Some(40), vec![22], 41),
            (Some(0), vec![1, 22, 22], 23),
        ] {
            let fixture = TempRuntimeDb::new()?;
            let db = &fixture.db;
            if let Some(counter) = counter {
                let mut state = AgentState::new("a");
                state.turn_index = counter;
                db.agent_states().upsert_for_fixture(&state)?;
            }
            for (i, index) in history.iter().enumerate() {
                db.turn_records()
                    .upsert(&TurnRecord::new("a", format!("old-{i}"), *index))?;
            }
            db.turn_records()
                .upsert(&TurnRecord::new("b", "other-agent", 900))?;
            let expected = db.agent_states().latest("a")?;
            let (state, record) = candidate("a", "new");
            let state = db
                .turn_records()
                .start(expected.as_ref(), &state, &record, 1)?;
            assert_eq!(state.turn_index, next);
            assert_eq!(db.agent_states().latest("a")?, Some(state));
            assert_eq!(
                db.turn_records()
                    .by_id(Some("a"), "new")?
                    .unwrap()
                    .turn_index,
                next
            );
            for (i, index) in history.iter().enumerate() {
                assert_eq!(
                    db.turn_records()
                        .by_id(Some("a"), &format!("old-{i}"))?
                        .unwrap()
                        .turn_index,
                    *index
                );
            }
        }
        Ok(())
    }

    #[test]
    fn canonical_turn_start_retry_preserves_identity_evidence_and_later_state() -> Result<()> {
        let fixture = TempRuntimeDb::new()?;
        let db = &fixture.db;
        let (state, mut record) = candidate("a", "one");
        record.input_message_ids.push("input".into());
        let started = db.turn_records().start(None, &state, &record, 1)?;
        let mut stored = db.turn_records().by_id(Some("a"), "one")?.unwrap();
        stored.tool_execution_ids.push("later-evidence".into());
        db.turn_records().upsert(&stored)?;
        assert_eq!(db.turn_records().start(None, &state, &record, 1)?, started);
        assert_eq!(db.turn_records().by_id(Some("a"), "one")?, Some(stored));
        let mut conflicting = record.clone();
        conflicting.agent_id = "b".into();
        let (other_state, _) = candidate("b", "one");
        assert!(db
            .turn_records()
            .start(None, &other_state, &conflicting, 1)
            .is_err());
        let mut conflicting = record.clone();
        conflicting.run_id = Some("other-run".into());
        let mut other_state = state.clone();
        other_state.current_run_id = conflicting.run_id.clone();
        assert!(db
            .turn_records()
            .start(None, &other_state, &conflicting, 1)
            .is_err());
        let (next_state, next_record) = candidate("a", "two");
        let next = db
            .turn_records()
            .start(Some(&started), &next_state, &next_record, 1)?;
        assert_eq!(next.turn_index, 2);
        assert!(db.turn_records().start(None, &state, &record, 1).is_err());
        assert_eq!(db.agent_states().latest("a")?, Some(next));
        let assignments: i64 = db.transaction(|tx| {
            Ok(tx.query_row(
                "SELECT COUNT(*) FROM conversation_input_assignments",
                [],
                |row| row.get(0),
            )?)
        })?;
        assert_eq!(assignments, 1);
        Ok(())
    }

    #[test]
    fn canonical_turn_start_rolls_back_state_turn_and_input_projection() -> Result<()> {
        for table in [
            "agent_states",
            "turn_records",
            "conversation_input_assignments",
        ] {
            let fixture = TempRuntimeDb::new()?;
            let db = &fixture.db;
            let (state, mut record) = candidate("a", "one");
            record.input_message_ids.push("input".into());
            db.transaction(|tx| {
                tx.execute_batch(&format!(
                    "CREATE TRIGGER reject_start BEFORE INSERT ON {table}
                     BEGIN SELECT RAISE(ABORT, 'injected start failure'); END;"
                ))?;
                Ok(())
            })?;
            assert!(
                db.turn_records().start(None, &state, &record, 1).is_err(),
                "{table}"
            );
            assert!(db.agent_states().latest("a")?.is_none(), "{table}");
            assert!(
                db.turn_records().by_id(Some("a"), "one")?.is_none(),
                "{table}"
            );
            db.transaction(|tx| {
                for table in [
                    "conversation_input_assignments",
                    "conversation_turn_revisions",
                    "conversation_source_revisions",
                ] {
                    let count: i64 =
                        tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                            row.get(0)
                        })?;
                    assert_eq!(count, 0, "{table}");
                }
                tx.execute_batch("DROP TRIGGER reject_start")?;
                Ok(())
            })?;
            assert_eq!(
                db.turn_records()
                    .start(None, &state, &record, 1)?
                    .turn_index,
                1
            );
        }
        Ok(())
    }

    #[test]
    fn canonical_turn_start_rejects_stale_state_and_integer_overflow() -> Result<()> {
        let fixture = TempRuntimeDb::new()?;
        let db = &fixture.db;
        let expected = AgentState::new("a");
        db.agent_states().upsert_for_fixture(&expected)?;
        let mut actual = expected.clone();
        actual.pending = 1;
        db.agent_states().upsert_for_fixture(&actual)?;
        let (state, record) = candidate("a", "one");
        assert!(db
            .turn_records()
            .start(Some(&expected), &state, &record, 1)
            .is_err());
        assert_eq!(db.agent_states().latest("a")?, Some(actual.clone()));
        db.turn_records()
            .upsert(&TurnRecord::new("a", "maximum", i64::MAX as u64))?;
        assert!(db
            .turn_records()
            .start(Some(&actual), &state, &record, 1)
            .is_err());
        assert!(db.turn_records().by_id(Some("a"), "one")?.is_none());
        assert_eq!(db.agent_states().latest("a")?, Some(actual));
        Ok(())
    }

    #[test]
    fn canonical_turn_start_competing_snapshots_commit_only_once() -> Result<()> {
        let fixture = TempRuntimeDb::new()?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut threads = Vec::new();
        for turn in ["one", "two"] {
            let db = fixture.db.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let (state, record) = candidate("a", turn);
                barrier.wait();
                db.turn_records().start(None, &state, &record, 1)
            }));
        }
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let current = fixture.db.agent_states().latest("a")?.unwrap();
        assert_eq!(current.turn_index, 1);
        let (state, record) = candidate("a", "retry-with-current-snapshot");
        assert_eq!(
            fixture
                .db
                .turn_records()
                .start(Some(&current), &state, &record, 1)?
                .turn_index,
            2
        );
        Ok(())
    }
}
