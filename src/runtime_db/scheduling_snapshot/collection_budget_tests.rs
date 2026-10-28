use super::*;
use crate::diagnostics::resource_io::{admit_cloned_value, ResourceReadMaterial};
use crate::runtime_db::scheduling_snapshot::material_cost_fixture::ExpectedSqlMaterial;

fn measure(
    ledger: &InputEvidenceBudgetLedger,
    action: impl FnOnce(&ResourceReadMaterial) -> Result<Vec<(u64, usize)>>,
) -> Result<Vec<(u64, usize)>> {
    let phase = ResourceReadMaterial::with_test_limits(ledger, &scope(), 16 * 1024 * 1024, 4096)?;
    let result = {
        let _entry = phase.enter();
        action(&phase)?
    };
    drop(phase);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(result)
}

fn checkpoint(
    phase: &ResourceReadMaterial,
    prefix: &mut Vec<(u64, usize)>,
    value: &impl serde::Serialize,
) -> Result<()> {
    admit_cloned_value(value)?;
    prefix.push(phase.finish()?);
    Ok(())
}

fn assert_sentinel_matrix(
    db: &RuntimeDb,
    ledger: &InputEvidenceBudgetLedger,
    load: &SchedulingDomainLoadRequest,
    prefix: &[(u64, usize)],
    first_gate: usize,
) -> Result<()> {
    let before: String = db.connection()?.query_row(
        "SELECT coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
        [&scope().agent_id],
        |row| row.get(0),
    )?;
    let aliases: Vec<(String, String, String)> = db
        .connection()?
        .prepare("SELECT family,record_id,availability FROM scheduling_domain_records ORDER BY family,record_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for allowed in first_gate..=prefix.len() {
        for bytes_bound in [false, true] {
            let (bytes, records) = prefix[allowed - 1];
            let phase = ResourceReadMaterial::with_test_limits(
                ledger,
                &scope(),
                if bytes_bound { bytes } else { 16 * 1024 * 1024 },
                if bytes_bound { 4096 } else { records },
            )?;
            let error = {
                let _entry = phase.enter();
                db.transaction(|tx| {
                    prepare_scheduling_domain_snapshot_tx(tx, &scope(), load, ledger)
                })
                .err()
                .context("SQL sentinel did not stop the loader")?
            };
            let text = format!("{error:#}");
            if allowed < prefix.len() {
                assert!(
                    text.contains("ResourceExhausted: context resource read material"),
                    "allowed={allowed}, bytes={bytes_bound}: {text}"
                );
                assert!(!text.contains("collection sentinel reached"), "{text}");
                assert!(phase.finish().is_err());
            } else {
                assert!(text.contains("collection sentinel reached"), "{text}");
                assert_eq!(phase.finish()?, *prefix.last().unwrap());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let after: String = db.connection()?.query_row(
                "SELECT coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
                [&scope().agent_id],
                |row| row.get(0),
            )?;
            assert_eq!(after, before);
            let after_aliases: Vec<(String, String, String)> = db
                .connection()?
                .prepare("SELECT family,record_id,availability FROM scheduling_domain_records ORDER BY family,record_id")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(after_aliases, aliases);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn live_history_prior_and_pruned_copies_use_original_phase_before_sql_writes() -> Result<()> {
    for variant in ["live", "history", "prior", "pruned"] {
        let id = format!("collection-{variant}-{}", "x".repeat(1024));
        let active = variant == "live";
        let (_home, db) = legacy(true, |tx| {
            repositories::upsert_timer_tx(
                tx,
                &timer(
                    &id,
                    if active {
                        TimerStatus::Active
                    } else {
                        TimerStatus::Completed
                    },
                ),
            )
        })?;
        let ledger = ledger()?;
        let mut load = request();
        load.capture.history_limit = 1;
        let previous = db.transaction(|tx| {
            prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &ledger)?.require_current()
        })?;
        let mut coverage = previous.resident.coverage().clone();
        drop(previous);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        if active || variant == "prior" {
            load.capture.history_limit = 0;
        }
        if variant == "pruned" {
            db.transaction(|tx| {
                tx.execute("DELETE FROM timers WHERE timer_id=?1", [&id])?;
                tx.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE record_id=?1", [&id])?;
                Ok(())
            })?;
            coverage.observations.iter_mut().for_each(|observation| {
                observation.availability = SchedulingAvailability::Pruned;
            });
        }
        let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &id)]);
        let history: Vec<_> = keys.iter().cloned().collect();
        let empty = BTreeSet::<SchedulingKey>::new();
        let fixture_scope = scope();
        let sql = ExpectedSqlMaterial::new(&db, &fixture_scope);
        let prefix = measure(&ledger, |phase| {
            let mut prefix = Vec::new();
            sql.before_selection(&load, &coverage)?;
            checkpoint(phase, &mut prefix, &empty)?; // coverage required_keys
            checkpoint(phase, &mut prefix, &empty)?; // selected required_keys
            sql.ids(
                SchedulingFamily::Timer,
                true,
                None,
                if active { &keys } else { &empty },
            )?;
            if active {
                checkpoint(phase, &mut prefix, &keys)?; // live -> selected
            }
            sql.ids(
                SchedulingFamily::Timer,
                false,
                Some(load.capture.history_limit + 1),
                if active { &[][..] } else { &history[..] },
            )?;
            if load.capture.history_limit > 0 && !active {
                sql.loaded_history(
                    SchedulingFamily::Timer,
                    load.capture.history_limit + 1,
                    (!active).then(|| keys.first().unwrap()),
                )?;
            }
            if variant == "prior" {
                checkpoint(phase, &mut prefix, &history)?; // lookahead before truncation to zero
            } else if !active {
                if variant == "pruned" {
                    checkpoint(phase, &mut prefix, &history)?; // saved loaded order
                }
                checkpoint(phase, &mut prefix, &history)?; // history -> selected
                checkpoint(phase, &mut prefix, &history)?; // loaded -> selected
            }
            for family in [
                SchedulingFamily::TimerWake,
                SchedulingFamily::ExternalTrigger,
            ] {
                sql.ids(family, true, None, &empty)?;
                sql.ids(family, false, Some(load.capture.history_limit + 1), &empty)?;
            }
            checkpoint(
                phase,
                &mut prefix,
                if variant == "prior" { &empty } else { &keys },
            )?;
            if active {
                checkpoint(phase, &mut prefix, &keys)?; // prior live -> old_live
            } else {
                checkpoint(phase, &mut prefix, &history)?; // prior history -> old_live
                checkpoint(phase, &mut prefix, &history)?; // prior loaded -> old_live
            }
            checkpoint(phase, &mut prefix, &keys)?; // selected -> pending
            sql.alias(keys.first().unwrap())?;
            if variant == "pruned" {
                sql.charge(serde_json::to_vec(&coverage)?.len(), 1)?;
                // Coverage moves into the resident; no clone or alias write for Pruned.
                // Pruned observations still allocate borrowed validation nodes.
                for _ in &coverage.observations {
                    crate::diagnostics::resource_io::charge_owned_metadata(128, 1)?;
                    prefix.push(phase.finish()?);
                }
                // Validation and budget refresh borrow the validated closure;
                // neither allocates an empty captured-closure Vec.
                // The resident's owned metadata view has one ledger node;
                // its two empty fact views do not register owned bytes.
                crate::diagnostics::resource_io::charge_owned_metadata(
                    (fixture_scope.agent_id.len() + 256) as u64,
                    8,
                )?;
                prefix.push(phase.finish()?);
            } else {
                sql.stored(keys.first().unwrap())?;
                *prefix.last_mut().unwrap() = phase.finish()?;
            }
            Ok(prefix)
        })?;
        let table = if variant == "pruned" {
            "scheduling_domain_current"
        } else {
            "scheduling_domain_records"
        };
        db.connection()?.execute_batch(&format!(
            "CREATE TRIGGER collection_sentinel BEFORE INSERT ON {table}
             BEGIN SELECT RAISE(ABORT, 'collection sentinel reached'); END;"
        ))?;
        assert_sentinel_matrix(&db, &ledger, &load, &prefix, 2)?;
    }
    Ok(())
}

