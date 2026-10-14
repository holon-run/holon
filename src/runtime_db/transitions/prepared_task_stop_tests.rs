use super::*;
use crate::runtime::{
    input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory},
    resident_task::{CanonicalTaskFact, TaskCaptureRequest, TaskFamily, TaskKey},
};
use crate::runtime_db::{
    task_snapshot::{prepare_task_domain_snapshot_with_budget_tx, TaskDomainLoadRequest},
    task_stop::{
        load_task_stop_by_sequence_tx, PreparedTaskStop, TaskStopChildTarget,
        TaskStopPhysicalState, TaskStopResultPlan, TaskStopResultState, TaskStopSignal,
    },
};
use crate::types::{AdmissionContext, MessageDeliverySurface, TaskKind, TaskRecord, TaskStatus};

pub(super) fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn command(f: &Fixture, task: TaskRecord) -> TaskTransitionCommand {
    TaskTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        task,
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        agent_state: None,
        message_evidence: Vec::new(),
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: true,
        commit_on_idempotent: false,
        fault: None,
    }
}
pub(super) fn seed(f: &Fixture, kind: TaskKind) -> Result<TaskRecord> {
    seed_status(f, kind, TaskStatus::Running)
}
fn seed_status(f: &Fixture, kind: TaskKind, status: TaskStatus) -> Result<TaskRecord> {
    let task = TaskRecord {
        id: "prepared-stop-task".into(),
        agent_id: f.scope.agent_id.clone(),
        kind,
        status,
        created_at: f.at,
        updated_at: f.at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original running task".into()),
        detail: Some(serde_json::json!({
            "cancel_requested":false,"rejoin_obligation_id":"prepared-stop-task","rejoin_generation":1,
            "parent_turn_id":"original-task-parent","child_agent_id":"prepared-stop-child"})),
        recovery: None,
    };
    let prepared = PreparedInputTransition::task(
        f.scope.clone(),
        "seed-original-stop-task".into(),
        f.at,
        PreparedTaskTransition::new(
            command(f, task.clone()),
            Default::default(),
            false,
            None,
            None,
        )?,
    )?;
    let InputMutationExecution::Committed { receipt, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("seed Task must commit")
    };
    assert!(matches!(
        receipt.outcome.result,
        InputMutationResult::Task { stop: None, .. }
    ));
    Ok(task)
}
pub(super) fn bundle(
    f: &Fixture,
    original: &TaskRecord,
    nonce: &str,
    signal: TaskStopSignal,
    child: Option<TaskStopChildTarget>,
) -> Result<PreparedInputTransition> {
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let locator = f.db.transaction(|tx| {
        let current =
            prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &request(), &ledger)?
                .require_current()?;
        let captured = current.resident.capture(&request().capture)?;
        Ok(captured
            .lookup(&TaskKey::new(TaskFamily::Task, &original.id))?
            .context("original Task missing")?
            .locator
            .clone())
    })?;
    let mut task = original.clone();
    task.status = if original.kind == TaskKind::CommandTask && signal != TaskStopSignal::NoHandle {
        TaskStatus::Cancelling
    } else {
        TaskStatus::Cancelled
    };
    task.updated_at = original.updated_at + chrono::Duration::seconds(1);
    task.parent_message_id = None;
    task.detail.as_mut().unwrap()["task_status"] = serde_json::json!(task.status);
    if original.kind == TaskKind::CommandTask {
        task.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
        if signal == TaskStopSignal::Force {
            task.detail.as_mut().unwrap()["force_stop_requested"] = serde_json::json!(true);
        }
    }
    let result = if original.kind != TaskKind::CommandTask {
        let mut message = MessageEnvelope::new(
            &f.scope.agent_id,
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: original.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "frozen later result only".into(),
            },
        );
        message.id = "not-admitted-stop-result".into();
        message.task_id = Some(original.id.clone());
        message.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
        message.admission_context = Some(AdmissionContext::RuntimeOwned);
        Some(TaskStopResultPlan {
            phase_nonce: format!("result-{nonce}"),
            message,
        })
    } else {
        None
    };
    let intent = PreparedTaskStop {
        task: locator,
        kind: original.kind,
        created_at: original.created_at,
        rejoin: original.rejoin_fence().ok(),
        owner_epoch: 1,
        signal,
        lease_id: (signal != TaskStopSignal::NoHandle).then(|| "original-monitor-lease".into()),
        child,
        child_unavailable: None,
        worktree: None,
        worktree_unavailable: None,
        result,
    };
    PreparedInputTransition::task(
        f.scope.clone(),
        nonce.into(),
        task.updated_at,
        PreparedTaskTransition::new(command(f, task), Default::default(), false, None, None)?
            .with_stop_intent(intent)?,
    )
}
pub(super) fn rows(f: &Fixture) -> Result<BTreeMap<String, Vec<String>>> {
    let mut values = namespace_rows(f)?;
    let conn = f.db.connection()?;
    for table in [
        "tasks",
        "task_result_settlements",
        "task_stop_effects",
        "task_domain_records",
        "task_domain_current",
        "agent_deletion_jobs",
        "execution_protocol_attempts",
        "execution_protocol_outcomes",
    ] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}"))?;
        let cols = stmt.column_count();
        let mut data = stmt
            .query_map([], |row| {
                (0..cols)
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|value| format!("{value:?}"))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        data.sort();
        values.insert(table.into(), data);
    }
    Ok(values)
}
fn stop_binding(
    receipt: &InputMutationReceipt,
) -> crate::runtime_db::input_mutation_receipts::TaskStopReceiptBinding {
    let InputMutationResult::Task {
        stop: Some(stop),
        deletion: None,
        ..
    } = &receipt.outcome.result
    else {
        panic!("Stop ACK missing")
    };
    stop.clone()
}

