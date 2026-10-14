use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::resident_task::TaskAvailability,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories,
        task_result_settlement::{upsert_pending_tx, TaskResultSettlementRecord},
        task_snapshot::{
            prepare_task_write_tx, register_task_writes_tx, TaskDomainLoadRequest,
            TaskSnapshotError,
        },
        transitions::PostCommitEffects,
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
        AgentVisibility, AuthorityClass, MessageBody, Priority, QueueEntryRecord, QueueEntryStatus,
        TaskKind, TaskRecord, TaskStatus,
    },
};
use chrono::{DateTime, Duration, Utc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "joint-task-owner".into(),
        identity_incarnation: 1,
    }
}
fn at() -> DateTime<Utc> {
    "2026-10-01T00:00:00Z".parse().unwrap()
}
fn task(id: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: scope().agent_id,
        kind: TaskKind::CommandTask,
        status,
        created_at: at(),
        updated_at: at(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: None,
        recovery: None,
    }
}
fn load_request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    AgentState,
    InputEvidenceBudgetLedger,
)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    let state = AgentState::new(&scope().agent_id);
    db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &scope().agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &state)?;
        task_snapshot::prepare_task_domain_snapshot_tx(tx, &scope(), &load_request())?
            .require_current()?;
        Ok(())
    })?;
    Ok((
        dir,
        db,
        state,
        InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
    ))
}
fn write_task(tx: &Transaction<'_>, state: &AgentState, task: &TaskRecord) -> Result<()> {
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    repositories::upsert_task_tx(tx, task)?;
    register_task_writes_tx(tx, &proof, &keys, state, &load_request())?;
    Ok(())
}
fn result_message(task_id: &str, id: &str) -> MessageEnvelope {
    let mut source = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "result".into(),
        },
    );
    source.id = id.into();
    source.task_id = Some(task_id.into());
    source.created_at = at();
    source
}

#[test]
fn joint_task_receipt_selectors_do_not_force_old_terminal_input_bodies_or_raw_adoption(
) -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| {
        let mut active = task("current", TaskStatus::Running);
        active.parent_message_id = Some("current-parent".into());
        write_task(tx, &state, &active)?;
        let mut terminal = task("old-receipt-target", TaskStatus::Completed);
        terminal.parent_message_id = Some("old-parent-not-current".into());
        write_task(tx, &state, &terminal)?;
        repositories::upsert_task_tx(tx, &task("raw-cleanup-target", TaskStatus::Completed))?;
        let mut selectors = TaskCaptureRequest::current();
        selectors.required_keys.extend([
            TaskKey::new(TaskFamily::Task, &terminal.id),
            TaskKey::new(TaskFamily::Task, "raw-cleanup-target"),
        ]);
        let current = prepare_original_task_domains_tx(
            tx,
            &scope(),
            &state,
            &selectors,
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
        )?;
        assert!(current.input_roots.contains(&InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            "current-parent"
        )));
        assert!(!current.input_roots.contains(&InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            "old-parent-not-current"
        )));
        let captured = current.resident.capture(&TaskCaptureRequest::current())?;
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, &terminal.id))?
            .is_some());
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, "raw-cleanup-target"))
            .is_err());
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_domain_records WHERE record_id='raw-cleanup-target'",
                [],
                |row| row.get::<_, usize>(0)
            )?,
            0
        );
        Ok(())
    })
}

fn append_pending(
    tx: &Transaction<'_>,
    state: &mut AgentState,
    source: &MessageEnvelope,
) -> Result<MessageEnvelope> {
    let (source, _) = append_message_tx(tx, source)?;
    repositories::upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: source.id.clone(),
            agent_id: source.agent_id.clone(),
            priority: source.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: source.created_at,
            updated_at: source.created_at,
        },
    )?;
    state.total_message_count = 1;
    state.pending = 1;
    upsert_agent_state_tx(tx, state)?;
    Ok(source)
}

