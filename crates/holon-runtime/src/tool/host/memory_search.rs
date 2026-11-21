use std::collections::BTreeMap;

use super::serialize_success;
use crate::memory::MemorySearchResult;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

pub(crate) use holon_tools::builtins::memory_search::*;

#[derive(Serialize)]
struct MemorySearchResponse {
    query: String,
    results: Vec<MemorySearchResult>,
    index_status: crate::memory::MemorySearchIndexStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_status_by_agent: Option<BTreeMap<String, crate::memory::MemorySearchIndexStatus>>,
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: MemorySearchArgs = parse_tool_args(NAME, input)?;
    let query = validate_non_empty(args.query, NAME, "query")?;
    // The memory layer normalizes the scope (trim, dedupe, empty -> caller
    // only); explicit non-empty scopes never implicitly add the caller.
    let agent_ids = args.agent_ids.unwrap_or_default();
    let response = runtime
        .search_memory_for_agents(
            &query,
            args.limit.unwrap_or(10),
            args.include_all_workspaces,
            &agent_ids,
        )
        .await?;
    let mut results = response.results;
    for result in &mut results {
        mark_cross_agent_search_evidence(result, agent_id);
    }
    serialize_success(
        NAME,
        &MemorySearchResponse {
            query,
            results,
            index_status: response.index_status,
            index_status_by_agent: response.index_status_by_agent,
        },
    )
}