#[test]
fn prepared_stop_initial_ack_binds_actual_facet_and_defers_noncommand_result_without_signal(
) -> Result<()> {
    for kind in [TaskKind::CommandTask, TaskKind::AgentMessageWait] {
        let f = Fixture::new()?;
        let original = seed(&f, kind)?;
        let signal = if kind == TaskKind::CommandTask {
            TaskStopSignal::Cancel
        } else {
            TaskStopSignal::Abort
        };
        let prepared = bundle(&f, &original, "initial-stop", signal, None)?;
        assert!(
            !prepared.dispatch_memory_candidate(),
            "initial Stop cannot create a dispatch memory obligation"
        );
        assert!(
            prepared.pending_messages()?.is_empty(),
            "future template is not pending admission"
        );
        let before = rows(&f)?;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        let InputMutationExecution::Committed { value, receipt } =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &prepared,
                    &work,
                    &ledger,
                    &request(),
                )?
        else {
            panic!("first Stop commits")
        };
        assert!(value.applied && value.effects.messages.is_empty());
        assert!(value.effects.agent_identities.is_empty());
        let delta = value
            .effects
            .task_delta
            .as_ref()
            .context("Stop must publish one Task delta")?;
        let record = delta
            .records
            .iter()
            .find(|fact| fact.locator.key == TaskKey::new(TaskFamily::Task, &original.id))
            .unwrap();
        let CanonicalTaskFact::Task(task) = &record.fact else {
            unreachable!()
        };
        assert_eq!(
            task.status,
            if kind == TaskKind::CommandTask {
                TaskStatus::Cancelling
            } else {
                TaskStatus::Cancelled
            }
        );
        assert!(!delta.stop_records.is_empty());
        let observed = ledger.report()?;
        let duplicate = ledger.register_task_stop_view(
            &f.scope,
            InputMemoryCategory::Candidate,
            delta.stop_records.iter(),
        )?;
        assert_eq!(
            ledger
                .report()?
                .categories
                .get(&InputMemoryCategory::Candidate),
            observed.categories.get(&InputMemoryCategory::Candidate),
            "new Stop Arcs must already retain Candidate membership through ACK",
        );
        drop(duplicate);
        let binding = stop_binding(&receipt);
        f.db.transaction(|tx| {
            let loaded = load_task_stop_by_sequence_tx(
                tx,
                &f.scope,
                binding.effect_seq,
                request().byte_limit,
                &ledger,
            )?;
            let actual = loaded.records.first().unwrap();
            assert_eq!(actual.receipt_binding(), binding);
            assert_eq!(actual.intent.signal, signal);
            assert_eq!(actual.progress.physical, TaskStopPhysicalState::Pending);
            assert_eq!(
                actual.progress.result,
                if kind == TaskKind::CommandTask {
                    TaskStopResultState::NotRequired
                } else {
                    TaskStopResultState::Pending
                }
            );
            assert!(actual.result_nonce.is_none());
            Ok(())
        })?;
        let after = rows(&f)?;
        for name in [
            "messages",
            "queue_entries",
            "task_result_settlements",
            "agent_deletion_jobs",
            "turn_records",
            "execution_protocol_attempts",
        ] {
            assert_eq!(
                after.get(name),
                before.get(name),
                "first Stop cannot write {name}"
            );
        }
        let InputMutationExecution::Replayed {
            receipt: replay, ..
        } =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)?
        else {
            panic!("exact Stop replay")
        };
        assert_eq!(replay, receipt);
        assert_eq!(rows(&f)?, after);
    }
    Ok(())
}

