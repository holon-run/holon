//! Same frozen command and initial database, with the original Task path forced.
use super::*;
use std::time::Instant;

#[path = "prepared_live_cost_matrix_tests.rs"]
mod matrix;

pub(super) fn fork_db(db: &RuntimeDb, directory: &std::path::Path) -> Result<RuntimeDb> {
    let path = directory.join("oracle.sqlite");
    db.connection()?
        .execute("VACUUM INTO ?1", [path.to_str().context("oracle path")?])?;
    RuntimeDb::open_and_migrate(path, directory.join("oracle.lock"))
}

pub(super) fn commit_recovery(
    db: &RuntimeDb,
    f: &RecoveryFixture,
    prepared: &PreparedInputTransition,
    resident: &Arc<crate::runtime::resident_task::ResidentTaskDomain>,
    full: bool,
) -> Result<(TransitionCommit, u128, serde_json::Value)> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut bases = PreparedTaskBaselines::default();
    bases.defer_roots(vec![resident.clone()]);
    let started = Instant::now();
    let mut run = || {
        db.transitions_in_scope(&f.f.scope)
            .commit_prepared_transition_with_task_preparation(
                prepared,
                &work_request(),
                &ledger,
                &task_request(),
                &mut bases,
            )
    };
    let execution = crate::runtime_db::transitions::with_live_input_oracle(
        f.f.at + chrono::Duration::seconds(1),
        || {
            if full {
                live_task::with_full_task_oracle(run)
            } else {
                run()
            }
        },
    )?;
    let elapsed = started.elapsed().as_nanos();
    let InputMutationExecution::Committed { value, .. } = execution else {
        anyhow::bail!("oracle input did not commit")
    };
    assert!(value.applied);
    assert_eq!(value.effects.task_delta.is_some(), full);
    assert_eq!(bases.has_prepared_scope(&f.f.scope), full);
    if !full {
        assert!(Arc::ptr_eq(bases.resident(&f.f.scope).unwrap(), resident));
    }
    Ok((value, elapsed, serde_json::to_value(ledger.report()?)?))
}

pub(super) fn compare_acks(fast: &TransitionCommit, full: &TransitionCommit) -> Result<()> {
    assert_eq!(fast.applied, full.applied);
    assert_eq!(fast.effects.turns, full.effects.turns);
    assert_eq!(
        fast.effects.execution_protocol,
        full.effects.execution_protocol
    );
    assert_eq!(
        serde_json::to_value(&fast.effects.transcript_entries)?,
        serde_json::to_value(&full.effects.transcript_entries)?,
    );
    assert_eq!(
        fast.effects.removed_queue_message_ids,
        full.effects.removed_queue_message_ids,
    );
    assert_eq!(
        serde_json::to_value(&fast.effects.agent_state)?,
        serde_json::to_value(&full.effects.agent_state)?,
    );
    let left = &fast.effects.input_evidence_delta;
    let right = &full.effects.input_evidence_delta;
    assert_eq!(left.records, right.records);
    assert_eq!(left.turn_transcript_keys, right.turn_transcript_keys);
    assert_eq!(left.message_turn_keys, right.message_turn_keys);
    assert_eq!(left.replay_source_absences, right.replay_source_absences);
    assert!(left.removals.is_empty() && right.removals.is_empty());
    assert!(
        left.output_recall_invalidation.is_none() && right.output_recall_invalidation.is_none()
    );
    let execution = |delta: &crate::runtime::resident_input_evidence::ExecutionInputDelta| {
        serde_json::to_value((
            delta.initialized,
            &delta.sources,
            &delta.open_attempts,
            &delta.nonterminal_work_items,
            &delta.work_items,
        ))
    };
    assert_eq!(execution(&left.execution)?, execution(&right.execution)?);
    assert_eq!(left.query_advance.is_some(), right.query_advance.is_some());
    if let (Some(left), Some(right)) = (&left.query_advance, &right.query_advance) {
        assert_eq!(left.oracle_facts()?, right.oracle_facts()?);
    }
    let left = fast
        .effects
        .current_read_writes
        .as_ref()
        .context("fast Current ACK")?;
    let right = full
        .effects
        .current_read_writes
        .as_ref()
        .context("full Current ACK")?;
    assert!(left.is_complete() && right.is_complete());
    assert_eq!(left.scope, right.scope);
    assert_eq!(left.base_revision, right.base_revision);
    assert_eq!(left.revision, right.revision);
    assert_eq!(left.queue_posture, right.queue_posture);
    assert_eq!(left.scheduler_events, right.scheduler_events);
    let records =
        |ack: &crate::runtime_db::current_read_writes::CommittedCurrentWrites| -> Result<Vec<_>> {
            use crate::runtime_db::current_read_writes::CurrentRecord;
            ack.records()
                .map(|record| match record.record() {
                    CurrentRecord::Queue(queue) => Ok(serde_json::to_value(queue)?),
                    CurrentRecord::Event(event) => Ok(serde_json::to_value(&**event)?),
                })
                .collect()
        };
    assert_eq!(records(left)?, records(right)?);
    assert_eq!(left.facts(), right.facts());
    Ok(())
}

