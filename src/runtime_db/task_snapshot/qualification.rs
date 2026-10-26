use super::super::episode_snapshot::task_checkpoint::{self, TaskContinuityCheckpoint};
use super::*;
pub(in crate::runtime_db) fn capture_task_domain_upgrade_tx(tx: &Transaction<'_>) -> Result<()> {
    // DDL only for an unsupported intermediate development DB: never recapture
    // its old canonical history after the original fixed boundary.
    task_checkpoint::queries::ensure_schema(tx)?;
    super::super::input_evidence_snapshot::initialize_output_input_relations_tx(tx)?;
    let mut query =
        tx.prepare("SELECT seed_id,source_kind FROM episode_continuity_seeds ORDER BY seed_id")?;
    let rows = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seed, kind) in rows {
        capture_seed(
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
            capture_seed(tx, &seed, "first_import")?;
        }
    }
    Ok(())
}
pub(in crate::runtime_db) fn capture_task_domain_seed_if_ready_tx(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
) -> Result<()> {
    let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='task_domain_captures')",[],|r|r.get(0))?;
    if ready {
        capture_seed(tx, seed, phase)?;
    }
    Ok(())
}
fn capture_seed(tx: &Transaction<'_>, seed: &str, phase: &str) -> Result<()> {
    let checkpoint = task_checkpoint::load(tx, seed, phase)?;
    let encoded = serde_json::to_vec(&checkpoint)?;
    bound(
        checkpoint
            .record_digests
            .len()
            .checked_add(checkpoint.unavailable_refs.len())
            .context("fixed Task count overflow")?,
        encoded.len(),
        None,
    )?;
    let expected = digest(&encoded);
    let prior:Option<String>=tx.query_row("SELECT metadata_digest FROM task_domain_captures WHERE seed_id=?1 AND capture_phase=?2",params![seed,phase],|r|r.get(0)).optional()?;
    if let Some(prior) = prior {
        ensure!(prior == expected, "fixed Task checkpoint changed");
        return Ok(());
    }
    tx.execute(
        "INSERT INTO task_domain_captures(seed_id,capture_phase,metadata_digest) VALUES(?1,?2,?3)",
        params![seed, phase, expected],
    )?;
    for (key, value) in &checkpoint.record_digests {
        let key = task_checkpoint::parse_key(key)?;
        let projection = checkpoint
            .projection_digests
            .get(&format!(
                "{}:{}",
                if key.family == TaskFamily::Task {
                    "task"
                } else {
                    "settlement"
                },
                key.id
            ))
            .context("fixed Task projection missing")?;
        tx.execute("INSERT INTO task_domain_eligibility(seed_id,capture_phase,family,record_id,availability,content_digest,projection_digest) VALUES(?1,?2,?3,?4,'present',?5,?6)",params![seed,phase,family_name(key.family),key.id,value,projection])?;
    }
    for key in &checkpoint.unavailable_refs {
        let key = task_checkpoint::parse_key(key)?;
        tx.execute("INSERT INTO task_domain_eligibility(seed_id,capture_phase,family,record_id,availability,content_digest) VALUES(?1,?2,?3,?4,'unavailable',NULL)",params![seed,phase,family_name(key.family),key.id])?;
    }
    Ok(())
}
pub(super) fn manifests(
    tx: &Transaction<'_>,
    seed: Option<&str>,
) -> Result<Vec<TaskContinuityCheckpoint>> {
    let Some(seed) = seed else {
        return Ok(Vec::new());
    };
    // There are at most the three fixed phases, but damaged digest TEXT can
    // still be arbitrarily large. Check SQL bytes before collecting Strings.
    let malformed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_domain_captures WHERE seed_id=?1 AND COALESCE(length(CAST(metadata_digest AS BLOB))!=64,1))",
        [seed], |row| row.get(0),
    )?;
    ensure!(!malformed, "fixed Task checkpoint digest length changed");
    let mut q=tx.prepare("SELECT capture_phase,metadata_digest FROM task_domain_captures WHERE seed_id=?1 ORDER BY CASE capture_phase WHEN 'first_import' THEN 1 ELSE 0 END")?;
    let rows = q
        .query_map([seed], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(!rows.is_empty(), "bound fixed Task checkpoint missing");
    rows.into_iter()
        .map(|(phase, expected)| {
            let checkpoint = task_checkpoint::load(tx, seed, &phase)?;
            ensure!(
                digest(&serde_json::to_vec(&checkpoint)?) == expected,
                "fixed Task checkpoint changed"
            );
            Ok(checkpoint)
        })
        .collect()
}
pub(super) fn eligible_digest(
    tx: &Transaction<'_>,
    seed: Option<&str>,
    key: &TaskKey,
) -> Result<Option<(String, String)>> {
    let Some(seed) = seed else { return Ok(None) };
    if let Some(current) = seed_maintenance::current_seed_digest_tx(tx, seed, key)? {
        return Ok(Some(current));
    }
    if let Some(fixed) = fixed_eligible_digest(tx, seed, key)? {
        return Ok(Some(fixed));
    }
    for checkpoint in manifests(tx, Some(seed))?.into_iter().rev() {
        if let Some(inventory) = checkpoint.query_inventory {
            return task_checkpoint::queries::fact_digest(tx, &inventory, key);
        }
    }
    Ok(None)
}
/// SQL byte lengths precede either digest String allocation, including damaged
/// present rows. Unavailable membership remains distinct from malformed present.
pub(super) fn fixed_eligible_digest(
    tx: &Transaction<'_>,
    seed: &str,
    key: &TaskKey,
) -> Result<Option<(String, String)>> {
    let mut query = tx.prepare_cached("SELECT length(CAST(content_digest AS BLOB)),length(CAST(projection_digest AS BLOB)),content_digest,projection_digest FROM task_domain_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3 AND availability='present' ORDER BY CASE capture_phase WHEN 'first_import' THEN 0 ELSE 1 END LIMIT 1")?;
    let mut rows = query.query(params![seed, family_name(key.family), key.id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let content_bytes: Option<usize> = row.get(0)?;
    let projection_bytes: Option<usize> = row.get(1)?;
    ensure!(
        content_bytes == Some(64) && projection_bytes == Some(64),
        "fixed Task eligibility digest length changed"
    );
    let pair: (String, String) = (row.get(2)?, row.get(3)?);
    ensure!(
        pair.0
            .bytes()
            .chain(pair.1.bytes())
            .all(|byte| byte.is_ascii_hexdigit()),
        "invalid fixed Task eligibility digest"
    );
    Ok(Some(pair))
}
pub(super) fn known_anywhere(tx: &Transaction<'_>, key: &TaskKey) -> Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE family=?1 AND record_id=?2 AND availability!='absent' UNION ALL SELECT 1 FROM task_domain_eligibility WHERE family=?1 AND record_id=?2 UNION ALL SELECT 1 FROM task_query_fixed_tasks WHERE ?1='task' AND task_id=?2 UNION ALL SELECT 1 FROM task_query_fixed_results WHERE ?1='result_settlement' AND result_identity=?2)",params![family_name(key.family),key.id],|r|r.get(0)).map_err(Into::into)
}
pub(super) fn validate_member(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    key: &TaskKey,
    stored: &StoredTask,
) -> Result<u64> {
    if let Some(alias) = alias(tx, scope, key)? {
        ensure!(
            alias.availability == TaskAvailability::Present,
            "Task alias is absent or pruned"
        );
        if alias.digest.as_deref() != Some(stored.digest().as_str())
            || alias.projection_digest.as_deref() != Some(stored.projection_digest.as_str())
        {
            return Err(TaskSnapshotError::CanonicalChanged(key.clone()).into());
        }
        return Ok(alias.revision);
    }
    let expected = eligible_digest(tx, seed, key)?
        .ok_or_else(|| TaskSnapshotError::NeedsPreparation(key.clone()))?;
    if expected != (stored.digest(), stored.projection_digest.clone()) {
        return Err(TaskSnapshotError::CanonicalChanged(key.clone()).into());
    }
    Ok(1)
}
