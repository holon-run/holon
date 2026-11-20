use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn owner_window_prepares_legacy_turn_selected_by_global_window() -> Result<()> {
    let (_directory, db) = fresh()?;
    let owner = TurnOwner::WorkItem {
        work_item_id: "owned-work".into(),
    };
    db.transaction(|tx| {
        for index in 1..=2 {
            let mut turn = TurnRecord::new("agent-a", format!("turn-{index}"), index);
            turn.current_work_item_id = owner.work_item_id().map(str::to_owned);
            upsert_turn_record_tx(tx, &turn)?;
            let entry = TranscriptEntry::new(
                "agent-a",
                TranscriptEntryKind::AssistantRound,
                None,
                None,
                serde_json::json!({"turn_id": turn.turn_id}),
            );
            let entry = append_transcript_entry_tx(tx, &entry)?.0;
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[
                    CanonicalInputEvidence::Turn(turn),
                    CanonicalInputEvidence::Transcript(entry),
                ],
            )?;
        }
        Ok(())
    })?;

    let mut policy = request(InputWindowSelector::GlobalTurns, 2);
    let read = |policy: &InputEvidenceLoadRequest| {
        db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), policy))
            .map(current)
    };
    assert!(read(&policy)?
        .resident
        .capture()?
        .turn_transcript_keys
        .is_empty());
    let selector = InputWindowSelector::OwnerTurns { owner };
    policy.windows.push(InputWindowRequest {
        selector: selector.clone(),
        limit: 1,
    });
    let prepared = read(&policy)?.resident;
    let captured = prepared.capture()?;
    assert_eq!(captured.window(&selector, 1)?[0].locator.key.id, "turn-2");
    assert_eq!(captured.transcripts_for_turns(&["turn-2".into()])?.len(), 1);
    assert!(!captured.turn_transcript_keys.contains_key("turn-1"));
    assert!(prepared
        .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
        .is_some());
    Ok(())
}

fn migration() -> &'static crate::runtime_db::migrations::Migration {
    MIGRATIONS
        .iter()
        .find(|migration| migration.name == "index_turn_transcript_qualification")
        .expect("turn transcript index migration")
}

fn predecessor() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for previous in MIGRATIONS
        .iter()
        .take_while(|previous| previous.version < migration().version)
    {
        apply_migration(&mut connection, previous)?;
    }
    Ok(connection)
}

fn select(connection: &mut Connection, seed_id: Option<&str>, turn: &str) -> Result<Vec<String>> {
    let tx = connection.transaction()?;
    Ok(qualified_turn_transcript_keys(
        &tx,
        &scope(),
        &Binding {
            seed_id: seed_id.map(str::to_owned),
        },
        turn,
        1,
    )?
    .into_iter()
    .map(|key| key.id)
    .collect())
}

