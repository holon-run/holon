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

const AGENT: &str = "member-sort";
const SEED: &str = "sort-seed";

fn member(id: &str, index: usize, receipted: bool) -> Result<EpisodeRow> {
    let base: DateTime<Utc> = "2026-10-18T00:00:00Z".parse()?;
    Ok(EpisodeRow {
        record: serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": AGENT,
            "created_at": base + chrono::Duration::seconds((index % 3) as i64),
            "finalized_at": base + chrono::Duration::seconds((index % 5) as i64),
            "start_turn_index": 1, "end_turn_index": 1,
            "start_message_count": 0, "end_message_count": 1,
            "boundary_reason": "wait_boundary",
        }))?,
        payload: format!("payload:{index}:\0界"),
        receipted,
    })
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE sort_markers (value TEXT);")?;
    connection.execute(
        "INSERT INTO episode_continuity_seeds
         (seed_id, agent_id, policy_version, boundary_id, source_kind, roots_json,
          candidate_payloads_json, import_required, captured_at)
         VALUES (?1, ?2, 1, 'boundary', 'upgrade_unbound', '{}', '{}', 0, 'original')",
        params![SEED, AGENT],
    )?;
    Ok(connection)
}

fn stored(connection: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut statement = connection.prepare(
        "SELECT capture_phase, episode_id, content_digest
         FROM episode_continuity_members ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    }
}

fn initial() -> (u64, usize) {
    ((AGENT.len() as u64 + 256 + 31) * 8, 64 + 3 * 8)
}

