//! The upgrade visits history incrementally; total history is not a read quota.

use super::*;
use std::cmp::Reverse;

pub(super) fn capture_episodes(
    tx: &Transaction<'_>,
    agent_id: &str,
    seed_id: &str,
    roots: &EpisodeContinuityRoots,
) -> Result<()> {
    // Only IDs/digests and the twelve recent candidates outlive each decoded row.
    // Sort only compact ID/digest metadata, never full historical payloads.
    let mut statement = tx.prepare(
        "SELECT e.episode_id, e.payload_json, EXISTS (
           SELECT 1 FROM episode_memory_results r WHERE r.finalized_episode_id=e.episode_id)
         FROM context_episode_anchors e WHERE e.agent_id=?1",
    )?;
    let mut rows = statement.query([agent_id])?;
    let mut candidates = BTreeMap::new();
    let mut recent = BTreeMap::new();
    let mut count = 0usize;
    let mut bytes = 0u64;
    while let Some(row) = rows.next()? {
        let id = row.get_ref(0)?.as_str()?;
        let payload = row.get_ref(1)?.as_str()?;
        crate::diagnostics::resource_io::admit_json_payload(payload)?;
        let episode = decode_context_episode_payload(payload)?;
        ensure!(
            episode.id == id && episode.agent_id == agent_id,
            "upgrade Episode columns and payload disagree"
        );
        count = count
            .checked_add(1)
            .context("upgrade Episode count overflow")?;
        bytes = bytes
            .checked_add(u64::try_from(payload.len())?)
            .context("upgrade Episode bytes overflow")?;
        if row.get::<_, bool>(2)? {
            continue;
        }
        let digest = CandidatePayloadDigest(Sha256::digest(payload.as_bytes()));
        candidates.insert(id.to_owned(), digest);
        let live_owner = episode
            .current_work_item_id
            .as_ref()
            .is_some_and(|id| roots.work_item_ids.contains(id));
        let explicit_ref = member_has_explicit_ref(roots, id)?;
        if live_owner || explicit_ref {
            insert_member(tx, seed_id, id, payload, live_owner, explicit_ref, false)?;
        }
        recent.insert(
            (
                Reverse(episode.finalized_at),
                Reverse(episode.created_at),
                episode.id,
            ),
            (
                member_content_digest(payload.as_bytes()),
                live_owner,
                explicit_ref,
            ),
        );
        if recent.len() > LEGACY_EPISODE_RECENT_CANDIDATES {
            recent.pop_last();
        }
    }
    tx.execute(
        "UPDATE episode_continuity_seeds SET candidate_payloads_json=?2 WHERE seed_id=?1",
        params![seed_id, serde_json::to_string(&candidates)?],
    )?;
    for ((_, _, id), (digest, live_owner, explicit_ref)) in recent {
        write_member(tx, seed_id, &id, &digest, live_owner, explicit_ref, true)?;
    }
    tracing::info!(
        agent_id,
        episode_count = count,
        payload_bytes = bytes,
        "captured upgrade Episode history incrementally"
    );
    Ok(())
}

fn insert_member(
    tx: &Transaction<'_>,
    seed: &str,
    id: &str,
    payload: &str,
    live_owner: bool,
    explicit_ref: bool,
    recent: bool,
) -> Result<()> {
    write_member(
        tx,
        seed,
        id,
        &member_content_digest(payload.as_bytes()),
        live_owner,
        explicit_ref,
        recent,
    )
}

fn write_member(
    tx: &Transaction<'_>,
    seed: &str,
    id: &str,
    digest: &[u8; 64],
    live_owner: bool,
    explicit_ref: bool,
    recent: bool,
) -> Result<()> {
    let mut reasons = Vec::new();
    if live_owner {
        reasons.push("live_owner");
    }
    if explicit_ref {
        reasons.push("explicit_live_ref");
    }
    if recent {
        reasons.push("explicit_upgrade_recent_candidate");
    }
    reasons.sort_unstable();
    tx.execute(
        "INSERT INTO episode_continuity_members
         (seed_id,capture_phase,episode_id,content_digest,qualification_json)
         VALUES (?1,'upgrade',?2,?3,?4)
         ON CONFLICT(seed_id,capture_phase,episode_id) DO UPDATE
         SET qualification_json=excluded.qualification_json",
        params![
            seed,
            id,
            std::str::from_utf8(digest)?,
            member_qualification_json(&reasons)?
        ],
    )?;
    Ok(())
}
