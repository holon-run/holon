//! Phase-one attribution harness for Input reference-closure preparation.
//! Exact assertions use the per-run thread-local summary (parallel-safe);
//! the shared content-free stage counters get lower-bound checks only.

use super::*;
use crate::diagnostics::attribution;
use crate::runtime_db::context_inputs::ContextDurableRead;

fn stage_count(name: &str) -> u64 {
    attribution::snapshot()
        .into_iter()
        .find(|stage| stage.name == name)
        .map(|stage| stage.count)
        .unwrap_or(0)
}

fn seeded(messages: &[(String, usize)]) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let (directory, db) = fresh()?;
    db.transaction(|tx| {
        for (id, pad) in messages {
            let mut value = message(id);
            if let MessageBody::Text { text } = &mut value.body {
                *text = format!("{id}{}", "x".repeat(*pad));
            }
            let (committed, _) = append_message_tx(tx, &value)?;
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[CanonicalInputEvidence::Message(committed)],
            )?;
        }
        Ok(())
    })?;
    Ok((directory, db))
}

fn exact_roots(ids: &[&str]) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        query: None,
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: Default::default(),
        windows: Vec::new(),
        mandatory_roots: ids
            .iter()
            .map(|id| Key::new(Family::Message, *id))
            .collect(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}

fn prepare_context(
    db: &RuntimeDb,
    request: &InputEvidenceLoadRequest,
) -> Result<ResidentInputEvidence> {
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
        prepare_named_input_evidence_tx(tx, &scope(), request, &read, false)
    })
}

#[test]
fn context_read_attributes_families_sources_and_closure() -> Result<()> {
    let (_directory, db) = seeded(&[
        ("msg-a".into(), 32),
        ("msg-b".into(), 32),
        ("msg-c".into(), 32),
    ])?;
    let before = stage_count("db.input.prepare.source.context");
    let resident = prepare_context(&db, &exact_roots(&["msg-a", "msg-b", "msg-c"]))?;
    assert_eq!(resident.capture()?.records.len(), 3);
    let run = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(run.source, "context");
    assert_eq!(run.roots, 3);
    assert_eq!(run.records, 3);
    assert!(run.edges >= run.records);
    assert!(
        run.duplicate_edges >= 1,
        "overlapping seed paths are attributed"
    );
    assert_eq!(run.family_records.get("message"), Some(&3));
    let message_bytes = run
        .family_payload_bytes
        .get("message")
        .copied()
        .unwrap_or(0);
    assert!(message_bytes >= 3 * 32);
    assert!(run.resident_bytes >= message_bytes);
    assert_eq!(
        run.qualification_backfills, 0,
        "committed-writer seeding needs no qualification backfill"
    );
    assert_eq!(run.dominant_family(), Some("message"));
    assert!(stage_count("db.input.prepare.source.context") >= before + 1);
    Ok(())
}

#[test]
fn owner_change_bootstrap_prepare_is_attributed() -> Result<()> {
    let (_directory, db) = seeded(&[("msg-a".into(), 16)])?;
    let before = stage_count("db.input.prepare.source.owner_change");
    let prepared = db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(tx, &scope(), &exact_roots(&["msg-a"]))
    })?;
    assert!(matches!(
        prepared,
        InputEvidenceBootstrapSnapshot::Current(_)
    ));
    let run = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(run.source, "owner_change");
    assert_eq!(run.records, 1);
    assert!(stage_count("db.input.prepare.source.owner_change") >= before + 1);
    Ok(())
}

#[test]
fn legacy_first_delivery_backfill_is_attributed() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        append_message_tx(tx, &message("legacy-msg"))?;
        Ok(())
    })?;
    let before = stage_count("db.input.prepare.op.qualification_reread");
    let prepared = db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(tx, &scope(), &exact_roots(&["legacy-msg"]))
    })?;
    assert!(matches!(
        prepared,
        InputEvidenceBootstrapSnapshot::Current(_)
    ));
    let run = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(run.source, "owner_change");
    assert_eq!(run.records, 1);
    assert_eq!(
        run.qualification_backfills, 1,
        "legacy first delivery backfills the missing qualification"
    );
    assert!(
        stage_count("db.input.prepare.op.qualification_reread") >= before + 1,
        "a qualification backfill is followed by a counted re-read"
    );
    Ok(())
}

#[test]
fn scope_read_prepare_distinguishes_generic_and_validated_recovery() -> Result<()> {
    let (_directory, db) = seeded(&[("msg-a".into(), 16)])?;
    let request = exact_roots(&["msg-a"]);
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
        prepare_input_evidence_with_scope_read_tx(tx, &scope(), request.borrowed(), false, read)
    })?;
    assert_eq!(
        prepare_attribution::take_last_run()
            .expect("published run")
            .source,
        "generic"
    );
    let request = exact_roots(&["msg-a"]);
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
        prepare_input_evidence_with_scope_read_tx(tx, &scope(), request.borrowed(), true, read)
    })?;
    assert_eq!(
        prepare_attribution::take_last_run()
            .expect("published run")
            .source,
        "validated_recovery"
    );
    Ok(())
}