#[test]
fn current_valid_task_source_is_strict_while_malformed_pair_reaches_authority() -> Result<()> {
    let (_dir, db, mut state, budget) = fixture()?;
    let unqualified = db.transaction(|tx| {
        let mut raw = task("raw-current", TaskStatus::Completed);
        raw.parent_message_id = Some("raw-current-result".into());
        raw.detail = Some(serde_json::json!({"rejoin_obligation_id":raw.id,
            "rejoin_generation":1,"parent_turn_id":"original-parent-turn"}));
        repositories::upsert_task_tx(tx, &raw)?;
        append_pending(
            tx,
            &mut state,
            &result_message("raw-current", "raw-current-result"),
        )
    })?;
    let error = db
        .transaction(|tx| {
            prepare_bootstrap_task_domains_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                std::slice::from_ref(&unqualified),
                4096,
                4 * 1024 * 1024,
                &budget,
            )
        })
        .err()
        .unwrap();
    assert!(
        matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::NeedsPreparation(key))
        if key == &TaskKey::new(TaskFamily::Task,"raw-current"))
    );

    let (_malformed_dir, malformed_db, mut malformed_state, malformed_budget) = fixture()?;
    let malformed = malformed_db.transaction(|tx| {
        repositories::upsert_task_tx(tx, &task("raw-current", TaskStatus::Completed))?;
        let mut source = result_message("raw-current", "malformed-result");
        source.task_id = Some("different-task".into());
        append_pending(tx, &mut malformed_state, &source)
    })?;
    malformed_db.transaction(|tx| {
        let current=prepare_bootstrap_task_domains_tx(tx,&scope(),&malformed_state,
            &TaskCaptureRequest::current(),std::slice::from_ref(&malformed),4096,4*1024*1024,&malformed_budget)?;
        assert!(current.resident.coverage().required_keys.is_empty());
        assert!(current.resident.coverage().active_task_ids.is_empty());
        assert!(current.input_roots.is_empty());
        // Passive collection preserves the real malformed canonical incoming
        // Message for authority; neither asserted raw Task becomes qualified.
        assert_eq!(malformed.task_id.as_deref(),Some("different-task"));
        assert!(matches!(&malformed.origin,MessageOrigin::Task{task_id} if task_id == "raw-current"));
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_records WHERE record_id IN ('raw-current','different-task')",[],|row|row.get::<_,usize>(0))?,0);
        Ok(())
    })?;

    let (_missing_dir, missing_db, mut missing_state, missing_budget) = fixture()?;
    let missing = missing_db.transaction(|tx| {
        append_pending(
            tx,
            &mut missing_state,
            &result_message("never-existed", "missing-task-result"),
        )
    })?;
    missing_db.transaction(|tx| {
        let current = prepare_bootstrap_task_domains_tx(
            tx,
            &scope(),
            &missing_state,
            &TaskCaptureRequest::current(),
            std::slice::from_ref(&missing),
            4096,
            4 * 1024 * 1024,
            &missing_budget,
        )?;
        assert_eq!(
            current
                .resident
                .coverage()
                .observations
                .get(&TaskKey::new(TaskFamily::Task, "never-existed")),
            Some(&TaskAvailability::Absent)
        );
        Ok(())
    })
}

#[test]
fn original_joint_task_recovery_accepts_only_same_deleting_scope_and_never_fakes_no_state(
) -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| {
        write_task(tx, &state, &task("original-running", TaskStatus::Running))?;
        let payload: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
            [&scope().agent_id],
            |row| row.get(0),
        )?;
        let mut identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
        identity.status = AgentRegistryStatus::Deleting;
        identity.updated_at += Duration::seconds(1);
        upsert_agent_identity_tx(tx, &identity)?;
        assert!(prepare_bootstrap_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget
        )
        .is_err());
        let current = prepare_original_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
        )?;
        assert!(current
            .resident
            .coverage()
            .active_task_ids
            .contains("original-running"));
        let successor = InputEvidenceScope {
            agent_id: scope().agent_id,
            identity_incarnation: 2,
        };
        assert!(prepare_original_task_domains_tx(
            tx,
            &successor,
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget
        )
        .is_err());
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&scope().agent_id],
        )?;
        assert!(prepare_original_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget
        )
        .is_err());
        Ok(())
    })
}

