//! One already accepted child monitor owns the complete sorted observer group.
use super::*;
use crate::{
    runtime::{
        task_queries::TaskRecoveryQueries, FrozenChildTaskTerminal, OriginalChildTaskProducer,
        OriginalChildTerminalRead,
    },
    runtime_db::{
        child_task_terminal::{discover_original_child_observers_tx, terminal_observer_scopes},
        task_snapshot::TaskDomainLoadRequest,
        transitions::{OriginalChildStatusSelection, PreparedInputTransition},
    },
};
impl RuntimeHostBridge {
    pub(crate) async fn commit_original_child_task_running(
        &self,
        source: &Arc<OriginalChildTaskProducer>,
        pending: &mut Option<Arc<PreparedInputTransition>>,
    ) -> Result<()> {
        use crate::runtime::input_evidence_budget::{serialized_bytes, PendingAdmission};
        use crate::runtime::mutation_witness::MutationWitness;
        use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;
        use crate::runtime_db::transitions::InputMutationExecution;
        let host = self.host()?;
        let scope = &source.address().scope;
        let ledger = &host.inner.input_evidence_budget;
        source.validate_material_ledger(ledger)?;
        let limits = ledger.limits();
        let (record_limit, byte_limit) = TaskDomainLoadRequest::bounded_material_limits(
            usize::try_from(limits.record_limit)?,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Running limits overflow")?,
            )?,
        );
        let mut request = TaskDomainLoadRequest {
            recovery_at: None,
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            query: Default::default(),
            record_limit,
            byte_limit,
        };
        // This is the original monitor, not a new enqueue admission or worker.
        // The current scope owner still owns its normal mutation fence.
        let mut reservations = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        let mut facts = Vec::new();
        let mut queries = Vec::new();
        if let Some(runtime) = self.runtime_for_mutation(&reservations[0]).await? {
            if let Some(query) =
                runtime.task_recovery_queries(&reservations[0], record_limit, byte_limit)?
            {
                if let Some(selection) = query.selection_for(scope, reservations[0].owner_epoch()) {
                    request.query = selection.clone();
                    request
                        .capture
                        .required_keys
                        .extend(selection.required_keys.iter().cloned());
                }
                queries.push(query);
            }
            if let Some(root) = reservations[0].capture_ready_snapshot()? {
                facts.push(Arc::new(
                    runtime
                        .prepare_external_input_facts(
                            &reservations[0],
                            root.state.clone(),
                            root.episodes.clone(),
                            root.input_evidence.clone(),
                        )
                        .await?,
                ));
            }
        }
        if pending.is_none() {
            let prepared = host.runtime_db().transaction_once(|tx| {
                PreparedInputTransition::prepare_original_child_running_tx(
                    tx, source, &request, ledger,
                )
            })?;
            let prepared = crate::runtime::attach_original_child_memory_to_owner(
                host.runtime_db(),
                ledger,
                &reservations[0],
                prepared,
                source
                    .frozen_running()?
                    .context("Running Memory source absent")?
                    .at,
            )?;
            *pending = Some(Arc::new(prepared));
        }
        let prepared = pending.as_ref().context("Running capsule absent")?;
        prepared.validate_original_child_running(source)?;
        let mut tickets = Vec::new();
        for message in prepared.pending_messages()? {
            match ledger.reserve_pending(scope, &message.id, serialized_bytes(&message.body)?) {
                Ok(PendingAdmission::Reserved(ticket)) => tickets.push((ledger.clone(), ticket)),
                Ok(PendingAdmission::ExistingAccepted) => {}
                Err(error) => {
                    for (ledger, ticket) in &tickets {
                        ledger.definitely_not_committed(ticket)?;
                    }
                    return Err(error.into());
                }
            }
        }
        let cleanup = tickets.clone();
        let witness = match MutationWitness::input_mutation(
            prepared.descriptor().command_nonce().into(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            tickets,
        )
        .and_then(|witness| witness.with_input_runtime(facts))
        .and_then(|witness| witness.with_original_child_running(source, prepared, &reservations[0]))
        {
            Ok(witness) => witness,
            Err(error) => {
                for (ledger, ticket) in &cleanup {
                    ledger.definitely_not_committed(ticket)?;
                }
                return Err(error);
            }
        };
        if let Err(error) = MutationReservation::begin_all_with_witness(
            &mut reservations,
            prepared.descriptor().command_nonce().into(),
            witness.clone(),
        ) {
            witness.release_pending_not_committed(false)?;
            return Err(error.into());
        }
        if let Err(error) = witness.mark_pending_started() {
            witness.finish_original_attempt();
            witness.release_pending_not_committed(false)?;
            MutationReservation::definitely_not_committed_all(&mut reservations)?;
            return Err(error);
        }
        let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit,
            byte_limit,
        };
        let execution = host
            .runtime_db()
            .transitions_in_scope(scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                prepared, &work, ledger, &request,
            );
        if matches!(&execution, Ok(InputMutationExecution::Replayed { .. })) {
            witness.mark_input_storage_replayed()?;
        }
        witness.finish_original_attempt();
        let execution = match execution {
            Ok(execution) => execution,
            Err(error) => {
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    witness.release_pending_not_committed(false)?;
                    MutationReservation::definitely_not_committed_all(&mut reservations)?;
                } else {
                    witness.mark_pending_unknown()?;
                }
                return Err(error);
            }
        };
        let (receipt, _guards) = match execution {
            InputMutationExecution::Committed { value, receipt } => {
                #[cfg(test)]
                if value.effects.fault
                    == Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate)
                {
                    // Lose the physical ACK before installing any of its domains.
                    // The original witness/capsule must resolve this as Unknown.
                    witness.mark_pending_unknown()?;
                    anyhow::bail!("original Running committed before ACK test fault");
                }
                witness.mark_original_input_receipt_pending(&receipt, false)?;
                (receipt, value.effects.task_material_budget_views)
            }
            InputMutationExecution::Replayed {
                receipt,
                _material_budget_views,
            } => {
                witness.mark_original_input_receipt_pending(&receipt, true)?;
                (receipt, _material_budget_views)
            }
        };
        anyhow::ensure!(
            matches!(&receipt.outcome.result,
                crate::runtime_db::input_mutation_receipts::InputMutationResult::Task {
                    task_id, deletion: None, stop: None
                } if task_id == &source.address().admitted.key.id),
            "Running whole ACK changed its original Task"
        );
        crate::runtime::input_recovery::resolve_input_recovery_with_queries(
            host.runtime_db(),
            &mut reservations,
            ledger,
            &host.runtime_context_config(),
            &RuntimeModelCatalog::from_config(&host.config()),
            &queries,
        )?;
        host.reconcile_external_mutations(&mut reservations).await?;
        Ok(())
    }
    pub(crate) async fn commit_original_child_task_terminal(
        &self,
        source: &Arc<OriginalChildTaskProducer>,
        frozen: &FrozenChildTaskTerminal,
        pending: &mut Option<Arc<PreparedInputTransition>>,
        base: &TaskDomainLoadRequest,
    ) -> Result<OriginalChildTerminalRead> {
        let host = self.host()?;
        let scope = &source.address().scope;
        let ledger = &host.inner.input_evidence_budget;
        source.validate_material_ledger(ledger)?;
        let mut held_inventory = None;
        let mut held_group = None;
        let targets = if let Some(prepared) = pending.as_ref() {
            anyhow::ensure!(
                prepared
                    .original_child_terminal()
                    .is_some_and(|intent| &intent.original == source.address()),
                "child retry changed original producer"
            );
            prepared
                .descriptor()
                .scopes()
                .iter()
                .map(|s| (s.agent_id.clone(), s.identity_incarnation))
                .collect::<Vec<_>>()
        } else {
            // Discovery never expands one held parent into an unsorted nested lock.
            let initial = host
                .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
                .await?;
            let inventory = host
                .runtime_db()
                .transaction(|tx| discover_original_child_observers_tx(tx, source, base, ledger))?;
            let group = terminal_observer_scopes(source, frozen, &inventory, base, ledger)?;
            let targets = group
                .scopes
                .iter()
                .map(|s| (s.agent_id.clone(), s.identity_incarnation))
                .collect::<Vec<_>>();
            held_inventory = Some(inventory);
            held_group = Some(group);
            drop(initial);
            targets
        };
        let mut reservations = host.reserve_mutation_scopes(&targets).await?;
        let actor_modes = host.runtime_db().transaction(|tx| {
            crate::runtime_db::child_task_terminal::observe_original_child_actor_modes_tx(
                tx, source, &targets, base, ledger,
            )
        })?;
        let mut facts = Vec::new();
        let mut queries = Vec::<TaskRecoveryQueries>::new();
        let mut selections = Vec::new();
        let mut request = base.clone();
        for reservation in &reservations {
            let reserved_scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: reservation.agent_id().to_owned(),
                identity_incarnation: reservation.identity_incarnation(),
            };
            if !actor_modes.is_current(&reserved_scope) {
                continue;
            }
            if let Some(runtime) = self.runtime_for_mutation(reservation).await? {
                if let Some(query) = runtime.task_recovery_queries(
                    reservation,
                    base.record_limit,
                    base.byte_limit,
                )? {
                    if reservation.agent_id() == scope.agent_id
                        && reservation.identity_incarnation() == scope.identity_incarnation
                    {
                        if let Some(selection) =
                            query.selection_for(scope, reservation.owner_epoch())
                        {
                            request.query = selection.clone();
                            request
                                .capture
                                .required_keys
                                .extend(selection.required_keys.iter().cloned());
                        }
                    }
                    queries.push(query);
                }
                if let Some(root) = reservation.capture_ready_snapshot()? {
                    facts.push(Arc::new(
                        runtime
                            .prepare_external_input_facts(
                                reservation,
                                root.state.clone(),
                                root.episodes.clone(),
                                root.input_evidence.clone(),
                            )
                            .await?,
                    ));
                }
            }
            if let (Some(inventory), Some(root)) = (
                held_inventory.as_ref(),
                reservation.capture_ready_snapshot()?,
            ) {
                if let Some(tasks) = root.tasks.as_ref() {
                    let selected_scope =
                        crate::runtime::resident_input_evidence::InputEvidenceScope {
                            agent_id: reservation.agent_id().to_owned(),
                            identity_incarnation: reservation.identity_incarnation(),
                        };
                    let count = inventory
                        .rows()
                        .iter()
                        .filter(|row| row.task.scope == selected_scope)
                        .count();
                    if count > 0 {
                        let _slots = crate::runtime_db::command_task_terminal::charge_material(
                            scope,
                            count
                                .checked_mul(
                                    std::mem::size_of::<
                                        &Arc<crate::runtime::resident_task::QualifiedTaskFact>,
                                    >() + 128,
                                )
                                .and_then(|n| n.checked_mul(8))
                                .context("child Status borrowed slots overflow")?,
                            count
                                .checked_mul(8)
                                .context("child Status slots count overflow")?,
                            &request,
                            ledger,
                        )?;
                        let mut records = Vec::with_capacity(count);
                        for row in inventory
                            .rows()
                            .iter()
                            .filter(|row| row.task.scope == selected_scope)
                        {
                            if let Some(record) = tasks.original_status_fact(&row.task)? {
                                records.push(record);
                            }
                        }
                        if !records.is_empty() {
                            selections.push(OriginalChildStatusSelection::from_captured(
                                &selected_scope,
                                tasks.coverage().revision,
                                &records,
                                inventory,
                                &request,
                                ledger,
                            )?);
                        }
                    }
                }
            }
        }
        if pending.is_none() {
            let inventory = held_inventory
                .take()
                .context("child discovery inventory missing")?;
            let prepared = host.runtime_db().transaction_once(|tx| {
                PreparedInputTransition::prepare_original_child_terminal_tx(
                    tx,
                    source,
                    frozen,
                    inventory,
                    selections,
                    Some(actor_modes),
                    &request,
                    ledger,
                )
            })?;
            let primary = reservations
                .iter()
                .find(|r| {
                    r.agent_id() == scope.agent_id
                        && r.identity_incarnation() == scope.identity_incarnation
                })
                .context("child original parent reservation missing")?;
            let prepared = crate::runtime::attach_original_child_memory_to_owner(
                host.runtime_db(),
                ledger,
                primary,
                prepared,
                Utc::now(),
            )?;
            *pending = Some(Arc::new(prepared));
        }
        let exact = pending
            .as_deref()
            .context("child original phase missing")?
            .descriptor()
            .scopes();
        if exact.len() != reservations.len()
            || !exact.iter().all(|scope| {
                reservations.iter().any(|held| {
                    held.agent_id() == scope.agent_id
                        && held.identity_incarnation() == scope.identity_incarnation
                })
            })
        {
            // Preparation may prove NoOp/NoState and omit its proposed deletion
            // child. No attempt exists yet: reacquire the exact frozen subset.
            drop(request);
            drop(queries);
            drop(facts);
            drop(reservations);
            drop(held_group);
            return Box::pin(
                self.commit_original_child_task_terminal(source, frozen, pending, base),
            )
            .await;
        }
        let result = crate::runtime::execute_original_child_terminal(
            host.runtime_db(),
            &mut reservations,
            source,
            pending.as_deref().context("child original phase missing")?,
            &request,
            ledger,
            &host.runtime_context_config(),
            &RuntimeModelCatalog::from_config(&host.config()),
            facts,
            &queries,
        )?;
        host.reconcile_external_mutations(&mut reservations).await?;
        drop(held_group);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        child_observer_domains_for_test, continue_child_monitor_for_test,
        prepare_child_observer_for_test, register_child_monitor_for_test,
        write_child_task_for_test,
    };
    use crate::runtime_db::agent_deletion_cleanup::{
        export_private_history, purge_private_history, settle_execution,
    };
    use crate::types::*;
    fn observer_task(agent: &str, id: &str, child: &str, at: chrono::DateTime<Utc>) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: agent.into(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("global child observer".into()),
            detail: Some(serde_json::json!({"child_agent_id":child,"workspace_mode":"inherit"})),
            recovery: None,
        }
    }
    fn read_actual_delegation(
        db: &crate::runtime_db::RuntimeDb,
        id: &str,
    ) -> Result<WorkItemDelegationRecord> {
        let raw: String = db.connection()?.query_row(
            "SELECT payload_json FROM work_item_delegations WHERE delegation_id=?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    }
    #[tokio::test]
    async fn child_global_delegation_terminal_updates_all_observed_owners_and_replays_once(
    ) -> Result<()> {
        for fault in [
            None,
            Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit),
            Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate),
        ] {
            let (_home, host, parent) =
                crate::runtime::tests::support::host_backed_test_runtime().await;
            let parent_id = parent.agent_state().await?.id;
            host.create_named_agent("child-status-target", None).await?;
            host.create_named_agent("child-status-observer", None)
                .await?;
            let other = host.get_or_create_agent("child-status-observer").await?;
            let at = Utc::now();
            for (id, state, delta) in [
                ("global-open", WorkItemDelegationState::Open, 3),
                (
                    "global-completed-head",
                    WorkItemDelegationState::Completed,
                    1,
                ),
            ] {
                host.runtime_db()
                    .work_item_delegations()
                    .upsert(&WorkItemDelegationRecord {
                        delegation_id: id.into(),
                        parent_agent_id: "independent-source-owner".into(),
                        parent_work_item_id: format!("work:{id}"),
                        child_agent_id: "child-status-target".into(),
                        child_work_item_id: "child-work".into(),
                        state,
                        result_summary: None,
                        created_at: at - chrono::Duration::seconds(delta),
                        updated_at: at - chrono::Duration::seconds(delta),
                    })?;
            }
            let observed = observer_task(
                "child-status-observer",
                "task:global-observer",
                "child-status-target",
                at,
            );
            write_child_task_for_test(&other, &observed).await?;
            prepare_child_observer_for_test(&other, &observed).await?;
            let before = child_observer_domains_for_test(&other, &observed.id)?;
            let mut task = observer_task(
                &parent_id,
                "task:actual-global-producer",
                "child-status-target",
                at,
            );
            task.kind = TaskKind::ActorInvocation;
            task.detail = Some(
                serde_json::json!({"target_agent_id":"child-status-target","created_new_subagent":false}),
            );
            task.recovery = Some(TaskRecoverySpec::AgentInvocation {
                summary: "actual existing target monitor".into(),
                prompt: "accepted invocation".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                target_agent_id: Some("child-status-target".into()),
                created_new_subagent: false,
                workspace_mode: ChildAgentWorkspaceMode::Inherit,
                lifecycle_disposition: AgentLifecycleDisposition::Retain,
            });
            write_child_task_for_test(&parent, &task).await?;
            let mut run = register_child_monitor_for_test(
                &parent,
                &task,
                Some("child-status-target"),
                None,
                fault,
                false,
            )
            .await?;
            run.start();
            let answer = run.finish().await?;
            assert_eq!(
                answer.is_err(),
                fault.is_some(),
                "actual first terminal result for {fault:?}: {answer:?}"
            );
            if fault == Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit) {
                let source = read_actual_delegation(host.runtime_db(), "global-open")?;
                assert_eq!(source.state, WorkItemDelegationState::Open);
            }
            if fault.is_some() {
                continue_child_monitor_for_test(&parent, run.source.clone()).await?;
            }
            let after = child_observer_domains_for_test(&other, &observed.id)?;
            assert_eq!(
                before.0, after.0,
                "source-only metadata keeps State/Episode/Input/Work Arcs"
            );
            assert_eq!(
                before.1, after.1,
                "metadata does not clone or replace Task body"
            );
            assert_eq!(after.2, before.2 + 1);
            let receipts:(u64,u64)=host.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1),(SELECT COUNT(*) FROM audit_events WHERE kind='work_item_delegation_completed')",
                [format!("child-terminal:{}",run.source.address().lease_id)],|row|Ok((row.get(0)?,row.get(1)?)))?;
            assert_eq!(receipts, (1, 1));
            let result_raw:String=host.runtime_db().connection()?.query_row(
                "SELECT payload_json FROM messages WHERE kind='task_result' AND json_extract(payload_json,'$.task_id')=?1",[&task.id],|row|row.get(0))?;
            let result: MessageEnvelope = serde_json::from_str(&result_raw)?;
            assert_eq!(
                result.work_item_id, task.work_item_id,
                "global delegation metadata is not the original Task/Message Work binding"
            );
            assert_eq!(
                result.metadata.as_ref().unwrap()["delegation_parent_work_item_id"],
                "work:global-open"
            );
            assert_eq!(
                result.metadata.as_ref().unwrap()["delegation_child_work_item_id"],
                "child-work"
            );
            let record = read_actual_delegation(host.runtime_db(), "global-open")?;
            assert_eq!(record.state, WorkItemDelegationState::Completed);
            assert_eq!(
                record.result_summary.as_deref(),
                Some("actual child result")
            );
            host.shutdown().await?;
        }
        Ok(())
    }
    #[tokio::test]
    async fn child_noop_and_no_state_reacquire_exact_frozen_subset_before_attempt_and_retry(
    ) -> Result<()> {
        for (mode, fault) in [
            (
                "noop",
                crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit,
            ),
            (
                "noop",
                crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
            ),
            (
                "no_state",
                crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit,
            ),
            (
                "no_state",
                crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
            ),
        ] {
            let (_home, host, parent) =
                crate::runtime::tests::support::host_backed_test_runtime().await;
            let parent_id = parent.agent_state().await?.id;
            let at = Utc::now();
            let mut task = observer_task(&parent_id, "task:subset-producer", "pending-child", at);
            task.detail = Some(
                serde_json::json!({"workspace_mode":"inherit","lifecycle_disposition":"delete_on_terminal"}),
            );
            task.recovery = Some(TaskRecoverySpec::ChildAgentTask {
                summary: "subset producer".into(),
                prompt: "accepted".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: ChildAgentWorkspaceMode::Inherit,
                lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
            });
            write_child_task_for_test(&parent, &task).await?;
            let home = host.agent_data_dir(&parent_id);
            let child = host
                .create_child_identity(&parent_id, &task, None, &home)
                .await?;
            host.create_named_agent("subset-status-observer", None)
                .await?;
            let observer = host.get_or_create_agent("subset-status-observer").await?;
            host.runtime_db()
                .work_item_delegations()
                .upsert(&WorkItemDelegationRecord {
                    delegation_id: "subset-original-open".into(),
                    parent_agent_id: "original-independent-parent".into(),
                    parent_work_item_id: "original-work".into(),
                    child_agent_id: child.agent_id.clone(),
                    child_work_item_id: "original-child-work".into(),
                    state: WorkItemDelegationState::Open,
                    result_summary: None,
                    created_at: at - chrono::Duration::seconds(1),
                    updated_at: at - chrono::Duration::seconds(1),
                })?;
            let observed = observer_task(
                "subset-status-observer",
                "subset-observed-task",
                &child.agent_id,
                at,
            );
            write_child_task_for_test(&observer, &observed).await?;
            prepare_child_observer_for_test(&observer, &observed).await?;
            let original_domains = child_observer_domains_for_test(&observer, &observed.id)?;
            let original_task_domain = observer.child_task_domain_pointer_for_test()?;
            let deletion=crate::runtime_db::repositories::AgentDeletionRequest{agent_id:child.agent_id.clone(),
                admission:crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild{parent_agent_id:parent_id.clone(),task_id:task.id.clone()},requested_by:"task_terminal".into(),cascade_private_children:true};
            let mut run = register_child_monitor_for_test(
                &parent,
                &task,
                Some(&child.agent_id),
                Some(deletion),
                Some(fault),
                false,
            )
            .await?;
            if mode == "noop" {
                let mut failed = task.clone();
                failed.status = TaskStatus::Failed;
                failed.updated_at = Utc::now();
                write_child_task_for_test(&parent, &failed).await?;
            } else {
                host.runtime_db().transaction(|tx| {
                    tx.execute("DELETE FROM agent_states WHERE agent_id=?1", [&parent_id])?;
                    Ok(())
                })?;
            }
            run.start();
            let actual = run.finish().await?;
            assert!(
                actual.is_err(),
                "actual {mode}/{fault:?} terminal result: {actual:?}"
            );
            let before: u64 = host.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
                [format!("child-terminal:{}", run.source.address().lease_id)],
                |row| row.get(0),
            )?;
            assert_eq!(
                before,
                u64::from(
                    fault
                        == crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate
                ),
                "only actual lost ACK retains the original whole receipt"
            );
            continue_child_monitor_for_test(&parent, run.source.clone()).await?;
            let raw: String = host.runtime_db().connection()?.query_row(
                "SELECT outcome_json FROM input_mutation_receipts WHERE command_nonce=?1",
                [format!("child-terminal:{}", run.source.address().lease_id)],
                |row| row.get(0),
            )?;
            let outcome: crate::runtime_db::input_mutation_receipts::InputMutationOutcome =
                serde_json::from_str(&raw)?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::ChildTaskTerminal{ack}=outcome.result else{anyhow::bail!("not child ACK")};
            assert!(ack.deletion.is_none());
            assert_eq!(ack.status_scopes.len(), 2);
            assert!(ack
                .status_scopes
                .iter()
                .any(|row| row.scope == run.source.address().scope));
            assert!(!ack
                .status_scopes
                .iter()
                .any(|row| row.scope.agent_id == child.agent_id));
            let current_domains = child_observer_domains_for_test(&observer, &observed.id)?;
            assert_eq!(original_domains.0, current_domains.0);
            assert_eq!(original_domains.1, current_domains.1);
            assert_eq!(
                current_domains.2, original_domains.2,
                "the same latest delegation ID/Work fields leave the observed Status head unchanged"
            );
            assert_eq!(
                observer.child_task_domain_pointer_for_test()?,
                original_task_domain,
                "same-group recovery preserves the complete original Task domain for a projection no-op"
            );
            let related = ack
                .status_scopes
                .iter()
                .find(|row| row.scope.agent_id == observed.agent_id)
                .context("original related scope ACK missing")?;
            assert_eq!(
                related.mode,
                crate::runtime_db::child_task_terminal::ChildTaskTerminalInstallMode::Current
            );
            assert!(
                related.base_revision.is_none() && related.revision.is_none(),
                "source-only completion cannot manufacture a related Task revision"
            );
            let actual_source = read_actual_delegation(host.runtime_db(), "subset-original-open")?;
            assert_eq!(actual_source.state, WorkItemDelegationState::Completed);
            assert_eq!(
                actual_source.result_summary.as_deref(),
                Some("actual child result")
            );
            let audits: u64 = host.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1",
                [format!(
                    "audit:child-delegation:{}",
                    run.source.address().lease_id
                )],
                |row| row.get(0),
            )?;
            assert_eq!(
                audits, 1,
                "the original Source/audit wholeACK still commits and replays once"
            );
            if mode == "noop" {
                assert!(matches!(
                    ack.result,
                    crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::NoOp
                ));
                assert_eq!(
                    host.runtime_db().tasks().latest(&task.id)?.unwrap().status,
                    TaskStatus::Failed
                );
            } else {
                assert!(matches!(ack.result,crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Unavailable{reason:crate::runtime_db::child_task_terminal::ChildTaskTerminalUnavailable::OwnerNoState}));
                assert_eq!(
                    host.runtime_db().tasks().latest(&task.id)?.unwrap().status,
                    TaskStatus::Completed
                );
            }
            let actual = host
                .runtime_db()
                .agent_identities()
                .latest(&child.agent_id)?
                .unwrap();
            assert_eq!(actual.status, AgentRegistryStatus::Active);
            let writes:(u64,u64)=host.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM agent_deletion_jobs WHERE agent_id=?1),(SELECT COUNT(*) FROM messages WHERE kind='task_result')",[&child.agent_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
            assert_eq!(writes, (0, 0));
            assert!(run.source.is_durable_finished());
            // NoState intentionally cannot run an ordinary State reconciliation.
            if mode == "noop" {
                host.shutdown().await?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn child_changed_deletion_incarnation_falls_back_before_attempt_without_recomputing_terminal(
    ) -> Result<()> {
        let (_home, host, parent) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        let parent_id = parent.agent_state().await?.id;
        let at = Utc::now();
        let mut task = observer_task(&parent_id, "task:changed-child-deletion", "unbound", at);
        task.detail = Some(serde_json::json!({"workspace_mode":"inherit"}));
        task.recovery = Some(TaskRecoverySpec::ChildAgentTask {
            summary: "original child deletion".into(),
            prompt: "accepted".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
        });
        write_child_task_for_test(&parent, &task).await?;
        let child = host
            .create_child_identity(&parent_id, &task, None, &host.agent_data_dir(&parent_id))
            .await?;
        let deletion = crate::runtime_db::repositories::AgentDeletionRequest {
            agent_id: child.agent_id.clone(),
            admission:
                crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild {
                    parent_agent_id: parent_id.clone(),
                    task_id: task.id.clone(),
                },
            requested_by: "task_terminal".into(),
            cascade_private_children: false,
        };
        let mut run = register_child_monitor_for_test(
            &parent,
            &task,
            Some(&child.agent_id),
            Some(deletion),
            None,
            false,
        )
        .await?;
        host.runtime_db().transaction(|tx|{tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&child.agent_id])?;Ok(())})?;
        run.start();
        run.finish().await??;
        assert!(!run.source.original_deletion_requested());
        assert!(run.source.is_durable_finished());
        let actual = host.runtime_db().tasks().latest(&task.id)?.unwrap();
        assert_eq!(actual.status, TaskStatus::Completed);
        assert_eq!(
            actual.detail.as_ref().unwrap()["output_summary"],
            "actual child result"
        );
        let counts:(u64,u64,u64)=host.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM agent_deletion_jobs WHERE agent_id=?1),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?2),(SELECT COUNT(*) FROM messages WHERE kind='task_result')",rusqlite::params![&child.agent_id,format!("child-terminal:{}",run.source.address().lease_id)],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert_eq!(counts, (0, 1, 1));
        assert_eq!(
            host.runtime_db()
                .agent_identities()
                .latest(&child.agent_id)?
                .unwrap()
                .incarnation,
            2
        );
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn child_deletion_delivery_payload_cannot_escape_the_original_budgeted_pool() -> Result<()>
    {
        let (_home, host, parent) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        let parent_id = parent.agent_state().await?.id;
        let at = Utc::now();
        let mut task = observer_task(&parent_id, "task:delivery-closure", "unbound", at);
        task.detail = Some(serde_json::json!({"workspace_mode":"inherit"}));
        task.recovery = Some(TaskRecoverySpec::ChildAgentTask {
            summary: "delivery closure".into(),
            prompt: "accepted".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
        });
        write_child_task_for_test(&parent, &task).await?;
        let child = host
            .create_child_identity(&parent_id, &task, None, &host.agent_data_dir(&parent_id))
            .await?;
        fn delivery(target: &str, parent: &str, key: &str) -> Result<AgentMessageDeliveryRecord> {
            let prepared = crate::runtime::AgentMessageDeliveryService::prepare(
                AgentMessageSendRequest {
                    target_agent_id: target.into(),
                    content: MessageBody::Text {
                        text: "original accepted delivery".into(),
                    },
                    client_idempotency_key: key.into(),
                    correlation_id: None,
                    causation_id: None,
                    requested_priority: None,
                    forward: false,
                    reply_context_id: None,
                },
                AgentMessageCallerContext {
                    caller_principal: format!("agent:{parent}"),
                    caller_agent_id: Some(parent.into()),
                    principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                    route: "agent_invocation".into(),
                    origin: MessageOrigin::Task {
                        task_id: "task:delivery-closure".into(),
                    },
                    authority_class: AuthorityClass::RuntimeInstruction,
                    delivery_surface: MessageDeliverySurface::RuntimeSystem,
                    admission_context: AdmissionContext::RuntimeOwned,
                    current_turn_id: None,
                    current_task_id: Some("task:delivery-closure".into()),
                    current_work_item_id: None,
                    inherited_routing_context: None,
                    routing_activation_id: None,
                },
            )?;
            let mut record = prepared.record;
            record.outcome = AgentMessageDeliveryOutcome::Accepted;
            record.state = AgentMessageDeliveryState::Queued;
            record.accepted_at = Some(record.created_at);
            Ok(record)
        }
        let selected = delivery(&child.agent_id, &parent_id, "selected")?;
        let foreign = delivery("other-delivery-target", &parent_id, "foreign")?;
        host.runtime_db().transaction(|tx| {
            crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx,&selected)?;
            crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx,&foreign)?;
            tx.execute("UPDATE agent_message_deliveries SET payload_json=json_set(payload_json,'$.message_id',?2) WHERE delivery_id=?1",rusqlite::params![selected.delivery_id,foreign.message_id])?;
            tx.execute("UPDATE agent_message_deliveries SET payload_json=?2 WHERE delivery_id=?1",rusqlite::params![foreign.delivery_id,format!("{{bad{}","x".repeat(1024*1024))])?;Ok(())
        })?;
        let deletion = crate::runtime_db::repositories::AgentDeletionRequest {
            agent_id: child.agent_id.clone(),
            admission:
                crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild {
                    parent_agent_id: parent_id.clone(),
                    task_id: task.id.clone(),
                },
            requested_by: "task_terminal".into(),
            cascade_private_children: false,
        };
        let mut run = register_child_monitor_for_test(
            &parent,
            &task,
            Some(&child.agent_id),
            Some(deletion),
            None,
            false,
        )
        .await?;
        run.start();
        let first = run
            .finish()
            .await?
            .expect_err("damaged delivery closure must fail before the second body acquisition");
        assert!(
            format!("{first:#}")
                .contains("original child deletion delivery normalized projection differs"),
            "{first:#}"
        );
        let parent_loop = parent.pause_run_loop_for_monitor_test().await;
        let resource_refresh = parent.hold_resource_refresh_for_test().await?;
        let before = host.inner.input_evidence_budget.report()?.observed_bytes;
        let retry = continue_child_monitor_for_test(&parent, run.source.clone())
            .await
            .expect_err("same original damaged closure remains rejected");
        assert!(
            format!("{retry:#}")
                .contains("original child deletion delivery normalized projection differs"),
            "{retry:#}"
        );
        assert_eq!(
            before,
            host.inner.input_evidence_budget.report()?.observed_bytes,
            "failed temporary material guards release while original responsibility remains"
        );
        assert!(!run.source.is_durable_finished());
        assert_eq!(
            host.runtime_db().tasks().latest(&task.id)?.unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            host.runtime_db()
                .agent_identities()
                .latest(&child.agent_id)?
                .unwrap()
                .status,
            AgentRegistryStatus::Active
        );
        let actual:(u64,u64,String,String)=host.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1),(SELECT COUNT(*) FROM agent_deletion_jobs WHERE agent_id=?2),(SELECT state FROM agent_message_deliveries WHERE delivery_id=?3),(SELECT state FROM agent_message_deliveries WHERE delivery_id=?4)",
            rusqlite::params![format!("child-terminal:{}",run.source.address().lease_id),child.agent_id,selected.delivery_id,foreign.delivery_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(actual, (0, 0, "queued".into(), "queued".into()));
        drop((resource_refresh, parent_loop));
        // Restore only the corrupted payloads. The original whole operation can
        // then perform its intended deletion and never touches the foreign row.
        host.runtime_db().transaction(|tx| {
            tx.execute(
                "UPDATE agent_message_deliveries SET payload_json=?2 WHERE delivery_id=?1",
                rusqlite::params![selected.delivery_id, serde_json::to_string(&selected)?],
            )?;
            tx.execute(
                "UPDATE agent_message_deliveries SET payload_json=?2 WHERE delivery_id=?1",
                rusqlite::params![foreign.delivery_id, serde_json::to_string(&foreign)?],
            )?;
            Ok(())
        })?;
        continue_child_monitor_for_test(&parent, run.source.clone()).await?;
        assert!(run.source.is_durable_finished());
        assert_eq!(
            host.runtime_db()
                .agent_message_deliveries()
                .latest(&foreign.delivery_id)?
                .unwrap(),
            foreign
        );
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn original_child_terminal_uses_later_qualified_running_revision_and_keeps_observed_clock(
    ) -> Result<()> {
        for fault in [
            None,
            Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate),
        ] {
            let (_home, host, parent) =
                crate::runtime::tests::support::host_backed_test_runtime().await;
            let parent_id = parent.agent_state().await?.id;
            let at = Utc::now();
            let mut task =
                observer_task(&parent_id, "task:terminal-running-revision", "unbound", at);
            task.detail = Some(serde_json::json!({"workspace_mode":"inherit"}));
            task.recovery = Some(TaskRecoverySpec::ChildAgentTask {
                summary: "original short child".into(),
                prompt: "accepted original child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: ChildAgentWorkspaceMode::Inherit,
                lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
            });
            write_child_task_for_test(&parent, &task).await?;
            let child = host
                .create_child_identity(&parent_id, &task, None, &host.agent_data_dir(&parent_id))
                .await?;
            task.detail.as_mut().unwrap()["child_agent_id"] = serde_json::json!(child.agent_id);
            write_child_task_for_test(&parent, &task).await?;
            let deletion=crate::runtime_db::repositories::AgentDeletionRequest {
                agent_id:child.agent_id.clone(),
                admission:crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild {
                    parent_agent_id:parent_id.clone(),task_id:task.id.clone(),
                },requested_by:"original_child_revision_fixture".into(),cascade_private_children:false,
            };
            let mut run = register_child_monitor_for_test(
                &parent,
                &task,
                Some(&child.agent_id),
                Some(deletion.clone()),
                fault,
                false,
            )
            .await?;
            let (observed, message) =
                run.freeze_completed_observation_for_test(&task, &child.agent_id, deletion, fault)?;
            let frozen = run
                .source
                .frozen_terminal()?
                .context("original frozen terminal absent")?;
            let frozen_digest = frozen.digest()?;
            // A later genuine Task ACK, not a raw row or a future clock.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while Utc::now().timestamp_millis() <= observed.updated_at.timestamp_millis() {
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "fixture clock did not advance"
                );
                tokio::task::yield_now().await;
            }
            let mut running = task.clone();
            running.updated_at = Utc::now();
            write_child_task_for_test(&parent, &running).await?;
            assert!(running.updated_at > observed.updated_at);
            assert_eq!(
                host.runtime_db().tasks().latest(&task.id)?,
                Some(running.clone())
            );
            run.start();
            let first = run.finish().await?;
            assert_eq!(
                first.is_err(),
                fault.is_some(),
                "actual first terminal result: {first:?}"
            );
            let nonce = format!("child-terminal:{}", run.source.address().lease_id);
            let before: String = host.runtime_db().connection()?.query_row(
                "SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",
                [&nonce],
                |row| row.get(0),
            )?;
            if fault.is_some() {
                assert_eq!(
                    run.source
                        .frozen_terminal()?
                        .context("lost ACK discarded observation")?
                        .digest()?,
                    frozen_digest
                );
                continue_child_monitor_for_test(&parent, run.source.clone()).await?;
            }
            let actual = host
                .runtime_db()
                .tasks()
                .latest(&task.id)?
                .context("actual terminal Task absent")?;
            assert_eq!(actual.status, TaskStatus::Completed);
            assert_eq!(actual.created_at, task.created_at);
            assert_eq!(actual.updated_at, running.updated_at);
            assert_eq!(
                actual.parent_message_id.as_deref(),
                Some(message.id.as_str())
            );
            let stored = host
                .runtime_db()
                .messages()
                .by_id(Some(&parent_id), &message.id)?
                .context("original terminal Message absent")?;
            assert_eq!(stored.id, message.id);
            assert_eq!(stored.created_at, message.created_at);
            assert_eq!(stored.body, message.body);
            assert_eq!(stored.turn_id, message.turn_id);
            let (after,outcome):(String,String)=host.runtime_db().connection()?.query_row(
                "SELECT prepared_json,outcome_json FROM input_mutation_receipts WHERE command_nonce=?1",[&nonce],|row|Ok((row.get(0)?,row.get(1)?)))?;
            assert_eq!(
                before, after,
                "lost ACK must replay the exact original Prepared descriptor"
            );
            let prepared: crate::runtime_db::input_mutation_receipts::PreparedInputMutation =
                serde_json::from_str(&after)?;
            assert_eq!(prepared.prepared_at(), observed.updated_at);
            let outcome: crate::runtime_db::input_mutation_receipts::InputMutationOutcome =
                serde_json::from_str(&outcome)?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::ChildTaskTerminal { ack }=outcome.result else {anyhow::bail!("wrong whole terminal family")};
            assert!(ack.task_changed);
            assert!(ack.deletion.is_some());
            assert!(
                matches!(ack.result,crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Delivered {ref message_id} if message_id==&message.id)
            );
            assert!(
                run.source.original_deletion_requested(),
                "valid original deletion must not enter fallback"
            );
            let job = host
                .runtime_db()
                .agent_deletions()
                .latest_for_agent(&child.agent_id)?
                .context("original deletion job absent")?;
            assert_eq!(
                ack.deletion.as_ref().unwrap().job.deletion_id,
                job.deletion_id
            );
            assert_eq!(job.mode, AgentDeletionMode::Delete);
            let counts:(u64,u64,u64)=host.runtime_db().connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1),(SELECT COUNT(*) FROM messages WHERE evidence_id=?2),(SELECT COUNT(*) FROM agent_deletion_jobs WHERE agent_id=?3)",
                rusqlite::params![nonce,message.id,child.agent_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            assert_eq!(counts, (1, 1, 1));
            continue_child_monitor_for_test(&parent, run.source.clone()).await?;
            assert_eq!(
                host.runtime_db()
                    .agent_deletions()
                    .latest_for_agent(&child.agent_id)?,
                Some(job)
            );
            host.shutdown().await?;
        }
        Ok(())
    }

    struct ChildTerminalReceiptFixture {
        _home: tempfile::TempDir,
        host: RuntimeHost,
        parent_id: String,
        task: TaskRecord,
        child: crate::types::AgentIdentityRecord,
        nonce: String,
        receipt: (String, String, String),
    }

    /// Real original ChildTaskTerminal writer: a whole parent+child ACK with a
    /// live terminal-child deletion. No receipt or checksum is fabricated.
    async fn child_terminal_receipt_fixture() -> Result<ChildTerminalReceiptFixture> {
        child_terminal_receipt_fixture_for_target(false).await
    }

    async fn child_terminal_receipt_fixture_for_target(
        typed: bool,
    ) -> Result<ChildTerminalReceiptFixture> {
        let (_home, host, parent) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        let parent_id = parent.agent_state().await?.id;
        let at = Utc::now();
        let mut task = TaskRecord {
            id: "task:terminal-whole-receipt-cleanup".into(),
            agent_id: parent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("original whole-receipt cleanup child".into()),
            detail: Some(serde_json::json!({"workspace_mode":"inherit"})),
            recovery: Some(TaskRecoverySpec::ChildAgentTask {
                summary: "original whole-receipt cleanup child".into(),
                prompt: "accepted whole-receipt cleanup".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: ChildAgentWorkspaceMode::Inherit,
                lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
            }),
        };
        write_child_task_for_test(&parent, &task).await?;
        let child = host
            .create_child_identity(&parent_id, &task, None, &host.agent_data_dir(&parent_id))
            .await?;
        task.detail.as_mut().unwrap()["child_agent_id"] = serde_json::json!(child.agent_id);
        if typed {
            task.detail.as_mut().unwrap()["target"] = serde_json::json!({
                "kind": "child_agent",
                "agent_id": child.agent_id,
                "incarnation": child.incarnation,
            });
        } else {
            task.detail.as_mut().unwrap()["target_identity_incarnation"] =
                serde_json::json!(child.incarnation);
        }
        task.updated_at = Utc::now();
        write_child_task_for_test(&parent, &task).await?;
        let deletion = crate::runtime_db::repositories::AgentDeletionRequest {
            agent_id: child.agent_id.clone(),
            admission:
                crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild {
                    parent_agent_id: parent_id.clone(),
                    task_id: task.id.clone(),
                },
            requested_by: "original_child_terminal_fixture".into(),
            cascade_private_children: false,
        };
        let mut run = register_child_monitor_for_test(
            &parent,
            &task,
            Some(&child.agent_id),
            Some(deletion),
            None,
            false,
        )
        .await?;
        let nonce = format!("child-terminal:{}", run.source.address().lease_id);
        run.start();
        run.finish().await??;
        let db = host.runtime_db();
        let job = db
            .agent_deletions()
            .latest_for_agent(&child.agent_id)?
            .context("terminal child deletion job absent")?;
        assert_eq!(job.mode, AgentDeletionMode::Delete);
        assert_eq!(
            job.deletion_id,
            format!("deletion:child-terminal:{}", run.source.address().lease_id)
        );
        assert_eq!(
            db.agent_identities()
                .latest(&child.agent_id)?
                .context("deleting child identity absent")?
                .status,
            AgentRegistryStatus::Deleting
        );
        let receipt = read_child_terminal_receipt(db, &nonce)?;
        let outcome: crate::runtime_db::input_mutation_receipts::InputMutationOutcome =
            serde_json::from_str(&receipt.1)?;
        let crate::runtime_db::input_mutation_receipts::InputMutationResult::ChildTaskTerminal {
            ack,
        } = outcome.result
        else {
            anyhow::bail!("whole terminal receipt family differs");
        };
        assert!(
            ack.deletion.is_some(),
            "the real writer receipt must carry its terminal deletion"
        );
        assert_eq!(
            ack.status_scopes.len(),
            2,
            "the whole terminal ACK owns the exact parent+child scope group"
        );
        Ok(ChildTerminalReceiptFixture {
            _home,
            host,
            parent_id,
            task,
            child,
            nonce,
            receipt,
        })
    }

    fn read_child_terminal_receipt(
        db: &crate::runtime_db::RuntimeDb,
        nonce: &str,
    ) -> Result<(String, String, String)> {
        Ok(db.connection()?.query_row(
            "SELECT prepared_json,outcome_json,scopes_json FROM input_mutation_receipts
             WHERE command_nonce=?1",
            [nonce],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }

    fn parent_task_row(
        db: &crate::runtime_db::RuntimeDb,
        task_id: &str,
    ) -> Result<(String, String, Option<String>)> {
        Ok(db.connection()?.query_row(
            "SELECT payload_json,status,child_agent_id FROM tasks WHERE task_id=?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }

    #[tokio::test]
    async fn approved_child_purges_whole_terminal_receipt_and_parent_task_survives() -> Result<()> {
        let f = child_terminal_receipt_fixture().await?;
        let db = f.host.runtime_db();
        // Positive gate: the approved child exports the whole shared receipt.
        let export = export_private_history(db, &f.child.agent_id, f.child.incarnation)?;
        let receipts = export["tables"]["input_mutation_receipts"]
            .as_array()
            .context("export omitted the shared command receipt")?;
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0]["command_nonce"].as_str(),
            Some(f.nonce.as_str())
        );
        let parent_task_before = parent_task_row(db, &f.task.id)?;
        // Parent-first cleanup stays fail-closed on the whole-receipt gate.
        let parent_identity = db
            .agent_identities()
            .latest(&f.parent_id)?
            .context("parent identity absent")?;
        let _ = db.agent_deletions().begin(
            &f.parent_id,
            parent_identity.revision,
            "parent_first_probe",
            false,
        )?;
        let parent_first =
            export_private_history(db, &f.parent_id, parent_identity.incarnation).unwrap_err();
        assert!(
            format!("{parent_first:#}")
                .contains("child receipt is not the exact parent/child group"),
            "parent-first probe: {parent_first:#}"
        );
        assert_eq!(read_child_terminal_receipt(db, &f.nonce)?, f.receipt);
        // The approved child owns the complete receipt purge.
        settle_execution(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
        purge_private_history(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
        let remaining: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [&f.nonce],
            |row| row.get(0),
        )?;
        assert_eq!(remaining, 0, "the whole shared receipt must be purged");
        assert_eq!(
            parent_task_row(db, &f.task.id)?,
            parent_task_before,
            "the canonical parent Task must survive the child receipt purge"
        );
        assert_eq!(
            db.agent_identities()
                .latest(&f.parent_id)?
                .context("parent identity absent")?
                .incarnation,
            parent_identity.incarnation
        );
        let job = db
            .agent_deletions()
            .latest_for_agent(&f.child.agent_id)?
            .context("child deletion job absent")?;
        let (_, completed) = db.agent_deletions().finalize(&job)?;
        assert_eq!(completed.status, AgentDeletionStatus::Completed);
        // After the child purge the shared gate no longer pins the parent side.
        match export_private_history(db, &f.parent_id, parent_identity.incarnation) {
            Ok(_) => {}
            Err(error) => assert!(
                !format!("{error:#}").contains("shared command history"),
                "parent export still pinned after the child purge: {error:#}"
            ),
        }
        f.host.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn approved_child_purges_terminal_receipt_with_typed_target() -> Result<()> {
        let f = child_terminal_receipt_fixture_for_target(true).await?;
        let db = f.host.runtime_db();
        let before = parent_task_row(db, &f.task.id)?;
        let task: TaskRecord = serde_json::from_str(&before.0)?;
        assert_eq!(
            task.target().unwrap().incarnation,
            Some(f.child.incarnation)
        );
        assert!(task
            .detail
            .as_ref()
            .unwrap()
            .get("target_identity_incarnation")
            .is_none());
        export_private_history(db, &f.child.agent_id, f.child.incarnation)?;
        settle_execution(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
        purge_private_history(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
        assert_eq!(parent_task_row(db, &f.task.id)?, before);
        let remaining: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [&f.nonce],
            |row| row.get(0),
        )?;
        assert_eq!(remaining, 0);
        f.host.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn child_terminal_receipt_gate_fails_closed_on_tampered_shared_history() -> Result<()> {
        for scenario in [
            "receipt-descriptor",
            "receipt-result-checksum",
            "receipt-third-scope",
            "parent-incarnation",
            "deletion-job-id",
            "canonical-task-nonterminal",
            "canonical-task-child-link",
            "typed-target-unknown-incarnation",
            "typed-target-malformed-incarnation",
            "typed-target-wrong-incarnation",
            "typed-target-wrong-child",
            "typed-target-wrong-kind",
            "typed-target-conflicting-legacy-incarnation",
        ] {
            let f = child_terminal_receipt_fixture().await?;
            let db = f.host.runtime_db();
            let nonce = f.nonce.clone();
            match scenario {
                "receipt-descriptor" => db.transaction(|tx| {
                    tx.execute(
                        "UPDATE input_mutation_receipts
                         SET prepared_json=replace(prepared_json,'child-terminal:','child-termina1:')
                         WHERE command_nonce=?1",
                        [&nonce],
                    )?;
                    Ok(())
                })?,
                "receipt-result-checksum" => db.transaction(|tx| {
                    tx.execute(
                        "UPDATE input_mutation_receipts
                         SET outcome_json=json_set(outcome_json,
                             '$.result.ack.original.admitted.key.id','task:checksum-mismatch')
                         WHERE command_nonce=?1",
                        [&nonce],
                    )?;
                    Ok(())
                })?,
                "receipt-third-scope" => db.transaction(|tx| {
                    tx.execute(
                        "UPDATE input_mutation_receipts
                         SET scopes_json=json_insert(scopes_json,'$[#]',
                             json_object('agent_id','third-shared-owner','identity_incarnation',1))
                         WHERE command_nonce=?1",
                        [&nonce],
                    )?;
                    Ok(())
                })?,
                "parent-incarnation" => db.transaction(|tx| {
                    tx.execute(
                        "UPDATE agent_identities SET payload_json=
                             json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",
                        [&f.parent_id],
                    )?;
                    Ok(())
                })?,
                "deletion-job-id" => db.transaction(|tx| {
                    tx.execute(
                        "UPDATE agent_deletion_jobs
                         SET deletion_id=deletion_id||'-mismatch' WHERE agent_id=?1",
                        [&f.child.agent_id],
                    )?;
                    Ok(())
                })?,
                "canonical-task-nonterminal" => db.transaction(|tx| {
                    tx.execute("UPDATE tasks SET status='running' WHERE task_id=?1", [&f.task.id])?;
                    Ok(())
                })?,
                "canonical-task-child-link" => db.transaction(|tx| {
                    tx.execute(
                        "UPDATE tasks SET child_agent_id='foreign-child' WHERE task_id=?1",
                        [&f.task.id],
                    )?;
                    Ok(())
                })?,
                scenario if scenario.starts_with("typed-target-") => db.transaction(|tx| {
                    let raw: String = tx.query_row(
                        "SELECT payload_json FROM tasks WHERE task_id=?1",
                        [&f.task.id],
                        |row| row.get(0),
                    )?;
                    let mut task: TaskRecord = serde_json::from_str(&raw)?;
                    let mut target = serde_json::json!({
                        "kind": "child_agent",
                        "agent_id": f.child.agent_id,
                        "incarnation": f.child.incarnation,
                    });
                    match scenario {
                        "typed-target-unknown-incarnation" => {
                            target.as_object_mut().unwrap().remove("incarnation");
                        }
                        "typed-target-malformed-incarnation" => {
                            target["incarnation"] = serde_json::json!("not-an-incarnation");
                        }
                        "typed-target-wrong-incarnation" => {
                            target["incarnation"] = serde_json::json!(f.child.incarnation + 1);
                        }
                        "typed-target-wrong-child" => {
                            target["agent_id"] = serde_json::json!("foreign-child");
                        }
                        "typed-target-wrong-kind" => {
                            target["kind"] = serde_json::json!("agent_message_wait");
                        }
                        "typed-target-conflicting-legacy-incarnation" => {
                            task.detail.as_mut().unwrap()["target_identity_incarnation"] =
                                serde_json::json!(f.child.incarnation + 1);
                        }
                        _ => unreachable!(),
                    }
                    task.detail.as_mut().unwrap()["target"] = target;
                    tx.execute(
                        "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
                        rusqlite::params![f.task.id, serde_json::to_string(&task)?],
                    )?;
                    Ok(())
                })?,
                other => anyhow::bail!("unknown tamper scenario {other}"),
            }
            settle_execution(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
            let error =
                purge_private_history(db, &f.child.agent_id, f.child.incarnation, Utc::now())
                    .unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("shared command history requires explicit ownership resolution"),
                "{scenario}: {message}"
            );
            let count: u64 = db.connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
                [&nonce],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "{scenario}: the shared receipt must survive");
            if !scenario.starts_with("receipt-") {
                assert_eq!(
                    read_child_terminal_receipt(db, &nonce)?,
                    f.receipt,
                    "{scenario}: receipt bytes must stay untouched"
                );
            }
            let status: String = db.connection()?.query_row(
                "SELECT status FROM agent_identities WHERE agent_id=?1",
                [&f.child.agent_id],
                |row| row.get(0),
            )?;
            assert_eq!(
                status, "deleting",
                "{scenario}: fence must survive rollback"
            );
            f.host.shutdown().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn child_terminal_receipt_purge_fault_rolls_back_whole_receipt_and_proof() -> Result<()> {
        let f = child_terminal_receipt_fixture().await?;
        let db = f.host.runtime_db();
        settle_execution(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
        db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER fail_whole_receipt_purge BEFORE DELETE ON input_mutation_receipts
                              BEGIN SELECT RAISE(ABORT,'injected whole receipt purge failure'); END;",
            )?;
            Ok(())
        })?;
        let parent_task_before = parent_task_row(db, &f.task.id)?;
        let error = purge_private_history(db, &f.child.agent_id, f.child.incarnation, Utc::now())
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("injected whole receipt purge failure"),
            "purge fault: {error:#}"
        );
        db.transaction(|tx| {
            tx.execute_batch("DROP TRIGGER fail_whole_receipt_purge")?;
            Ok(())
        })?;
        assert_eq!(
            read_child_terminal_receipt(db, &f.nonce)?,
            f.receipt,
            "the failed purge must roll the whole receipt back verbatim"
        );
        assert_eq!(parent_task_row(db, &f.task.id)?, parent_task_before);
        let status: String = db.connection()?.query_row(
            "SELECT status FROM agent_identities WHERE agent_id=?1",
            [&f.child.agent_id],
            |row| row.get(0),
        )?;
        assert_eq!(status, "deleting");
        let purged: u8 = db.connection()?.query_row(
            "SELECT json_extract(value,'$.purged') FROM runtime_metadata WHERE key=?1",
            [format!("agent_deletion_cleanup:{}", f.child.agent_id)],
            |row| row.get(0),
        )?;
        assert_eq!(purged, 0, "a failed purge must not record a cleared proof");
        // The rolled-back state remains a valid purge input.
        purge_private_history(db, &f.child.agent_id, f.child.incarnation, Utc::now())?;
        let remaining: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [&f.nonce],
            |row| row.get(0),
        )?;
        assert_eq!(remaining, 0);
        assert_eq!(parent_task_row(db, &f.task.id)?, parent_task_before);
        f.host.shutdown().await?;
        Ok(())
    }
}

impl RuntimeHostBridge {
    pub(crate) fn handoff_original_child_monitor_recovery<'a>(
        &'a self,
        source: &'a Arc<OriginalChildTaskProducer>,
        request: &'a TaskDomainLoadRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let host = self.host()?;
            let ledger = &host.inner.input_evidence_budget;
            source.validate_material_ledger(ledger)?;
            let scope = &source.address().scope;
            let (record_limit, byte_limit) = TaskDomainLoadRequest::bounded_material_limits(
                request.record_limit,
                request.byte_limit,
            );
            let bounded = TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit,
                byte_limit,
            };
            let request = &bounded;
            let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                source.address(),
            )?)?
            .checked_add(512)
            .and_then(|n| n.checked_mul(8))
            .context("monitor recovery owner slots overflow")?;
            let _slots = crate::runtime_db::command_task_terminal::charge_material(
                scope, bytes, 8, request, ledger,
            )?;
            let remaining = TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: request
                    .record_limit
                    .checked_sub(8)
                    .context("monitor owner slots exceed record bound")?,
                byte_limit: request
                    .byte_limit
                    .checked_sub(bytes)
                    .context("monitor owner slots exceed byte bound")?,
            };
            let held = host
                .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
                .await?;
            let proof=host.runtime_db().transaction(|tx|
                crate::runtime_db::child_task_terminal::observe_original_child_monitor_recovery_handoff_tx(
                    tx,source,&remaining,ledger))?;
            let Some(proof) = proof else { return Ok(false) };
            // Keep the actual current original-scope owner held through transfer;
            // an old Runtime receives no business-install token or new handle.
            source.acknowledge_monitor_recovery_handoff(&proof)?;
            drop(held);
            Ok(true)
        })
    }
}
