use super::super::episode_snapshot::work_checkpoint::{self, WorkContinuityCheckpoint};
use super::*;

pub(in crate::runtime_db) fn capture_work_domain_upgrade_tx(tx: &Transaction<'_>) -> Result<()> {
    let mut statement =
        tx.prepare("SELECT seed_id,source_kind FROM episode_continuity_seeds ORDER BY seed_id")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (seed, kind) in rows {
        capture_work_domain_seed_tx(
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
            |row| row.get(0),
        )?;
        if imported {
            capture_work_domain_seed_tx(tx, &seed, "first_import")?;
        }
    }
    Ok(())
}

pub(super) fn capture_work_domain_seed_tx(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
) -> Result<()> {
    let checkpoint = work_checkpoint::load(tx, seed, phase)?;
    let json = serde_json::to_vec(&checkpoint)?;
    bound_metadata(checkpoint.record_digests.len(), json.len())?;
    let previous:Option<String>=tx.query_row("SELECT metadata_digest FROM work_domain_captures WHERE seed_id=?1 AND capture_phase=?2",params![seed,phase],|row|row.get(0)).optional()?;
    let expected = digest(&json);
    if let Some(previous) = previous {
        ensure!(previous == expected, "fixed Work manifest was replaced");
        return Ok(());
    }
    tx.execute(
        "INSERT INTO work_domain_captures(seed_id,capture_phase,metadata_digest) VALUES(?1,?2,?3)",
        params![seed, phase, expected],
    )?;
    for (typed, digest) in &checkpoint.record_digests {
        let key = checkpoint_key(typed)?;
        tx.execute("INSERT INTO work_domain_eligibility(seed_id,capture_phase,family,record_id,content_digest) VALUES(?1,?2,?3,?4,?5)",params![seed,phase,family_name(key.family),key.id,digest])?;
    }
    Ok(())
}

pub(in crate::runtime_db) fn capture_work_domain_seed_if_ready_tx(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
) -> Result<()> {
    let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_domain_captures')",[],|row|row.get(0))?;
    if ready {
        capture_work_domain_seed_tx(tx, seed, phase)?;
    }
    Ok(())
}

pub(super) fn checkpoint_key(value: &str) -> Result<WorkKey> {
    let (family, id) = value.split_once(':').context("invalid fixed Work key")?;
    ensure!(!id.is_empty(), "empty fixed Work ID");
    Ok(WorkKey::new(
        match family {
            "work_item" => WorkFamily::Work,
            "wait" => WorkFamily::Wait,
            "continuation" => WorkFamily::Frame,
            _ => anyhow::bail!("invalid fixed Work family"),
        },
        id,
    ))
}

pub(super) fn eligible_digest(
    tx: &Transaction<'_>,
    seed: Option<&str>,
    key: &WorkKey,
) -> Result<Option<String>> {
    let Some(seed) = seed else { return Ok(None) };
    let fixed = tx.query_row("SELECT content_digest FROM work_domain_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3 ORDER BY CASE capture_phase WHEN 'first_import' THEN 0 ELSE 1 END LIMIT 1",params![seed,family_name(key.family),key.id],|row|row.get(0)).optional()?;
    super::seed_wait_cleanup::digest_override_tx(tx, seed, key, fixed)
}

pub(super) fn manifests(
    tx: &Transaction<'_>,
    seed: Option<&str>,
) -> Result<Vec<WorkContinuityCheckpoint>> {
    let Some(seed) = seed else {
        return Ok(Vec::new());
    };
    let mut query=tx.prepare("SELECT capture_phase,metadata_digest FROM work_domain_captures WHERE seed_id=?1 ORDER BY CASE capture_phase WHEN 'first_import' THEN 1 ELSE 0 END")?;
    let phases = query
        .query_map([seed], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        !phases.is_empty(),
        "bound fixed Work manifest was not captured"
    );
    phases
        .into_iter()
        .map(|(phase, expected)| {
            let checkpoint = work_checkpoint::load(tx, seed, &phase)?;
            ensure!(
                digest(&serde_json::to_vec(&checkpoint)?) == expected,
                "fixed Work manifest changed"
            );
            Ok(checkpoint)
        })
        .collect()
}

pub(super) fn known_anywhere(tx: &Transaction<'_>, key: &WorkKey) -> Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM work_domain_records WHERE family=?1 AND record_id=?2 AND availability!='absent' UNION ALL SELECT 1 FROM work_domain_eligibility WHERE family=?1 AND record_id=?2)",params![family_name(key.family),key.id],|row|row.get(0)).map_err(Into::into)
}

pub(super) fn validate_member(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    key: &WorkKey,
    stored: &StoredRecord,
) -> Result<u64> {
    if let Some(alias) = alias(tx, scope, key)? {
        ensure!(
            alias.availability == WorkAvailability::Present,
            "Work ID is pruned or has an incompatible absence observation"
        );
        ensure!(
            alias.digest.as_deref() == Some(stored.digest().as_str())
                && alias.fingerprint.as_ref() == Some(&stored.fingerprint),
            "qualified Work changed outside its original transaction: {:?}",
            key
        );
        Ok(alias.revision)
    } else {
        let expected = eligible_digest(tx, seed, key)?
            .ok_or_else(|| WorkSnapshotError::NeedsPreparation(key.clone()))?;
        if expected != stored.digest() {
            return Err(WorkSnapshotError::CanonicalChanged(key.clone()).into());
        }
        Ok(1)
    }
}
