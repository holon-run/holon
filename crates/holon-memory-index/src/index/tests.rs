use super::test_support::maintenance_test_documents;
use super::*;
use std::fs;
use std::sync::{mpsc, Barrier};
use tempfile::tempdir;

#[test]
fn outbox_cursor_failure_rolls_back_the_document_and_source_state() -> Result<()> {
    let directory = tempdir()?;
    let index = IndexStore::open_in(directory.path(), WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:pending".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "pending document".into(),
        body: "pending projection".into(),
        sanitized_excerpt: "pending".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    let source = PendingSource {
        document_key: document_key(&document),
        source_ref: document.source_ref.clone(),
        source_kind: document.source_kind.clone(),
        source_id: "pending".into(),
        operation: "upsert".into(),
    };
    let prepared = PreparedPendingSource::Upsert(document);
    index.connection.execute_batch(
        "CREATE TEMP TRIGGER reject_outbox_cursor BEFORE INSERT ON memory_index_cursors
             BEGIN SELECT RAISE(ABORT, 'test cursor failure'); END;",
    )?;

    let error = index
        .apply_outbox_change("runtime", "default", &source, &prepared, 1)
        .expect_err("projection and cursor must commit together");
    assert!(error.to_string().contains("test cursor failure"));
    assert_eq!(
        index.cursor("runtime", "default", MEMORY_INDEX_OUTBOX_CURSOR)?,
        0
    );
    assert!(index
        .document_hash("default", &source.source_ref)?
        .is_none());
    let state_count: usize = index.connection.query_row(
        "SELECT count(*) FROM memory_index_source_state WHERE document_key = ?1",
        [&source.document_key],
        |row| row.get(0),
    )?;
    assert_eq!(state_count, 0);

    index
        .connection
        .execute_batch("DROP TRIGGER reject_outbox_cursor")?;
    index.apply_outbox_change("runtime", "default", &source, &prepared, 1)?;
    assert_eq!(
        index.cursor("runtime", "default", MEMORY_INDEX_OUTBOX_CURSOR)?,
        1
    );
    assert!(index
        .document_hash("default", &source.source_ref)?
        .is_some());
    Ok(())
}

#[test]
fn maintenance_delete_is_sliced_retryable_and_repairs_broken_maps() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let keys = maintenance_test_documents(&index, "deleted", 140)?;
    let peers = maintenance_test_documents(&index, "deleted-extra", 3)?;
    let peer_rowid: i64 = index.connection.query_row(
        "SELECT fts_rowid FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&peers[0]],
        |row| row.get(0),
    )?;
    index.connection.execute(
        "DELETE FROM memory_documents_fts_rows WHERE document_key IN (?1, ?2)",
        params![keys[0], peers[0]],
    )?;
    index.connection.execute(
        "UPDATE memory_documents_fts_rows SET fts_rowid = ?1 WHERE document_key = ?2",
        params![peer_rowid, keys[1]],
    )?;
    index.connection.execute(
        "INSERT INTO memory_documents_fts (document_key, title, body, sanitized_excerpt)
             VALUES (?1, 'duplicate', 'duplicate', 'duplicate')",
        [&keys[2]],
    )?;
    let next = index.delete_agent_documents_slice("deleted", "")?.unwrap();
    assert!(next >= keys[0]);
    let remaining: usize = index.connection.query_row(
        "SELECT COUNT(*) FROM memory_documents WHERE agent_id = 'deleted'",
        [],
        |row| row.get(0),
    )?;
    assert!(remaining >= 140 - MEMORY_INDEX_MAINTENANCE_BATCH);
    index.connection.execute_batch(
        "CREATE TRIGGER fail_agent_cleanup BEFORE DELETE ON memory_documents
             WHEN OLD.document_key = 'deleted:message:maintenance-0099'
             BEGIN SELECT RAISE(ABORT, 'injected cleanup failure'); END;",
    )?;
    assert!(index.delete_agent_projection("deleted").is_err());
    index
        .connection
        .execute_batch("DROP TRIGGER fail_agent_cleanup")?;
    drop(index);
    IndexStore::open_in(index_dir, WriterMetrics::default())?.delete_agent_projection("deleted")?;
    IndexStore::open_in(index_dir, WriterMetrics::default())?.delete_agent_projection("deleted")?;
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    for table in [
        "memory_documents",
        "memory_index_source_state",
        "memory_documents_fts",
        "memory_documents_fts_rows",
    ] {
        let count: usize = index.connection.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE document_key >= 'deleted:' AND document_key < 'deleted;'"),
                [], |row| row.get(0),
            )?;
        assert_eq!(count, 0, "{table} retains deleted projection");
    }
    for table in [
        "memory_documents",
        "memory_index_source_state",
        "memory_documents_fts",
    ] {
        let count: usize = index.connection.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE document_key >= 'deleted-extra:' AND document_key < 'deleted-extra;'"),
                [], |row| row.get(0),
            )?;
        assert_eq!(count, 3, "{table} changed a peer");
    }
    Ok(())
}

