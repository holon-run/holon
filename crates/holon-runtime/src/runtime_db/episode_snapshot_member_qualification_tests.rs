use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use std::sync::Arc;

const AGENT: &str = "member-qualification";
const SEED: &str = "qualification-seed";
const REASONS: [&str; 3] = [
    "explicit_live_ref",
    "explicit_upgrade_recent_candidate",
    "live_owner",
];
type StoredMember = (String, String, String, String);

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON; CREATE TABLE qualification_markers (value TEXT);",
    )?;
    connection.execute(
        "INSERT INTO episode_continuity_seeds
         (seed_id, agent_id, policy_version, boundary_id, source_kind, roots_json,
          candidate_payloads_json, import_required, captured_at)
         VALUES (?1, ?2, 1, 'boundary', 'upgrade_unbound', '{}', '{}', 0, 'original')",
        params![SEED, AGENT],
    )?;
    Ok(connection)
}

fn member(id: &str, live_owner: bool, receipted: bool) -> Result<EpisodeRow> {
    Ok(EpisodeRow {
        record: serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": AGENT,
            "created_at": "2026-10-18T00:00:00Z",
            "finalized_at": "2026-10-18T00:00:00Z",
            "start_turn_index": 1, "end_turn_index": 1,
            "start_message_count": 0, "end_message_count": 1,
            "boundary_reason": "wait_boundary",
            "current_work_item_id": if live_owner { "live-work" } else { "other-work" },
        }))?,
        payload: format!("payload:{id}:\0\"\\界"),
        receipted,
    })
}

fn stored(connection: &Connection) -> Result<Vec<StoredMember>> {
    let mut statement = connection.prepare(
        "SELECT capture_phase, episode_id, qualification_json, typeof(qualification_json)
         FROM episode_continuity_members ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn original(
    rows: &[EpisodeRow],
    roots: &EpisodeContinuityRoots,
    phase: &str,
) -> Result<Vec<StoredMember>> {
    let mut rows: Vec<_> = rows.iter().collect();
    rows.sort_by(|left, right| {
        right
            .record
            .finalized_at
            .cmp(&left.record.finalized_at)
            .then_with(|| right.record.created_at.cmp(&left.record.created_at))
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    let recent: BTreeSet<_> = rows
        .iter()
        .filter(|row| !row.receipted)
        .take(12)
        .map(|row| row.record.id.clone())
        .collect();
    let mut expected = Vec::new();
    for row in rows.iter().filter(|row| !row.receipted) {
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
                phase.to_owned(),
                row.record.id.clone(),
                serde_json::to_string(&reasons)?,
                "text".to_owned(),
            ));
        }
    }
    Ok(expected)
}

#[test]
fn member_qualification_fixed_json_matches_all_combinations_and_borrows_text() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    for mask in 0..8 {
        let reasons: Vec<_> = REASONS
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, reason)| *reason)
            .collect();
        let expected = serde_json::to_string(&reasons)?;
        let json = member_qualification_json(&reasons)?;
        let ToSqlOutput::Borrowed(ValueRef::Text(bound)) = json.to_sql()? else {
            panic!("qualification JSON must bind borrowed SQL TEXT");
        };
        assert_eq!(bound.as_ptr(), json.bytes.as_ptr());
        assert_eq!(bound, expected.as_bytes());
        assert_eq!(json.len, expected.len());
        if mask == 7 {
            assert_eq!(json.len, MEMBER_QUALIFICATION_JSON_MAX_BYTES);
        }
        let result: (String, String) =
            connection.query_row("SELECT typeof(?1), ?1", [&json], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!(result, ("text".to_owned(), expected));
    }
    // A future longer reason must fail rather than truncate or grow the buffer.
    let oversized = "x".repeat(MEMBER_QUALIFICATION_JSON_MAX_BYTES);
    let error = member_qualification_json(&[&oversized])
        .err()
        .expect("bounded qualification encoder unexpectedly grew");
    assert!(error.downcast_ref::<serde_json::Error>().unwrap().is_io());
    Ok(())
}

