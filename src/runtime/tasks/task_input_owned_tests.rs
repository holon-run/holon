use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::types::{CommandTaskSpec, WorkspaceEntry};
use std::time::Duration;
async fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeHandle,
    TaskRecord,
    Arc<crate::runtime::control_cancellation::ControlJoinLease>,
)> {
    let home = tempdir()?;
    let workspace = home.path().join("input-workspace");
    std::fs::create_dir(&workspace)?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-task-input",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let entry = WorkspaceEntry::new("original-input-workspace", workspace.clone(), None);
    runtime.attach_workspace(&entry).await?;
    runtime
        .enter_workspace(
            &entry,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            None,
            None,
        )
        .await?;
    let task=runtime.schedule_command_task("actual input process".into(),CommandTaskSpec {
        cmd:"IFS= read -r line; printf '%s' \"$line\" > accepted-input; while :; do sleep 1; done".into(),
        workdir:Some(workspace.to_string_lossy().into()),shell:None,login:false,tty:false,yield_time_ms:10,
        max_output_tokens:Some(128),accepts_input:true,terminal_reentry:false,
    },AuthorityClass::OperatorInstruction).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if runtime
                .runtime_db()
                .tasks()
                .latest(&task.id)?
                .is_some_and(|task| task.status == TaskStatus::Running)
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let task = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
    let lease = runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .and_then(|h| h.control_lease())
        .context("actual command lease missing")?;
    Ok((home, runtime, task, lease))
}
async fn written(home: &tempfile::TempDir) -> Result<()> {
    let path = home.path().join("input-workspace/accepted-input");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(&path).is_ok_and(|s| s == "hello") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("actual stdin reader did not observe acknowledged bytes")?;
    Ok(())
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events WHERE kind='task_input_delivered'),(SELECT COUNT(*) FROM current_read_members WHERE family='event'),(SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
async fn cleanup(lease: Arc<crate::runtime::control_cancellation::ControlJoinLease>) -> Result<()> {
    lease.cancel();
    tokio::time::timeout(Duration::from_secs(5), lease.join()).await??;
    Ok(())
}
#[tokio::test]
async fn actual_command_input_keeps_original_ack_audit_and_result_wire_guards_without_task_sql(
) -> Result<()> {
    let (home, runtime, task, lease) = fixture().await?;
    let result: Result<()> = async {
        let baseline=counts(&runtime)?;
        runtime.runtime_db().connection()?.execute_batch("ALTER TABLE tasks RENAME TO input_forbidden_tasks")?;
        let read=runtime.task_input(&task.id,"hello\n").await;
        runtime.runtime_db().connection()?.execute_batch("ALTER TABLE input_forbidden_tasks RENAME TO tasks")?;
        let read=read?;written(&home).await?;
        assert!(read.accepted_input);assert_eq!(read.input_target.as_deref(),Some("stdin"));assert_eq!(read.bytes_written,Some(6));
        assert_eq!(read.task,TaskStatusSnapshot::from_task_record(&task));
        assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?,Some(task.clone()));
        assert_eq!(counts(&runtime)?,(baseline.0+1,baseline.1+1,baseline.2+1,baseline.3+1));
        let root=runtime.inner.mutation_owner.capture()?.unwrap();let strong=Arc::strong_count(&root);
        let current=root.current_reads.as_ref().context("input Audit invalidated Current")?.capture(&crate::runtime::current_read_facts::CurrentReadRequest::current(64*1024*1024,65536))?;
        let event=current.events.iter().find_map(|r|match &r.fact {crate::runtime::current_read_facts::CurrentReadFact::Event(e) if e.kind=="task_input_delivered"=>Some(e),_=>None}).context("published TaskInput Audit absent")?;
        assert_eq!(event.data,serde_json::json!({"task_id":task.id,"task_kind":task.kind,"bytes_written":6,"input_target":"stdin"}));
        let weak=Arc::downgrade(&read.material);let usage=read.usage.0;
        let expected=serde_json::to_vec(&*read)?;
        let before=runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let body=axum::body::Bytes::from_owner(read.into_json_wire()?);assert_eq!(body.as_ref(),expected);
        let clone=body.clone();drop(body);assert!(weak.upgrade().is_some());assert_eq!(Arc::strong_count(&root),strong);
        drop(clone);assert!(weak.upgrade().is_none());
        assert_eq!(runtime.inner.input_evidence_budget.report()?.observed_bytes,before-usage);
        Ok(())
    }.await;
    let clean = cleanup(lease).await;
    result?;
    clean
}
#[tokio::test]
async fn admitted_task_input_survives_caller_drop_and_closing_until_actual_stdin_and_audit_ack(
) -> Result<()> {
    let (home, runtime, task, lease) = fixture().await?;
    let result: Result<()> = async {
        let owner = runtime.begin_mutation().await?;
        let baseline = counts(&runtime)?;
        let mut caller = Box::pin(runtime.task_input_with_trust(
            &task.id,
            "hello\n",
            &AuthorityClass::OperatorInstruction,
        ));
        assert!(
            futures_util::poll!(&mut caller).is_pending(),
            "original registered worker must wait on the held owner"
        );
        drop(caller);
        runtime.close_mutation_admission();
        assert!(runtime.task_input(&task.id, "second\n").await.is_err());
        drop(owner);
        runtime.join_owned_mutations().await?;
        written(&home).await?;
        assert_eq!(
            counts(&runtime)?,
            (
                baseline.0 + 1,
                baseline.1 + 1,
                baseline.2 + 1,
                baseline.3 + 1
            )
        );
        assert_eq!(
            runtime.runtime_db().tasks().latest(&task.id)?,
            Some(task.clone())
        );
        Ok(())
    }
    .await;
    let clean = cleanup(lease).await;
    result?;
    clean
}
#[tokio::test]
async fn task_input_audit_failure_preserves_actual_delivery_and_has_no_deleting_or_stateless_audit_authority(
) -> Result<()> {
    for mode in ["rollback", "deleting", "no_state"] {
        let (home, runtime, task, lease) = fixture().await?;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let old_state = root.state.clone();
        drop(root);
        let old_identity = runtime
            .runtime_db()
            .agent_identities()
            .latest(runtime.instance_agent_id())?
            .unwrap();
        let result: Result<()> = async {
            match mode {
                "rollback"=>runtime.runtime_db().connection()?.execute_batch("CREATE TRIGGER reject_input_audit BEFORE INSERT ON audit_events WHEN NEW.kind='task_input_delivered' BEGIN SELECT RAISE(ABORT,'original TaskInput Audit rollback'); END;")?,
                "deleting"=>{let mut identity=old_identity.clone();identity.status=crate::types::AgentRegistryStatus::Deleting;runtime.runtime_db().agent_identities().upsert(&identity)?;},
                _=>{runtime.runtime_db().connection()?.execute("DELETE FROM agent_states WHERE agent_id=?1",[runtime.instance_agent_id()])?;},
            }
            let baseline=counts(&runtime)?;
            let error=runtime.task_input(&task.id,"hello\n").await.err().context("TaskInput Audit incorrectly committed")?;
            if mode=="rollback" {assert!(format!("{error:#}").contains("original TaskInput Audit rollback"));}
            else {assert!(matches!(error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),Some(crate::runtime::current_read_facts::CurrentReadError::RetiredScope)),"{mode}: {error:#}");}
            written(&home).await?;assert_eq!(counts(&runtime)?,baseline);
            assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?,Some(task.clone()));Ok(())
        }.await;
        runtime
            .runtime_db()
            .connection()?
            .execute_batch("DROP TRIGGER IF EXISTS reject_input_audit")?;
        runtime
            .runtime_db()
            .agent_identities()
            .upsert(&old_identity)?;
        runtime
            .runtime_db()
            .transaction(|tx| crate::runtime_db::evidence::upsert_agent_state_tx(tx, &old_state))?;
        let clean = cleanup(lease).await;
        result?;
        clean?;
    }
    Ok(())
}
#[tokio::test]
async fn task_input_request_and_final_wire_each_reject_before_their_first_extra_copy() -> Result<()>
{
    let (_home, runtime, task, lease) = fixture().await?;
    let result: Result<()> = async {
        let baseline = counts(&runtime)?;
        let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let input = "x".repeat(9 * 1024 * 1024);
        let error = runtime
            .task_input(&task.id, &input)
            .await
            .err()
            .context("large first TaskInput copy accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<TaskReadError>(),
                Some(TaskReadError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(counts(&runtime)?, baseline);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            before
        );
        let unsupported = TaskRecord {
            id: crate::ids::task_id(),
            agent_id: runtime.instance_agent_id().into(),
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Running,
            created_at: runtime.now(),
            updated_at: runtime.now(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        };
        runtime
            .persist_task_transition(&unsupported, "original_input_unsupported")
            .await?;
        let read = runtime.task_input(&unsupported.id, "ignored").await?;
        assert!(!read.accepted_input);
        let weak = Arc::downgrade(&read.material);
        let own = crate::runtime::input_evidence_budget::serialized_bytes(&*read)? + 4096;
        let cap = usize::try_from(read.usage.0 + own - 1)?;
        let error = read
            .into_json_wire_with_limits((cap, 65536))
            .err()
            .context("wire reset source allowance")?;
        assert!(
            matches!(
                error.downcast_ref::<TaskReadError>(),
                Some(TaskReadError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert!(weak.upgrade().is_none());
        Ok(())
    }
    .await;
    let clean = cleanup(lease).await;
    result?;
    clean
}

#[tokio::test]
async fn actual_command_input_does_not_signal_a_live_original_handle_after_a_new_typed_task_generation(
) -> Result<()> {
    let (home, runtime, task, lease) = fixture().await?;
    let result: Result<()> = async {
        let mut next = task.clone();
        let detail = next.detail.get_or_insert_with(|| serde_json::json!({}));
        let old = detail
            .get("rejoin_generation")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        detail["rejoin_obligation_id"] = serde_json::json!(task.id);
        detail["rejoin_generation"] = serde_json::json!(old + 1);
        next.updated_at += chrono::Duration::milliseconds(1);
        runtime
            .persist_task_transition(&next, "task_input_generation_boundary")
            .await?;
        assert!(!lease.matches_task_generation(&next));
        let before = counts(&runtime)?;
        let read = runtime.task_input(&task.id, "hello\n").await?;
        assert!(!read.accepted_input);
        assert_eq!(read.bytes_written, None);
        assert_eq!(read.task, TaskStatusSnapshot::from_task_record(&next));
        assert!(!home.path().join("input-workspace/accepted-input").exists());
        assert_eq!(counts(&runtime)?, before);
        assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?, Some(next));
        Ok(())
    }
    .await;
    let clean = cleanup(lease).await;
    result?;
    clean
}
