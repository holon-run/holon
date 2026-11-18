use super::*;

pub fn maintenance_test_documents(
    index: &IndexStore,
    agent: &str,
    count: usize,
) -> Result<Vec<String>> {
    let documents = (0..count)
        .map(|number| MemoryDocument {
            source_ref: format!("message:maintenance-{number:04}"),
            source_kind: "message".into(),
            scope_kind: "agent".into(),
            workspace_id: None,
            agent_id: agent.into(),
            source_path: None,
            title: "maintenance fixture".into(),
            body: "maintenance searchable sentinel".into(),
            sanitized_excerpt: "maintenance sentinel".into(),
            metadata: Value::Null,
            updated_at: Utc::now(),
        })
        .collect::<Vec<_>>();
    index.run_write_transaction("test.seed_maintenance", |tx| {
        for document in &documents {
            upsert_document_tx(tx, document)?;
            upsert_source_state_tx(tx, document, source_id_from_ref(&document.source_ref))?;
        }
        Ok(())
    })?;
    Ok(documents.iter().map(document_key).collect())
}

pub fn maintenance_test_job(index: &IndexStore, agent: &str, phase: &str) -> Result<RebuildJob> {
    let now = Utc::now().to_rfc3339();
    index.connection.execute(
        "INSERT INTO memory_index_rebuild_jobs (
                 agent_id, generation, phase, source_kind_index, source_cursor, source_offset,
                 runtime_high_watermark, documents_processed, started_at, last_progress_at
             ) VALUES (?1, 'maintenance-generation', ?2, ?3, '', 0, 0, 0, ?4, ?4)",
        params![agent, phase, all_backfill_source_kinds().len(), now],
    )?;
    Ok(index.rebuild_job(agent)?.unwrap())
}

pub fn seed_outbox_cursor(
    index: &IndexStore,
    runtime_id: &str,
    agent_id: &str,
    change_seq: i64,
) -> Result<()> {
    upsert_cursor_tx(
        &index.connection,
        runtime_id,
        agent_id,
        MEMORY_INDEX_OUTBOX_CURSOR,
        change_seq,
    )
}
