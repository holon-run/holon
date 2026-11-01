use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use std::sync::Arc;

#[path = "episode_snapshot_seed_candidate_node_tests.rs"]
mod node_tests;

#[path = "episode_snapshot_seed_candidate_json_tests.rs"]
mod json_tests;

#[path = "episode_snapshot_seed_roots_json_tests.rs"]
mod roots_json_tests;

const AGENT: &str = "seed-candidates";

pub(super) fn seed_text_cost(agent: &str) -> (u64, usize) {
    // Two owned strings: the fixed prefix/hex ID and the agent ID copy.
    ((13 + 64 + agent.len() as u64) * 8, 2 * 8)
}

pub(super) fn seed_insert_cost(agent: &str, records: &[EpisodeRow]) -> Result<(u64, usize)> {
    let json = seed_insert_json_cost(records)?;
    let text = seed_text_cost(agent);
    Ok((json.0 + text.0, json.1 + text.1))
}

pub(super) fn seed_insert_json_cost(records: &[EpisodeRow]) -> Result<(u64, usize)> {
    let candidates = candidate_json_cost(records)?;
    let roots = roots_json_tests::json_cost(&EpisodeContinuityRoots::default())?;
    Ok((candidates.0 + roots.0, candidates.1 + roots.1))
}

pub(super) fn candidate_json_cost(records: &[EpisodeRow]) -> Result<(u64, usize)> {
    let json = original_json(records)?;
    let original: BTreeMap<String, String> = serde_json::from_str(&json)?;
    // Braces, opening key/value quotes, colons and separators, not escaped key text.
    let slots = if original.is_empty() {
        2
    } else {
        original.len() * 4 + 1
    };
    Ok((
        (json.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    ))
}

fn node_cost(records: &[EpisodeRow]) -> Result<(u64, usize)> {
    let original: BTreeMap<String, String> = serde_json::from_str(&original_json(records)?)?;
    Ok((original.len() as u64 * 256 * 8, original.len() * 8))
}

fn rows() -> Result<Vec<EpisodeRow>> {
    [
        ("z", "original", false),
        ("a", "first", false),
        ("a", "ignored receipt", true),
        ("a", "replacement", false),
        ("\"\\\0\n界İß\u{2028}", "escaped\0payload", false),
        ("receipted-only", "ignored", true),
        ("", "empty ID", false),
        ("long-key", "long payload", false),
    ]
    .into_iter()
    .map(|(id, payload, receipted)| {
        let id = if id == "long-key" {
            "界\0\"\\".repeat(16_384)
        } else {
            id.into()
        };
        let record = serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": AGENT,
            "created_at": "2026-10-18T00:00:00Z",
            "finalized_at": "2026-10-18T00:00:01Z",
            "start_turn_index": 1, "end_turn_index": 1,
            "start_message_count": 0, "end_message_count": 1,
            "boundary_reason": "wait_boundary", "current_work_item_id": null,
        }))?;
        Ok(EpisodeRow {
            record,
            payload: payload.into(),
            receipted,
        })
    })
    .collect()
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE candidate_markers (value TEXT);",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES (?1, 'candidate-boundary')",
        [UPGRADE_BOUNDARY_KEY],
    )?;
    Ok(connection)
}

fn insert(tx: &Transaction<'_>, records: &[EpisodeRow]) -> Result<ContinuitySeed> {
    insert_seed_with_candidates_tx(
        tx,
        AGENT,
        None,
        "upgrade_unbound",
        &EpisodeContinuityRoots::default(),
        false,
        "2026-10-18T00:00:00Z".parse()?,
        records,
    )
}

fn original_json(records: &[EpisodeRow]) -> Result<String> {
    let owning: BTreeMap<String, String> = records
        .iter()
        .filter(|row| !row.receipted)
        .map(|row| {
            (
                row.record.id.clone(),
                format!("{:x}", Sha256::digest(row.payload.as_bytes())),
            )
        })
        .collect();
    Ok(serde_json::to_string(&owning)?)
}

