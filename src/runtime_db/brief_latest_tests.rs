//! Synthetic, bounded checks of the production latest-Brief SQL. Never opens a
//! production database. The ignored test is a repeatable A/B/A measurement.
use std::{fs, path::Path, time::Instant};

use anyhow::{ensure, Result};
use rusqlite::{params, Connection, StatementStatus};
use serde_json::{json, Value};

use super::{
    connection::{
        configure_new_database_auto_vacuum, configure_persistent_database, open_connection,
    },
    migrations::{
        apply_migration, current_schema_version, ensure_migration_table, schema_fingerprint,
        Migration, MIGRATIONS,
    },
    observer_sync::{AGENT_LATEST_BRIEF_SQL, AGENT_ROSTER_LATEST_BRIEFS_SQL},
    RuntimeDb,
};

const INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS idx_briefs_agent_latest
    ON briefs(agent_id, created_at DESC, evidence_id DESC)";
const DROP_INDEX_SQL: &str = "DROP INDEX IF EXISTS idx_briefs_agent_latest";
const DATE: &str = "2026-01-01T00:00:00.000Z";

fn seed(db: &RuntimeDb, hot_history: usize) -> Result<()> {
    db.transaction(|tx| seed_rows(tx, hot_history))
}

fn seed_rows(connection: &Connection, hot_history: usize) -> Result<()> {
    for (id, visibility, status) in [
        ("hot", "public", "active"),
        ("small", "public", "active"),
        ("empty", "public", "active"),
        ("hidden", "private", "active"),
        ("deleted", "public", "deleted"),
    ] {
        connection.execute(
            "INSERT INTO agent_identities
                 (agent_id, kind, visibility, status, created_at, updated_at, payload_json)
                 VALUES (?1, 'named', ?2, ?3, ?4, ?4, '{}')",
            params![id, visibility, status, DATE],
        )?;
        let count = match id {
            "hot" => hot_history,
            "empty" => 0,
            _ => 32,
        };
        let mut insert = connection.prepare(
            "INSERT INTO briefs
                 (evidence_id, agent_id, turn_id, created_at, kind, preview, payload_json)
                 VALUES (?1, ?2, ?3, ?4, 'result', 'synthetic', '{}')",
        )?;
        for row in 0..count {
            // Fixed permutation: insertion/evidence order is not time order;
            // timestamps tie across turns and agents.
            insert.execute(params![
                format!("{id}-{row:08}"),
                id,
                format!("turn-{}", row % 4),
                format!("2026-01-{:02}T00:00:00.000Z", (row * 7 % 23) + 1),
            ])?;
        }
    }
    Ok(())
}

fn roster_briefs(connection: &Connection) -> Result<Vec<(String, String)>> {
    let mut statement = connection.prepare(AGENT_ROSTER_LATEST_BRIEFS_SQL)?;
    let mut rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.sort();
    Ok(rows)
}

