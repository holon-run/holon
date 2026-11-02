//! Fixed unbound startup Wait cancellation; no new incarnation or membership.
use super::*;
use crate::runtime_db::input_evidence_snapshot::legacy_recovery::{
    legacy_seed_recovery_scope_tx, ExclusiveStartupInputRecovery, LegacySeedRecoveryScope,
};
use crate::types::WaitConditionStatus;

pub(crate) struct PreparedUnboundWorkWaitCleanup<'a, 'lease> {
    scope: LegacySeedRecoveryScope,
    candidates: BTreeSet<String>,
    before: BTreeMap<WorkKey, StoredRecord>,
    _capability: &'a ExclusiveStartupInputRecovery<'lease>,
}

pub(crate) fn prepare_unbound_work_wait_cleanup_tx<'a, 'lease>(
    tx: &Transaction<'_>,
    scope: &LegacySeedRecoveryScope,
    wait_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
    capability: &'a ExclusiveStartupInputRecovery<'lease>,
) -> Result<PreparedUnboundWorkWaitCleanup<'a, 'lease>> {
    ensure!(
        legacy_seed_recovery_scope_tx(tx, &scope.agent_id)? == *scope,
        "unbound Work startup seed proof changed"
    );
    ensure!(
        !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_domain_current WHERE agent_id=?1)",
            [&scope.agent_id],
            |row| row.get::<_, bool>(0)
        )?,
        "unbound Work startup has a published scope"
    );
    // Verify the immutable checkpoint before using its finite membership.
    qualification::manifests(tx, Some(&scope.seed_id))?;
    bound_metadata(wait_ids.len(), wait_ids.iter().map(String::len).sum())?;
    let mut before = BTreeMap::new();
    let mut bytes = 0usize;
    for id in wait_ids {
        let key = WorkKey::new(WorkFamily::Wait, id);
        let Some(expected) = qualification::eligible_digest(tx, Some(&scope.seed_id), &key)? else {
            // Historical global cleanup keeps its old write set. A late row
            // outside this fixed seed gains no current qualification.
            continue;
        };
        let length: usize = tx.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM wait_conditions WHERE wait_condition_id=?1",
            [id], |row| row.get(0))?;
        bytes = bytes.saturating_add(length);
        ensure!(
            bytes <= request.byte_limit && before.len() < request.record_limit,
            "unbound Work Wait cleanup budget exhausted"
        );
        let stored =
            stored_on(tx, &scope.agent_id, &key)?.context("fixed startup Wait disappeared")?;
        ensure!(
            stored.digest() == expected,
            "fixed startup Wait changed without its original writer"
        );
        let CanonicalWorkRecord::Wait(wait) = &stored.record else {
            unreachable!()
        };
        ensure!(
            matches!(
                wait.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            ),
            "unbound startup Wait is not a live original candidate"
        );
        before.insert(key, stored);
    }
    Ok(PreparedUnboundWorkWaitCleanup {
        scope: scope.clone(),
        candidates: wait_ids.clone(),
        before,
        _capability: capability,
    })
}

pub(crate) fn register_unbound_work_wait_cleanup_tx(
    tx: &Transaction<'_>,
    proof: &PreparedUnboundWorkWaitCleanup<'_, '_>,
    writes: &repositories::WaitConditionWrites,
) -> Result<()> {
    ensure!(
        legacy_seed_recovery_scope_tx(tx, &proof.scope.agent_id)? == proof.scope,
        "unbound Work startup seed changed before ACK"
    );
    ensure!(
        writes.wait_ids.is_subset(&proof.candidates),
        "unbound startup has an unprepared Wait write"
    );
    for id in &writes.wait_ids {
        let key = WorkKey::new(WorkFamily::Wait, id);
        let Some(before) = proof.before.get(&key) else {
            continue;
        };
        let after =
            stored_on(tx, &proof.scope.agent_id, &key)?.context("startup removed a fixed Wait")?;
        let (CanonicalWorkRecord::Wait(prior), CanonicalWorkRecord::Wait(current)) =
            (&before.record, &after.record)
        else {
            unreachable!()
        };
        let mut expected = prior.clone();
        expected.status = WaitConditionStatus::Cancelled;
        expected.updated_at = current.updated_at;
        expected.cancelled_at = current.cancelled_at;
        ensure!(
            current == &expected,
            "unbound startup changed more than original Wait cancellation"
        );
        ensure!(
            current.status == WaitConditionStatus::Cancelled
                && current.work_item_id == prior.work_item_id
                && current.turn_id == prior.turn_id
                && current.wake_sources == prior.wake_sources
                && current.trigger_message_id == prior.trigger_message_id
                && current.triggered_at == prior.triggered_at,
            "unbound startup changed fixed Wait ownership or source relations"
        );
        ensure!(
            current.id == prior.id && current.agent_id == prior.agent_id,
            "unbound startup Wait scope changed"
        );
        let original: Option<(String, i64)> = tx.query_row(
            "SELECT original_digest,revision FROM work_domain_seed_recovery_current WHERE seed_id=?1 AND family='wait' AND record_id=?2",
            params![proof.scope.seed_id,id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (anchor, revision) = original.map_or_else(
            || Ok::<_, anyhow::Error>((before.digest(), 1i64)),
            |(digest, revision)| {
                Ok((
                    digest,
                    revision
                        .checked_add(1)
                        .context("unbound Work revision overflow")?,
                ))
            },
        )?;
        let actual = after.digest();
        tx.execute("INSERT INTO work_domain_seed_recovery_current(seed_id,family,record_id,original_digest,content_digest,revision)
            VALUES(?1,'wait',?2,?3,?4,?5) ON CONFLICT(seed_id,family,record_id) DO UPDATE SET content_digest=excluded.content_digest,revision=excluded.revision",
            params![proof.scope.seed_id,id,anchor,actual,revision])?;
        let readback: String = tx.query_row("SELECT content_digest FROM work_domain_seed_recovery_current WHERE seed_id=?1 AND family='wait' AND record_id=?2",
            params![proof.scope.seed_id,id], |row| row.get(0))?;
        ensure!(readback == actual, "unbound Work Wait overlay ACK differs");
    }
    Ok(())
}

pub(super) fn digest_override_tx(
    tx: &Transaction<'_>,
    seed: &str,
    key: &WorkKey,
    fixed: Option<String>,
) -> Result<Option<String>> {
    if key.family != WorkFamily::Wait {
        return Ok(fixed);
    }
    let overlay: Option<(String,String)> = tx.query_row(
        "SELECT original_digest,content_digest FROM work_domain_seed_recovery_current WHERE seed_id=?1 AND family='wait' AND record_id=?2",
        params![seed,key.id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((original, current)) = overlay else {
        return Ok(fixed);
    };
    let fixed = fixed.context("unbound Work overlay has no original fixed member")?;
    ensure!(
        fixed == original || fixed == current,
        "unbound Work overlay conflicts with a later fixed import version"
    );
    ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM work_domain_eligibility WHERE seed_id=?1 AND family='wait' AND record_id=?2 AND content_digest=?3)",
        params![seed,key.id,original], |row| row.get::<_,bool>(0))?, "unbound Work overlay anchor is not a fixed member");
    Ok(Some(current))
}