#[test]
fn wake_parent_and_input_root_copies_use_original_phase_before_alias_write() -> Result<()> {
    let timer_id = format!("completed-parent-{}", "x".repeat(1024));
    let message_id = format!("pending-message-{}", "y".repeat(1024));
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer(&timer_id, TimerStatus::Completed))?;
        wake(tx, &timer_id, &message_id, 3)
    })?;
    let ledger = ledger()?;
    let mut load = request();
    load.capture.history_limit = 0;
    let previous = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &ledger)?.require_current()
    })?;
    let wakes = previous.resident.coverage().pending_wakes.clone();
    let coverage = previous.resident.coverage().clone();
    assert_eq!(wakes.len(), 1);
    assert_eq!(
        previous.input_roots,
        BTreeSet::from([InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            &message_id
        )])
    );
    drop(previous);
    let empty = BTreeSet::<SchedulingKey>::new();
    let parent = SchedulingKey::new(SchedulingFamily::Timer, &timer_id);
    let fixture_scope = scope();
    let sql = ExpectedSqlMaterial::new(&db, &fixture_scope);
    let prefix = measure(&ledger, |phase| {
        let mut prefix = Vec::new();
        sql.before_selection(&load, &coverage)?;
        checkpoint(phase, &mut prefix, &empty)?;
        checkpoint(phase, &mut prefix, &empty)?;
        sql.ids(SchedulingFamily::Timer, true, None, &empty)?;
        sql.ids(
            SchedulingFamily::Timer,
            false,
            Some(load.capture.history_limit + 1),
            [&parent],
        )?;
        checkpoint(phase, &mut prefix, &vec![parent.clone()])?; // Timer lookahead, then truncate
        sql.ids(SchedulingFamily::TimerWake, true, None, &wakes)?;
        checkpoint(phase, &mut prefix, &wakes)?; // live wakes
        sql.ids(
            SchedulingFamily::TimerWake,
            false,
            Some(load.capture.history_limit + 1),
            &empty,
        )?;
        sql.ids(SchedulingFamily::ExternalTrigger, true, None, &empty)?;
        sql.ids(
            SchedulingFamily::ExternalTrigger,
            false,
            Some(load.capture.history_limit + 1),
            &empty,
        )?;
        checkpoint(phase, &mut prefix, &wakes)?; // core
        checkpoint(phase, &mut prefix, &wakes)?; // prior wakes
        checkpoint(phase, &mut prefix, &wakes)?; // pending
        sql.alias(wakes.first().unwrap())?;
        sql.stored(wakes.first().unwrap())?;
        checkpoint(phase, &mut prefix, &timer_id)?; // new parent
        checkpoint(phase, &mut prefix, &parent)?; // parent -> core
        checkpoint(phase, &mut prefix, &parent)?; // parent -> selected/pending
        checkpoint(phase, &mut prefix, &message_id)?; // new Message root
        Ok(prefix)
    })?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER collection_sentinel BEFORE INSERT ON scheduling_domain_records
         BEGIN SELECT RAISE(ABORT, 'collection sentinel reached'); END;",
    )?;
    assert_sentinel_matrix(&db, &ledger, &load, &prefix, 6)?;
    db.connection()?
        .execute_batch("DROP TRIGGER collection_sentinel")?;
    // Real Input installation must retain the wake root and the original phase
    // after the temporary Scheduling snapshot and TLS entry are gone.
    let mut state = AgentState::new(&scope().agent_id);
    state.pending = 1;
    state.total_message_count = 1;
    let input = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: None,
        mandatory_roots: BTreeSet::new(),
        windows: vec![],
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: load.record_limit,
        byte_limit: load.byte_limit,
    };
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let weak = Arc::downgrade(&phase.guard()?);
    let installed = {
        let _entry = phase.enter();
        db.transaction(|tx| {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx,
                &scope(),
                &state,
                &input,
                12,
                &ledger,
                &load.capture,
            )
        })?
    };
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    phase.finish()?;
    let root = InputEvidenceKey::new(InputEvidenceFamily::Message, &message_id);
    assert!(installed.inputs.mandatory_roots().contains(&root));
    let input_capture = installed.inputs.capture()?;
    assert!(input_capture.lookup(&root)?.is_some());
    drop(input_capture);
    drop(phase);
    assert!(weak.upgrade().is_some());
    let last = installed.scheduling.clone();
    let capture = last.capture(&load.capture)?;
    assert_eq!(
        capture.pending_wake(&timer_id)?.unwrap().message_id,
        message_id
    );
    drop(installed);
    drop(last);
    assert!(weak.upgrade().is_some());
    drop(capture);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
