use super::*;
use crate::types::AgentInvocationReceipt;
use anyhow::{Context, Result};

struct ChildGate {
    started: Notify,
    release: Notify,
    first: AtomicBool,
}
#[async_trait]
impl AgentProvider for ChildGate {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if request
            .continuation_scope_id
            .as_ref()
            .is_some_and(|scope| scope.as_str() != "task-input-parent")
            && !self.first.swap(true, Ordering::SeqCst)
        {
            self.started.notify_one();
            self.release.notified().await;
        }
        StubProvider::new("original child callback complete")
            .complete_turn(request)
            .await
    }
}
async fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeHost,
    Arc<ChildGate>,
    RuntimeHandle,
    AgentInvocationReceipt,
    RuntimeHandle,
)> {
    let home = tempdir()?;
    write_test_model_config(home.path());
    let mut config = AppConfig::load_with_home(Some(home.path().into()))?;
    config.default_agent_id = "task-input-parent".into();
    config.user_home_dir = None;
    config.workspace_dir = home.path().join("empty-task-input-workspace");
    fs::create_dir(&config.workspace_dir)?;
    let gate = Arc::new(ChildGate {
        started: Notify::new(),
        release: Notify::new(),
        first: AtomicBool::new(false),
    });
    let host = RuntimeHost::new_with_provider(config, gate.clone())?;
    host.ensure_default_agent_identity()?;
    let parent = host.default_runtime().await?;
    let created = invoke_new_subagent(
        &parent,
        "original child input gate".into(),
        AuthorityClass::ExternalEvidence,
        None,
        None,
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(5), gate.started.notified())
        .await
        .context("real child dispatch did not reach provider")?;
    let child = host.get_or_create_agent(&created.agent_id).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if parent
                .original_task_input_child_binding_for_test(
                    &created.task_handle.task_id,
                    "original follow-up",
                    AuthorityClass::ExternalEvidence,
                )
                .await
                .is_ok()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("actual original producer target did not become bound")?;
    parent
        .quiesce_original_task_input_resources_for_test()
        .await?;
    child
        .quiesce_original_task_input_resources_for_test()
        .await?;
    Ok((home, host, gate, parent, created, child))
}
fn followups(child: &RuntimeHandle) -> Result<Vec<MessageEnvelope>> {
    Ok(child
        .storage()
        .read_recent_messages(100)?
        .into_iter()
        .filter(|m| {
            m.metadata
                .as_ref()
                .and_then(|v| v.get("followup_via"))
                .and_then(Value::as_str)
                == Some("task_input")
        })
        .collect())
}
fn audit_count(parent: &RuntimeHandle) -> Result<usize> {
    Ok(parent.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE kind='task_input_delivered'",
        [],
        |r| r.get(0),
    )?)
}
async fn finish(host: &RuntimeHost, gate: &ChildGate) -> Result<()> {
    gate.release.notify_one();
    host.shutdown().await
}
fn message(binding: &crate::runtime::OriginalTaskInputChildBinding) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        &binding.child_scope().agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: binding.task_id().into(),
        },
        AuthorityClass::ExternalEvidence,
        Priority::Normal,
        MessageBody::Text {
            text: "original follow-up".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.metadata = Some(
        json!({"delegated_task_id":binding.task_id(),"parent_agent_id":binding.parent_id(),"child_agent_id":binding.child_scope().agent_id,"followup_via":"task_input","delegated_authority_class":AuthorityClass::ExternalEvidence}),
    );
    message
}
#[tokio::test]
async fn actual_original_child_task_input_delivers_runtime_followup_with_same_parent_task_and_keeps_original_authority(
) -> Result<()> {
    let (_home, host, gate, parent, created, child) = fixture().await?;
    let result: Result<()> = async {
        let before = followups(&child)?.len();
        let audits = audit_count(&parent)?;
        let task = parent
            .runtime_db()
            .tasks()
            .latest(&created.task_handle.task_id)?
            .context("actual Task absent")?;
        assert!(task.is_child_agent_task());
        assert_eq!(task.status, TaskStatus::Running);
        let read = parent
            .task_input_with_trust(
                &task.id,
                "original follow-up",
                &AuthorityClass::ExternalEvidence,
            )
            .await?;
        assert!(read.accepted_input);
        assert_eq!(read.input_target.as_deref(), Some("child_followup"));
        assert_eq!(read.bytes_written, Some(18));
        assert_eq!(read.task.task_id, task.id);
        let mut rows = followups(&child)?;
        assert_eq!(rows.len(), before + 1);
        let actual = rows.pop().unwrap();
        assert_eq!(
            actual.body,
            MessageBody::Text {
                text: "original follow-up".into()
            }
        );
        assert_eq!(actual.authority_class, AuthorityClass::ExternalEvidence);
        assert_eq!(actual.kind, MessageKind::InternalFollowup);
        assert_eq!(
            actual.delivery_surface,
            Some(MessageDeliverySurface::RuntimeSystem)
        );
        assert_eq!(
            actual.admission_context,
            Some(AdmissionContext::RuntimeOwned)
        );
        assert_eq!(
            actual.origin,
            MessageOrigin::Task {
                task_id: task.id.clone()
            }
        );
        let metadata = actual.metadata.as_ref().unwrap();
        assert_eq!(metadata["parent_agent_id"], parent.instance_agent_id());
        assert_eq!(metadata["child_agent_id"], child.instance_agent_id());
        assert_eq!(metadata["delegated_task_id"], task.id);
        assert_eq!(audit_count(&parent)?, audits + 1);
        let identity = host.agent_identity_record(&created.agent_id)?.unwrap();
        assert_eq!(
            identity.parent_agent_id.as_deref(),
            Some(parent.instance_agent_id())
        );
        assert_eq!(
            identity.delegated_from_task_id.as_deref(),
            Some(task.id.as_str())
        );
        assert_eq!(identity.incarnation, child.instance_incarnation());
        let bytes = axum::body::Bytes::from_owner(read.into_json_wire()?);
        let json: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(json["accepted_input"], true);
        drop(bytes);
        Ok(())
    }
    .await;
    let closed = finish(&host, &gate).await;
    result?;
    closed
}
#[tokio::test]
async fn original_child_input_rejects_actual_identity_successor_parent_or_creator_drift_before_new_followup(
) -> Result<()> {
    for mode in ["incarnation", "parent", "creator"] {
        let (_home, host, gate, parent, created, child) = fixture().await?;
        let original = host.agent_identity_record(&created.agent_id)?.unwrap();
        let result: Result<()> = async {
            let binding = parent
                .original_task_input_child_binding_for_test(
                    &created.task_handle.task_id,
                    "original follow-up",
                    AuthorityClass::ExternalEvidence,
                )
                .await?;
            let before = followups(&child)?;
            let audits = audit_count(&parent)?;
            let mut changed = original.clone();
            match mode {
                "incarnation" => changed.incarnation += 1,
                "parent" => changed.parent_agent_id = Some("another-parent".into()),
                _ => changed.delegated_from_task_id = Some("another-creation-task".into()),
            };
            host.runtime_db().agent_identities().upsert(&changed)?;
            // This corruption changes canonical SQL only, not Host's original cache.
            assert_eq!(
                host.runtime_db()
                    .agent_identities()
                    .latest(&created.agent_id)?
                    .as_ref(),
                Some(&changed)
            );
            assert_eq!(
                host.agent_identity_record(&created.agent_id)?.as_ref(),
                Some(&original)
            );
            let value = host
                .bridge()
                .enqueue_original_task_input_followup(
                    child.clone(),
                    binding,
                    message_from_identity(
                        &created.agent_id,
                        parent.instance_agent_id(),
                        &created.task_handle.task_id,
                    ),
                )
                .await;
            assert!(
                !value.as_ref().is_ok_and(|accepted| *accepted),
                "{mode}: original callback crossed identity association"
            );
            assert_eq!(followups(&child)?, before);
            assert_eq!(audit_count(&parent)?, audits);
            assert_eq!(
                host.runtime_db()
                    .agent_identities()
                    .latest(&created.agent_id)?
                    .as_ref(),
                Some(&changed)
            );
            assert_eq!(
                host.agent_identity_record(&created.agent_id)?.as_ref(),
                Some(&original)
            );
            Ok(())
        }
        .await;
        host.runtime_db().agent_identities().upsert(&original)?;
        let closed = finish(&host, &gate).await;
        result?;
        closed?;
    }
    Ok(())
}
fn message_from_identity(child: &str, parent: &str, task: &str) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        child,
        MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: task.into(),
        },
        AuthorityClass::ExternalEvidence,
        Priority::Normal,
        MessageBody::Text {
            text: "original follow-up".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.metadata = Some(
        json!({"delegated_task_id":task,"parent_agent_id":parent,"child_agent_id":child,"followup_via":"task_input","delegated_authority_class":AuthorityClass::ExternalEvidence}),
    );
    message
}
#[tokio::test]
async fn original_child_input_retry_reloads_same_identity_after_idle_retirement_without_requiring_old_runtime_epoch(
) -> Result<()> {
    let (_home, host, gate, parent, created, child) = fixture().await?;
    let result: Result<()> = async {
        let binding = parent
            .original_task_input_child_binding_for_test(
                &created.task_handle.task_id,
                "original follow-up",
                AuthorityClass::ExternalEvidence,
            )
            .await?;
        let original = host.agent_identity_record(&created.agent_id)?.unwrap();
        let old_epoch = child.instance_owner_epoch();
        let loan = binding.for_attempt(&child, AuthorityClass::ExternalEvidence)?;
        let mut weak = None;
        loan.visit_material(|guard| weak = Some(Arc::downgrade(guard)))?;
        drop(loan);
        gate.release.notify_one();
        wait_for_terminal_task(&parent, &created.task_handle.task_id).await;
        let generation = observe_idle_candidate(&host, &created.agent_id).await;
        host.try_retire_idle_runtime(&created.agent_id, generation)
            .await?;
        assert_eq!(
            host.agent_identity_record(&created.agent_id)?.as_ref(),
            Some(&original)
        );
        let delivered = message(&binding);
        let id = delivered.id.clone();
        assert!(
            host.bridge()
                .enqueue_original_task_input_followup(child.clone(), binding.clone(), delivered)
                .await?
        );
        let replacement = host
            .try_get_loaded_runtime(&created.agent_id)
            .await
            .context("same-incarnation replacement absent")?;
        assert_eq!(replacement.instance_incarnation(), original.incarnation);
        assert_ne!(replacement.instance_owner_epoch(), old_epoch);
        let rows = followups(&replacement)?;
        assert_eq!(rows.iter().filter(|m| m.id == id).count(), 1);
        assert_eq!(
            host.agent_identity_record(&created.agent_id)?.as_ref(),
            Some(&original)
        );
        assert!(weak.as_ref().unwrap().upgrade().is_some());
        drop(binding);
        assert!(weak.unwrap().upgrade().is_none());
        assert!(parent
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&created.agent_id)?
            .is_none());
        Ok(())
    }
    .await;
    let closed = finish(&host, &gate).await;
    result?;
    closed
}