#[test]
fn prepared_stop_alias_or_whole_receipt_fault_rolls_back_task_intent_audit_and_metadata(
) -> Result<()> {
    for phase in ["canonical", "insert", "finalizer", "receipt"] {
        let f = Fixture::new()?;
        let original = seed(&f, TaskKind::CommandTask)?;
        let mut prepared = bundle(&f, &original, "rollback-stop", TaskStopSignal::Cancel, None)?;
        if let InputTransitionCommand::Task(task) = prepared.command.as_mut() {
            let mut event = AuditEvent::legacy(
                "task_stop_requested",
                serde_json::json!({"task_id":original.id}),
            );
            event.id = "original-stop-audit".into();
            event.created_at = task.command.task.updated_at;
            task.command.audit_events.push(event);
            if phase == "canonical" {
                task.command.fault = Some(TransitionFaultPoint::AfterCanonicalWrites);
            }
        }
        // This fixture adds an audit before freezing the final command digest.
        prepared.descriptor =
            crate::runtime_db::input_mutation_receipts::PreparedInputMutation::new(
                InputMutationFamily::Task,
                "rollback-stop".into(),
                prepared.descriptor.scopes().to_vec(),
                canonical_command_digest(prepared.command.as_ref())?,
                prepared.descriptor.prepared_at(),
            )?;
        if phase != "canonical" {
            let sql=match phase {
                "insert"=>"CREATE TRIGGER fail_stop_insert AFTER INSERT ON task_stop_effects BEGIN SELECT RAISE(ABORT,'stop insertion fault'); END;",
                "finalizer"=>"CREATE TRIGGER fail_stop_finalizer BEFORE UPDATE ON task_domain_current BEGIN SELECT RAISE(ABORT,'stop finalizer fault'); END;",
                _=>"CREATE TRIGGER fail_stop_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'stop receipt fault'); END;",
            };
            f.db.transaction(|tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })?;
        }
        let before = rows(&f)?;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let request = request();
        let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &prepared, &work, &ledger, &request,
                )
                .err()
                .context("fault must fail original whole Tx")?;
        assert!(format!("{error:#}").contains(if phase == "canonical" {
            "injected"
        } else {
            "fault"
        }));
        assert_eq!(rows(&f)?, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn prepared_new_stop_rejects_deleting_but_original_receipt_replay_and_signal_stay_frozen(
) -> Result<()> {
    for already_committed in [false, true] {
        let f = Fixture::new()?;
        let original = seed(&f, TaskKind::CommandTask)?;
        let prepared = bundle(
            &f,
            &original,
            "original-cancel",
            TaskStopSignal::Cancel,
            None,
        )?;
        let receipt = if already_committed {
            match f
                .db
                .transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)?
            {
                InputMutationExecution::Committed { receipt, .. } => Some(receipt),
                _ => unreachable!(),
            }
        } else {
            None
        };
        let force = if already_committed {
            Some(bundle(
                &f,
                &original,
                "original-cancel",
                TaskStopSignal::Force,
                None,
            )?)
        } else {
            None
        };
        let identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
        f.db.transaction(|tx| {
            repositories::ensure_agent_deletion_at_tx(
                tx,
                &repositories::AgentDeletionRequest {
                    agent_id: f.scope.agent_id.clone(),
                    admission: repositories::AgentDeletionAdmission::ExpectedRevision(
                        identity.revision,
                    ),
                    requested_by: "operator".into(),
                    cascade_private_children: false,
                },
                "operator-stop-test-deletion",
                f.at + chrono::Duration::seconds(2),
            )?;
            Ok(())
        })?;
        let before = rows(&f)?;
        let result =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared);
        if let Some(original_receipt) = receipt {
            let InputMutationExecution::Replayed { receipt, .. } = result? else {
                panic!("original receipt wins before Active/OCC")
            };
            assert_eq!(receipt, original_receipt);
            let binding = stop_binding(&receipt);
            assert!(binding.effect_seq > 0);
            let changed = force.as_ref().unwrap();
            // Original Task locator is stale; receipt conflict still precedes it.
            let error =
                f.db.transitions_in_scope(&f.scope)
                    .commit_prepared_transition(changed)
                    .err()
                    .context("Force cannot replace original nonce")?;
            assert!(matches!(
                error.downcast_ref::<ReceiptError>(),
                Some(ReceiptError::CommandConflict)
            ));
        } else {
            let error = result.err().context("new Deleting Stop must reject")?;
            assert!(format!("{error:#}").contains("new TaskStop requires original Active identity"));
        }
        assert_eq!(rows(&f)?, before);
    }
    Ok(())
}