#[test]
fn turn_transcript_upgrade_preserves_overrides_and_bounds_history_scans() -> Result<()> {
    let mut connection = predecessor()?;
    connection.execute_batch(
        r#"
BEGIN;
INSERT INTO episode_continuity_seeds(
  seed_id,agent_id,policy_version,boundary_id,identity_incarnation,
  source_kind,roots_json,candidate_payloads_json,import_required,captured_at
) VALUES
  ('seed-a','agent-a',1,'fixture',1,'upgrade_identity','{}','[]',0,'2026-10-01'),
  ('seed-b','agent-b',1,'fixture',1,'upgrade_identity','{}','[]',0,'2026-10-01');
WITH RECURSIVE history(seq) AS (
  VALUES(1) UNION ALL SELECT seq+1 FROM history WHERE seq<50000
)
INSERT INTO input_evidence_current(
  agent_id,identity_incarnation,family,record_id,revision,
  fingerprint_json,availability,qualification
)
SELECT 'agent-a',1,'transcript',printf('current-old-%05d',seq),1,
  '{"related_turn_id":"old-turn"}','present','committed_writer' FROM history;
INSERT INTO input_evidence_eligibility
SELECT 'seed-a','upgrade','transcript','seed-'||record_id,fingerprint_json,0
  FROM input_evidence_current;
INSERT INTO input_evidence_seed_recovery_current(
  seed_id,family,record_id,revision,fingerprint_json,origin_kind
)
SELECT 'seed-a','transcript','recovery-'||record_id,1,fingerprint_json,'fixed_member'
  FROM input_evidence_current;
INSERT INTO input_evidence_current(
  agent_id,identity_incarnation,family,record_id,revision,
  fingerprint_json,availability,qualification
) VALUES
  ('agent-a',1,'transcript','current',1,'{"related_turn_id":"target"}','present','committed_writer'),
  ('agent-a',1,'transcript','current-overrides-seed',1,'{"related_turn_id":"other"}','present','committed_writer'),
  ('agent-a',1,'transcript','pruned-overrides-seed',1,'{"related_turn_id":"other"}','pruned','committed_writer'),
  ('agent-a',1,'transcript','current-overrides-recovery',1,'{"related_turn_id":"other"}','present','committed_writer'),
  ('agent-a',2,'transcript','other-incarnation',1,'{"related_turn_id":"target"}','present','committed_writer'),
  ('agent-b',1,'transcript','other-agent',1,'{"related_turn_id":"target"}','present','committed_writer'),
  ('agent-a',1,'message','not-transcript',1,'not-json','present','committed_writer');
INSERT INTO input_evidence_eligibility VALUES
  ('seed-a','upgrade','transcript','seed', '{"related_turn_id":"target"}',0),
  ('seed-a','upgrade','transcript','current-overrides-seed','{"related_turn_id":"target"}',0),
  ('seed-a','upgrade','transcript','pruned-overrides-seed','{"related_turn_id":"target"}',0),
  ('seed-a','upgrade','transcript','recovery-overrides-seed','{"related_turn_id":"target"}',0),
  ('seed-a','upgrade','transcript','recovery-reassigned','{"related_turn_id":"target"}',0),
  ('seed-b','upgrade','transcript','other-seed','{"related_turn_id":"target"}',0),
  ('seed-a','upgrade','message','not-transcript','not-json',0);
INSERT INTO input_evidence_seed_recovery_current(
  seed_id,family,record_id,revision,fingerprint_json,origin_kind
) VALUES
  ('seed-a','transcript','recovery',1,'{"related_turn_id":"target"}','fixed_member'),
  ('seed-a','transcript','recovery-overrides-seed',1,'{"related_turn_id":"target"}','fixed_member'),
  ('seed-a','transcript','recovery-reassigned',1,'{"related_turn_id":"other"}','fixed_member'),
  ('seed-a','transcript','current-overrides-recovery',1,'{"related_turn_id":"target"}','fixed_member'),
  ('seed-b','transcript','other-recovery-seed',1,'{"related_turn_id":"target"}','fixed_member'),
  ('seed-a','message','not-transcript',1,'not-json','fixed_member');
COMMIT;
"#,
    )?;
    let expected = ["current", "recovery", "recovery-overrides-seed", "seed"];
    assert_eq!(select(&mut connection, Some("seed-a"), "target")?, expected);
    assert_eq!(select(&mut connection, None, "target")?, ["current"]);
    assert_eq!(
        select(&mut connection, Some("seed-a"), "other")?,
        [
            "current-overrides-recovery",
            "current-overrides-seed",
            "pruned-overrides-seed",
            "recovery-reassigned",
        ]
    );

    let counts = |connection: &Connection| -> Result<Vec<i64>> {
        [
            "input_evidence_current",
            "input_evidence_eligibility",
            "input_evidence_seed_recovery_current",
        ]
        .into_iter()
        .map(|table| {
            Ok(connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
        })
        .collect()
    };
    let before = counts(&connection)?;
    apply_migration(&mut connection, migration())?;
    assert_eq!(counts(&connection)?, before);
    assert_eq!(
        crate::runtime_db::migrations::current_schema_version(&connection)?,
        migration().version
    );

    // The actual selector must avoid three long tails, regardless of timing.
    let ticks = Arc::new(AtomicUsize::new(0));
    connection.progress_handler(
        1_000,
        Some({
            let ticks = ticks.clone();
            move || {
                ticks.fetch_add(1, Ordering::Relaxed);
                false
            }
        }),
    );
    let selected = select(&mut connection, Some("seed-a"), "target");
    connection.progress_handler(0, None::<fn() -> bool>);
    assert_eq!(selected?, expected);
    assert!(
        ticks.load(Ordering::Relaxed) < 10,
        "turn transcript lookup scanned unrelated history: at least {} VM steps",
        ticks.load(Ordering::Relaxed) * 1_000
    );
    assert_eq!(select(&mut connection, None, "target")?, ["current"]);
    assert_eq!(
        select(&mut connection, Some("seed-a"), "other")?,
        [
            "current-overrides-recovery",
            "current-overrides-seed",
            "pruned-overrides-seed",
            "recovery-reassigned",
        ]
    );
    Ok(())
}

#[test]
fn turn_transcript_upgrade_accepts_preinstalled_indexes_without_rebuilding() -> Result<()> {
    let mut connection = predecessor()?;
    connection.execute_batch(migration().sql)?;
    let indexes = |connection: &Connection| -> Result<Vec<(String, i64)>> {
        let mut statement = connection.prepare(
            "SELECT name,rootpage FROM sqlite_schema
             WHERE type='index' AND name LIKE 'idx_input_evidence_%_transcript_turn'
             ORDER BY name",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    let before = indexes(&connection)?;
    assert_eq!(before.len(), 3);
    let schema_version: i64 =
        connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    apply_migration(&mut connection, migration())?;
    assert_eq!(indexes(&connection)?, before);
    apply_migration(&mut connection, migration())?;
    assert_eq!(indexes(&connection)?, before);
    assert_eq!(
        connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?,
        schema_version
    );
    assert_eq!(
        connection.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version=?1",
            [migration().version],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );
    Ok(())
}

#[test]
fn committed_transcripts_extend_turn_coverage_without_refreshing_existing_turns() -> Result<()> {
    let (_home, db) = fresh()?;
    let owner = crate::types::TurnOwner::WorkItem {
        work_item_id: "covered-work".into(),
    };
    let mut turn = TurnRecord::new("agent-a", "covered-turn", 1);
    turn.created_at = time();
    turn.owner = Some(owner.clone());
    turn.current_work_item_id = owner.work_item_id().map(str::to_owned);
    let turn_key = Key::new(Family::Turn, &turn.turn_id);
    let policy = request(InputWindowSelector::OwnerTurns { owner }, 2);
    let initial =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    let start = db.transaction(|tx| {
        upsert_turn_record_tx(tx, &turn)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn.clone())],
        )
    })?;
    assert_eq!(
        start.turn_transcript_keys.get(&turn.turn_id),
        Some(&BTreeSet::new())
    );
    let roots = BTreeSet::from([turn_key.clone()]);
    let mut resident = initial
        .resident
        .with_committed(&start, roots.clone())?
        .unwrap();

    let mut keys = BTreeSet::new();
    for (round, kind) in [
        (1, TranscriptEntryKind::AssistantRound),
        (2, TranscriptEntryKind::ToolResults),
    ] {
        let mut transcript = TranscriptEntry::new(
            "agent-a",
            kind,
            Some(round),
            None,
            serde_json::json!({"turn_id": turn.turn_id}),
        );
        transcript.id = format!("committed-transcript-{round}");
        keys.insert(Key::new(Family::Transcript, &transcript.id));
        for _ in 0..2 {
            let delta = db.transaction(|tx| {
                let actual = append_transcript_entry_tx(tx, &transcript)?.0;
                register_committed_input_evidence_tx(
                    tx,
                    &scope(),
                    &[CanonicalInputEvidence::Transcript(actual)],
                )
            })?;
            assert!(delta
                .records
                .iter()
                .any(|record| record.locator.key == turn_key));
            assert!(delta.turn_transcript_keys.is_empty());
            resident = resident.with_committed(&delta, roots.clone())?.unwrap();
            assert_eq!(
                resident.capture()?.turn_transcript_keys[&turn.turn_id],
                keys
            );
        }
    }
    let mut terminal = turn.clone();
    terminal.terminal = Some(crate::types::TurnTerminalSummary {
        kind: crate::types::TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: time(),
        duration_ms: 1,
    });
    let settled = db.transaction(|tx| {
        upsert_turn_record_tx(tx, &terminal)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(terminal.clone())],
        )
    })?;
    assert!(settled.turn_transcript_keys.is_empty());
    resident = resident.with_committed(&settled, BTreeSet::new())?.unwrap();

    let pruned = keys.first().context("fixture transcript")?;
    let removed = db.transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id=?1",
            [&pruned.id],
        )?;
        register_pruned_input_evidence_tx(tx, &scope(), std::slice::from_ref(pruned))
    })?;
    resident = resident.with_committed(&removed, BTreeSet::new())?.unwrap();

    let mut successor = TurnRecord::new("agent-a", "next-turn", 2);
    successor.owner = turn.owner.clone();
    successor.current_work_item_id = turn.current_work_item_id.clone();
    let next = db.transaction(|tx| {
        upsert_turn_record_tx(tx, &successor)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(successor.clone())],
        )
    })?;
    let roots = BTreeSet::from([Key::new(Family::Turn, &successor.turn_id)]);
    resident = resident.with_committed(&next, roots.clone())?.unwrap();

    let moved = keys.last().context("second fixture transcript")?;
    let reassigned = db.transaction(|tx| {
        use crate::runtime_db::evidence::{committed_transcript_tx, upsert_transcript_entry_tx};
        let mut actual = committed_transcript_tx(tx, "agent-a", &moved.id)?;
        actual.data["turn_id"] = serde_json::json!(successor.turn_id);
        upsert_transcript_entry_tx(tx, &actual)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Transcript(actual)],
        )
    })?;
    assert!(reassigned.turn_transcript_keys.is_empty());
    let warm = resident
        .with_committed(&reassigned, roots)?
        .unwrap()
        .capture()?;
    assert!(warm.unavailable.contains(pruned));
    assert!(warm
        .transcripts_for_turns(std::slice::from_ref(&turn.turn_id))?
        .is_empty());
    assert_eq!(
        warm.transcripts_for_turns(std::slice::from_ref(&successor.turn_id))?
            .len(),
        1
    );
    for turn_id in [turn.turn_id, successor.turn_id] {
        let cold_keys = db.transaction(|tx| {
            let scope = scope();
            qualified_turn_transcript_keys(
                tx,
                &scope,
                &binding_on(tx, &scope)?,
                &turn_id,
                MAX_RECORDS,
            )
        })?;
        assert_eq!(warm.turn_transcript_keys[&turn_id], cold_keys);
    }
    Ok(())
}

