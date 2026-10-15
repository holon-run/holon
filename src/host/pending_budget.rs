//! Release logical Accepted charges only after exact retired-scope proof.

use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use rusqlite::{OptionalExtension, TransactionBehavior};

impl RuntimeHost {
    /// The reservation excludes online writers/reincarnation. Known metadata
    /// may reconcile repeatedly; Unknown attempts and their tickets stay fenced.
    /// A nonempty queue preserves charges and does not prevent archive cleanup.
    pub(super) fn reconcile_deleted_pending_budget(
        &self,
        reservation: &MutationReservation,
    ) -> Result<bool> {
        if let Some(recovery) = reservation.recovery()? {
            anyhow::ensure!(
                recovery.known.is_some(),
                "retired pending budget requires original mutation reconciliation"
            );
        }
        let scope = InputEvidenceScope {
            agent_id: reservation.agent_id().to_owned(),
            identity_incarnation: reservation.identity_incarnation(),
        };
        let mut connection = self.runtime_db().connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let row = tx
            .query_row(
                "SELECT status, payload_json FROM agent_identities WHERE agent_id = ?1",
                [&scope.agent_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .context("retired pending budget identity is absent")?;
        let actual = crate::runtime_db::repositories::decode_agent_identity_payload(&row.1)?;
        anyhow::ensure!(
            row.0 == "deleted"
                && actual.agent_id == scope.agent_id
                && actual.incarnation == scope.identity_incarnation
                && actual.status == AgentRegistryStatus::Deleted,
            "retired pending budget identity differs from the original deleted scope"
        );
        // These are the durable accepted-body states used by cold admission.
        let has_pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queue_entries
             WHERE agent_id = ?1 AND status IN ('queued', 'interrupted'))",
            [&scope.agent_id],
            |row| row.get(0),
        )?;
        if !has_pending {
            // This rejects Reserved/Started/Unknown tickets before changing any
            // charge. The ledger itself is not an identity or receipt witness.
            self.inner
                .input_evidence_budget
                .install_actual_pending(&scope, &[])?;
        }
        tx.commit()?;
        Ok(!has_pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::input_evidence_budget::{
        serialized_bytes, InputEvidenceBudgetError, PendingAdmission, PendingBodyCharge,
    };

    fn fixture_host(body: &MessageBody) -> (tempfile::TempDir, RuntimeHost) {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut config = AppConfig::load_with_home(Some(home.path().to_owned())).unwrap();
        config.user_home_dir = Some(home.path().to_owned());
        let bytes = serialized_bytes(body).unwrap();
        config.stored_config.runtime.state_memory.pending_host_bytes = Some(bytes);
        config
            .stored_config
            .runtime
            .state_memory
            .pending_per_agent_bytes = Some(bytes);
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("unused")),
        )
        .unwrap();
        host.ensure_default_agent_identity().unwrap();
        (home, host)
    }

    fn body() -> MessageBody {
        MessageBody::Text {
            text: "accepted until durable retirement".into(),
        }
    }