#[test]
fn prepared_stop_child_is_only_original_readonly_scope_including_deleted_and_never_successor_state(
) -> Result<()> {
    for corruption in ["none", "successor", "parent", "creation_task"] {
        let f = Fixture::new()?;
        let child = AgentIdentityRecord::new(
            "prepared-stop-child",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(f.scope.agent_id.clone()),
            Some("prepared-stop-task".into()),
        );
        f.db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &child)?;
            Ok(())
        })?;
        let original = seed(&f, TaskKind::ChildAgentTask)?;
        let target = TaskStopChildTarget {
            scope: InputEvidenceScope {
                agent_id: child.agent_id.clone(),
                identity_incarnation: child.incarnation,
            },
            parent_agent_id: f.scope.agent_id.clone(),
            creation_task_id: original.id.clone(),
        };
        let prepared = bundle(
            &f,
            &original,
            "child-stop",
            TaskStopSignal::Abort,
            Some(target.clone()),
        )?;
        assert!(prepared.descriptor.scopes().contains(&target.scope));
        // Use the original metadata actor's monotonic tombstone transition.
        // A task's frozen clock may precede this child's real creation clock.
        let retired =
            f.db.agent_identities()
                .tombstone_with_closed_supervision(&child.agent_id)?;
        assert_eq!(retired.status, crate::types::AgentRegistryStatus::Deleted);
        assert_eq!(
            f.db.agent_identities().latest(&child.agent_id)?,
            Some(retired.clone())
        );
        f.db.transaction(|tx| {
            upsert_agent_state_tx(tx,&AgentState::new(&child.agent_id))?;
            // Explicit poison: a readonly witness must never decode child State.
            tx.execute("UPDATE agent_states SET payload_json='not-json-child-State' WHERE agent_id=?1",[&child.agent_id])?;
            match corruption {
                "successor"=>{tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&child.agent_id])?;}
                "parent"=>{tx.execute("UPDATE agent_identities SET parent_agent_id='foreign-parent',payload_json=json_set(payload_json,'$.parent_agent_id','foreign-parent') WHERE agent_id=?1",[&child.agent_id])?;}
                "creation_task"=>{tx.execute("UPDATE agent_identities SET delegated_from_task_id='other-task',payload_json=json_set(payload_json,'$.delegated_from_task_id','other-task') WHERE agent_id=?1",[&child.agent_id])?;}
                _=>{},
            }
            Ok(())
        })?;
        let before = rows(&f)?;
        let result =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared);
        if corruption == "none" {
            let InputMutationExecution::Committed { receipt, value } = result? else {
                panic!("same Deleted child is readonly witness")
            };
            assert!(value.effects.agent_identities.is_empty());
            let secondary = receipt
                .outcome
                .scopes
                .iter()
                .find(|scope| scope.scope == target.scope)
                .unwrap();
            assert!(
                secondary.audit_events.is_empty()
                    && !secondary.notify_memory_index
                    && !secondary.notify_scheduler
            );
            assert_eq!(
                serde_json::to_value(&secondary.install_keys)?,
                serde_json::to_value(
                    crate::runtime_db::input_recovery_install::InputMutationInstallKeys::default()
                )?
            );
            assert_eq!(
                f.db.agent_identities().latest(&child.agent_id)?,
                Some(retired)
            );
            assert!(matches!(
                f.db.transitions_in_scope(&f.scope)
                    .commit_prepared_transition(&prepared)?,
                InputMutationExecution::Replayed { .. }
            ));
        } else {
            let error = result
                .err()
                .context("foreign original-child witness must reject")?;
            assert!(format!("{error:#}")
                .contains("TaskStop child is not the original identity/parent/creation Task"));
            assert_eq!(rows(&f)?, before);
        }
        let current = rows(&f)?;
        assert_eq!(
            current.get("agent_identities"),
            before.get("agent_identities")
        );
        assert_eq!(current.get("agent_states"), before.get("agent_states"));
    }
    Ok(())
}