fn stored_json(connection: &Connection) -> Result<String> {
    Ok(connection.query_row(
        "SELECT candidate_payloads_json FROM episode_continuity_seeds",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn fixed_candidate_digests_preserve_sha256_vectors_and_json_hex_bytes() -> Result<()> {
    assert_eq!(std::mem::size_of::<CandidatePayloadDigest>(), 32);
    for (payload, hex) in [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
    ] {
        let digest = CandidatePayloadDigest(Sha256::digest(payload));
        assert_eq!(serde_json::to_string(&digest)?, format!("\"{hex}\""));
    }
    for payload in [
        (0..=255).collect::<Vec<u8>>(),
        "\"\\\0\n界İß\u{2028}".as_bytes().to_vec(),
        b"long payload".repeat(16_384),
    ] {
        let original = format!("{:x}", Sha256::digest(&payload));
        let digest = CandidatePayloadDigest(Sha256::digest(&payload));
        assert_eq!(
            serde_json::to_string(&digest)?,
            serde_json::to_string(&original)?
        );
    }
    Ok(())
}

#[test]
fn fixed_candidate_digest_streams_to_stack_writer_and_propagates_each_io_failure() -> Result<()> {
    struct Writer {
        bytes: [u8; 66],
        written: usize,
        limit: usize,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let count = bytes.len().min(self.limit - self.written);
            if count == 0 && !bytes.is_empty() {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            self.bytes[self.written..self.written + count].copy_from_slice(&bytes[..count]);
            self.written += count;
            Ok(count)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let payload = b"fixed candidate digest";
    let expected = serde_json::to_vec(&format!("{:x}", Sha256::digest(payload)))?;
    let digest = CandidatePayloadDigest(Sha256::digest(payload));
    for limit in 0..=66 {
        let mut writer = Writer {
            bytes: [0; 66],
            written: 0,
            limit,
        };
        let result = serde_json::to_writer(&mut writer, &digest);
        if limit == 66 {
            result?;
        } else {
            assert!(result.unwrap_err().is_io(), "limit={limit}");
        }
        assert_eq!(writer.written, limit);
        assert_eq!(&writer.bytes[..limit], &expected[..limit]);
    }
    Ok(())
}

#[test]
fn fixed_candidate_digests_preserve_actual_seed_sql_payload_json() -> Result<()> {
    let long = "界\0\"\\".repeat(16_384);
    for payload in ["", "abc", "\0\n界İß\u{2028}", long.as_str()] {
        let mut records = rows()?;
        for (index, row) in records.iter_mut().enumerate() {
            row.payload = if index == 0 {
                payload.to_owned()
            } else {
                format!("{payload}{index}")
            };
        }
        let expected = original_json(&records)?;
        let mut connection = connection()?;
        let tx = connection.transaction()?;
        insert(&tx, &records)?;
        assert_eq!(stored_json(&tx)?, expected);
        tx.commit()?;
        assert_eq!(stored_json(&connection)?, expected);
    }
    Ok(())
}

#[test]
fn fixed_candidate_digests_keep_original_exhausted_phase_through_sql_and_rollback() -> Result<()> {
    let records = rows()?;
    let expected = original_json(&records)?;
    let mut connection = connection()?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let mut exact = ((scope.agent_id.len() as u64 + 256 + 23) * 8, 64 + 8);
    let nodes = node_cost(&records)?;
    let json = seed_insert_cost(AGENT, &records)?;
    let per_call = (nodes.0 + json.0, nodes.1 + json.1);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        exact.0 + 2 * per_call.0,
        exact.1 + 2 * per_call.1,
    )?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(23, 1)?;
        for fail in [false, true] {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'digest SQL failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            tx.execute(
                "INSERT INTO candidate_markers VALUES ('digest rollback')",
                [],
            )?;
            let result = insert(&tx, &records);
            if fail {
                let error = result.err().context("seed unexpectedly inserted")?;
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
                assert!(format!("{error:#}").contains("digest SQL failure"));
            } else {
                result?;
                assert_eq!(stored_json(&tx)?, expected);
            }
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
            tx.rollback()?;
            for table in ["candidate_markers", "episode_continuity_seeds"] {
                let count: u64 =
                    connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 0);
            }
            exact.0 += per_call.0;
            exact.1 += per_call.1;
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            assert_eq!(original_json(&records)?, expected);
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn borrowed_candidate_keys_preserve_owning_map_bytes_filter_order_and_duplicates() -> Result<()> {
    let mut records = rows()?;
    for mode in 0..3 {
        if mode == 1 {
            for row in &mut records {
                row.receipted = true;
            }
        } else if mode == 2 {
            records.clear();
        }
        let expected = original_json(&records)?;
        let mut connection = connection()?;
        let tx = connection.transaction()?;
        insert(&tx, &records)?;
        let stored = stored_json(&tx)?;
        assert_eq!(stored, expected);
        let parsed: BTreeMap<String, String> = serde_json::from_str(&stored)?;
        if mode == 0 {
            assert_eq!(parsed.len(), 5);
            assert_eq!(parsed["a"], format!("{:x}", Sha256::digest(b"replacement")));
            assert!(!parsed.contains_key("receipted-only"));
            assert!(stored.starts_with("{\"\":"));
        } else {
            assert_eq!(stored, "{}");
        }
        tx.commit()?;
    }
    Ok(())
}

#[test]
fn borrowed_candidate_keys_keep_exhausted_caller_phase_tls_and_last_holder() -> Result<()> {
    let records = rows()?;
    let expected = original_json(&records)?;
    let mut connection = connection()?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let mut exact = ((scope.agent_id.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let nodes = node_cost(&records)?;
    let json = seed_insert_cost(AGENT, &records)?;
    let per_call = (nodes.0 + json.0, nodes.1 + json.1);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        exact.0 + 2 * per_call.0,
        exact.1 + 2 * per_call.1,
    )?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        for fail in [false, true] {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'candidate failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            let result = insert(&tx, &records);
            if fail {
                let error = result.err().context("seed unexpectedly inserted")?;
                assert!(format!("{error:#}").contains("candidate failure"));
            } else {
                result?;
                assert_eq!(stored_json(&tx)?, expected);
            }
            tx.rollback()?;
            assert_eq!(original_json(&records)?, expected);
            exact.0 += per_call.0;
            exact.1 += per_call.1;
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn borrowed_candidate_keys_sql_failure_releases_reader_and_rolls_back_original_tx() -> Result<()> {
    let records = rows()?;
    let expected = original_json(&records)?;
    for conflict in [false, true] {
        let mut connection = connection()?;
        if conflict {
            let tx = connection.transaction()?;
            insert(&tx, &records)?;
            tx.commit()?;
        } else {
            connection.execute_batch(
                "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                 BEGIN SELECT RAISE(ABORT, 'candidate SQL failure'); END;",
            )?;
        }
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
        let error = insert(&tx, &records)
            .err()
            .context("seed unexpectedly inserted")?;
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        assert!(format!("{error:#}").contains(if conflict {
            "UNIQUE constraint failed"
        } else {
            "candidate SQL failure"
        }));
        tx.execute_batch("DROP TABLE runtime_metadata;")?;
        tx.rollback()?;
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM candidate_markers", [], |row| {
                row.get::<_, u64>(0)
            })?,
            0
        );
        assert_eq!(
            connection.query_row("SELECT value FROM runtime_metadata", [], |row| {
                row.get::<_, String>(0)
            })?,
            "candidate-boundary"
        );
        assert_eq!(original_json(&records)?, expected);
        if conflict {
            assert_eq!(stored_json(&connection)?, expected);
        } else {
            assert_eq!(
                connection.query_row(
                    "SELECT COUNT(*) FROM episode_continuity_seeds",
                    [],
                    |row| { row.get::<_, u64>(0) }
                )?,
                0
            );
        }
    }
    Ok(())
}

#[test]
fn borrowed_candidate_keys_real_import_keeps_original_rows_and_atomic_seed_binding() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let mut records = rows()?;
    records.truncate(2);
    for row in &mut records {
        row.payload = serde_json::to_string(&row.record)?;
        db.connection()?.execute(
            "INSERT INTO context_episode_anchors (episode_id, agent_id, workspace_id, boundary_reason,
             start_turn_index, end_turn_index, started_at, ended_at, payload_json)
             VALUES (?1, ?2, ?3, 'wait_boundary', ?4, ?5, ?6, ?7, ?8)",
            params![row.record.id, AGENT, row.record.workspace_id, row.record.start_turn_index, row.record.end_turn_index,
                row.record.created_at.to_rfc3339(), row.record.finalized_at.to_rfc3339(), row.payload],
        )?;
    }
    let expected = original_json(&records)?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
         BEGIN SELECT RAISE(ABORT, 'candidate import failure'); END;",
    )?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let holder = phase.guard()?;
    let used;
    {
        let _entry = phase.enter();
        let write = |tx: &Transaction<'_>| -> Result<()> {
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("candidate import failure"));
        for table in [
            "episode_continuity_seeds",
            "agent_episode_continuity",
            "agent_identities",
            "agent_identity_reservations",
        ] {
            assert_eq!(
                db.connection()?.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
                    [AGENT],
                    |row| row.get::<_, u64>(0)
                )?,
                0
            );
        }
        db.connection()?
            .execute_batch("DROP TRIGGER reject_seed;")?;
        db.transaction(write)?;
        let stored: (String, String, String) = db.connection()?.query_row(
            "SELECT s.seed_id, c.seed_id, s.candidate_payloads_json
             FROM episode_continuity_seeds s JOIN agent_episode_continuity c ON c.seed_id = s.seed_id
             JOIN agent_identities i ON i.agent_id = s.agent_id
             WHERE s.agent_id = ?1 AND s.source_kind = 'legacy_identity_import'",
            [AGENT],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(stored.0, stored.1);
        assert_eq!(stored.2, expected);
        for row in &records {
            assert_eq!(
                db.connection()?.query_row(
                    "SELECT payload_json FROM context_episode_anchors WHERE episode_id = ?1",
                    [&row.record.id],
                    |row| row.get::<_, String>(0)
                )?,
                row.payload
            );
        }
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        used = phase.finish()?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(used.0 > 0 && used.1 > 0);
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
