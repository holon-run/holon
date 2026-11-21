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

const AGENT: &str = "member-reasons";
const SEED: &str = "reason-seed";
type StoredMember = (String, String, String, String);

fn rows(count: usize, receipted: bool) -> Result<Vec<EpisodeRow>> {
    (0..count)
        .map(|index| {
            let record = serde_json::from_value(serde_json::json!({
                "id": format!("episode-{index:03}:\0\"\\界"),
                "agent_id": AGENT,
                "created_at": "2026-10-18T00:00:00Z",
                "finalized_at": "2026-10-18T00:00:00Z",
                "start_turn_index": 1, "end_turn_index": 1,
                "start_message_count": 0, "end_message_count": 1,
                "boundary_reason": "wait_boundary",
                "current_work_item_id": if index & 1 != 0 { "live-work" } else { "other-work" },
            }))?;
            Ok(EpisodeRow {
                record,
                payload: format!("payload:{index}:\0\"\\界"),
                receipted: receipted || matches!(index, 8 | 9 | 24 | 25),
            })
        })
        .collect()
}

fn roots() -> Result<EpisodeContinuityRoots> {
    Ok(EpisodeContinuityRoots {
        work_item_ids: ["live-work".into()].into(),
        source_refs: rows(32, false)?
            .iter()
            .enumerate()
            .filter(|(index, _)| index & 2 != 0)
            .map(|(_, row)| format!("episode:{}", row.record.id))
            .collect(),
        ..EpisodeContinuityRoots::default()
    })
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE reason_markers (value TEXT);")?;
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
        "SELECT capture_phase, episode_id, content_digest, qualification_json
         FROM episode_continuity_members ORDER BY rowid",
    )?;
    let result = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(result)
}

// Original owning set algorithm, independent of the fixed reason storage.
fn original(
    mut rows: Vec<EpisodeRow>,
    roots: &EpisodeContinuityRoots,
    phase: &str,
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
    let mut result = Vec::new();
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
            result.push((
                phase.to_owned(),
                row.record.id,
                format!("{:x}", Sha256::digest(row.payload.as_bytes())),
                serde_json::to_string(&reasons)?,
            ));
        }
    }
    Ok(result)
}

#[test]
fn reason_slices_preserve_all_combinations_empty_skips_receipts_and_sql_order() -> Result<()> {
    let roots = roots()?;
    let expected = original(rows(32, false)?, &roots, "upgrade")?;
    let combinations: BTreeSet<_> = expected.iter().map(|row| row.3.as_str()).collect();
    let mut original_combinations = BTreeSet::new();
    for mask in 1..8 {
        let mut reasons = BTreeSet::new();
        if mask & 1 != 0 {
            reasons.insert("live_owner");
        }
        if mask & 2 != 0 {
            reasons.insert("explicit_live_ref");
        }
        if mask & 4 != 0 {
            reasons.insert("explicit_upgrade_recent_candidate");
        }
        original_combinations.insert(serde_json::to_string(&reasons)?);
    }
    assert_eq!(
        combinations,
        original_combinations.iter().map(String::as_str).collect()
    );
    // The eighth (empty) combination must not reach INSERT.
    assert!(!expected.iter().any(|row| row.1.starts_with("episode-020:")));
    for count in [0, 4, 12, 32] {
        for receipted in [false, true] {
            for phase in ["upgrade", "first_import"] {
                let mut connection = connection()?;
                let tx = connection.transaction()?;
                let mut input = rows(count, receipted)?;
                input.reverse();
                capture_members_tx(&tx, SEED, phase, &roots, input)?;
                assert_eq!(
                    stored(&tx)?,
                    original(rows(count, receipted)?, &roots, phase)?
                );
                tx.rollback()?;
            }
        }
    }
    Ok(())
}

#[test]
fn reason_storage_needs_no_new_budget_and_preserves_original_phase_and_last_holder() -> Result<()> {
    let roots = roots()?;
    let input = rows(32, false)?;
    let expected = original(rows(32, false)?, &roots, "upgrade")?;
    let mut connection = connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let initial = ((AGENT.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let ref_bytes: u64 = input
        .iter()
        .filter(|row| !row.receipted)
        .map(|row| 8 + row.record.id.len() as u64)
        .sum();
    let ref_count = input.iter().filter(|row| !row.receipted).count();
    let sort_bytes = input.len() as u64 * std::mem::size_of::<usize>() as u64;
    let exact = (
        initial.0 + (ref_bytes + sort_bytes) * 8,
        initial.1 + (ref_count + input.len()) * 8,
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        assert_eq!(phase.finish()?, initial);
        let tx = connection.transaction()?;
        capture_members_tx(&tx, SEED, "upgrade", &roots, input)?;
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

fn sql_failure_rolls_back(duplicate: bool) -> Result<()> {
    let roots = roots()?;
    let mut input = rows(32, false)?;
    if duplicate {
        input = vec![rows(4, false)?.remove(3), rows(4, false)?.remove(3)];
    }
    let expected = original(
        if duplicate {
            vec![rows(4, false)?.remove(3)]
        } else {
            rows(32, false)?
        },
        &roots,
        "first_import",
    )?;
    let mut connection = connection()?;
    if !duplicate {
        connection.execute_batch(
            "CREATE TRIGGER reject_reason_member BEFORE INSERT ON episode_continuity_members
             WHEN (SELECT COUNT(*) FROM episode_continuity_members) > 0
             BEGIN SELECT RAISE(ABORT, 'reason SQL failure'); END;",
        )?;
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    // The first two input rows are also the first two selected SQL attempts.
    let ref_bytes: u64 = input
        .iter()
        .take(2)
        .map(|row| 8 + row.record.id.len() as u64)
        .sum();
    let exact = (
        (AGENT.len() as u64
            + 256
            + 31
            + ref_bytes
            + input.len() as u64 * std::mem::size_of::<usize>() as u64)
            * 8,
        64 + (3 + 2 + input.len()) * 8,
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO reason_markers VALUES ('rollback')", [])?;
        let error = capture_members_tx(&tx, SEED, "first_import", &roots, input)
            .expect_err("member SQL failure unexpectedly succeeded");
        assert_eq!(
            error
                .downcast_ref::<rusqlite::Error>()
                .unwrap()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
        if !duplicate {
            assert!(format!("{error:#}").contains("reason SQL failure"));
        }
        assert_eq!(stored(&tx)?, expected[..1]);
        assert_eq!(phase.finish()?, exact);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        // No local reason slice or SQL statement escapes the failed capture.
        tx.execute_batch("DROP TABLE episode_continuity_members;")?;
        tx.rollback()?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(stored(&connection)?.is_empty());
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM reason_markers", [], |row| {
            row.get::<_, u64>(0)
        })?,
        0
    );
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn reason_capture_sql_abort_releases_locals_and_rolls_back_whole_transaction() -> Result<()> {
    sql_failure_rolls_back(false)
}

#[test]
fn reason_capture_duplicate_constraint_releases_locals_and_rolls_back_whole_transaction(
) -> Result<()> {
    sql_failure_rolls_back(true)
}
