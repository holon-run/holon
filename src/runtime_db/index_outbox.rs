//! Runtime index outbox repository implementation.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::runtime_db::evidence::insert_runtime_index_changes_tx;
use crate::runtime_db::RuntimeDb;
use holon_storage::index_outbox::*;

/// Runtime index outbox repository.
pub struct RuntimeIndexOutboxRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub use holon_storage::index_outbox::{
    RuntimeIndexChange, RuntimeIndexOperation, RuntimeIndexOutboxRow,
};

impl RuntimeIndexOutboxRepository<'_> {
    pub fn append_changes(&self, changes: &[RuntimeIndexChange]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        self.db
            .transaction(|tx| insert_runtime_index_changes_tx(tx, changes))
    }

    /// Delete all runtime-index outbox rows for one agent while preserving its
    /// monotonic produced watermark.
    pub fn delete_all_for_agent(&self, agent_id: &str) -> Result<usize> {
        delete_all_for_agent_in(&self.db.connection()?, agent_id)
    }

    pub fn high_watermark(&self) -> Result<i64> {
        self.db
            .with_read(|connection| high_watermark_in(connection))
    }

    /// Return distinct agent ids that have at least one pending outbox row,
    /// ordered for deterministic processing.
    ///
    /// Can be served by the `idx_runtime_index_outbox_agent_seq` covering index,
    /// though the planner ultimately decides the access path.
    pub fn agent_ids_with_pending(&self) -> Result<Vec<String>> {
        self.db.with_read(agent_ids_with_pending_in)
    }

    pub(crate) async fn agent_ids_with_pending_async(&self) -> Result<Vec<String>> {
        self.db.with_read_async(agent_ids_with_pending_in).await
    }

    /// Distinct agent ids that ever produced an outbox change, including
    /// agents whose rows are fully consumed. Self-heal discovery uses this
    /// to find agents whose dirty marker or missing backfill checkpoints
    /// would otherwise have no consumer.
    pub fn agent_ids_with_watermarks(&self) -> Result<Vec<String>> {
        self.db.with_read(agent_ids_with_watermarks_in)
    }

    pub(crate) async fn agent_ids_with_watermarks_async(&self) -> Result<Vec<String>> {
        self.db.with_read_async(agent_ids_with_watermarks_in).await
    }

    pub fn high_watermark_for_agent(&self, agent_id: &str) -> Result<i64> {
        self.db
            .with_read(|connection| high_watermark_for_agent_in(connection, agent_id))
    }

    /// Monotonic produced watermark: the highest `change_seq` ever appended
    /// for the agent in this runtime database. Unlike
    /// [`Self::high_watermark_for_agent`], this does not fall back to 0 when
    /// consumed rows are deleted. Rows appended before the watermark table
    /// existed are still covered by taking the max with the current outbox.
    pub fn produced_watermark_for_agent(&self, agent_id: &str) -> Result<i64> {
        self.db
            .with_read(|connection| produced_watermark_for_agent_in(connection, agent_id))
    }

    /// Exact number of pending outbox rows for the agent. Sequence distance
    /// between cursors cannot substitute for this: `change_seq` is a global
    /// autoincrement shared across agents. Only rows above the agent's
    /// applied cursor count as pending; rows at or below it are already
    /// acknowledged and merely await GC.
    pub fn pending_count_for_agent(&self, agent_id: &str, after_change_seq: i64) -> Result<i64> {
        self.db.with_read(|connection| {
            let _query = crate::diagnostics::attribution::INDEX_PENDING_COUNT.start();
            pending_count_for_agent_in(connection, agent_id, after_change_seq)
        })
    }

    /// `created_at` of the oldest pending outbox row for the agent, the real
    /// propagation-delay anchor for index lag observability.
    pub fn oldest_pending_created_at_for_agent(
        &self,
        agent_id: &str,
        after_change_seq: i64,
    ) -> Result<Option<DateTime<Utc>>> {
        self.db.with_read(|connection| {
            let _query = crate::diagnostics::attribution::INDEX_PENDING_OLDEST.start();
            oldest_pending_created_at_for_agent_in(connection, agent_id, after_change_seq)
        })
    }

    pub fn read_after(
        &self,
        agent_id: &str,
        after_change_seq: i64,
        limit: usize,
    ) -> Result<Vec<RuntimeIndexOutboxRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db
            .with_read(|connection| read_after_in(connection, agent_id, after_change_seq, limit))
    }

    pub fn delete_through(&self, agent_id: &str, through_change_seq: i64) -> Result<usize> {
        self.db
            .transaction(|tx| delete_through_in(tx, agent_id, through_change_seq))
    }

    /// Best-effort GC for acknowledged rows at or below `through_change_seq`.
    /// Returns the number of rows removed without opening a write transaction
    /// when no such row exists. Callers use it after the applied cursor has
    /// already covered those rows: either directly after consumption, after a
    /// full rebuild jumped the cursor, or to compensate a crash between apply
    /// and delete.
    pub fn delete_acknowledged_through(
        &self,
        agent_id: &str,
        through_change_seq: i64,
    ) -> Result<usize> {
        let has_acknowledged_rows = self.db.with_read(|connection| {
            has_acknowledged_rows(connection, agent_id, through_change_seq)
        })?;
        if !has_acknowledged_rows {
            return Ok(0);
        }
        self.delete_through(agent_id, through_change_seq)
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn runtime_db() -> (tempfile::TempDir, RuntimeDb) {
        let dir = tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )
        .unwrap();
        (dir, db)
    }

    fn change(agent_id: &str, id: &str) -> RuntimeIndexChange {
        RuntimeIndexChange {
            agent_id: agent_id.into(),
            source_kind: "brief".into(),
            source_id: id.into(),
            source_ref: format!("brief:{id}"),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(Utc::now()),
            reason: "test_watermark".into(),
        }
    }

    #[test]
    fn produced_watermark_survives_outbox_gc_and_stays_monotonic() {
        let (_dir, db) = runtime_db();
        let outbox = db.runtime_index_outbox();
        outbox
            .append_changes(&[
                change("agent-a", "1"),
                change("agent-b", "2"),
                change("agent-a", "3"),
            ])
            .unwrap();
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 3);
        assert_eq!(outbox.produced_watermark_for_agent("agent-b").unwrap(), 2);
        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 2);
        assert_eq!(outbox.pending_count_for_agent("agent-b", 0).unwrap(), 1);

        // Draining an agent's outbox resets the row max but not the produced
        // watermark: `produced - applied` must stay meaningful after GC.
        outbox.delete_through("agent-a", 3).unwrap();
        assert_eq!(outbox.high_watermark_for_agent("agent-a").unwrap(), 0);
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 3);
        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 0);

        // Later appends only move the watermark forward.
        outbox.append_changes(&[change("agent-a", "4")]).unwrap();
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 4);
    }

    #[test]
    fn deleting_pending_rows_preserves_produced_watermark() {
        let (_dir, db) = runtime_db();
        let outbox = db.runtime_index_outbox();
        outbox
            .append_changes(&[
                change("agent-a", "1"),
                change("agent-b", "2"),
                change("agent-a", "3"),
            ])
            .unwrap();

        assert_eq!(outbox.delete_all_for_agent("agent-a").unwrap(), 2);
        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 0);
        assert_eq!(outbox.pending_count_for_agent("agent-b", 0).unwrap(), 1);
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 3);
        assert_eq!(outbox.delete_all_for_agent("agent-a").unwrap(), 0);
    }

    #[test]
    fn pending_count_and_oldest_pending_track_per_agent_backlog() {
        let (_dir, db) = runtime_db();
        let outbox = db.runtime_index_outbox();
        outbox
            .append_changes(&[change("agent-a", "1"), change("agent-b", "2")])
            .unwrap();

        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 1);
        assert_eq!(outbox.pending_count_for_agent("agent-c", 0).unwrap(), 0);
        let oldest = outbox
            .oldest_pending_created_at_for_agent("agent-a", 0)
            .unwrap()
            .expect("pending row has created_at");
        assert!(oldest <= Utc::now());
        assert_eq!(
            outbox
                .oldest_pending_created_at_for_agent("agent-c", 0)
                .unwrap(),
            None
        );

        // Rows at or below the applied cursor are acknowledged garbage, not
        // backlog, even before GC removes them.
        assert_eq!(outbox.pending_count_for_agent("agent-a", 1).unwrap(), 0);
        assert_eq!(
            outbox
                .oldest_pending_created_at_for_agent("agent-a", 1)
                .unwrap(),
            None
        );
        assert_eq!(outbox.delete_acknowledged_through("agent-a", 1).unwrap(), 1);
        assert_eq!(outbox.delete_acknowledged_through("agent-a", 1).unwrap(), 0);
    }
}