fn plan(connection: &Connection, sql: &str, agent: bool) -> Result<Vec<String>> {
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let parameters: &[&dyn rusqlite::ToSql] = if agent { &[&"hot"] } else { &[] };
    let rows = statement
        .query_map(parameters, |row| row.get(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[test]
fn latest_brief_index_preserves_time_ties_and_visibility() -> Result<()> {
    let root = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        root.path().join("runtime.sqlite"),
        root.path().join("runtime.lock"),
    )?;
    seed(&db, 256)?;
    let connection = db.connection()?;
    connection.execute_batch(DROP_INDEX_SQL)?;
    let before = roster_briefs(&connection)?;
    assert_eq!(before.len(), 2);
    assert!(before
        .iter()
        .all(|(agent, _)| agent == "hot" || agent == "small"));
    let expected_hot = (0..256)
        .max_by_key(|row| ((row * 7 % 23) + 1, *row))
        .unwrap();
    assert!(before.contains(&("hot".into(), format!("hot-{expected_hot:08}"))));
    assert!(plan(&connection, AGENT_ROSTER_LATEST_BRIEFS_SQL, false)?
        .iter()
        .any(|detail| detail.contains("TEMP B-TREE")));
    connection.execute_batch(INDEX_SQL)?;
    connection.execute_batch(INDEX_SQL)?;
    assert_eq!(before, roster_briefs(&connection)?);
    for (sql, agent) in [
        (AGENT_ROSTER_LATEST_BRIEFS_SQL, false),
        (AGENT_LATEST_BRIEF_SQL, true),
    ] {
        let details = plan(&connection, sql, agent)?;
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("idx_briefs_agent_latest")),
            "{details:?}"
        );
        assert!(
            details.iter().all(|detail| !detail.contains("TEMP B-TREE")),
            "{details:?}"
        );
    }
    for agent in ["hot", "small", "empty", "hidden", "deleted"] {
        let mut statement = connection.prepare(AGENT_LATEST_BRIEF_SQL)?;
        let single = statement
            .query_map([agent], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // The selector alone is not an authorization boundary; the roster is.
        assert_eq!(single.len(), usize::from(agent != "empty"));
        if let Some((_, id)) = before.iter().find(|(id, _)| id == agent) {
            assert_eq!(single, vec![id.clone()]);
        }
    }
    let rows = db.agent_roster_snapshot_rows()?;
    assert_eq!(rows.rows.len(), 3);
    assert!(rows
        .rows
        .iter()
        .find(|row| row.agent_id == "empty")
        .unwrap()
        .latest_brief
        .is_none());
    Ok(())
}

fn index_migration() -> &'static Migration {
    MIGRATIONS
        .iter()
        .find(|migration| migration.name == "briefs_agent_latest_index")
        .expect("latest-Brief index migration")
}

fn predecessor_database(path: &Path, hot_history: usize) -> Result<Connection> {
    let mut connection = open_connection(path)?;
    configure_new_database_auto_vacuum(&connection)?;
    configure_persistent_database(&connection)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version < index_migration().version)
    {
        apply_migration(&mut connection, migration)?;
    }
    let transaction = connection.transaction()?;
    seed_rows(&transaction, hot_history)?;
    transaction.commit()?;
    Ok(connection)
}

