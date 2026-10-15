//! Only the original whole Stop may close its old-scope Audit responsibility.
use super::*;
use crate::runtime_db::task_stop::OriginalTaskStopAuditView;

pub(in crate::runtime_db::current_read_snapshot) fn prepare_metadata_stop_events_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    source: &OriginalTaskStopAuditView<'_, '_>,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    source.validate_tx(tx)?;
    let scope = source.scope();
    request.check_material(prior.0, prior.1)?;
    let mut material = Material::new(scope, request, ledger);
    material.bytes = prior.0;
    material.records = prior.1;
    material.guards = guards;
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys, request))?)?,
        keys.len() + 8,
    )?;
    ensure!(
        keys.iter().all(|key| key.family == CurrentReadFamily::Event
            && source.events().iter().any(|event| event.id == key.id)),
        "focused Stop Audit contains a foreign target"
    );
    // This reads only original-scope metadata, never a successor identity/State.
    let saved = loader::saved(tx, scope, &mut material)?;
    let members:u64=tx.query_row("SELECT COALESCE(MAX(revision),0) FROM current_read_members WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    ensure!(
        saved.as_ref().is_none_or(|saved| members <= saved.revision),
        CurrentReadError::RevisionMismatch
    );
    let base_revision = saved
        .as_ref()
        .map_or(members, |saved| saved.revision.max(members));
    let mut before = BTreeMap::new();
    for key in keys {
        if let Some((_, availability, content, projection)) =
            loader::eligible(tx, scope, key, &mut material)?
        {
            ensure!(
                availability == CurrentReadAvailability::Present,
                CurrentReadError::Pruned(key.clone())
            );
            let record = stored::read(tx, key, &scope.agent_id, &mut material)?
                .ok_or(CurrentReadError::CanonicalChanged)?;
            ensure!(
                record.content == content && record.projection == projection,
                CurrentReadError::CanonicalChanged
            );
            before.insert(key.clone(), Some((content, projection)));
        } else {
            let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM audit_events WHERE audit_event_id=?1 UNION ALL SELECT 1 FROM current_read_members WHERE family='event' AND record_id=?1 UNION ALL SELECT 1 FROM current_read_fixed_members WHERE family='event' AND record_id=?1)",[&key.id],|r|r.get(0))?;
            ensure!(!known, CurrentReadError::NeedsPreparation);
            before.insert(key.clone(), None);
        }
    }
    Ok(CurrentReadWriteProof {
        tx,
        scope: scope.clone(),
        nonce: uuid::Uuid::new_v4(),
        request: request.clone(),
        ledger: ledger.clone(),
        base_revision,
        before_state: String::new(),
        before_identity: String::new(),
        original_recovery: true,
        binding_rebound: false,
        state_source: None,
        projection: CurrentReadProjection::FocusedEvents(saved),
        before,
        bytes: material.bytes,
        records: material.records,
        guards: material.guards,
    })
}

