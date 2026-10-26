//! Focused receipt recovery for the complete atomic Control family.

use std::sync::Arc;

use anyhow::{ensure, Result};

use super::{
    mutation_owner::{IdentityMutationFacts, KnownCommit, MutationReservation},
    mutation_witness::MutationWitnessFamily,
    state_mutation::{RuntimeMutationFacts, SavedControlCancellation},
    CurrentRunAbortHandle, RuntimeHandle,
};
use crate::runtime_db::{control_transition::ControlRecoveryObservation, RuntimeDb};

struct ControlResolution {
    known: Option<KnownCommit>,
    cancellation: Option<SavedControlCancellation>,
}

fn resolve_control_recovery_with_run(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    recent_episode_limit: usize,
    budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    original_run: Option<CurrentRunAbortHandle>,
    task_queries: Option<&super::task_queries::TaskRecoveryQueries>,
) -> Result<Option<ControlResolution>> {
    if reservations.len() != 1 {
        return Ok(None);
    }
    let Some(recovery) = reservations[0].recovery()? else {
        return Ok(None);
    };
    if recovery.known.is_some() {
        return Ok(None);
    }
    let Some(witness) = recovery.witness.as_ref() else {
        return Ok(None);
    };
    if !witness.original_attempt_finished() {
        return Ok(None);
    }
    let MutationWitnessFamily::Control {
        key,
        invocation,
        command_digest,
    } = witness.family()
    else {
        return Ok(None);
    };
    ensure!(
        witness.operation_id() == recovery.key.operation_id
            && witness.owner()
                == (
                    reservations[0].agent_id(),
                    reservations[0].identity_incarnation()
                )
            && key.agent_id == recovery.key.agent_id
            && key.identity_incarnation == recovery.key.identity_incarnation,
        "Control recovery witness differs from its original owner ticket"
    );
    let owner_epoch = witness
        .control_owner_epoch()
        .ok_or_else(|| anyhow::anyhow!("Control original owner epoch missing"))?;
    let cancellation = witness
        .control_cancellation()
        .map(|facts| SavedControlCancellation {
            key: key.clone(),
            digest: command_digest.clone(),
            owner_epoch,
            facts,
        });
    let limits = budget.limits();
    let input_request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: usize::try_from(limits.record_limit)?,
        byte_limit: usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .ok_or_else(|| anyhow::anyhow!("Control recovery byte limit overflow"))?,
        )?,
    };
    // Positive receipts prepare current State and all three domains in one view.
    let known = match db.load_control_recovery_with_queries(
        key,
        invocation,
        command_digest,
        recent_episode_limit,
        &input_request,
        &super::resident_work::WorkCaptureRequest::current(),
        budget,
        task_queries.and_then(|queries| {
            queries.selection_for_owner(
                &key.agent_id,
                key.identity_incarnation,
                reservations[0].owner_epoch(),
            )
        }),
    )? {
        ControlRecoveryObservation::Absent { obligations } => {
            validate_obligation_receipt(key, None, &obligations)?;
            None
        }
        ControlRecoveryObservation::Current {
            snapshot,
            episodes,
            domains,
            receipt,
            obligations,
            facts,
        } => {
            validate_obligation_receipt(key, receipt.as_ref(), &obligations)?;
            if receipt.is_none() {
                None
            } else {
                let state = Arc::new(snapshot.state);
                ensure!(
                    episodes.belongs_to(&key.agent_id, key.identity_incarnation),
                    "Control recovery Episodes differ from original scope"
                );
                let domains = domains.ok_or_else(|| {
                    anyhow::anyhow!("positive Control recovery omitted current domains")
                })?;
                let runtime = RuntimeMutationFacts::for_control_recovery(
                    owner_epoch,
                    state.clone(),
                    episodes.clone(),
                    domains.clone(),
                    budget,
                    *facts,
                    original_run,
                    cancellation.clone(),
                )?;
                Some(KnownCommit::State {
                    work: Some(domains.work),

                    tasks: Some(domains.tasks),
                    scheduling: Some(domains.scheduling),
                    state,
                    episodes: Some(episodes),
                    input_evidence: Some(domains.inputs),
                    runtime: Some(Arc::new(runtime)),
                })
            }
        }
        ControlRecoveryObservation::Retired {
            current_identity,
            receipt,
            obligations,
        } => {
            validate_obligation_receipt(key, receipt.as_ref(), &obligations)?;
            if receipt.is_some() {
                budget.install_actual_pending(
                    &super::resident_input_evidence::InputEvidenceScope {
                        agent_id: key.agent_id.clone(),
                        identity_incarnation: key.identity_incarnation,
                    },
                    &[],
                )?;
            }
            receipt.map(|_| {
                KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                    identity: current_identity.map(Arc::new),
                    state: None,
                }))
            })
        }
    };
    MutationReservation::resolve_control_unknown(
        reservations,
        &recovery.key,
        witness,
        known.clone(),
    )?;
    Ok(Some(ControlResolution {
        known,
        cancellation,
    }))
}

