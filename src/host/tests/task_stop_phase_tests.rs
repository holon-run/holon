use super::*;
use crate::{
    runtime::resident_work::{CanonicalWorkRecord, WorkCaptureRequest, WorkFamily, WorkKey},
    runtime_db::{
        task_snapshot::TaskDomainLoadRequest,
        task_stop::{TaskStopPhaseInstallMode, TaskStopResultOutcome},
        transitions::PreparedInputTransition,
    },
};
fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn source(
    host: &RuntimeHost,
    with_work: bool,
) -> Result<(
    Arc<crate::runtime_db::task_stop::TaskStopRecord>,
    Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
)> {
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: host.config().default_agent_id.clone(),
        identity_incarnation: 1,
    };
    host.runtime_db().transaction(|tx| {
        if crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
            .scope_facts()?
            .state
            .is_none()
        {
            crate::runtime_db::evidence::upsert_agent_state_tx(
                tx,
                &AgentState::new(&scope.agent_id),
            )?;
        }
        Ok(())
    })?;
    host.runtime_db().original_stop_result_host_fixture(
        &scope,
        &host.inner.input_evidence_budget,
        with_work,
    )
}
fn incomplete(host: &RuntimeHost, agent: &str) -> Result<()> {
    host.runtime_db().connection()?.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.active_complete',json('false'),'$.unsettled_complete',json('false'),'$.stop.pending_complete',json('false')) WHERE agent_id=?1",[agent])?;
    Ok(())
}
#[tokio::test]
async fn host_incomplete_result_actual_domains_publish_once_after_known_fault() -> Result<()> {
    let (_home, host) = test_host();
    let (source, _guard) = source(&host, true)?;
    let scope = &source.intent.task.scope;
    incomplete(&host, &scope.agent_id)?;
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    assert!(owner.capture()?.is_none());
    host.fail_next_external_mutation_publication();
    let p = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    assert!(host
        .bridge()
        .commit_task_stop_phase_metadata(p, request())
        .await
        .is_err());
    assert!(owner.capture().is_err());
    let reservation = owner.reserve().await?;
    assert!(
        reservation.recovery()?.unwrap().known.is_some(),
        "whole ACK must be Known before fault"
    );
    drop(reservation);
    let p = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    let read = host
        .bridge()
        .commit_task_stop_phase_metadata(p, request())
        .await?;
    assert_eq!(read.ack.install_mode, TaskStopPhaseInstallMode::Incomplete);
    assert!(matches!(
        read.ack.result.as_ref().unwrap().outcome,
        TaskStopResultOutcome::Delivered { .. }
    ));
    let root = owner.capture()?.unwrap();
    assert!(root.tasks.is_none());
    assert_eq!(root.state.pending, 1);
    assert!(root.input_evidence.is_some() && root.episodes.is_some());
    let captured = root
        .work
        .as_ref()
        .context("actual Work domain missing")?
        .capture(&WorkCaptureRequest::current())?;
    let wait = captured
        .lookup(&WorkKey::new(WorkFamily::Wait, "result-existing-wait"))?
        .context("actual Wait missing")?;
    let CanonicalWorkRecord::Wait(wait) = &wait.record else {
        unreachable!()
    };
    assert_eq!(wait.status, crate::types::WaitConditionStatus::Triggered);
    let count: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE evidence_id='frozen-stop-feedback'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1);
    assert_eq!(host.inner.input_evidence_budget.report()?.pending.count, 1);
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn host_no_state_result_unavailable_releases_only_its_new_ticket_without_ready() -> Result<()>
{
    let (_home, host) = test_host();
    let (source, _guard) = source(&host, false)?;
    let scope = &source.intent.task.scope;
    host.runtime_db().connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
    )?;
    let p = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    let read = host
        .bridge()
        .commit_task_stop_phase_metadata(p, request())
        .await?;
    assert_eq!(read.ack.install_mode, TaskStopPhaseInstallMode::NoState);
    assert!(matches!(
        read.ack.result.as_ref().unwrap().outcome,
        TaskStopResultOutcome::Unavailable { .. }
    ));
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    assert!(owner.capture()?.is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.pending.count, 0);
    let count: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE evidence_id='frozen-stop-feedback'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn host_result_receipt_failure_rolls_back_original_ticket_and_business_then_retries(
) -> Result<()> {
    let (_home, host) = test_host();
    let (source, _guard) = source(&host, true)?;
    let scope = &source.intent.task.scope;
    incomplete(&host, &scope.agent_id)?;
    host.runtime_db().connection()?.execute_batch("CREATE TRIGGER fail_host_stop_phase BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='original-frozen-result-phase' BEGIN SELECT RAISE(ABORT,'host-stop-receipt-fault'); END;")?;
    let p = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    let error = host
        .bridge()
        .commit_task_stop_phase_metadata(p, request())
        .await
        .err()
        .context("receipt fault ignored")?;
    assert!(format!("{error:#}").contains("host-stop-receipt-fault"));
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    assert!(owner.capture()?.is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.pending.count, 0);
    let count: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE evidence_id='frozen-stop-feedback'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    host.runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER fail_host_stop_phase;")?;
    let p = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    host.bridge()
        .commit_task_stop_phase_metadata(p, request())
        .await?;
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn cold_started_result_attaches_original_deferred_memory_without_runtime_or_task_hydration(
) -> Result<()> {
    let (_home, host) = test_host();
    let (source, _guard) = source(&host, true)?;
    let scope = &source.intent.task.scope;
    host.runtime_db()
        .original_cold_started_stop_host_fixture(scope)?;
    incomplete(&host, &scope.agent_id)?;
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    assert!(owner.capture()?.is_none());
    let p = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    host.bridge()
        .commit_task_stop_phase_metadata(p, request())
        .await?;
    assert!(host
        .bridge()
        .runtime_for_mutation(&owner.reserve().await?)
        .await?
        .is_none());
    let record:String=host.runtime_db().connection()?.query_row("SELECT state FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id='stop-host-original-dispatch-turn'",rusqlite::params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    assert_eq!(record, "pending");
    assert!(owner.capture()?.unwrap().tasks.is_none());
    let members:u64=host.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM dispatch_memory_auto_commands WHERE command_nonce='original-frozen-result-phase'",[],|r|r.get(0))?;
    assert_eq!(members, 1);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn host_normal_first_phase_ack_keeps_actual_live_query_roles_without_recovery_witness(
) -> Result<()> {
    let (_home, host) = test_host();
    let (source, _guard) = source(&host, false)?;
    let scope = &source.intent.task.scope;
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let role = runtime.register_task_query(crate::runtime::task_queries::TaskQuerySpec {
        birth_after: Some(0),
        output_task_ids: &[&source.intent.task.key.id],
        reply_message_ids: &["phase-live-empty-reply"],
        ..Default::default()
    })?;
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    let reservation = owner.reserve().await?;
    assert!(
        reservation.recovery()?.is_none(),
        "normal first ACK begins Ready"
    );
    drop(reservation);
    let prepared = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    let read = host
        .bridge()
        .commit_task_stop_phase_metadata(prepared, request())
        .await?;
    assert_eq!(read.ack.install_mode, TaskStopPhaseInstallMode::Current);
    let root = owner
        .capture()?
        .context("phase must publish its exact original owner")?;
    let tasks = root
        .tasks
        .as_ref()
        .context("Current phase Task domain missing")?;
    assert_eq!(tasks.coverage().query.birth_after, Some(0));
    assert!(tasks
        .coverage()
        .query
        .born_task_seqs
        .contains_key(&source.intent.task.key.id));
    assert_eq!(
        tasks
            .coverage()
            .query
            .reply_task_ids
            .get("phase-live-empty-reply"),
        Some(&Default::default())
    );
    let selected = tasks.capture_query(role.selection())?;
    assert!(selected.lookup(&source.intent.task.key)?.is_some());
    assert_eq!(
        selected
            .output_source(&source.intent.task.key.id)?
            .unwrap()
            .message
            .key
            .id,
        "frozen-stop-feedback"
    );
    drop((selected, root, read, role));
    host.shutdown().await?;
    Ok(())
}
