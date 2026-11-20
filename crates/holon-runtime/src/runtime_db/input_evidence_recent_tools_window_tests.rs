use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Snapshot of the pre-#4165 canonical-driven window SQL, kept as the
/// equivalence oracle: the eligibility-key access path must return exactly
/// the same keys in the same order for every configuration.
fn legacy_recent_tools_window(
    tx: &rusqlite::Transaction<'_>,
    agent: &str,
    incarnation: u64,
    seed: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    let mut statement = tx.prepare(
        "SELECT r.evidence_id FROM tool_executions r
         WHERE r.agent_id=?1 AND 1
           AND (EXISTS(SELECT 1 FROM input_evidence_current c
                       WHERE c.agent_id=r.agent_id AND c.identity_incarnation=?3 AND c.family='tool_execution' AND c.record_id=r.evidence_id AND c.availability='present')
                OR (NOT EXISTS(SELECT 1 FROM input_evidence_current c WHERE c.agent_id=r.agent_id AND c.identity_incarnation=?3 AND c.family='tool_execution' AND c.record_id=r.evidence_id)
                    AND (EXISTS(SELECT 1 FROM input_evidence_eligibility e WHERE e.seed_id=?4 AND e.family='tool_execution' AND e.record_id=r.evidence_id)
                         OR EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current e
                                   WHERE e.seed_id=?4 AND e.family='tool_execution' AND e.record_id=r.evidence_id))))
         ORDER BY r.created_at DESC,r.evidence_id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            agent,
            i64::try_from(limit)?,
            i64::try_from(incarnation)?,
            seed
        ],
        |row| row.get::<_, String>(0),
    )?;
    rows.map(|row| Ok(row?)).collect()
}

/// Full migrated schema with a fixed qualification set in front of an
/// ineligible canonical tail of `rows` tool executions. Present aliases cover
/// tool-00..07, seed eligibility tool-08..15, recovery membership tool-12..19,
/// and pruned current rows block tool-10/tool-14, leaving 18 qualified keys
/// regardless of history size. tool-20 is qualified only by another
/// incarnation, tool-21 only for another agent, tool-22 only through seed-b,
/// and tool-23 carries a pruned alias with no seed membership at all.
fn recent_tools_fixture(rows: i64) -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    tx.execute_batch(&format!(
        "INSERT INTO episode_continuity_seeds(
           seed_id,agent_id,policy_version,boundary_id,identity_incarnation,
           source_kind,roots_json,candidate_payloads_json,import_required,captured_at
         ) VALUES
           ('seed-a','agent-a',1,'fixture',1,'upgrade_identity','{{}}','[]',0,'2026-10-01'),
           ('seed-b','agent-b',1,'fixture',1,'upgrade_identity','{{}}','[]',0,'2026-10-01');
         WITH RECURSIVE history(seq) AS (
           VALUES(1) UNION ALL SELECT seq+1 FROM history WHERE seq<{rows}
         )
         INSERT INTO tool_executions(evidence_id,agent_id,created_at,kind,payload_json)
         SELECT printf('old-%07d',seq),'agent-a',
                '2026-10-01T00:00:00.' || printf('%06d',seq),
                'command','{{}}'
           FROM history;"
    ))?;
    tx.execute_batch(
        "INSERT INTO tool_executions(evidence_id,agent_id,created_at,kind,payload_json) VALUES
           ('tool-00','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-01','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-02','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-03','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-04','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-05','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-06','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-07','agent-a','2026-10-02T00:00:00','command','{}'),
           ('tool-08','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-09','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-10','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-11','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-12','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-13','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-14','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-15','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-16','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-17','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-18','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-19','agent-a','2026-10-01T12:00:00','command','{}'),
           ('tool-20','agent-a','2026-10-03T00:00:00','command','{}'),
           ('tool-21','agent-b','2026-10-03T00:00:00','command','{}'),
           ('tool-22','agent-a','2026-10-02T06:00:00','command','{}'),
           ('tool-23','agent-a','2026-10-01T06:00:00','command','{}');
         INSERT INTO input_evidence_eligibility(
           seed_id,capture_phase,family,record_id,fingerprint_json,mandatory
         ) VALUES
           ('seed-a','upgrade','tool_execution','tool-08','{}',0),
           ('seed-a','upgrade','tool_execution','tool-09','{}',0),
           ('seed-a','upgrade','tool_execution','tool-10','{}',0),
           ('seed-a','upgrade','tool_execution','tool-11','{}',0),
           ('seed-a','upgrade','tool_execution','tool-12','{}',0),
           ('seed-a','upgrade','tool_execution','tool-13','{}',0),
           ('seed-a','upgrade','tool_execution','tool-14','{}',0),
           ('seed-a','upgrade','tool_execution','tool-15','{}',0),
           ('seed-b','upgrade','tool_execution','tool-22','{}',0);
         INSERT INTO input_evidence_seed_recovery_current(
           seed_id,family,record_id,revision,fingerprint_json,origin_kind
         ) VALUES
           ('seed-a','tool_execution','tool-12',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-13',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-14',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-15',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-16',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-17',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-18',1,'{}','fixed_member'),
           ('seed-a','tool_execution','tool-19',1,'{}','fixed_member');
         INSERT INTO input_evidence_current(
           agent_id,identity_incarnation,family,record_id,revision,
           fingerprint_json,availability,qualification
         ) VALUES
           ('agent-a',1,'tool_execution','tool-00',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-01',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-02',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-03',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-04',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-05',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-06',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-07',1,'{}','present','committed_writer'),
           ('agent-a',1,'tool_execution','tool-10',1,'{}','pruned','committed_writer'),
           ('agent-a',1,'tool_execution','tool-14',1,'{}','pruned','committed_writer'),
           ('agent-a',1,'tool_execution','tool-23',1,'{}','pruned','committed_writer'),
           ('agent-a',2,'tool_execution','tool-20',1,'{}','present','committed_writer'),
           ('agent-b',1,'tool_execution','tool-21',1,'{}','present','committed_writer');",
    )?;
    tx.commit()?;
    Ok(connection)
}

