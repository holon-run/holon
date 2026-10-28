use super::*;

fn scope(f: &Fixture) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: f.source.agent_id.clone(),
        identity_incarnation: 1,
    }
}

#[test]
fn stopped_pause_memory_freezes_original_seed_and_whole_ack_without_writes() -> Result<()> {
    let f = Fixture::new()?;
    f.stop()?;
    let queue = f.ack_transition(&f.stop_committed_queue()?, &f.ack_brief())?;
    let prepared = PreparedInputTransition::queue(
        scope(&f),
        "read-original-pause-memory".into(),
        f.source.created_at,
        queue,
    )?;
    let original_digest = prepared.descriptor().command_digest().to_owned();
    let memory = f.db.transaction(|tx| {
        let before: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let ack = prepared.stopped_provider_pause_ack().context("typed ACK")?;
        let memory = load_stopped_provider_pause_memory_tx(tx, &ack, &scope(&f), &f.ledger)?;
        assert!(
            !serde_json::to_value(&memory)?["observed"]["Dispatch"]["binding"].is_null(),
            "a real seed must not become claim-only/Done/legacy absence"
        );
        let after: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        assert_eq!(before, after, "memory observation is read-only");
        Ok(memory)
    })?;
    let prepared = prepared.with_stopped_provider_pause_memory(memory)?;
    assert_ne!(prepared.descriptor().command_digest(), original_digest);
    assert!(!prepared.dispatch_memory_candidate());
    assert_eq!(
        prepared.dispatch_memory_policy(&f.memory_binding()?),
        DispatchMemoryCommandPolicy::None
    );
    let before: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM dispatch_memory_checkpoints WHERE turn_id='pause-turn'",
        [],
        |row| row.get(0),
    )?;
    f.commit_ack(&prepared)?;
    let after: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM dispatch_memory_checkpoints WHERE turn_id='pause-turn'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        before, after,
        "ACK must not Start/Finish/defer the original seed"
    );
    Ok(())
}