#[test]
fn first_work_turn_registration_includes_previously_qualified_transcripts() -> Result<()> {
    let (_home, db) = fresh()?;
    let owner = crate::types::TurnOwner::WorkItem {
        work_item_id: "preallocated-work".into(),
    };
    let mut turn = TurnRecord::new("agent-a", "preallocated-turn", 1);
    turn.current_work_item_id = owner.work_item_id().map(str::to_owned);
    turn.owner = Some(owner);
    let turn_key = Key::new(Family::Turn, &turn.turn_id);
    let transcript = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        None,
        serde_json::json!({"turn_id": turn.turn_id}),
    );
    let transcript_key = Key::new(Family::Transcript, &transcript.id);
    db.transaction(|tx| {
        let (actual, _) = append_transcript_entry_tx(tx, &transcript)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Transcript(actual)],
        )?;
        Ok(())
    })?;

    let policy = request(InputWindowSelector::RecentTranscript, 1);
    let prepared =
        current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    assert!(prepared.resident.capture()?.turn_transcript_keys.is_empty());

    let start = db.transaction(|tx| {
        assert!(payload_on(tx, "agent-a", &turn_key)?.is_none());
        validate_existing_input_evidence_tx(tx, &scope(), &transcript_key)?;
        upsert_turn_record_tx(tx, &turn)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn.clone())],
        )
    })?;

    assert_eq!(
        start.turn_transcript_keys.get(&turn.turn_id),
        Some(&BTreeSet::from([transcript_key.clone()])),
    );
    let resident = prepared
        .resident
        .with_committed(&start, BTreeSet::from([turn_key]))?
        .context("the new Turn can reuse the previously loaded Transcript")?;
    let captured = resident.capture()?;
    let entries = captured.transcripts_for_turns(std::slice::from_ref(&turn.turn_id))?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].locator.key, transcript_key);
    Ok(())
}