#[test]
fn member_qualification_capture_preserves_original_phase_tls_and_last_holder() -> Result<()> {
    let mut connection = connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let initial = ((AGENT.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let ref_bytes = (12 * (8 + "filler-00".len()) + 8 + "a-target:\0\"\\界".len()) as u64;
    let sort_bytes = 14 * std::mem::size_of::<usize>() as u64;
    let exact = (
        initial.0 + 16 * (ref_bytes + sort_bytes) * 8,
        initial.1 + 16 * (13 + 14) * 8,
    );
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = material.guard()?;
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        assert_eq!(material.finish()?, initial);
        let mut used = initial;
        for phase in ["upgrade", "first_import"] {
            for mask in 0..8 {
                let id = if mask & 2 != 0 {
                    "a-target:\0\"\\界"
                } else {
                    "z-target:\0\"\\界"
                };
                let mut rows = (0..12)
                    .map(|index| member(&format!("filler-{index:02}"), false, false))
                    .collect::<Result<Vec<_>>>()?;
                rows.push(member(id, mask & 4 != 0, false)?);
                rows.push(member("receipted", true, true)?);
                let mut roots = EpisodeContinuityRoots {
                    work_item_ids: ["live-work".into()].into(),
                    ..EpisodeContinuityRoots::default()
                };
                if mask & 1 != 0 {
                    roots.source_refs.insert(format!("episode:{id}"));
                }
                roots.source_refs.insert("episode:receipted".into());
                let expected = original(&rows, &roots, phase)?;
                let tx = connection.transaction()?;
                capture_members_tx(&tx, SEED, phase, &roots, rows)?;
                assert_eq!(stored(&tx)?, expected);
                used.0 += (ref_bytes + sort_bytes) * 8;
                used.1 += (13 + 14) * 8;
                assert_eq!(material.finish()?, used);
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                tx.rollback()?;
            }
        }
        assert_eq!(used, exact);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(stored(&connection)?.is_empty());
    let last = holder.clone();
    drop((material, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn sql_failure_rolls_back(duplicate: bool) -> Result<()> {
    let mut connection = connection()?;
    if !duplicate {
        connection.execute_batch(
            "CREATE TRIGGER reject_qualification BEFORE INSERT ON episode_continuity_members
             WHEN NEW.episode_id = 'b'
             BEGIN SELECT RAISE(ABORT, 'qualification SQL failure'); END;",
        )?;
    }
    let roots = EpisodeContinuityRoots {
        work_item_ids: ["live-work".into()].into(),
        source_refs: ["episode:a".into(), "episode:b".into()].into(),
        ..EpisodeContinuityRoots::default()
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let exact = (
        (AGENT.len() as u64 + 256 + 31 + 2 * 9 + 2 * std::mem::size_of::<usize>() as u64) * 8,
        64 + (3 + 2 + 2) * 8,
    );
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = material.guard()?;
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO qualification_markers VALUES ('rollback')", [])?;
        let error = capture_members_tx(
            &tx,
            SEED,
            "upgrade",
            &roots,
            vec![
                member("a", true, false)?,
                member(if duplicate { "a" } else { "b" }, true, false)?,
            ],
        )
        .expect_err("member SQL failure unexpectedly succeeded");
        assert_eq!(
            error
                .downcast_ref::<rusqlite::Error>()
                .unwrap()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
        if !duplicate {
            assert!(format!("{error:#}").contains("qualification SQL failure"));
        }
        assert_eq!(
            stored(&tx)?,
            vec![(
                "upgrade".to_owned(),
                "a".to_owned(),
                serde_json::to_string(&REASONS)?,
                "text".to_owned(),
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
        connection.query_row("SELECT COUNT(*) FROM qualification_markers", [], |row| {
            row.get::<_, u64>(0)
        })?,
        0
    );
    let last = holder.clone();
    drop((material, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn member_qualification_sql_abort_releases_borrows_and_rolls_back_whole_group() -> Result<()> {
    sql_failure_rolls_back(false)
}

#[test]
fn member_qualification_duplicate_releases_borrows_and_rolls_back_whole_group() -> Result<()> {
    sql_failure_rolls_back(true)
}