#[test]
fn stopped_pause_memory_rejects_missing_corrupt_or_unrelated_seed() -> Result<()> {
    for sql in [
        "DELETE FROM dispatch_memory_checkpoints WHERE turn_id='pause-turn'",
        "UPDATE dispatch_memory_checkpoints SET seed_digest=printf('%064d',0) WHERE turn_id='pause-turn'",
        "UPDATE dispatch_memory_checkpoints SET source_message_id='another-source' WHERE turn_id='pause-turn'",
        "DELETE FROM input_mutation_receipts WHERE command_nonce='pause-original-start'",
    ] {
        let f = Fixture::new()?;
        f.stop()?;
        let prepared = PreparedInputTransition::queue(
            scope(&f), "invalid-pause-seed".into(), f.source.created_at,
            f.ack_transition(&f.stop_committed_queue()?, &f.ack_brief())?,
        )?;
        let ack = prepared.stopped_provider_pause_ack().context("typed ACK")?;
        f.db.connection()?.execute(sql, [])?;
        let observed = f.ledger.report()?.observed_bytes;
        f.db.transaction(|tx| {
            let before: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            assert!(
                load_stopped_provider_pause_memory_tx(tx, &ack, &scope(&f), &f.ledger).is_err(),
                "{sql}"
            );
            let after: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            assert_eq!(before, after);
            Ok(())
        })?;
        assert_eq!(f.ledger.report()?.observed_bytes, observed, "failed read releases guards");
    }
    let f = Fixture::new()?;
    f.stop()?;
    let prepared = PreparedInputTransition::queue(
        scope(&f),
        "invalid-pause-legacy".into(),
        f.source.created_at,
        f.ack_transition(&f.stop_committed_queue()?, &f.ack_brief())?,
    )?;
    let ack = prepared.stopped_provider_pause_ack().context("typed ACK")?;
    f.db.connection()?.execute(
        "DELETE FROM dispatch_memory_checkpoints WHERE turn_id='pause-turn'",
        [],
    )?;
    f.db.connection()?.execute(
        "INSERT INTO dispatch_memory_legacy_dispatches
         (agent_id,identity_incarnation,turn_id,source_message_id,turn_index)
         VALUES (?1,1,'pause-turn','unrelated-legacy-source',1)",
        [&f.source.agent_id],
    )?;
    f.db.transaction(|tx| {
        assert!(
            load_stopped_provider_pause_memory_tx(tx, &ack, &scope(&f), &f.ledger).is_err(),
            "an unrelated legacy marker cannot exempt the missing original seed"
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn stopped_pause_memory_final_ack_requires_frozen_identity_and_shared_ledger() -> Result<()> {
    for missing in [false, true] {
        let f = Fixture::new()?;
        f.stop()?;
        let record = f.stop_committed_queue()?;
        let queue = f.ack_transition(&record, &f.ack_brief())?;
        let prepared = if missing {
            PreparedInputTransition::queue(
                scope(&f),
                "missing-pause-memory".into(),
                f.source.created_at,
                queue,
            )?
        } else {
            f.ack_prepared("changed-pause-memory", queue)?
        };
        if !missing {
            f.db.connection()?.execute(
                "DELETE FROM dispatch_memory_checkpoints WHERE turn_id='pause-turn'",
                [],
            )?;
        }
        let before = f.evidence_counts()?;
        assert!(f.commit_ack(&prepared).is_err(), "missing={missing}");
        assert_eq!(
            f.evidence_counts()?,
            before,
            "rejected ACK writes no Brief/Audit"
        );
    }
    let f = Fixture::new()?;
    f.stop()?;
    let prepared = f.ack_prepared(
        "wrong-pause-memory-ledger",
        f.ack_transition(&f.stop_committed_queue()?, &f.ack_brief())?,
    )?;
    let before = f.evidence_counts()?;
    assert!(
        f.db.transitions_in_scope(&scope(&f))
            .commit_prepared_transition(&prepared)
            .is_err(),
        "a new ledger cannot replace the observed carrier's shared ledger"
    );
    assert_eq!(f.evidence_counts()?, before);
    Ok(())
}

#[test]
fn stopped_pause_memory_claim_only_requires_canonical_no_turn_or_seed() -> Result<()> {
    let f = Fixture::with_dispatch(false)?;
    f.stop()?;
    let frozen = f.frozen_memory(&f.ledger)?;
    assert_eq!(
        serde_json::to_value(&frozen)?["memory"]["observed"],
        serde_json::json!("ClaimOnly")
    );
    let prepared = f.ack_prepared(
        "claim-only-pause-ack",
        f.ack_transition(&f.stop_committed_queue()?, &f.ack_brief())?,
    )?;
    f.commit_ack(&prepared)?;
    let turns: u64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM turn_records WHERE run_id='pause-run'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(turns, 0, "claim-only ACK cannot invent a dispatch");
    Ok(())
}

#[test]
fn stopped_pause_memory_done_requires_original_d1_receipt() -> Result<()> {
    use crate::{
        memory::episode_transition::{
            plan_episode_memory, EpisodeMemoryPlanInput, FrozenEpisodeMemoryValues,
        },
        runtime_db::{
            context_inputs::ContextDurableRead,
            dispatch_memory::mark_dispatch_memory_done_tx,
            episode_memory::{commit_episode_memory_tx, EpisodeMemoryTransitionCommand},
        },
    };
    let f = Fixture::new()?;
    let start = f.memory_start()?;
    f.db.transaction(|tx| {
        let state = ContextDurableRead::new(tx, &f.source.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .context("fixture State")?;
        let previous = state.working_memory.current_working_memory.clone();
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &state,
                actual_dispatch_turn_id: &start.key().actual_dispatch_turn_id,
                trigger: &f.source,
                prior_closure: start.prior_closure(),
                current_closure: start.prior_closure(),
                previous_snapshot: &previous,
                current_snapshot: &previous,
            },
            &FrozenEpisodeMemoryValues {
                timestamp: f.source.created_at,
                initial_builder_id: "episode:pause-initial".into(),
                fallback_builder_id: "episode:pause-fallback".into(),
                next_builder_id: "episode:pause-next".into(),
                audit_event_id: "audit:pause-memory".into(),
            },
        );
        let command = EpisodeMemoryTransitionCommand::new(
            start.key().clone(),
            start.invocation().clone(),
            state,
            plan,
            f.source.created_at,
        )?;
        commit_episode_memory_tx(tx, &command)?;
        mark_dispatch_memory_done_tx(tx, start.key(), start.invocation())?;
        Ok(())
    })?;
    f.stop()?;
    let frozen = f.frozen_memory(&f.ledger)?;
    let value = serde_json::to_value(&frozen)?;
    assert!(value["memory"]["observed"]["Dispatch"]["binding"].is_null());
    assert!(!value["memory"]["observed"]["Dispatch"]["record_digest"].is_null());
    f.db.transaction(|tx| acknowledge_frozen_stopped_provider_pause_tx(tx, &frozen))?;
    f.db.connection()?
        .execute("DELETE FROM episode_memory_results", [])?;
    f.db.transaction(|tx| {
        assert!(acknowledge_frozen_stopped_provider_pause_tx(tx, &frozen).is_err());
        Ok(())
    })
}
