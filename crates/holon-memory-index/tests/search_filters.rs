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
#[test]
fn agent_scope_filter_is_independent_of_workspace_filter() {
    let dir = tempdir().unwrap();
    let index = IndexStore::open_in(dir.path(), WriterMetrics::default()).unwrap();
    let query = "agent scope workspace sentinel";
    for agent_id in ["alpha", "beta"] {
        index
            .upsert_document(&MemoryDocument {
                source_ref: format!("message:agent-{agent_id}"),
                source_kind: "message".into(),
                scope_kind: "agent".into(),
                workspace_id: None,
                agent_id: agent_id.into(),
                source_path: None,
                title: format!("{query} agent message {agent_id}"),
                body: query.into(),
                sanitized_excerpt: query.into(),
                metadata: Value::Null,
                updated_at: Utc::now(),
            })
            .unwrap();
        index
            .upsert_document(&MemoryDocument {
                source_ref: format!("brief:ws-{agent_id}"),
                source_kind: "brief".into(),
                scope_kind: "workspace".into(),
                workspace_id: Some("target-workspace".into()),
                agent_id: agent_id.into(),
                source_path: None,
                title: format!("{query} workspace brief {agent_id}"),
                body: query.into(),
                sanitized_excerpt: query.into(),
                metadata: Value::Null,
                updated_at: Utc::now(),
            })
            .unwrap();
    }

    // An explicit agent scope changes only which owners are searched; the
    // workspace filter keeps excluding foreign workspace documents.
    let results = index
        .search(
            query,
            10,
            &["alpha".into()],
            &[],
            Some("other-workspace"),
            false,
        )
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_ref, "message:agent-alpha");

    // The same scope with a matching workspace keeps both alpha documents.
    let results = index
        .search(
            query,
            10,
            &["alpha".into()],
            &[],
            Some("target-workspace"),
            false,
        )
        .unwrap();
    assert_eq!(results.len(), 2);

    // include_all_workspaces ignores the workspace restriction entirely.
    let results = index
        .search(query, 10, &["alpha".into()], &[], None, true)
        .unwrap();
    assert_eq!(results.len(), 2);

    // Unknown agent ids match nothing and never widen to other agents.
    let results = index
        .search(query, 10, &["ghost".into()], &[], None, true)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn cross_agent_scope_ranks_globally_and_caps_the_shared_limit() {
    let dir = tempdir().unwrap();
    let index = IndexStore::open_in(dir.path(), WriterMetrics::default()).unwrap();
    let query = "global rank sentinel";
    // Beta owns the by-far most relevant document; ranking must stay global
    // across the requested agent scope instead of grouping by owner.
    index
        .upsert_document(&MemoryDocument {
            source_ref: "message:beta-strong".into(),
            source_kind: "message".into(),
            scope_kind: "agent".into(),
            workspace_id: None,
            agent_id: "beta".into(),
            source_path: None,
            title: format!("{query} strong beta"),
            body: format!("{query} {}", "global rank sentinel ".repeat(40)),
            sanitized_excerpt: query.into(),
            metadata: Value::Null,
            updated_at: Utc::now(),
        })
        .unwrap();
    index
        .upsert_document(&MemoryDocument {
            source_ref: "message:alpha-weak".into(),
            source_kind: "message".into(),
            scope_kind: "agent".into(),
            workspace_id: None,
            agent_id: "alpha".into(),
            source_path: None,
            title: format!("{query} weak alpha"),
            body: format!("{query} {}", "filler ".repeat(400)),
            sanitized_excerpt: query.into(),
            metadata: Value::Null,
            updated_at: Utc::now(),
        })
        .unwrap();
    let results = index
        .search(query, 10, &["alpha".into(), "beta".into()], &[], None, true)
        .unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].agent_id, "beta");

    // The limit applies once across the whole requested scope, capped at the
    // store maximum of 50.
    for offset in 0..80 {
        index
            .upsert_document(&MemoryDocument {
                source_ref: format!("message:alpha-bulk-{offset}"),
                source_kind: "message".into(),
                scope_kind: "agent".into(),
                workspace_id: None,
                agent_id: "alpha".into(),
                source_path: None,
                title: format!("{query} bulk {offset}"),
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
            100,
            &["alpha".into(), "beta".into()],
            &[],
            None,
            true,
        )
        .unwrap();
    assert_eq!(results.len(), 50);
}