#[test]
fn prepared_stop_cannot_smuggle_first_result_queue_activation_or_identity_deletion() -> Result<()> {
    for extra in [
        "message",
        "queue",
        "result",
        "protocol",
        "protocol_sync",
        "State",
        "work",
    ] {
        let f = Fixture::new()?;
        let original = seed(&f, TaskKind::CommandTask)?;
        let prepared = bundle(&f, &original, "shape-stop", TaskStopSignal::Cancel, None)?;
        let before = rows(&f)?;
        let InputTransitionCommand::Task(mut task) = *prepared.command else {
            unreachable!()
        };
        match extra {
            "message" => task.command.message_evidence = f.queue()?.command.message_evidence,
            "queue" => {
                let QueueMutation::Upsert(record) = f.queue()?.command.mutation else {
                    unreachable!()
                };
                task.command.queue_entry = Some(record);
            }
            "result" => {
                let mut terminal = original.clone();
                terminal.status = TaskStatus::Cancelled;
                let mut message = MessageEnvelope::new(
                    &f.scope.agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: original.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "must not enqueue".into(),
                    },
                );
                message.task_id = Some(original.id.clone());
                task.command.task_result_settlement =
                    crate::runtime_db::task_result_settlement::TaskResultSettlementRecord::pending(
                        &terminal, &message, f.at,
                    )?;
            }
            "protocol" => {
                task.execution_protocol.bootstrap = Some(
                    crate::domain::execution_protocol::ExecutionProtocolState::empty(
                        &f.scope.agent_id,
                    ),
                )
            }
            "protocol_sync" => task.synchronize_execution_protocol = true,
            "State" => {
                let state = f.state()?;
                task.command.agent_state = Some(AgentStateMutation {
                    expected: Some(Box::new(state.clone())),
                    record: Box::new(state),
                });
            }
            "work" => task
                .command
                .expected_wait_conditions
                .push(WaitConditionExpectation {
                    id: "forbidden-wait".into(),
                    agent_id: f.scope.agent_id.clone(),
                    status: crate::types::WaitConditionStatus::Active,
                    updated_at: f.at,
                }),
            _ => unreachable!(),
        }
        let error = PreparedInputTransition::task(f.scope.clone(), "shape-stop".into(), f.at, task)
            .err()
            .context("Stop shape must reject before SQL")?;
        assert!(format!("{error:#}").contains("TaskStop initial ACK"));
        assert_eq!(rows(&f)?, before);
    }
    Ok(())
}