#[test]
fn recent_tools_window_work_follows_eligibility_keys() -> Result<()> {
    let expected_seed_a = [
        "tool-07", "tool-06", "tool-05", "tool-04", "tool-03", "tool-02", "tool-01", "tool-00",
        "tool-19", "tool-18", "tool-17", "tool-16", "tool-15", "tool-13", "tool-12", "tool-11",
        "tool-09", "tool-08",
    ];
    let window =
        |connection: &mut Connection, seed: Option<&str>, limit: usize| -> Result<Vec<String>> {
            let tx = connection.transaction()?;
            let keys = qualified_window_keys(
                &tx,
                &scope(),
                &Binding {
                    seed_id: seed.map(str::to_owned),
                },
                &InputWindowSelector::RecentTools,
                limit,
            )?
            .into_iter()
            .map(|key| key.id)
            .collect::<Vec<_>>();
            Ok(keys)
        };

    // Equivalence with the pre-change SQL across plentiful hits, the
    // exhaustive boundary, pruned overrides, seed isolation, and no-seed
    // bindings.
    let mut connection = recent_tools_fixture(20_000)?;
    for limit in [5usize, 17, 18, 19, 65] {
        let tx = connection.transaction()?;
        let legacy = legacy_recent_tools_window(&tx, "agent-a", 1, Some("seed-a"), limit)?;
        drop(tx);
        assert_eq!(
            window(&mut connection, Some("seed-a"), limit)?,
            legacy,
            "seed-a limit={limit} diverged"
        );
    }
    for seed in [Some("seed-b"), None] {
        for limit in [5usize, 65] {
            let tx = connection.transaction()?;
            let legacy = legacy_recent_tools_window(&tx, "agent-a", 1, seed, limit)?;
            drop(tx);
            assert_eq!(
                window(&mut connection, seed, limit)?,
                legacy,
                "seed={seed:?} limit={limit} diverged"
            );
        }
    }
    assert_eq!(
        window(&mut connection, Some("seed-a"), 65)?,
        expected_seed_a
    );
    assert_eq!(
        window(&mut connection, Some("seed-b"), 65)?,
        [
            "tool-22", "tool-07", "tool-06", "tool-05", "tool-04", "tool-03", "tool-02", "tool-01",
            "tool-00",
        ]
    );
    assert_eq!(
        window(&mut connection, None, 65)?,
        ["tool-07", "tool-06", "tool-05", "tool-04", "tool-03", "tool-02", "tool-01", "tool-00",]
    );

    // Count VM work instead of wall time: the ineligible canonical tail must
    // not be scanned just to pick the qualified window.
    let window_ticks = |rows: i64, limit: usize| -> Result<(usize, Vec<String>)> {
        let mut connection = recent_tools_fixture(rows)?;
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
        let keys = window(&mut connection, Some("seed-a"), limit)?;
        Ok((ticks.load(Ordering::Relaxed), keys))
    };
    let mut smallest = usize::MAX;
    let mut largest = 0usize;
    for rows in [20_000i64, 80_000, 320_000] {
        for limit in [5usize, 65] {
            let (ticks, keys) = window_ticks(rows, limit)?;
            assert_eq!(keys, &expected_seed_a[..limit.min(expected_seed_a.len())]);
            assert!(
                ticks < 10,
                "recent tools window used at least {} VM steps for {rows} canonical rows",
                ticks * 1_000
            );
            smallest = smallest.min(ticks);
            largest = largest.max(ticks);
        }
    }
    assert!(
        largest <= smallest + 1,
        "window VM steps grew with canonical history: {smallest}..{largest}"
    );
    Ok(())
}
