//! Original admitted Task -> Message/queue/pending in one Deleting transaction.
use super::*;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key,
};
use crate::runtime_db::input_evidence_snapshot::{
    register_pruned_input_evidence_tx, CanonicalInputEvidence,
};
use crate::runtime_db::prepared_input_evidence::PreparedInputEvidence;

fn terminal_command(f: &Fixture, id: &str) -> Result<TaskTransitionCommand> {
    setup_terminal(f, id, true)
}
fn setup_terminal(f: &Fixture, id: &str, deleting: bool) -> Result<TaskTransitionCommand> {
    // Freeze an actual canonical source assignment before the admitted Task.
    let mut source = MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original Task admission".into(),
        },
    );
    source.id = format!("{id}:source");
    source.created_at = f.at;
    source.normalize_admission_fields();
    f.db.evidence()
        .commit_prepared_input_evidence(&PreparedInputEvidence::message(
            f.scope.clone(),
            format!("{id}:source-write"),
            f.at,
            source.clone(),
            vec![],
        )?)?;
    let parent = format!("{id}:parent-turn");
    let mut turn = TurnRecord::new(&f.scope.agent_id, &parent, 1);
    turn.created_at = f.at;
    turn.input_message_ids.push(source.id);
    f.db.evidence()
        .commit_prepared_input_evidence(&PreparedInputEvidence::turn(
            f.scope.clone(),
            format!("{id}:parent-write"),
            f.at,
            turn,
        )?)?;
    let mut admitted = command(f, id, TaskStatus::Running);
    admitted.task.kind = TaskKind::AgentMessageWait;
    admitted.task.detail = Some(serde_json::json!({"rejoin_obligation_id":id,
        "rejoin_generation":1,"parent_turn_id":parent}));
    execute(
        f,
        &freeze(f, &format!("{id}:admitted"), admitted.clone())?,
        &load_request(),
        &ledger()?,
    )?;
    let mut wait = super::super::namespace_wait(&f.scope.agent_id, f.at);
    wait.id = format!("{id}:wait");
    wait.kind = crate::types::WaitConditionKind::Task;
    wait.wake_sources = vec![crate::types::WakeSource::TaskResult { task_id: id.into() }];
    let mut wait_command = super::super::original_wait_command(f, None)?;
    wait_command.wait_conditions = vec![wait];
    let prepared = PreparedInputTransition::wait(
        f.scope.clone(),
        format!("{id}:wait-write"),
        f.at,
        wait_command,
        Default::default(),
        None,
    )?;
    execute(f, &prepared, &load_request(), &ledger()?)?;
    let mut final_command = admitted;
    final_command.task.status = TaskStatus::Interrupted;
    final_command.task.updated_at += chrono::Duration::seconds(1);
    let mut message = MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task { task_id: id.into() },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original terminal result".into(),
        },
    );
    message = message.with_admission(
        crate::types::MessageDeliverySurface::TaskRejoin,
        crate::types::AdmissionContext::RuntimeOwned,
    );
    message.id = format!("{id}:result");
    message.created_at = final_command.task.updated_at;
    message.turn_id = Some(format!("{id}:unstarted-hint"));
    message.normalize_admission_fields();
    final_command.task.parent_message_id = Some(message.id.clone());
    final_command.task_result_settlement = results::TaskResultSettlementRecord::pending(
        &final_command.task,
        &message,
        message.created_at,
    )?;
    final_command.queue_entry = Some(QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: message.agent_id.clone(),
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: message.created_at,
        updated_at: message.created_at,
    });
    let old = f.state()?;
    let mut next = old.clone();
    next.pending += 1;
    next.total_message_count += 1;
    next.last_wake_reason = Some("TaskResult".into());
    // This original no-run Booting fixture wakes to idle for its TaskResult.
    next.status = crate::types::AgentStatus::AwakeIdle;
    next.sleeping_until = None;
    final_command.agent_state = Some(AgentStateMutation {
        expected: Some(Box::new(old)),
        record: Box::new(next),
    });
    final_command.message_evidence = vec![message];
    if deleting {
        operator_delete(f, final_command.task.updated_at)?;
    }
    Ok(final_command)
}
fn operator_delete(f: &Fixture, at: chrono::DateTime<Utc>) -> Result<()> {
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
            "original-result-deletion",
            at,
        )?;
        Ok(())
    })
}
fn rows(f: &Fixture) -> Result<BTreeMap<String, Vec<String>>> {
    let mut all = super::super::namespace_rows(f)?;
    let connection = f.db.connection()?;
    for table in [
        "tasks",
        "task_result_settlements",
        "task_domain_current",
        "task_domain_records",
    ] {
        let mut query = connection.prepare(&format!("SELECT * FROM {table}"))?;
        let columns = query.column_count();
        let mut values = query
            .query_map([], |row| {
                (0..columns)
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|value| format!("{value:?}"))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values.sort();
        all.insert(table.into(), values);
    }
    Ok(all)
}

#[test]
fn deleting_original_task_result_commits_message_queue_wait_and_three_domains_once() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let command = terminal_command(&f, "task:input-bridge")?;
    let result = command.task_result_settlement.as_ref().unwrap();
    let message = &command.message_evidence[0];
    let before_turns =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM turn_records", [], |r| {
                r.get::<_, usize>(0)
            })?;
    let prepared = freeze(&f, "input-bridge:terminal", command.clone())?;
    let InputMutationExecution::Committed { value, receipt } =
        execute(&f, &prepared, &load_request(), &ledger)?
    else {
        panic!("first terminal ACK")
    };
    let actual = f.db.transaction(|tx| {
        crate::runtime_db::evidence::committed_message_tx(tx, &f.scope.agent_id, &message.id)
    })?;
    assert!(actual.message_seq.is_some_and(|seq| seq > 0));
    let mut expected = message.clone();
    expected.message_seq = actual.message_seq;
    assert_eq!(actual, expected);
    assert!(value
        .effects
        .input_evidence_delta
        .records
        .iter()
        .any(|record| record.locator.key == Key::new(Family::Message, &message.id)));
    assert_eq!(
        value
            .effects
            .input_evidence_delta
            .message_turn_keys
            .get(&message.id),
        Some(&None)
    );
    assert!(value
        .effects
        .task_delta
        .as_ref()
        .unwrap()
        .coverage
        .unsettled_result_ids
        .contains(&result.result_identity));
    let wait_id = "task:input-bridge:wait";
    assert!(value
        .effects
        .wait_condition_writes
        .wait_ids
        .contains(wait_id));
    assert!(value
        .effects
        .work_delta
        .as_ref()
        .unwrap()
        .coverage
        .unresolved_wait_ids
        .contains(wait_id));
    assert!(value
        .effects
        .audit_events
        .iter()
        .any(|event| event.id == format!("audit:wait-triggered:{}", message.id)));
    f.db.transaction(|tx| {
        assert_eq!(
            tx.query_row(
                "SELECT status FROM queue_entries WHERE message_id=?1",
                [&message.id],
                |r| r.get::<_, String>(0)
            )?,
            "queued"
        );
        assert_eq!(
            tx.query_row(
                "SELECT status FROM wait_conditions WHERE wait_condition_id=?1",
                [wait_id],
                |r| r.get::<_, String>(0)
            )?,
            "triggered"
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM execution_protocol_attempts",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM turn_records", [], |r| r
                .get::<_, usize>(0))?,
            before_turns
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM turn_records WHERE turn_id=?1",
                [message.turn_id.as_ref().unwrap()],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        Ok(())
    })?;
    let state = f.state()?;
    assert_eq!(state.pending, 1);
    assert_eq!(state.total_message_count, 2);
    assert!(
        state.current_run_id.is_none()
            && state.current_turn_id.is_none()
            && state.current_execution_binding.is_none()
    );
    assert!(receipt.outcome.scopes[0]
        .install_keys
        .wait_conditions
        .contains(wait_id));
    let before = rows(&f)?;
    assert!(matches!(
        execute(&f, &prepared, &load_request(), &ledger)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(rows(&f)?, before);
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn derived_task_input_cannot_create_run_binding_extra_message_or_change_generation() -> Result<()> {
    for case in [
        "run",
        "binding",
        "extra-message",
        "wrong-kind",
        "authority",
        "generation",
        "pending",
    ] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let mut command = terminal_command(&f, &format!("task:invalid-{case}"))?;
        match case {
            "run" => {
                command.agent_state.as_mut().unwrap().record.current_run_id = Some("new-run".into())
            }
            "binding" => {
                command
                    .agent_state
                    .as_mut()
                    .unwrap()
                    .record
                    .current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
                    activation_id: Some("invented-activation".into()),
                    admission_provenance: None,
                    source_message_id: command.message_evidence[0].id.clone(),
                    turn_id: "new-turn".into(),
                    owner: None,
                    work_item_id: None,
                    claimed_work_revision: None,
                })
            }
            "extra-message" => {
                let mut other = command.message_evidence[0].clone();
                other.id.push_str(":other");
                command.message_evidence.push(other);
            }
            "wrong-kind" => command.message_evidence[0].kind = MessageKind::OperatorPrompt,
            "authority" => {
                command.message_evidence[0].authority_class = AuthorityClass::OperatorInstruction
            }
            "generation" => {
                command.task.detail.as_mut().unwrap()["rejoin_generation"] = 2.into();
                command.task_result_settlement = results::TaskResultSettlementRecord::pending(
                    &command.task,
                    &command.message_evidence[0],
                    command.task.updated_at,
                )?;
            }
            _ => command.agent_state.as_mut().unwrap().record.pending = 2,
        }
        let before = rows(&f)?;
        let p = freeze(&f, &format!("invalid:{case}"), command)?;
        let error = execute(&f, &p, &load_request(), &ledger).unwrap_err();
        let expected = match case {
            "run" | "binding" => "cannot start a run",
            "extra-message" => "Task trigger requires exactly one frozen Message",
            "wrong-kind" | "authority" => "not its original terminal Result Message",
            "generation" => "changed original admitted generation",
            _ => "pending differs from its one actual queue membership",
        };
        assert!(format!("{error:#}").contains(expected), "{case}: {error:#}");
        assert_eq!(rows(&f)?, before, "{case}");
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn derived_task_input_rejects_pruned_and_foreign_existing_message_ids() -> Result<()> {
    for pruned in [true, false] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let mut command = setup_terminal(&f, &format!("task:collision-{pruned}"), false)?;
        let message = command.message_evidence[0].clone();
        f.db.transaction(|tx| {
            // Construct a real collision while the original scope is Active;
            // admit the operator deletion fence only after this transaction.
            let mut stored = message.clone();
            if !pruned {
                stored.agent_id = "foreign-owner".into();
            }
            crate::runtime_db::evidence::append_message_tx(tx, &stored)?;
            if pruned {
                let actual = crate::runtime_db::evidence::committed_message_tx(
                    tx,
                    &f.scope.agent_id,
                    &stored.id,
                )?;
                crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(
                    tx,
                    &f.scope,
                    &[CanonicalInputEvidence::Message(actual)],
                )?;
                tx.execute("DELETE FROM messages WHERE evidence_id=?1", [&stored.id])?;
                register_pruned_input_evidence_tx(
                    tx,
                    &f.scope,
                    &[Key::new(Family::Message, &stored.id)],
                )?;
            }
            Ok(())
        })?;
        operator_delete(&f, command.task.updated_at)?;
        // The deliberate foreign/pruned fixture did not enqueue a membership.
        let old = f.state()?;
        command.agent_state.as_mut().unwrap().expected = Some(Box::new(old.clone()));
        command.agent_state.as_mut().unwrap().record.pending = old.pending + 1;
        let before = rows(&f)?;
        let p = freeze(&f, &format!("collision:{pruned}"), command)?;
        let error = execute(&f, &p, &load_request(), &ledger).unwrap_err();
        assert!(
            format!("{error:#}").contains(if pruned {
                "pruned evidence ID"
            } else {
                "another agent"
            }),
            "{error:#}"
        );
        assert_eq!(rows(&f)?, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn derived_task_message_field_tamper_and_alias_fault_roll_back_whole_business() -> Result<()> {
    for tamper in [true, false] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let command = terminal_command(&f, &format!("task:rollback-{tamper}"))?;
        if tamper {
            f.db.connection()?.execute_batch("CREATE TRIGGER tamper_derived_message AFTER INSERT ON messages BEGIN UPDATE messages SET payload_json=json_set(payload_json,'$.body.text','tampered actual body') WHERE evidence_id=NEW.evidence_id; END;")?;
        } else {
            f.db.connection()?.execute_batch("CREATE TRIGGER fail_derived_message_alias BEFORE INSERT ON input_evidence_current WHEN NEW.family='message' BEGIN SELECT RAISE(ABORT,'derived Message alias fault'); END;")?;
        }
        let before = rows(&f)?;
        let p = freeze(&f, &format!("rollback:{tamper}"), command)?;
        let error = execute(&f, &p, &load_request(), &ledger).unwrap_err();
        assert!(
            format!("{error:#}").contains(if tamper {
                "original frozen fields"
            } else {
                "derived Message alias fault"
            }),
            "{error:#}"
        );
        assert_eq!(rows(&f)?, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn task_input_bridge_keeps_parent_cleanup_business_fence() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let command = terminal_command(&f, "task:parent-fence")?;
    // Deliberately put the durable parent fence in place after setup. This
    // exercises fence enforcement, not parent-cleanup admission authorization.
    f.db.connection()?.execute("UPDATE agent_deletion_jobs SET payload_json=json_set(payload_json,'$.mode','parent_cleanup') WHERE agent_id=?1",[&f.scope.agent_id])?;
    let before = rows(&f)?;
    let p = freeze(&f, "parent-fence:terminal", command)?;
    let error = execute(&f, &p, &load_request(), &ledger).unwrap_err();
    assert!(
        format!("{error:#}").contains("agent_cleanup_fenced"),
        "{error:#}"
    );
    assert_eq!(rows(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
