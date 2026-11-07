use super::*;
use crate::runtime::ended_dispatch::test_physical_stopped_authorization;
use crate::runtime_db::{
    control_transition::{
        prepare_control, ControlCancellationKind, ControlCancellationTarget, ControlCommit,
        ControlInvocation, ControlKey, ControlPlanInput, ControlReceipt, ControlSource,
    },
    dispatch_memory::retired::validated_retired_handoff_in,
    repositories::upsert_queue_entry_tx,
};
use crate::types::ControlAction;
use serde_json::json;

struct StoppedFixture {
    f: Fixture,
    receipt: ControlReceipt,
    target: ControlCancellationTarget,
}
impl StoppedFixture {
    fn new() -> Result<Self> {
        let f = Fixture::with_trigger(true, true)?;
        f.update("retired-memory-member", 1, false)?;
        f.db.transaction(|tx| {
            upsert_queue_entry_tx(
                tx,
                &QueueEntryRecord {
                    message_id: f.source.id.clone(),
                    agent_id: f.scope.agent_id.clone(),
                    priority: f.source.priority.clone(),
                    status: QueueEntryStatus::Dequeued,
                    created_at: f.source.created_at,
                    updated_at: f.at,
                },
            )?;
            Ok(())
        })?;
        let key = ControlKey {
            agent_id: f.scope.agent_id.clone(),
            identity_incarnation: f.scope.identity_incarnation,
            command_id: "stop-original-memory-run".into(),
        };
        let target = ControlCancellationTarget {
            obligation_id: "original-memory-run-obligation".into(),
            target_kind: ControlCancellationKind::Run,
            target_identity: "run:dispatch-memory".into(),
            target_generation: json!({"owner_epoch":1,"runtime_lease_id":"original-memory-run-lease","activation_id":null,"source_message_id":f.source.id}),
            target: json!({"reason":"agent_stopped"}),
        };
        let prepared = prepare_control(ControlPlanInput {
            snapshot: f.db.load_control_snapshot(&key)?,
            key,
            invocation: ControlInvocation {
                action: ControlAction::Stop,
                source: ControlSource::DirectApi,
                authority_class: AuthorityClass::OperatorInstruction,
            },
            prepared_at: f.at + chrono::Duration::seconds(10),
            task_results: Vec::new(),
            healthy_running: None,
            protected_control_attempt: None,
            protected_control_run_id: None,
            cancellation_targets: vec![target.clone()],
            audit_events: Vec::new(),
            index_changes: Vec::new(),
        })?;
        let ControlCommit::Applied { receipt, .. } = f.db.commit_control(&prepared)? else {
            anyhow::bail!("Stop was not applied")
        };
        assert_eq!(
            receipt.outcome.interrupted_turn_ids,
            vec![f.start.key().actual_dispatch_turn_id.clone()]
        );
        Ok(Self { f, receipt, target })
    }
    fn authorization(
        &self,
        completed: bool,
    ) -> crate::runtime::ended_dispatch::EndedDispatchAuthorization {
        test_physical_stopped_authorization(
            self.receipt.key.clone(),
            self.receipt.command_digest.clone(),
            1,
            self.f.source.clone(),
            "run:dispatch-memory",
            "original-memory-run-lease",
            None,
            completed,
        )
    }
    fn identity(&self, status: AgentRegistryStatus, incarnation: u64) -> Result<()> {
        let mut identity = self
            .f
            .db
            .agent_identities()
            .latest(&self.f.scope.agent_id)?
            .unwrap();
        let previous = identity.incarnation;
        identity.status = status;
        identity.incarnation = incarnation;
        self.f.db.transaction(|tx| {
            if incarnation > previous {
                crate::runtime_db::evidence::release_agent_identity_reservation_tx(
                    tx,
                    &identity.agent_id,
                )?;
            }
            upsert_agent_identity_tx(tx, &identity)
        })?;
        Ok(())
    }
    fn handoff(&self, at: DateTime<Utc>) -> Result<RetiredDispatchMemoryHandoff> {
        self.f
            .db
            .transaction(|tx| {
                prepare_retired_physical_handoff_tx(
                    tx,
                    &self.f.scope,
                    &self.f.start.key().actual_dispatch_turn_id,
                    &self.authorization(true),
                    at,
                )
            })?
            .context("handoff absent")
    }
    fn checkpoint(&self) -> Result<String> {
        self.f
            .db
            .connection()?
            .query_row(
                "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1",
                [&self.f.scope.agent_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }
    fn count(&self) -> Result<u64> {
        self.f
            .db
            .connection()?
            .query_row(
                "SELECT COUNT(*) FROM dispatch_memory_retired_handoffs",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }
}

#[test]
fn retired_physical_handoff_preserves_unfinished_checkpoint_and_original_obligation_cas(
) -> Result<()> {
    let stopped = StoppedFixture::new()?;
    let f = &stopped.f;
    let before = stopped.checkpoint()?;
    assert!(!f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .is_empty());
    stopped.identity(AgentRegistryStatus::Deleted, 1)?;
    let first = stopped.handoff(f.at + chrono::Duration::seconds(20))?;
    assert_eq!(
        first,
        stopped.handoff(f.at + chrono::Duration::seconds(30))?
    );
    assert_eq!(stopped.count()?, 1);
    assert_eq!(stopped.checkpoint()?, before);
    assert!(matches!(
        f.read()?,
        Some(DispatchMemoryRecord::Pending { .. })
    ));
    assert!(f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .is_empty());
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_none());
    let original =
        f.db.control_obligation(&stopped.receipt.key, &stopped.target)?
            .unwrap();
    assert_eq!(
        original.state,
        crate::runtime_db::control_transition::ControlObligationState::Pending
    );
    assert!(f.db.complete_control_obligation(
        &stopped.receipt.key,
        &stopped.target,
        f.at + chrono::Duration::seconds(30),
        json!({"physical_joined":true,"memory_disposition":"retired_handoff"})
    )?);
    assert_eq!(stopped.checkpoint()?, before);
    Ok(())
}

#[test]
fn retired_handoff_requires_original_completed_arc_and_never_reads_successor_state() -> Result<()> {
    let stopped = StoppedFixture::new()?;
    let f = &stopped.f;
    assert!(stopped.handoff(f.at).is_err());
    stopped.identity(AgentRegistryStatus::Deleting, 1)?;
    assert!(stopped.handoff(f.at).is_err());
    stopped.identity(AgentRegistryStatus::Deleted, 1)?;
    assert!(f
        .db
        .transaction(|tx| prepare_retired_physical_handoff_tx(
            tx,
            &f.scope,
            &f.start.key().actual_dispatch_turn_id,
            &stopped.authorization(false),
            f.at
        ))
        .is_err());
    let wrong = test_physical_stopped_authorization(
        stopped.receipt.key.clone(),
        stopped.receipt.command_digest.clone(),
        1,
        f.source.clone(),
        "run:dispatch-memory",
        "different-successor-lease",
        None,
        true,
    );
    assert!(f
        .db
        .transaction(|tx| prepare_retired_physical_handoff_tx(
            tx,
            &f.scope,
            &f.start.key().actual_dispatch_turn_id,
            &wrong,
            f.at
        ))
        .is_err());
    assert_eq!(stopped.count()?, 0);
    stopped.identity(AgentRegistryStatus::Active, 2)?;
    let successor = AgentState::new(&f.scope.agent_id);
    f.db.transaction(|tx| {
        upsert_agent_state_tx(tx, &successor)?;
        Ok(())
    })?;
    let before: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&f.scope.agent_id],
        |r| r.get(0),
    )?;
    f.db.connection()?
        .execute_batch("ALTER TABLE agent_states RENAME TO unavailable_successor_state")?;
    stopped.handoff(f.at + chrono::Duration::seconds(20))?;
    f.db.connection()?
        .execute_batch("ALTER TABLE unavailable_successor_state RENAME TO agent_states")?;
    let after: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&f.scope.agent_id],
        |r| r.get(0),
    )?;
    assert_eq!(before, after);
    Ok(())
}

