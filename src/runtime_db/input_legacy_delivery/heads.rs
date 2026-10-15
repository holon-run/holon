//! Accepted locators of the original Input family, never raw latest-row adoption.
use super::*;
use crate::runtime::resident_input_evidence::{
    LegacyDeliveryHeadObservation, LegacyDeliveryHeadSelector, LegacyDeliverySourceAvailability,
    LegacyDeliverySourceLocator,
};

/// Caller has the original scope owner and already validated Input/Work scope.
/// This copies only immutable SQL metadata. It never inspects State or bodies.
pub(in crate::runtime_db) fn initialize_scope_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
) -> Result<()> {
    ensure!(
        scope.identity_incarnation > 0,
        "LegacyDelivery scope must be positive"
    );
    let exact_binding:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1 AND seed_id IS ?3 AND ((mode='fresh_empty' AND seed_id IS NULL) OR (mode='legacy_adopted' AND seed_id IS NOT NULL)))",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,seed],|r|r.get(0))?;
    ensure!(
        exact_binding,
        "LegacyDelivery differs from its original continuity binding"
    );
    if let Some(seed) = seed {
        let sealed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM episode_continuity_seeds s WHERE s.seed_id=?1 AND s.agent_id=?2 AND (s.identity_incarnation IS NULL OR s.identity_incarnation=?3) AND (s.import_required=0 OR EXISTS(SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id=s.seed_id)))",params![seed,scope.agent_id,i64::try_from(scope.identity_incarnation)?],|r|r.get(0))?;
        ensure!(
            sealed,
            "LegacyDelivery original seed is unsealed or foreign"
        );
    }
    let previous:Option<bool>=tx.query_row("SELECT seed_id IS ?3 FROM input_legacy_delivery_scopes WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,seed],|r|r.get(0)).optional()?;
    if let Some(exact) = previous {
        ensure!(exact, "LegacyDelivery scope seed changed");
        return Ok(());
    }
    let fixed:Option<(bool,bool)>=tx.query_row("SELECT sealed,table_available FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/upgrade'",[],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (sealed, available) = fixed.context("LegacyDelivery original fixed capture missing")?;
    ensure!(sealed, "LegacyDelivery original fixed capture is unsealed");
    let incomplete:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import' AND sealed=0)",[],|r|r.get(0))?;
    ensure!(!incomplete, "LegacyDelivery original import is unsealed");
    // An actual original import version supersedes the fixed ID globally, even
    // if that write changed its Work/agent association. No raw row participates.
    tx.execute("INSERT INTO input_legacy_delivery_current_sources(agent_id,identity_incarnation,record_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest,source_revision,availability) SELECT ?1,?2,m.record_id,m.work_item_id,m.created_at,m.text_nonempty,m.fingerprint_json,m.content_digest,0,'present' FROM input_legacy_delivery_fixed_members m JOIN input_legacy_delivery_captures c USING(capture_id) WHERE c.sealed=1 AND m.agent_id=?1 AND (m.identity_incarnation=?2 OR (m.identity_incarnation IS NULL AND ?3 IS NOT NULL AND (m.seed_id=?3 OR (m.seed_id IS NULL AND m.capture_id='legacy_delivery/v1/actual_evidence_import' AND EXISTS(SELECT 1 FROM episode_continuity_seeds s WHERE s.seed_id=?3 AND s.source_kind IN ('legacy_identity_import','legacy_state_import')))))) AND (m.capture_id='legacy_delivery/v1/actual_evidence_import' OR (m.capture_id='legacy_delivery/v1/upgrade' AND NOT EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members imported JOIN input_legacy_delivery_captures ic USING(capture_id) WHERE imported.record_id=m.record_id AND imported.capture_id='legacy_delivery/v1/actual_evidence_import' AND ic.sealed=1))) ON CONFLICT(agent_id,identity_incarnation,record_id) DO NOTHING",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,seed])?;
    tx.execute("INSERT INTO input_legacy_delivery_scopes(agent_id,identity_incarnation,seed_id,namespace_available) VALUES(?1,?2,?3,?4)",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,seed,available])?;
    Ok(())
}

