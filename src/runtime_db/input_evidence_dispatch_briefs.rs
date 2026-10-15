//! Read-only D4 closure observations from the existing D3 qualified namespace.

use super::*;

/// Uses the same RecentBriefs selector, scope, fixed eligibility and raw digest
/// rules as D3 preparation. It does not hydrate aliases or expand source closure.
pub(crate) fn qualified_dispatch_briefs_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    limit: usize,
    byte_limit: usize,
) -> Result<Vec<BriefRecord>> {
    ensure!(
        limit <= 64 && byte_limit > 0,
        "invalid dispatch Brief observation budget"
    );
    require_original_input_recovery_scope(tx, scope)?;
    let binding = binding_on(tx, scope)?;
    let keys = qualified_window_keys(
        tx,
        scope,
        &binding,
        &InputWindowSelector::RecentBriefs,
        limit,
        None,
    )?;
    let mut bytes = 0usize;
    let mut result = Vec::with_capacity(keys.len());
    for key in keys {
        let fingerprint = fingerprint_on(tx, &scope.agent_id, &key)?
            .context("qualified dispatch Brief disappeared")?;
        let alias = alias_on(tx, scope, &key)?;
        let fixed_digests = if let Some(alias) = &alias {
            ensure!(alias.present, "pruned dispatch Brief reappeared");
            if alias.fingerprint != fingerprint {
                return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into());
            }
            BTreeSet::new()
        } else {
            require_eligible_metadata(tx, &binding, &key, &fingerprint)?
        };
        if fingerprint.payload_bytes > byte_limit.min(MAX_METADATA_BYTES).saturating_sub(bytes) {
            return Err(
                InputEvidenceSnapshotError::ResourceExhausted("dispatch Brief body bytes").into(),
            );
        }
        let payload = payload_on(tx, &scope.agent_id, &key)?
            .context("qualified dispatch Brief payload disappeared")?;
        let actual_digest = bytes_digest(payload.as_bytes());
        validate_canonical_payload_digest(&key, &fingerprint, &actual_digest)?;
        if let Some(alias) = alias {
            ensure!(
                alias.digest.as_deref() == Some(actual_digest.as_str()),
                "dispatch Brief raw/normalized digest mismatch"
            );
        } else if requires_frozen_body_digest(&key, &fingerprint) {
            validate_fixed_body_digest(&key, &fixed_digests, &actual_digest)?;
        }
        let record = prepare_record_on(
            tx,
            &scope.agent_id,
            &key,
            Some(decode_canonical(&key, &payload)?),
        )?;
        let PreparedInputEvidence::Brief(brief) = record else {
            anyhow::bail!("dispatch Brief selector returned another family")
        };
        bytes = bytes.saturating_add(payload.len());
        result.push(brief);
    }
    // Match read_recent_briefs: chronological order among the selected tail.
    result.reverse();
    Ok(result)
}

/// Metadata-only sizing of the exact existing qualified selector. The actual
/// reader still performs canonical/raw digest validation before using bodies.
pub(crate) fn qualified_dispatch_brief_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    limit: usize,
) -> Result<(usize, usize)> {
    ensure!(limit <= 64, "invalid dispatch Brief observation limit");
    require_original_input_recovery_scope(tx, scope)?;
    let binding = binding_on(tx, scope)?;
    let keys = qualified_window_keys(
        tx,
        scope,
        &binding,
        &InputWindowSelector::RecentBriefs,
        limit,
        None,
    )?;
    let mut bytes = 0usize;
    for key in &keys {
        let fingerprint = fingerprint_on(tx, &scope.agent_id, key)?
            .context("qualified dispatch Brief disappeared")?;
        bytes = bytes.checked_add(fingerprint.payload_bytes).context(
            InputEvidenceSnapshotError::ResourceExhausted("dispatch Brief body bytes"),
        )?;
        ensure!(
            bytes <= MAX_METADATA_BYTES,
            InputEvidenceSnapshotError::ResourceExhausted("dispatch Brief body bytes")
        );
    }
    Ok((keys.len(), bytes))
}
