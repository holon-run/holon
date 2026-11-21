//! Differential live-input oracle; runs only against two clones of the live cut.
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::{CommittedInputEvidenceDelta, InputEvidenceScope},
        resident_task::ResidentTaskDomain,
        RuntimeHandle,
    },
    runtime_db::{
        current_read_writes::{
            CommittedCurrentWrites, CurrentAddressFact, CurrentFamily, CurrentRecord,
        },
        input_mutation_receipts::{InputMutationFamily, InputMutationReceipt},
        task_snapshot::{self, TaskDomainLoadRequest},
        transitions::{
            with_full_task_oracle, with_live_input_oracle, InputMutationExecution,
            PreparedInputTransition, PreparedTaskBaselines, ProviderResultHandoffCommand,
            TransitionCommit,
        },
        work_snapshot::WorkDomainLoadRequest,
        RuntimeDb,
    },
};

struct LiveCommandRun {
    commit: TransitionCommit,
    receipt: InputMutationReceipt,
    tasks: Arc<ResidentTaskDomain>,
    writer_at: DateTime<Utc>,
}

fn fork_db(source: &RuntimeDb, directory: &std::path::Path) -> Result<RuntimeDb> {
    let path = directory.join("live-input.sqlite");
    source
        .connection()?
        .execute("VACUUM INTO ?1", [path.to_str().context("oracle path")?])?;
    RuntimeDb::open_and_migrate(path, directory.join("live-input.lock"))
}

fn run_prepared_live_command(
    db: &RuntimeDb,
    prepared: &PreparedInputTransition,
    resident: &Arc<ResidentTaskDomain>,
    work: &WorkDomainLoadRequest,
    tasks: &TaskDomainLoadRequest,
    full: bool,
    transcript_at: DateTime<Utc>,
) -> Result<LiveCommandRun> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut bases = PreparedTaskBaselines::default();
    bases.defer_roots(vec![resident.clone()]);
    let mut commit = || {
        db.transitions_in_scope(resident.scope())
            .commit_prepared_transition_with_task_preparation(
                prepared, work, &ledger, tasks, &mut bases,
            )
    };
    let result = with_live_input_oracle(transcript_at, || {
        if full {
            with_full_task_oracle(commit)
        } else {
            commit()
        }
    })?;
    let InputMutationExecution::Committed { value, receipt } = result else {
        anyhow::bail!("fresh live-input oracle did not commit");
    };
    ensure!(value.applied, "live-input oracle did not apply");
    assert_eq!(bases.has_prepared_scope(resident.scope()), full);
    let applied = match &value.effects.task_delta {
        Some(delta) => {
            ensure!(
                full,
                "retained live input unexpectedly published Task delta"
            );
            Arc::new(resident.with_committed(delta)?)
        }
        None => {
            ensure!(!full, "full live input omitted Task ACK");
            resident.clone()
        }
    };
    if !full {
        assert!(Arc::ptr_eq(resident, &applied));
    }
    Ok(LiveCommandRun {
        commit: value,
        receipt,
        tasks: applied,
        writer_at: transcript_at,
    })
}

