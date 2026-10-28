//! Cold original deletion metadata ACKs; Scheduling remains explicitly unready.
use super::*;
use crate::{
    runtime::input_evidence_budget::{serialized_bytes, InputMemoryCategory, OwnedBytesEstimate},
    runtime_db::scheduling_snapshot::{
        DeletionSchedulingAck, DeletionSchedulingSelection, SchedulingSnapshotError,
    },
};
enum Operation {
    Timer(String),
    ParentTimers,
    Trigger(String),
}
impl RuntimeHost {
    pub(super) async fn cancel_deletion_timers(
        &self,
        agent: &str,
        job: &AgentDeletionJob,
        now: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        if job.mode == AgentDeletionMode::ParentCleanup {
            return self
                .commit_deletion_scheduling_operation(agent, job, Operation::ParentTimers, now)
                .await;
        }
        self.maintain_deletion_scheduling_rows(agent, job, DeletionSchedulingSelection::Timers, now)
            .await
    }
    pub(super) async fn revoke_deletion_triggers(
        &self,
        agent: &str,
        job: &AgentDeletionJob,
        now: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        self.maintain_deletion_scheduling_rows(
            agent,
            job,
            DeletionSchedulingSelection::ExternalTriggers,
            now,
        )
        .await
    }
    async fn maintain_deletion_scheduling_rows(
        &self,
        agent: &str,
        job: &AgentDeletionJob,
        selection: DeletionSchedulingSelection,
        now: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        ensure_unloaded(self, agent).await?;
        let ledger = self.bridge().input_evidence_budget()?;
        // This existing bounded identity/State header is metadata discovery,
        // not Task membership, business authority or an invented incarnation.
        let (scope, _scope_guard) = self
            .runtime_db()
            .deletion_task_scope_with_budget(agent, &ledger)?;
        let mut count = 0usize;
        loop {
            let batch = self
                .runtime_db()
                .deletion_scheduling_batch(&scope, job, selection, &ledger)?;
            if batch.ids.is_empty() {
                break;
            }
            for id in &batch.ids {
                // Register before the new owned ID clone across the actor await.
                let id_guard = ledger.register_owned_bytes(
                    &scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: u64::try_from(
                            id.len()
                                .checked_add(256)
                                .and_then(|n| n.checked_mul(8))
                                .context("deletion Scheduling ID bytes overflow")?,
                        )?,
                    },
                )?;
                let operation = match selection {
                    DeletionSchedulingSelection::Timers => Operation::Timer(id.clone()),
                    DeletionSchedulingSelection::ExternalTriggers => Operation::Trigger(id.clone()),
                };
                count = count
                    .checked_add(
                        self.commit_deletion_scheduling_operation(agent, job, operation, now)
                            .await?,
                    )
                    .context("deletion Scheduling count overflow")?;
                drop(id_guard);
            }
        }
        Ok(count)
    }
    async fn commit_deletion_scheduling_operation(
        &self,
        agent: &str,
        job: &AgentDeletionJob,
        operation: Operation,
        now: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        ensure_unloaded(self, agent).await?;
        let ledger = self.bridge().input_evidence_budget()?;
        let (scope, _scope_guard) = self
            .runtime_db()
            .deletion_task_scope_with_budget(agent, &ledger)?;
        let bytes = serialized_bytes(&(job, &scope))?
            .checked_add(512)
            .and_then(|n| n.checked_mul(8))
            .context("deletion Scheduling frozen material overflow")?;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("deletion Scheduling actor limit overflow")?
                    .min(64 * 1024 * 1024)
                && 16 <= limits.record_limit,
            SchedulingSnapshotError::ResourceExhausted("deletion Scheduling frozen metadata")
        );
        let guard = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let original_job = job.clone();
        let original_scope = scope.clone();
        let budget = ledger.clone();
        let ack: DeletionSchedulingAck = self
            .commit_deletion_identity_metadata_checked(
                agent.into(),
                Some(scope.identity_incarnation),
                vec![],
                |reservations| {
                    for reservation in reservations {
                        if let Some(root) = reservation.capture_ready_snapshot()? {
                            anyhow::ensure!(
                                root.episodes.is_none()
                                    && root.input_evidence.is_none()
                                    && root.work.is_none()
                                    && root.tasks.is_none(),
                                "deletion Scheduling maintenance cannot overwrite prepared domains"
                            );
                        }
                    }
                    Ok(())
                },
                move |host| {
                    let (mut ack, identity) = match operation {
                        Operation::Timer(id) => host.runtime_db().cancel_timer_for_deletion(
                            &original_scope,
                            &original_job,
                            &id,
                            &budget,
                        )?,
                        Operation::ParentTimers => host.runtime_db().cancel_parent_cleanup_timers(
                            &original_scope,
                            &original_job,
                            now,
                            &budget,
                        )?,
                        Operation::Trigger(id) => host.runtime_db().revoke_trigger_for_deletion(
                            &original_scope,
                            &original_job,
                            &id,
                            now,
                            &budget,
                        )?,
                    };
                    ack.guards.push(guard);
                    Ok((ack, identity))
                },
            )
            .await?;
        // Actual keys/invalidation are qualification ACKs only. This actor
        // publishes KnownMetadata, never a Scheduling Ready view.
        let changed = ack.changed;
        debug!(
            agent_id = agent,
            changed,
            qualified_keys = ack.qualified_actual_keys.len(),
            invalidated = ack.invalidation.is_some(),
            "maintained original deletion Scheduling namespace"
        );
        drop(ack);
        Ok(changed)
    }
}
async fn ensure_unloaded(host: &RuntimeHost, agent: &str) -> Result<()> {
    anyhow::ensure!(
        host.try_get_loaded_runtime(agent).await.is_none(),
        "deletion Scheduling cleanup requires the original unloaded boundary"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    use crate::{
        runtime::resident_scheduling::{SchedulingCaptureRequest, SchedulingFamily, SchedulingKey},
        runtime_db::scheduling_snapshot::{
            prepare_scheduling_write_tx, register_scheduling_writes_tx, SchedulingDomainLoadRequest,
        },
    };
    use std::collections::BTreeSet;
    fn original(
        host: &RuntimeHost,
        agent: &str,
    ) -> Result<(AgentIdentityRecord, AgentDeletionJob, TimerRecord)> {
        let id = crate::deletion::tests::metadata_identity(&host, agent);
        let timer = TimerRecord {
            calendar: None,
            id: "original-known-timer".into(),
            agent_id: id.agent_id.clone(),
            created_at: Utc::now() - chrono::Duration::seconds(2),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: None,
            next_fire_at: Some(Utc::now() + chrono::Duration::seconds(1)),
            last_fired_at: None,
            fire_count: 0,
        };
        let ledger = host.bridge().input_evidence_budget()?;
        let scope = InputEvidenceScope {
            agent_id: id.agent_id.clone(),
            identity_incarnation: id.incarnation,
        };
        let request = SchedulingDomainLoadRequest {
            capture: SchedulingCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 16 * 1024 * 1024,
        };
        host.runtime_db().transaction(|tx| {
            let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &timer.id)]);
            let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
            crate::runtime_db::repositories::upsert_timer_tx(tx, &timer)?;
            let state =
                crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                    .scope_facts()?
                    .state
                    .unwrap();
            register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)?;
            Ok(())
        })?;
        let (_, mut job, _) = host.runtime_db().agent_deletions().begin(
            &id.agent_id,
            id.revision,
            "fixture",
            false,
        )?;
        job.phase = AgentDeletionPhase::Quiesce;
        job.status = AgentDeletionStatus::Running;
        host.runtime_db().agent_deletions().update(&job)?;
        Ok((id, job, timer))
    }
    #[tokio::test]
    async fn scheduling_deletion_known_metadata_ack_survives_publication_fault_without_ready(
    ) -> Result<()> {
        let (_home, host) = crate::deletion::tests::metadata_test_host();
        let (id, job, timer) = original(&host, "scheduling-delete-known")?;
        let owner = host
            .bridge()
            .mutation_owner_for(&id.agent_id, id.incarnation)?;
        host.fail_next_external_mutation_publication();
        assert_eq!(
            host.cancel_deletion_timers(&id.agent_id, &job, Utc::now())
                .await?,
            1
        );
        let root = owner.capture()?.context("deletion metadata ACK missing")?;
        assert!(
            root.episodes.is_none()
                && root.input_evidence.is_none()
                && root.work.is_none()
                && root.tasks.is_none()
                && root.scheduling.is_none()
        );
        let before: u64 = host.runtime_db().connection()?.query_row(
            "SELECT revision FROM scheduling_domain_current WHERE agent_id=?1",
            [&id.agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(
            host.cancel_deletion_timers(&id.agent_id, &job, Utc::now())
                .await?,
            0
        );
        assert_eq!(
            host.runtime_db().connection()?.query_row(
                "SELECT revision FROM scheduling_domain_current WHERE agent_id=?1",
                [&id.agent_id],
                |r| r.get::<_, u64>(0)
            )?,
            before
        );
        assert_eq!(
            host.runtime_db()
                .timers()
                .latest(&timer.id)?
                .unwrap()
                .status,
            TimerStatus::Cancelled
        );
        assert!(owner.try_reserve().is_ok());
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn scheduling_deletion_lost_ack_keeps_original_unknown_and_blocks_new_sql() -> Result<()>
    {
        let (_home, host) = crate::deletion::tests::metadata_test_host();
        let (id, job, timer) = original(&host, "scheduling-delete-unknown")?;
        let scope = InputEvidenceScope {
            agent_id: id.agent_id.clone(),
            identity_incarnation: id.incarnation,
        };
        let ledger = host.bridge().input_evidence_budget()?;
        let frozen = job.clone();
        let timer_id = timer.id.clone();
        let result: Result<()> = host
            .commit_deletion_identity_metadata(
                id.agent_id.clone(),
                Some(id.incarnation),
                vec![],
                move |host| {
                    let _ack = host
                        .runtime_db()
                        .cancel_timer_for_deletion(&scope, &frozen, &timer_id, &ledger)?;
                    Err(crate::runtime_db::persistence::mark_persistence_failure(
                        anyhow!("original Scheduling deletion ACK lost"),
                        crate::runtime_db::PersistenceCertainty::Indeterminate,
                        crate::runtime_db::persistence::PersistenceFailurePhase::Commit,
                        None,
                    ))
                },
            )
            .await;
        assert!(result
            .unwrap_err()
            .downcast_ref::<DeletionMetadataPending>()
            .is_some());
        let owner = host
            .bridge()
            .mutation_owner_for(&id.agent_id, id.incarnation)?;
        assert!(matches!(
            owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ));
        assert_eq!(
            host.runtime_db()
                .timers()
                .latest(&timer.id)?
                .unwrap()
                .status,
            TimerStatus::Cancelled
        );
        assert!(host
            .commit_deletion_identity_metadata(
                id.agent_id.clone(),
                Some(id.incarnation),
                vec![],
                move |_| -> Result<((), AgentIdentityRecord)> {
                    panic!("Unknown original Scheduling ACK must block new SQL")
                }
            )
            .await
            .is_err());
        assert!(matches!(
            owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ));
        host.shutdown().await?;
        Ok(())
    }
}
