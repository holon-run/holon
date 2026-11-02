//! Physical completion, durable memory and original Control responsibility.
use super::*;
use crate::{
    provider::StubProvider,
    runtime::{
        resident_input_evidence::InputEvidenceScope,
        scheduler_executor::{RunLoopPoll, ScheduledMessage, SchedulerDecisionExecutor},
        tests::support::context_config,
    },
    runtime_db::{
        control_transition::{ControlCancellationKind, ControlObligationState, ControlReceipt},
        dispatch_memory::{load_dispatch_memory_tx, DispatchMemoryRecord},
        evidence::upsert_agent_identity_tx,
        transitions::TransitionFaultPoint,
    },
    types::{
        AgentRegistryStatus, AgentStatus, AuthorityClass, ControlAction, MessageBody, MessageKind,
        MessageOrigin, Priority,
    },
};
use std::{future::Future, pin::Pin, time::Duration};

struct Fixture {
    runtime: RuntimeHandle,
    scheduled: Option<ScheduledMessage>,
    original: Arc<ControlRunLease>,
    key: EpisodeMemoryKey,
    invocation: EpisodeMemoryInvocation,
    _home: tempfile::TempDir,
    _workspace: tempfile::TempDir,
}
impl Fixture {
    fn started() -> Pin<Box<impl Future<Output = Result<Self>> + Send>> {
        Box::pin(async {
            let home = tempfile::tempdir()?;
            let workspace = tempfile::tempdir()?;
            let runtime = RuntimeHandle::new(
                "default",
                home.path().to_owned(),
                workspace.path().to_owned(),
                String::new(),
                Arc::new(StubProvider::new("unused")),
                "default".into(),
                context_config(),
            )?;
            runtime
                .enqueue(MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "original dispatch with detached Work".into(),
                    },
                ))
                .await?;
            let RunLoopPoll::Message(scheduled) =
                SchedulerDecisionExecutor::new(&runtime).poll().await?
            else {
                anyhow::bail!("actual source was not claimed");
            };
            runtime
                .begin_dispatch_turn(
                    &scheduled.message,
                    None,
                    None,
                    scheduled
                        .dispatch_plan
                        .execution_admission_provenance
                        .clone(),
                    &scheduled.dispatch_plan.prior_closure,
                )
                .await?;
            let state = runtime.agent_state().await?;
            let key = EpisodeMemoryKey {
                agent_id: "default".into(),
                identity_incarnation: runtime.inner.identity_incarnation,
                actual_dispatch_turn_id: state.current_turn_id.context("actual Turn missing")?,
            };
            let invocation = EpisodeMemoryInvocation {
                message_id: scheduled.message.id.clone(),
                turn_index: state.turn_index,
            };
            let original = scheduled.run_completion.lease();
            runtime
                .create_work_item("detached original Work".into(), None, None, Vec::new())
                .await?;
            let fixture = Self {
                runtime,
                scheduled: Some(scheduled),
                original,
                key,
                invocation,
                _home: home,
                _workspace: workspace,
            };
            assert!(matches!(
                fixture.memory()?,
                DispatchMemoryRecord::Pending { .. }
            ));
            Ok(fixture)
        })
    }
    fn memory(&self) -> Result<DispatchMemoryRecord> {
        self.runtime.runtime_db().transaction(|tx| {
            load_dispatch_memory_tx(tx, &self.key, &self.invocation)?
                .context("original dispatch checkpoint missing")
        })
    }
    fn checkpoint_image(&self) -> Result<String> {
        self.runtime.runtime_db().transaction(|tx| {
            tx.query_row(
                "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
                rusqlite::params![self.key.agent_id,self.key.identity_incarnation,self.key.actual_dispatch_turn_id],
                |row|row.get(0),
            ).map_err(Into::into)
        })
    }
    fn d1_count(&self) -> Result<u64> {
        self.runtime.runtime_db().transaction(|tx| {
            tx.query_row(
                "SELECT COUNT(*) FROM episode_memory_results WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
                rusqlite::params![self.key.agent_id,self.key.identity_incarnation,self.key.actual_dispatch_turn_id],
                |row|row.get(0),
            ).map_err(Into::into)
        })
    }
    fn stop(&self) -> Pin<Box<impl Future<Output = Result<ControlReceipt>> + Send + '_>> {
        Box::pin(async {
            let (key, invocation) = self
                .runtime
                .freeze_direct_control(ControlAction::Stop, AuthorityClass::OperatorInstruction);
            let mut mutation = self.runtime.begin_mutation().await?;
            let prepared = self
                .runtime
                .prepare_runtime_control(&mutation, key.clone(), invocation.clone())
                .await?;
            mutation.commit_control(&prepared).await?;
            drop(mutation);
            self.runtime
                .runtime_db()
                .control_receipt(&key, &invocation, None)?
                .context("original Stop receipt missing")
        })
    }
    fn run_obligation(&self, receipt: &ControlReceipt) -> Result<ControlObligationState> {
        let target = self
            .runtime
            .runtime_db()
            .pending_control_obligations(&self.key.agent_id, self.key.identity_incarnation)?
            .into_iter()
            .find(|(key, target)| {
                key == &receipt.key && target.target_kind == ControlCancellationKind::Run
            })
            .context("original physical Run obligation missing")?
            .1;
        Ok(self
            .runtime
            .runtime_db()
            .control_obligation(&receipt.key, &target)?
            .context("original obligation missing")?
            .state)
    }
    async fn drain(&self) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.runtime.inner.control_cancellations.drain(),
        )
        .await
        .context("original completion worker timed out")?
    }
}