#[test]
fn maintenance_delete_yields_to_queued_foreground() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    maintenance_test_documents(&index, "deleted", 140)?;
    index.connection.execute_batch(
        "CREATE TABLE test_writer_order (kind TEXT NOT NULL);
             CREATE TRIGGER foreground_order AFTER INSERT ON memory_index_pending_sources
             BEGIN INSERT INTO test_writer_order VALUES ('foreground'); END;
             CREATE TRIGGER maintenance_order AFTER DELETE ON memory_documents
             BEGIN INSERT INTO test_writer_order VALUES ('maintenance'); END;",
    )?;
    let mut foreground = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let maintenance = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let coordinator = Arc::clone(&index.write_coordinator);
    // Stall the first real maintenance transaction at SQLite admission,
    // then queue foreground work while that maintenance turn is active.
    let blocker = Connection::open(index_dir.join(INDEX_FILENAME))?;
    blocker.execute_batch("BEGIN IMMEDIATE")?;
    let maintenance_thread = thread::spawn(move || maintenance.delete_agent_projection("deleted"));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !coordinator.active_for_test() {
        assert!(Instant::now() < deadline, "maintenance failed to start");
        thread::sleep(Duration::from_millis(1));
    }
    let foreground_thread = thread::spawn(move || {
        foreground.enqueue_upsert("peer", "message", "queued", "message:queued")
    });
    while coordinator.queued_for_test(MemoryIndexWriteClass::Foreground) == 0 {
        assert!(Instant::now() < deadline, "foreground failed to queue");
        thread::sleep(Duration::from_millis(1));
    }
    blocker.execute_batch("COMMIT")?;
    foreground_thread
        .join()
        .expect("foreground thread panicked")?;
    maintenance_thread
        .join()
        .expect("maintenance thread panicked")?;
    let order = index
        .connection
        .prepare("SELECT kind FROM test_writer_order ORDER BY rowid")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let foreground_position = order.iter().position(|kind| kind == "foreground").unwrap();
    assert!(foreground_position > 0);
    assert!(foreground_position <= MEMORY_INDEX_MAINTENANCE_BATCH);
    assert_eq!(order.last().map(String::as_str), Some("maintenance"));
    assert_eq!(
        order.iter().filter(|kind| *kind == "maintenance").count(),
        140
    );
    Ok(())
}

