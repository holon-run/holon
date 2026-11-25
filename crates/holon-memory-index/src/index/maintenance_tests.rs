use super::*;
use crate::maintenance::{
    IndexOutboxChange, RebuildAdvance, RebuildPage, RebuildSourceRecord, RebuildSources,
    RebuildStart,
};
use crate::test_support::{maintenance_test_documents, maintenance_test_job};
use tempfile::tempdir;

struct ExistingRebuild;

impl RebuildSources for ExistingRebuild {
    fn start(&self, _: &str) -> Result<RebuildStart> {
        anyhow::bail!("fixture already has a persisted rebuild");
    }

    fn consume_initial_dirty_marker(&self, _: &RebuildJob) -> Result<()> {
        anyhow::bail!("resuming a rebuild must not consume a new dirty marker");
    }

    fn read_page(&self, _: RebuildPage<'_>) -> Result<Vec<RebuildSourceRecord>> {
        anyhow::bail!("maintenance phases must not fetch source pages");
    }
}

fn advance_existing_rebuild(index: &mut IndexStore, limit: usize) -> Result<bool> {
    match index.advance_rebuild(&ExistingRebuild, "default", limit)? {
        RebuildAdvance::Idle => Ok(false),
        RebuildAdvance::Pending => Ok(true),
        RebuildAdvance::ReadyToFinalize(job) => {
            index.finalize_rebuild(&job, "test-runtime", Utc::now())?;
            Ok(false)
        }
    }
}

#[test]
fn maintenance_legacy_finalize_cleans_in_slices_before_completion() -> Result<()> {
    let directory = tempdir()?;
    let mut index = IndexStore::open_in(directory.path(), WriterMetrics::default())?;
    let keys = maintenance_test_documents(&index, "default", 140)?;
    for number in 0..140 {
        index.enqueue_upsert(
            "default",
            "message",
            &number.to_string(),
            &format!("message:pending-{number:04}"),
        )?;
    }
    let job = maintenance_test_job(&index, "default", MEMORY_INDEX_REBUILD_PHASE_FINALIZE)?;
    for key in keys {
        index.connection.execute(
            "INSERT INTO memory_index_rebuild_seen VALUES (?1, ?2, ?3)",
            params![job.agent_id, job.generation, key],
        )?;
    }
    assert!(advance_existing_rebuild(&mut index, 7)?);
    let progress = index.rebuild_job("default")?.unwrap();
    assert_eq!(progress.phase, MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING);
    assert!(!progress.source_cursor.is_empty());
    assert_eq!(index.pending_sources_for_agent("default")?.len(), 133);
    index.enqueue_upsert("default", "message", "new", "message:aaa-new")?;
    index.connection.execute(
        "UPDATE memory_index_pending_sources SET enqueued_at = ?1
         WHERE source_ref = 'message:aaa-new'",
        [(job.started_at + chrono::Duration::seconds(1)).to_rfc3339()],
    )?;
    drop(index);
    let mut index = IndexStore::open_in(directory.path(), WriterMetrics::default())?;
    assert_eq!(
        index.rebuild_job("default")?.unwrap().source_cursor,
        progress.source_cursor
    );
    for _ in 0..128 {
        if !advance_existing_rebuild(&mut index, 7)? {
            break;
        }
    }
    assert!(index.rebuild_job("default")?.is_none());
    let pending = index.pending_sources_for_agent("default")?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_ref, "message:aaa-new");
    let seen: usize = index.connection.query_row(
        "SELECT COUNT(*) FROM memory_index_rebuild_seen",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(seen, 0);
    Ok(())
}

#[test]
fn maintenance_prune_bounds_seen_candidates_and_resumes_repair() -> Result<()> {
    let directory = tempdir()?;
    let mut index = IndexStore::open_in(directory.path(), WriterMetrics::default())?;
    let keys = maintenance_test_documents(&index, "default", 140)?;
    maintenance_test_documents(&index, "default-extra", 3)?;
    let job = maintenance_test_job(&index, "default", MEMORY_INDEX_REBUILD_PHASE_PRUNE)?;
    for key in &keys[..139] {
        index.connection.execute(
            "INSERT INTO memory_index_rebuild_seen VALUES (?1, ?2, ?3)",
            params![job.agent_id, job.generation, key],
        )?;
    }
    index.connection.execute(
        "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&keys[139]],
    )?;
    index.connection.execute(
        "INSERT INTO memory_documents_fts (document_key, title, body, sanitized_excerpt)
         VALUES (?1, 'duplicate', 'duplicate', 'duplicate')",
        [&keys[139]],
    )?;
    assert_eq!(index.prune_rebuild_slice(&job, 3)?, 3);
    assert_eq!(
        index.rebuild_job("default")?.unwrap().source_cursor,
        keys[2]
    );
    drop(index);
    let mut index = IndexStore::open_in(directory.path(), WriterMetrics::default())?;
    let resumed = index.rebuild_job("default")?.unwrap();
    assert_eq!(index.prune_rebuild_slice(&resumed, 3)?, 3);
    assert_eq!(
        index.rebuild_job("default")?.unwrap().source_cursor,
        keys[5]
    );
    for _ in 0..256 {
        if !advance_existing_rebuild(&mut index, 3)? {
            break;
        }
    }
    assert!(index.rebuild_job("default")?.is_none());
    for table in [
        "memory_documents",
        "memory_documents_fts",
        "memory_documents_fts_rows",
    ] {
        let count: usize = index.connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE document_key = ?1"),
            [&keys[139]],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table} retains stale broken-map document");
    }
    let remaining: usize =
        index
            .connection
            .query_row("SELECT COUNT(*) FROM memory_documents", [], |row| {
                row.get(0)
            })?;
    assert_eq!(remaining, 142);
    Ok(())
}

#[test]
fn outbox_batches_stop_at_failed_preparation_and_retry_from_committed_cursor() -> Result<()> {
    let directory = tempdir()?;
    let mut index = IndexStore::open_in(directory.path(), WriterMetrics::default())?;
    let changes = || {
        [3, 7, 12].into_iter().map(|change_seq| {
            let source_ref = format!("message:{change_seq}");
            IndexOutboxChange {
                change_seq,
                source: PendingSource {
                    document_key: document_key_for("default", &source_ref),
                    source_ref,
                    source_kind: "message".into(),
                    source_id: change_seq.to_string(),
                    operation: "delete".into(),
                },
            }
        })
    };
    let first =
        index.apply_outbox_prefix(
            "runtime",
            "default",
            0,
            &mut changes(),
            &|source| match source.source_id.as_str() {
                "3" => Ok(PreparedPendingSource::Delete),
                "7" => anyhow::bail!("test preparation failed"),
                _ => panic!("consumer must stop before the next row"),
            },
        );
    assert_eq!(first.acknowledged_through, 3);
    let error = format!("{:#}", first.error.expect("failed preparation"));
    assert!(error.contains("change_seq=7"));
    assert!(error.contains("test preparation failed"));
    assert_eq!(
        index.cursor("runtime", "default", MEMORY_INDEX_OUTBOX_CURSOR)?,
        3
    );

    let retry = index.apply_outbox_prefix(
        "runtime",
        "default",
        3,
        &mut changes().filter(|row| row.change_seq > 3),
        &|_| Ok(PreparedPendingSource::Delete),
    );
    assert!(retry.error.is_none());
    assert_eq!(retry.acknowledged_through, 12);
    assert_eq!(
        index.cursor("runtime", "default", MEMORY_INDEX_OUTBOX_CURSOR)?,
        12
    );
    Ok(())
}