#[test]
fn original_task_observation_keys_survive_noop_without_using_install_delta_as_writes() -> Result<()>
{
    let mut effects = PostCommitEffects::default();
    effects
        .task_write_keys
        .insert(TaskKey::new(TaskFamily::Task, "actual-write"));
    effects
        .task_observation_keys
        .insert(TaskKey::new(TaskFamily::Task, "original-noop-target"));
    effects.task_observation_keys.insert(TaskKey::new(
        TaskFamily::ResultSettlement,
        "original-result",
    ));
    effects
        .tasks
        .push(task("legacy-task-selector", TaskStatus::Completed));
    let keys = crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
        &scope(),
        &effects,
    )?;
    assert_eq!(
        keys.tasks,
        BTreeSet::from([
            "actual-write".into(),
            "original-noop-target".into(),
            "legacy-task-selector".into()
        ])
    );
    assert_eq!(
        keys.task_result_settlements,
        BTreeSet::from(["original-result".into()])
    );
    effects.task_write_keys.clear();
    let noop = crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
        &scope(),
        &effects,
    )?;
    assert!(noop.tasks.contains("original-noop-target"));
    assert!(noop.task_result_settlements.contains("original-result"));
    assert!(!noop.tasks.contains("actual-write"));
    Ok(())
}

#[test]
fn current_unsettled_task_result_derives_exact_input_and_work_dependencies() -> Result<()> {
    let (_dir, db, mut state, budget) = fixture()?;
    db.transaction(|tx| {
        let mut source = result_message("result-task", "current-result-message");
        source.work_item_id = Some("original-work".into());
        let (source, _) = append_message_tx(tx, &source)?;
        state.total_message_count = 1;
        upsert_agent_state_tx(tx, &state)?;
        let mut completed = task("result-task", TaskStatus::Completed);
        completed.parent_message_id = Some(source.id.clone());
        completed.work_item_id = source.work_item_id.clone();
        completed.detail = Some(serde_json::json!({"rejoin_obligation_id":completed.id,
            "rejoin_generation":1,"parent_turn_id":"original-parent-turn"}));
        let result = TaskResultSettlementRecord::pending(&completed, &source, at())?.unwrap();
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &completed.id),
            TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
        ]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &completed)?;
        upsert_pending_tx(tx, &result)?;
        register_task_writes_tx(tx, &proof, &keys, &state, &load_request())?;
        let prepared = prepare_original_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
        )?;
        assert_eq!(
            prepared.input_roots,
            BTreeSet::from([
                InputEvidenceKey::new(InputEvidenceFamily::Message, &source.id),
                InputEvidenceKey::new(InputEvidenceFamily::Turn, "original-parent-turn"),
            ])
        );
        assert_eq!(
            prepared.work_roots,
            BTreeSet::from(["original-work".into()])
        );
        assert!(prepared
            .resident
            .coverage()
            .unsettled_result_ids
            .contains(&result.result_identity));
        assert_eq!(prepared.resident.scope(), &scope());
        Ok(())
    })
}

#[test]
fn joint_receipt_recovery_prepares_tasks_work_inputs_and_episodes_from_one_view() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| {
        write_task(tx, &state, &task("same-view-running", TaskStatus::Running))?;
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        let prepared =
            crate::runtime_db::input_preparation::prepare_original_input_recovery_with_work_tx(
                tx,
                &scope(),
                &state,
                &request,
                16,
                &budget,
                &crate::runtime::resident_work::WorkCaptureRequest::current(),
                &TaskCaptureRequest::current(),
            )?;
        assert_eq!(prepared.facts.state.as_ref(), Some(&state));
        assert!(prepared.episodes.belongs_to(&scope().agent_id, 1));
        assert!(prepared.inputs.belongs_to(&scope()));
        assert_eq!(
            prepared.pending.metadata.facts.identity,
            prepared.facts.identity
        );
        assert_eq!(prepared.pending.metadata.facts.state, prepared.facts.state);
        assert_eq!(
            prepared.pending.metadata.facts.control_revision,
            prepared.facts.control_revision
        );
        assert_eq!(prepared.work.as_ref().unwrap().scope(), &scope());
        let tasks = prepared.tasks.as_ref().unwrap();
        assert_eq!(tasks.scope(), &scope());
        let captured = tasks.capture(&TaskCaptureRequest::current())?;
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, "same-view-running"))?
            .is_some());
        assert!(captured
            .coverage
            .active_task_ids
            .contains("same-view-running"));
        Ok(())
    })
}