#[test]
fn prepared_plain_task_digest_has_no_stop_field_and_new_noop_stop_still_advances_once() -> Result<()>
{
    let f = Fixture::new()?;
    let original = seed(&f, TaskKind::CommandTask)?;
    let plain = PreparedTaskTransition::new(
        command(&f, original.clone()),
        Default::default(),
        false,
        None,
        None,
    )?;
    assert!(serde_json::to_value(&plain)?.get("stop_intent").is_none());
    let first = bundle(&f, &original, "first-cancel", TaskStopSignal::Cancel, None)?;
    let InputMutationExecution::Committed {
        receipt: first_receipt,
        ..
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&first)?
    else {
        unreachable!()
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let current = f.db.transaction(|tx| {
        let loaded =
            prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &request(), &ledger)?
                .require_current()?;
        let captured = loaded.resident.capture(&request().capture)?;
        let CanonicalTaskFact::Task(task) = &captured
            .lookup(&TaskKey::new(TaskFamily::Task, &original.id))?
            .unwrap()
            .fact
        else {
            unreachable!()
        };
        Ok((task.clone(), loaded.resident.coverage().revision))
    })?;
    let next = bundle(
        &f,
        &current.0,
        "second-original-cancel",
        TaskStopSignal::Cancel,
        None,
    )?;
    let at = next.descriptor.prepared_at();
    let InputTransitionCommand::Task(mut task) = *next.command else {
        unreachable!()
    };
    task.command.task = current.0.clone(); // Real canonical Task no-op; intent INSERT still changes the domain.
    let next =
        PreparedInputTransition::task(f.scope.clone(), "second-original-cancel".into(), at, task)?;
    let InputMutationExecution::Committed { receipt, value } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&next)?
    else {
        unreachable!()
    };
    assert!(value.applied);
    assert!(value.effects.task_write_keys.is_empty());
    assert!(value.effects.task_delta.as_ref().unwrap().revision > current.1);
    assert!(stop_binding(&receipt).effect_seq > stop_binding(&first_receipt).effect_seq);
    Ok(())
}