#[test]
fn only_owner_history_hydrates_turn_transcripts_in_an_ordinary_snapshot() -> Result<()> {
    use crate::runtime::resident_input_evidence::InputEvidenceReadError;
    use crate::types::TurnOwner;

    let (_home, db) = fresh()?;
    let owner = TurnOwner::WorkItem {
        work_item_id: "owned-work".into(),
    };
    let mut owned = TurnRecord::new("agent-a", "owned-turn", 1);
    owned.owner = Some(owner.clone());
    owned.current_work_item_id = owner.work_item_id().map(str::to_owned);
    let global = TurnRecord::new("agent-a", "global-only", 2);
    let (owned_transcript, global_transcript) = db.transaction(|tx| {
        let mut transcript_ids = Vec::new();
        for turn in [&owned, &global] {
            upsert_turn_record_tx(tx, turn)?;
            let entry = TranscriptEntry::new(
                "agent-a",
                TranscriptEntryKind::AssistantRound,
                Some(1),
                None,
                serde_json::json!({"turn_id": turn.turn_id}),
            );
            let actual = append_transcript_entry_tx(tx, &entry)?.0;
            transcript_ids.push(Key::new(Family::Transcript, &actual.id));
            let delta = register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[
                    CanonicalInputEvidence::Turn(turn.clone()),
                    CanonicalInputEvidence::Transcript(actual),
                ],
            )?;
            assert_eq!(
                delta.turn_transcript_keys.contains_key(&turn.turn_id),
                turn.current_work_item_id.is_some(),
            );
        }
        Ok((transcript_ids.remove(0), transcript_ids.remove(0)))
    })?;
    let mut policy = request(InputWindowSelector::GlobalTurns, 2);
    policy.windows.push(InputWindowRequest {
        selector: InputWindowSelector::RecentTranscript,
        limit: 1,
    });
    let load = |policy: &InputEvidenceLoadRequest| {
        Ok::<_, anyhow::Error>(
            current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), policy))?)
                .resident
                .capture()?,
        )
    };
    let global_history = load(&policy)?;
    assert!(global_history.turn_transcript_keys.is_empty());
    assert!(global_history.lookup(&global_transcript)?.is_some());
    assert!(matches!(
        global_history.transcripts_for_turns(std::slice::from_ref(&global.turn_id)),
        Err(InputEvidenceReadError::NeedsPreparation(_)),
    ));
    policy.windows.push(InputWindowRequest {
        selector: InputWindowSelector::OwnerTurns { owner },
        limit: 1,
    });
    let scoped_history = load(&policy)?;
    assert_eq!(
        scoped_history.turn_transcript_keys,
        BTreeMap::from([(owned.turn_id, BTreeSet::from([owned_transcript.clone()]))]),
    );
    assert!(scoped_history.lookup(&owned_transcript)?.is_some());
    assert!(scoped_history.lookup(&global_transcript)?.is_some());
    Ok(())
}
