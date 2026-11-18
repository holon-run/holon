use super::*;
use sha2::{Digest, Sha256};

const OUTCOME_ID: &str = "outcome:message:msg-legacy";
const AGENT_ID: &str = "agent-a";
const SEED_ID: &str = "seed-a";

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn legacy_payload(repair_id: Option<&str>) -> String {
    let inner = match repair_id {
        Some(repair_id) => serde_json::json!({
            "kind": "needs_repair",
            "repair_id": repair_id,
        }),
        None => serde_json::json!({ "kind": "needs_repair" }),
    };
    serde_json::json!({
        "outcome_id": OUTCOME_ID,
        "attempt_id": "attempt-legacy",
        "outcome": { "owner": "work_item", "outcome": inner },
        "created_at": "2026-10-30T00:00:00Z",
    })
    .to_string()
}

fn conversation_payload() -> String {
    serde_json::json!({
        "outcome_id": "outcome-keep",
        "attempt_id": "attempt-keep",
        "outcome": { "owner": "conversation", "outcome": { "kind": "replied" } },
        "created_at": "2026-10-30T00:00:00Z",
    })
    .to_string()
}

fn create_tables(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE execution_protocol_outcomes (
               agent_id TEXT NOT NULL,
               outcome_id TEXT NOT NULL,
               attempt_id TEXT NOT NULL,
               payload_json TEXT NOT NULL,
               PRIMARY KEY (agent_id, outcome_id)
             );
             CREATE TABLE episode_continuity_seeds (
               seed_id TEXT NOT NULL PRIMARY KEY,
               agent_id TEXT NOT NULL,
               roots_json TEXT NOT NULL
             );
             CREATE TABLE input_evidence_eligibility (
               seed_id TEXT NOT NULL,
               capture_phase TEXT NOT NULL,
               family TEXT NOT NULL,
               record_id TEXT NOT NULL,
               fingerprint_json TEXT NOT NULL,
               mandatory INTEGER NOT NULL,
               PRIMARY KEY (seed_id, capture_phase, family, record_id)
             );
             CREATE TABLE input_evidence_seed_recovery_current (
               seed_id TEXT NOT NULL,
               family TEXT NOT NULL,
               record_id TEXT NOT NULL,
               revision INTEGER NOT NULL,
               fingerprint_json TEXT NOT NULL,
               PRIMARY KEY (seed_id, family, record_id)
             );
             CREATE TABLE input_evidence_current (
               agent_id TEXT NOT NULL,
               identity_incarnation INTEGER NOT NULL,
               family TEXT NOT NULL,
               record_id TEXT NOT NULL,
               revision INTEGER NOT NULL,
               content_digest TEXT,
               fingerprint_json TEXT NOT NULL,
               availability TEXT NOT NULL,
               qualification TEXT NOT NULL,
               seed_id TEXT,
               attempt_turn_proof_json TEXT,
               PRIMARY KEY (agent_id, identity_incarnation, family, record_id)
             );",
    )?;
    Ok(())
}