fn compare_input(
    left: &CommittedInputEvidenceDelta,
    right: &CommittedInputEvidenceDelta,
    scope: &InputEvidenceScope,
) -> Result<()> {
    // Exhaustive destructuring prevents newly added ACK fields going unnoticed.
    let CommittedInputEvidenceDelta {
        query_advance,
        output_recall_invalidation,
        execution,
        records,
        removals,
        replay_source_absences,
        turn_transcript_keys,
        message_turn_keys,
    } = left;
    assert_eq!(records, &right.records);
    assert_eq!(replay_source_absences, &right.replay_source_absences);
    assert_eq!(turn_transcript_keys, &right.turn_transcript_keys);
    assert_eq!(message_turn_keys, &right.message_turn_keys);
    let removals_json = |delta: &CommittedInputEvidenceDelta| {
        serde_json::to_value(
            delta
                .removals
                .iter()
                .map(|r| (&r.scope, &r.key, r.revision, &r.previous_content_digest))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(removals.len(), right.removals.len());
    assert_eq!(removals_json(left)?, removals_json(right)?);
    let execution_json = |e: &crate::runtime::resident_input_evidence::ExecutionInputDelta| {
        serde_json::to_value((
            e.initialized,
            &e.sources,
            &e.open_attempts,
            &e.nonterminal_work_items,
            &e.work_items,
        ))
    };
    assert_eq!(
        execution_json(execution)?,
        execution_json(&right.execution)?
    );
    assert_eq!(
        output_recall_invalidation.is_some(),
        right.output_recall_invalidation.is_some()
    );
    for invalidation in [
        output_recall_invalidation.as_ref(),
        right.output_recall_invalidation.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        ensure!(invalidation.belongs_to(scope), "Output ACK scope differs");
    }
    assert_eq!(query_advance.is_some(), right.query_advance.is_some());
    if let (Some(left), Some(right)) = (query_advance, &right.query_advance) {
        left.validate_delta(scope, records)?;
        right.validate_delta(scope, records)?;
        // Test-only semantic snapshot: scope/cursors/births/publication coverage,
        // never allocation identities or budget guards.
        assert_eq!(left.oracle_facts()?, right.oracle_facts()?);
    }
    Ok(())
}

type CurrentFacts = Vec<(String, String, Option<Value>)>;

fn current_facts(db: &RuntimeDb, run: &LiveCommandRun, event_kind: &str) -> Result<CurrentFacts> {
    let effects = &run.commit.effects;
    assert_eq!(effects.audit_events.len(), 1, "one live-input audit event");
    let expected_event = &effects.audit_events[0];
    assert_eq!(expected_event.kind, event_kind);
    let ack = effects
        .current_read_writes
        .as_ref()
        .context("Current ACK")?;
    ensure!(ack.is_complete(), "incomplete Current ACK");
    // This direct oracle supplies no query source. Do not silently drop one.
    ensure!(
        ack.query.is_none() && ack.query_publication.is_none(),
        "unexpected Current query publication"
    );
    let connection = db.connection()?;
    let mut events = 0;
    let mut result = Vec::new();
    for (key, fact) in ack.facts() {
        let address = format!("{:?}:{}", key.0, key.1);
        let Some(record) = ack.record(&key) else {
            ensure!(
                !matches!(fact, CurrentAddressFact::Qualified(_)),
                "qualified Current address has no body"
            );
            result.push((address, format!("{fact:?}"), None));
            continue;
        };
        let CurrentAddressFact::Qualified(ref digest) = fact else {
            anyhow::bail!("Current body lacks exact qualification");
        };
        let (sql, body) = match record.record() {
            CurrentRecord::Queue(queue) => {
                assert_eq!(key.0, CurrentFamily::Queue);
                (
                    "SELECT payload_json FROM queue_entries WHERE message_id=?1",
                    serde_json::to_value(queue)?,
                )
            }
            CurrentRecord::Event(event) => {
                assert_eq!(key.0, CurrentFamily::Event);
                assert_eq!(event.id, expected_event.id);
                assert_eq!(event.kind, event_kind);
                assert_eq!(event.created_at, run.writer_at);
                assert_eq!(event.data["result_identities"], serde_json::json!([]));
                events += 1;
                (
                    "SELECT data_json FROM audit_events WHERE audit_event_id=?1",
                    serde_json::to_value(event)?,
                )
            }
        };
        let payload: String = connection.query_row(sql, [&key.1], |row| row.get(0))?;
        assert_eq!(digest, &format!("{:x}", Sha256::digest(payload.as_bytes())));
        assert_eq!(body, serde_json::from_str::<Value>(&payload)?);
        result.push((address, format!("{fact:?}"), Some(body)));
    }
    assert_eq!(
        events, 1,
        "the frozen live-input event must be acknowledged"
    );
    assert_eq!(effects.audit_events.len(), events);
    for event in &effects.audit_events {
        let key = (CurrentFamily::Event, event.id.clone());
        let CurrentRecord::Event(current) = ack
            .record(&key)
            .context("audit effect absent from Current")?
            .record()
        else {
            anyhow::bail!("audit effect has another Current family");
        };
        assert_eq!(serde_json::to_value(event)?, serde_json::to_value(current)?);
    }
    Ok(result)
}

fn compare_current_headers(left: &CommittedCurrentWrites, right: &CommittedCurrentWrites) {
    assert_eq!(left.scope, right.scope);
    assert_eq!(left.base_revision, right.base_revision);
    assert_eq!(left.revision, right.revision);
    assert_eq!(left.queue_posture, right.queue_posture);
    assert_eq!(left.scheduler_events, right.scheduler_events);
}

fn compare_tasks(left: &ResidentTaskDomain, right: &ResidentTaskDomain) {
    assert_eq!(left.scope(), right.scope());
    assert_eq!(left.coverage(), right.coverage());
    assert_eq!(
        left.selected_install_records(),
        right.selected_install_records()
    );
    assert_eq!(
        left.selected_install_stop_records(),
        right.selected_install_stop_records()
    );
}

/// Call after begin_dispatch_turn/record_incoming_transcript_entry and before
/// recover_provider_result_inputs. Both writers operate on clones, not `runtime`.
/// This is a correctness oracle, not a cost sample or human approval.
pub(super) async fn assert_ordinary_handoff_task_oracle(runtime: &RuntimeHandle) -> Result<()> {
    let prepared = {
        let mutation = runtime.begin_mutation().await?;
        let scope = mutation.input_evidence_scope();
        let root = mutation.capture_published_root()?;
        let turn = runtime.storage().read_recent_turns(1)?.remove(0);
        let binding = root
            .state
            .current_execution_binding
            .as_ref()
            .context("current execution binding")?;
        assert_eq!(binding.turn_id, turn.turn_id);
        let activation = binding.activation_id.as_deref().context("activation")?;
        let attempt = runtime
            .inner
            .runtime_db
            .transitions()
            .execution_attempt_by_id(&scope.agent_id, activation)?
            .context("current execution attempt")?;
        let at = runtime.now();
        PreparedInputTransition::provider_result_handoff(
            scope,
            crate::ids::audit_event_id(),
            at,
            ProviderResultHandoffCommand {
                expected_attempt: attempt,
                expected_turn: turn,
                at,
                audit_event_id: crate::ids::audit_event_id(),
                fault: None,
            },
        )?
    };
    compare_prepared_live_command(runtime, prepared).await
}

/// Compare the same frozen, fresh ordinary attachment or handoff command.
/// The caller must release any mutation guard before awaiting this helper.
/// No writer mutates `runtime`. Transcript IDs/time are frozen on this thread;
/// audit wall time is the sole normalized field.
pub(super) async fn compare_prepared_live_command(
    runtime: &RuntimeHandle,
    prepared: PreparedInputTransition,
) -> Result<()> {
    let mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    assert_eq!(prepared.descriptor().scopes(), std::slice::from_ref(&scope));
    let event_kind = match prepared.descriptor().family() {
        InputMutationFamily::LiveInputAttachment => "live_input_attached",
        InputMutationFamily::ProviderResultHandoff => "provider_task_results_transferred",
        _ => anyhow::bail!("oracle requires an ordinary attachment or handoff command"),
    };
    let root = mutation.capture_published_root()?;
    let resident = root.tasks.as_ref().context("ready unrelated Task domain")?;
    ensure!(
        !resident.coverage().task_metadata.is_empty(),
        "oracle requires a nonempty unrelated Task domain"
    );
    let db = &runtime.inner.runtime_db;
    let work = mutation.owner_work_load_request()?;
    let (task_request, _request_material) = mutation.owner_task_load_request()?;
    let fast_directory = tempfile::tempdir()?;
    let full_directory = tempfile::tempdir()?;
    let fast_db = fork_db(db, fast_directory.path())?;
    let full_db = fork_db(db, full_directory.path())?;
    let transcript_at = runtime.now();
    let fast = run_prepared_live_command(
        &fast_db,
        &prepared,
        resident,
        &work,
        &task_request,
        false,
        transcript_at,
    )?;
    let full = run_prepared_live_command(
        &full_db,
        &prepared,
        resident,
        &work,
        &task_request,
        true,
        transcript_at,
    )?;
    let (left, right) = (&fast.commit.effects, &full.commit.effects);
    assert_eq!(fast.receipt.prepared, full.receipt.prepared);
    assert_eq!(fast.receipt.outcome, full.receipt.outcome);
    assert_eq!(fast.commit.applied, full.commit.applied);
    assert_eq!(
        serde_json::to_value(&fast.commit.delivery_receipt)?,
        serde_json::to_value(&full.commit.delivery_receipt)?
    );
    assert_eq!(
        serde_json::to_value(&left.agent_state)?,
        serde_json::to_value(&right.agent_state)?
    );
    assert_eq!(left.turns, right.turns);
    assert_eq!(left.execution_protocol, right.execution_protocol);
    assert_eq!(
        serde_json::to_value(&left.transcript_entries)?,
        serde_json::to_value(&right.transcript_entries)?
    );
    assert_eq!(
        left.removed_queue_message_ids,
        right.removed_queue_message_ids
    );
    compare_input(
        &left.input_evidence_delta,
        &right.input_evidence_delta,
        &scope,
    )?;
    compare_current_headers(
        left.current_read_writes.as_ref().context("fast Current")?,
        right.current_read_writes.as_ref().context("full Current")?,
    );
    assert_eq!(
        current_facts(&fast_db, &fast, event_kind)?,
        current_facts(&full_db, &full, event_kind)?
    );
    compare_tasks(&fast.tasks, &full.tasks);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for (cloned_db, run) in [(&fast_db, &fast), (&full_db, &full)] {
        let canonical = cloned_db.transaction_once(|tx| {
            task_snapshot::prepare_task_domain_snapshot_with_budget_tx(
                tx,
                &scope,
                &task_request,
                &ledger,
            )?
            .require_current()
        })?;
        compare_tasks(&run.tasks, &canonical.resident);
    }
    assert_eq!(
        fast_db.agent_states().latest(&scope.agent_id)?,
        full_db.agent_states().latest(&scope.agent_id)?
    );
    assert_eq!(
        fast_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&scope.agent_id)?,
        full_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&scope.agent_id)?
    );
    Ok(())
}
