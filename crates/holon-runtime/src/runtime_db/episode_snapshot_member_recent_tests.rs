use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

const AGENT: &str = "member-recent";
const SEED: &str = "member-seed";
type StoredMember = (String, String, String);

fn record(id: String, index: usize, receipted: bool) -> Result<EpisodeRow> {
    let base: DateTime<Utc> = "2026-10-18T00:00:00Z".parse()?;
    let record = serde_json::from_value(serde_json::json!({
        "id": id, "agent_id": AGENT,
        "created_at": base + chrono::Duration::seconds((index / 2) as i64),
        "finalized_at": base + chrono::Duration::seconds((index / 4) as i64),
        "start_turn_index": 1, "end_turn_index": 1,
        "start_message_count": 0, "end_message_count": 1,
        "boundary_reason": "wait_boundary",
        "current_work_item_id": if index % 5 == 0 { Some("live-work") } else { None },
    }))?;
    Ok(EpisodeRow {
        record,
        payload: format!("payload:{index}:\0界"),
        receipted,
    })
}

fn records(count: usize) -> Result<Vec<EpisodeRow>> {
    (0..count)
        .map(|index| {
            let id = match index {
                0 => String::new(),
                1 => "\"\\\0\n界İß\u{2028}".into(),
                2 => "界\0\"\\".repeat(16_384),
                _ => format!("episode-{index:03}"),
            };
            record(id, index, index % 7 == 3)
        })
        .collect()
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE member_markers (value TEXT);")?;
    connection.execute(
        "INSERT INTO episode_continuity_seeds
         (seed_id, agent_id, policy_version, boundary_id, source_kind, roots_json,
          candidate_payloads_json, import_required, captured_at)
         VALUES (?1, ?2, 1, 'boundary', 'upgrade_unbound', '{}', '{}', 0, 'original')",
        params![SEED, AGENT],
    )?;
    Ok(connection)
}

fn stored(connection: &Connection) -> Result<Vec<StoredMember>> {
    let mut statement = connection.prepare(
        "SELECT episode_id, content_digest, qualification_json
         FROM episode_continuity_members ORDER BY rowid",
    )?;
    let members = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(members)
}