/// Scalar-only qualification before any selected body decode. Source digests
/// have a closed 64-byte shape and no late raw row is admitted by this lookup.
pub(in crate::runtime_db::input_evidence_snapshot) fn member_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3 AND availability='present')",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id],|r|r.get(0)).map_err(Into::into)
}
pub(in crate::runtime_db::input_evidence_snapshot) fn qualify_metadata_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
    current: &Fingerprint,
    mut admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<String> {
    let raw: Option<u64> = tx.query_row("SELECT 1024+length(CAST(record_id AS BLOB))+length(CAST(fingerprint_json AS BLOB))+length(CAST(content_digest AS BLOB)) FROM input_legacy_delivery_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3 AND availability='present'",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id],|r|r.get(0)).optional()?;
    let raw = raw.ok_or_else(|| {
        InputEvidenceSnapshotError::NeedsPreparation(format!(
            "LegacyDelivery {} has no exact accepted source",
            key.id
        ))
    })?;
    let raw = raw
        .checked_mul(8)
        .context("LegacyDelivery source raw overflow")?;
    ensure!(
        raw <= MAX_METADATA_BYTES as u64,
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery source raw")
    );
    admit(raw, 32)?;
    let nodes:usize=tx.query_row("SELECT 32+(SELECT COUNT(*) FROM json_tree(fingerprint_json)) FROM input_legacy_delivery_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3 AND availability='present'",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id],|r|r.get(0))?;
    let nodes = nodes
        .checked_mul(8)
        .context("LegacyDelivery source native overflow")?;
    let native = (nodes as u64)
        .checked_mul(256)
        .context("LegacyDelivery source native overflow")?;
    ensure!(
        nodes.checked_add(32).is_some_and(|n| n <= MAX_RECORDS)
            && raw
                .checked_add(native)
                .is_some_and(|n| n <= MAX_METADATA_BYTES as u64),
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery source native")
    );
    admit(native, nodes)?;
    let digest:Option<String>=tx.query_row("SELECT content_digest FROM input_legacy_delivery_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3 AND availability='present' AND json_extract(fingerprint_json,'$.content_hash') IS ?4 AND json_extract(fingerprint_json,'$.sequence') IS ?5 AND json_extract(fingerprint_json,'$.turn_index') IS ?6 AND json_extract(fingerprint_json,'$.created_at')=?7 AND json_extract(fingerprint_json,'$.owner_kind') IS ?8 AND json_extract(fingerprint_json,'$.owner_id') IS ?9 AND json_extract(fingerprint_json,'$.revision_marker')=?10 AND json_extract(fingerprint_json,'$.payload_bytes')=?11 AND json_extract(fingerprint_json,'$.related_turn_id') IS ?12",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id,current.content_hash,current.sequence.map(i64::try_from).transpose()?,current.turn_index.map(i64::try_from).transpose()?,current.created_at,current.owner_kind,current.owner_id,current.revision_marker,i64::try_from(current.payload_bytes)?,current.related_turn_id],|r|r.get(0)).optional()?;
    match digest {
        Some(digest) => {
            ensure!(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "LegacyDelivery source digest is corrupt"
            );
            Ok(digest)
        }
        None if member_tx(tx, scope, key)? => {
            Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into())
        }
        None => Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
            "LegacyDelivery {} has no exact accepted source",
            key.id
        ))
        .into()),
    }
}

/// Invoked after the actual alias readback, within that original whole ACK TX.
/// Fixed hydration alone keeps source revision zero and changes no business fact.
pub(in crate::runtime_db::input_evidence_snapshot) fn after_alias_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
    qualification: &str,
    present: bool,
    seed: Option<&str>,
) -> Result<()> {
    if key.family != Family::LegacyDelivery {
        return Ok(());
    }
    initialize_scope_tx(tx, scope, seed)?;
    if !present {
        tx.execute("UPDATE input_legacy_delivery_current_sources SET availability='pruned' WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id])?;
        return Ok(());
    }
    if qualification != "committed_writer" {
        return Ok(());
    }
    tx.execute("INSERT INTO input_legacy_delivery_current_sources(agent_id,identity_incarnation,record_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest,source_revision,availability) SELECT a.agent_id,a.identity_incarnation,a.record_id,r.work_item_id,r.created_at,length(CAST(json_extract(r.payload_json,'$.text') AS BLOB))>0,a.fingerprint_json,a.content_digest,a.revision,a.availability FROM input_evidence_current a JOIN delivery_summaries r ON r.evidence_id=a.record_id AND r.agent_id=a.agent_id WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.record_id=?3 AND a.family='legacy_delivery' AND a.availability='present' ON CONFLICT(agent_id,identity_incarnation,record_id) DO UPDATE SET work_item_id=excluded.work_item_id,created_at=excluded.created_at,text_nonempty=excluded.text_nonempty,fingerprint_json=excluded.fingerprint_json,content_digest=excluded.content_digest,source_revision=excluded.source_revision,availability=excluded.availability",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id])?;
    Ok(())
}

