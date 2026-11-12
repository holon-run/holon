use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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
