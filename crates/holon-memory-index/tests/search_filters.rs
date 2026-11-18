use chrono::Utc;
use holon_memory_index::{IndexStore, MemoryDocument, WriterMetrics};
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn source_kind_filter_is_applied_before_search_limit() {
    let dir = tempdir().unwrap();
    let index = IndexStore::open_in(dir.path(), WriterMetrics::default()).unwrap();
    let query = "kind filter limit sentinel";
    let message = MemoryDocument {
        source_ref: "message:target".into(),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: "default".into(),
        source_path: None,
        title: "target message".into(),
        body: format!("{query} {}", "low relevance filler ".repeat(200)),
        sanitized_excerpt: "low relevance message excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now() - chrono::Duration::seconds(1),
    };
    index.upsert_document(&message).unwrap();
    let limit = 20;
    for offset in 0..limit {
        index
            .upsert_document(&MemoryDocument {
                source_ref: format!("tool_execution:tool-{offset}:output"),
                source_kind: "tool_execution_output".into(),
                scope_kind: "agent".into(),
                workspace_id: None,
                agent_id: "default".into(),
                source_path: None,
                title: format!("{query} tool output {offset}"),
                body: query.into(),
                sanitized_excerpt: query.into(),
                metadata: Value::Null,
                updated_at: Utc::now(),
            })
            .unwrap();
    }

    let unfiltered = index
        .search(query, limit, &["default".into()], &[], None, false)
        .unwrap();
    assert!(!unfiltered
        .iter()
        .any(|result| result.source_ref == message.source_ref));

    let filtered = index
        .search(
            query,
            limit,
            &["default".into()],
            &["message".into()],
            None,
            false,
        )
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].source_ref, message.source_ref);
}

#[test]
fn workspace_filter_is_applied_before_search_limit() {
    let dir = tempdir().unwrap();
    let index = IndexStore::open_in(dir.path(), WriterMetrics::default()).unwrap();
    let query = "workspace filter limit sentinel";
    let target = MemoryDocument {
        source_ref: "brief:target".into(),
        source_kind: "brief".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some("target-workspace".into()),
        agent_id: "default".into(),
        source_path: None,
        title: "target brief".into(),
        body: format!("{query} {}", "low relevance filler ".repeat(200)),
        sanitized_excerpt: "low relevance workspace excerpt".into(),
        metadata: Value::Null,
        updated_at: Utc::now() - chrono::Duration::seconds(1),
    };
    index.upsert_document(&target).unwrap();
    let limit = 20;
    for offset in 0..limit {
        index
            .upsert_document(&MemoryDocument {
                source_ref: format!("brief:other-{offset}"),
                source_kind: "brief".into(),
                scope_kind: "workspace".into(),
                workspace_id: Some("other-workspace".into()),
                agent_id: "default".into(),
                source_path: None,
                title: format!("{query} other workspace {offset}"),
                body: query.into(),
                sanitized_excerpt: query.into(),
                metadata: Value::Null,
                updated_at: Utc::now(),
            })
            .unwrap();
    }

    let results = index
        .search(
            query,
            limit,
            &["default".into()],
            &[],
            Some("target-workspace"),
            false,
        )
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_ref, target.source_ref);
}