#[tokio::test]
async fn admitted_original_child_task_input_finishes_after_caller_drop_and_parent_closing(
) -> Result<()> {
    let (_home, host, gate, parent, created, child) = fixture().await?;
    let result: Result<()> = async {
        let held = child.reserve_original_task_input_owner_for_test().await?;
        let before = followups(&child)?.len();
        let audits = audit_count(&parent)?;
        let mut caller = Box::pin(parent.task_input_with_trust(
            &created.task_handle.task_id,
            "original follow-up",
            &AuthorityClass::ExternalEvidence,
        ));
        assert!(
            futures_util::poll!(&mut caller).is_pending(),
            "registered original request must wait for its child owner"
        );
        drop(caller);
        parent.close_original_task_input_admission_for_test();
        assert!(parent
            .task_input_with_trust(
                &created.task_handle.task_id,
                "another request",
                &AuthorityClass::ExternalEvidence
            )
            .await
            .is_err());
        drop(held);
        parent.join_owned_mutations().await?;
        assert_eq!(followups(&child)?.len(), before + 1);
        assert_eq!(audit_count(&parent)?, audits + 1);
        let identity = host.agent_identity_record(&created.agent_id)?.unwrap();
        assert_eq!(identity.incarnation, child.instance_incarnation());
        assert_eq!(identity.status, AgentRegistryStatus::Active);
        Ok(())
    }
    .await;
    let closed = finish(&host, &gate).await;
    result?;
    closed
}