fn validate_obligation_receipt(
    key: &crate::runtime_db::control_transition::ControlKey,
    receipt: Option<&crate::runtime_db::control_transition::ControlReceipt>,
    obligations: &[crate::runtime_db::control_transition::ControlCancellationObligation],
) -> Result<()> {
    ensure!(
        obligations.iter().all(|obligation| &obligation.key == key),
        "Control recovery obligation scope differs"
    );
    let actual = obligations
        .iter()
        .map(|obligation| &obligation.target.obligation_id)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = receipt
        .into_iter()
        .flat_map(|receipt| receipt.obligation_ids.iter())
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        actual == expected,
        "Control recovery obligations differ from original receipt"
    );
    Ok(())
}

/// Construction has no running resources yet. Normal canonical bootstrap still
/// follows before any admission; pending physical obligations remain explicit.
pub(crate) fn resolve_unloaded_control_recovery(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    recent_episode_limit: usize,
    budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<bool> {
    Ok(resolve_control_recovery_with_run(
        db,
        reservations,
        recent_episode_limit,
        budget,
        None,
        None,
    )?
    .is_some())
}

pub(crate) async fn resolve_control_recovery(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    recent_episode_limit: usize,
    runtime: Option<&RuntimeHandle>,
    budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<bool> {
    if reservations.len() != 1 {
        return Ok(false);
    }
    if reservations[0]
        .recovery()?
        .is_none_or(|recovery| recovery.known.is_some())
    {
        return Ok(false);
    }
    let limits = budget.limits();
    let task_queries = match (runtime, reservations.first()) {
        (Some(runtime), Some(reservation)) => runtime.task_recovery_queries(
            reservation,
            usize::try_from(limits.record_limit)?,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .ok_or_else(|| anyhow::anyhow!("Control live query limit overflow"))?,
            )?,
        )?,
        _ => None,
    };
    let original_run = match runtime {
        Some(runtime) => runtime.inner.agent.lock().await.current_run_abort.clone(),
        None => None,
    };
    let Some(resolution) = resolve_control_recovery_with_run(
        db,
        reservations,
        recent_episode_limit,
        budget,
        original_run,
        task_queries.as_ref(),
    )?
    else {
        return Ok(false);
    };
    if resolution.known.is_none() {
        // Exact absence after the synchronous original attempt ended is the
        // only negative proof that releases this retained local preparation.
        if let (Some(runtime), Some(saved)) = (runtime, resolution.cancellation) {
            runtime.inner.control_cancellations.release_prepared(
                &saved.key,
                &saved.digest,
                saved.owner_epoch,
            )?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{
            mutation_witness::MutationWitness,
            tests::support::{context_config, tempdir, StubProvider},
        },
        runtime_db::{
            control_transition::ControlFaultPoint, persistence_certainty, PersistenceCertainty,
        },
        types::{
            AgentStatus, AuthorityClass, ControlAction, MessageBody, MessageEnvelope, MessageKind,
            MessageOrigin, Priority, QueueEntryStatus,
        },
    };

    fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
        let home = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "control-recovery",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "control-recovery".into(),
            context_config(),
        )
        .unwrap();
        (home, runtime)
    }
    async fn enqueue(runtime: &RuntimeHandle, text: &str) -> MessageEnvelope {
        runtime
            .enqueue(MessageEnvelope::new(
                "control-recovery",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Next,
                MessageBody::Text { text: text.into() },
            ))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn lost_ack_recovers_current_exact_queue_and_replay_does_not_stop_a_later_start() {
        let (_home, runtime) = fixture();
        let input = enqueue(&runtime, "before Stop").await;
        let role = runtime
            .register_task_query(super::super::task_queries::TaskQuerySpec {
                birth_after: Some(0),
                reply_message_ids: &["control-recovery-empty-reply"],
                ..Default::default()
            })
            .unwrap();
        let (key, invocation) =
            runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::OperatorInstruction);
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let mut prepared = runtime
            .prepare_runtime_control(&mutation, key.clone(), invocation.clone())
            .await
            .unwrap();
        prepared.command = prepared
            .command
            .with_fault(ControlFaultPoint::AfterCommitBeforeAck);
        let error = mutation.commit_control(&prepared).await.unwrap_err();
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::Indeterminate)
        );
        drop(mutation);
        assert!(runtime.inner.mutation_owner.capture().is_err());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&input.id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        let receipt = runtime
            .inner
            .runtime_db
            .control_receipt(&key, &invocation, None)
            .unwrap()
            .unwrap();
        let restored = runtime.begin_mutation().await.unwrap();
        assert_eq!(restored.state.status, AgentStatus::Stopped);
        assert!(restored.queue.is_empty());
        let tasks = restored
            .capture_tasks(&super::super::resident_task::TaskCaptureRequest::current())
            .unwrap();
        assert_eq!(tasks.coverage.query.birth_after, Some(0));
        assert_eq!(
            tasks
                .coverage
                .query
                .reply_task_ids
                .get("control-recovery-empty-reply"),
            Some(&Default::default())
        );
        assert_eq!(tasks.task_birth_cursor().unwrap(), 0);
        drop((tasks, restored, role));
        runtime.inner.control_cancellations.drain().await.unwrap();
        runtime.control(ControlAction::Start).await.unwrap();
        let later = enqueue(&runtime, "after Start").await;
        let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let admission = runtime.execution_admission_lease().unwrap();
        runtime
            .control_command_admitted(key.clone(), invocation.clone(), admission)
            .await
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .control_receipt(&key, &invocation, None)
                .unwrap()
                .unwrap(),
            receipt
        );
        let current = runtime.begin_mutation().await.unwrap();
        assert_ne!(current.state.status, AgentStatus::Stopped);
        assert!(current
            .queue
            .peek_next_matching(|message| message.id == later.id)
            .is_some());
        drop(current);
        assert_eq!(
            runtime
                .inner
                .mutation_owner
                .capture()
                .unwrap()
                .unwrap()
                .token,
            root.token
        );
        runtime.join_owned_mutations().await.unwrap();
        runtime.inner.control_cancellations.drain().await.unwrap();
    }

    #[tokio::test]
    async fn claim_known_publication_failure_retains_its_original_run_until_stop_joins() {
        let (_home, runtime) = fixture();
        let input = enqueue(&runtime, "claim publication failure").await;
        runtime.inject_next_transition_fault(
            crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
        );
        assert!(
            crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .is_err()
        );
        assert!(runtime.inner.mutation_owner.capture().is_err());
        let recovered = runtime.begin_mutation().await.unwrap();
        let run_id = recovered.state.current_run_id.clone().unwrap();
        let token = recovered.current_run_abort.as_ref().unwrap().token.clone();
        let lease = runtime
            .inner
            .control_cancellations
            .run_for(&run_id)
            .unwrap();
        // The claim's caller ended before any processing future started.
        assert!(lease.is_complete());
        drop(recovered);
        runtime.control(ControlAction::Stop).await.unwrap();
        runtime.inner.control_cancellations.drain().await.unwrap();
        assert!(token.is_cancelled());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&input.id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        assert_eq!(
            runtime.agent_state().await.unwrap().status,
            AgentStatus::Stopped
        );
        runtime.join_owned_mutations().await.unwrap();
    }

    #[tokio::test]
    async fn healthy_start_during_claim_before_turn_keeps_the_original_processing_lease() {
        let (_home, runtime) = fixture();
        enqueue(&runtime, "claim before begin Turn").await;
        let crate::runtime::scheduler_executor::RunLoopPoll::Message(scheduled) =
            crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .unwrap()
        else {
            panic!("expected a canonical claim");
        };
        let before = runtime.agent_state().await.unwrap();
        let run_id = before.current_run_id.as_ref().unwrap();
        let lease = runtime.inner.control_cancellations.run_for(run_id).unwrap();
        assert!(!lease.is_complete());
        runtime.control(ControlAction::Start).await.unwrap();
        let after_start = runtime.agent_state().await.unwrap();
        assert_eq!(after_start.current_run_id, before.current_run_id);
        assert_eq!(after_start.status, AgentStatus::AwakeRunning);
        assert!(!lease.is_complete());
        runtime.control(ControlAction::Stop).await.unwrap();
        assert!(
            !lease.is_complete(),
            "canonical Stop is not physical completion"
        );
        drop(scheduled);
        runtime.inner.control_cancellations.drain().await.unwrap();
        assert!(lease.is_complete());
        runtime.join_owned_mutations().await.unwrap();
    }

    #[tokio::test]
    async fn definite_rollback_keeps_publication_and_accepted_payload_unchanged() {
        let (_home, runtime) = fixture();
        let input = enqueue(&runtime, "rollback must retain this body").await;
        let before = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let (key, invocation) =
            runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::OperatorInstruction);
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let mut prepared = runtime
            .prepare_runtime_control(&mutation, key.clone(), invocation.clone())
            .await
            .unwrap();
        prepared.command = prepared
            .command
            .with_fault(ControlFaultPoint::AfterStateWrite);
        let error = mutation.commit_control(&prepared).await.unwrap_err();
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::DefinitelyNotCommitted)
        );
        drop(mutation);
        let after = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        assert_eq!(after.token, before.token);
        assert_eq!(after.state, before.state);
        assert!(runtime
            .inner
            .runtime_db
            .control_receipt(&key, &invocation, None)
            .unwrap()
            .is_none());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&input.id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        assert_eq!(
            runtime
                .storage()
                .read_message_by_id(&input.id)
                .unwrap()
                .unwrap()
                .body,
            input.body
        );
        runtime.inner.control_cancellations.drain().await.unwrap();
    }

    #[tokio::test]
    async fn unfinished_control_attempt_stays_fenced_and_exact_absence_preserves_the_old_root() {
        let (_home, runtime) = fixture();
        let before = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let (key, invocation) =
            runtime.freeze_direct_control(ControlAction::Start, AuthorityClass::RuntimeInstruction);
        let mutation = runtime.begin_mutation().await.unwrap();
        let prepared = runtime
            .prepare_runtime_control(&mutation, key.clone(), invocation)
            .await
            .unwrap();
        drop(mutation);
        let witness = MutationWitness::control(
            "unexecuted-control".into(),
            &prepared.command,
            prepared.cancellation.clone(),
            runtime.inner.mutation_owner.owner_epoch(),
        )
        .unwrap();
        let mut reservations =
            crate::runtime::mutation_owner::MutationOwner::reserve_all(vec![runtime
                .inner
                .mutation_owner
                .clone()])
            .await
            .unwrap();
        MutationReservation::begin_all_with_witness(
            &mut reservations,
            "unexecuted-control".into(),
            witness.clone(),
        )
        .unwrap();
        assert!(!resolve_control_recovery(
            &runtime.inner.runtime_db,
            &mut reservations,
            12,
            Some(&runtime),
            &runtime.inner.input_evidence_budget
        )
        .await
        .unwrap());
        assert!(!crate::runtime::episode_recovery::resolve_episode_recovery(
            &runtime.inner.runtime_db,
            &mut reservations,
            12
        )
        .unwrap());
        assert!(runtime.inner.mutation_owner.capture().is_err());
        witness.finish_original_attempt();
        assert!(resolve_control_recovery(
            &runtime.inner.runtime_db,
            &mut reservations,
            12,
            Some(&runtime),
            &runtime.inner.input_evidence_budget
        )
        .await
        .unwrap());
        assert!(reservations[0].recovery().unwrap().is_none());
        let after = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        assert_eq!(after.token, before.token);
        assert!(Arc::ptr_eq(&after, &before));
    }
}