#[tokio::test]
async fn stopped_dispatch_memory_waits_for_original_guard_then_finishes_once() -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::started().await?;
        let pending = f.memory()?;
        let receipt = f.stop().await?;
        let stopped = f
            .runtime
            .runtime_db()
            .agent_states()
            .latest("default")?
            .unwrap();
        assert_eq!(stopped.status, AgentStatus::Stopped);
        assert!(!f.original.is_complete());
        assert!(f.original.was_stopped());
        assert_eq!(f.run_obligation(&receipt)?, ControlObligationState::Pending);
        assert!(f
            .runtime
            .finish_original_stopped_dispatch(f.original.clone())
            .await
            .is_err());
        assert_eq!(f.memory()?, pending);
        assert_eq!(f.d1_count()?, 0);
        drop(f.scheduled.take());
        f.drain().await?;
        assert!(f.original.is_complete());
        assert!(matches!(f.memory()?, DispatchMemoryRecord::Done { .. }));
        assert_eq!(f.d1_count()?, 1);
        assert!(f
            .runtime
            .runtime_db()
            .pending_control_obligations("default", f.key.identity_incarnation)?
            .is_empty());
        let final_state = f.runtime.agent_state().await?;
        assert_eq!(final_state.status, AgentStatus::Stopped);
        assert!(final_state.current_run_id.is_none());
        let done = f.checkpoint_image()?;
        let turns = f.runtime.storage().read_recent_turns(16)?;
        f.runtime
            .finish_original_stopped_dispatch(f.original.clone())
            .await?;
        assert_eq!(f.checkpoint_image()?, done);
        assert_eq!(f.d1_count()?, 1);
        assert_eq!(f.runtime.agent_state().await?, final_state);
        assert_eq!(f.runtime.storage().read_recent_turns(16)?, turns);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn stopped_dispatch_known_memory_ack_recovers_before_original_obligation_completion(
) -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::started().await?;
        let receipt = f.stop().await?;
        // The held guard keeps the only completion worker behind physical join.
        f.runtime
            .inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
        drop(f.scheduled.take());
        let error = f.drain().await.unwrap_err();
        assert!(
            format!("{error:#}").contains("publication requires reconciliation"),
            "{error:#}"
        );
        assert!(f.original.is_complete());
        assert!(f.runtime.inner.mutation_owner.capture().is_err());
        assert!(matches!(f.memory()?, DispatchMemoryRecord::Done { .. }));
        assert_eq!(f.d1_count()?, 1);
        assert_eq!(f.run_obligation(&receipt)?, ControlObligationState::Pending);
        let done = f.checkpoint_image()?;
        let durable = f
            .runtime
            .runtime_db()
            .agent_states()
            .latest("default")?
            .unwrap();
        f.runtime
            .resume_original_control_cancellation(&receipt)
            .await?;
        f.drain().await?;
        assert_eq!(f.checkpoint_image()?, done);
        assert_eq!(f.d1_count()?, 1);
        assert_eq!(f.runtime.agent_state().await?, durable);
        assert!(f.runtime.inner.mutation_owner.capture().is_ok());
        assert!(f
            .runtime
            .runtime_db()
            .pending_control_obligations("default", f.key.identity_incarnation)?
            .is_empty());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn retired_original_dispatch_hands_off_memory_before_releasing_original_run() -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::started().await?;
        let receipt = f.stop().await?;
        let checkpoint = f.checkpoint_image()?;
        let state = f
            .runtime
            .runtime_db()
            .agent_states()
            .latest("default")?
            .unwrap();
        // Fixture-only tombstone: no real Agent or directory is removed.
        let mut identity = f
            .runtime
            .runtime_db()
            .agent_identities()
            .latest("default")?
            .unwrap();
        identity.status = AgentRegistryStatus::Deleted;
        f.runtime
            .runtime_db()
            .transaction(|tx| upsert_agent_identity_tx(tx, &identity))?;
        assert_eq!(f.run_obligation(&receipt)?, ControlObligationState::Pending);
        drop(f.scheduled.take());
        f.drain().await?;
        assert!(f.original.is_complete());
        assert_eq!(f.checkpoint_image()?, checkpoint);
        assert_eq!(f.d1_count()?, 0);
        assert_eq!(
            f.runtime.runtime_db().agent_states().latest("default")?,
            Some(state.clone())
        );
        let scope = InputEvidenceScope {
            agent_id: f.key.agent_id.clone(),
            identity_incarnation: f.key.identity_incarnation,
        };
        assert!(f.runtime.runtime_db().transaction(|tx| {
            crate::runtime_db::dispatch_memory::retired_physical_handoff_exists_tx(
                tx,
                &scope,
                f.original.run_id(),
                f.original.lease_id(),
                f.runtime.inner.mutation_owner.owner_epoch(),
                &f.invocation.message_id,
            )
        })?);
        assert!(f
            .runtime
            .runtime_db()
            .pending_control_obligations("default", f.key.identity_incarnation)?
            .is_empty());
        f.runtime
            .finish_original_stopped_dispatch(f.original.clone())
            .await?;
        assert_eq!(f.checkpoint_image()?, checkpoint);
        assert_eq!(f.d1_count()?, 0);
        Ok(())
    })
    .await
}
