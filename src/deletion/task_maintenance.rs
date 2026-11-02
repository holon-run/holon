//! Cold deletion keeps original per-Task SQL and metadata publication ordering.
use super::*;
#[cfg(test)]
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::{
    runtime::input_evidence_budget::{serialized_bytes, InputMemoryCategory, OwnedBytesEstimate},
    runtime_db::task_snapshot::{DeletionTaskAck, TaskSnapshotError},
};
impl RuntimeHost {
    pub(super) async fn terminalize_deletion_tasks(
        &self,
        agent_id: &str,
        job: &AgentDeletionJob,
        now: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        anyhow::ensure!(
            self.try_get_loaded_runtime(agent_id).await.is_none(),
            "deletion Task cleanup requires the original unloaded Quiesce boundary"
        );
        let ledger = self.bridge().input_evidence_budget()?;
        let (scope, _scope_guard) = self
            .runtime_db()
            .deletion_task_scope_with_budget(agent_id, &ledger)?;
        let mut changed = 0usize;
        loop {
            let batch = self
                .runtime_db()
                .deletion_task_batch(&scope, job, &ledger)?;
            if batch.ids.is_empty() {
                break;
            }
            for id in &batch.ids {
                // Register new owned selector/job copies before cloning them
                // into the original external-mutation worker across await.
                let bytes = serialized_bytes(job)?
                    .checked_add(serialized_bytes(&scope)?)
                    .and_then(|n| n.checked_add(id.len() as u64 + 256))
                    .and_then(|n| n.checked_mul(8))
                    .context("deletion Task frozen metadata overflow")?;
                let limits = ledger.limits();
                ensure_actor_material(bytes, &limits)?;
                let guard = ledger.register_owned_bytes(
                    &scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: bytes,
                    },
                )?;
                let frozen_job = job.clone();
                let frozen_scope = scope.clone();
                let task_id = id.clone();
                let budget = ledger.clone();
                let ack: DeletionTaskAck = self
                    .commit_deletion_identity_metadata_checked(
                        agent_id.into(),
                        Some(scope.identity_incarnation),
                        vec![],
                        |reservations| {
                            for reservation in reservations {
                                if let Some(root)=reservation.capture_ready_snapshot()? {
                                    anyhow::ensure!(root.episodes.is_none() && root.input_evidence.is_none() && root.work.is_none() && root.tasks.is_none(),
                                        "deletion Task maintenance cannot overwrite prepared domains");
                                }
                            }
                            Ok(())
                        },
                        move |host| {
                            let (mut ack, identity) = host.runtime_db().cancel_task_for_deletion(
                                &frozen_scope,
                                &frozen_job,
                                &task_id,
                                now,
                                &budget,
                            )?;
                            ack.guards.push(guard);
                            Ok((ack, identity))
                        },
                    )
                    .await?;
                changed = changed
                    .checked_add(usize::from(ack.changed))
                    .context("deletion Task count overflow")?;
                // No Task body survives the ACK; KnownMetadata keeps TaskNone.
                drop(ack);
            }
        }
        Ok(changed)
    }
}
fn ensure_actor_material(bytes: u64, limits: &crate::config::StateMemoryLimits) -> Result<()> {
    anyhow::ensure!(
        bytes
            <= limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("deletion Task limits overflow")?
                .min(64 * 1024 * 1024)
            && 8 <= limits.record_limit,
        TaskSnapshotError::ResourceExhausted("deletion Task frozen metadata")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
        runtime_db::task_snapshot::{
            prepare_task_write_tx, register_task_writes_tx, TaskDomainLoadRequest,
        },
    };
    use std::collections::BTreeSet;
    fn qualified(
        host: &RuntimeHost,
        id: &str,
    ) -> Result<(AgentIdentityRecord, AgentDeletionJob, TaskRecord)> {
        let identity = crate::deletion::tests::metadata_identity(host, id);
        let task = TaskRecord {
            id: format!("task:{id}"),
            agent_id: id.into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: Utc::now() - chrono::Duration::seconds(2),
            updated_at: Utc::now() - chrono::Duration::seconds(1),
            parent_message_id: None,
            work_item_id: None,
            summary: Some("original task".into()),
            detail: None,
            recovery: None,
        };
        let scope = InputEvidenceScope {
            agent_id: id.into(),
            identity_incarnation: identity.incarnation,
        };
        host.runtime_db().transaction(|tx| {
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
            let proof = prepare_task_write_tx(tx, &scope, &keys)?;
            crate::runtime_db::repositories::upsert_task_tx(tx, &task)?;
            let state = crate::runtime_db::context_inputs::ContextDurableRead::new(tx, id)
                .scope_facts()?
                .state
                .unwrap();
            register_task_writes_tx(
                tx,
                &proof,
                &keys,
                &state,
                &TaskDomainLoadRequest {
                    recovery_at: None,
                    query: Default::default(),
                    capture: TaskCaptureRequest::current(),
                    record_limit: 4096,
                    byte_limit: 4 * 1024 * 1024,
                },
            )?;
            Ok(())
        })?;
        let (_, mut job, _) =
            host.runtime_db()
                .agent_deletions()
                .begin(id, identity.revision, "fixture", false)?;
        job.phase = AgentDeletionPhase::Quiesce;
        job.status = AgentDeletionStatus::Running;
        host.runtime_db().agent_deletions().update(&job)?;
        Ok((identity, job, task))
    }
    #[tokio::test]
    async fn deletion_task_known_publication_fault_retains_sparse_ack_without_replaying_body(
    ) -> Result<()> {
        let (_home, host) = crate::deletion::tests::metadata_test_host();
        let (identity, job, task) = qualified(&host, "task-delete-known")?;
        let owner = host
            .bridge()
            .mutation_owner_for(&identity.agent_id, identity.incarnation)?;
        host.fail_next_external_mutation_publication();
        assert_eq!(
            host.terminalize_deletion_tasks(&identity.agent_id, &job, Utc::now())
                .await?,
            1
        );
        let root = owner
            .capture()?
            .context("cold metadata publication missing")?;
        assert!(root.tasks.is_none());
        assert_eq!(
            host.runtime_db().tasks().latest(&task.id)?.unwrap().status,
            TaskStatus::Cancelled
        );
        let before: u64 = host.runtime_db().connection()?.query_row(
            "SELECT revision FROM task_domain_current WHERE agent_id=?1",
            [&identity.agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(
            host.terminalize_deletion_tasks(&identity.agent_id, &job, Utc::now())
                .await?,
            0
        );
        assert_eq!(
            host.runtime_db().connection()?.query_row(
                "SELECT revision FROM task_domain_current WHERE agent_id=?1",
                [&identity.agent_id],
                |r| r.get::<_, u64>(0)
            )?,
            before
        );
        assert!(owner.try_reserve().is_ok());
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn deletion_task_lost_ack_keeps_original_unknown_and_never_replays_cleanup() -> Result<()>
    {
        let (_home, host) = crate::deletion::tests::metadata_test_host();
        let (identity, job, task) = qualified(&host, "task-delete-unknown")?;
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let ledger = host.bridge().input_evidence_budget()?;
        let original = job.clone();
        let task_id = task.id.clone();
        let result: Result<()> = host
            .commit_deletion_identity_metadata(
                identity.agent_id.clone(),
                Some(identity.incarnation),
                vec![],
                move |host| {
                    let _ack = host.runtime_db().cancel_task_for_deletion(
                        &scope,
                        &original,
                        &task_id,
                        Utc::now(),
                        &ledger,
                    )?;
                    Err(crate::runtime_db::persistence::mark_persistence_failure(
                        anyhow!("original deletion Task ACK lost"),
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
            .mutation_owner_for(&identity.agent_id, identity.incarnation)?;
        assert!(matches!(
            owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ));
        assert_eq!(
            host.runtime_db().tasks().latest(&task.id)?.unwrap().status,
            TaskStatus::Cancelled
        );
        // A retry is not allowed to turn a subsequent no-op into absence proof.
        assert!(host
            .commit_deletion_identity_metadata(
                identity.agent_id.clone(),
                Some(identity.incarnation),
                vec![],
                move |_| -> Result<((), AgentIdentityRecord)> {
                    panic!("unknown original phase must prevent all new SQL")
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

#[cfg(test)]
mod scope_budget_tests {
    use super::*;
    #[tokio::test]
    async fn deletion_task_header_budget_precedes_bad_identity_and_state_decode() -> Result<()> {
        for table in ["agent_identities", "agent_states"] {
            let (_home, host) = crate::deletion::tests::metadata_test_host();
            let identity = crate::deletion::tests::metadata_identity(&host, "delete-scope-budget");
            let (_, mut job, _) = host.runtime_db().agent_deletions().begin(
                &identity.agent_id,
                identity.revision,
                "fixture",
                false,
            )?;
            job.status = AgentDeletionStatus::Running;
            job.phase = AgentDeletionPhase::Quiesce;
            host.runtime_db().agent_deletions().update(&job)?;
            let original: String = host.runtime_db().connection()?.query_row(
                &format!("SELECT payload_json FROM {table} WHERE agent_id=?1"),
                [&identity.agent_id],
                |r| r.get(0),
            )?;
            let ledger = host.bridge().input_evidence_budget()?;
            let before = ledger.report()?.observed_bytes;
            host.runtime_db().connection()?.execute(&format!("UPDATE {table} SET payload_json='{{'||hex(zeroblob(5*1024*1024)) WHERE agent_id=?1"),[&identity.agent_id])?;
            let error = host
                .terminalize_deletion_tasks(&identity.agent_id, &job, Utc::now())
                .await
                .err()
                .context("bad large scope body accepted")?;
            assert!(
                matches!(
                    error.downcast_ref::<TaskSnapshotError>(),
                    Some(TaskSnapshotError::ResourceExhausted(
                        "deletion Task scope header"
                    ))
                ),
                "{table}: {error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, before);
            let owner = host
                .bridge()
                .mutation_owner_for(&identity.agent_id, identity.incarnation)?;
            assert!(
                owner.capture().is_ok(),
                "no mutation ticket may begin before budget rejection"
            );
            host.runtime_db().connection()?.execute(
                &format!("UPDATE {table} SET payload_json=?1 WHERE agent_id=?2"),
                rusqlite::params![original, identity.agent_id],
            )?;
            assert_eq!(
                host.runtime_db()
                    .agent_deletions()
                    .latest_for_agent(&identity.agent_id)?
                    .unwrap(),
                job
            );
            host.shutdown().await?;
        }
        Ok(())
    }
}