fn assert_index_schema(connection: &Connection) -> Result<()> {
    let columns = connection
        .prepare("PRAGMA index_xinfo(idx_briefs_agent_latest)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, bool>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, _, key)| *key)
        .map(|(name, descending, _)| (name.unwrap(), descending))
        .collect::<Vec<_>>();
    assert_eq!(
        columns,
        vec![
            ("agent_id".into(), 0),
            ("created_at".into(), 1),
            ("evidence_id".into(), 1),
        ]
    );
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    assert_eq!(integrity, "ok");
    Ok(())
}

fn brief_contents(connection: &Connection) -> Result<String> {
    Ok(connection.query_row(
        "SELECT json_group_array(json_array(
            evidence_id, agent_id, turn_id, created_at, kind, preview, payload_json))
         FROM (SELECT * FROM briefs ORDER BY evidence_id)",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn latest_brief_index_fresh_schema_matches_upgrade_and_reopen() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("upgrade.sqlite");
    let connection = predecessor_database(&path, 256)?;
    assert_eq!(
        current_schema_version(&connection)?,
        index_migration().version - 1
    );
    let before = brief_contents(&connection)?;
    let latest = roster_briefs(&connection)?;
    assert!(plan(&connection, AGENT_ROSTER_LATEST_BRIEFS_SQL, false)?
        .iter()
        .any(|detail| detail.contains("TEMP B-TREE")));
    drop(connection);

    let lock = path.with_extension("lock");
    let upgraded = RuntimeDb::open_and_migrate(&path, &lock)?;
    let connection = upgraded.connection()?;
    assert_index_schema(&connection)?;
    assert_eq!(brief_contents(&connection)?, before);
    assert_eq!(roster_briefs(&connection)?, latest);
    let fingerprint = schema_fingerprint(&connection)?;
    let schema_cookie: i64 = connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    let applied_at: String = connection.query_row(
        "SELECT applied_at FROM schema_migrations WHERE version = ?1",
        [index_migration().version],
        |row| row.get(0),
    )?;
    drop(connection);
    drop(upgraded);

    let reopened = RuntimeDb::open_and_migrate(&path, &lock)?;
    let connection = reopened.connection()?;
    assert_index_schema(&connection)?;
    assert_eq!(brief_contents(&connection)?, before);
    assert_eq!(schema_fingerprint(&connection)?, fingerprint);
    assert_eq!(
        connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?,
        schema_cookie,
        "reopening must not rebuild the index"
    );
    assert_eq!(
        connection.query_row(
            "SELECT applied_at FROM schema_migrations WHERE version = ?1",
            [index_migration().version],
            |row| row.get::<_, String>(0),
        )?,
        applied_at
    );
    let fresh = RuntimeDb::open_and_migrate(
        root.path().join("fresh.sqlite"),
        root.path().join("fresh.lock"),
    )?;
    let connection = fresh.connection()?;
    assert_index_schema(&connection)?;
    assert_eq!(schema_fingerprint(&connection)?, fingerprint);
    Ok(())
}

#[test]
fn latest_brief_index_migration_rolls_back_and_retries_atomically() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut connection = predecessor_database(&root.path().join("rollback.sqlite"), 256)?;
    let before = brief_contents(&connection)?;
    connection.execute_batch(&format!(
        "CREATE TRIGGER reject_brief_index_marker BEFORE INSERT ON schema_migrations
         WHEN NEW.version = {}
         BEGIN SELECT RAISE(ABORT, 'injected migration marker failure'); END;",
        index_migration().version
    ))?;
    let error = apply_migration(&mut connection, index_migration()).unwrap_err();
    assert!(error
        .to_string()
        .contains("injected migration marker failure"));
    assert_eq!(
        current_schema_version(&connection)?,
        index_migration().version - 1
    );
    let index_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_briefs_agent_latest'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(index_count, 0, "index DDL must roll back with its marker");
    assert_eq!(brief_contents(&connection)?, before);
    connection.execute_batch("DROP TRIGGER reject_brief_index_marker")?;
    apply_migration(&mut connection, index_migration())?;
    assert_index_schema(&connection)?;
    assert_eq!(
        current_schema_version(&connection)?,
        index_migration().version
    );
    let cookie: i64 = connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    apply_migration(&mut connection, index_migration())?;
    assert_eq!(
        connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?,
        cookie
    );
    assert_eq!(brief_contents(&connection)?, before);
    Ok(())
}

#[test]
fn latest_brief_index_migration_accepts_missing_legacy_evidence_table() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    let cookie: i64 = connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    apply_migration(&mut connection, index_migration())?;
    apply_migration(&mut connection, index_migration())?;
    assert_eq!(
        current_schema_version(&connection)?,
        index_migration().version
    );
    let marker_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1 AND name = ?2",
        params![index_migration().version, index_migration().name],
        |row| row.get(0),
    )?;
    assert_eq!(marker_count, 1);
    let schema_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE name IN ('briefs', 'idx_briefs_agent_latest')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(schema_count, 0, "do not recreate missing legacy tables");
    assert_eq!(
        connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?,
        cookie
    );
    Ok(())
}

fn percentiles(samples: &[u128]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = |p: usize| sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)];
    json!({"p50_ns": percentile(50), "p95_ns": percentile(95), "p99_ns": percentile(99)})
}

fn measure_sql(connection: &Connection, sql: &str, agent: bool, samples: usize) -> Result<Value> {
    let mut statement = connection.prepare(sql)?;
    let parameters: &[&dyn rusqlite::ToSql] = if agent { &[&"hot"] } else { &[] };
    let mut times = Vec::new();
    let mut sorts = Vec::new();
    let mut vm_steps = Vec::new();
    // Preparation is excluded; every execution is drained and decoded.
    for iteration in 0..=samples {
        statement.reset_status(StatementStatus::Sort);
        statement.reset_status(StatementStatus::VmStep);
        let start = Instant::now();
        let mut rows = statement.query(parameters)?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            std::hint::black_box(row.get::<_, String>(0)?);
            count += 1;
        }
        let elapsed = start.elapsed().as_nanos();
        drop(rows);
        ensure!(
            count == if agent { 1 } else { 2 },
            "unexpected SQL result count"
        );
        if iteration > 0 {
            times.push(elapsed);
            sorts.push(statement.get_status(StatementStatus::Sort));
            vm_steps.push(statement.get_status(StatementStatus::VmStep));
        }
    }
    Ok(json!({"latency": percentiles(&times), "samples_ns": times,
        "sorts": sorts, "vm_steps": vm_steps, "eqp": plan(connection, sql, agent)?}))
}