fn seed_legacy_rows(connection: &Connection, payload: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO episode_continuity_seeds (seed_id, agent_id, roots_json)
             VALUES (?1, ?2, '{}')",
        params![SEED_ID, AGENT_ID],
    )?;
    connection.execute(
        "INSERT INTO execution_protocol_outcomes (agent_id, outcome_id, attempt_id, payload_json)
             VALUES (?1, ?2, 'attempt-legacy', ?3)",
        params![AGENT_ID, OUTCOME_ID, payload],
    )?;
    connection.execute(
        "INSERT INTO execution_protocol_outcomes (agent_id, outcome_id, attempt_id, payload_json)
             VALUES (?1, 'outcome-keep', 'attempt-keep', ?2)",
        params![AGENT_ID, conversation_payload()],
    )?;
    let stale_fingerprint = serde_json::json!({
        "content_hash": serde_json::Value::Null,
        "sequence": serde_json::Value::Null,
        "turn_index": serde_json::Value::Null,
        "created_at": "2026-10-30T00:00:00Z",
        "owner_kind": serde_json::Value::Null,
        "owner_id": serde_json::Value::Null,
        "revision_marker": "attempt-legacy",
        "payload_bytes": payload.len(),
        "related_turn_id": serde_json::Value::Null,
        "frozen_payload_digest": sha256_hex(payload),
    })
    .to_string();
    for phase in ["upgrade", "first_import"] {
        connection.execute(
            "INSERT INTO input_evidence_eligibility
                 (seed_id, capture_phase, family, record_id, fingerprint_json, mandatory)
                 VALUES (?1, ?2, 'outcome', ?3, ?4, 0)",
            params![SEED_ID, phase, OUTCOME_ID, stale_fingerprint],
        )?;
    }
    connection.execute(
        "INSERT INTO input_evidence_seed_recovery_current
             (seed_id, family, record_id, revision, fingerprint_json)
             VALUES (?1, 'outcome', ?2, 1, ?3)",
        params![SEED_ID, OUTCOME_ID, stale_fingerprint],
    )?;
    connection.execute(
        "INSERT INTO input_evidence_current
             (agent_id, identity_incarnation, family, record_id, revision, content_digest,
              fingerprint_json, availability, qualification, seed_id)
             VALUES (?1, 1, 'outcome', ?2, 3, ?3, json_remove(?4, '$.frozen_payload_digest'),
                     'present', 'committed_writer', ?5)",
        params![
            AGENT_ID,
            OUTCOME_ID,
            sha256_hex(payload),
            stale_fingerprint,
            SEED_ID
        ],
    )?;
    Ok(())
}

fn retired_outcome_migration() -> Migration {
    Migration {
        version: 115,
        name: "retire_outcome_needs_repair",
        sql: "",
    }
}

