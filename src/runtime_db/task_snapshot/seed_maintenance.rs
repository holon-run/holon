//! Exclusive pre-activation maintenance of immutable unbound fixed members.
use super::*;
#[cfg(test)]
use crate::runtime_db::input_evidence_snapshot::legacy_recovery::{
    legacy_seed_recovery_scope_tx, ExclusiveStartupInputRecovery, LegacySeedRecoveryScope,
};

#[cfg(test)]
pub(crate) struct UnboundTaskMaintenanceProof<'tx, 'connection, 'lease> {
    tx: &'tx Transaction<'connection>,
    cap: &'tx ExclusiveStartupInputRecovery<'lease>,
    seed: LegacySeedRecoveryScope,
    before: BTreeMap<TaskKey, StoredTask>,
    original_overlays: BTreeMap<TaskKey, Option<SeedOverlay>>,
}
#[derive(Debug)]
#[cfg(test)]
pub(crate) struct SeedTaskMaintenanceAck {
    pub seed: LegacySeedRecoveryScope,
    pub actual_fixed_keys: BTreeSet<TaskKey>,
}

/// Lengths only. The original actor owns its actual shared-ledger preparation
/// guard; this API does not invent an identity incarnation for an unbound seed.
#[cfg(test)]
pub(crate) fn unbound_task_maintenance_material_tx(
    tx: &Transaction<'_>,
    seed: &LegacySeedRecoveryScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<TaskMaintenanceMaterial> {
    let roots: usize = tx.query_row(
        "SELECT COALESCE(SUM(CASE WHEN c.capture_phase='first_import' THEN length(CAST(i.roots_json AS BLOB)) ELSE length(CAST(s.roots_json AS BLOB)) END),0) FROM task_domain_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id WHERE c.seed_id=?1",
        [&seed.seed_id],
        |r| r.get(0),
    )?;
    let mut bytes = roots
        .checked_add(read_preparation::encoded_metadata_bytes(seed)?)
        .context("unbound Task material overflow")?;
    for key in keys {
        let n = checkpoint_material_size_on(tx, key)?.unwrap_or_default();
        bytes = bytes
            .checked_add(n)
            .and_then(|n| n.checked_add(key.id.len() + 768))
            .context("unbound Task material overflow")?;
        bound(keys.len() + 1, bytes, None)?;
    }
    let bytes = bytes
        .checked_mul(8)
        .context("unbound Task copies overflow")?;
    let records = (keys.len() + 1)
        .checked_mul(8)
        .context("unbound Task count overflow")?;
    bound(records, bytes, None)?;
    Ok(TaskMaintenanceMaterial { bytes, records })
}
#[cfg(test)]
fn validate_seed_tx(tx: &Transaction<'_>, seed: &LegacySeedRecoveryScope) -> Result<()> {
    ensure!(
        legacy_seed_recovery_scope_tx(tx, &seed.agent_id)? == *seed,
        "unbound Task fixed seed changed/bound"
    );
    qualification::manifests(tx, Some(&seed.seed_id))?;
    Ok(())
}
/// Observe only the original fixed subset of a real cleanup's finite keys.
/// Raw old targets keep their business behavior without becoming members.
#[cfg(test)]
pub(crate) fn qualified_unbound_task_maintenance_keys_tx(
    tx: &Transaction<'_>,
    cap: &ExclusiveStartupInputRecovery<'_>,
    seed: &LegacySeedRecoveryScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<BTreeSet<TaskKey>> {
    cap.validate_transaction(tx)?;
    ensure!(
        legacy_seed_recovery_scope_tx(tx, &seed.agent_id)? == *seed,
        "unbound Task fixed seed changed/bound"
    );
    bound(
        keys.len(),
        keys.iter().map(|key| key.id.len() + 128).sum(),
        None,
    )?;
    let mut qualified = BTreeSet::new();
    for key in keys {
        if qualification::eligible_digest(tx, Some(&seed.seed_id), key)?.is_some() {
            qualified.insert(key.clone());
        }
    }
    Ok(qualified)
}
#[cfg(test)]
pub(crate) fn prepare_unbound_task_maintenance_tx<'tx, 'connection, 'lease>(
    tx: &'tx Transaction<'connection>,
    cap: &'tx ExclusiveStartupInputRecovery<'lease>,
    seed: &LegacySeedRecoveryScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<UnboundTaskMaintenanceProof<'tx, 'connection, 'lease>> {
    cap.validate_transaction(tx)?;
    unbound_task_maintenance_material_tx(tx, seed, keys)?;
    validate_seed_tx(tx, seed)?;
    lengths(tx, keys, None)?;
    let mut before = BTreeMap::new();
    let mut original_overlays = BTreeMap::new();
    for key in keys {
        let expected = qualification::eligible_digest(tx, Some(&seed.seed_id), key)?
            .context("unbound Task target is not a fixed present member")?;
        let actual =
            stored_on(tx, &seed.agent_id, key)?.context("unbound Task fixed body missing")?;
        ensure!(
            expected == (actual.digest(), actual.projection_digest.clone()),
            "unbound Task fixed/overlay version changed"
        );
        original_overlays.insert(key.clone(), seed_overlay_tx(tx, &seed.seed_id, key)?);
        before.insert(key.clone(), actual);
    }
    Ok(UnboundTaskMaintenanceProof {
        tx,
        cap,
        seed: seed.clone(),
        before,
        original_overlays,
    })
}
#[cfg(test)]
pub(crate) fn register_unbound_task_maintenance_tx(
    tx: &Transaction<'_>,
    proof: UnboundTaskMaintenanceProof<'_, '_, '_>,
    actual_keys: &BTreeSet<TaskKey>,
) -> Result<SeedTaskMaintenanceAck> {
    ensure!(
        std::ptr::eq(tx, proof.tx),
        "unbound Task proof belongs to another transaction"
    );
    proof.cap.validate_transaction(tx)?;
    validate_seed_tx(tx, &proof.seed)?;
    ensure!(
        actual_keys.iter().all(|key| proof.before.contains_key(key)),
        "unbound Task actual writes exceed fixed pre-SQL footprint"
    );
    lengths(tx, actual_keys, None)?;
    for key in actual_keys {
        let after = stored_on(tx, &proof.seed.agent_id, key)?
            .context("unbound Task fixed body missing after maintenance")?;
        let before = &proof.before[key];
        // This overlay only advances existing bodies. It cannot create a new
        // Result/Task, delete one, or change the immutable fixed membership.
        let fixed = qualification::fixed_eligible_digest(tx, &proof.seed.seed_id, key)?
            .context("unbound Task fixed origin disappeared")?;
        let prior = seed_overlay_tx(tx, &proof.seed.seed_id, key)?;
        ensure!(
            prior == proof.original_overlays[key],
            "unbound Task overlay changed after preparation"
        );
        let revision = prior.as_ref().map_or(Ok(1), |value| {
            value
                .revision
                .checked_add(1)
                .context("unbound Task overlay revision overflow")
        })?;
        let (origin_content, origin_projection) =
            prior.map_or(fixed, |p| (p.fixed_content, p.fixed_projection));
        if before == &after {
            continue;
        }
        tx.execute("INSERT INTO task_domain_seed_recovery_current(seed_id,family,record_id,revision,content_digest,projection_digest,fixed_content_digest,fixed_projection_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(seed_id,family,record_id) DO UPDATE SET revision=excluded.revision,content_digest=excluded.content_digest,projection_digest=excluded.projection_digest",params![proof.seed.seed_id,family_name(key.family),key.id,revision,after.digest(),after.projection_digest,origin_content,origin_projection])?;
        let actual = seed_overlay_tx(tx, &proof.seed.seed_id, key)?
            .context("unbound Task overlay ACK missing")?;
        ensure!(
            actual.revision == revision
                && actual.content == after.digest()
                && actual.projection == after.projection_digest,
            "unbound Task overlay exact ACK mismatch"
        );
    }
    Ok(SeedTaskMaintenanceAck {
        seed: proof.seed,
        actual_fixed_keys: actual_keys.clone(),
    })
}
#[derive(Debug, PartialEq, Eq)]
struct SeedOverlay {
    revision: u64,
    content: String,
    projection: String,
    fixed_content: String,
    fixed_projection: String,
}
fn seed_overlay_tx(tx: &Transaction<'_>, seed: &str, key: &TaskKey) -> Result<Option<SeedOverlay>> {
    let n:Option<usize>=tx.query_row("SELECT length(CAST(content_digest AS BLOB))+length(CAST(projection_digest AS BLOB))+length(CAST(fixed_content_digest AS BLOB))+length(CAST(fixed_projection_digest AS BLOB)) FROM task_domain_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,family_name(key.family),key.id],|r|r.get(0)).optional()?;
    let Some(n) = n else { return Ok(None) };
    ensure!(n == 256, "unbound Task overlay digest length changed");
    let row=tx.query_row("SELECT revision,content_digest,projection_digest,fixed_content_digest,fixed_projection_digest FROM task_domain_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,family_name(key.family),key.id],|r|Ok(SeedOverlay{revision:r.get(0)?,content:r.get(1)?,projection:r.get(2)?,fixed_content:r.get(3)?,fixed_projection:r.get(4)?}))?;
    ensure!(
        row.revision > 0
            && [
                &row.content,
                &row.projection,
                &row.fixed_content,
                &row.fixed_projection
            ]
            .iter()
            .all(|s| s.bytes().all(|b| b.is_ascii_hexdigit())),
        "invalid unbound Task overlay digest/revision"
    );
    let fixed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3 AND availability='present' AND content_digest=?4 AND projection_digest=?5)",params![seed,family_name(key.family),key.id,row.fixed_content,row.fixed_projection],|r|r.get(0))?;
    ensure!(fixed, "unbound Task overlay has no exact fixed origin");
    Ok(Some(row))
}
/// Used only after binding the same immutable seed, never by agent-name lookup.
pub(super) fn current_seed_digest_tx(
    tx: &Transaction<'_>,
    seed: &str,
    key: &TaskKey,
) -> Result<Option<(String, String)>> {
    Ok(seed_overlay_tx(tx, seed, key)?.map(|p| (p.content, p.projection)))
}