/// Run alone with `cargo test --lib runtime_db::brief_latest_tests::brief_latest_history_benchmark
/// -- --exact --ignored --nocapture`. Output is one `brief_latest_benchmark=` JSON line.
#[test]
#[ignore = "bounded synthetic A/B/A performance measurement, not a timing assertion"]
fn brief_latest_history_benchmark() -> Result<()> {
    let samples = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(40);
    ensure!(
        (20..=200).contains(&samples),
        "HOLON_BENCH_SAMPLES must be 20..=200"
    );
    let root = tempfile::tempdir()?;
    let base = RuntimeDb::open_and_migrate(
        root.path().join("base.sqlite"),
        root.path().join("base.lock"),
    )?;
    seed(&base, 8192)?;
    let connection = base.connection()?;
    connection.execute_batch(DROP_INDEX_SQL)?;
    let expected = roster_briefs(&connection)?;
    let snapshot = root.path().join("snapshot.sqlite");
    // An isolated synthetic snapshot, including committed WAL contents.
    connection.execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])?;
    drop(connection);
    drop(base);
    let mut cases = Vec::new();
    for round in 0..3 {
        for (arm, indexed) in [("before", false), ("indexed", true), ("after", false)] {
            let path = root.path().join(format!("{round}-{arm}.sqlite"));
            fs::copy(&snapshot, &path)?;
            let db = RuntimeDb::open_and_migrate(&path, path.with_extension("lock"))?;
            let connection = db.connection()?;
            // A future fresh-schema baseline may already contain the candidate.
            connection.execute_batch(DROP_INDEX_SQL)?;
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            let bytes_before = fs::metadata(&path)?.len();
            let migration = Instant::now();
            if indexed {
                connection.execute_batch(INDEX_SQL)?;
            }
            let index_build_ns = migration.elapsed().as_nanos();
            let wal_after_build = fs::metadata(path.with_extension("sqlite-wal"))
                .map_or(0, |metadata| metadata.len());
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            let bytes_after = fs::metadata(&path)?.len();
            ensure!(
                roster_briefs(&connection)? == expected,
                "A/B changed results"
            );
            let diagnostics_before = crate::diagnostics::performance_snapshot();
            let roster_sql =
                measure_sql(&connection, AGENT_ROSTER_LATEST_BRIEFS_SQL, false, samples)?;
            let single_sql = measure_sql(&connection, AGENT_LATEST_BRIEF_SQL, true, samples)?;
            let mut roster = Vec::new();
            for _ in 0..samples {
                let started = Instant::now();
                let rows = db.agent_roster_snapshot_rows()?;
                roster.push(started.elapsed().as_nanos());
                ensure!(rows.rows.len() == 3, "roster membership changed");
            }
            let mut writes = Vec::new();
            for write in 0..samples {
                let started = Instant::now();
                db.transaction(|tx| {
                    tx.execute(
                        "INSERT INTO briefs
                        (evidence_id, agent_id, created_at, kind, payload_json)
                        VALUES (?1, 'hot', ?2, 'result', '{}')",
                        params![format!("write-{write:08}"), DATE],
                    )?;
                    Ok(())
                })?;
                writes.push(started.elapsed().as_nanos());
            }
            cases.push(json!({"round": round, "arm": arm, "indexed": indexed,
                "roster_sql": roster_sql, "single_sql": single_sql,
                "roster_read": percentiles(&roster), "roster_samples_ns": roster,
                "writer_commit": percentiles(&writes), "writer_samples_ns": writes,
                "index_build_ns": index_build_ns,
                "database_bytes_before": bytes_before, "database_bytes_after": bytes_after,
                "wal_bytes_after_build": wal_after_build,
                "diagnostics_before": diagnostics_before,
                "diagnostics_after": crate::diagnostics::performance_snapshot()}));
        }
    }
    println!(
        "brief_latest_benchmark={}",
        json!({
        "schema": "holon.brief_latest_benchmark.v1", "sqlite": rusqlite::version(),
        "seed": "fixed-permutation-7-mod-23", "hot_history": 8192,
        "other_history": 32, "identities": 5, "public_active": 3,
        "samples": samples, "warmups": 1, "reader_capacity": 4,
        "debug_assertions": cfg!(debug_assertions), "cases": cases,
        "limitations": [
            "Synthetic bounded local fixture, not production attribution or capacity.",
            "A/B/A resets the same logical snapshot per arm; warm OS caches and shared-host noise remain.",
            "SQL timing is stepped/decoded execution, excluding preparation; roster_read includes checkout and assembly.",
            "Writer commit is one serial brief insert per transaction, not a production workload.",
            "Database/WAL bytes are checkpoints/end snapshots, not peak temporary disk consumption.",
            "Diagnostics are cumulative and overlapping; pool stages are aggregate, not per-SQL attribution.",
            "No frontend traffic or HTTP leader measurements in this DB-only workload."
        ]})
    );
    Ok(())
}