// Independent original owning-set selection, including its stable sort and JSON.
fn original(
    mut rows: Vec<EpisodeRow>,
    roots: &EpisodeContinuityRoots,
) -> Result<Vec<StoredMember>> {
    rows.sort_by(|left, right| {
        right
            .record
            .finalized_at
            .cmp(&left.record.finalized_at)
            .then_with(|| right.record.created_at.cmp(&left.record.created_at))
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    let recent: BTreeSet<String> = rows
        .iter()
        .filter(|row| !row.receipted)
        .take(12)
        .map(|row| row.record.id.clone())
        .collect();
    let mut expected = Vec::new();
    for row in rows.into_iter().filter(|row| !row.receipted) {
        let mut reasons = BTreeSet::new();
        if row
            .record
            .current_work_item_id
            .as_ref()
            .is_some_and(|id| roots.work_item_ids.contains(id))
        {
            reasons.insert("live_owner");
        }
        if roots
            .source_refs
            .contains(&format!("episode:{}", row.record.id))
        {
            reasons.insert("explicit_live_ref");
        }
        if recent.contains(&row.record.id) {
            reasons.insert("explicit_upgrade_recent_candidate");
        }
        if !reasons.is_empty() {
            expected.push((
                row.record.id,
                format!("{:x}", Sha256::digest(row.payload.as_bytes())),
                serde_json::to_string(&reasons)?,
            ));
        }
    }
    Ok(expected)
}

#[test]
fn recent_slots_borrow_original_ids_and_preserve_duplicates_receipts_and_cutoff() -> Result<()> {
    assert_eq!(LEGACY_EPISODE_RECENT_CANDIDATES, 12);
    assert_eq!(
        std::mem::size_of::<[Option<&str>; LEGACY_EPISODE_RECENT_CANDIDATES]>(),
        12 * std::mem::size_of::<Option<&str>>()
    );
    for count in [0, 1, 11, 12, 13, 40] {
        let mut rows = records(count)?;
        if count > 13 {
            rows[12] = record(rows[1].record.id.clone(), 12, false)?;
            rows[13] = record(rows[0].record.id.clone(), 13, false)?;
            rows[count - 1] = record(rows[1].record.id.clone(), count - 1, false)?;
        }
        let old: BTreeSet<String> = rows
            .iter()
            .filter(|row| !row.receipted)
            .take(12)
            .map(|row| row.record.id.clone())
            .collect();
        let actual = recent_episode_ids(&rows);
        for (slot, row) in actual
            .iter()
            .flatten()
            .zip(rows.iter().filter(|row| !row.receipted).take(12))
        {
            assert!(std::ptr::eq(*slot, row.record.id.as_str()));
        }
        assert_eq!(
            actual.iter().flatten().count(),
            rows.iter().filter(|row| !row.receipted).count().min(12)
        );
        for row in &rows {
            assert_eq!(
                actual.contains(&Some(row.record.id.as_str())),
                old.contains(&row.record.id)
            );
        }
    }
    let mut rows = records(40)?;
    for row in &mut rows {
        row.receipted = true;
    }
    assert_eq!(recent_episode_ids(&rows), [None; 12]);
    Ok(())
}

#[test]
fn member_capture_matches_original_sql_order_reasons_and_bytes() -> Result<()> {
    for count in [0, 1, 11, 12, 13, 40] {
        for reverse in [false, true] {
            let mut rows = records(count)?;
            if reverse {
                rows.reverse();
            }
            let roots = EpisodeContinuityRoots {
                work_item_ids: ["live-work".into()].into(),
                source_refs: ["episode:".into(), "episode:episode-004".into()].into(),
                ..EpisodeContinuityRoots::default()
            };
            let expected = original(records(count)?, &roots)?;
            let mut connection = connection()?;
            for phase in ["upgrade", "first_import"] {
                let tx = connection.transaction()?;
                // Rebuild owned input outside the kernel; no output owns its IDs.
                let input = if phase == "upgrade" {
                    std::mem::take(&mut rows)
                } else {
                    records(count)?
                };
                capture_members_tx(&tx, SEED, phase, &roots, input)?;
                assert_eq!(stored(&tx)?, expected);
                assert_eq!(
                    tx.query_row(
                        "SELECT COUNT(*) FROM episode_continuity_members WHERE capture_phase != ?1",
                        [phase],
                        |row| row.get::<_, u64>(0)
                    )?,
                    0
                );
                tx.rollback()?;
            }
        }
    }
    Ok(())
}

#[test]
fn recent_selection_and_capture_reuse_exhausted_caller_phase_until_last_holder() -> Result<()> {
    let rows = records(40)?;
    let expected = original(records(40)?, &EpisodeContinuityRoots::default())?;
    let mut connection = connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let initial = ((AGENT.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let exact = (
        initial.0 + rows.len() as u64 * std::mem::size_of::<usize>() as u64 * 8,
        initial.1 + rows.len() * 8,
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        {
            let selected = recent_episode_ids(&rows);
            assert_eq!(selected.iter().flatten().count(), 12);
            assert_eq!(phase.finish()?, initial);
        }
        let tx = connection.transaction()?;
        capture_members_tx(
            &tx,
            SEED,
            "upgrade",
            &EpisodeContinuityRoots::default(),
            rows,
        )?;
        assert_eq!(stored(&tx)?, expected);
        tx.commit()?;
        assert_eq!(phase.finish()?, exact);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(stored(&connection)?, expected);
    Ok(())
}

#[test]
fn member_sql_abort_and_duplicate_conflict_release_rows_and_rollback_whole_transaction(
) -> Result<()> {
    for duplicate in [false, true] {
        let mut rows = records(40)?;
        if duplicate {
            rows = vec![
                record("duplicate".into(), 0, false)?,
                record("duplicate".into(), 0, false)?,
            ];
        }
        let expected = original(
            if duplicate {
                vec![record("duplicate".into(), 0, false)?]
            } else {
                records(40)?
            },
            &EpisodeContinuityRoots::default(),
        )?;
        let mut connection = connection()?;
        if !duplicate {
            connection.execute_batch(
                "CREATE TRIGGER reject_member BEFORE INSERT ON episode_continuity_members
                 WHEN (SELECT COUNT(*) FROM episode_continuity_members) > 0
                 BEGIN SELECT RAISE(ABORT, 'member SQL failure'); END;",
            )?;
        }
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: AGENT.into(),
            identity_incarnation: 1,
        };
        let exact = (
            (AGENT.len() as u64
                + 256
                + 31
                + rows.len() as u64 * std::mem::size_of::<usize>() as u64)
                * 8,
            64 + (3 + rows.len()) * 8,
        );
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(31, 3)?;
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO member_markers VALUES ('rollback')", [])?;
            let error = capture_members_tx(
                &tx,
                SEED,
                "upgrade",
                &EpisodeContinuityRoots::default(),
                rows,
            )
            .expect_err("member capture unexpectedly succeeded");
            let sql_error = error.downcast_ref::<rusqlite::Error>().unwrap();
            assert_eq!(
                sql_error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::ConstraintViolation)
            );
            if !duplicate {
                assert!(format!("{error:#}").contains("member SQL failure"));
            }
            assert_eq!(stored(&tx)?, expected[..1]);
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            // No borrowed row/statement escapes either SQL failure.
            tx.execute_batch("DROP TABLE episode_continuity_members;")?;
            tx.rollback()?;
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(stored(&connection)?.is_empty());
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM member_markers", [], |row| {
                row.get::<_, u64>(0)
            })?,
            0
        );
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
