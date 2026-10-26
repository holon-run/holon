//! Isolate the Task/Result bridge; this does not grant D3 Message/queue authority.
use super::*;

fn cleanup(f: &Fixture, id: &str) -> Result<(TaskTransitionCommand, PreparedInputTransition)> {
    let mut admitted = command(f, id, TaskStatus::Running);
    admitted.task.kind = TaskKind::AgentMessageWait;
    admitted.task.detail = Some(serde_json::json!({"rejoin_obligation_id":id,
        "rejoin_generation":1,"parent_turn_id":"original-admitted-turn"}));
    execute(
        f,
        &freeze(f, &format!("{id}:admitted"), admitted.clone())?,
        &load_request(),
        &ledger()?,
    )?;
    let mut final_command = admitted;
    final_command.task.status = TaskStatus::Interrupted;
    final_command.task.updated_at += chrono::Duration::milliseconds(1);
    let mut message = MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task { task_id: id.into() },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original cleanup result".into(),
        },
    );
    message.id = format!("{id}:result-message");
    message.task_id = Some(id.into());
    message.created_at = final_command.task.updated_at;
    final_command.task.parent_message_id = Some(message.id.clone());
    final_command.task_result_settlement = results::TaskResultSettlementRecord::pending(
        &final_command.task,
        &message,
        message.created_at,
    )?;
    final_command.message_evidence.push(message);
    let p = freeze(f, &format!("{id}:cleanup"), final_command.clone())?;
    let mut identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
    identity.status = crate::types::AgentRegistryStatus::Deleting;
    identity.updated_at += chrono::Duration::seconds(1);
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &identity))?;
    Ok((final_command, p))
}

#[test]
fn deleting_result_cap_joins_original_task_inner_and_once_finalizer_without_input_grant(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let (command, p) = cleanup(&f, "task:deleting-original")?;
    let result = command.task_result_settlement.as_ref().unwrap();
    let value=f.db.transaction(|tx| {
        let mut task=super::super::super::task_writes::prepare_task_transaction_tx(tx,&p,&f.scope,
            true,&load_request(),&ledger,None)?.context("missing original Task preparation")?;
        let pending=task.take_result();
        // Original offline/cfgtest adapter intentionally has no Input scope.
        // Production keeps its D3 proof gate; M2 alone cannot grant Message/Queue.
        let repository=RuntimeTransitionRepository {db:&f.db,input_scope:None,cleanup_input:None};
        let (mut value,_)=repository.commit_task_at_with_task_results_tx(tx,&command,
            &Default::default(),true,p.descriptor().prepared_at(),None,pending,task.cleanup(),None,None)?;
        task.finish(tx,&mut value,&ledger,None)?;
        let key=TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity);
        let delta=value.effects.task_delta.as_ref().unwrap();
        assert_eq!(delta.revision,delta.base_revision+1);
        assert!(delta.coverage.unsettled_result_ids.contains(&result.result_identity));
        assert!(value.effects.task_write_keys.contains(&key));
        assert!(value.effects.task_write_keys.contains(&TaskKey::new(TaskFamily::Task,&command.task.id)));
        assert!(value.effects.task_delta_requires_prepared_domain);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3",
            params![f.scope.agent_id,f.scope.identity_incarnation,result.message_id],|r|r.get::<_,usize>(0))?,0,
            "the Task/Result capability must not grant Input qualification");
        Ok(value)
    })?;
    assert!(ledger.report()?.observed_bytes > 0);
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_writer_complete_path_rejects_no_state_and_preserves_sparse_responsibility() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let (command, p) = cleanup(&f, "task:no-state-original")?;
    f.db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&f.scope.agent_id],
    )?;
    f.db.transaction(|tx| {
        let error = match super::super::super::task_writes::prepare_task_transaction_tx(
            tx,
            &p,
            &f.scope,
            true,
            &load_request(),
            &ledger,
            None,
        ) {
            Ok(_) => panic!("NoState cannot enter complete Task finalizer"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error.downcast_ref::<tasks::TaskSnapshotError>(),
                Some(tasks::TaskSnapshotError::RetiredScope)
            ),
            "{error:#}"
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_result_settlements WHERE task_id=?1",
                [&command.task.id],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM agent_states WHERE agent_id=?1",
                [&f.scope.agent_id],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