#[tokio::test]
async fn original_child_queue_precheck_borrows_only_its_real_attempt_loan_before_raw_native_decode(
) -> Result<()> {
    let (_home, host, gate, parent, created, child) = fixture().await?;
    let result: Result<()> = async {
        let binding=parent.original_task_input_child_binding_for_test(&created.task_handle.task_id,"original follow-up",AuthorityClass::ExternalEvidence).await?;
        let loan=binding.for_attempt(&child,AuthorityClass::ExternalEvidence)?;
        let _parent=parent.reserve_original_task_input_owner_for_test().await?;
        let ledger=host.bridge().input_evidence_budget()?;
        let foreign=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
        let db=host.runtime_db();
        db.connection()?.execute_batch("ALTER TABLE queue_entries RENAME TO task_input_forbidden_queue")?;
        let wrong_ledger=db.original_task_input_queue_observation("unused",&loan,child.instance_owner_epoch(),&foreign);
        let wrong_epoch=db.original_task_input_queue_observation("unused",&loan,child.instance_owner_epoch()+1,&ledger);
        db.connection()?.execute_batch("ALTER TABLE task_input_forbidden_queue RENAME TO queue_entries")?;
        for value in [wrong_ledger,wrong_epoch] {
            let error=value.err().context("foreign original attempt read forbidden Queue")?;
            assert!(format!("{error:#}").contains("original attempt scope/epoch/ledger"),"{error:#}");
        }
        let id=crate::ids::message_id();let at=chrono::Utc::now();
        let row=QueueEntryRecord{message_id:id.clone(),agent_id:created.agent_id.clone(),priority:Priority::Normal,status:QueueEntryStatus::Queued,created_at:at,updated_at:at};
        db.queue_entries().upsert(&row)?;
        let baseline=ledger.report()?.observed_bytes;
        let counts=db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM current_read_members)",[],|r|Ok((r.get::<_,usize>(0)?,r.get::<_,usize>(1)?)))?;
        assert_eq!(db.original_task_input_queue_observation(&id,&loan,child.instance_owner_epoch(),&ledger)?,(false,Some(at)));
        assert_eq!(ledger.report()?.observed_bytes,baseline);
        let raw="!".repeat(9*1024*1024);
        db.connection()?.execute("UPDATE queue_entries SET payload_json=?2 WHERE message_id=?1",rusqlite::params![id,raw])?;
        let error=db.original_task_input_queue_observation(&id,&loan,child.instance_owner_epoch(),&ledger).unwrap_err();
        assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))),"raw: {error:#}");
        assert_eq!(ledger.report()?.observed_bytes,baseline);
        let mut value=serde_json::to_value(&row)?;
        let dense=(0..10000).map(|n|(format!("actual-key-{n}"),serde_json::Value::Null)).collect::<serde_json::Map<_,_>>();
        value["ignored-but-native"]=serde_json::Value::Object(dense);let dense=serde_json::to_string(&value)?;
        assert!(dense.len()*8<64*1024*1024);
        db.connection()?.execute("UPDATE queue_entries SET payload_json=?2 WHERE message_id=?1",rusqlite::params![id,dense])?;
        let error=db.original_task_input_queue_observation(&id,&loan,child.instance_owner_epoch(),&ledger).unwrap_err();
        assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))),"native: {error:#}");
        assert_eq!(ledger.report()?.observed_bytes,baseline);
        assert_eq!(db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM current_read_members)",[],|r|Ok((r.get::<_,usize>(0)?,r.get::<_,usize>(1)?)))?,counts);
        assert_eq!(db.connection()?.execute("DELETE FROM queue_entries WHERE message_id=?1",[&id])?,1);
        Ok(())
    }.await;
    let closed = finish(&host, &gate).await;
    result?;
    closed
}

#[tokio::test]
async fn original_child_input_lost_whole_ack_retains_its_host_loan_through_fresh_receipt_and_deleting_replay(
) -> Result<()> {
    let (_home, host, gate, parent, created, child) = fixture().await?;
    let result: Result<()> = async {
        let binding = parent
            .original_task_input_child_binding_for_test(
                &created.task_handle.task_id,
                "original follow-up",
                AuthorityClass::ExternalEvidence,
            )
            .await?;
        let loan = binding.for_attempt(&child, AuthorityClass::ExternalEvidence)?;
        let mut weak = None;
        loan.visit_material(|guard| weak = Some(Arc::downgrade(guard)))?;
        let delivered = message(&binding);
        let id = delivered.id.clone();
        child
            .original_task_input_child_lost_ack_for_test(delivered, loan.clone())
            .await?;
        assert_eq!(
            followups(&child)?
                .iter()
                .filter(|message| message.id == id)
                .count(),
            1
        );
        assert!(weak.as_ref().unwrap().upgrade().is_some());
        drop((loan, binding));
        assert!(weak.unwrap().upgrade().is_none());
        Ok(())
    }
    .await;
    let closed = finish(&host, &gate).await;
    result?;
    closed
}
