//! Real original Stop and typed Event ACKs; no FS deletion is authorized here.
use super::*;
use crate::runtime::current_read_facts::{CurrentReadError, CurrentReadKey, CurrentReadRequest};
use crate::runtime_db::{
    current_read_snapshot as reads,
    transitions::{AgentPostureTransitionCommand, AgentStateMutation},
};
use rusqlite::OptionalExtension;

fn event(f: &Fixture, id: &str) -> AuditEvent {
    let mut event = AuditEvent::legacy(
        "task_worktree_cleanup_retained",
        json!({
        "task_id":f.original.intent.task.key.id,"reason":"unknown_group","cleanup_trigger":"task_stop"}),
    );
    event.id = id.into();
    event.created_at = f.original.updated_at + chrono::Duration::seconds(1);
    event
}
fn phase(f: &Fixture, events: Vec<AuditEvent>) -> Result<PreparedInputTransition> {
    let ledger = &f.host.inner.input_evidence_budget;
    let observed = TaskStopWorktreeCleanupObserved::new(
        &f.original,
        f.original.updated_at + chrono::Duration::seconds(2),
        TaskStopWorktreeState::Retained,
        events,
        None,
        Vec::new(),
        &request(),
        ledger,
    )?;
    PreparedInputTransition::task_stop_worktree_cleanup_observed(
        &f.original,
        Arc::new(observed),
        &request(),
        ledger,
    )
}
fn commit(
    f: &Fixture,
    p: &PreparedInputTransition,
) -> Result<InputMutationExecution<crate::runtime_db::transitions::TransitionCommit>> {
    f.host
        .runtime_db()
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &work(),
            &f.host.inner.input_evidence_budget,
            &request(),
        )
}
fn current(f: &Fixture) -> Result<reads::CurrentReadSnapshot> {
    let state = f
        .host
        .runtime_db()
        .agent_states()
        .latest(&f.scope.agent_id)?
        .context("actual parent State")?;
    f.host.runtime_db().transaction(|tx| {
        reads::prepare_current_read_snapshot_tx(
            tx,
            &f.scope,
            &state,
            &CurrentReadRequest::current(64 * 1024 * 1024, 65536),
            &f.host.inner.input_evidence_budget,
        )
    })
}
fn seed_event(f: &Fixture, event: &AuditEvent) -> Result<()> {
    let state = f
        .host
        .runtime_db()
        .agent_states()
        .latest(&f.scope.agent_id)?
        .context("actual parent State")?;
    // Posture with an unchanged State is a business no-op and never appends Audit.
    let mut after = state.clone();
    after.last_wake_reason = Some(format!("actual-seed-{}", event.id));
    let prepared = PreparedInputTransition::posture(
        f.scope.clone(),
        format!("seed-{}", event.id),
        event.created_at,
        AgentPostureTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            expected_incarnation: Some(f.scope.identity_incarnation),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(state.clone())),
                record: Box::new(after.clone()),
            },
            audit_events: vec![event.clone()],
            fault: None,
        },
        None,
    )?;
    let InputMutationExecution::Committed { value, .. } = commit(f, &prepared)? else {
        anyhow::bail!("new seed must be an actual Posture ACK");
    };
    assert_eq!(value.effects.audit_events.len(), 1);
    assert_eq!(value.effects.audit_events[0].id, event.id);
    assert_eq!(
        f.host
            .runtime_db()
            .agent_states()
            .latest(&f.scope.agent_id)?,
        Some(after)
    );
    assert_eq!(native_birth(f, &event.id)?, member(f, &event.id)?);
    Ok(())
}
fn member(f: &Fixture, id: &str) -> Result<(u64, u64)> {
    f.host.runtime_db().connection()?.query_row(
        "SELECT identity_incarnation,sort_seq FROM current_read_members WHERE agent_id=?1 AND family='event' AND record_id=?2",
        rusqlite::params![f.scope.agent_id,id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(Into::into)
}

fn native_birth(f: &Fixture, id: &str) -> Result<(u64, u64)> {
    f.host.runtime_db().connection()?.query_row(
        "SELECT b.identity_incarnation,b.birth_seq FROM current_event_native_births b JOIN current_event_birth_cursors c ON c.agent_id=b.agent_id AND c.identity_incarnation=b.identity_incarnation WHERE b.agent_id=?1 AND b.event_id=?2 AND c.high_water>=b.birth_seq",
        rusqlite::params![f.scope.agent_id,id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(Into::into)
}

#[tokio::test]
async fn worktree_audit_joins_current_once_and_whole_replay_survives_body_denial() -> Result<()> {
    let f = Fixture::new().await?;
    let before = current(&f)?;
    let revision = before.resident.coverage().revision;
    let p = phase(&f, vec![event(&f, "actual-worktree-current-audit")])?;
    let baseline = f.host.inner.input_evidence_budget.report()?.observed_bytes;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p)? else {
        unreachable!()
    };
    assert!(
        f.path.exists(),
        "Retained Audit cannot remove the original worktree"
    );
    assert!(value.effects.agent_state.is_none() && value.effects.agent_identities.is_empty());
    assert_eq!(value.effects.audit_events.len(), 1);
    assert!(value.effects.current_read_facts.is_none());
    let facts = value
        .effects
        .current_read_maintenance
        .as_ref()
        .context("actual Current maintenance")?;
    assert_eq!(
        (facts.base_revision, facts.revision),
        (revision, revision + 1)
    );
    let resident = facts
        .resident()
        .context("complete Current after actual Event ACK")?;
    let request = CurrentReadRequest::current(64 * 1024 * 1024, 65536);
    let selected = resident.capture(&request)?;
    assert!(selected
        .records
        .iter()
        .any(|r| r.locator.key == CurrentReadKey::event("actual-worktree-current-audit")));
    assert_eq!(
        member(&f, "actual-worktree-current-audit")?,
        (
            f.scope.identity_incarnation,
            value.effects.audit_events[0].event_seq
        )
    );
    assert_eq!(
        native_birth(&f, "actual-worktree-current-audit")?,
        member(&f, "actual-worktree-current-audit")?
    );
    let count = f.audits()?;
    // Ordinary capture and exact receipt replay must use their owned facts.
    f.host.runtime_db().connection()?.execute_batch("ALTER TABLE queue_entries RENAME TO progress_queue_forbidden; ALTER TABLE audit_events RENAME TO progress_event_forbidden;")?;
    assert_eq!(
        resident.capture(&request)?.records.len(),
        selected.records.len()
    );
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit(&f, &p)?
    else {
        unreachable!()
    };
    assert_eq!(replay, receipt);
    f.host.runtime_db().connection()?.execute_batch("ALTER TABLE progress_queue_forbidden RENAME TO queue_entries; ALTER TABLE progress_event_forbidden RENAME TO audit_events;")?;
    assert_eq!(f.audits()?, count);
    drop((selected, resident, value, replay, receipt));
    assert_eq!(
        f.host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    drop((p, before));
    f.host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn worktree_audit_no_state_incomplete_and_retired_keep_only_original_members() -> Result<()> {
    for mode in ["no_state", "incomplete", "retired"] {
        let f = Fixture::new().await?;
        let old = current(&f)?;
        let revision = old.resident.coverage().revision;
        let p = phase(&f, vec![event(&f, "actual-worktree-old-scope-audit")])?;
        let conn = f.host.runtime_db().connection()?;
        let identity: String = conn.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
            [&f.scope.agent_id],
            |r| r.get(0),
        )?;
        let state: String = conn.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
            [&f.scope.agent_id],
            |r| r.get(0),
        )?;
        if mode == "no_state" {
            conn.execute(
                "DELETE FROM agent_states WHERE agent_id=?1",
                [&f.scope.agent_id],
            )?;
        } else if mode == "retired" {
            conn.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&f.scope.agent_id])?;
            conn.execute(
                "UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1",
                [&f.scope.agent_id],
            )?;
        } else {
            conn.execute(
                "UPDATE agent_states SET payload_json='poison-incomplete-State' WHERE agent_id=?1",
                [&f.scope.agent_id],
            )?;
            conn.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.active_complete',json('false'),'$.unsettled_complete',json('false'),'$.stop.pending_complete',json('false')) WHERE agent_id=?1 AND identity_incarnation=?2",rusqlite::params![f.scope.agent_id,f.scope.identity_incarnation])?;
        }
        drop(conn);
        let baseline = f.host.inner.input_evidence_budget.report()?.observed_bytes;
        let InputMutationExecution::Committed { value, receipt } = commit(&f, &p)? else {
            unreachable!()
        };
        let facts = value
            .effects
            .current_read_maintenance
            .as_ref()
            .context("old Event metadata ACK")?;
        assert!(facts.resident().is_none() && value.effects.current_read_facts.is_none());
        assert_eq!(
            (facts.base_revision, facts.revision),
            (revision, revision + 1)
        );
        assert!(
            value.effects.agent_state.is_none()
                && value.effects.agent_identities.is_empty()
                && value.effects.task_delta.is_none()
        );
        assert_eq!(value.effects.audit_events.len(), 1);
        assert_eq!(member(&f, "actual-worktree-old-scope-audit")?.0, 1);
        assert_eq!(
            native_birth(&f, "actual-worktree-old-scope-audit")?,
            (1, value.effects.audit_events[0].event_seq)
        );
        let conn = f.host.runtime_db().connection()?;
        let successor:usize=conn.query_row("SELECT COUNT(*) FROM current_read_members WHERE agent_id=?1 AND identity_incarnation=2",[&f.scope.agent_id],|r|r.get(0))?;
        assert_eq!(successor, 0);
        let successor_birth:usize=conn.query_row("SELECT COUNT(*) FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=2",[&f.scope.agent_id],|r|r.get(0))?;
        assert_eq!(successor_birth, 0);

        let (saved,freshness):(u64,String)=conn.query_row("SELECT revision,freshness FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=1",[&f.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(saved, revision + 1);
        assert_eq!(freshness, "records_invalidated");
        let actual: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                [&f.scope.agent_id],
                |r| r.get(0),
            )
            .optional()?;
        assert_eq!(
            actual.as_deref(),
            match mode {
                "no_state" => None,
                "retired" => Some("poison-successor-State"),
                _ => Some("poison-incomplete-State"),
            }
        );
        drop(conn);
        let InputMutationExecution::Replayed {
            receipt: replay, ..
        } = commit(&f, &p)?
        else {
            unreachable!()
        };
        assert_eq!(replay, receipt);
        assert!(f.path.exists());
        drop((value, receipt, replay));
        assert_eq!(
            f.host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        // Restore only conflicting-restore setup for normal Host teardown.
        f.host.runtime_db().transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
                rusqlite::params![identity, f.scope.agent_id],
            )?;
            crate::runtime_db::evidence::upsert_agent_state_tx(
                tx,
                &serde_json::from_str::<AgentState>(&state)?,
            )?;
            Ok(())
        })?;
        drop((p, old));
        f.host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn worktree_audit_receipt_fault_rolls_back_member_sequence_and_stop_progress() -> Result<()> {
    let f = Fixture::new().await?;
    let before = current(&f)?;
    let original = f.current()?;
    let audits = f.audits()?;
    let receipts = f.receipts()?;
    let sequence_scope =
        crate::runtime_db::evidence::audit_event_sequence_scope(Some(&f.scope.agent_id));
    let sequence = |f: &Fixture| -> Result<u64> {
        f.host.runtime_db().connection()?.query_row("SELECT COALESCE((SELECT last_value FROM runtime_sequences WHERE domain='audit_event' AND scope_key=?1),0)",
            [&sequence_scope],|r|r.get(0)).map_err(Into::into)
    };
    let original_sequence = sequence(&f)?;
    let p = phase(&f, vec![event(&f, "worktree-rollback-audit")])?;
    let baseline = f.host.inner.input_evidence_budget.report()?.observed_bytes;
    f.host.runtime_db().connection()?.execute_batch("CREATE TRIGGER worktree_current_receipt_fault BEFORE INSERT ON input_mutation_receipts WHEN json_extract(NEW.outcome_json,'$.result.kind')='task_stop_phase' BEGIN SELECT RAISE(ABORT,'original worktree whole receipt fault'); END;")?;
    let error = commit(&f, &p)
        .err()
        .context("receipt fault must abort original Event and progress")?;
    assert!(
        format!("{error:#}").contains("original worktree whole receipt fault"),
        "{error:#}"
    );
    assert_eq!(f.current()?.0.as_ref(), original.0.as_ref());
    assert_eq!(f.audits()?, audits);
    assert_eq!(f.receipts()?, receipts);
    assert_eq!(sequence(&f)?, original_sequence);
    let count:usize=f.host.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM current_read_members WHERE family='event' AND record_id='worktree-rollback-audit'",[],|r|r.get(0))?;
    assert_eq!(count, 0);
    let birth: usize = f.host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM current_event_native_births WHERE event_id='worktree-rollback-audit'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(birth, 0);

    assert_eq!(
        f.host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    f.host
        .runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER worktree_current_receipt_fault;")?;
    let InputMutationExecution::Committed { value, .. } = commit(&f, &p)? else {
        unreachable!()
    };
    assert!(value
        .effects
        .current_read_maintenance
        .as_ref()
        .and_then(|f| f.resident())
        .is_some());
    assert_eq!(value.effects.audit_events.len(), 1);
    assert!(f.path.exists());
    drop((value, p, before, original));
    f.host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn worktree_audit_strict_sources_and_epoch_budget_leave_no_committed_progress() -> Result<()>
{
    for mode in ["pruned", "foreign", "large", "epoch"] {
        let f = Fixture::new().await?;
        let e = event(&f, "worktree-exact-negative-audit");
        if mode != "foreign" && mode != "epoch" {
            seed_event(&f, &e)?;
        }
        let saved_event: Option<(String, String)> = f
            .host
            .runtime_db()
            .connection()?
            .query_row(
                "SELECT kind,data_json FROM audit_events WHERE audit_event_id=?1",
                [&e.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let saved_member:Option<(String,String)>=f.host.runtime_db().connection()?.query_row(
            "SELECT content_digest,projection_digest FROM current_read_members WHERE family='event' AND record_id=?1",[&e.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let epoch: String = f.host.runtime_db().event_log_epoch()?;
        let p = phase(&f, vec![e.clone()])?;
        let original = f.current()?;
        let receipts = f.receipts()?;
        f.host.runtime_db().transaction(|tx| {
            if mode=="pruned" {assert_eq!(tx.execute("UPDATE current_read_members SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE family='event' AND record_id=?1",[&e.id])?,1);}
            else if mode=="large" {assert_eq!(tx.execute("UPDATE audit_events SET kind=?1,data_json='malformed-Audit' WHERE audit_event_id=?2",rusqlite::params!["x".repeat(9*1024*1024),e.id])?,1);}
            else if mode=="epoch" {tx.execute("UPDATE runtime_metadata SET value=?1 WHERE key='event_log_epoch'",["x".repeat(9*1024*1024)])?;}
            else {crate::runtime_db::evidence::append_audit_event_tx(tx,Some("foreign-worktree-owner"),&e)?;}
            Ok(())
        })?;
        let baseline = f.host.inner.input_evidence_budget.report()?.observed_bytes;
        let error = commit(&f, &p).err().with_context(|| {
            format!("exact Event negative mode={mode} must reject before Stop progress")
        })?;
        match mode {
            "pruned" => assert!(
                matches!(error.downcast_ref::<CurrentReadError>(),Some(CurrentReadError::Pruned(key)) if key==&CurrentReadKey::event(&e.id)),
                "{error:#}"
            ),
            "large" | "epoch" => assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            ),
            _ => assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::NeedsPreparation)
                ),
                "{error:#}"
            ),
        }
        assert_eq!(f.current()?.0.as_ref(), original.0.as_ref());
        assert_eq!(f.receipts()?, receipts);
        assert_eq!(
            f.host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(f.path.exists());
        f.host.runtime_db().transaction(|tx| {
            if mode=="foreign" {assert_eq!(tx.execute("DELETE FROM audit_events WHERE audit_event_id=?1 AND agent_id='foreign-worktree-owner'",[&e.id])?,1);}
            if let Some((kind,payload))=&saved_event {tx.execute("UPDATE audit_events SET kind=?1,data_json=?2 WHERE audit_event_id=?3",rusqlite::params![kind,payload,e.id])?;}
            if let Some((content,projection))=&saved_member {tx.execute("UPDATE current_read_members SET availability='present',content_digest=?1,projection_digest=?2 WHERE family='event' AND record_id=?3",rusqlite::params![content,projection,e.id])?;}
            if mode=="epoch" {tx.execute("UPDATE runtime_metadata SET value=?1 WHERE key='event_log_epoch'",[&epoch])?;}
            Ok(())
        })?;
        drop((p, original));
        f.host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn worktree_qualified_audit_noop_preserves_original_epoch_and_native_birth() -> Result<()> {
    let f = Fixture::new().await?;
    let e = event(&f, "worktree-existing-immutable-audit");
    seed_event(&f, &e)?;
    let before = current(&f)?;
    let birth = native_birth(&f, &e.id)?;
    let count = f.audits()?;
    let p = phase(&f, vec![e.clone()])?;
    let epoch = f.host.runtime_db().event_log_epoch()?;
    f.host.runtime_db().connection()?.execute(
        "UPDATE runtime_metadata SET value=?1 WHERE key='event_log_epoch'",
        ["x".repeat(9 * 1024 * 1024)],
    )?;
    let InputMutationExecution::Committed { value, .. } = commit(&f, &p)? else {
        unreachable!()
    };
    assert_eq!(value.effects.audit_events.len(), 1);
    assert_eq!(value.effects.audit_events[0].event_seq, birth.1);
    assert_eq!(value.effects.audit_events[0].event_log_epoch, epoch);
    assert_eq!(native_birth(&f, &e.id)?, birth);
    assert_eq!(f.audits()?, count);
    let current = value
        .effects
        .current_read_maintenance
        .as_ref()
        .context("original no-op Current ACK")?;
    assert_eq!(
        (current.base_revision, current.revision),
        (
            before.resident.coverage().revision,
            before.resident.coverage().revision
        )
    );
    assert!(f.path.exists());
    f.host.runtime_db().connection()?.execute(
        "UPDATE runtime_metadata SET value=?1 WHERE key='event_log_epoch'",
        [epoch],
    )?;
    drop((value, p, before));
    f.host.shutdown().await?;
    Ok(())
}
