//! Exclusive startup advances only the versions of original unbound members.
use super::*;
use crate::runtime_db::input_evidence_snapshot::legacy_recovery::{
    legacy_seed_recovery_scope_tx, ExclusiveStartupInputRecovery, LegacySeedRecoveryScope,
};
use crate::types::TimerStatus;

struct Target {
    before: stored::StoredScheduling,
    fixed: Option<(String, String)>,
}
pub(super) struct UnboundSchedulingCleanup<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: LegacySeedRecoveryScope,
    targets: BTreeMap<SchedulingKey, Target>,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
fn validate_unbound(
    tx: &Transaction<'_>,
    scope: &LegacySeedRecoveryScope,
    material: &mut Material<'_, '_>,
) -> Result<()> {
    let (n,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(seed_id AS BLOB))+length(CAST(boundary_id AS BLOB))+length(CAST(source_kind AS BLOB))+length(CAST(agent_id AS BLOB))),0) FROM episode_continuity_seeds WHERE agent_id=?1",[&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    material.charge(bytes, n)?;
    let (n,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(capture_phase AS BLOB))+length(CAST(metadata_digest AS BLOB))+64),0) FROM input_evidence_captures WHERE seed_id=?1",[&scope.seed_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    material.charge(bytes, n)?;
    let boundary:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM runtime_metadata WHERE key='episode_continuity/v1/upgrade_boundary'",[],|r|r.get(0))?;
    material.charge(boundary, 1)?;
    ensure!(
        legacy_seed_recovery_scope_tx(tx, &scope.agent_id)? == *scope,
        "unbound Scheduling original seed changed"
    );
    ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM scheduling_domain_current WHERE agent_id=?1 UNION ALL SELECT 1 FROM scheduling_domain_records WHERE agent_id=?1)",[&scope.agent_id],|r|r.get::<_,bool>(0))?,"unbound Scheduling has a published namespace");
    Ok(())
}
pub(super) fn validate_unbound_scope_tx(
    tx: &Transaction<'_>,
    scope: &LegacySeedRecoveryScope,
    material: &mut Material<'_, '_>,
) -> Result<()> {
    validate_unbound(tx, scope, material)
}

pub(super) fn prepare_unbound_scheduling_cleanup_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &LegacySeedRecoveryScope,
    keys: &BTreeSet<SchedulingKey>,
    accounting: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    admission: &ExclusiveStartupInputRecovery<'_>,
) -> Result<UnboundSchedulingCleanup<'tx, 'conn>> {
    admission.validate_transaction(tx)?;
    ensure!(
        accounting.agent_id == scope.agent_id,
        "unbound Scheduling accounting owner differs"
    );
    let mut material = Material::new(accounting, request, ledger);
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys))?)?,
        keys.len() + 1,
    )?;
    validate_unbound(tx, scope, &mut material)?;
    let manifests =
        qualification::manifests_for_seed(tx, &scope.agent_id, &scope.seed_id, &mut material)?;
    let mut targets = BTreeMap::new();
    for key in keys {
        ensure!(
            matches!(
                key.family,
                SchedulingFamily::Timer | SchedulingFamily::TimerWake
            ),
            "unbound startup cannot write Trigger/new family"
        );
        if key.family == SchedulingFamily::TimerWake {
            let (timer, _) = key.wake_pair()?;
            ensure!(
                keys.contains(&SchedulingKey::new(SchedulingFamily::Timer, timer)),
                "unbound Wake omitted owning Timer"
            );
        }
        let before = stored::read_stored(tx, key, &scope.agent_id, Some(&mut material))?
            .context("unbound startup target disappeared")?;
        let mut fixed = None;
        for manifest in manifests.iter().rev() {
            material.charge(128, 0)?;
            if let Some(pair) = qualification::fixed_digest(tx, manifest, key)? {
                fixed = Some(pair);
                break;
            }
        }
        if let Some(expected) = &fixed {
            ensure!(
                *expected == (before.content.clone(), before.projection.clone()),
                SchedulingSnapshotError::CanonicalChanged(key.clone())
            );
        }
        targets.insert(key.clone(), Target { before, fixed });
    }
    Ok(UnboundSchedulingCleanup {
        tx,
        scope: scope.clone(),
        targets,
        bytes: material.bytes,
        records: material.records,
        guards: material.guards,
    })
}
fn cancelled_version(before: &CanonicalSchedulingFact, after: &CanonicalSchedulingFact) -> bool {
    match (before, after) {
        (CanonicalSchedulingFact::Timer(a), CanonicalSchedulingFact::Timer(b)) => {
            let mut expected = a.clone();
            expected.status = TimerStatus::Cancelled;
            a.status == TimerStatus::Active && *b == expected
        }
        (CanonicalSchedulingFact::TimerWake(a), CanonicalSchedulingFact::TimerWake(b)) => {
            let mut expected = a.clone();
            expected.status = SchedulingWakeStatus::Cancelled;
            expected.updated_at = b.updated_at;
            expected.cancelled_at = b.cancelled_at;
            a.status == SchedulingWakeStatus::Pending
                && b.cancelled_at == Some(b.updated_at)
                && *b == expected
        }
        _ => false,
    }
}
pub(super) fn register_unbound_scheduling_cleanup_tx(
    tx: &Transaction<'_>,
    mut proof: UnboundSchedulingCleanup<'_, '_>,
    actual: &BTreeSet<SchedulingKey>,
    accounting: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    admission: &ExclusiveStartupInputRecovery<'_>,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    ensure!(
        std::ptr::eq(tx, proof.tx),
        "unbound Scheduling crossed transaction"
    );
    admission.validate_transaction(tx)?;
    ensure!(
        accounting.agent_id == proof.scope.agent_id,
        "unbound Scheduling accounting owner differs"
    );
    ensure!(
        actual.iter().all(|key| proof.targets.contains_key(key)),
        "unbound Scheduling actual footprint exceeds original targets"
    );
    let mut material = Material::new(accounting, request, ledger);
    material.bytes = proof.bytes;
    material.records = proof.records;
    validate_unbound(tx, &proof.scope, &mut material)?;
    for (key, target) in proof.targets {
        let after = stored::read_stored(tx, &key, &proof.scope.agent_id, Some(&mut material))?
            .context("unbound cleanup removed original target")?;
        let changed =
            after.content != target.before.content || after.projection != target.before.projection;
        ensure!(
            !changed || actual.contains(&key),
            "unbound Scheduling changed undeclared target"
        );
        if !changed {
            continue;
        }
        ensure!(
            cancelled_version(&target.before.fact, &after.fact),
            "unbound Scheduling is not original cancellation"
        );
        let Some(fixed) = target.fixed else {
            continue;
        };
        // All fixed digests are scalar-gated before strings by fixed_digest.
        // The first insert freezes the original anchor; retries only advance its version.
        tx.execute("INSERT INTO scheduling_seed_recovery_current(seed_id,family,record_id,original_content_digest,original_projection_digest,content_digest,projection_digest,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,1) ON CONFLICT(seed_id,family,record_id) DO UPDATE SET content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,revision=revision+1 WHERE content_digest=?4 AND projection_digest=?5",params![proof.scope.seed_id,family_name(key.family),key.id,fixed.0,fixed.1,after.content,after.projection])?;
        let matches:bool=tx.query_row("SELECT content_digest=?4 AND projection_digest=?5 FROM scheduling_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![proof.scope.seed_id,family_name(key.family),key.id,after.content,after.projection],|r|r.get(0))?;
        ensure!(matches, "unbound Scheduling version overlay conflicts");
    }
    proof.guards.extend(material.guards);
    Ok(proof.guards)
}
