//! A successful original delivery lends its existing request/output allocation.
use super::*;
use crate::runtime::tasks::OriginalTaskInputMaterial;
impl RuntimeHandle {
    pub(in crate::runtime) async fn finish_owned_original_task_input_audit(
        &self,
        event: AuditEvent,
        material: &OriginalTaskInputMaterial,
    ) -> Result<()> {
        material.validate_owner(self)?;
        let kind = if event.data.get("child_agent_id").is_some() {
            OriginalLifecycleNoopKind::TaskInputChild
        } else {
            OriginalLifecycleNoopKind::TaskInputCommand
        };
        let (scope, owner_epoch, ledger, guard, records) = material.source_parts();
        let source = OriginalLifecycleNoopAuditSource {
            scope,
            owner_epoch,
            ledger,
            kind,
            event_digest: OriginalLifecycleNoopAuditSource::digest(&event)?,
            shutdown_abort_digest: None,
            workspace_request: None,
            material_records: records,
            _material: guard,
        };
        source.validate_owner(self, &event)?;
        let mut owner = self.begin_mutation().await?;
        anyhow::ensure!(
            owner.reservations[owner.primary_index].owner_epoch() == owner_epoch,
            "TaskInput Audit owner changed while waiting"
        );
        let root = owner.capture_published_root()?;
        root.active_identity()?;
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_task_input_audit(
                owner.input_evidence_scope(),
                event,
                root.state.clone(),
                source,
            )?;
        drop(root);
        owner.commit_prepared_input_transition(prepared).await?;
        Ok(())
    }
}

#[cfg(test)]
impl RuntimeHandle {
    pub(crate) async fn original_task_input_child_lost_ack_for_test(
        &self,
        mut message: MessageEnvelope,
        material: Arc<super::super::OriginalTaskInputChildMaterial>,
    ) -> Result<()> {
        use crate::runtime_db::{
            input_mutation_receipts::OriginalInputMutationAttempt,
            transitions::{
                AgentStateMutation, InputMutationExecution, PreparedInputTransition,
                PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
            },
        };
        material.validate_message(&message)?;
        message.normalize_admission_fields();
        message.turn_id = Some(crate::ids::turn_id());
        let id = message.id.clone();
        let mut owner = self.begin_mutation().await?;
        let scope = owner.input_evidence_scope();
        let epoch = owner.reservations[owner.primary_index].owner_epoch();
        let ledger = &self.inner.input_evidence_budget;
        let before = owner.last_persisted_state.clone();
        let mut next = owner.state.clone();
        next.pending = owner.queue.len() + 1;
        super::super::scheduler::apply_message_wake_projection(&mut next);
        let command = QueueTransitionCommand {
            agent_id: scope.agent_id.clone(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: id.clone(),
                agent_id: scope.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            }),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(before)),
                record: Box::new(next),
            }),
            message_evidence: vec![message],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        };
        let prepared = PreparedInputTransition::queue(
            scope.clone(),
            crate::ids::audit_event_id(),
            self.now(),
            PreparedQueueTransition::new(command),
        )?
        .with_original_task_input_child_material(material.clone(), epoch, ledger)?;
        let mut prepared = owner.attach_actual_dispatch_memory(prepared)?;
        if let Some(current) = owner.current_reads.as_ref() {
            prepared = prepared.with_current_read_source(current.clone())?;
        }
        let work = owner.owner_work_load_request()?;
        let (task, _task_budget) = owner.owner_task_load_request()?;
        let source = owner.reservations[owner.primary_index].input_query_source(
            &scope,
            ledger,
            (work.byte_limit, work.record_limit),
        )?;
        let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
            prepared.descriptor().command_nonce().into(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            Vec::new(),
        )?
        .with_input_runtime(vec![Arc::new(owner.facts(Default::default())?)])?
        .with_original_task_input_child_material(
            &prepared,
            &owner.reservations[owner.primary_index],
            ledger,
        )?;
        assert_eq!(
            witness.original_input_material_prior(&scope, ledger)?,
            material.material_estimate()?
        );
        let foreign = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
            Default::default(),
        )?;
        assert!(witness
            .original_input_material_prior(&scope, &foreign)
            .is_err());
        assert_eq!(foreign.report()?.observed_bytes, 0);
        let commit = || {
            self.runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition_with_input_query_source(
                    &prepared, &work, ledger, &task, &source,
                )
        };
        let error = owner
            .commit_transition_inner(
                prepared.descriptor().command_nonce().into(),
                Some(witness),
                || {
                    assert!(matches!(
                        commit()?,
                        InputMutationExecution::Committed { .. }
                    ));
                    Err(anyhow!("TaskInput child ACK lost after actual COMMIT"))
                },
            )
            .await
            .err()
            .context("TaskInput child ACK was not lost")?;
        assert!(format!("{error:#}").contains("lost after actual COMMIT"));
        drop(owner);
        let held = self.inner.mutation_owner.reserve().await?;
        assert!(held
            .recovery()?
            .context("TaskInput child unknown missing")?
            .known
            .is_none());
        drop(held);
        drop(self.begin_mutation().await?);
        assert_eq!(
            self.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM messages WHERE message_id=?1",
                [&id],
                |row| row.get::<_, usize>(0)
            )?,
            1
        );
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("TaskInput recovered Root missing")?;
        assert!(root
            .input_evidence
            .as_ref()
            .context("recovered Input missing")?
            .capture()?
            .lookup(
                &super::super::resident_input_evidence::InputEvidenceKey::new(
                    super::super::resident_input_evidence::InputEvidenceFamily::Message,
                    &id
                )
            )?
            .is_some());
        drop(root);
        let original = self
            .runtime_db()
            .agent_identities()
            .latest(self.instance_agent_id())?
            .context("actual child identity absent")?;
        let mut deleting = original.clone();
        deleting.status = crate::types::AgentRegistryStatus::Deleting;
        self.runtime_db().agent_identities().upsert(&deleting)?;
        let replay = commit();
        self.runtime_db().agent_identities().upsert(&original)?;
        assert!(
            matches!(replay?, InputMutationExecution::Replayed { .. }),
            "exact child receipt must keep its original authority after Deleting"
        );
        assert_eq!(
            self.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM messages WHERE message_id=?1",
                [&id],
                |row| row.get::<_, usize>(0)
            )?,
            1
        );
        Ok(())
    }
}