#[test]
fn growth_fixtures_attribute_width_and_single_payload_separately() -> Result<()> {
    let width: Vec<(String, usize)> = (0..48)
        .map(|index| (format!("wide-{index:03}"), 64))
        .collect();
    let (_directory, db) = seeded(&width)?;
    let ids: Vec<String> = width.iter().map(|(id, _)| id.clone()).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    prepare_context(&db, &exact_roots(&refs))?;
    let wide = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(wide.records, 48);
    let wide_bytes = wide
        .family_payload_bytes
        .get("message")
        .copied()
        .unwrap_or(0);
    assert!(wide_bytes >= 48 * 64 && wide_bytes < 48 * 1024);

    let (_directory, db) = seeded(&[("heavy".into(), 256 * 1024)])?;
    prepare_context(&db, &exact_roots(&["heavy"]))?;
    let heavy = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(heavy.records, 1);
    let heavy_bytes = heavy
        .family_payload_bytes
        .get("message")
        .copied()
        .unwrap_or(0);
    assert!(heavy_bytes >= 256 * 1024);
    // The harness distinguishes "many records" from "one large body" shapes.
    assert!(heavy_bytes > wide_bytes);
    Ok(())
}

#[test]
fn limit_rejected_prepare_attempt_is_still_attributed() -> Result<()> {
    let (_directory, db) = seeded(&[("msg-a".into(), 16)])?;
    // Root-only context reads treat request byte_limit as advisory at the DB
    // layer, so the deterministic in-prepare budget rejection is the qualified
    // input-history window limit. It fails after the attempt's attribution
    // begins, so the source stage still counts the rejected attempt.
    let mut request = exact_roots(&[]);
    request.windows = vec![InputWindowRequest {
        selector: InputWindowSelector::RecentBriefs,
        limit: InputWindowSelector::RecentBriefs.maximum_candidates() + 1,
    }];
    let before = stage_count("db.input.prepare.source.generic");
    let error = db
        .transaction(|tx| {
            let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
            prepare_input_evidence_with_scope_read_tx(tx, &scope(), request.borrowed(), false, read)
        })
        .err()
        .context("over-limit window is rejected")?;
    assert!(error.to_string().contains("input history limit"));
    assert!(
        stage_count("db.input.prepare.source.generic") >= before + 1,
        "a rejected attempt is still attributed to its source stage"
    );
    Ok(())
}

#[test]
fn empty_selection_reports_unknown_family_attribution() -> Result<()> {
    let (_directory, db) = seeded(&[("msg-a".into(), 16)])?;
    prepare_context(&db, &exact_roots(&[]))?;
    let run = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(run.records, 0);
    assert!(run.family_payload_bytes.is_empty());
    assert_eq!(run.dominant_family(), None);
    Ok(())
}

#[test]
fn recovery_chain_depth_expands_a_single_root_into_full_lineage() -> Result<()> {
    let (_directory, db, latest) = provider_lineage_fixture()?;
    let wanted = provider_lineage_request(&latest);
    let _snapshot =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    let run = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(run.source, "owner_change");
    // Depth dimension: one root closes over the whole provider-recovery
    // chain (3 messages, 2 turns, 2 attempts, 2 outcomes) instead of a
    // single flat record.
    assert_eq!(run.roots, 1);
    assert!(
        run.records >= 9,
        "recovery lineage should expand records, got {run:?}"
    );
    for family in ["attempt", "outcome", "turn"] {
        assert!(
            run.family_records.contains_key(family),
            "family {family} missing from {run:?}"
        );
    }
    Ok(())
}

#[test]
fn history_window_bounds_the_closure_selection_size() -> Result<()> {
    let (_directory, db) = fresh()?;
    // Six qualified turns, each closing over exactly one message.
    db.transaction(|tx| {
        for index in 0..6usize {
            let message_id = format!("window-message-{index}");
            let turn_id = format!("window-turn-{index}");
            let mut trigger = message(&message_id);
            trigger.turn_id = Some(turn_id.clone());
            let (committed, _) = append_message_tx(tx, &trigger)?;
            let mut turn = TurnRecord::new("agent-a", &turn_id, index as u64 + 1);
            turn.owner = Some(TurnOwner::AgentLifecycle {
                agent_id: "agent-a".into(),
            });
            turn.input_message_ids = vec![message_id.clone()];
            upsert_turn_record_tx(tx, &turn)?;
            let turn_record = ContextDurableRead::new(tx, "agent-a")
                .turn_by_id(&turn_id)?
                .context("fixture turn")?;
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[
                    CanonicalInputEvidence::Message(committed),
                    CanonicalInputEvidence::Turn(turn_record),
                ],
            )?;
        }
        Ok(())
    })?;
    let windowed = |limit: usize| InputEvidenceLoadRequest {
        query: None,
        windows: vec![InputWindowRequest {
            selector: InputWindowSelector::GlobalTurns,
            limit,
        }],
        mandatory_roots: Default::default(),
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: Default::default(),
        record_limit: 64,
        byte_limit: 1024 * 1024,
    };
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
        prepare_input_evidence_with_scope_read_tx(tx, &scope(), windowed(3).borrowed(), false, read)
    })?;
    let bounded = prepare_attribution::take_last_run().expect("published run");
    // The window picks the three newest turns; the closure then adds each
    // turn's input message, so the run reports both selected roots and the
    // actual record count.
    assert_eq!(bounded.roots, 3);
    assert_eq!(bounded.records, 6);
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
        prepare_input_evidence_with_scope_read_tx(tx, &scope(), windowed(6).borrowed(), false, read)
    })?;
    let full = prepare_attribution::take_last_run().expect("published run");
    assert_eq!(full.roots, 6);
    assert_eq!(full.records, 12);
    Ok(())
}
