use super::*;
use crate::{
    runtime_db::{
        input_mutation_receipts::{
            canonical_command_digest, insert_input_mutation_receipt_tx, InputMutationFamily,
            InputMutationOutcome, InputMutationResult, InputMutationScopeOutcome,
            PreparedInputMutation,
        },
        task_stop::{
            insert_task_stop_with_observation_ack_tx, prepare_task_stop_tx, PreparedTaskStop,
            TaskStopSignal,
        },
    },
    types::TaskKind,
};

#[test]
fn incremental_stop_ack_matches_oracle_with_original_same_transaction_receipt() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let mut value = task("stop-target", TaskStatus::Running);
        value.detail = Some(serde_json::json!({"cancel_requested":false}));
        seed_tasks(tx, &[value.clone(), task("unchanged", TaskStatus::Running)])?;
        let scope = scope();
        let load = request();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let key = TaskKey::new(TaskFamily::Task, &value.id);
        let keys = BTreeSet::from([key.clone()]);
        let original = prepare_task_write_tx(tx, &scope, &keys)?;
        let mut incremental = None;
        tx.execute_batch("SAVEPOINT stop_delta_oracle")?;
        for use_delta in [true, false] {
            let baseline =
                preflight_task_write_snapshot_with_budget_tx(tx, &original, &load, &ledger)?
                    .require_current()?;
            let captured = baseline.resident.capture(&load.capture)?;
            let intent = PreparedTaskStop {
                task: captured
                    .lookup(&key)?
                    .context("Stop Task absent")?
                    .locator
                    .clone(),
                kind: TaskKind::CommandTask,
                created_at: at(),
                rejoin: None,
                owner_epoch: 1,
                signal: TaskStopSignal::Cancel,
                lease_id: Some("delta-lease".into()),
                child: None,
                child_unavailable: None,
                worktree: None,
                worktree_unavailable: None,
                result: None,
            };
            let prepared = PreparedInputMutation::new(
                InputMutationFamily::Task,
                "delta-stop".into(),
                vec![scope.clone()],
                canonical_command_digest(&intent)?,
                at(),
            )?;
            let stop_proof =
                prepare_task_stop_tx(tx, &prepared, &intent, &original, &load, &ledger)?;
            let mut expected = value.clone();
            expected.status = TaskStatus::Cancelling;
            expected.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
            repositories::upsert_task_tx(tx, &expected)?;
            let (record, inserted) =
                insert_task_stop_with_observation_ack_tx(&stop_proof, &expected)?;
            let facts = if use_delta {
                let mut material =
                    read_preparation::TaskReadMaterial::new(&scope, &load, Some(&ledger));
                original.register_incremental_tx(
                    tx,
                    baseline,
                    &keys,
                    &AgentState::new("task-agent"),
                    &load,
                    &ledger,
                    &mut material,
                    Some(&inserted),
                )?
            } else {
                register_task_writes_with_stop_observation_tx(
                    tx,
                    &original,
                    &keys,
                    &AgentState::new("task-agent"),
                    &load,
                    &ledger,
                    None,
                    &inserted,
                )?
            };
            let outcome = InputMutationOutcome {
                result: InputMutationResult::Task {
                    task_id: value.id.clone(),
                    deletion: None,
                    stop: Some(record.receipt_binding()),
                },
                applied: true,
                delivery_receipt: None,
                scopes: vec![InputMutationScopeOutcome {
                    scope: scope.clone(),
                    install_keys: Default::default(),
                    audit_events: vec![],
                    notify_memory_index: false,
                    notify_scheduler: false,
                }],
            };
            insert_input_mutation_receipt_tx(tx, &prepared, &outcome, at())?;
            if use_delta {
                assert!(!facts.delta.stop_records.is_empty());
                incremental = Some(facts);
                tx.execute_batch("ROLLBACK TO stop_delta_oracle")?;
            } else {
                assert_whole_ack(incremental.as_ref().unwrap(), &facts);
            }
        }
        tx.execute_batch("RELEASE stop_delta_oracle")?;
        Ok(())
    })
}
