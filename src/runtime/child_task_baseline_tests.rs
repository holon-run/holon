//! Exercise the actual Host producer, not a writer-only stand-in.
use super::*;
use crate::runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey};
use crate::runtime_db::task_snapshot::test_probe;
use crate::types::*;

#[tokio::test]
async fn actual_child_terminal_task_baseline_has_zero_writer_load_and_full_ack_oracle() -> Result<()>
{
    let (_home, host, parent) = super::super::tests::support::host_backed_test_runtime().await;
    let _pause = parent.pause_run_loop_for_monitor_test().await;
    host.create_named_agent("baseline-child-target", None)
        .await?;
    let at = chrono::Utc::now();
    let task = TaskRecord {
        id: "task:actual-baseline-producer".into(),
        agent_id: parent.agent_state().await?.id,
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("actual producer baseline".into()),
        detail: Some(serde_json::json!({
            "target_agent_id":"baseline-child-target","created_new_subagent":false
        })),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "actual producer baseline".into(),
            prompt: "accepted".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some("baseline-child-target".into()),
            created_new_subagent: false,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    super::super::write_child_task_for_test(&parent, &task).await?;
    let run = super::super::register_child_monitor_for_test(
        &parent,
        &task,
        Some("baseline-child-target"),
        None,
        None,
        false,
    )
    .await?;
    let mut terminal = task.clone();
    terminal.status = TaskStatus::Completed;
    terminal.updated_at = chrono::Utc::now();
    let mut message = MessageEnvelope::new(
        task.agent_id.clone(),
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "actual producer result".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.created_at = terminal.updated_at;
    message.task_id = Some(task.id.clone());
    message.turn_id = Some(crate::ids::turn_id());
    terminal.parent_message_id = Some(message.id.clone());
    let frozen = run.source.freeze_terminal(
        &terminal,
        &message,
        Some("baseline-child-target"),
        None,
        &[],
        None,
    )?;
    let mut request = TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    request.capture.required_keys.insert(TaskKey::new(
        TaskFamily::Task,
        "task:producer-exact-absence",
    ));
    let before = parent
        .inner
        .mutation_owner
        .capture()?
        .context("registered producer root missing")?;
    let before_tasks = before
        .tasks
        .as_ref()
        .context("registered Task root missing")?;
    // ACK recovery selects the frozen command's install keys plus live roles,
    // not the expanded preparation request used to trigger the reader miss.
    let oracle_queries = parent.inner.task_queries.snapshot(
        &run.source.address().scope,
        before.token.owner_epoch,
        &parent.inner.input_evidence_budget,
        request.record_limit,
        request.byte_limit,
    )?;
    let mut oracle_request = request.clone();
    oracle_request.query = oracle_queries.selection.clone();
    oracle_request.capture = TaskCaptureRequest::current();
    oracle_request
        .capture
        .required_keys
        .insert(TaskKey::new(TaskFamily::Task, &task.id));
    oracle_request
        .capture
        .required_keys
        .extend(oracle_queries.selection.required_keys.iter().cloned());
    let durable_revision: u64 = host.runtime_db().connection()?.query_row(
        "SELECT revision FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![
            task.agent_id,
            run.source.address().scope.identity_incarnation
        ],
        |row| row.get(0),
    )?;
    assert_eq!(
        before_tasks.coverage().revision,
        durable_revision,
        "the borrowed root is current by revision, not an OCC conflict"
    );
    assert!(
        before_tasks.capture(&request.capture).is_err(),
        "the same-revision root must fail the expanded selection"
    );
    let bridge = parent
        .inner
        .host_bridge
        .as_ref()
        .context("Host bridge missing")?;
    let mut pending = None;
    let (read, loads) = test_probe::observe(bridge.commit_original_child_task_terminal(
        &run.source,
        &frozen,
        &mut pending,
        &request,
    ))
    .await;
    let read = read?;
    assert_eq!(loads.writer_loads, 0, "{loads:?}");
    assert_eq!(loads.borrowed_roots, 1, "{loads:?}");
    assert!(
        loads.reader_loads > 0,
        "the typed miss must prepare on the reader: {loads:?}"
    );
    assert!(
        loads.writer_attempts >= 3,
        "cold preflight, rejected borrowed root, and reader baseline must all be attempted: {loads:?}"
    );
    assert!(
        read.ack.task_changed,
        "the actual producer performed its terminal write"
    );
    assert!(matches!(
        &read.ack.result,
        crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Delivered { message_id }
            if message_id == &message.id
    ));
    let original = pending
        .as_ref()
        .context("frozen producer command missing")?;
    let raw: String = host.runtime_db().connection()?.query_row(
        "SELECT outcome_json FROM input_mutation_receipts WHERE command_nonce=?1",
        [original.descriptor().command_nonce()],
        |row| row.get(0),
    )?;
    let outcome: crate::runtime_db::input_mutation_receipts::InputMutationOutcome =
        serde_json::from_str(&raw)?;
    let InputMutationResult::ChildTaskTerminal { ack } = outcome.result else {
        anyhow::bail!("actual producer returned another receipt family");
    };
    assert_eq!(read.ack, ack, "whole actual ACK equals its durable receipt");
    let root = parent
        .inner
        .mutation_owner
        .capture()?
        .context("installed root missing")?;
    let installed = root.tasks.as_ref().context("installed Task ACK missing")?;
    let oracle = host.runtime_db().read_for_preparation(|tx| {
        crate::runtime_db::task_snapshot::load_task_domain_snapshot_with_budget_tx(
            tx,
            &run.source.address().scope,
            &oracle_request,
            &parent.inner.input_evidence_budget,
        )?
        .require_current()
    })?;
    assert_eq!(installed.coverage(), oracle.resident.coverage());
    assert_eq!(
        installed.selected_install_records(),
        oracle.resident.selected_install_records()
    );
    assert_eq!(
        installed.selected_install_stop_records(),
        oracle.resident.selected_install_stop_records()
    );
    let replay = bridge
        .commit_original_child_task_terminal(&run.source, &frozen, &mut pending, &request)
        .await?;
    assert_eq!(
        replay.ack, read.ack,
        "replay preserves the complete actual ACK"
    );
    run.source.acknowledge(&read.ack)?;
    drop((pending, run, _pause));
    host.shutdown().await?;
    Ok(())
}
