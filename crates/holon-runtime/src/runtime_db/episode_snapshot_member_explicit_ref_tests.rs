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

const AGENT: &str = "member-explicit-ref";
const SEED: &str = "explicit-ref-seed";

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON; CREATE TABLE explicit_ref_markers (value TEXT);",
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

fn member(id: &str, payload: &str, receipted: bool) -> Result<EpisodeRow> {
    Ok(EpisodeRow {
        record: serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": AGENT,
            "created_at": "2026-10-18T00:00:00Z",
            "finalized_at": "2026-10-18T00:00:00Z",
            "start_turn_index": 1, "end_turn_index": 1,
            "start_message_count": 0, "end_message_count": 1,
            "boundary_reason": "wait_boundary",
        }))?,
        payload: payload.into(),
        receipted,
    })
}

fn stored(connection: &Connection) -> Result<Vec<(String, String, String, String)>> {
    let mut statement = connection.prepare(
        "SELECT capture_phase, episode_id, content_digest, qualification_json
         FROM episode_continuity_members ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
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
fn explicit_ref_matches_original_utf8_bytes_and_reuses_caller_until_last_holder() -> Result<()> {
    let long_id = "界é:\0\"\\".repeat(1024);
    let ids = ["", "a", "a\0", "episode:a", "界é:\0\"\\", &long_id];
    let roots = EpisodeContinuityRoots {
        source_refs: [
            "episode:".to_owned(),
            "episode:a\0".to_owned(),
            "episode:界é:\0\"\\".to_owned(),
            format!("episode:{long_id}"),
            "Episode:a".to_owned(),
            "turn:a".to_owned(),
        ]
        .into(),
        ..EpisodeContinuityRoots::default()
    };
    let total_bytes: u64 = ids.iter().map(|id| 8 + id.len() as u64).sum();
    let start = initial();
    let exact = (start.0 + total_bytes * 8, start.1 + ids.len() * 8);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = material.guard()?;
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        let mut used = start;
        assert_eq!(material.finish()?, used);
        for id in ids {
            let expected = roots.source_refs.contains(&format!("episode:{id}"));
            assert_eq!(member_has_explicit_ref(&roots, id)?, expected);
            used.0 += (8 + id.len() as u64) * 8;
            used.1 += 8;
            assert_eq!(material.finish()?, used);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert_eq!(used, exact);
        // Empty roots need neither a lookup key nor any remaining capacity.
        assert!(!member_has_explicit_ref(
            &EpisodeContinuityRoots::default(),
            &long_id
        )?);
        assert_eq!(material.finish()?, exact);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((material, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // No phase is invented for existing standalone callers.
    assert!(member_has_explicit_ref(&roots, &long_id)?);
    Ok(())
}

#[test]
fn explicit_ref_both_limits_reject_before_next_sql_and_roll_back_whole_group() -> Result<()> {
    for records_limit in [false, true] {
        for phase in ["upgrade", "first_import"] {
            let mut connection = connection()?;
            connection.execute_batch(
                "CREATE TRIGGER reject_second_ref BEFORE INSERT ON episode_continuity_members
                 WHEN NEW.episode_id = 'b'
                 BEGIN SELECT RAISE(ABORT, 'second SQL must not run'); END;",
            )?;
            let roots = EpisodeContinuityRoots {
                source_refs: ["episode:a".into(), "episode:b".into()].into(),
                ..EpisodeContinuityRoots::default()
            };
            let start = initial();
            let sort_bytes = 3 * std::mem::size_of::<usize>() as u64;
            let exact = (start.0 + (18 + sort_bytes) * 8, start.1 + (2 + 3) * 8);
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
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                let tx = connection.transaction()?;
                tx.execute("INSERT INTO explicit_ref_markers VALUES ('rollback')", [])?;
                let error = capture_members_tx(
                    &tx,
                    SEED,
                    phase,
                    &roots,
                    vec![
                        member("b", "second", false)?,
                        member("c", "receipted", true)?,
                        member("a", "first", false)?,
                    ],
                )
                .expect_err("second key unexpectedly exceeded the original phase");
                assert_eq!(
                    error.downcast_ref::<std::io::Error>().unwrap().kind(),
                    std::io::ErrorKind::Other
                );
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert_eq!(stored(&tx)?.len(), 1);
                assert_eq!(stored(&tx)?[0].1, "a");
                assert!(material.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                tx.execute_batch("DROP TABLE episode_continuity_members;")?;
                tx.rollback()?;
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(stored(&connection)?.is_empty());
            assert_eq!(
                connection.query_row("SELECT COUNT(*) FROM explicit_ref_markers", [], |row| row
                    .get::<_, u64>(0))?,
                0
            );
            let last = holder.clone();
            drop((material, holder));
            assert_eq!(
                ledger.report()?.observed_bytes,
                start.0 + (9 + sort_bytes) * 8
            );
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

fn sql_failure_rolls_back(duplicate: bool) -> Result<()> {
    let mut connection = connection()?;
    if !duplicate {
        connection.execute_batch(
            "CREATE TRIGGER reject_explicit_ref BEFORE INSERT ON episode_continuity_members
             WHEN NEW.episode_id = 'b'
             BEGIN SELECT RAISE(ABORT, 'explicit ref SQL failure'); END;",
        )?;
    }
    let roots = EpisodeContinuityRoots {
        source_refs: ["episode:a".into(), "episode:b".into()].into(),
        ..EpisodeContinuityRoots::default()
    };
    let start = initial();
    let sort_bytes = 2 * std::mem::size_of::<usize>() as u64;
    let exact = (start.0 + (18 + sort_bytes) * 8, start.1 + (2 + 2) * 8);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = material.guard()?;
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO explicit_ref_markers VALUES ('rollback')", [])?;
        let error = capture_members_tx(
            &tx,
            SEED,
            "upgrade",
            &roots,
            vec![
                member("a", "first:\0界", false)?,
                member(if duplicate { "a" } else { "b" }, "second", false)?,
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
            assert!(format!("{error:#}").contains("explicit ref SQL failure"));
        }
        assert_eq!(
            stored(&tx)?,
            vec![(
                "upgrade".to_owned(),
                "a".to_owned(),
                format!("{:x}", Sha256::digest("first:\0界".as_bytes())),
                "[\"explicit_live_ref\",\"explicit_upgrade_recent_candidate\"]".to_owned(),
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
        connection.query_row("SELECT COUNT(*) FROM explicit_ref_markers", [], |row| {
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
fn explicit_ref_sql_abort_keeps_original_phase_and_releases_group_on_rollback() -> Result<()> {
    sql_failure_rolls_back(false)
}

#[test]
fn explicit_ref_duplicate_keeps_original_phase_and_releases_group_on_rollback() -> Result<()> {
    sql_failure_rolls_back(true)
}