#[test]
fn maintenance_candidate_pages_use_key_and_fts_rowid_ranges() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let keys = maintenance_test_documents(&index, "default", 8192)?;
    index.connection.execute(
            "INSERT INTO memory_index_pending_sources
                 (document_key, source_ref, agent_id, source_kind, source_id, operation, enqueued_at)
             SELECT document_key, source_ref, agent_id, source_kind, source_ref, 'upsert', 'now'
             FROM memory_documents",
            [],
        )?;
    for table in [
        "memory_documents",
        "memory_index_source_state",
        "memory_index_pending_sources",
        "memory_documents_fts_rows",
    ] {
        let mut steps = Vec::new();
        for cursor in ["", keys[8091].as_str()] {
            let (sql, bound) = agent_document_keys_page_query(table, "default", cursor);
            let mut statement = index.connection.prepare(&sql)?;
            let page = statement
                .query_map(
                    params![bound, "default;", MEMORY_INDEX_MAINTENANCE_BATCH],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(page.len(), MEMORY_INDEX_MAINTENANCE_BATCH);
            steps.push(statement.get_status(rusqlite::StatementStatus::VmStep));
            assert_eq!(
                page,
                agent_document_keys_page(&index.connection, table, "default", cursor, 64)?,
            );
            let plan = index
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
                .query_map(
                    params![bound, "default;", MEMORY_INDEX_MAINTENANCE_BATCH],
                    |row| row.get::<_, String>(3),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(
                plan.iter()
                    .any(|line| line.contains("SEARCH") && line.contains("document_key")),
                "{table}: {plan:?}"
            );
            assert!(
                plan.iter().all(|line| !line.contains("TEMP B-TREE")),
                "{table}: {plan:?}"
            );
        }
        assert!(
            steps[1] <= steps[0] * 2,
            "{table} late-page work must not grow with the retained prefix: {steps:?}"
        );
    }
    let plan = index
        .connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT rowid, document_key FROM memory_documents_fts
             WHERE rowid > 100 ORDER BY rowid LIMIT 64",
        )?
        .query_map([], |row| row.get::<_, String>(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(plan.iter().any(|line| line.contains('>')), "{plan:?}");
    Ok(())
}

#[test]
fn existing_reader_open_does_not_reapply_schema_or_allow_writes() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    drop(IndexStore::open_in(index_dir, WriterMetrics::default())?);

    let connection = Connection::open(index_dir.join(INDEX_FILENAME))?;
    connection.execute("DROP TABLE memory_index_rebuild_seen", [])?;
    drop(connection);

    let reader = IndexStore::open_existing_reader(
        &index_dir.join(INDEX_FILENAME),
        WriterMetrics::default(),
    )?;
    assert!(!reader.table_exists("memory_index_rebuild_seen")?);
    let error = reader
        .enqueue_source(
            "default",
            "message",
            "msg-reader",
            "message:msg-reader",
            "upsert",
            None,
            "test",
        )
        .expect_err("reader connection must reject index writes");
    assert!(error
        .to_string()
        .contains("cannot write through read-only memory index connection"));
    Ok(())
}

#[test]
fn unchanged_document_upsert_preserves_fts_row() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:stable".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "stable title".into(),
        body: "stable body".into(),
        sanitized_excerpt: "stable excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    index.upsert_document(&document)?;
    index.upsert_document(&MemoryDocument {
        source_ref: "message:sentinel".into(),
        ..document.clone()
    })?;

    let document_key = document_key(&document);
    let rowid_before: i64 = index.connection.query_row(
        "SELECT rowid FROM memory_documents_fts WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;
    upsert_document_if_needed_tx(&index.connection, &document)?;
    let rowid_after: i64 = index.connection.query_row(
        "SELECT rowid FROM memory_documents_fts WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;

    assert_eq!(rowid_after, rowid_before);
    Ok(())
}

#[test]
fn changed_document_upsert_replaces_mapped_fts_row() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:changed".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "old title".into(),
        body: "old body".into(),
        sanitized_excerpt: "old excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    index.upsert_document(&document)?;
    index.upsert_document(&MemoryDocument {
        source_ref: "message:sentinel".into(),
        ..document.clone()
    })?;
    let document_key = document_key(&document);
    let old_rowid: i64 = index.connection.query_row(
        "SELECT fts_rowid FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;

    index.upsert_document(&MemoryDocument {
        title: "new title".into(),
        body: "new body".into(),
        sanitized_excerpt: "new excerpt".into(),
        ..document
    })?;

    let new_rowid: i64 = index.connection.query_row(
        "SELECT fts_rowid FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;
    let old_row_count: i64 = index.connection.query_row(
        "SELECT COUNT(*) FROM memory_documents_fts WHERE rowid = ?1",
        [old_rowid],
        |row| row.get(0),
    )?;
    let new_title: String = index.connection.query_row(
        "SELECT title FROM memory_documents_fts WHERE rowid = ?1",
        [new_rowid],
        |row| row.get(0),
    )?;

    assert_ne!(new_rowid, old_rowid);
    assert_eq!(old_row_count, 0);
    assert_eq!(new_title.trim_end(), "new title");

    index.delete_document("default", "message:changed")?;
    let mapped_row_count: i64 = index.connection.query_row(
        "SELECT COUNT(*) FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;
    let current_row_count: i64 = index.connection.query_row(
        "SELECT COUNT(*) FROM memory_documents_fts WHERE rowid = ?1",
        [new_rowid],
        |row| row.get(0),
    )?;
    assert_eq!(mapped_row_count, 0);
    assert_eq!(current_row_count, 0);
    Ok(())
}

#[test]
fn opening_existing_index_backfills_fts_row_map() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:legacy".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "legacy title".into(),
        body: "legacy body".into(),
        sanitized_excerpt: "legacy excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    index.upsert_document(&document)?;
    index
        .connection
        .execute("DROP TABLE memory_documents_fts_rows", [])?;
    drop(index);

    let reopened = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document_key = document_key(&document);
    let mapped_rowid: i64 = reopened.connection.query_row(
        "SELECT fts_rowid FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;
    let fts_rowid: i64 = reopened.connection.query_row(
        "SELECT rowid FROM memory_documents_fts WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;

    assert_eq!(mapped_rowid, fts_rowid);
    Ok(())
}

#[test]
fn fts_row_map_schema_creation_rolls_back_after_population_failure() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:schema-rollback".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "schema rollback title".into(),
        body: "schema rollback body".into(),
        sanitized_excerpt: "schema rollback excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    index.upsert_document(&document)?;
    let document_key = document_key(&document);
    index.connection.execute(
        "INSERT INTO memory_documents_fts
                 (document_key, title, body, sanitized_excerpt)
             SELECT document_key, title, body, sanitized_excerpt
             FROM memory_documents_fts
             WHERE document_key = ?1",
        [&document_key],
    )?;
    index
        .connection
        .execute("DROP TABLE memory_documents_fts_rows", [])?;

    index
        .ensure_schema_until(Instant::now() + MEMORY_INDEX_TRANSACTION_DEADLINE)
        .expect_err("duplicate document keys should fail row map population");

    assert!(index.connection.is_autocommit());
    assert!(!index.table_exists("memory_documents_fts_rows")?);

    index.connection.execute(
        "DELETE FROM memory_documents_fts
             WHERE rowid = (
                 SELECT MAX(rowid) FROM memory_documents_fts WHERE document_key = ?1
             )",
        [&document_key],
    )?;
    index.ensure_schema_until(Instant::now() + MEMORY_INDEX_TRANSACTION_DEADLINE)?;

    let mapped_count: i64 = index.connection.query_row(
        "SELECT COUNT(*) FROM memory_documents_fts_rows WHERE document_key = ?1",
        [&document_key],
        |row| row.get(0),
    )?;
    assert_eq!(mapped_count, 1);
    Ok(())
}

#[test]
fn memory_index_agent_cleanup_queries_use_agent_indexes() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;

    for (table, index_name) in [
        ("memory_documents", "idx_memory_documents_agent"),
        (
            "memory_index_source_state",
            "idx_memory_index_source_state_agent",
        ),
    ] {
        let details = index
            .connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN DELETE FROM {table} WHERE agent_id = 'default'"
            ))?
            .query_map([], |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            details.iter().any(|detail| detail.contains(index_name)),
            "{table} cleanup should use {index_name}, got {details:?}"
        );
    }

    Ok(())
}

#[test]
fn deleting_agent_cleans_in_flight_rebuild_metadata() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    index.connection.execute(
        "INSERT INTO memory_index_rebuild_jobs (
                agent_id, generation, phase, source_kind_index, source_cursor, source_offset,
                runtime_high_watermark, documents_processed, started_at, last_progress_at
             ) VALUES ('deleted-agent', 'generation-1', 'scan', 0, '', 0, 0, 0, 'now', 'now'),
                    ('retained-agent', 'generation-2', 'scan', 0, '', 0, 0, 0, 'now', 'now')",
        [],
    )?;
    index.connection.execute(
        "INSERT INTO memory_index_rebuild_seen (
                agent_id, generation, document_key
             ) VALUES ('deleted-agent', 'generation-1', 'deleted-agent:message:old'),
                    ('retained-agent', 'generation-2', 'retained-agent:message:keep')",
        [],
    )?;
    drop(index);

    IndexStore::open_in(index_dir, WriterMetrics::default())?
        .delete_agent_projection("deleted-agent")?;

    let connection = rusqlite::Connection::open(index_dir.join(INDEX_FILENAME))?;
    for table in ["memory_index_rebuild_jobs", "memory_index_rebuild_seen"] {
        let deleted_count: i64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = 'deleted-agent'"),
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            deleted_count, 0,
            "{table} must not retain deleted agent rows"
        );

        let retained_count: i64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = 'retained-agent'"),
            [],
            |row| row.get(0),
        )?;
        assert_eq!(retained_count, 1, "{table} must retain other agent rows");
    }
    Ok(())
}

