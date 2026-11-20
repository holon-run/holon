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

const AGENT: &str = "member-digest";
const SEED: &str = "digest-seed";
type StoredMember = (String, String, String, String);

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE digest_markers (value TEXT);")?;
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
        payload: payload.to_owned(),
        receipted,
    })
}

fn stored(connection: &Connection) -> Result<Vec<StoredMember>> {
    let mut statement = connection.prepare(
        "SELECT capture_phase, episode_id, content_digest, typeof(content_digest)
         FROM episode_continuity_members ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[test]
fn member_digest_fixed_hex_matches_original_bytes_and_borrows_sql_text() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    assert_eq!(
        &member_content_digest(b""),
        b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        &member_content_digest(b"abc"),
        b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let binary: Vec<u8> = (0..=255).collect();
    let long = "  {\n \"escaped\": \"\\u754c\", \"x\": 1 }\0\\\"\n".repeat(2048);
    for payload in [
        b"".as_slice(),
        b"abc",
        b"\0\"\\\r\n",
        "界é🦀".as_bytes(),
        binary.as_slice(),
        long.as_bytes(),
    ] {
        let hex = member_content_digest(payload);
        let text = ToSqlOutput::Borrowed(ValueRef::Text(&hex));
        let ToSqlOutput::Borrowed(ValueRef::Text(bound)) = text.to_sql()? else {
            panic!("member digest must bind borrowed SQL TEXT");
        };
        assert_eq!(bound.as_ptr(), hex.as_ptr());
        assert_eq!(bound.len(), 64);
        let expected = format!("{:x}", Sha256::digest(payload));
        assert_eq!(bound, expected.as_bytes());
        let result: (String, String) =
            connection.query_row("SELECT typeof(?1), ?1", [text], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!(result, ("text".to_owned(), expected));
    }
    Ok(())
}

#[test]
fn member_digest_capture_keeps_exhausted_original_phase_tls_and_last_holder() -> Result<()> {
    let mut connection = connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let initial = ((AGENT.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let sort_bytes = 3 * std::mem::size_of::<usize>() as u64;
    let exact = (initial.0 + 2 * sort_bytes * 8, initial.1 + 2 * 3 * 8);
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = material.guard()?;
    let mut expected = Vec::new();
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        assert_eq!(material.finish()?, initial);
        let mut used = initial;
        for phase in ["upgrade", "first_import"] {
            let tx = connection.transaction()?;
            capture_members_tx(
                &tx,
                SEED,
                phase,
                &EpisodeContinuityRoots::default(),
                vec![
                    member("b", "  {\n \"x\":1, \"s\":\"\\u754c\" }\0", false)?,
                    member("c", "receipted", true)?,
                    member("a", "界é🦀\0\"\\\n", false)?,
                ],
            )?;
            for (id, payload) in [
                ("a", "界é🦀\0\"\\\n"),
                ("b", "  {\n \"x\":1, \"s\":\"\\u754c\" }\0"),
            ] {
                expected.push((
                    phase.to_owned(),
                    id.to_owned(),
                    format!("{:x}", Sha256::digest(payload.as_bytes())),
                    "text".to_owned(),
                ));
            }
            assert_eq!(stored(&tx)?, expected);
            used.0 += sort_bytes * 8;
            used.1 += 3 * 8;
            assert_eq!(material.finish()?, used);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            tx.commit()?;
        }
        assert_eq!(used, exact);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((material, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    assert_eq!(stored(&connection)?, expected);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn sql_failure_rolls_back(duplicate: bool) -> Result<()> {
    let mut connection = connection()?;
    if !duplicate {
        connection.execute_batch(
            "CREATE TRIGGER reject_digest_member BEFORE INSERT ON episode_continuity_members
             WHEN NEW.episode_id = 'b'
             BEGIN SELECT RAISE(ABORT, 'digest SQL failure'); END;",
        )?;
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let exact = (
        (AGENT.len() as u64 + 256 + 31 + 2 * std::mem::size_of::<usize>() as u64) * 8,
        64 + (3 + 2) * 8,
    );
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = material.guard()?;
    {
        let _entry = material.enter();
        charge_owned_metadata(31, 3)?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO digest_markers VALUES ('rollback')", [])?;
        let error = capture_members_tx(
            &tx,
            SEED,
            "upgrade",
            &EpisodeContinuityRoots::default(),
            vec![
                member("a", "original:\0界", false)?,
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
            assert!(format!("{error:#}").contains("digest SQL failure"));
        }
        assert_eq!(
            stored(&tx)?,
            vec![(
                "upgrade".to_owned(),
                "a".to_owned(),
                format!("{:x}", Sha256::digest("original:\0界".as_bytes())),
                "text".to_owned(),
            )]
        );
        assert_eq!(material.finish()?, exact);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        // The synchronous bind/step has released all statement borrows.
        tx.execute_batch("DROP TABLE episode_continuity_members;")?;
        tx.rollback()?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(stored(&connection)?.is_empty());
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM digest_markers", [], |row| {
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
fn member_digest_sql_abort_releases_borrows_and_rolls_back_whole_transaction() -> Result<()> {
    sql_failure_rolls_back(false)
}

#[test]
fn member_digest_duplicate_releases_borrows_and_rolls_back_whole_transaction() -> Result<()> {
    sql_failure_rolls_back(true)
}