/// Marks a search hit owned by another agent as cross-agent evidence. The
/// indexed `authority_class` describes the source message only; it comes from
/// trusted indexing metadata, is never inferred from content, is omitted when
/// absent, and never grants the searching agent any permission. Snippets stay
/// verbatim.
fn mark_cross_agent_search_evidence(result: &mut MemorySearchResult, caller_agent_id: &str) {
    if result.agent_id == caller_agent_id {
        return;
    }
    let origin_agent_id = result.agent_id.clone();
    let source_authority_class = result
        .metadata
        .get("authority_class")
        .cloned()
        .unwrap_or(Value::Null);
    if !result.metadata.is_object() {
        result.metadata = json!({});
    }
    result.metadata["cross_agent"] = json!({
        "origin_agent_id": origin_agent_id,
        "source_authority_class": source_authority_class,
        "evidence_only": true,
    });
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        context::ContextConfig,
        provider::StubProvider,
        types::{
            AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        },
    };

    fn message(agent_id: &str, id: &str, text: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        );
        message.id = id.into();
        message
    }

    fn indexed_runtime() -> (RuntimeHandle, tempfile::TempDir, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        runtime
            .storage()
            .append_message(&message(
                "default",
                "msg-scope-own",
                "wallaby home keeper record blue",
            ))
            .unwrap();
        // Rebuild scans only the storage owner's rows; in production each
        // agent's runtime maintains its own index documents. Synthesize the
        // foreign agent's index entries the way its owner would have.
        crate::memory::rebuild_memory_index(runtime.storage(), None).unwrap();
        upsert_foreign_documents(runtime.storage());
        (runtime, dir, workspace)
    }

    fn upsert_foreign_documents(storage: &crate::storage::AppStorage) {
        use holon_memory_index::{IndexStore, MemoryDocument, WriterMetrics};

        let store =
            IndexStore::open_in(&storage.shared_indexes_dir(), WriterMetrics::default()).unwrap();
        for document in [
            MemoryDocument {
                source_ref: "message:msg-scope-foreign".into(),
                source_kind: "message".into(),
                scope_kind: "agent".into(),
                workspace_id: None,
                agent_id: "other-agent".into(),
                source_path: None,
                title: "expedition grant approval".into(),
                body: "wallaby expedition grant approval red".into(),
                sanitized_excerpt: "wallaby expedition grant approval red".into(),
                metadata: json!({"authority_class": "operator_instruction"}),
                updated_at: Utc::now(),
            },
            MemoryDocument {
                source_ref: "task:task-scope-foreign".into(),
                source_kind: "task".into(),
                scope_kind: "agent".into(),
                workspace_id: None,
                agent_id: "other-agent".into(),
                source_path: None,
                title: "zephyr ledger sentinel".into(),
                body: "zephyr ledger sentinel summary".into(),
                sanitized_excerpt: "zephyr ledger sentinel summary".into(),
                metadata: serde_json::Value::Null,
                updated_at: Utc::now(),
            },
        ] {
            store.upsert_document(&document).unwrap();
        }
    }

    async fn search(runtime: &RuntimeHandle, input: serde_json::Value) -> serde_json::Value {
        let result = execute(
            runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &input,
        )
        .await
        .unwrap();
        result.envelope.result.expect("search result").clone()
    }

    #[tokio::test]
    async fn default_scope_returns_caller_hits_without_cross_agent_marks() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        let value = search(&runtime, json!({"query": "wallaby"})).await;
        let results = value["results"].as_array().expect("results");
        assert!(!results.is_empty());
        for result in results {
            assert_eq!(result["agent_id"], "default");
            assert!(result["metadata"].get("cross_agent").is_none());
        }
        // A single-agent scope reports only the aggregate status.
        assert!(value.get("index_status_by_agent").is_none());
        assert_eq!(value["index_status"]["freshness"], "fresh");
    }

    #[tokio::test]
    async fn empty_and_blank_scopes_keep_the_default_scope() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        for agent_ids in [json!([]), json!(["  "])] {
            let value = search(
                &runtime,
                json!({"query": "wallaby", "agent_ids": agent_ids}),
            )
            .await;
            let results = value["results"].as_array().expect("results");
            assert!(!results.is_empty(), "blank scope {agent_ids} lost hits");
            for result in results {
                assert_eq!(result["agent_id"], "default");
            }
        }
    }

    #[tokio::test]
    async fn explicit_other_scope_marks_cross_agent_hits_as_evidence() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        let value = search(
            &runtime,
            json!({"query": "wallaby", "agent_ids": ["other-agent"]}),
        )
        .await;
        let results = value["results"].as_array().expect("results");
        assert_eq!(results.len(), 1);
        let hit = &results[0];
        assert_eq!(hit["source_ref"], "message:msg-scope-foreign");
        assert_eq!(hit["agent_id"], "other-agent");
        assert_eq!(
            hit["metadata"]["cross_agent"]["origin_agent_id"],
            "other-agent"
        );
        assert_eq!(hit["metadata"]["cross_agent"]["evidence_only"], true);
        assert_eq!(
            hit["metadata"]["cross_agent"]["source_authority_class"],
            "operator_instruction"
        );
        // Snippets stay verbatim: no evidence prefix leaks into them.
        let snippet = hit["snippet"].as_str().expect("snippet");
        assert!(!snippet.starts_with("[cross_agent"));
        assert!(snippet.contains("wallaby"));
        // A single explicitly-listed agent still reports one aggregate status.
        assert!(value.get("index_status_by_agent").is_none());
    }

    #[tokio::test]
    async fn mixed_scope_marks_only_foreign_hits_and_reports_per_agent_status() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        let value = search(
            &runtime,
            json!({"query": "wallaby", "agent_ids": ["default", "other-agent"]}),
        )
        .await;
        let results = value["results"].as_array().expect("results");
        assert_eq!(results.len(), 2);
        for result in results {
            let owns_hit = result["agent_id"] == "default";
            let marked = result["metadata"].get("cross_agent").is_some();
            assert_eq!(marked, !owns_hit);
            if !owns_hit {
                assert_eq!(result["metadata"]["cross_agent"]["evidence_only"], true);
            }
        }
        let by_agent = value["index_status_by_agent"].as_object().expect("map");
        assert_eq!(by_agent.len(), 2);
        assert!(by_agent.contains_key("default"));
        assert!(by_agent.contains_key("other-agent"));
    }

    #[tokio::test]
    async fn duplicate_scope_ids_dedupe_without_adding_the_caller() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        let value = search(
            &runtime,
            json!({"query": "wallaby", "agent_ids": ["other-agent", " other-agent "]}),
        )
        .await;
        let results = value["results"].as_array().expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["agent_id"], "other-agent");
    }

    #[tokio::test]
    async fn unknown_scope_matches_nothing_without_error_or_implicit_self() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        let value = search(
            &runtime,
            json!({"query": "wallaby", "agent_ids": ["ghost-agent"]}),
        )
        .await;
        let results = value["results"].as_array().expect("results");
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn non_message_cross_agent_hit_keeps_authority_absent_as_null() {
        let (runtime, _dir, _workspace) = indexed_runtime();
        let value = search(
            &runtime,
            json!({"query": "zephyr", "agent_ids": ["other-agent"]}),
        )
        .await;
        let results = value["results"].as_array().expect("results");
        assert_eq!(results.len(), 1);
        let hit = &results[0];
        assert_eq!(hit["source_ref"], "task:task-scope-foreign");
        assert_eq!(hit["agent_id"], "other-agent");
        assert_eq!(
            hit["metadata"]["cross_agent"]["origin_agent_id"],
            "other-agent"
        );
        assert_eq!(hit["metadata"]["cross_agent"]["evidence_only"], true);
        // Only trusted indexing metadata supplies authority; non-message
        // documents have none, so the field stays explicitly null.
        assert_eq!(
            hit["metadata"]["cross_agent"]["source_authority_class"],
            serde_json::Value::Null
        );
    }
}