    // Pause only the fixture scheduler so the real enqueue stays Queued without
    // provider calls. Runtime admission, its owner and the SQL path remain real.
    fn enqueue_without_dispatch<'a>(
        host: &'a RuntimeHost,
        agent_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = (RuntimeHandle, MessageEnvelope)> + Send + 'a>> {
        Box::pin(async move {
            let runtime = host.get_or_create_agent(agent_id).await.unwrap();
            runtime.wait_for_bootstrap().await.unwrap();
            {
                let registry = host.inner.runtimes.read().await;
                registry.agents.get(agent_id).unwrap().task.abort();
            }
            tokio::task::yield_now().await;
            let message = runtime
                .enqueue(MessageEnvelope::new(
                    agent_id,
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: Some("test".into()),
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    body(),
                ))
                .await
                .unwrap();
            assert_eq!(
                host.runtime_db()
                    .queue_entries()
                    .latest(&message.id)
                    .unwrap()
                    .unwrap()
                    .status,
                QueueEntryStatus::Queued
            );
            (runtime, message)
        })
    }

    #[tokio::test]
    async fn cold_unload_then_operator_delete_releases_pending_budget_for_new_incarnation() {
        Box::pin(async {
            let (_home, host) = fixture_host(&body());
            let agent_id = "pending-budget-reborn";
            let first = host
                .create_public_named_agent(agent_id, None, None, None)
                .await
                .unwrap();
            let (runtime, message) = enqueue_without_dispatch(&host, agent_id).await;
            let bytes = serialized_bytes(&body()).unwrap();
            assert_eq!(
                host.inner
                    .input_evidence_budget
                    .report()
                    .unwrap()
                    .pending
                    .body_bytes,
                bytes
            );
            host.unload_runtime(agent_id).await;
            assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
            assert_eq!(
                host.inner
                    .input_evidence_budget
                    .report()
                    .unwrap()
                    .pending
                    .body_bytes,
                bytes
            );
            assert_eq!(
                host.runtime_db()
                    .queue_entries()
                    .latest(&message.id)
                    .unwrap()
                    .unwrap()
                    .status,
                QueueEntryStatus::Queued
            );

            let (_, job, _) = host
                .begin_public_agent_deletion(agent_id, false, "test-operator")
                .await
                .unwrap();
            host.execute_deletion_job(job).await.unwrap();
            assert_eq!(
                host.agent_identity_record(agent_id)
                    .unwrap()
                    .unwrap()
                    .status,
                AgentRegistryStatus::Deleted
            );
            assert_eq!(
                host.runtime_db()
                    .queue_entries()
                    .latest(&message.id)
                    .unwrap()
                    .unwrap()
                    .status,
                QueueEntryStatus::Aborted
            );
            assert_eq!(
                host.inner
                    .input_evidence_budget
                    .report()
                    .unwrap()
                    .pending
                    .count,
                0
            );
            assert_eq!(
                host.inner
                    .input_evidence_budget
                    .report()
                    .unwrap()
                    .pending
                    .body_bytes,
                0
            );

            let second = host
                .create_public_named_agent(agent_id, None, None, None)
                .await
                .unwrap();
            assert_eq!(second.identity.incarnation, first.identity.incarnation + 1);
            let (_new_runtime, new_message) = enqueue_without_dispatch(&host, agent_id).await;
            assert_ne!(new_message.id, message.id);
            let report = host.inner.input_evidence_budget.report().unwrap();
            assert_eq!(report.pending.count, 1);
            assert_eq!(report.pending.body_bytes, bytes);
            assert!(report
                .scopes
                .iter()
                .filter(|entry| entry.pending.count > 0)
                .all(|entry| entry.scope.identity_incarnation == second.identity.incarnation));
            assert!(!runtime.accepts_execution_admission());
            host.shutdown().await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn private_archive_known_ack_keeps_nonempty_budget_then_releases_after_cleanup() {
        Box::pin(async {
            let (_home, host) = fixture_host(&body());
            let agent_id = "pending-budget-private";
            let identity = AgentIdentityRecord::new(
                agent_id,
                AgentKind::Named,
                AgentVisibility::Private,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PrivateChild,
                None,
                None,
            );
            host.runtime_db().agent_identities().upsert(&identity).unwrap();
            host.cache_agent_identity(&identity).unwrap();
            let (_runtime, message) = enqueue_without_dispatch(&host, agent_id).await;
            host.unload_runtime(agent_id).await;
            let owner = host.inner.mutation_owners.current_owner(agent_id, identity.incarnation).unwrap();
            let mut reservations = host.reserve_agent_mutations(&[agent_id.to_owned()]).await.unwrap();
            let mut facts = host.external_identity_facts(&reservations).unwrap();
            host.fail_next_external_mutation_publication();
            let error = host.commit_external_metadata(&mut reservations, ids::audit_event_id(), || {
                let deleted = host.runtime_db().agent_identities().tombstone_with_closed_supervision(agent_id)?;
                facts[0].identity = Some(Arc::new(deleted));
                Ok(((), facts))
            }).await.unwrap_err();
            assert!(error.to_string().contains("committed but publication requires reconciliation"));
            let key = reservations[0].recovery().unwrap().unwrap().key;
            assert!(matches!(owner.capture(), Err(MutationOwnerError::NeedsReconcile(pending)) if pending == key));
            assert_eq!(host.inner.input_evidence_budget.report().unwrap().pending.count, 1);
            host.reconcile_external_mutations(&mut reservations).await.unwrap();
            assert!(owner.capture().is_ok());
            assert_eq!(host.inner.input_evidence_budget.report().unwrap().pending.count, 1, "Deleted ACK alone does not prove the queue empty");
            drop(reservations);

            host.archive_private_agent(agent_id).await.unwrap();
            assert_eq!(host.runtime_db().queue_entries().latest(&message.id).unwrap().unwrap().status, QueueEntryStatus::Aborted);
            assert_eq!(host.inner.input_evidence_budget.report().unwrap().pending.count, 0);
            assert!(!host.agent_data_dir(agent_id).exists());
            host.shutdown().await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn retired_budget_does_not_clear_unknown_attempt_or_unresolved_ticket() {
        let (_home, host) = fixture_host(&body());
        let agent_id = host.config().default_agent_id.clone();
        let identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let scope = InputEvidenceScope {
            agent_id: agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let ledger = &host.inner.input_evidence_budget;
        let ticket = match ledger.reserve_pending(&scope, "unknown-body", 1).unwrap() {
            PendingAdmission::Reserved(ticket) => ticket,
            PendingAdmission::ExistingAccepted => panic!("fixture needs an original ticket"),
        };
        ledger.mark_attempt_started(&ticket).unwrap();
        ledger.mark_unknown(&ticket).unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&agent_id, identity.incarnation)
            .unwrap();
        let mut reservations = MutationOwner::reserve_all(vec![owner.clone()])
            .await
            .unwrap();
        let key = MutationReservation::begin_all(&mut reservations, ids::audit_event_id())
            .unwrap()
            .remove(0);
        let deleted = host
            .runtime_db()
            .agent_identities()
            .tombstone_with_closed_supervision(&agent_id)
            .unwrap();
        // The database can contain a tombstone even though this attempt has no
        // ACK. It must not turn an Unknown ticket into an empty-queue release.
        assert!(host
            .reconcile_deleted_pending_budget(&reservations[0])
            .is_err());
        assert_eq!(ledger.report().unwrap().pending.unresolved_count, 1);
        assert!(
            matches!(owner.capture(), Err(MutationOwnerError::NeedsReconcile(pending)) if pending == key)
        );
        assert!(reservations[0].recovery().unwrap().unwrap().known.is_none());

        MutationReservation::committed_all(
            &mut reservations,
            vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                _identity_budget: None,
                identity: Some(Arc::new(deleted)),
                state: None,
            }))],
        )
        .unwrap();
        let error = host
            .reconcile_external_mutations(&mut reservations)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<InputEvidenceBudgetError>(),
            Some(InputEvidenceBudgetError::ScopeHasUnresolvedAttempt)
        ));
        assert_eq!(ledger.report().unwrap().pending.unresolved_count, 1);
        assert!(
            matches!(owner.capture(), Err(MutationOwnerError::NeedsReconcile(pending)) if pending == key)
        );
        assert!(reservations[0].recovery().unwrap().unwrap().known.is_some());
        drop(reservations);
        // Original-family proof is the only way to settle this ticket.
        ledger.resolve_not_committed(&ticket).unwrap();
        let reservations = host
            .reserve_mutation_scopes(&[(agent_id.clone(), identity.incarnation)])
            .await
            .unwrap();
        drop(reservations);
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retired_budget_and_archive_scope_check_reject_a_foreign_incarnation() {
        let (_home, host) = fixture_host(&body());
        let agent_id = host.config().default_agent_id.clone();
        let identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let scope = InputEvidenceScope {
            agent_id: agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        host.inner
            .input_evidence_budget
            .install_actual_pending(
                &scope,
                &[PendingBodyCharge {
                    message_id: "accepted-old-body".into(),
                    body_bytes: 1,
                }],
            )
            .unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&agent_id, identity.incarnation)
            .unwrap();
        let reservations = MutationOwner::reserve_all(vec![owner]).await.unwrap();
        let mut successor = identity.clone();
        successor.incarnation += 1;
        successor.status = AgentRegistryStatus::Deleted;
        successor.deleted_at = Some(Utc::now());
        let payload = serde_json::to_string(&successor).unwrap();
        // Simulate a conflicting restore after the old drain window. Ordinary
        // identity creation is owner-fenced and cannot perform this change.
        host.runtime_db()
            .transaction(|tx| {
                tx.execute(
                    "UPDATE agent_identities SET status='deleted', payload_json=?2 WHERE agent_id=?1",
                    rusqlite::params![agent_id, payload],
                )?;
                Ok(())
            })
            .unwrap();
        let new_home = host.agent_data_dir(&agent_id);
        fs::create_dir_all(&new_home).unwrap();
        fs::write(new_home.join("successor-marker"), "keep").unwrap();
        assert!(host
            .reconcile_deleted_pending_budget(&reservations[0])
            .is_err());
        assert_eq!(
            host.inner
                .input_evidence_budget
                .report()
                .unwrap()
                .pending
                .count,
            1
        );
        assert_eq!(
            host.runtime_db()
                .agent_identities()
                .latest(&agent_id)
                .unwrap()
                .unwrap(),
            successor
        );
        assert_eq!(
            fs::read_to_string(new_home.join("successor-marker")).unwrap(),
            "keep"
        );
        assert!(host
            .runtime_db()
            .queue_entries()
            .queued_for_agent(&agent_id)
            .unwrap()
            .is_empty());
        drop(reservations);
        host.shutdown().await.unwrap();
    }
}