#[test]
fn member_sort_indices_match_original_stable_order_and_borrow_original_rows() -> Result<()> {
    for count in [0, 1, 2, 12, 20, 21, 64, 4096] {
        let mut rows = (0..count)
            .map(|index| {
                let id = ["", "a", "a\0", "界", "\"\\"][index % 5];
                member(id, index, index % 7 == 0)
            })
            .collect::<Result<Vec<_>>>()?;
        rows.reverse();
        // The original stable comparator intentionally has no position tie-break.
        let mut expected: Vec<usize> = (0..count).collect();
        expected.sort_by(|&left, &right| {
            rows[right]
                .record
                .finalized_at
                .cmp(&rows[left].record.finalized_at)
                .then_with(|| {
                    rows[right]
                        .record
                        .created_at
                        .cmp(&rows[left].record.created_at)
                })
                .then_with(|| rows[left].record.id.cmp(&rows[right].record.id))
        });
        let order = member_capture_order(&rows)?;
        if count < 2 {
            assert!(order.is_empty());
            assert_eq!(order.capacity(), 0);
        } else {
            assert_eq!(order, expected);
        }
        let ordered = (0..count).map(|index| &rows[order.get(index).copied().unwrap_or(index)]);
        for (row, &index) in ordered.clone().zip(&expected) {
            assert!(std::ptr::eq(row, &rows[index]));
        }
        let original_recent: Vec<&str> = expected
            .iter()
            .map(|&index| &rows[index])
            .filter(|row| !row.receipted)
            .take(12)
            .map(|row| row.record.id.as_str())
            .collect();
        assert_eq!(
            recent_episode_ids(ordered)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
            original_recent
        );
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn member_sort_charges_exact_slots_on_same_phase_and_keeps_last_holder() -> Result<()> {
    let start = initial();
    let slots = 2 + 33 + 64;
    let exact = (
        start.0 + slots as u64 * std::mem::size_of::<usize>() as u64 * 8,
        start.1 + slots * 8,
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = material.guard()?;
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        let mut used = start;
        for count in [2, 33, 64] {
            let rows = (0..count)
                .map(|index| member("same-id", index, true))
                .collect::<Result<Vec<_>>>()?;
            let order = member_capture_order(&rows)?;
            assert_eq!(order.len(), count);
            used.0 += count as u64 * std::mem::size_of::<usize>() as u64 * 8;
            used.1 += count * 8;
            assert_eq!(material.finish()?, used);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert_eq!(used, exact);
        assert!(member_capture_order(&[])?.is_empty());
        assert!(member_capture_order(&[member("single", 0, false)?])?.is_empty());
        assert_eq!(material.finish()?, exact);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((material, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn member_sort_both_limits_reject_before_first_insert_and_roll_back_whole_group() -> Result<()> {
    for records_limit in [false, true] {
        for phase in ["upgrade", "first_import"] {
            let mut connection = connection()?;
            connection.execute_batch(
                "CREATE TRIGGER reject_sorted_member BEFORE INSERT ON episode_continuity_members
                 BEGIN SELECT RAISE(ABORT, 'sort SQL must not run'); END;",
            )?;
            let rows = vec![member("b", 0, false)?, member("a", 0, false)?];
            let start = initial();
            let exact = (
                start.0 + 2 * std::mem::size_of::<usize>() as u64 * 8,
                start.1 + 2 * 8,
            );
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let material = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(!records_limit),
                exact.1 - usize::from(records_limit),
            )?;
            let holder = material.guard()?;
            {
                let _entry = material.enter();
                charge_owned_metadata(31, 3)?;
                let tx = connection.transaction()?;
                tx.execute("INSERT INTO sort_markers VALUES ('rollback')", [])?;
                let error =
                    capture_members_tx(&tx, SEED, phase, &EpisodeContinuityRoots::default(), rows)
                        .expect_err("sort allocation exceeded the caller's remainder");
                assert_eq!(
                    error.downcast_ref::<std::io::Error>().unwrap().kind(),
                    std::io::ErrorKind::Other
                );
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(stored(&tx)?.is_empty());
                assert!(material.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                tx.rollback()?;
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(stored(&connection)?.is_empty());
            assert_eq!(
                connection.query_row("SELECT COUNT(*) FROM sort_markers", [], |row| row
                    .get::<_, u64>(0))?,
                0
            );
            let last = holder.clone();
            drop((material, holder));
            assert_eq!(ledger.report()?.observed_bytes, start.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn member_sort_sql_abort_and_stable_duplicate_preserve_phase_and_release_on_rollback() -> Result<()>
{
    for duplicate in [false, true] {
        for phase in ["upgrade", "first_import"] {
            let mut connection = connection()?;
            if !duplicate {
                connection.execute_batch(
                    "CREATE TRIGGER reject_sorted_member BEFORE INSERT ON episode_continuity_members
                     WHEN NEW.episode_id = 'b'
                     BEGIN SELECT RAISE(ABORT, 'sorted member SQL failure'); END;",
                )?;
            }
            let mut first = member("a", 0, false)?;
            first.payload = "first:\0界".into();
            let rows = vec![
                first,
                member("0", 0, true)?,
                member(if duplicate { "a" } else { "b" }, 0, false)?,
            ];
            let start = initial();
            let exact = (
                start.0 + 3 * std::mem::size_of::<usize>() as u64 * 8,
                start.1 + 3 * 8,
            );
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let material =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
            let holder = material.guard()?;
            {
                let _entry = material.enter();
                charge_owned_metadata(31, 3)?;
                let tx = connection.transaction()?;
                tx.execute("INSERT INTO sort_markers VALUES ('rollback')", [])?;
                let error =
                    capture_members_tx(&tx, SEED, phase, &EpisodeContinuityRoots::default(), rows)
                        .expect_err("sorted SQL failure unexpectedly succeeded");
                assert_eq!(
                    error
                        .downcast_ref::<rusqlite::Error>()
                        .unwrap()
                        .sqlite_error_code(),
                    Some(rusqlite::ErrorCode::ConstraintViolation)
                );
                if !duplicate {
                    assert!(format!("{error:#}").contains("sorted member SQL failure"));
                }
                assert_eq!(
                    stored(&tx)?,
                    vec![(
                        phase.to_owned(),
                        "a".to_owned(),
                        format!("{:x}", Sha256::digest("first:\0界".as_bytes()))
                    )]
                );
                assert_eq!(material.finish()?, exact);
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                tx.execute_batch("DROP TABLE episode_continuity_members;")?;
                tx.rollback()?;
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(stored(&connection)?.is_empty());
            assert_eq!(
                connection.query_row("SELECT COUNT(*) FROM sort_markers", [], |row| row
                    .get::<_, u64>(0))?,
                0
            );
            let last = holder.clone();
            drop((material, holder));
            assert_eq!(ledger.report()?.observed_bytes, exact.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}
