use super::*;
use crate::{
    runtime::{
        mutation_owner::{MutationOwner, MutationOwnerError},
        mutation_witness::MutationWitness,
        resident_input_evidence::InputEvidenceScope,
        tests::support::{context_config, CountingProvider},
        RuntimeHandle,
    },
    runtime_db::{
        input_mutation_receipts::OriginalInputMutationAttempt,
        transitions::{
            AgentStateMutation, InputMutationExecution, PreparedInputTransition,
            PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
            WaitTransitionCommand, WorkItemMutation,
        },
    },
    types::{
        AuditEvent, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, QueueEntryRecord, QueueEntryStatus, WorkItemRecord, WorkItemState,
    },
};

#[tokio::test]
async fn positive_input_receipt_installs_current_touched_work_cache_without_replaying_sql(
) -> Result<()> {
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_owned(),
        workspace.path().to_owned(),
        "http://127.0.0.1:7878".into(),
        Arc::new(CountingProvider {
            calls: tokio::sync::Mutex::new(0),
            reply: "unused",
        }),
        "default".into(),
        context_config(),
    )?;
    let before = runtime.agent_state().await?;
    let scope = InputEvidenceScope {
        agent_id: before.id.clone(),
        identity_incarnation: runtime.instance_incarnation(),
    };
    let mut old = WorkItemRecord::new(
        &before.id,
        "cached before original transaction",
        WorkItemState::Open,
    );
    old.id = "work-input-recovery-touched".into();
    runtime.inner.runtime_db.work_items().insert_new(&old)?;
    let mut unrelated =
        WorkItemRecord::new(&before.id, "unrelated cached item", WorkItemState::Open);
    unrelated.id = "work-input-recovery-unrelated".into();
    runtime
        .inner
        .runtime_db
        .work_items()
        .insert_new(&unrelated)?;
    {
        let mut cache = runtime.inner.projection_cache.lock().await;
        cache.upsert_work_item(old.clone());
        cache.upsert_work_item(unrelated.clone());
    }
    let mut current = old.clone();
    current.revision += 1;
    current.objective = "actual committed work objective".into();
    let message = MessageEnvelope::new(
        &before.id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "actual receipt recovery input".into(),
        },
    );
    let mut next = before.clone();
    next.pending += 1;
    let event = AuditEvent::legacy(
        "input_cache_recovery_once",
        serde_json::json!({"agent_id":before.id}),
    );
    let mut queue = PreparedQueueTransition::new(QueueTransitionCommand {
        agent_id: before.id.clone(),
        operation: QueueOperation::Admit,
        mutation: QueueMutation::Upsert(QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: before.id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: message.created_at,
        }),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(before.clone())),
            record: Box::new(next),
        }),
        message_evidence: vec![message],
        transcript_entries: Vec::new(),
        turn_record: None,
        audit_events: vec![event.clone()],
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    });
    queue.wait_registration = Some(WaitTransitionCommand {
        agent_id: before.id.clone(),
        work_items: vec![WorkItemMutation::Update {
            record: current.clone(),
            expected_revision: old.revision,
        }],
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        timer_wake: None,
        task_result_admission: None,
        agent_state: None,
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: false,
        fault: None,
    });
    let prepared = PreparedInputTransition::queue(
        scope,
        "input-cache-original".into(),
        chrono::Utc::now(),
        queue,
    )?;
    let witness = MutationWitness::input_mutation(
        "input-cache-original".into(),
        OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
        Vec::new(),
    )?;
    {
        let mut permits =
            MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
        MutationReservation::begin_all_with_witness(
            &mut permits,
            "input-cache-original".into(),
            witness.clone(),
        )?;
        let InputMutationExecution::Committed { receipt, .. } = runtime
            .inner
            .runtime_db
            .transitions_in_scope(&prepared.descriptor().scopes()[0])
            .commit_prepared_transition(&prepared)?
        else {
            anyhow::bail!("original fixture SQL must execute once");
        };
        assert_eq!(
            receipt.outcome.scopes[0].install_keys.work_items,
            std::collections::BTreeSet::from([old.id.clone()])
        );
        // Lose the returned ACK before core records Known, after real SQL.
        witness.finish_original_attempt();
    }
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_eq!(
        runtime
            .inner
            .projection_cache
            .lock()
            .await
            .work_items
            .get(&old.id),
        Some(&old)
    );
    runtime.reconcile_posture_observation().await?;
    {
        let cache = runtime.inner.projection_cache.lock().await;
        assert_eq!(cache.work_items.get(&old.id), Some(&current));
        assert_eq!(cache.work_items.get(&unrelated.id), Some(&unrelated));
    }
    assert_eq!(
        runtime.agent_state().await?,
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest(&before.id)?
            .unwrap()
    );
    assert_eq!(
        runtime
            .storage()
            .read_recent_events(100)?
            .iter()
            .filter(|event| event.kind == "input_cache_recovery_once")
            .count(),
        1
    );
    assert_eq!(runtime.inner.runtime_db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='input-cache-original'", [], |row| row.get::<_, i64>(0))?, 1);
    Ok(())
}
