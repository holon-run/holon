//! Original Episode/Pending loaders borrow this Input phase before decoding.
use super::*;
use crate::runtime_db::{
    bootstrap_inputs::{BootstrapPendingMetadata, PreparedPendingQueue},
    episode_snapshot::{
        EpisodeMemoryCurrentSnapshot, EpisodeMemoryLoadScope, EpisodeSnapshotError,
    },
};

// Keep the original Episode selector, including the look-ahead record used for
// has_more. Neither material accounting nor a Pending root grants membership.
const SELECTED_EPISODES: &str = "WITH selected AS (
    SELECT e.episode_id,e.agent_id,e.work_item_id,e.payload_json
    FROM context_episode_anchors e
    WHERE e.agent_id=?1 AND (
      EXISTS(SELECT 1 FROM episode_memory_results r
        WHERE r.agent_id=?1 AND r.identity_incarnation=?2
          AND r.phase_version=?3 AND r.finalized_episode_id=e.episode_id)
      OR EXISTS(SELECT 1 FROM agent_episode_continuity b
        JOIN episode_continuity_members m ON m.seed_id=b.seed_id
        WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1
          AND m.episode_id=e.episode_id))
    ORDER BY e.ended_at DESC,e.started_at DESC,e.episode_id ASC LIMIT ?4
)";