#[test]
fn direct_upsert_rolls_back_projection_when_source_state_write_fails() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:atomic-upsert".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "atomic title".into(),
        body: "atomic body".into(),
        sanitized_excerpt: "atomic excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    index.connection.execute_batch(
        "CREATE TEMP TRIGGER fail_source_state_upsert
             BEFORE INSERT ON memory_index_source_state
             BEGIN
                 SELECT RAISE(ABORT, 'source state write failed');
             END;",
    )?;

    assert!(index.upsert_document(&document).is_err());
    let document_key = document_key(&document);
    for table in [
        "memory_documents",
        "memory_documents_fts",
        "memory_documents_fts_rows",
        "memory_index_source_state",
    ] {
        let count: i64 = index.connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE document_key = ?1"),
            [&document_key],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table} must roll back with source state");
    }
    Ok(())
}

#[test]
fn direct_delete_rolls_back_projection_when_source_state_write_fails() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let document = MemoryDocument {
        source_ref: "message:atomic-delete".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "atomic title".into(),
        body: "atomic body".into(),
        sanitized_excerpt: "atomic excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now(),
    };
    index.upsert_document(&document)?;
    index.connection.execute_batch(
        "CREATE TEMP TRIGGER fail_source_state_delete
             BEFORE DELETE ON memory_index_source_state
             BEGIN
                 SELECT RAISE(ABORT, 'source state delete failed');
             END;",
    )?;

    assert!(index
        .delete_document("default", "message:atomic-delete")
        .is_err());
    let document_key = document_key(&document);
    for table in [
        "memory_documents",
        "memory_documents_fts",
        "memory_documents_fts_rows",
        "memory_index_source_state",
    ] {
        let count: i64 = index.connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE document_key = ?1"),
            [&document_key],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1, "{table} must remain when delete rolls back");
    }
    Ok(())
}