#[test]
fn joint_current_task_keeps_legacy_detail_work_owner_without_adopting_work() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| {
        let mut legacy = task("legacy-current", TaskStatus::Running);
        legacy.detail = Some(serde_json::json!({"work_item_id":"legacy-detail-work"}));
        write_task(tx, &state, &legacy)?;
        let prepared = prepare_original_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
        )?;
        assert_eq!(
            prepared.work_roots,
            BTreeSet::from(["legacy-detail-work".into()])
        );
        let current = prepared.resident.capture(&TaskCaptureRequest::current())?;
        let record = current
            .lookup(&TaskKey::new(TaskFamily::Task, &legacy.id))?
            .unwrap();
        let CanonicalTaskFact::Task(stored) = &record.fact else {
            panic!("expected Task")
        };
        assert!(stored.work_item_id.is_none());
        assert_eq!(stored.effective_work_item_id(), Some("legacy-detail-work"));
        // This helper selects an owning dependency; it cannot grant Work body.
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM work_domain_records WHERE record_id='legacy-detail-work'",
                [],
                |row| row.get::<_, usize>(0),
            )?,
            0
        );
        Ok(())
    })
}

#[test]
fn current_source_header_reserves_real_metadata_before_decode_and_releases_it() -> Result<()> {
    let (_dir, db, _state, budget) = fixture()?;
    db.transaction(|tx| {
        let original_scope = scope();
        let (source, _) = append_message_tx(tx, &result_message("header-task", "bound-header"))?;
        let baseline = budget.report()?.owned.metadata_estimate_bytes;
        let mut material = TaskSelectorMaterial::new(&original_scope, &budget, 4096, 4 * 1024 * 1024, None);
        let header = current_source_header_tx(tx, &source.id, &mut material)?.unwrap();
        assert_eq!(header.5.as_deref(), Some("header-task"));
        assert_eq!(header.6.as_deref(), Some("header-task"));
        assert!(budget.report()?.owned.metadata_estimate_bytes > baseline);
        drop(header);
        assert!(budget.report()?.owned.metadata_estimate_bytes > baseline);
        drop(material);
        assert_eq!(budget.report()?.owned.metadata_estimate_bytes, baseline);

        // Oversized malformed metadata must fail before row.get::<String>.
        // A binding may name a nonpending row, so no pending charge is assumed.
        let long_id = "x".repeat(64 * 1024);
        tx.execute(
            "UPDATE messages SET payload_json=json_set(payload_json,'$.id',7,'$.task_id',?2,'$.origin.task_id',?2) WHERE message_id=?1",
            rusqlite::params![source.id, long_id],
        )?;
        let mut small = TaskSelectorMaterial::new(&original_scope, &budget, 4096, 4096, None);
        let error = current_source_header_tx(tx, &source.id, &mut small).err().unwrap();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted("current selectors"))));
        drop(small);
        assert_eq!(budget.report()?.owned.metadata_estimate_bytes, baseline);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_records WHERE record_id='header-task'",[],|row|row.get::<_,usize>(0))?,0);
        Ok(())
    })
}

#[test]
fn pending_task_selector_copies_are_bounded_before_qualification_and_do_not_leak_guards(
) -> Result<()> {
    let (_dir, db, mut state, budget) = fixture()?;
    let long_id = "p".repeat(64 * 1024);
    let source = db.transaction(|tx| {
        append_pending(
            tx,
            &mut state,
            &result_message(&long_id, "pending-large-id"),
        )
    })?;
    let baseline = budget.report()?.owned.metadata_estimate_bytes;
    let error = db
        .transaction(|tx| {
            prepare_bootstrap_task_domains_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                std::slice::from_ref(&source),
                4096,
                4096,
                &budget,
            )
        })
        .err()
        .unwrap();
    assert!(matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::ResourceExhausted("current selectors"))
    ));
    assert_eq!(budget.report()?.owned.metadata_estimate_bytes, baseline);
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_domain_records WHERE record_id=?1",
                [&long_id],
                |row| row.get::<_, usize>(0)
            )?,
            0
        );
        Ok(())
    })
}

