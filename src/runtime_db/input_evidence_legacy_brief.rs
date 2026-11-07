//! Exact proof for the historical linkage backfill that left the pre-link hash.
use super::*;

pub(super) fn validate_payload_digest(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    fingerprint: &Fingerprint,
    payload: &str,
    actual: &str,
) -> Result<()> {
    let strict = validate_canonical_payload_digest(key, fingerprint, actual);
    if strict.is_ok() || key.family != Family::Brief {
        return strict;
    }
    if proves_fixed_linkage_backfill(tx, agent, key, fingerprint, payload)? {
        return Ok(());
    }
    strict
}

fn proves_fixed_linkage_backfill(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    fingerprint: &Fingerprint,
    payload: &str,
) -> Result<bool> {
    let Some(hash) = fingerprint.content_hash.as_deref() else {
        return Ok(false);
    };
    let Ok(sequence) = fingerprint.revision_marker.parse::<u64>() else {
        return Ok(false);
    };
    if sequence == 0 {
        return Ok(false);
    }
    // The old migration serialized exactly this additive field. Hash the two
    // remaining slices; do not reserialize or normalize any substantive body.
    let marker = format!(",\"created_event_seq\":{sequence}");
    let mut offsets = payload
        .match_indices(&marker)
        .filter(|(at, _)| matches!(payload.as_bytes().get(at + marker.len()), Some(b',' | b'}')));
    let Some((at, _)) = offsets.next() else {
        return Ok(false);
    };
    if offsets.next().is_some() {
        return Ok(false);
    }
    let mut digest = Sha256::new();
    digest.update(&payload.as_bytes()[..at]);
    digest.update(&payload.as_bytes()[at + marker.len()..]);
    if format!("{:x}", digest.finalize()) != normalized_payload_digest(hash)? {
        return Ok(false);
    }
    // This is original fixed history only. Raw late rows and successor scopes
    // cannot borrow the old checksum even when their bodies look identical.
    let mut statement = tx.prepare_cached(
        "SELECT m.fingerprint_json FROM input_evidence_seed_membership m
         JOIN agent_episode_continuity b ON b.seed_id=m.seed_id
         JOIN agent_identities i ON i.agent_id=b.agent_id
          AND COALESCE(json_extract(i.payload_json,'$.incarnation'),1)=b.identity_incarnation
         WHERE b.agent_id=?1 AND b.mode='legacy_adopted' AND b.policy_version=1
          AND m.family='brief' AND m.record_id=?2",
    )?;
    let mut rows = statement.query(params![agent, key.id])?;
    let mut fixed = false;
    while let Some(row) = rows.next()? {
        let json = row.get_ref(0)?.as_str()?;
        ensure!(
            json.len() <= MAX_METADATA_BYTES,
            "fixed Brief fingerprint exceeds limit"
        );
        fixed |= serde_json::from_str::<Fingerprint>(json)?.same_metadata(fingerprint);
    }
    if !fixed {
        return Ok(false);
    }
    // The fixed marker must still name the creating event in the same scope.
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM briefs b
         JOIN audit_events e ON e.agent_id=b.agent_id AND e.event_seq=b.created_event_seq
         WHERE b.agent_id=?1 AND b.evidence_id=?2 AND b.created_event_seq=?3
          AND json_extract(b.payload_json,'$.created_event_seq')=b.created_event_seq
          AND e.kind='brief_created' AND json_extract(e.data_json,'$.data.brief_id')=b.evidence_id)",
        params![agent, key.id, i64::try_from(sequence)?], |row| row.get(0),
    )?)
}