fn stored_fingerprints(connection: &Connection) -> Result<Vec<String>> {
    let mut fingerprints = Vec::new();
    let mut statement = connection.prepare(
        "SELECT fingerprint_json FROM input_evidence_eligibility
             WHERE family = 'outcome' AND record_id = ?1",
    )?;
    for row in statement.query_map([OUTCOME_ID], |row| row.get::<_, String>(0))? {
        fingerprints.push(row?);
    }
    let mut statement = connection.prepare(
        "SELECT fingerprint_json FROM input_evidence_seed_recovery_current
             WHERE family = 'outcome' AND record_id = ?1",
    )?;
    for row in statement.query_map([OUTCOME_ID], |row| row.get::<_, String>(0))? {
        fingerprints.push(row?);
    }
    let current: (String, String) = connection.query_row(
        "SELECT content_digest, fingerprint_json FROM input_evidence_current
             WHERE family = 'outcome' AND record_id = ?1",
        [OUTCOME_ID],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(current.0, sha256_hex(&rewritten_payload(connection)?));
    fingerprints.push(current.1);
    Ok(fingerprints)
}

fn rewritten_payload(connection: &Connection) -> Result<String> {
    Ok(connection.query_row(
        "SELECT payload_json FROM execution_protocol_outcomes
             WHERE agent_id = ?1 AND outcome_id = ?2",
        params![AGENT_ID, OUTCOME_ID],
        |row| row.get::<_, String>(0),
    )?)
}

#[test]
fn migration_converges_legacy_outcome_and_refreezes_evidence() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    create_tables(&connection)?;
    ensure_migration_table(&connection)?;
    let legacy = legacy_payload(Some("provider_dispatch_rejected:turn-1"));
    let conversation = conversation_payload();
    seed_legacy_rows(&connection, &legacy)?;

    apply_migration(
        &mut connection,
        &retired_outcome_migration(),
        unexpected_runtime_backfill,
    )?;

    let rewritten = rewritten_payload(&connection)?;
    let value: serde_json::Value = serde_json::from_str(&rewritten)?;
    assert_eq!(value["outcome"]["owner"], "work_item");
    assert_eq!(value["outcome"]["outcome"]["kind"], "interrupted");
    assert_eq!(
        value["outcome"]["outcome"]["reason"],
        "provider_dispatch_rejected:turn-1"
    );
    assert_eq!(value["outcome_id"], OUTCOME_ID);
    assert_eq!(value["attempt_id"], "attempt-legacy");
    assert_eq!(value["created_at"], "2026-10-30T00:00:00Z");
    let decoded = crate::execution::decode_outcome_record_with_migration(&rewritten)?;
    assert_eq!(
        decoded.outcome,
        holon_domain::execution_protocol::ExecutionOutcome::WorkItem(
            holon_domain::execution_protocol::WorkItemOutcome::Interrupted {
                reason: "provider_dispatch_rejected:turn-1".into()
            }
        )
    );

    let kept: String = connection.query_row(
        "SELECT payload_json FROM execution_protocol_outcomes WHERE outcome_id = 'outcome-keep'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(kept, conversation, "conversation outcomes are untouched");

    let mut legacy_rows = 0usize;
    let mut statement = connection
        .prepare("SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id = ?1")?;
    for row in statement.query_map([AGENT_ID], |row| row.get::<_, String>(0))? {
        let payload = row?;
        let value: serde_json::Value = serde_json::from_str(&payload)?;
        if value["outcome"]["outcome"]["kind"] == "needs_repair" {
            legacy_rows += 1;
        }
    }
    assert_eq!(legacy_rows, 0, "no retired encoding survives the migration");

    let mut fingerprints = stored_fingerprints(&connection)?;
    let current: serde_json::Value =
        serde_json::from_str(&fingerprints.pop().expect("Current fingerprint"))?;
    assert_eq!(current["payload_bytes"], rewritten.len());
    assert!(current.get("frozen_payload_digest").is_none());
    for fingerprint in fingerprints {
        let value: serde_json::Value = serde_json::from_str(&fingerprint)?;
        assert_eq!(value["payload_bytes"], rewritten.len());
        assert_eq!(value["frozen_payload_digest"], sha256_hex(&rewritten));
        assert_eq!(value["revision_marker"], "attempt-legacy");
    }
    assert_eq!(current_schema_version(&connection)?, 115);
    Ok(())
}

#[test]
fn damaged_legacy_outcome_rolls_the_migration_back() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    create_tables(&connection)?;
    ensure_migration_table(&connection)?;
    let damaged = legacy_payload(None);
    seed_legacy_rows(&connection, &damaged)?;

    let error = apply_migration(
        &mut connection,
        &retired_outcome_migration(),
        unexpected_runtime_backfill,
    )
    .unwrap_err();
    assert!(error.to_string().contains("lacks repair_id"));
    assert_eq!(rewritten_payload(&connection)?, damaged);
    assert_eq!(current_schema_version(&connection)?, 0);
    Ok(())
}

#[test]
fn malformed_outcome_json_rolls_the_migration_back() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    create_tables(&connection)?;
    ensure_migration_table(&connection)?;
    let legacy = legacy_payload(Some("provider_dispatch_rejected:turn-1"));
    seed_legacy_rows(&connection, &legacy)?;
    connection.execute(
        "INSERT INTO execution_protocol_outcomes (agent_id, outcome_id, attempt_id, payload_json)
             VALUES (?1, 'outcome-damaged', 'attempt-damaged', ?2)",
        params![AGENT_ID, "{\"outcome_id\": \"outcome-damaged\","],
    )?;

    let error = apply_migration(
        &mut connection,
        &retired_outcome_migration(),
        unexpected_runtime_backfill,
    )
    .unwrap_err();
    assert!(error.to_string().contains("malformed payload_json"));
    assert_eq!(current_schema_version(&connection)?, 0);
    let unchanged: String = connection.query_row(
        "SELECT payload_json FROM execution_protocol_outcomes WHERE outcome_id = ?1",
        [OUTCOME_ID],
        |row| row.get(0),
    )?;
    assert_eq!(
        unchanged, legacy,
        "rollback leaves the legacy row untouched"
    );
    Ok(())
}