pub(in crate::runtime_db::current_read_snapshot) fn register_metadata_stop_events_tx(
    mut proof: CurrentReadWriteProof<'_, '_>,
    actual: &[CurrentReadActualWrite<'_, '_>],
) -> Result<CommittedCurrentReadMaintenance> {
    let CurrentReadProjection::FocusedEvents(before_saved) =
        std::mem::replace(&mut proof.projection, CurrentReadProjection::Ready)
    else {
        anyhow::bail!("focused Stop Audit requires its own consumed proof")
    };
    ensure!(
        proof.state_source.is_none(),
        "focused Audit cannot carry a State source"
    );
    let tx = proof.tx;
    let mut material = Material::new(&proof.scope, &proof.request, &proof.ledger);
    material.bytes = proof.bytes;
    material.records = proof.records;
    material.charge(
        usize::try_from(serialized_bytes(&FrozenCurrentReadKeys(&proof.before))?)?,
        actual.len() + 8,
    )?;
    let saved = loader::saved(tx, &proof.scope, &mut material)?;
    ensure!(
        before_saved.as_ref().map(|s| &s.coverage) == saved.as_ref().map(|s| &s.coverage),
        CurrentReadError::RevisionMismatch
    );
    let members:u64=tx.query_row("SELECT COALESCE(MAX(revision),0) FROM current_read_members WHERE agent_id=?1 AND identity_incarnation=?2",
        params![proof.scope.agent_id,proof.scope.identity_incarnation],|r|r.get(0))?;
    ensure!(
        members <= proof.base_revision,
        CurrentReadError::RevisionMismatch
    );
    let mut seen = BTreeSet::new();
    for write in actual {
        ensure!(
            std::ptr::eq(tx, write.tx)
                && write.nonce == proof.nonce
                && write.key.family == CurrentReadFamily::Event
                && proof.before.contains_key(&write.key)
                && seen.insert(&write.key),
            "focused Audit actual ACK differs from original proof"
        );
        let record = stored::read(tx, &write.key, &proof.scope.agent_id, &mut material)?
            .ok_or(CurrentReadError::CanonicalChanged)?;
        ensure!(
            record.content == write.content && record.projection == write.projection,
            CurrentReadError::CanonicalChanged
        );
        if !write.changed {
            ensure!(
                proof.before[&write.key]
                    .as_ref()
                    .is_some_and(|(c, p)| c == &write.content && p == &write.projection),
                "focused Audit noop cannot adopt a raw old row"
            );
        }
    }
    for (key, before) in &proof.before {
        if seen.contains(key) {
            continue;
        }
        let after = stored::read(tx, key, &proof.scope.agent_id, &mut material)?;
        ensure!(
            before.as_ref().map(|(c, p)| (c.as_str(), p.as_str()))
                == after
                    .as_ref()
                    .map(|r| (r.content.as_str(), r.projection.as_str())),
            "focused Audit actual write set incomplete"
        );
    }
    let changed = actual.iter().any(|write| write.changed);
    let revision = if changed {
        proof
            .base_revision
            .checked_add(1)
            .context("focused Audit revision exhausted")?
    } else {
        proof.base_revision
    };
    for write in actual.iter().filter(|write| write.changed) {
        let record = stored::read(tx, &write.key, &proof.scope.agent_id, &mut material)?
            .ok_or(CurrentReadError::CanonicalChanged)?;
        tx.execute("INSERT INTO current_read_members(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest,status,sort_seq,sort_at,created_at) VALUES(?1,?2,'event',?3,?4,'present',?5,?6,?7,?8,?9,?10) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,availability=excluded.availability,content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,status=excluded.status,sort_seq=excluded.sort_seq,sort_at=excluded.sort_at,created_at=excluded.created_at",
            params![proof.scope.agent_id,proof.scope.identity_incarnation,write.key.id,revision,record.content,record.projection,record.status,record.seq,record.sort_at,record.created_at])?;
        record_original_event_birth_tx(&proof, write, revision)?;
    }
    if changed {
        if let Some(saved) = before_saved {
            // Keep the original binding digest; no successor body is consulted.
            let mut coverage = saved.coverage;
            coverage.revision = revision;
            material.charge(
                usize::try_from(serialized_bytes(&coverage)?)?,
                coverage.observations.len() + 32,
            )?;
            let changed=tx.execute("UPDATE current_read_current SET revision=?1,coverage_json=?2,freshness='records_invalidated' WHERE agent_id=?3 AND identity_incarnation=?4 AND revision=?5",
                params![revision,serde_json::to_string(&coverage)?,proof.scope.agent_id,proof.scope.identity_incarnation,proof.base_revision])?;
            ensure!(changed == 1, CurrentReadError::RevisionMismatch);
        }
    }
    material.guards.append(&mut proof.guards);
    Ok(CommittedCurrentReadMaintenance {
        base_revision: proof.base_revision,
        revision,
        snapshot: None,
        _material: material.guards,
    })
}