impl InputReadMaterial<'_> {
    fn domain_raw(&mut self, bytes: usize, rows: usize) -> Result<()> {
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Episode/Pending raw material");
        let bytes = bytes
            .checked_mul(8)
            .and_then(|n| rows.checked_mul(8 * 256).and_then(|m| n.checked_add(m)))
            .context(exhausted())?;
        self.observe_projected(
            u64::try_from(bytes)?,
            rows.checked_mul(8).context(exhausted())?,
        )
    }
    fn domain_nodes(&mut self, nodes: usize, copies: usize) -> Result<()> {
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Episode/Pending native material");
        let nodes = nodes.checked_mul(copies).context(exhausted())?;
        self.observe_projected(
            u64::try_from(nodes.checked_mul(256).context(exhausted())?)?,
            nodes,
        )
    }
    /// These are new DTO allocations by the original loader, distinct from the
    /// caller's held scope facts. Never inspect a successor State as this scope.
    fn domain_scope_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        original_recovery: bool,
    ) -> Result<()> {
        self.validate_accounting(scope, ledger)?;
        self.remaining_material_limits()?;
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))),0),COUNT(*) FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id], |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.domain_raw(bytes, rows)?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities r,json_tree(r.payload_json) j WHERE r.agent_id=?1",
            [&scope.agent_id], |r| r.get(0),
        )?;
        self.domain_nodes(nodes, 2)?;
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1
             AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1
                 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2
             AND (json_extract(payload_json,'$.status')='active'
                 OR (?3 AND json_extract(payload_json,'$.status')='deleting')))",
            params![scope.agent_id,scope.identity_incarnation,original_recovery], |r| r.get(0),
        )?;
        ensure!(current, InputEvidenceSnapshotError::RetiredScope);
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(control_revision AS BLOB))),0),COUNT(*) FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id], |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.domain_raw(bytes, rows)?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_states r,json_tree(r.payload_json) j WHERE r.agent_id=?1",
            [&scope.agent_id], |r| r.get(0),
        )?;
        self.domain_nodes(nodes, 2)
    }

    pub(in crate::runtime_db) fn episode_snapshot_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        recent_limit: usize,
        original_recovery: bool,
    ) -> Result<EpisodeMemoryCurrentSnapshot> {
        self.validate_accounting(scope, ledger)?;
        ensure!(
            recent_limit <= 512,
            EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "recent window count"
            }
        );
        self.domain_scope_tx(tx, scope, ledger, original_recovery)?;
        // Bound the SQL strings and scope copies before format!/to_owned. Their
        // guards remain in this original Input phase, without another ledger.
        let scratch = u64::try_from(scope.agent_id.len())?
            .checked_mul(8)
            .and_then(|n| n.checked_add(8192))
            .context("Episode query scratch overflow")?;
        self.observe_projected(scratch, 16)?;
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(b.mode AS BLOB))+COALESCE(length(CAST(b.seed_id AS BLOB)),0)
             +COALESCE(length(CAST(s.agent_id AS BLOB)),0)+COALESCE(length(CAST(s.boundary_id AS BLOB)),0)
             +COALESCE(length(CAST(s.source_kind AS BLOB)),0)+128),0),COUNT(*)
             FROM agent_episode_continuity b LEFT JOIN episode_continuity_seeds s ON s.seed_id=b.seed_id
             WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1",
            params![scope.agent_id,scope.identity_incarnation], |r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        self.domain_raw(bytes, rows)?;
        let episode_scope = EpisodeMemoryLoadScope {
            agent_id: scope.agent_id.clone(),
            identity_incarnation: scope.identity_incarnation,
        };
        // The original seal/scope gate precedes every selected Episode read.
        // Its temporary strings are covered by the continuity raw guard above.
        drop(crate::runtime_db::episode_snapshot::load_continuity_binding(tx, &episode_scope)?);
        let limit = i64::try_from(recent_limit + 1)?;
        let phase = crate::runtime_db::episode_memory::EPISODE_MEMORY_PHASE;
        let args = params![scope.agent_id, scope.identity_incarnation, phase, limit];
        let sql = format!("{SELECTED_EPISODES} SELECT COALESCE(SUM(length(CAST(episode_id AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(work_item_id AS BLOB)),0)+length(CAST(payload_json AS BLOB))),0),COUNT(*) FROM selected");
        let (bytes, rows): (usize, usize) =
            tx.query_row(&sql, args, |r| Ok((r.get(0)?, r.get(1)?)))?;
        drop(sql);
        self.domain_raw(bytes, rows)?;
        let sql = format!("{SELECTED_EPISODES} SELECT COALESCE(SUM(length(CAST(r.agent_id AS BLOB))+length(CAST(r.turn_id AS BLOB))+length(CAST(r.phase_version AS BLOB))+length(CAST(r.command_digest AS BLOB))+length(CAST(r.invocation_json AS BLOB))+length(CAST(r.finalized_episode_id AS BLOB))+length(CAST(r.result_json AS BLOB))+length(CAST(r.committed_at AS BLOB))),0),COUNT(*) FROM episode_memory_results r JOIN selected e ON e.episode_id=r.finalized_episode_id WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.phase_version=?3");
        let (bytes, rows): (usize, usize) =
            tx.query_row(&sql, args, |r| Ok((r.get(0)?, r.get(1)?)))?;
        drop(sql);
        self.domain_raw(bytes, rows)?;
        let sql = format!("{SELECTED_EPISODES} SELECT COALESCE(SUM(length(CAST(m.content_digest AS BLOB))),0),COUNT(*) FROM episode_continuity_members m JOIN selected e ON e.episode_id=m.episode_id JOIN agent_episode_continuity b ON b.seed_id=m.seed_id WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1");
        let (bytes, rows): (usize, usize) =
            tx.query_row(&sql, args, |r| Ok((r.get(0)?, r.get(1)?)))?;
        drop(sql);
        self.domain_raw(bytes, rows)?;
        // All raw guards exist before SQLite parses any selected Episode or
        // qualification receipt JSON, including every object-key allocation.
        let sql = format!("{SELECTED_EPISODES} SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM selected e,json_tree(e.payload_json) j");
        let nodes: usize = tx.query_row(&sql, args, |r| r.get(0))?;
        drop(sql);
        self.domain_nodes(nodes, 2)?;
        let sql = format!("{SELECTED_EPISODES} SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM episode_memory_results r JOIN selected e ON e.episode_id=r.finalized_episode_id,json_tree(r.invocation_json) j WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.phase_version=?3)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM episode_memory_results r JOIN selected e ON e.episode_id=r.finalized_episode_id,json_tree(r.result_json) j WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.phase_version=?3)");
        let nodes: usize = tx.query_row(&sql, args, |r| r.get(0))?;
        drop(sql);
        // Invocation is decoded both by the qualifier and original load_receipt;
        // the third copy covers that real overlap, not a second phase allowance.
        self.domain_nodes(nodes, 3)?;
        let loaded = if original_recovery {
            crate::runtime_db::episode_snapshot::load_episode_memory_for_input_recovery_in(
                tx,
                &episode_scope,
                recent_limit,
            )?
        } else {
            crate::runtime_db::episode_snapshot::load_episode_memory_snapshot_in(
                tx,
                &episode_scope,
                recent_limit,
                None,
            )?
        };
        loaded.require_current()
    }

    pub(in crate::runtime_db) fn pending_metadata_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        original_recovery: bool,
    ) -> Result<BootstrapPendingMetadata> {
        self.domain_scope_tx(tx, scope, ledger, original_recovery)?;
        if original_recovery {
            crate::runtime_db::bootstrap_inputs::pending_metadata_for_input_recovery_tx(
                tx, scope, ledger,
            )
        } else {
            crate::runtime_db::bootstrap_inputs::pending_metadata_tx(tx, scope, ledger)
        }
    }

    pub(in crate::runtime_db) fn pending_queue_inputs_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        original_recovery: bool,
    ) -> Result<PreparedPendingQueue> {
        self.domain_scope_tx(tx, scope, ledger, original_recovery)?;
        let (bytes, rows, missing): (usize, usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(q.message_id AS BLOB))+length(CAST(q.agent_id AS BLOB))+length(CAST(q.status AS BLOB))+length(CAST(q.payload_json AS BLOB))+COALESCE(length(CAST(m.message_id AS BLOB)),0)+COALESCE(length(CAST(m.agent_id AS BLOB)),0)+COALESCE(length(CAST(m.message_seq AS BLOB)),0)+COALESCE(length(CAST(m.payload_json AS BLOB)),0)),0),COUNT(*),COALESCE(SUM(CASE WHEN m.message_id IS NULL OR m.agent_id!=q.agent_id THEN 1 ELSE 0 END),0) FROM queue_entries q LEFT JOIN messages m ON m.message_id=q.message_id WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted')",
            [&scope.agent_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        )?;
        let payload_bytes: u64 = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(q.payload_json AS BLOB))+COALESCE(length(CAST(m.payload_json AS BLOB)),0)),0) FROM queue_entries q LEFT JOIN messages m ON m.message_id=q.message_id WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted')",
            [&scope.agent_id], |r| r.get(0),
        )?;
        // Preserve logical Accepted admission independently of temporary Input
        // material. This does not install or replace any PendingBodyCharge.
        ensure!(
            u64::try_from(rows)? <= ledger.limits().record_limit,
            "bootstrap pending queue exceeds input record limit"
        );
        ledger.preflight_actual_pending(scope, u64::try_from(rows)?, payload_bytes)?;
        ensure!(
            missing == 0,
            "canonical pending queue has no same-agent Message"
        );
        self.domain_raw(
            bytes,
            rows.checked_mul(2).context("pending row count overflow")?,
        )?;
        let nodes: usize = tx.query_row(
            "SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM queue_entries q,json_tree(q.payload_json) j WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted'))+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM queue_entries q JOIN messages m ON m.message_id=q.message_id,json_tree(m.payload_json) j WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted'))",
            [&scope.agent_id], |r| r.get(0),
        )?;
        self.domain_nodes(nodes, 2)?;
        if original_recovery {
            crate::runtime_db::bootstrap_inputs::pending_queue_inputs_for_input_recovery_tx(
                tx, scope, ledger,
            )
        } else {
            crate::runtime_db::bootstrap_inputs::pending_queue_inputs_tx(tx, scope, ledger)
        }
    }
}

#[cfg(test)]
#[path = "input_read_episode_pending_tests.rs"]
mod tests;
