use super::*;

/// Runs only in the original fixed cutover/import actor. The manifest is stored
/// separately so a selected loader never retains a whole historical body map.
pub(in crate::runtime_db) fn capture_fixed_checkpoint_on(
    connection: &Connection,
    agent: &str,
    tables: Option<&BTreeSet<String>>,
    original: Option<&SchedulingContinuityCheckpoint>,
) -> Result<SchedulingContinuityCheckpoint> {
    connection.execute_batch(SCHEDULING_SCHEMA)?;
    if let Some(original) = original {
        validate_manifest(connection, original)?;
        ensure!(
            original.agent_id == agent,
            "Scheduling import owner changed"
        );
    }
    let id = crate::ids::runtime_id("scheduling_fixed");
    let mut observed = BTreeSet::new();
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        if tables.is_none_or(|t| t.contains(stored::table(family)))
            && (family != SchedulingFamily::TimerWake
                || tables.is_none_or(|t| t.contains("timers")))
        {
            observed.insert(family);
        }
    }
    let table_json = serde_json::to_string(&observed)?;
    connection.execute("INSERT INTO scheduling_fixed_manifests(inventory_id,agent_id,tables_json,member_count,metadata_bytes,content_digest,sealed) VALUES(?1,?2,?3,0,0,?4,0)",params![id,agent,table_json,"0".repeat(64)])?;
    let mut metadata = 0usize;
    let mut count = 0u64;
    for family in observed.iter().copied() {
        let (from, key, owner, _, sort) = stored::source(family);
        let predicate = if original.is_some() {
            format!("({key} IN(SELECT record_id FROM scheduling_fixed_members WHERE inventory_id=?2 AND family='{}') OR {key} IN(SELECT record_id FROM scheduling_import_inventory WHERE agent_id=?1 AND family='{}'))",family_name(family),family_name(family))
        } else {
            "1".into()
        };
        let sql=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({key} AS BLOB))+length(CAST(c.status AS BLOB))+length(CAST({sort} AS BLOB))+length(CAST(c.created_at AS BLOB))+512),0) FROM {from} WHERE {owner}=?1 AND {predicate}");
        let original_id = original.map(|x| x.inventory_id.as_str());
        let estimated: usize = if original.is_some() {
            connection.query_row(&sql, params![agent, original_id], |r| r.get(1))?
        } else {
            connection.query_row(&sql, [agent], |r| r.get(1))?
        };
        ensure!(
            metadata
                .checked_add(estimated)
                .is_some_and(|n| n <= MAX_BYTES),
            SchedulingSnapshotError::ResourceExhausted("fixed metadata")
        );
        let sql =
            format!("SELECT {key} FROM {from} WHERE {owner}=?1 AND {predicate} ORDER BY {key}");
        let mut statement = connection.prepare(&sql)?;
        let mut rows = if original.is_some() {
            statement.query(params![agent, original_id])?
        } else {
            statement.query([agent])?
        };
        while let Some(row) = rows.next()? {
            let key = SchedulingKey::new(family, row.get::<_, String>(0)?);
            let stored = stored::read_stored(connection, &key, agent, None)?
                .context("fixed Scheduling row missing")?;
            let chosen = if let Some(original) = original {
                let received:Option<(String,String)>=connection.query_row("SELECT content_digest,projection_digest FROM scheduling_import_inventory WHERE agent_id=?1 AND family=?2 AND record_id=?3",params![agent,family_name(family),key.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                if received.as_ref() == Some(&(stored.content.clone(), stored.projection.clone())) {
                    received.unwrap()
                } else if let Some(fixed) = fixed_digest(connection, original, &key)? {
                    fixed
                } else {
                    continue;
                }
            } else {
                (stored.content.clone(), stored.projection.clone())
            };
            let size = usize::try_from(serialized_bytes(&(
                &key,
                &chosen,
                &stored.status,
                &stored.sort_at,
                &stored.created_at,
            ))?)?;
            metadata = metadata
                .checked_add(size)
                .context("fixed Scheduling metadata overflow")?;
            ensure!(
                metadata <= MAX_BYTES,
                SchedulingSnapshotError::ResourceExhausted("fixed metadata")
            );
            count = count
                .checked_add(1)
                .context("fixed Scheduling member count overflow")?;
            connection.execute("INSERT INTO scheduling_fixed_members(inventory_id,family,record_id,content_digest,projection_digest,status,sort_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id,family_name(family),key.id,chosen.0,chosen.1,stored.status,stored.sort_at,stored.created_at])?;
        }
    }
    // Original members absent from current tables retain their frozen qualification;
    // a missing canonical body is later CanonicalChanged, never observed absence.
    if let Some(original) = original {
        connection.execute("INSERT OR IGNORE INTO scheduling_fixed_members SELECT ?1,family,record_id,content_digest,projection_digest,status,sort_at,created_at FROM scheduling_fixed_members WHERE inventory_id=?2",params![id,original.inventory_id])?;
    }
    let (actual_count,actual_bytes):(u64,usize)=connection.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(record_id AS BLOB))+length(CAST(content_digest AS BLOB))+length(CAST(projection_digest AS BLOB))+length(CAST(status AS BLOB))+length(CAST(sort_at AS BLOB))+length(CAST(created_at AS BLOB))+128),0) FROM scheduling_fixed_members WHERE inventory_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        actual_count >= count && actual_bytes <= MAX_BYTES,
        SchedulingSnapshotError::ResourceExhausted("fixed final metadata")
    );
    let mut hash = Sha256::new();
    hash.update(table_json.as_bytes());
    let mut statement=connection.prepare("SELECT family,record_id,content_digest,projection_digest,status,sort_at,created_at FROM scheduling_fixed_members WHERE inventory_id=?1 ORDER BY family,record_id")?;
    let mut rows = statement.query([&id])?;
    while let Some(row) = rows.next()? {
        let fields: (String, String, String, String, String, String, String) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        );
        hash.update(serde_json::to_vec(&fields)?);
    }
    let content = format!("{:x}", hash.finalize());
    connection.execute("UPDATE scheduling_fixed_manifests SET member_count=?2,metadata_bytes=?3,content_digest=?4,sealed=1 WHERE inventory_id=?1 AND sealed=0",params![id,actual_count,actual_bytes,content])?;
    Ok(SchedulingContinuityCheckpoint {
        policy_version: 1,
        inventory_id: id,
        agent_id: agent.to_owned(),
        observed_tables: observed,
        member_count: actual_count,
        metadata_bytes: u64::try_from(actual_bytes)?,
        content_digest: content,
    })
}
fn validate_manifest(
    connection: &Connection,
    checkpoint: &SchedulingContinuityCheckpoint,
) -> Result<()> {
    ensure!(
        checkpoint.policy_version == 1
            && checkpoint.content_digest.len() == 64
            && checkpoint.metadata_bytes <= u64::try_from(MAX_BYTES)?,
        "invalid Scheduling fixed checkpoint"
    );
    let (length,agent_length,digest_length):(usize,usize,usize)=connection.query_row("SELECT length(CAST(tables_json AS BLOB)),length(CAST(agent_id AS BLOB)),length(CAST(content_digest AS BLOB)) FROM scheduling_fixed_manifests WHERE inventory_id=?1",[&checkpoint.inventory_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    ensure!(
        length <= 1024 && agent_length == checkpoint.agent_id.len() && digest_length == 64,
        "Scheduling fixed manifest scalar differs"
    );
    let value:(String,String,u64,u64,String,bool)=connection.query_row("SELECT agent_id,tables_json,member_count,metadata_bytes,content_digest,sealed FROM scheduling_fixed_manifests WHERE inventory_id=?1",[&checkpoint.inventory_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    ensure!(
        value.0 == checkpoint.agent_id
            && serde_json::from_str::<BTreeSet<SchedulingFamily>>(&value.1)?
                == checkpoint.observed_tables
            && value.2 == checkpoint.member_count
            && value.3 == checkpoint.metadata_bytes
            && value.4 == checkpoint.content_digest
            && value.5,
        "Scheduling fixed manifest changed"
    );
    Ok(())
}
pub(super) fn fixed_digest(
    connection: &Connection,
    checkpoint: &SchedulingContinuityCheckpoint,
    key: &SchedulingKey,
) -> Result<Option<(String, String)>> {
    let mut statement=connection.prepare("SELECT length(CAST(content_digest AS BLOB)),length(CAST(projection_digest AS BLOB)),content_digest,projection_digest FROM scheduling_fixed_members WHERE inventory_id=?1 AND family=?2 AND record_id=?3")?;
    let mut rows = statement.query(params![
        checkpoint.inventory_id,
        family_name(key.family),
        key.id
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    ensure!(
        row.get::<_, usize>(0)? == 64 && row.get::<_, usize>(1)? == 64,
        "Scheduling fixed digest length changed"
    );
    let frozen: (String, String) = (row.get(2)?, row.get(3)?);
    drop(rows);
    drop(statement);
    let mut overlays=connection.prepare("SELECT length(CAST(o.original_content_digest AS BLOB)),length(CAST(o.original_projection_digest AS BLOB)),length(CAST(o.content_digest AS BLOB)),length(CAST(o.projection_digest AS BLOB)),o.original_content_digest,o.original_projection_digest,o.content_digest,o.projection_digest,o.revision,EXISTS(SELECT 1 FROM scheduling_domain_captures original JOIN scheduling_fixed_members m ON m.inventory_id=original.inventory_id WHERE original.seed_id=o.seed_id AND m.family=o.family AND m.record_id=o.record_id AND m.content_digest=o.original_content_digest AND m.projection_digest=o.original_projection_digest) FROM scheduling_seed_recovery_current o JOIN scheduling_domain_captures c ON c.seed_id=o.seed_id JOIN episode_continuity_seeds s ON s.seed_id=o.seed_id WHERE c.inventory_id=?1 AND o.family=?2 AND o.record_id=?3 AND s.agent_id=?4 AND s.identity_incarnation IS NULL AND s.source_kind IN ('upgrade_unbound','legacy_state_import')")?;
    let mut rows = overlays.query(params![
        checkpoint.inventory_id,
        family_name(key.family),
        key.id,
        checkpoint.agent_id
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(Some(frozen));
    };
    ensure!(
        (0..4).all(|i| row.get::<_, usize>(i).ok() == Some(64)),
        "Scheduling seed overlay digest length changed"
    );
    ensure!(
        row.get::<_, u64>(8)? > 0 && row.get::<_, bool>(9)?,
        "Scheduling seed overlay lost its original fixed anchor"
    );
    let original: (String, String) = (row.get(4)?, row.get(5)?);
    let current: (String, String) = (row.get(6)?, row.get(7)?);
    ensure!(
        frozen == original || frozen == current,
        "Scheduling fixed overlay does not match this checkpoint"
    );
    ensure!(
        rows.next()?.is_none(),
        "Scheduling fixed overlay has multiple seed owners"
    );
    Ok(Some(current))
}
pub(super) fn manifests(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
) -> Result<Vec<SchedulingContinuityCheckpoint>> {
    manifests_inner(tx, scope, material, true)
}
/// Existing cleanup retains frozen cutover members before import is sealed.
/// It neither prepares the ordinary loader nor extends original membership.
pub(super) fn original_maintenance_manifests(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
) -> Result<Vec<SchedulingContinuityCheckpoint>> {
    manifests_inner(tx, scope, material, false)
}
fn manifests_inner(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
    require_sealed: bool,
) -> Result<Vec<SchedulingContinuityCheckpoint>> {
    let size:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    material.charge(size, 1)?;
    let (mode,seed):(String,Option<String>)=tx.query_row("SELECT mode,seed_id FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if mode == "fresh_empty" {
        ensure!(seed.is_none(), "Fresh Scheduling binding has a seed");
        return Ok(Vec::new());
    }
    ensure!(
        mode == "legacy_adopted",
        "invalid Scheduling continuity mode"
    );
    let seed = seed.context("legacy Scheduling seed missing")?;
    let (seed_matches,sealed):(bool,bool)=tx.query_row("SELECT agent_id=?2 AND (identity_incarnation IS NULL OR identity_incarnation=?3),NOT import_required OR EXISTS(SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id=s.seed_id) FROM episode_continuity_seeds s WHERE seed_id=?1",params![seed,scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(seed_matches, "Scheduling original seed scope differs");
    ensure!(
        !require_sealed || sealed,
        SchedulingSnapshotError::LegacyImportPending
    );
    manifests_for_seed(tx, &scope.agent_id, &seed, material)
}
pub(super) fn manifests_for_seed(
    tx: &Transaction<'_>,
    agent: &str,
    seed: &str,
    material: &mut Material<'_, '_>,
) -> Result<Vec<SchedulingContinuityCheckpoint>> {
    let size:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(inventory_id AS BLOB))+64),0) FROM scheduling_domain_captures WHERE seed_id=?1",[&seed],|r|r.get(0))?;
    material.charge(size, 3)?;
    let mut statement=tx.prepare("SELECT inventory_id FROM scheduling_domain_captures WHERE seed_id=?1 ORDER BY CASE capture_phase WHEN 'first_import' THEN 1 ELSE 0 END")?;
    let ids = statement
        .query_map([&seed], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !ids.is_empty() && ids.len() <= 3,
        "Scheduling fixed capture missing"
    );
    let mut out = Vec::new();
    for id in ids {
        let bytes:usize=tx.query_row("SELECT length(CAST(agent_id AS BLOB))+length(CAST(tables_json AS BLOB))+length(CAST(content_digest AS BLOB))+128 FROM scheduling_fixed_manifests WHERE inventory_id=?1",[&id],|r|r.get(0))?;
        material.charge(bytes, 1)?;
        let (actual_agent,tables,count,bytes,content):(String,String,u64,u64,String)=tx.query_row("SELECT agent_id,tables_json,member_count,metadata_bytes,content_digest FROM scheduling_fixed_manifests WHERE inventory_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let checkpoint = SchedulingContinuityCheckpoint {
            policy_version: 1,
            inventory_id: id,
            agent_id: actual_agent,
            observed_tables: serde_json::from_str(&tables)?,
            member_count: count,
            metadata_bytes: bytes,
            content_digest: content,
        };
        validate_manifest(tx, &checkpoint)?;
        ensure!(
            checkpoint.agent_id == agent,
            "Scheduling fixed owner changed"
        );
        out.push(checkpoint);
    }
    Ok(out)
}
pub(super) fn known_anywhere(tx: &Connection, key: &SchedulingKey) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM scheduling_domain_records WHERE family=?1 AND record_id=?2 AND availability!='absent' UNION ALL SELECT 1 FROM scheduling_fixed_members WHERE family=?1 AND record_id=?2)",params![family_name(key.family),key.id],|r|r.get(0))?)
}
pub(in crate::runtime_db) fn consume_scheduling_upgrade_tx(tx: &Transaction<'_>) -> Result<()> {
    let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(seed_id AS BLOB))+length(CAST(source_kind AS BLOB))),0) FROM episode_continuity_seeds",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        count <= MAX_RECORDS && bytes <= MAX_BYTES,
        SchedulingSnapshotError::ResourceExhausted("seed inventory")
    );
    let mut statement =
        tx.prepare("SELECT seed_id,source_kind FROM episode_continuity_seeds ORDER BY seed_id")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seed, kind) in rows {
        capture_scheduling_seed_if_ready_tx(
            tx,
            &seed,
            if kind.starts_with("upgrade_") {
                "upgrade"
            } else {
                "legacy_import"
            },
        )?;
        let imported: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM episode_continuity_imports WHERE seed_id=?1)",
            [&seed],
            |r| r.get(0),
        )?;
        if imported {
            capture_scheduling_seed_if_ready_tx(tx, &seed, "first_import")?;
        }
    }
    Ok(())
}
pub(in crate::runtime_db) fn capture_scheduling_seed_if_ready_tx(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
) -> Result<()> {
    let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='scheduling_domain_captures' AND type='table')",[],|r|r.get(0))?;
    if !ready {
        return Ok(());
    }
    let table = if phase == "first_import" {
        "episode_continuity_imports"
    } else {
        "episode_continuity_seeds"
    };
    let size: usize = tx.query_row(
        &format!("SELECT length(CAST(roots_json AS BLOB)) FROM {table} WHERE seed_id=?1"),
        [seed],
        |r| r.get(0),
    )?;
    ensure!(
        size <= MAX_BYTES,
        SchedulingSnapshotError::ResourceExhausted("original roots")
    );
    let raw: String = tx.query_row(
        &format!("SELECT roots_json FROM {table} WHERE seed_id=?1"),
        [seed],
        |r| r.get(0),
    )?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let checkpoint: SchedulingContinuityCheckpoint = serde_json::from_value(
        value
            .get("scheduling_domain_checkpoint")
            .context("original Scheduling checkpoint missing; later recapture forbidden")?
            .clone(),
    )?;
    validate_manifest(tx, &checkpoint)?;
    tx.execute("INSERT INTO scheduling_domain_captures(seed_id,capture_phase,inventory_id) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![seed,phase,checkpoint.inventory_id])?;
    let actual: String = tx.query_row(
        "SELECT inventory_id FROM scheduling_domain_captures WHERE seed_id=?1 AND capture_phase=?2",
        params![seed, phase],
        |r| r.get(0),
    )?;
    ensure!(
        actual == checkpoint.inventory_id,
        "Scheduling fixed phase cannot be replaced"
    );
    Ok(())
}