fn paired_recovery(sample: usize, report: bool) -> Result<()> {
    let f = RecoveryFixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let resident = resident_with_task(&f, &ledger)?;
    let prepared = f.freeze(f.command(None))?;
    let directory = tempfile::tempdir()?;
    let full_db = fork_db(&f.f.db, directory.path())?;
    let stages = || {
        attribution::snapshot()
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect::<BTreeMap<_, _>>()
    };
    let before = stages();
    let (fast, fast_ns, fast_budget) = commit_recovery(&f.f.db, &f, &prepared, &resident, false)?;
    let middle = stages();
    let (full, full_ns, full_budget) = commit_recovery(&full_db, &f, &prepared, &resident, true)?;
    let after = stages();
    compare_acks(&fast, &full)?;
    for db in [&f.f.db, &full_db] {
        let oracle = db.transaction_once(|tx| {
            tasks::prepare_task_domain_snapshot_with_budget_tx(
                tx,
                &f.f.scope,
                &task_request(),
                &ledger,
            )?
            .require_current()
        })?;
        assert_eq!(oracle.resident.coverage(), resident.coverage());
        assert_eq!(
            oracle.resident.selected_install_records(),
            resident.selected_install_records()
        );
        assert_eq!(
            db.agent_states().latest(&f.f.scope.agent_id)?,
            Some(f.f.state()?)
        );
        assert!(matches!(
            db.transitions_in_scope(&f.f.scope)
                .commit_prepared_transition(&prepared)?,
            InputMutationExecution::Replayed { .. }
        ));
    }
    assert_eq!(
        full.effects.task_delta.as_ref().unwrap().coverage,
        *resident.coverage()
    );
    if report {
        // Global stage deltas require this ignored test to run in isolation.
        // No elapsed-time threshold: counters are the causal acceptance gate.
        for name in before.keys() {
            let fast_count = middle[name].count - before[name].count;
            let full_count = after[name].count - middle[name].count;
            if name.starts_with("db.task")
                || name.starts_with("db.input")
                || name.starts_with("db.current")
                || name.contains("transaction")
            {
                eprintln!("live_cost sample={sample} stage={name} fast_count={fast_count} full_count={full_count} fast_ns={} full_ns={}",
                    middle[name].total_ns-before[name].total_ns,
                    after[name].total_ns-middle[name].total_ns);
            }
        }
        for name in [
            "db.task.domain_prepare",
            "db.task.candidate.read",
            "db.task.write.delta_finalize",
        ] {
            assert_eq!(
                middle[name].count, before[name].count,
                "fast path entered {name}"
            );
        }
        assert!(
            after["db.task.write.delta_finalize"].count
                > middle["db.task.write.delta_finalize"].count
        );
        // This surrounds the commit API, not the exclusive SQLite writer lease.
        eprintln!("live_cost sample={sample} commit_elapsed_fast_ns={fast_ns} commit_elapsed_full_ns={full_ns} fast_budget={fast_budget} full_budget={full_budget}");
    }
    Ok(())
}

#[test]
fn prepared_live_input_recovery_matches_original_full_task_path_acks() -> Result<()> {
    paired_recovery(0, false)
}

#[test]
#[ignore = "paired stage attribution; run alone with --ignored --nocapture --test-threads=1"]
fn prepared_live_input_paired_recovery_stage_cost() -> Result<()> {
    for sample in 0..5 {
        paired_recovery(sample, true)?;
    }
    Ok(())
}
