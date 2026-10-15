use super::*;
/// Original cutover only. A seed retains its original bound/unbound identity.
pub(crate) fn capture_current_read_upgrade_tx(tx: &Transaction<'_>) -> Result<()> {
    super::record_invalidation::install_tx(tx)?;
    let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(seed_id AS BLOB))+length(CAST(agent_id AS BLOB))+128),0) FROM episode_continuity_seeds",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        count <= CURRENT_READ_MAX_RECORDS && bytes <= CURRENT_READ_MAX_BYTES,
        CurrentReadError::ResourceExhausted
    );
    let mut statement =
        tx.prepare("SELECT seed_id,agent_id FROM episode_continuity_seeds ORDER BY seed_id")?;
    let seeds = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seed, agent) in seeds {
        capture(tx, &seed, &agent, "upgrade", false)?;
        let imported: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM episode_continuity_imports WHERE seed_id=?1)",
            [&seed],
            |r| r.get(0),
        )?;
        if imported {
            // This original import completed before this domain was installed.
            // The new cutover set is already frozen above; no later tail refill.
            capture_current_read_import_if_ready_tx(tx, &seed, &agent)?;
        }
    }
    Ok(())
}
/// Only actual imported IDs may augment the original fixed set; no raw refill.
pub(in crate::runtime_db) fn capture_original_current_read_import_tx(
    tx: &Transaction<'_>,
    seed: &str,
    agent: &str,
) -> Result<()> {
    let valid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM episode_continuity_seeds WHERE seed_id=?1 AND agent_id=?2)",
        params![seed, agent],
        |r| r.get(0),
    )?;
    ensure!(valid, "CurrentRead import seed differs");
    capture(tx, seed, agent, "first_import", true)
}
fn capture(tx: &Transaction<'_>, seed: &str, agent: &str, phase: &str, import: bool) -> Result<()> {
    super::record_invalidation::install_tx(tx)?;
    if tx.query_row("SELECT EXISTS(SELECT 1 FROM current_read_fixed_captures WHERE seed_id=?1 AND capture_phase=?2 AND sealed=1)",params![seed,phase],|r|r.get::<_,bool>(0))? { return Ok(()); }
    let(queue_available,event_available):(bool,bool)=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='queue_entries'),EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='audit_events')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    tx.execute("INSERT INTO current_read_fixed_captures(seed_id,capture_phase,queue_available,event_available,member_count,sealed) VALUES(?1,?2,?3,?4,0,0)",params![seed,phase,queue_available,event_available])?;
    let mut count = 0usize;
    let mut bytes = 0usize;
    for f in [CurrentReadFamily::Queue, CurrentReadFamily::Event] {
        let (table, id, _, _) = stored::source(f);
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            continue;
        }
        let predicate = if import {
            format!("({id} IN(SELECT record_id FROM current_read_fixed_members WHERE seed_id=?2 AND family=?3) OR {id} IN(SELECT record_id FROM current_read_import_inventory WHERE agent_id=?1 AND family=?3))")
        } else {
            "1".into()
        };
        let sql = format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({id} AS BLOB))+256),0) FROM {table} WHERE agent_id=?1 AND {predicate}");
        let (n, size): (usize, usize) = if import {
            tx.query_row(&sql, params![agent, seed, family(f)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
        } else {
            tx.query_row(&sql, [agent], |r| Ok((r.get(0)?, r.get(1)?)))?
        };
        count = count
            .checked_add(n)
            .context("CurrentRead fixed count overflow")?;
        bytes = bytes
            .checked_add(size)
            .context("CurrentRead fixed metadata overflow")?;
        ensure!(
            count <= CURRENT_READ_MAX_RECORDS && bytes <= CURRENT_READ_MAX_BYTES,
            CurrentReadError::ResourceExhausted
        );
        let mut query = tx.prepare(&format!(
            "SELECT {id} FROM {table} WHERE agent_id=?1 AND {predicate} ORDER BY {id}"
        ))?;
        let mut rows = if import {
            query.query(params![agent, seed, family(f)])?
        } else {
            query.query([agent])?
        };
        while let Some(row) = rows.next()? {
            let key = CurrentReadKey {
                family: f,
                id: row.get(0)?,
            };
            let stored = stored::fixed_fingerprint(tx, &key, agent)?;
            if import {
                let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM current_read_fixed_members WHERE seed_id=?1 AND family=?2 AND record_id=?3 AND content_digest=?4 AND projection_digest=?5 UNION ALL SELECT 1 FROM current_read_import_inventory WHERE agent_id=?6 AND family=?2 AND record_id=?3 AND content_digest=?4 AND projection_digest=?5)",params![seed,family(f),key.id,stored.content,stored.projection,agent],|r|r.get(0))?;
                ensure!(valid, CurrentReadError::CanonicalChanged);
            }
            tx.execute("INSERT INTO current_read_fixed_members(seed_id,capture_phase,family,record_id,agent_id,content_digest,projection_digest,status,sort_seq,sort_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![seed,phase,family(f),key.id,agent,stored.content,stored.projection,stored.status,stored.seq,stored.sort_at,stored.created_at])?;
        }
    }
    tx.execute("UPDATE current_read_fixed_captures SET member_count=?3,sealed=1 WHERE seed_id=?1 AND capture_phase=?2 AND sealed=0",params![seed,phase,count])?;
    Ok(())
}