#[test]
fn reader_open_does_not_wait_for_writer_turn() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let writer = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let _turn = writer.write_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.hold_writer",
        Instant::now() + MEMORY_INDEX_TRANSACTION_DEADLINE,
    )?;
    let shared_indexes_dir = index_dir.to_path_buf();
    let (sender, receiver) = mpsc::channel();

    let handle = thread::spawn(move || {
        let result = IndexStore::open_reader_in(&shared_indexes_dir, WriterMetrics::default())
            .and_then(|reader| reader.table_exists("memory_documents"));
        sender.send(result).expect("reader result receiver dropped");
    });

    assert!(receiver.recv_timeout(Duration::from_secs(1))??);
    handle.join().expect("reader thread panicked");
    Ok(())
}

#[test]
fn reader_waits_for_incomplete_schema_initialization() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let shared_indexes_dir = index_dir.to_path_buf();
    fs::create_dir_all(&shared_indexes_dir)?;
    let index_path = shared_indexes_dir.join(INDEX_FILENAME);
    drop(Connection::open(&index_path)?);
    let coordinator = memory_index_write_coordinator(&index_path)?;
    let writer_turn = coordinator.wait_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.schema_initialization",
        Instant::now() + MEMORY_INDEX_TRANSACTION_DEADLINE,
    )?;
    let (sender, receiver) = mpsc::channel();

    let handle = thread::spawn(move || {
        let result = IndexStore::open_reader_in(&shared_indexes_dir, WriterMetrics::default())
            .and_then(|reader| reader.schema_is_ready());
        sender.send(result).expect("reader result receiver dropped");
    });

    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(writer_turn);
    assert!(receiver.recv_timeout(Duration::from_secs(2))??);
    handle.join().expect("reader thread panicked");
    Ok(())
}

#[test]
fn independent_connections_serialize_concurrent_enqueues() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    drop(IndexStore::open_in(index_dir, WriterMetrics::default())?);
    let shared_indexes_dir = index_dir.to_path_buf();
    let barrier = Arc::new(Barrier::new(9));
    let mut handles = Vec::new();

    for index in 0..8 {
        let shared_indexes_dir = shared_indexes_dir.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> Result<()> {
            let mut memory_index =
                IndexStore::open_in(&shared_indexes_dir, WriterMetrics::default())?;
            barrier.wait();
            memory_index.enqueue_upsert(
                "default",
                "message",
                &format!("msg-{index}"),
                &format!("message:msg-{index}"),
            )
        }));
    }
    barrier.wait();
    for handle in handles {
        handle.join().expect("writer thread panicked")?;
    }

    let reader = IndexStore::open_reader_in(index_dir, WriterMetrics::default())?;
    let pending_count: i64 = reader.connection.query_row(
        "SELECT COUNT(*) FROM memory_index_pending_sources
             WHERE agent_id = 'default'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(pending_count, 8);
    Ok(())
}