/// Measures a real predecessor upgrade, not just standalone CREATE INDEX.
#[test]
#[ignore = "bounded synthetic upgrade/reopen measurement, not a timing assertion"]
fn brief_latest_migration_benchmark() -> Result<()> {
    let root = tempfile::tempdir()?;
    let connection = predecessor_database(&root.path().join("predecessor.sqlite"), 8192)?;
    let expected = brief_contents(&connection)?;
    let snapshot = root.path().join("snapshot.sqlite");
    connection.execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])?;
    drop(connection);
    let mut cases = Vec::new();
    for round in 0..3 {
        let path = root.path().join(format!("upgrade-{round}.sqlite"));
        let lock = path.with_extension("lock");
        fs::copy(&snapshot, &path)?;
        let bytes_before = fs::metadata(&path)?.len();
        let started = Instant::now();
        let db = RuntimeDb::open_and_migrate(&path, &lock)?;
        let upgrade_open_ns = started.elapsed().as_nanos();
        let wal_after_upgrade =
            fs::metadata(path.with_extension("sqlite-wal")).map_or(0, |metadata| metadata.len());
        let connection = db.connection()?;
        assert_index_schema(&connection)?;
        assert_eq!(brief_contents(&connection)?, expected);
        let cookie: i64 = connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let bytes_after = fs::metadata(&path)?.len();
        drop(connection);
        drop(db);

        let started = Instant::now();
        let reopened = RuntimeDb::open_and_migrate(&path, &lock)?;
        let reopen_ns = started.elapsed().as_nanos();
        let connection = reopened.connection()?;
        assert_eq!(
            connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?,
            cookie,
            "reopen must not rebuild the index"
        );
        assert_eq!(brief_contents(&connection)?, expected);
        cases.push(json!({
            "round": round, "upgrade_open_ns": upgrade_open_ns, "reopen_ns": reopen_ns,
            "database_bytes_before": bytes_before, "database_bytes_after": bytes_after,
            "wal_bytes_after_upgrade": wal_after_upgrade,
        }));
    }
    println!(
        "brief_latest_migration_benchmark={}",
        json!({
            "schema": "holon.brief_latest_migration_benchmark.v1",
            "sqlite": rusqlite::version(), "predecessor": index_migration().version - 1,
            "target": super::migrations::max_known_migration_version(),
            "hot_history": 8192, "other_history": 32, "briefs": 8288,
            "debug_assertions": cfg!(debug_assertions), "cases": cases,
            "limitations": [
                "Synthetic bounded local fixture; no production rows, DDL or downtime estimate.",
                "Upgrade timing includes open, migrations and verification, not only index creation.",
                "Reopen timing includes normal startup checks; it is not index rebuild time.",
                "Warm OS caches and shared-host noise remain; three runs are not tail percentiles.",
                "Database/WAL snapshots are not peak temporary disk consumption."
            ]
        })
    );
    Ok(())
}