fn import_schema_ready(tx: &Transaction<'_>) -> Result<bool> {
    let ready: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='current_read_fixed_captures')", [], |r|r.get(0))?;
    if !ready {
        let installed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=90)",
            [],
            |r| r.get(0),
        )?;
        ensure!(!installed, "CurrentRead installed import schema missing");
    }
    Ok(ready)
}
/// Invoked by the original Episode first-import actor after its immutable receipt.
pub(in crate::runtime_db) fn capture_current_read_import_if_ready_tx(
    tx: &Transaction<'_>,
    seed: &str,
    agent: &str,
) -> Result<()> {
    if !import_schema_ready(tx)? {
        return Ok(());
    }
    let complete: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM episode_continuity_imports i JOIN episode_continuity_seeds s ON s.seed_id=i.seed_id WHERE i.seed_id=?1 AND s.agent_id=?2)", params![seed,agent], |r|r.get(0))?;
    ensure!(complete, "CurrentRead original import receipt missing");
    capture_original_current_read_import_tx(tx, seed, agent)
}
/// Only the actual existing legacy import kernel supplies this exact readback.
/// A rejected older Queue input supplies no authority for the newer DB body.
pub(in crate::runtime_db) fn record_current_read_import_tx(
    tx: &Transaction<'_>,
    agent: &str,
    record_family: CurrentReadFamily,
    id: &str,
    actual_received_digest: &str,
) -> Result<()> {
    if !import_schema_ready(tx)? {
        return Ok(());
    }
    ensure!(
        agent
            .len()
            .checked_add(id.len())
            .and_then(|n| n.checked_add(512))
            .and_then(|n| n.checked_mul(8))
            .is_some_and(|n| n <= CURRENT_READ_MAX_BYTES),
        CurrentReadError::ResourceExhausted
    );
    let key = CurrentReadKey {
        family: record_family,
        id: id.to_owned(),
    };
    let actual = stored::fixed_fingerprint(tx, &key, agent)?;
    if actual.content != actual_received_digest {
        return Ok(());
    }
    tx.execute("INSERT INTO current_read_import_inventory(agent_id,family,record_id,content_digest,projection_digest) VALUES(?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING", params![agent,family(key.family),key.id,actual.content,actual.projection])?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM current_read_import_inventory WHERE agent_id=?1 AND family=?2 AND record_id=?3 AND content_digest=?4 AND projection_digest=?5)", params![agent,family(key.family),key.id,actual.content,actual.projection], |r|r.get(0))?;
    ensure!(valid, "CurrentRead original import inventory changed");
    Ok(())
}

/// An original imported identity/State seed freezes only received import IDs.
pub(in crate::runtime_db) fn capture_current_read_legacy_seed_if_ready_tx(
    tx: &Transaction<'_>,
    seed: &str,
    agent: &str,
) -> Result<()> {
    if !import_schema_ready(tx)? {
        return Ok(());
    }
    let original: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM episode_continuity_seeds WHERE seed_id=?1 AND agent_id=?2 AND source_kind IN('legacy_identity_import','legacy_state_import'))", params![seed,agent], |r|r.get(0))?;
    ensure!(original, "CurrentRead original legacy seed differs");
    capture(tx, seed, agent, "legacy_import", true)
}