#[test]
fn enqueue_source_retries_until_external_lock_is_released() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let mut index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    index.connection.busy_timeout(Duration::from_millis(20))?;
    let blocker = Connection::open(index_dir.join(INDEX_FILENAME))?;
    blocker.execute_batch("BEGIN IMMEDIATE")?;
    let barrier = Arc::new(Barrier::new(2));
    let thread_barrier = Arc::clone(&barrier);
    let (sender, receiver) = mpsc::channel();

    let handle = thread::spawn(move || {
        thread_barrier.wait();
        let result =
            index.enqueue_upsert("retry-agent", "message", "message-retry", "message:retry");
        sender
            .send(result)
            .expect("transaction result receiver dropped");
    });

    barrier.wait();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(75)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    blocker.execute_batch("COMMIT")?;
    receiver.recv_timeout(Duration::from_secs(1))??;
    handle.join().expect("transaction thread panicked");

    let pending_count: i64 = Connection::open(index_dir.join(INDEX_FILENAME))?.query_row(
        "SELECT COUNT(*) FROM memory_index_pending_sources
             WHERE document_key = 'retry-agent:message:retry'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(pending_count, 1);
    Ok(())
}

#[test]
fn immediate_transaction_stops_retrying_at_deadline() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    index.connection.busy_timeout(Duration::from_millis(10))?;
    let blocker = Connection::open(index_dir.join(INDEX_FILENAME))?;
    blocker.execute_batch("BEGIN IMMEDIATE")?;
    let mut transaction_body_executed = false;
    let started_at = Instant::now();

    let error = index
        .run_write_transaction_with_deadline(
            MemoryIndexWriteClass::Foreground,
            "test.retry_deadline",
            Duration::from_millis(50),
            |_| {
                transaction_body_executed = true;
                Ok(())
            },
        )
        .expect_err("external write lock should exhaust the retry deadline");

    assert!(!transaction_body_executed);
    assert!(
        error.to_string().contains("exhausted retry deadline"),
        "unexpected error: {error:#}"
    );
    assert!(started_at.elapsed() < Duration::from_secs(1));
    blocker.execute_batch("ROLLBACK")?;
    Ok(())
}

#[test]
fn immediate_transaction_rolls_back_after_commit_failure() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    index.connection.execute_batch(
        "PRAGMA foreign_keys = ON;
             CREATE TABLE transaction_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE transaction_child (
                 parent_id INTEGER NOT NULL,
                 FOREIGN KEY (parent_id) REFERENCES transaction_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             );",
    )?;

    index
        .run_write_transaction("test.commit_failure", |transaction| {
            transaction.execute("INSERT INTO transaction_child (parent_id) VALUES (1)", [])?;
            Ok(())
        })
        .expect_err("deferred foreign key violation should fail commit");

    assert!(index.connection.is_autocommit());
    let child_count: i64 =
        index
            .connection
            .query_row("SELECT COUNT(*) FROM transaction_child", [], |row| {
                row.get(0)
            })?;
    assert_eq!(child_count, 0);

    index.run_write_transaction("test.after_commit_failure", |transaction| {
        transaction.execute("INSERT INTO transaction_parent (id) VALUES (1)", [])?;
        transaction.execute("INSERT INTO transaction_child (parent_id) VALUES (1)", [])?;
        Ok(())
    })?;
    Ok(())
}

#[test]
fn stale_meta_refresh_waits_for_writer_turn() -> Result<()> {
    let directory = tempdir()?;
    let index_dir = directory.path();
    let waiting_index = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let writer = IndexStore::open_in(index_dir, WriterMetrics::default())?;
    let writer_turn = writer.write_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.hold_writer",
        Instant::now() + MEMORY_INDEX_TRANSACTION_DEADLINE,
    )?;
    let (sender, receiver) = mpsc::channel();

    let handle = thread::spawn(move || {
        let result = waiting_index.refresh_stale_meta("default");
        sender
            .send(result)
            .expect("stale meta refresh result receiver dropped");
    });

    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(writer_turn);
    receiver.recv_timeout(Duration::from_secs(2))??;
    handle.join().expect("stale meta refresh thread panicked");
    Ok(())
}
