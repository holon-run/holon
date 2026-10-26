//! Memory lookup observes only the original incarnation's existing State.
use super::*;
use crate::{runtime_db::dispatch_memory::DispatchMemoryBinding, types::AgentState};

fn charge_identity_or_state(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    bytes: usize,
) -> Result<Arc<BudgetViewGuard>> {
    let bytes = u64::try_from(bytes)?
        .checked_mul(8)
        .context("TaskStop Memory material overflow")?;
    let limits = ledger.limits();
    ensure!(
        bytes
            <= limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("TaskStop Memory limits overflow")?
                .min(MAX_BYTES as u64)
            && limits.record_limit >= 8,
        super::super::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "TaskStop Memory identity/State"
        )
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: bytes,
            metadata_estimate_bytes: 0,
        },
    )?)
}

/// This observes availability only. It neither prepares Task nor grants Ready.
pub(crate) fn load_task_stop_phase_memory_binding_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: Option<&AgentState>,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<DispatchMemoryBinding>> {
    let _identity = charge_identity_or_state(
        ledger,
        scope,
        phase::task_stop_phase_identity_material_tx(tx, scope)?,
    )?;
    if phase::observe_original_task_stop_state_tx(tx, scope)?.is_some() {
        return Ok(None);
    }
    let state_bytes: usize = tx.query_row(
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id], |r| r.get(0),
    )?;
    let _state = charge_identity_or_state(ledger, scope, state_bytes)?;
    // Cold metadata preparation is finite and does not install a runtime/root.
    let cold_state;
    let state = match published_state {
        Some(state) => state,
        None => {
            let payload: String = tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                [&scope.agent_id],
                |r| r.get(0),
            )?;
            cold_state = serde_json::from_str::<AgentState>(&payload)?;
            ensure!(
                cold_state.id == scope.agent_id,
                "Stop Memory cold State scope differs"
            );
            &cold_state
        }
    };
    super::super::dispatch_memory::load_dispatch_memory_binding_tx(tx, scope, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        runtime_db::{
            evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
            RuntimeDb,
        },
        types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        },
    };
    #[test]
    fn no_state_or_retired_memory_does_not_follow_the_old_published_turn() -> Result<()> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "stop-memory-owner".into(),
            identity_incarnation: 1,
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let mut identity = AgentIdentityRecord::new(
            &scope.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let mut old = AgentState::new(&scope.agent_id);
        old.current_turn_id = Some("old-turn-must-not-be-read".into());
        db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            // A stale published Turn must not make a missing original State an error.
            assert!(
                load_task_stop_phase_memory_binding_tx(tx, &scope, Some(&old), &ledger)?.is_none()
            );
            Ok(())
        })?;
        identity.incarnation = 2;
        db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
            tx.execute(
                "UPDATE agent_states SET payload_json='{broken successor' WHERE agent_id=?1",
                [&scope.agent_id],
            )?;
            assert!(
                load_task_stop_phase_memory_binding_tx(tx, &scope, Some(&old), &ledger)?.is_none()
            );
            Ok(())
        })?;
        Ok(())
    }
    #[test]
    fn cold_original_state_memory_lookup_does_not_prepare_task_or_publish_a_runtime() -> Result<()>
    {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "stop-memory-cold".into(),
            identity_incarnation: 1,
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let identity = AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            upsert_agent_identity_tx(tx, &identity)?;
            upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
            tx.execute(
                "ALTER TABLE task_domain_current RENAME TO forbidden_task_preparation",
                [],
            )?;
            assert!(load_task_stop_phase_memory_binding_tx(tx, &scope, None, &ledger)?.is_none());
            Ok(())
        })?;
        ensure!(
            ledger.report()?.observed_bytes == 0,
            "cold State temporary view leaked"
        );
        Ok(())
    }

    #[test]
    fn oversized_normalized_run_is_rejected_before_canonical_state_tuple_decode() -> Result<()> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "stop-memory-normalized".into(),
            identity_incarnation: 1,
        };
        let mut limits = StateMemoryLimits::default();
        limits.history_per_agent_bytes = 4096;
        limits.pending_per_agent_bytes = 4096;
        let ledger = InputEvidenceBudgetLedger::new(limits)?;
        let mut state = AgentState::new(&scope.agent_id);
        state.current_turn_id = Some("original-turn".into());
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: None,
            admission_provenance: None,
            source_message_id: "original-message".into(),
            turn_id: "original-turn".into(),
            owner: None,
            work_item_id: None,
            claimed_work_revision: None,
        });
        db.transaction(|tx| {
            let identity = AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            upsert_agent_identity_tx(tx, &identity)?;
            upsert_agent_state_tx(tx, &state)?;
            tx.execute(
                "UPDATE agent_states SET current_run_id=?1 WHERE agent_id=?2",
                params!["x".repeat(8192), scope.agent_id],
            )?;
            let error = load_task_stop_phase_memory_binding_tx(tx, &scope, Some(&state), &ledger)
                .err()
                .context("oversize normalized run accepted")?;
            ensure!(
                error
                    .downcast_ref::<super::super::super::task_snapshot::TaskSnapshotError>()
                    .is_some(),
                "{error:#}"
            );
            Ok(())
        })?;
        ensure!(
            ledger.report()?.observed_bytes == 0,
            "temporary State guard leaked"
        );
        Ok(())
    }

    #[test]
    fn oversized_corrupt_identity_is_rejected_before_memory_json_decode() -> Result<()> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "stop-memory-budget".into(),
            identity_incarnation: 1,
        };
        let mut limits = StateMemoryLimits::default();
        limits.history_per_agent_bytes = 1024;
        limits.pending_per_agent_bytes = 1024;
        let ledger = InputEvidenceBudgetLedger::new(limits)?;
        db.transaction(|tx| {
            let identity = AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            upsert_agent_identity_tx(tx, &identity)?;
            tx.execute(
                "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
                params!["{".repeat(8192), scope.agent_id],
            )?;
            let error = load_task_stop_phase_memory_binding_tx(
                tx,
                &scope,
                Some(&AgentState::new(&scope.agent_id)),
                &ledger,
            )
            .err()
            .context("oversize identity accepted")?;
            ensure!(
                error
                    .downcast_ref::<super::super::super::task_snapshot::TaskSnapshotError>()
                    .is_some(),
                "{error:#}"
            );
            Ok(())
        })?;
        Ok(())
    }
}
