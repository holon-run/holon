//! Legacy associations are original cohort metadata, not current Task adoption.
use super::*;

pub(super) fn observe_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    fixed: &[super::super::episode_snapshot::task_checkpoint::TaskContinuityCheckpoint],
    mut charge: impl FnMut(usize, usize) -> Result<()>,
) -> Result<TaskRunningSourceCoverage> {
    let (Some(seed), Some(checkpoint)) = (
        seed,
        fixed
            .iter()
            .rev()
            .find(|item| item.query_inventory.is_some()),
    ) else {
        return Ok(Default::default());
    };
    let inventory = checkpoint
        .query_inventory
        .as_ref()
        .expect("selected fixed inventory");
    super::super::episode_snapshot::task_checkpoint::queries::validate(tx, inventory)?;
    ensure!(
        inventory.agent_id == scope.agent_id,
        "fixed Running source crossed original namespace"
    );
    // Both memberships belong to the original seed. A raw late Message or Task
    // cannot enter by sharing an ID with an unrelated accepted current record.
    const POOL: &str = "FROM task_query_fixed_messages f JOIN task_query_fixed_tasks t ON t.inventory_id=f.inventory_id AND t.task_id=f.source_task_id WHERE f.inventory_id=?1 AND f.running_created_at IS NOT NULL AND f.running_generation_digest IS NOT NULL AND EXISTS(SELECT 1 FROM input_evidence_seed_membership e WHERE e.seed_id=?2 AND e.family='message' AND e.record_id=f.evidence_id AND e.fingerprint_json=f.fingerprint_json) AND EXISTS(SELECT 1 FROM queue_entries q WHERE q.agent_id=?3 AND q.message_id=f.evidence_id AND q.status IN ('queued','interrupted','dequeued','interjected'))";
    let (count,bytes):(usize,usize)=tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(f.evidence_id AS BLOB))+length(CAST(f.content_digest AS BLOB))+length(CAST(f.source_task_id AS BLOB))+length(CAST(f.running_created_at AS BLOB))+length(CAST(f.running_generation_digest AS BLOB))+length(CAST(t.content_digest AS BLOB))+256),0) {POOL}"),params![inventory.id,seed,scope.agent_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    // Native strings/map slots are all bounded before SQLite returns a String.
    charge(
        bytes,
        count
            .checked_mul(8)
            .context("fixed Running metadata node overflow")?,
    )?;
    let mut result = TaskRunningSourceCoverage::default();
    let mut statement=tx.prepare(&format!("SELECT f.evidence_id,f.content_digest,f.source_task_id,f.running_created_at,f.running_generation_digest,t.content_digest {POOL} ORDER BY f.evidence_id"))?;
    let mut rows = statement.query(params![inventory.id, seed, scope.agent_id])?;
    while let Some(row) = rows.next()? {
        let (source_id, source_digest, task_id, created, generation, task_digest): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        );
        ensure!(
            [&source_digest, &generation, &task_digest].into_iter().all(
                |value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            ),
            "fixed Running association digest is invalid"
        );
        // Query inventory can include old optional history. Only the Task's
        // original qualified checkpoint is an association counterpart.
        if checkpoint.record_digests.get(&format!("task:{task_id}")) != Some(&task_digest) {
            continue;
        }
        let created_at =
            chrono::DateTime::parse_from_rfc3339(&created)?.with_timezone(&chrono::Utc);
        let observation = RunningSourceGeneration {
            source_id: source_id.clone(),
            source_digest,
            task_id,
            created_at,
            generation_digest: generation,
        };
        ensure!(
            result.sources.insert(source_id, observation).is_none(),
            "duplicate fixed Running association"
        );
    }
    ensure!(
        result.sources.len() <= count,
        "fixed Running pool changed during original read"
    );
    Ok(result)
}