fn result_task_for_joint(id: &str, source_id: &str) -> TaskRecord {
    let mut value = task(id, TaskStatus::Completed);
    value.kind = TaskKind::AgentMessageWait;
    value.parent_message_id = Some(source_id.into());
    value.work_item_id = Some("asserted-result-work".into());
    value.detail = Some(serde_json::json!({"rejoin_obligation_id":id,
        "rejoin_generation":1,"parent_turn_id":"asserted-result-parent-turn"}));
    value
}
fn qualified_pending_result(
    tx: &Transaction<'_>,
    state: &mut AgentState,
    template: &TaskRecord,
) -> Result<(MessageEnvelope, TaskResultSettlementRecord)> {
    let mut source = result_message(&template.id, template.parent_message_id.as_deref().unwrap());
    source.work_item_id = template.work_item_id.clone();
    let source = append_pending(tx, state, &source)?;
    let result =
        TaskResultSettlementRecord::pending(template, &source, at())?.context("pending")?;
    let keys = BTreeSet::from([TaskKey::new(
        TaskFamily::ResultSettlement,
        &result.result_identity,
    )]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    upsert_pending_tx(tx, &result)?;
    register_task_writes_tx(tx, &proof, &keys, state, &load_request())?;
    Ok((source, result))
}

#[test]
fn joint_invalid_result_uses_focused_relation_without_raw_parent_or_foreign_task_body() -> Result<()>
{
    for foreign in [false, true] {
        let (_dir, db, mut state, budget) = fixture()?;
        let template = result_task_for_joint("result-dependency", "result-dependency-message");
        let (source,result)=db.transaction(|tx| {
            if foreign {
                let mut other=template.clone();other.agent_id="foreign-owner".into();
                repositories::upsert_task_tx(tx,&other)?;
                // The predicate reads only its finite relation tuple; a foreign
                // Task's full canonical body is neither decoded nor adopted.
                tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.kind','not-a-task-kind') WHERE task_id=?1",[&other.id])?;
            }
            qualified_pending_result(tx,&mut state,&template)
        })?;
        let before = budget.report()?.observed_bytes;
        let prepared = db.transaction(|tx| {
            prepare_original_task_domains_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                std::slice::from_ref(&source),
                4096,
                4 * 1024 * 1024,
                &budget,
            )
        })?;
        let captured = prepared.resident.capture(&TaskCaptureRequest::current())?;
        assert!(matches!(
            &captured
                .result_dependency(&result.result_identity)?
                .relation,
            TaskResultDependencyRelation::Invalid { .. }
        ));
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, &template.id))
            .is_err());
        assert_eq!(
            prepared.input_roots,
            BTreeSet::from([InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                &source.id
            )])
        );
        assert!(prepared.work_roots.is_empty());
        assert!(budget.report()?.observed_bytes > before);
        drop(captured);
        drop(prepared);
        assert_eq!(budget.report()?.observed_bytes, before);
        db.transaction(|tx| {
            assert_eq!(
                tx.query_row(
                    "SELECT COUNT(*) FROM task_domain_records WHERE family='task' AND record_id=?1",
                    [&template.id],
                    |r| r.get::<_, usize>(0)
                )?,
                0
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn joint_qualified_result_keeps_actual_task_and_owning_dependencies() -> Result<()> {
    let (_dir, db, mut state, budget) = fixture()?;
    let template = result_task_for_joint("valid-result-dependency", "valid-result-message");
    let (source, result) = db.transaction(|tx| {
        write_task(tx, &state, &template)?;
        qualified_pending_result(tx, &mut state, &template)
    })?;
    let prepared = db.transaction(|tx| {
        prepare_bootstrap_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            std::slice::from_ref(&source),
            4096,
            4 * 1024 * 1024,
            &budget,
        )
    })?;
    let captured = prepared.resident.capture(&TaskCaptureRequest::current())?;
    assert!(matches!(
        &captured
            .result_dependency(&result.result_identity)?
            .relation,
        TaskResultDependencyRelation::QualifiedCurrent { .. }
    ));
    assert!(captured
        .lookup(&TaskKey::new(TaskFamily::Task, &template.id))?
        .is_some());
    assert!(prepared.input_roots.contains(&InputEvidenceKey::new(
        InputEvidenceFamily::Turn,
        "asserted-result-parent-turn"
    )));
    assert!(prepared.work_roots.contains("asserted-result-work"));
    drop(captured);
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn joint_result_reobserves_later_valid_unqualified_task_and_keeps_active_task_strict() -> Result<()>
{
    let (_dir, db, mut state, budget) = fixture()?;
    let template = result_task_for_joint("later-unqualified-task", "later-result-message");
    let (source, _) = db.transaction(|tx| qualified_pending_result(tx, &mut state, &template))?;
    db.transaction(|tx| repositories::upsert_task_tx(tx, &template).map(|_| ()))?;
    let error = db
        .transaction(|tx| {
            prepare_bootstrap_task_domains_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                std::slice::from_ref(&source),
                4096,
                4 * 1024 * 1024,
                &budget,
            )
        })
        .err()
        .unwrap();
    assert!(
        matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::NeedsPreparation(key))
        if key==&TaskKey::new(TaskFamily::Task,&template.id)),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);

    let (_dir, db, mut state, budget) = fixture()?;
    let mut running = result_task_for_joint("current-running", "current-running-result");
    running.status = TaskStatus::Running;
    let mut terminal = running.clone();
    terminal.status = TaskStatus::Completed;
    let (source, result) = db.transaction(|tx| {
        write_task(tx, &state, &running)?;
        qualified_pending_result(tx, &mut state, &terminal)
    })?;
    let prepared = db.transaction(|tx| {
        prepare_original_task_domains_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            std::slice::from_ref(&source),
            4096,
            4 * 1024 * 1024,
            &budget,
        )
    })?;
    let captured = prepared.resident.capture(&TaskCaptureRequest::current())?;
    assert!(matches!(
        &captured
            .result_dependency(&result.result_identity)?
            .relation,
        TaskResultDependencyRelation::Invalid { .. }
    ));
    assert!(captured.coverage.active_task_ids.contains(&running.id));
    // The independent active Task still owns these dependencies; Invalid only
    // suppresses the Result's asserted parent/Work edge.
    assert!(prepared.input_roots.contains(&InputEvidenceKey::new(
        InputEvidenceFamily::Turn,
        "asserted-result-parent-turn"
    )));
    assert!(prepared.work_roots.contains("asserted-result-work"));
    drop(captured);
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn joint_current_recovery_keeps_all_live_births_and_reply_roles_without_old_input_ownership(
) -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| {
        let tasks = (0..130).map(|n| {
            let mut value = task(&format!("recovery-born-{n:03}"), TaskStatus::Completed);
            value.parent_message_id = Some(format!("old-unselected-parent-{n:03}"));
            value
        }).collect::<Vec<_>>();
        let keys = tasks.iter().map(|row| TaskKey::new(TaskFamily::Task, &row.id)).collect();
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        for task in &tasks { repositories::upsert_task_tx(tx, task)?; }
        let mut load = load_request();
        load.record_limit = 65_536;
        load.byte_limit = 32 * 1024 * 1024;
        register_task_writes_tx(tx, &proof, &keys, &state, &load)?;
        let query = crate::runtime::task_queries::TaskQuerySelection {
            required_keys: BTreeSet::from([TaskKey::new(TaskFamily::Task, "recovery-born-129")]),
            output_task_ids: BTreeSet::from(["recovery-born-129".into()]),
            reply_message_ids: BTreeSet::from(["explicit-empty-reply".into()]),
            parent_turn_ids: Default::default(),
            birth_after: Some(0),
        };
        let inputs = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            selected_completion_brief_ids: Default::default(), interrupted_sources: Default::default(),
            windows: Vec::new(), mandatory_roots: Default::default(),
            record_limit: load.record_limit, byte_limit: load.byte_limit,
        };
        let prepared = crate::runtime_db::input_preparation::prepare_original_input_recovery_with_queries_tx(
            tx, &scope(), &state, &inputs, 16, &budget,
            &crate::runtime::resident_work::WorkCaptureRequest::current(),
            &TaskCaptureRequest::current(), Some(&query),
        )?;
        let domain = prepared.tasks.as_ref().unwrap();
        let captured = domain.capture_query(&query)?;
        assert_eq!(captured.born_task_ids_after(0)?.count(), 130);
        assert_eq!(captured.task_birth_cursor()?, 130);
        assert_eq!(captured.coverage.query.reply_task_ids.get("explicit-empty-reply"), Some(&BTreeSet::new()));
        assert!(captured.output_source("recovery-born-129")?.is_none());
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE record_id LIKE 'old-unselected-parent-%'", [], |row| row.get::<_, usize>(0))?, 0);
        drop((captured, prepared));
        let cold = crate::runtime_db::input_preparation::prepare_original_input_recovery_with_work_tx(
            tx, &scope(), &state, &inputs, 16, &budget,
            &crate::runtime::resident_work::WorkCaptureRequest::current(), &TaskCaptureRequest::current(),
        )?;
        let cold_tasks = cold.tasks.as_ref().unwrap();
        assert_eq!(cold_tasks.coverage().query.birth_after, None);
        assert!(cold_tasks.coverage().query.reply_task_ids.is_empty());
        assert!(cold_tasks.capture_query(&query).is_err());
        assert_eq!(cold_tasks.capture(&TaskCaptureRequest::current())?.records.len(), 128);
        Ok(())
    })
}