pub(in crate::runtime_db::input_evidence_snapshot) fn prune_fixed_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<Option<(u64, String)>> {
    // The original removal already proved its exact qualified key and deleted
    // its body. This SQL transfers the fixed fingerprint without decoding it.
    let digest:Option<String>=tx.query_row("INSERT INTO input_evidence_current(agent_id,identity_incarnation,family,record_id,revision,content_digest,fingerprint_json,availability,qualification,seed_id,attempt_turn_proof_json) SELECT s.agent_id,s.identity_incarnation,'legacy_delivery',s.record_id,1,s.content_digest,s.fingerprint_json,'pruned','legacy_prepared',b.seed_id,NULL FROM input_legacy_delivery_current_sources s JOIN input_legacy_delivery_scopes b USING(agent_id,identity_incarnation) WHERE s.agent_id=?1 AND s.identity_incarnation=?2 AND s.record_id=?3 AND s.availability='present' AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=s.agent_id AND a.identity_incarnation=s.identity_incarnation AND a.family='legacy_delivery' AND a.record_id=s.record_id) RETURNING content_digest",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id],|r|r.get(0)).optional()?;
    if digest.is_some() {
        tx.execute("UPDATE input_legacy_delivery_current_sources SET availability='pruned' WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.id])?;
    }
    Ok(digest.map(|digest| (1, digest)))
}
macro_rules! head_source_sql {
    ($selection:literal) => {
        concat!(
            r#"WITH selected AS (
SELECT record_id,content_digest,source_revision,availability,fingerprint_json,work_item_id,created_at
FROM input_legacy_delivery_current_sources
WHERE agent_id=?1 AND identity_incarnation=?2 AND work_item_id=?3
  AND (?4=0 OR text_nonempty=1)
ORDER BY created_at DESC,record_id DESC LIMIT 1)
"#,
            $selection
        )
    };
}
const HEAD_RAW_SQL:&str=head_source_sql!("SELECT 1024+length(CAST(record_id AS BLOB))+length(CAST(content_digest AS BLOB))+length(CAST(availability AS BLOB))+length(CAST(fingerprint_json AS BLOB)) FROM selected");
const HEAD_NATIVE_SQL: &str =
    head_source_sql!("SELECT 32+(SELECT COUNT(*) FROM json_tree(fingerprint_json)) FROM selected");
const HEAD_ASSOCIATION_SQL: &str = head_source_sql!("SELECT work_item_id=?3 AND json_extract(fingerprint_json,'$.owner_id')=?3 AND created_at=json_extract(fingerprint_json,'$.created_at') FROM selected");
const HEAD_ROW_SQL: &str =
    head_source_sql!("SELECT record_id,content_digest,source_revision,availability FROM selected");
/// Finite metadata source for a selected Work. Admission is the same original
/// Work/Input actor's cumulative material callback; this helper owns no ledger.
pub(in crate::runtime_db) fn observe_head_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    work: &str,
    selector: LegacyDeliveryHeadSelector,
    mut admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<LegacyDeliveryHeadObservation> {
    let nonempty = selector == LegacyDeliveryHeadSelector::LatestNonempty;
    let raw: Option<u64> = tx
        .query_row(
            HEAD_RAW_SQL,
            params![
                scope.agent_id,
                i64::try_from(scope.identity_incarnation)?,
                work,
                nonempty
            ],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        let available:bool=tx.query_row("SELECT namespace_available FROM input_legacy_delivery_scopes WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?],|r|r.get(0))?;
        return Ok(if available {
            LegacyDeliveryHeadObservation::Absent
        } else {
            LegacyDeliveryHeadObservation::Unavailable
        });
    };
    let raw = raw
        .checked_add(u64::try_from(
            scope
                .agent_id
                .len()
                .checked_add(work.len())
                .context("LegacyDelivery selector bytes overflow")?,
        )?)
        .and_then(|n| n.checked_mul(8))
        .context("LegacyDelivery head raw overflow")?;
    ensure!(
        raw <= MAX_METADATA_BYTES as u64,
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery head raw")
    );
    admit(raw, 32)?;
    let nodes: usize = tx.query_row(
        HEAD_NATIVE_SQL,
        params![
            scope.agent_id,
            i64::try_from(scope.identity_incarnation)?,
            work,
            nonempty
        ],
        |r| r.get(0),
    )?;
    let nodes = nodes
        .checked_mul(8)
        .context("LegacyDelivery head nodes overflow")?;
    let native = u64::try_from(nodes)?
        .checked_mul(256)
        .context("LegacyDelivery head native overflow")?;
    ensure!(
        nodes.checked_add(32).is_some_and(|n| n <= MAX_RECORDS)
            && raw
                .checked_add(native)
                .is_some_and(|n| n <= MAX_METADATA_BYTES as u64),
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery head native")
    );
    admit(native, nodes)?;
    let association: Option<bool> = tx.query_row(
        HEAD_ASSOCIATION_SQL,
        params![
            scope.agent_id,
            i64::try_from(scope.identity_incarnation)?,
            work,
            nonempty
        ],
        |row| row.get(0),
    )?;
    ensure!(
        association == Some(true),
        InputEvidenceSnapshotError::ProjectionChanged(format!(
            "LegacyDelivery accepted head association/time changed for {work}"
        ))
    );

    let (id, digest, revision, availability): (String, String, u64, String) = tx.query_row(
        HEAD_ROW_SQL,
        params![
            scope.agent_id,
            i64::try_from(scope.identity_incarnation)?,
            work,
            nonempty
        ],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "LegacyDelivery source digest is corrupt"
    );
    let availability = match availability.as_str() {
        "present" => LegacyDeliverySourceAvailability::Present,
        "pruned" => LegacyDeliverySourceAvailability::Pruned,
        _ => anyhow::bail!("invalid LegacyDelivery source availability"),
    };
    Ok(LegacyDeliveryHeadObservation::Present {
        source: LegacyDeliverySourceLocator {
            scope: scope.clone(),
            key: Key::new(Family::LegacyDelivery, id),
            source_revision: revision,
            content_digest: digest,
            availability,
        },
    })
}

#[cfg(test)]
#[path = "head_tests.rs"]
mod tests;
