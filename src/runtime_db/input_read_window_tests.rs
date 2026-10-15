//! Actual accepted rows exercise derived selectors before their first copy.
use super::*;
use crate::runtime::input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory};

fn policy() -> InputEvidenceLoadRequest {
    let mut request = request(InputWindowSelector::GlobalTurns, 1);
    request.record_limit = MAX_RECORDS;
    request.byte_limit = MAX_METADATA_BYTES;
    request
}

#[test]
fn actual_window_ids_use_blob_lengths_before_strings_and_ignore_raw_late_ids() -> Result<()> {
    let (_home, db) = fresh()?;
    let id = "\0".repeat(64 * 1024);
    db.transaction(|tx| {
        let turn = TurnRecord::new("agent-a", &id, 1);
        upsert_turn_record_tx(tx, &turn)?;
        register_committed_input_evidence_tx(tx, &scope(), &[CanonicalInputEvidence::Turn(turn)])?;
        // A newer unqualified row cannot enter the selected length pool.
        let raw = TurnRecord::new("agent-a", "raw-late-window".repeat(8192), 2);
        upsert_turn_record_tx(tx, &raw)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let full = policy();
    db.transaction(|tx| {
        let actual_scope = scope();
        let mut material = read_material::InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
        let read = current(prepare_input_evidence_snapshot_with_material_tx(tx,&actual_scope,&full,false,&mut material)?);
        let captured = read.resident.capture()?;
        assert_eq!(captured.windows[0].keys,vec![Key::new(Family::Turn,&id)]);
        assert!(captured.windows[0].is_exhaustive);
        // The raw-late member never grants either a body or a current alias.
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE record_id LIKE 'raw-late-window%'",[],|r|r.get::<_,usize>(0))?,0);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut small = full.clone();
    small.byte_limit = 128 * 1024;
    let before: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| {
                r.get(0)
            })?;
    let error = db
        .transaction(|tx| -> Result<()> {
            let actual_scope = scope();
            let mut material =
                read_material::InputReadMaterial::new(&actual_scope, &small, &ledger, (0, 0))?;
            material.scope_facts_tx(tx)?;
            material.binding_metadata_tx(tx)?;
            let binding = binding_on(tx, &actual_scope)?;
            // Calling the production selector itself pins the failure before any
            // later canonical-body/fingerprint gate can mask the ID allocation.
            qualified_window_keys(
                tx,
                &actual_scope,
                &binding,
                &InputWindowSelector::GlobalTurns,
                2,
                Some(&mut material),
            )?;
            anyhow::bail!("selected oversized ID bypassed its original phase")
        })
        .err()
        .context("oversized NUL-only ID was copied")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r
                .get::<_, usize>(
                0
            ))?,
        before
    );
    Ok(())
}

#[test]
fn actual_turn_transcript_ids_share_allowance_keep_equal_length_members_and_pruned_edges(
) -> Result<()> {
    let first = "first".repeat(8192);
    let second = "other".repeat(8192);
    let (_home, db) = legacy(|tx| {
        let turn = TurnRecord::new("agent-a", "window-owned-turn", 1);
        upsert_turn_record_tx(tx, &turn)?;
        for id in [&first, &second] {
            let mut trace = TranscriptEntry::new(
                "agent-a",
                TranscriptEntryKind::AssistantRound,
                None,
                None,
                serde_json::json!({"turn_id":"window-owned-turn","text":"actual original transcript"}),
            );
            trace.id = id.clone();
            trace.created_at = time();
            append_transcript_entry_tx(tx, &trace)?;
        }
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let full = policy();
    let held = db.transaction(|tx| {
        let actual_scope = scope();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &full, &ledger, (0, 0))?;
        let read = current(prepare_input_evidence_snapshot_with_material_tx(
            tx,
            &actual_scope,
            &full,
            false,
            &mut material,
        )?);
        let capture = read.resident.capture()?;
        assert_eq!(
            capture
                .transcripts_for_turns(&["window-owned-turn".into()])?
                .len(),
            2
        );
        let weak = material.into_guards();
        Ok((read, capture, weak))
    })?;
    let guards = held.2.iter().map(Arc::downgrade).collect::<Vec<_>>();
    let mut small = full.clone();
    small.byte_limit = 128 * 1024;
    let aliases: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| {
                r.get(0)
            })?;
    let error = db
        .transaction(|tx| -> Result<()> {
            let actual_scope = scope();
            let mut material =
                read_material::InputReadMaterial::new(&actual_scope, &small, &ledger, (0, 0))?;
            material.scope_facts_tx(tx)?;
            material.binding_metadata_tx(tx)?;
            let binding = binding_on(tx, &actual_scope)?;
            qualified_turn_transcript_keys(
                tx,
                &actual_scope,
                &binding,
                "window-owned-turn",
                MAX_RECORDS,
                Some(&mut material),
            )?;
            anyhow::bail!("actual Transcript owning IDs bypassed phase")
        })
        .err()
        .context("owning IDs received a second full allowance")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r
                .get::<_, usize>(
                0
            ))?,
        aliases
    );
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id=?1",
            [&first],
        )?;
        register_pruned_input_evidence_tx(tx, &scope(), &[Key::new(Family::Transcript, &first)])?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let actual_scope = scope();
        let binding = binding_on(tx, &actual_scope)?;
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &full, &ledger, (0, 0))?;
        let keys = qualified_turn_transcript_keys(
            tx,
            &actual_scope,
            &binding,
            "window-owned-turn",
            MAX_RECORDS,
            Some(&mut material),
        )?;
        assert_eq!(
            keys,
            BTreeSet::from([
                Key::new(Family::Transcript, &first),
                Key::new(Family::Transcript, &second)
            ])
        );
        Ok(())
    })?;
    assert_eq!(
        held.1
            .transcripts_for_turns(&["window-owned-turn".into()])?
            .len(),
        2,
        "old independently selected pins preserve bytes, not durable qualification"
    );
    assert!(guards.iter().all(|guard| guard.upgrade().is_some()));
    drop(held);
    assert!(guards.iter().all(|guard| guard.upgrade().is_none()));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn actual_qualified_owning_reference_ids_are_admitted_before_set_copies_with_shared_body_prior(
) -> Result<()> {
    let (_home, db) = fresh()?;
    let source = "accepted-long-source".repeat(4096);
    let record = db.transaction(|tx| {
        append_message_tx(tx, &message(&source))?;
        let mut turn = TurnRecord::new("agent-a", "reference-owner", 1);
        turn.input_message_ids.push(source.clone());
        upsert_turn_record_tx(tx, &turn)?;
        let delta = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn)],
        )?;
        delta
            .records
            .into_iter()
            .find(|r| r.locator.key == Key::new(Family::Turn, "reference-owner"))
            .context("actual qualified Turn missing")
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let body_pins = ledger.register_view(&scope(), InputMemoryCategory::Pins, &[record.clone()])?;
    let shared = record.resident_material_estimate()?;
    let baseline = ledger.report()?.observed_bytes;
    let mut small = policy();
    small.byte_limit = 256 * 1024;
    assert!(shared.0 + 8192 < small.byte_limit);
    let actual_scope = scope();
    let mut material =
        read_material::InputReadMaterial::new(&actual_scope, &small, &ledger, shared)?;
    let error = material
        .reference_selectors(&record)
        .err()
        .context("derived copies bypassed shared body prior")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let full = policy();
    let mut material =
        read_material::InputReadMaterial::new(&actual_scope, &full, &ledger, shared)?;
    material.reference_selectors(&record)?;
    let refs = record.references();
    assert_eq!(refs, BTreeSet::from([Key::new(Family::Message, &source)]));
    let guards = material.into_guards();
    let weak = guards.iter().map(Arc::downgrade).collect::<Vec<_>>();
    let held = guards.clone();
    drop(guards);
    assert!(weak.iter().all(|g| g.upgrade().is_some()));
    drop((refs, held));
    assert!(weak.iter().all(|g| g.upgrade().is_none()));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((body_pins, record));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