#[test]
fn task_and_stop_receipt_preflight_bounds_all_material_before_direct_and_fresh_lookup() -> Result<()>
{
    for stop in [false, true] {
        let f = Fixture::new()?;
        let original = seed(&f, TaskKind::CommandTask)?;
        let prepared = if stop {
            bundle(
                &f,
                &original,
                "task:full-receipt",
                TaskStopSignal::Cancel,
                None,
            )?
        } else {
            PreparedInputTransition::task(
                f.scope.clone(),
                "task:full-receipt".into(),
                f.at,
                PreparedTaskTransition::new(
                    command(&f, original),
                    Default::default(),
                    false,
                    None,
                    None,
                )?,
            )?
        };
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        drop(
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &prepared,
                    &work,
                    &ledger,
                    &request(),
                )?,
        );
        // Family has no length constraint. Small corrupt JSON ensures only
        // complete scalar preflight can reject before the original lookup.
        f.db.connection()?.execute("UPDATE input_mutation_receipts SET family=?1,prepared_json='bad-json' WHERE command_nonce=?2",
            params!["huge-normalized-family".repeat(128 * 1024),prepared.descriptor().command_nonce()])?;
        let mut tiny = request();
        tiny.byte_limit = 1024 * 1024;
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &prepared, &work, &ledger, &tiny,
                )
                .unwrap_err();
        let assert_budget = |error: anyhow::Error| {
            assert!(
                matches!(
                    error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
                    Some(
                        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                            "original Task receipt"
                        )
                    )
                ),
                "stop={stop}: {error:#}"
            );
        };
        assert_budget(error);
        let attempt = crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            prepared.descriptor().clone(),
        );
        attempt.finish_original_attempt();
        let mut connection = f.db.connection()?;
        let error = crate::runtime_db::input_mutation_receipts::with_fresh_input_mutation_resolution_and_preflight(
            &mut connection, &attempt,
            |tx| crate::runtime_db::transitions::task_result_receipt_budget_tx(tx,prepared.descriptor(),&f.scope,&tiny,&ledger),
            |_,_| -> Result<()> { panic!("Task/Stop budget must reject before receipt lookup/callback") },
        ).unwrap_err();
        assert_budget(error);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn interrupted_child_cancellation_requires_original_stop_proof_and_keeps_terminal_rules_atomic(
) -> Result<()> {
    for (kind, status, with_stop, fault) in [
        (
            TaskKind::ChildAgentTask,
            TaskStatus::Interrupted,
            true,
            false,
        ),
        (
            TaskKind::ChildAgentTask,
            TaskStatus::Interrupted,
            true,
            true,
        ),
        (
            TaskKind::ChildAgentTask,
            TaskStatus::Interrupted,
            false,
            false,
        ),
        (TaskKind::ChildAgentTask, TaskStatus::Completed, true, false),
        (
            TaskKind::AgentMessageWait,
            TaskStatus::Interrupted,
            true,
            false,
        ),
    ] {
        let f = Fixture::new()?;
        let child = if kind == TaskKind::ChildAgentTask {
            let identity = crate::types::AgentIdentityRecord::new(
                "prepared-stop-child",
                crate::types::AgentKind::Child,
                crate::types::AgentVisibility::Private,
                crate::types::AgentOwnership::ParentSupervised,
                crate::types::AgentProfilePreset::PrivateChild,
                Some(f.scope.agent_id.clone()),
                Some("prepared-stop-task".into()),
            );
            f.db.agent_identities().upsert(&identity)?;
            Some(TaskStopChildTarget {
                scope: InputEvidenceScope {
                    agent_id: identity.agent_id,
                    identity_incarnation: identity.incarnation,
                },
                parent_agent_id: f.scope.agent_id.clone(),
                creation_task_id: "prepared-stop-task".into(),
            })
        } else {
            None
        };
        let allowed = kind == TaskKind::ChildAgentTask
            && status == TaskStatus::Interrupted
            && with_stop
            && !fault;
        let original = seed_status(&f, kind, status)?;
        let prepared = if with_stop {
            bundle(
                &f,
                &original,
                "interrupted-child-stop",
                TaskStopSignal::NoHandle,
                child,
            )?
        } else {
            let mut cancelled = original.clone();
            cancelled.status = TaskStatus::Cancelled;
            cancelled.updated_at += chrono::Duration::seconds(1);
            PreparedInputTransition::task(
                f.scope.clone(),
                "ordinary-interrupted-rewrite".into(),
                cancelled.updated_at,
                PreparedTaskTransition::new(
                    command(&f, cancelled),
                    Default::default(),
                    false,
                    None,
                    None,
                )?,
            )?
        };
        if fault {
            f.db.connection()?.execute_batch("CREATE TRIGGER fail_interrupted_stop_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='interrupted-child-stop' BEGIN SELECT RAISE(ABORT,'interrupted Stop receipt fault'); END;")?;
        }
        let before = rows(&f)?;
        let committed =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared);
        if !allowed {
            let error = committed
                .err()
                .context("terminal rewrite unexpectedly committed")?;
            if fault {
                assert!(format!("{error:#}").contains("interrupted Stop receipt fault"));
            } else {
                assert!(format!("{error:#}").contains("conflicting task transition"));
            }
            assert_eq!(rows(&f)?, before);
            assert_eq!(f.db.tasks().latest(&original.id)?.unwrap(), original);
            continue;
        }
        let InputMutationExecution::Committed { value, receipt } = committed? else {
            panic!("new Stop must commit")
        };
        assert_eq!(
            f.db.tasks().latest(&original.id)?.unwrap().status,
            TaskStatus::Cancelled
        );
        let delta = value
            .effects
            .task_delta
            .as_ref()
            .context("Stop omitted actual Task delta")?;
        assert_eq!(delta.revision, delta.base_revision + 1);
        assert!(value
            .effects
            .task_write_keys
            .contains(&TaskKey::new(TaskFamily::Task, &original.id)));
        let binding = stop_binding(&receipt);
        assert_eq!(binding.scope, f.scope);
        assert!(
            matches!(&receipt.outcome.result,InputMutationResult::Task{task_id,..} if task_id==&original.id)
        );
        assert_eq!(
            f.db.agent_identities()
                .latest("prepared-stop-child")?
                .unwrap()
                .status,
            crate::types::AgentRegistryStatus::Active
        );
        let after = rows(&f)?;
        let InputMutationExecution::Replayed {
            receipt: replay, ..
        } =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)?
        else {
            panic!("original Stop must replay")
        };
        assert_eq!(replay, receipt);
        assert_eq!(rows(&f)?, after);
    }
    Ok(())
}