#[test]
fn retired_handoff_rolls_back_and_retention_validates_small_original_addresses_after_body_pruning(
) -> Result<()> {
    let stopped = StoppedFixture::new()?;
    let f = &stopped.f;
    stopped.identity(AgentRegistryStatus::Deleted, 1)?;
    let checkpoint = stopped.checkpoint()?;
    f.db.connection()?.execute_batch("CREATE TRIGGER retired_memory_fault BEFORE INSERT ON dispatch_memory_retired_handoffs BEGIN SELECT RAISE(ABORT,'retired memory fault');END;")?;
    assert!(stopped.handoff(f.at).is_err());
    assert_eq!(stopped.count()?, 0);
    assert_eq!(checkpoint, stopped.checkpoint()?);
    assert!(!f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .is_empty());
    f.db.connection()?.execute_batch("DROP TRIGGER retired_memory_fault;CREATE TRIGGER retired_memory_bad_ack AFTER INSERT ON dispatch_memory_retired_handoffs BEGIN UPDATE dispatch_memory_retired_handoffs SET control_digest='0000000000000000000000000000000000000000000000000000000000000000';END;")?;
    assert!(stopped.handoff(f.at).is_err());
    assert_eq!(stopped.count()?, 0);
    assert_eq!(checkpoint, stopped.checkpoint()?);
    f.db.connection()?
        .execute_batch("DROP TRIGGER retired_memory_bad_ack")?;
    stopped.handoff(f.at)?;
    f.db.connection()?.execute_batch("ALTER TABLE messages RENAME TO pruned_original_messages;ALTER TABLE turn_records RENAME TO pruned_original_turns")?;
    assert!(f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .is_empty());
    assert!(f.db.transaction(|tx| retired_physical_handoff_exists_tx(
        tx,
        &f.scope,
        "run:dispatch-memory",
        "original-memory-run-lease",
        1,
        &f.source.id
    ))?);
    assert!(!f.db.transaction(|tx| retired_physical_handoff_exists_tx(
        tx,
        &f.scope,
        "run:dispatch-memory",
        "different-lease",
        1,
        &f.source.id
    ))?);
    assert!(f
        .db
        .transaction(|tx| retired_physical_handoff_exists_tx(
            tx,
            &f.scope,
            "run:dispatch-memory",
            "original-memory-run-lease",
            2,
            &f.source.id
        ))
        .is_err());
    f.db.transaction(|tx| -> Result<()> {
        assert!(validated_retired_handoff_in(
            tx,
            f.start.key(),
            f.start.invocation()
        )?);
        tx.execute(
            "UPDATE dispatch_memory_retired_handoffs SET control_digest=?1",
            ["0".repeat(64)],
        )?;
        assert!(dispatch_memory_retention_roots_in(tx).is_err());
        anyhow::bail!("rollback deliberate corrupted handoff")
    })
    .unwrap_err();
    assert!(f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .is_empty());
    f.db.transaction(|tx| -> Result<()> {
        tx.execute(
            "DELETE FROM input_mutation_receipts WHERE command_nonce=?1",
            [f.original_start.command_nonce()],
        )?;
        assert!(dispatch_memory_retention_roots_in(tx).is_err());
        anyhow::bail!("rollback deliberate missing Start receipt")
    })
    .unwrap_err();
    assert!(f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .is_empty());
    assert_eq!(checkpoint, stopped.checkpoint()?);
    Ok(())
}
