// HTTP control route integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::Result;
use axum::Router;
use holon::{
    client::LocalClient,
    config::{load_persisted_config_at, ApiCorsConfigFile, AppConfig, ControlAuthMode},
    daemon::RuntimeServiceHandle,
    host::RuntimeHost,
    http::{self, AppState},
    provider::{AgentProvider, StubProvider},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentStatus, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, CommandTaskSpec, ContinuationClass, ControlAction, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority, TodoItem,
        TodoItemState, WorkItemState,
    },
};
use reqwest::Client;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::time::{sleep, Duration, Instant};

use super::{
    attach_default_workspace, connect_addr, git, init_git_repo, spawn_server,
    spawn_server_for_host, spawn_server_with_config, spawn_server_with_runtime_config,
    spawn_unix_server, tempdir, test_config, test_config_with_paths, test_work_item, unix_request,
    wait_until, RuntimeFailureProvider,
};

pub async fn control_prompt_is_open_on_loopback_auto() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert!(response.status().is_success());
    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn control_prompt_is_open_over_unix_socket_auto() -> Result<()> {
    let config = test_config();
    let (_host, socket_path, server) = spawn_unix_server(config).await?;
    let response = unix_request(
        &socket_path,
        "POST",
        "/control/agents/default/prompt",
        &[("content-type", "application/json")],
        Some(br#"{ "text": "hello" }"#),
    )
    .await?;
    assert_eq!(response.status, 200);
    server.abort();
    Ok(())
}

pub async fn agent_state_route_returns_aggregated_snapshot() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "snapshot bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let state_payload: serde_json::Value = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;

    assert!(state_payload["agent"].is_object());
    assert!(state_payload["agent"]["scheduling_posture"]["posture"].is_string());
    assert!(state_payload["agent"]["scheduling_posture"]["reason"].is_string());
    assert!(state_payload["session"].is_object());
    assert!(state_payload["tasks"].is_array());
    assert!(state_payload.get("transcript_tail").is_none());
    assert!(state_payload.get("operator_messages").is_none());
    assert!(state_payload.get("events_tail").is_none());
    assert!(state_payload.get("briefs_tail").is_none());
    assert!(state_payload.get("brief").is_none());
    assert!(state_payload["timers"].is_array());
    assert!(state_payload["work_items"].is_array());
    assert!(state_payload.get("work_plan").is_none());
    assert!(state_payload["waiting_intents"].is_array());
    assert!(state_payload["external_triggers"].is_array());
    assert!(state_payload["workspace"].is_object());
    assert!(state_payload.get("operator_notifications").is_none());
    assert!(state_payload.get("execution").is_none());
    assert!(state_payload.get("cursor").is_none());

    server.abort();
    Ok(())
}

pub async fn runtime_search_route_returns_memory_search_results() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:test".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "http memory search sentinel issue1879".into(),
        },
    );
    message.id = "msg-http-search-memory-v2".into();
    runtime.storage().append_message(&message)?;

    let response = client
        .post(format!("{base}/search"))
        .json(&serde_json::json!({
            "query": "issue1879",
            "limit": 5
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["query"], "issue1879");
    assert_eq!(payload["limit"], 5);
    assert!(payload["index_status"].is_object());

    let http_refs = payload["results"]
        .as_array()
        .expect("search results should be an array")
        .iter()
        .map(|result| {
            result["source_ref"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    let tool_result = runtime.search_memory("issue1879", 5, false).await?;
    assert_eq!(
        payload["index_status"]["freshness"],
        serde_json::to_value(tool_result.index_status.freshness)?
    );
    let tool_refs = tool_result
        .results
        .into_iter()
        .map(|result| result.source_ref)
        .collect::<Vec<_>>();
    assert_eq!(http_refs, tool_refs);
    assert!(payload["results"].as_array().unwrap().iter().any(|result| {
        result["kind"] == "message"
            && result["source_ref"] == "message:msg-http-search-memory-v2"
            && result["scope_kind"] == "agent"
    }));

    server.abort();
    Ok(())
}

pub async fn runtime_search_route_filters_memory_results_by_agent_ids() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    host.create_named_agent("alpha", None).await?;
    host.create_named_agent("beta", None).await?;
    let alpha = host.get_or_create_agent("alpha").await?;
    let beta = host.get_or_create_agent("beta").await?;

    let mut alpha_message = MessageEnvelope::new(
        "alpha",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:test".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "shared agent search sentinel agentfilter1879 alpha".into(),
        },
    );
    alpha_message.id = "msg-http-search-alpha".into();
    alpha.storage().append_message(&alpha_message)?;

    let mut beta_message = MessageEnvelope::new(
        "beta",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:test".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "shared agent search sentinel agentfilter1879 beta".into(),
        },
    );
    beta_message.id = "msg-http-search-beta".into();
    beta.storage().append_message(&beta_message)?;

    let (base, server) = spawn_server_for_host(host.clone()).await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/search"))
        .json(&serde_json::json!({
            "query": "agentfilter1879",
            "limit": 10,
            "agent_ids": ["alpha"]
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = response.json().await?;
    let results = payload["results"].as_array().expect("results array");
    assert!(results.iter().any(|result| {
        result["agent_id"] == "alpha" && result["source_ref"] == "message:msg-http-search-alpha"
    }));
    assert!(results.iter().all(|result| result["agent_id"] == "alpha"));

    let response = client
        .post(format!("{base}/search"))
        .json(&serde_json::json!({
            "query": "agentfilter1879",
            "limit": 10,
            "agent_ids": ["alpha", "beta"]
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = response.json().await?;
    let refs = payload["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|result| result["source_ref"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(refs.contains(&"message:msg-http-search-alpha"));
    assert!(refs.contains(&"message:msg-http-search-beta"));

    let response = client
        .post(format!("{base}/search"))
        .json(&serde_json::json!({
            "query": "agentfilter1879",
            "limit": 10
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.json::<serde_json::Value>().await?["results"]
        .as_array()
        .expect("results array")
        .is_empty());

    server.abort();
    Ok(())
}

pub async fn agent_brief_route_returns_full_brief_by_id() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "brief detail route" }))
        .send()
        .await?;
    assert!(response.status().is_success());

    wait_until(|| {
        Ok(runtime
            .storage()
            .read_recent_briefs(10)?
            .iter()
            .any(|brief| !brief.text.trim().is_empty()))
    })
    .await?;
    let brief = runtime
        .storage()
        .read_recent_briefs(10)?
        .into_iter()
        .find(|brief| !brief.text.trim().is_empty())
        .expect("brief should be persisted");

    let detail = client
        .get(format!("{base}/agents/default/briefs/{}", brief.id))
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let returned: BriefRecord = detail.json().await?;

    assert_eq!(returned.id, brief.id);
    assert_eq!(returned.agent_id, "default");
    assert_eq!(returned.text, brief.text);

    server.abort();
    Ok(())
}

pub async fn agent_state_route_scopes_work_items_to_requested_agent() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    host.create_named_agent("alpha", None).await?;
    host.create_named_agent("beta", None).await?;
    let alpha = host.get_or_create_agent("alpha").await?;
    let beta = host.get_or_create_agent("beta").await?;
    let alpha_item = alpha
        .create_work_item("alpha scoped work item".into(), None, None, Vec::new())
        .await?;
    let beta_item = beta
        .create_work_item("beta scoped work item".into(), None, None, Vec::new())
        .await?;
    let (base, server) = spawn_server_for_host(host.clone()).await?;
    let client = reqwest::Client::new();

    let alpha_state: serde_json::Value = client
        .get(format!("{base}/agents/alpha/state"))
        .send()
        .await?
        .json()
        .await?;
    let beta_state: serde_json::Value = client
        .get(format!("{base}/agents/beta/state"))
        .send()
        .await?
        .json()
        .await?;

    let alpha_ids: Vec<String> = alpha_state["work_items"]
        .as_array()
        .expect("work_items array")
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    let beta_ids: Vec<String> = beta_state["work_items"]
        .as_array()
        .expect("work_items array")
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();

    assert!(
        alpha_ids.iter().any(|id| id == &alpha_item.id),
        "alpha state should include alpha work item: {alpha_ids:?}",
    );
    assert!(
        !alpha_ids.iter().any(|id| id == &beta_item.id),
        "alpha state must not leak beta work items: {alpha_ids:?}",
    );
    assert!(
        beta_ids.iter().any(|id| id == &beta_item.id),
        "beta state should include beta work item: {beta_ids:?}",
    );
    assert!(
        !beta_ids.iter().any(|id| id == &alpha_item.id),
        "beta state must not leak alpha work items: {beta_ids:?}",
    );

    server.abort();
    Ok(())
}

pub async fn agent_state_route_includes_bootstrap_projection_fields_when_present() -> Result<()> {
    let mut config = test_config();
    let (host, base, server) = spawn_server_with_config(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    config.http_addr = base.trim_start_matches("http://").to_string();
    let local_client = LocalClient::new(config)?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "bootstrap contract prompt" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_briefs(1)?.first().is_some())).await?;

    let work_item = test_work_item(
        &runtime,
        "bootstrap active work",
        WorkItemState::Open,
        true,
        Some("shape /state"),
    )
    .await?;
    runtime
        .update_work_item_fields(
            work_item.id.clone(),
            None,
            None,
            None,
            Some(vec![TodoItem {
                text: "expand /state".into(),
                state: TodoItemState::InProgress,
            }]),
            None,
        )
        .await?;
    runtime
        .schedule_timer(5_000, None, Some("state timer".into()))
        .await?;
    runtime
        .create_callback(
            "wait for review".into(),
            "github".into(),
            "pr_reviewed".into(),
            Some("pull_request:249".into()),
            CallbackDeliveryMode::EnqueueMessage,
        )
        .await?;
    runtime.storage().append_brief(&BriefRecord::new(
        "default",
        BriefKind::Result,
        "newer brief",
        None,
        None,
    ))?;

    let runtime_work_items = runtime.latest_work_items().await?;
    assert!(
        runtime_work_items
            .iter()
            .any(|item| item.id == work_item.id && item.state != WorkItemState::Completed),
        "runtime work items missing expected bootstrap item: {:?}",
        runtime_work_items
    );

    let raw_snapshot: serde_json::Value = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        raw_snapshot["work_items"]
            .as_array()
            .map(|items| items.iter().any(|item| {
                item["id"] == serde_json::Value::String(work_item.id.clone())
                    && item["state"] != serde_json::Value::String("completed".into())
                    && item.get("todo_list").is_none()
                    && item.get("plan_artifact").is_none()
                    && item.get("work_refs").is_none()
            }))
            .unwrap_or(false),
        "raw snapshot missing expected work item: {}",
        raw_snapshot
    );
    assert!(raw_snapshot.get("operator_notifications").is_none());
    assert!(raw_snapshot.get("execution").is_none());
    assert!(
        raw_snapshot["agent"]
            .as_object()
            .is_some_and(|agent| !agent.contains_key("recent_operator_notifications")),
        "state agent should not embed recent operator notifications: {}",
        raw_snapshot["agent"]
    );

    let snapshot = local_client.agent_state_snapshot("default").await?;
    assert!(snapshot
        .timers
        .iter()
        .any(|timer| timer.summary.as_deref() == Some("state timer")));
    assert!(snapshot
        .work_items
        .iter()
        .any(|item| item.id == work_item.id && item.state != WorkItemState::Completed));
    assert_eq!(
        snapshot
            .work_items
            .iter()
            .find(|item| item.id == work_item.id)
            .map(|item| item.id.clone()),
        Some(work_item.id.clone())
    );
    assert!(snapshot.waiting_intents.is_empty());
    assert_eq!(snapshot.external_triggers.len(), 1);
    assert!(snapshot.external_triggers[0].waiting_intent_id.is_none());
    assert_eq!(
        snapshot.external_triggers[0].target_agent_id,
        snapshot.agent.identity.agent_id
    );
    assert!(
        raw_snapshot["external_triggers"][0]["external_trigger_id"].is_string(),
        "raw snapshot external trigger should expose external_trigger_id: {}",
        raw_snapshot["external_triggers"]
    );
    assert!(
        raw_snapshot["external_triggers"][0]["token_hash"].is_null(),
        "raw snapshot external trigger should not expose token_hash: {}",
        raw_snapshot["external_triggers"]
    );
    assert_eq!(
        snapshot
            .workspace
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone()),
        snapshot
            .agent
            .agent
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
    );
    assert!(snapshot.workspace.active_workspace_entry.is_some());

    server.abort();
    Ok(())
}

pub async fn list_skills_includes_all_agent_skill_roots() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent_home = host.config().data_dir.join("agents/default");
    for (root, name) in [("skills", "alpha"), (".codex/skills", "beta")] {
        let skill_dir = agent_home.join(root).join(name);
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: installed\n---\nbody"),
        )?;
    }

    let payload: serde_json::Value = Client::new()
        .get(format!("{base}/agents/default/skills"))
        .send()
        .await?
        .json()
        .await?;
    let names = payload["skills"]
        .as_array()
        .expect("skills should be an array")
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect::<Vec<_>>();

    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));

    server.abort();
    Ok(())
}

pub async fn agent_skills_endpoint_uses_effective_registry_snapshot() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent_home = host.config().data_dir.join("agents/default");
    let agent_skill_dir = agent_home.join("skills/shared-demo");
    std::fs::create_dir_all(&agent_skill_dir)?;
    std::fs::write(
        agent_skill_dir.join("SKILL.md"),
        "---\nname: shared-demo\ndescription: agent\n---\nbody",
    )?;

    let workspace_skill_dir = host
        .config()
        .workspace_dir
        .join(".agents/skills/workspace-demo");
    std::fs::create_dir_all(&workspace_skill_dir)?;
    std::fs::write(
        workspace_skill_dir.join("SKILL.md"),
        "---\nname: workspace-demo\ndescription: workspace\n---\nbody",
    )?;

    let client = Client::new();
    let catalog_payload: serde_json::Value = client
        .get(format!("{base}/api/skills/catalog?agent_id=default"))
        .send()
        .await?
        .json()
        .await?;
    let list_payload: serde_json::Value = client
        .get(format!("{base}/agents/default/skills"))
        .send()
        .await?
        .json()
        .await?;
    let catalog_names = catalog_payload["catalog"]
        .as_array()
        .expect("catalog should be an array")
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let listed_names = list_payload["skills"]
        .as_array()
        .expect("skills should be an array")
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(catalog_names, listed_names);
    assert!(listed_names.contains("shared-demo"));
    assert!(listed_names.contains("workspace-demo"));

    std::fs::remove_dir_all(&agent_skill_dir)?;
    let catalog_payload: serde_json::Value = client
        .get(format!("{base}/api/skills/catalog?agent_id=default"))
        .send()
        .await?
        .json()
        .await?;
    let list_payload: serde_json::Value = client
        .get(format!("{base}/agents/default/skills"))
        .send()
        .await?
        .json()
        .await?;
    assert!(!catalog_payload["catalog"]
        .as_array()
        .expect("catalog should be an array")
        .iter()
        .any(|skill| skill["name"] == "shared-demo"));
    assert!(!list_payload["skills"]
        .as_array()
        .expect("skills should be an array")
        .iter()
        .any(|skill| skill["name"] == "shared-demo"));

    server.abort();
    Ok(())
}

pub async fn agent_skills_endpoint_does_not_leak_stale_roots_between_agents() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    host.create_named_agent("alpha", None).await?;

    let default_home = host.config().data_dir.join("agents/default");
    let default_skill_dir = default_home.join("skills/default-only");
    std::fs::create_dir_all(&default_skill_dir)?;
    std::fs::write(
        default_skill_dir.join("SKILL.md"),
        "---\nname: default-only\ndescription: default\n---\nbody",
    )?;

    let alpha_home = host.config().data_dir.join("agents/alpha");
    let alpha_skill_dir = alpha_home.join("skills/alpha-only");
    std::fs::create_dir_all(&alpha_skill_dir)?;
    std::fs::write(
        alpha_skill_dir.join("SKILL.md"),
        "---\nname: alpha-only\ndescription: alpha\n---\nbody",
    )?;

    let (base, server) = spawn_server_for_host(host.clone()).await?;
    let client = Client::new();

    let default_payload: serde_json::Value = client
        .get(format!("{base}/agents/default/skills"))
        .send()
        .await?
        .json()
        .await?;
    assert!(default_payload["skills"]
        .as_array()
        .expect("default skills should be an array")
        .iter()
        .any(|skill| skill["name"] == "default-only"));

    let alpha_payload: serde_json::Value = client
        .get(format!("{base}/agents/alpha/skills"))
        .send()
        .await?
        .json()
        .await?;
    let alpha_skills = alpha_payload["skills"]
        .as_array()
        .expect("alpha skills should be an array");
    assert!(alpha_skills
        .iter()
        .any(|skill| skill["name"] == "alpha-only"));
    assert!(!alpha_skills
        .iter()
        .any(|skill| skill["name"] == "default-only"));

    server.abort();
    Ok(())
}

pub async fn skills_catalog_uses_shared_registry_with_agent_and_workspace_roots() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent_home = host.config().data_dir.join("agents/default");
    let agent_skill_dir = agent_home.join("skills/agent-demo");
    std::fs::create_dir_all(&agent_skill_dir)?;
    std::fs::write(
        agent_skill_dir.join("SKILL.md"),
        "---\nname: shared-demo\ndescription: agent\n---\nbody",
    )?;

    let workspace_skill_dir = host
        .config()
        .workspace_dir
        .join(".agents/skills/workspace-demo");
    std::fs::create_dir_all(&workspace_skill_dir)?;
    std::fs::write(
        workspace_skill_dir.join("SKILL.md"),
        "---\nname: workspace-demo\ndescription: workspace\n---\nbody",
    )?;

    let client = Client::new();
    let payload: serde_json::Value = client
        .get(format!("{base}/api/skills/catalog?agent_id=default"))
        .send()
        .await?
        .json()
        .await?;
    let skills = payload["catalog"]
        .as_array()
        .expect("catalog should be an array");
    assert!(skills
        .iter()
        .any(|skill| skill["name"] == "shared-demo" && skill["scope"] == "agent"));
    assert!(skills
        .iter()
        .any(|skill| skill["name"] == "workspace-demo" && skill["scope"] == "workspace"));

    std::fs::remove_dir_all(&agent_skill_dir)?;
    let payload: serde_json::Value = client
        .get(format!("{base}/api/skills/catalog?agent_id=default"))
        .send()
        .await?
        .json()
        .await?;
    let skills = payload["catalog"]
        .as_array()
        .expect("catalog should be an array");
    assert!(!skills.iter().any(|skill| skill["name"] == "shared-demo"));

    server.abort();
    Ok(())
}

pub async fn install_skill_existing_destination_returns_conflict() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = Client::new();
    let payload = serde_json::json!({
        "kind": {
            "kind": "builtin",
            "name": "ghx",
        }
    });

    let first = client
        .post(format!("{base}/control/agents/default/skills/install"))
        .json(&payload)
        .send()
        .await?;
    assert!(first.status().is_success());

    let second = client
        .post(format!("{base}/control/agents/default/skills/install"))
        .json(&payload)
        .send()
        .await?;
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = second.json().await?;
    assert_eq!(body["code"], "skill_already_installed");
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("uninstall it first"));
    assert!(body["hint"]
        .as_str()
        .unwrap_or_default()
        .contains("uninstall"));

    server.abort();
    Ok(())
}

pub async fn add_skill_to_catalog_existing_destination_returns_conflict() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = Client::new();
    let skill_name = format!("http-catalog-conflict-{}", std::process::id());
    let local_skill_root = tempdir()?;
    let local_skill_path = local_skill_root.path().join(&skill_name);
    std::fs::create_dir_all(&local_skill_path)?;
    std::fs::write(
        local_skill_path.join("SKILL.md"),
        format!("# {skill_name}\n\nTemporary catalog conflict test skill.\n"),
    )?;
    let user_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME must be set for skill library tests"))?;
    let library_root = [".agents/skills", ".codex/skills", ".claude/skills"]
        .iter()
        .map(|suffix| user_home.join(suffix))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| user_home.join(".agents").join("skills"));
    let library_path = library_root.join(&skill_name);
    let _ = std::fs::remove_file(&library_path);
    let _ = std::fs::remove_dir_all(&library_path);
    let payload = serde_json::json!({
        "kind": {
            "kind": "local",
            "path": local_skill_path,
        }
    });

    let first = client
        .post(format!("{base}/api/skills/catalog/add"))
        .json(&payload)
        .send()
        .await?;
    assert!(first.status().is_success());

    let second = client
        .post(format!("{base}/api/skills/catalog/add"))
        .json(&payload)
        .send()
        .await?;
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = second.json().await?;
    assert_eq!(body["code"], "skill_already_installed");

    let _ = std::fs::remove_file(&library_path);
    let _ = std::fs::remove_dir_all(&library_path);
    server.abort();
    Ok(())
}

pub async fn skill_library_add_remove_and_agent_enable_disable_are_separate() -> Result<()> {
    let config = test_config();
    let skill_name = format!("http-split-skill-{}", std::process::id());
    let local_skill_root = tempdir()?;
    let local_skill_path = local_skill_root.path().join(&skill_name);
    std::fs::create_dir_all(&local_skill_path)?;
    std::fs::write(
        local_skill_path.join("SKILL.md"),
        format!("# {skill_name}\n\nTemporary split API test skill.\n"),
    )?;
    let user_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME must be set for skill library tests"))?;
    let library_root = [".agents/skills", ".codex/skills", ".claude/skills"]
        .iter()
        .map(|suffix| user_home.join(suffix))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| user_home.join(".agents").join("skills"));
    let library_path = library_root.join(&skill_name);
    let agent_skill_path = config
        .data_dir
        .join("agents")
        .join("default")
        .join("skills")
        .join(&skill_name);
    let _ = std::fs::remove_file(&library_path);
    let _ = std::fs::remove_dir_all(&library_path);
    let _ = std::fs::remove_file(&agent_skill_path);
    let _ = std::fs::remove_dir_all(&agent_skill_path);
    let bind_addr = config.http_addr.clone();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("route result")))?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host));
    let listener = TcpListener::bind(&bind_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });
    let client = Client::new();

    let add = client
        .post(format!("{base}/api/skills/catalog/add"))
        .json(&serde_json::json!({
            "kind": {
                "kind": "local",
                "path": local_skill_path,
            }
        }))
        .send()
        .await?;
    let add_status = add.status();
    let add_body = add.text().await?;
    assert!(
        add_status.is_success(),
        "add failed: status={add_status}, body={add_body}"
    );
    assert!(library_path.join("SKILL.md").exists());
    assert!(!agent_skill_path.exists());

    let enable = client
        .post(format!("{base}/control/agents/default/skills/enable"))
        .json(&serde_json::json!({
            "name": skill_name,
        }))
        .send()
        .await?;
    assert!(enable.status().is_success());
    assert!(agent_skill_path.exists());

    let disable = client
        .post(format!("{base}/control/agents/default/skills/disable"))
        .json(&serde_json::json!({
            "name": skill_name,
        }))
        .send()
        .await?;
    assert!(disable.status().is_success());
    assert!(library_path.exists());
    assert!(!agent_skill_path.exists());

    let remove = client
        .post(format!("{base}/api/skills/catalog/remove"))
        .json(&serde_json::json!({
            "name": skill_name,
        }))
        .send()
        .await?;
    assert!(remove.status().is_success());
    assert!(!library_path.exists());

    server.abort();
    Ok(())
}

pub async fn control_agent_model_override_set_and_clear_updates_status() -> Result<()> {
    let mut config = test_config();
    config.default_model = holon::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap();
    config
        .providers
        .get_mut(&holon::config::ProviderId::anthropic())
        .unwrap()
        .credential = Some("dummy".into());
    let (_host, base, server) = spawn_server_with_runtime_config(config).await?;
    let client = reqwest::Client::new();

    let set_response = client
        .post(format!("{base}/control/agents/default/model"))
        .json(&serde_json::json!({ "model": "anthropic/claude-haiku-4-5" }))
        .send()
        .await?;
    assert!(set_response.status().is_success());
    let set_payload: serde_json::Value = set_response.json().await?;
    assert_eq!(set_payload["model"]["source"], "agent_override");
    assert_eq!(
        set_payload["model"]["effective_model"],
        "anthropic/claude-haiku-4-5"
    );

    let status_payload: serde_json::Value = client
        .get(format!("{base}/agents/default/status"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(status_payload["model"]["source"], "agent_override");
    assert_eq!(
        status_payload["agent"]["model_override"],
        "anthropic/claude-haiku-4-5"
    );

    let clear_response = client
        .post(format!("{base}/control/agents/default/model/clear"))
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert!(clear_response.status().is_success());
    let clear_payload: serde_json::Value = clear_response.json().await?;
    assert_eq!(clear_payload["model"]["source"], "runtime_default");
    assert!(clear_payload["model"]["override_model"].is_null());

    server.abort();
    Ok(())
}

pub async fn control_prompt_requires_bearer_token_when_required() -> Result<()> {
    let config = test_config_with_paths(
        tempdir().unwrap().keep(),
        tempdir().unwrap().keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    let (_host, base, server) = spawn_server_with_config(config).await?;
    let client = reqwest::Client::new();
    let denied = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    let allowed = client
        .post(format!("{base}/control/agents/default/prompt"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert!(allowed.status().is_success());
    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn control_runtime_status_is_open_over_unix_socket_when_auth_required() -> Result<()> {
    let config = test_config_with_paths(
        tempdir().unwrap().keep(),
        tempdir().unwrap().keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_unix_with_runtime_service(
        host.clone(),
        Some(runtime_service),
    ));
    let listener = UnixListener::bind(&config.socket_path)?;
    let socket_path = config.socket_path.clone();
    let server = tokio::spawn(async move {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        http::serve_unix(listener, router, rx).await?;
        Ok::<_, anyhow::Error>(())
    });

    let response = unix_request(&socket_path, "GET", "/control/runtime/status", &[], None).await?;
    assert_eq!(response.status, 200);

    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn control_runtime_readiness_is_open_over_unix_socket_when_auth_required() -> Result<()> {
    let config = test_config_with_paths(
        tempdir().unwrap().keep(),
        tempdir().unwrap().keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_unix_with_runtime_service(
        host.clone(),
        Some(runtime_service),
    ));
    let listener = UnixListener::bind(&config.socket_path)?;
    let socket_path = config.socket_path.clone();
    let server = tokio::spawn(async move {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        http::serve_unix(listener, router, rx).await?;
        Ok::<_, anyhow::Error>(())
    });

    let response =
        unix_request(&socket_path, "GET", "/control/runtime/readiness", &[], None).await?;
    assert_eq!(response.status, 200);

    server.abort();
    Ok(())
}

pub async fn remote_tcp_surfaces_require_bearer_token_when_required() -> Result<()> {
    let config = test_config_with_paths(
        tempdir().unwrap().keep(),
        tempdir().unwrap().keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });
    let client = reqwest::Client::new();

    for path in [
        "/handshake",
        "/",
        "/control/runtime/status",
        "/control/runtime/config",
        "/agents/list",
        "/agents/default/status",
        "/agents/default/state",
        "/agents/default/briefs",
        "/agents/default/briefs/brief-test",
        "/agents/default/transcript",
        "/agents/default/tasks",
        "/agents/default/timers",
        "/agents/default/worktree-summary",
        "/agents/default/skills",
        "/agents/default/events",
        "/agents/default/events/stream",
    ] {
        let denied = client.get(format!("{base}{path}")).send().await?;
        assert_eq!(
            denied.status(),
            reqwest::StatusCode::FORBIDDEN,
            "{path} should require bearer auth"
        );
        let body: serde_json::Value = denied.json().await?;
        assert_eq!(body["ok"], false, "{path} error should use envelope");
        assert!(
            body["error"].is_string(),
            "{path} error should include message: {body}"
        );
    }

    let handshake: serde_json::Value = client
        .get(format!("{base}/handshake"))
        .bearer_auth("secret")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(handshake["protocol"]["version"], 1);
    assert_eq!(handshake["auth"]["mode"], "bearer");
    assert_eq!(handshake["runtime"]["default_agent"], "default");

    let agents = client
        .get(format!("{base}/agents/list"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert!(agents.status().is_success());

    let removed_agents = client
        .get(format!("{base}/agents"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert_eq!(removed_agents.status(), reqwest::StatusCode::NOT_FOUND);
    let removed_body: serde_json::Value = removed_agents.json().await?;
    assert_eq!(removed_body["ok"], false);
    assert_eq!(removed_body["error"], "Not Found");

    let invalid_runtime_status = client
        .get(format!("{base}/control/runtime/status"))
        .bearer_auth("wrong")
        .send()
        .await?;
    assert_eq!(
        invalid_runtime_status.status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let invalid_body: serde_json::Value = invalid_runtime_status.json().await?;
    assert_eq!(invalid_body["ok"], false);
    assert!(invalid_body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("invalid control token"));

    let runtime_status = client
        .get(format!("{base}/control/runtime/status"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert!(runtime_status.status().is_success());

    let denied_enqueue = client
        .post(format!("{base}/enqueue"))
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert_eq!(denied_enqueue.status(), reqwest::StatusCode::FORBIDDEN);
    let denied_enqueue_body: serde_json::Value = denied_enqueue.json().await?;
    assert_eq!(denied_enqueue_body["ok"], false);
    assert!(denied_enqueue_body["error"].is_string());
    let allowed_enqueue = client
        .post(format!("{base}/enqueue"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert!(allowed_enqueue.status().is_success());

    server.abort();
    Ok(())
}

pub async fn control_wake_records_liveness_only_system_tick_on_loopback_auto() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    wait_until(|| {
        Ok(runtime
            .storage()
            .read_agent()?
            .map(|agent| agent.status == holon::types::AgentStatus::Asleep)
            .unwrap_or(false))
    })
    .await?;

    let allowed = client
        .post(format!("{base}/control/agents/default/wake"))
        .json(&serde_json::json!({
            "reason": "pr changed",
            "source": "github",
            "correlation_id": "corr-123"
        }))
        .send()
        .await?;
    assert!(allowed.status().is_success());
    let payload: serde_json::Value = allowed.json().await?;
    assert_eq!(payload["disposition"], "triggered");

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        let agent = runtime.storage().read_agent()?.expect("agent should exist");
        Ok(messages
            .iter()
            .any(|message| message.kind == holon::types::MessageKind::SystemTick)
            && agent
                .last_continuation
                .as_ref()
                .is_some_and(|continuation| {
                    continuation.class == ContinuationClass::LivenessOnly
                        && !continuation.model_reentry
                }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn control_prompt_requires_bearer_token_for_non_loopback_auto() -> Result<()> {
    let config = test_config_with_paths(
        tempdir().unwrap().keep(),
        tempdir().unwrap().keep(),
        "0.0.0.0:0".into(),
        ControlAuthMode::Auto,
    );
    let (_host, base, server) = spawn_server_with_config(config).await?;
    let client = reqwest::Client::new();
    let denied = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    let allowed = client
        .post(format!("{base}/control/agents/default/prompt"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert!(allowed.status().is_success());
    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn control_prompt_records_message_admission_fields() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let accepted: serde_json::Value = response.json().await?;
    let message_id = accepted["message_id"]
        .as_str()
        .expect("control prompt should return message_id")
        .to_string();

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        let events = runtime.storage().read_recent_events(200)?;
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::OperatorPrompt
                && message.id == message_id
                && message.body
                    == MessageBody::Text {
                        text: "hello".into(),
                    }
                && message.delivery_surface == Some(MessageDeliverySurface::HttpControlPrompt)
                && message.admission_context == Some(AdmissionContext::LocalProcess)
                && message.authority_class == AuthorityClass::OperatorInstruction
                && message.priority == Priority::Interject
        }) && events.iter().any(|event| {
            event.kind == "message_admitted"
                && event.data["delivery_surface"] == "http_control_prompt"
                && event.data["admission_context"] == "local_process"
                && event.data["authority_class"] == "operator_instruction"
        }))
    })
    .await?;

    let state_response = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?;
    assert!(state_response.status().is_success());
    let state_payload: serde_json::Value = state_response.json().await?;
    assert!(state_payload.get("operator_messages").is_none());

    server.abort();
    Ok(())
}

pub async fn control_prompt_rejects_stopped_agent_without_queueing() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    runtime.control(ControlAction::Stop).await?;
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_agent()?
            .map(|agent| agent.status == AgentStatus::Stopped)
            .unwrap_or(false))
    })
    .await?;

    let response = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "hello after stop" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["code"], "agent_stopped");

    let stored = runtime.storage().read_recent_messages(10)?;
    assert!(!stored.iter().any(|message| {
        message.kind == MessageKind::OperatorPrompt
            && matches!(
                &message.body,
                holon::types::MessageBody::Text { text } if text == "hello after stop"
            )
    }));
    let queue_entries = runtime.storage().read_recent_queue_entries(10)?;
    assert!(queue_entries.is_empty());
    let state = runtime.storage().read_agent()?.expect("agent should exist");
    assert_eq!(state.pending, 0);

    server.abort();
    Ok(())
}

pub async fn stopped_status_includes_lifecycle_start_guidance() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    runtime.control(ControlAction::Stop).await?;
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_agent()?
            .map(|agent| agent.status == AgentStatus::Stopped)
            .unwrap_or(false))
    })
    .await?;

    let payload: serde_json::Value = client
        .get(format!("{base}/agents/default/status"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(payload["agent"]["status"], "stopped");
    assert_eq!(payload["lifecycle"]["accepts_external_messages"], false);
    let lifecycle = payload["lifecycle"]
        .as_object()
        .expect("lifecycle must be a JSON object");
    for removed_field in [
        "resume_required",
        "wake_requires_resume",
        "resume_cli_hint",
        "resume_control_path",
    ] {
        assert!(!lifecycle.contains_key(removed_field));
    }
    assert!(payload["lifecycle"]["operator_hint"]
        .as_str()
        .unwrap_or_default()
        .contains("start before new prompts or wakes"));

    server.abort();
    Ok(())
}

pub async fn control_wake_rejects_stopped_agent_with_start_guidance() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    runtime.control(ControlAction::Stop).await?;
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_agent()?
            .map(|agent| agent.status == AgentStatus::Stopped)
            .unwrap_or(false))
    })
    .await?;

    let response = client
        .post(format!("{base}/control/agents/default/wake"))
        .json(&serde_json::json!({ "reason": "check if alive" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["code"], "agent_stopped");
    assert!(payload["error"]
        .as_str()
        .unwrap_or_default()
        .contains("wake does not override stopped"));
    assert!(payload["hint"]
        .as_str()
        .unwrap_or_default()
        .contains("holon agent start default"));

    let events = runtime.storage().read_recent_events(20)?;
    assert!(!events.iter().any(|event| event.kind == "wake_requested"));
    assert!(!events
        .iter()
        .any(|event| event.kind == "system_tick_emitted"));

    server.abort();
    Ok(())
}

pub async fn control_start_restores_live_runtime_loop_for_stopped_agent() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    runtime.control(ControlAction::Stop).await?;
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_agent()?
            .map(|agent| agent.status == AgentStatus::Stopped)
            .unwrap_or(false))
    })
    .await?;

    let rejected = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "before start" }))
        .send()
        .await?;
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);

    let started = client
        .post(format!("{base}/control/agents/default/control"))
        .json(&serde_json::json!({ "action": "start" }))
        .send()
        .await?;
    assert!(started.status().is_success());

    let accepted = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "after start" }))
        .send()
        .await?;
    assert!(accepted.status().is_success());

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(20)?;
        let briefs = runtime.storage().read_recent_briefs(10)?;
        let state = runtime.storage().read_agent()?.expect("agent should exist");
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::OperatorPrompt
                && matches!(
                    &message.body,
                    holon::types::MessageBody::Text { text } if text == "after start"
                )
        }) && briefs
            .iter()
            .any(|brief| brief.text.contains("route result"))
            && state.pending == 0
            && state.status != AgentStatus::Stopped)
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn daemon_shutdown_restart_preserves_public_agent_http_runnability() -> Result<()> {
    let data_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    std::fs::create_dir_all(&workspace_dir)?;
    init_git_repo(&workspace_dir)?;

    let config = test_config_with_paths(
        data_dir,
        workspace_dir,
        "127.0.0.1:0".into(),
        ControlAuthMode::Auto,
    );
    let host = RuntimeHost::new_with_provider(
        config.clone(),
        Arc::new(StubProvider::new("route result")),
    )?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let (base, server) = spawn_server_for_host(host.clone()).await?;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "before shutdown" }))
        .send()
        .await?;
    assert!(first.status().is_success());

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("route result")))
    })
    .await?;

    host.shutdown().await?;
    server.abort();

    let persisted = runtime.storage().read_agent()?.expect("agent should exist");
    assert_ne!(persisted.status, AgentStatus::Stopped);

    let host2 =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("route result")))?;
    attach_default_workspace(&host2).await?;
    let runtime2 = host2.default_runtime().await?;
    let (base2, server2) = spawn_server_for_host(host2.clone()).await?;

    let second = client
        .post(format!("{base2}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "after restart" }))
        .send()
        .await?;
    assert!(second.status().is_success());

    wait_until(|| {
        let messages = runtime2.storage().read_recent_messages(20)?;
        let briefs = runtime2.storage().read_recent_briefs(10)?;
        let state = runtime2
            .storage()
            .read_agent()?
            .expect("agent should exist");
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::OperatorPrompt
                && matches!(
                    &message.body,
                    holon::types::MessageBody::Text { text } if text == "after restart"
                )
        }) && briefs
            .iter()
            .any(|brief| brief.text.contains("route result"))
            && state.status != AgentStatus::Stopped)
    })
    .await?;

    server2.abort();
    Ok(())
}

pub async fn runtime_status_route_reports_runtime_metadata() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/control/runtime/status"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert!(response.status().is_success());
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["healthy"], true);
    assert_eq!(payload["home_dir"], config.home_dir.display().to_string());
    assert_eq!(
        payload["socket_path"],
        config.socket_path.display().to_string()
    );
    assert_eq!(
        payload["startup_surface"]["home_dir"],
        config.home_dir.display().to_string()
    );
    assert_eq!(
        payload["startup_surface"]["socket_path"],
        config.socket_path.display().to_string()
    );
    assert_eq!(
        payload["startup_surface"]["workspace_dir"],
        config.workspace_dir.display().to_string()
    );
    assert_eq!(
        payload["startup_surface"]["default_agent_id"],
        config.default_agent_id
    );
    assert_eq!(
        payload["startup_surface"]["callback_base_url"],
        config.callback_base_url
    );
    assert_eq!(payload["startup_surface"]["control_token_configured"], true);
    assert_eq!(payload["startup_surface"]["control_auth_mode"], "auto");
    assert_eq!(
        payload["runtime_surface"]["model_default"],
        config.default_model.as_string()
    );
    assert!(payload["runtime_surface"]["model_fallbacks"]
        .as_array()
        .is_some());
    assert!(payload["runtime_surface"]["model_catalog"]
        .as_array()
        .is_some());
    assert_eq!(
        payload["runtime_surface"]["unknown_model_fallback_configured"],
        false
    );
    assert_eq!(
        payload["runtime_surface"]["runtime_max_output_tokens"],
        8192
    );
    assert_eq!(
        payload["runtime_surface"]["disable_provider_fallback"],
        false
    );
    assert!(payload.get("agent_model_overrides").is_none());
    assert!(payload["pid"].as_u64().unwrap_or_default() > 0);
    assert_eq!(payload["activity"]["state"], "idle");
    assert_eq!(payload["activity"]["active_agent_count"], 1);
    assert_eq!(payload["activity"]["active_task_count"], 0);
    assert_eq!(payload["activity"]["processing_agent_count"], 0);
    assert_eq!(payload["activity"]["waiting_agent_count"], 0);

    server.abort();
    Ok(())
}

pub async fn runtime_readiness_route_omits_activity_summary() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/control/runtime/readiness"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert!(response.status().is_success());
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["healthy"], true);
    assert_eq!(payload["home_dir"], config.home_dir.display().to_string());
    assert_eq!(
        payload["startup_surface"]["default_agent_id"],
        config.default_agent_id
    );
    assert_eq!(
        payload["runtime_surface"]["model_default"],
        config.default_model.as_string()
    );
    assert!(payload.get("activity").is_none());
    assert!(payload.get("last_failure").is_none());

    server.abort();
    Ok(())
}

pub async fn runtime_config_route_reads_and_updates_persisted_runtime_config() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = reqwest::Client::new();
    let read_response = client
        .get(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert!(read_response.status().is_success());
    let read_payload: serde_json::Value = read_response.json().await?;
    assert_eq!(read_payload["ok"], true);
    assert_eq!(
        read_payload["runtime_surface"]["model_default"],
        config.default_model.as_string()
    );

    let update_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "model.default", "value": "openai/gpt-4.1" },
                { "key": "home_dir", "value": "/tmp/other-home" }
            ]
        }))
        .send()
        .await?;
    assert!(update_response.status().is_success());
    let update_payload: serde_json::Value = update_response.json().await?;
    assert_eq!(update_payload["ok"], true);
    assert_eq!(update_payload["changed"], false);
    assert_eq!(update_payload["results"][0]["key"], "model.default");
    assert_eq!(update_payload["results"][0]["effect"], "rejected");
    assert_eq!(update_payload["results"][1]["key"], "home_dir");
    assert_eq!(update_payload["results"][1]["effect"], "rejected");
    assert_eq!(
        update_payload["runtime_surface"]["model_default"],
        config.default_model.as_string()
    );

    let persisted = load_persisted_config_at(&config.config_file_path)?;
    assert_eq!(persisted.model.default, None);

    let valid_model_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "model.default", "value": "openai/gpt-4.1" }
            ]
        }))
        .send()
        .await?;
    assert!(valid_model_response.status().is_success());
    let valid_model_payload: serde_json::Value = valid_model_response.json().await?;
    assert_eq!(valid_model_payload["changed"], true);
    assert_eq!(
        valid_model_payload["results"][0]["effect"],
        "accepted_reloaded"
    );
    assert_eq!(
        valid_model_payload["runtime_surface"]["model_default"],
        "openai/gpt-4.1"
    );
    assert_eq!(host.config().default_model.as_string(), "openai/gpt-4.1");

    let persisted = load_persisted_config_at(&config.config_file_path)?;
    assert_eq!(persisted.model.default.as_deref(), Some("openai/gpt-4.1"));

    let provider_config_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "providers.openai.auth.env", "value": "OPENAI_API_KEY_TEST" }
            ]
        }))
        .send()
        .await?;
    assert!(
        provider_config_response.status().is_success(),
        "valid provider config update failed: {:?}",
        provider_config_response.text().await?
    );
    let provider_config_payload: serde_json::Value = provider_config_response.json().await?;
    assert_eq!(provider_config_payload["changed"], true);
    assert_eq!(
        provider_config_payload["results"][0]["effect"],
        "accepted_reloaded"
    );
    assert_eq!(
        provider_config_payload["runtime_surface"]["providers"]
            .as_array()
            .and_then(|providers| providers.iter().find(|provider| provider["id"] == "openai"))
            .and_then(|provider| provider["credential_env"].as_str()),
        Some("OPENAI_API_KEY_TEST")
    );
    assert_eq!(
        provider_config_payload["runtime_surface"]["providers"]
            .as_array()
            .and_then(|providers| providers.iter().find(|provider| provider["id"] == "openai"))
            .and_then(|provider| provider["configured_in_config"].as_bool()),
        Some(true)
    );

    let provider_remove_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "providers.openai", "unset": true }
            ]
        }))
        .send()
        .await?;
    assert!(
        provider_remove_response.status().is_success(),
        "provider config removal failed: {:?}",
        provider_remove_response.text().await?
    );
    let provider_remove_payload: serde_json::Value = provider_remove_response.json().await?;
    assert_eq!(provider_remove_payload["changed"], true);
    assert_eq!(
        provider_remove_payload["results"][0]["effect"],
        "accepted_reloaded"
    );
    assert_eq!(
        provider_remove_payload["runtime_surface"]["providers"]
            .as_array()
            .and_then(|providers| providers.iter().find(|provider| provider["id"] == "openai"))
            .and_then(|provider| provider["configured_in_config"].as_bool()),
        Some(false)
    );

    let persisted = load_persisted_config_at(&config.config_file_path)?;
    assert!(!persisted
        .providers
        .contains_key(&holon::config::ProviderId::parse("openai")?));

    let valid_cors_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "api.cors.enabled", "value": true },
                { "key": "api.cors.allowed_origins", "value": ["http://192.168.1.10:5173"] },
                { "key": "api.cors.allowed_methods", "value": ["GET", "POST"] },
                { "key": "api.cors.allowed_headers", "value": ["content-type", "authorization"] },
                { "key": "api.cors.allow_credentials", "value": false },
                { "key": "api.cors.max_age_seconds", "value": 120 }
            ]
        }))
        .send()
        .await?;
    assert!(
        valid_cors_response.status().is_success(),
        "valid api.cors update failed: {:?}",
        valid_cors_response.text().await?
    );
    let valid_cors_payload: serde_json::Value = valid_cors_response.json().await?;
    assert_eq!(valid_cors_payload["changed"], true);
    assert_eq!(
        valid_cors_payload["results"][0]["effect"],
        "accepted_reloaded"
    );

    let persisted = load_persisted_config_at(&config.config_file_path)?;
    assert_eq!(persisted.api.cors.enabled, Some(true));
    assert_eq!(
        persisted.api.cors.allowed_origins,
        vec!["http://192.168.1.10:5173".to_string()]
    );
    assert_eq!(
        persisted.api.cors.allowed_methods,
        vec!["GET".to_string(), "POST".to_string()]
    );
    assert_eq!(
        persisted.api.cors.allowed_headers,
        vec!["content-type".to_string(), "authorization".to_string()]
    );
    assert_eq!(persisted.api.cors.allow_credentials, Some(false));
    assert_eq!(persisted.api.cors.max_age_seconds, Some(120));

    let valid_web_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "web.providers.review.kind", "value": "command" },
                { "key": "web.providers.review.command.argv", "value": ["echo", "{{query}}"] },
                { "key": "web.providers.review.output.format", "value": "json" }
            ]
        }))
        .send()
        .await?;
    assert!(
        valid_web_response.status().is_success(),
        "valid command provider update failed: {:?}",
        valid_web_response.text().await?
    );
    let valid_web_payload: serde_json::Value = valid_web_response.json().await?;
    assert_eq!(valid_web_payload["changed"], true);

    let invalid_web_response = client
        .patch(format!("http://{addr}/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "updates": [
                { "key": "model.default", "value": "openai/gpt-4.2" },
                { "key": "web.providers.review.kind", "value": "brave" }
            ]
        }))
        .send()
        .await?;
    assert!(invalid_web_response.status().is_success());
    let invalid_web_payload: serde_json::Value = invalid_web_response.json().await?;
    assert_eq!(invalid_web_payload["changed"], false);
    assert_eq!(invalid_web_payload["results"][0]["effect"], "rejected");
    assert_eq!(invalid_web_payload["results"][1]["effect"], "rejected");
    assert!(
        invalid_web_payload["results"][1]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("must not configure command.argv"),
        "unexpected invalid config reason: {invalid_web_payload}"
    );

    let persisted = load_persisted_config_at(&config.config_file_path)?;
    assert_eq!(persisted.model.default.as_deref(), Some("openai/gpt-4.1"));
    assert_eq!(
        persisted
            .web
            .providers
            .get("review")
            .map(|provider| provider.kind.as_str()),
        Some("command")
    );

    server.abort();
    Ok(())
}

pub async fn cors_preflight_allows_default_localhost_origins() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = reqwest::Client::new();
    for origin in [
        "http://localhost:5173",
        "https://localhost:5173",
        "http://127.0.0.1:3000",
        "http://[::1]:8080",
    ] {
        let allowed = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/control/runtime/status"),
            )
            .header("origin", origin)
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "authorization")
            .send()
            .await?;
        assert!(allowed.status().is_success());
        assert_eq!(
            allowed
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some(origin)
        );
    }

    let denied = client
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{addr}/control/runtime/status"),
        )
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "GET")
        .send()
        .await?;
    assert!(denied
        .headers()
        .get("access-control-allow-origin")
        .is_none());

    server.abort();
    Ok(())
}

pub async fn cors_preflight_allows_default_put_credentials_route() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let allowed = reqwest::Client::new()
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{addr}/control/runtime/credentials/test-profile"),
        )
        .header("origin", "http://localhost:5173")
        .header("access-control-request-method", "PUT")
        .header(
            "access-control-request-headers",
            "authorization,content-type",
        )
        .send()
        .await?;
    assert!(allowed.status().is_success());
    assert_eq!(
        allowed
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );
    assert!(allowed
        .headers()
        .get("access-control-allow-methods")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|methods| methods.contains("PUT")));

    server.abort();
    Ok(())
}

pub async fn cors_preflight_respects_configured_origin() -> Result<()> {
    let mut config = test_config();
    config.api_cors = ApiCorsConfigFile {
        enabled: Some(true),
        allowed_origins: vec!["http://192.168.1.10:5173".to_string()],
        allowed_methods: vec!["GET".to_string(), "POST".to_string()],
        allowed_headers: vec!["content-type".to_string(), "authorization".to_string()],
        allow_credentials: Some(false),
        max_age_seconds: Some(600),
    };
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = reqwest::Client::new();
    let allowed = client
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{addr}/control/runtime/status"),
        )
        .header("origin", "http://192.168.1.10:5173")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "authorization")
        .send()
        .await?;
    assert!(allowed.status().is_success());
    assert_eq!(
        allowed
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://192.168.1.10:5173")
    );
    assert_eq!(
        allowed
            .headers()
            .get("access-control-max-age")
            .and_then(|value| value.to_str().ok()),
        Some("600")
    );

    let denied = client
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{addr}/control/runtime/status"),
        )
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "GET")
        .send()
        .await?;
    assert!(denied
        .headers()
        .get("access-control-allow-origin")
        .is_none());

    server.abort();
    Ok(())
}

pub async fn runtime_status_route_reports_waiting_activity_summary() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let runtime = host.default_runtime().await?;
    let _task = runtime
        .schedule_command_task(
            "daemon status wait".into(),
            CommandTaskSpec {
                cmd: "sleep 1".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::OperatorInstruction,
        )
        .await?;

    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !runtime.active_tasks(10).await?.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for background task to become visible");
        }
        sleep(Duration::from_millis(50)).await;
    }

    let response = client
        .get(format!("http://{addr}/control/runtime/status"))
        .bearer_auth("secret")
        .send()
        .await?;
    assert!(response.status().is_success());
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["activity"]["state"], "waiting");
    assert!(
        payload["activity"]["active_task_count"]
            .as_u64()
            .unwrap_or_default()
            >= 1
    );
    assert!(
        payload["activity"]["waiting_agent_count"]
            .as_u64()
            .unwrap_or_default()
            >= 1
    );

    server.abort();
    Ok(())
}

pub async fn runtime_status_route_reports_last_runtime_failure_summary() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(RuntimeFailureProvider))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let runtime = host.default_runtime().await?;
    runtime
        .enqueue(holon::types::MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            holon::types::MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            holon::types::Priority::Normal,
            holon::types::MessageBody::Text {
                text: "trigger runtime failure".into(),
            },
        ))
        .await?;

    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = client
            .get(format!("http://{addr}/control/runtime/status"))
            .bearer_auth("secret")
            .send()
            .await?;
        let payload: serde_json::Value = response.json().await?;
        if payload.get("last_failure").is_some() && !payload["last_failure"].is_null() {
            assert_eq!(payload["last_failure"]["phase"], "runtime_turn");
            assert!(payload["last_failure"]["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("provider transport broke"));
            assert_eq!(
                payload["last_failure"]["detail_hint"],
                "run `holon daemon logs` for details"
            );
            server.abort();
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for runtime status last_failure");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub async fn runtime_shutdown_route_requests_shutdown() -> Result<()> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("ok")))?;
    attach_default_workspace(&host).await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;
    let mut shutdown = runtime_service.shutdown_signal();
    let router: Router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{addr}/control/runtime/shutdown"))
        .bearer_auth("secret")
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert!(response.status().is_success());
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["shutdown_requested"], true);

    shutdown.changed().await?;
    assert!(*shutdown.borrow());

    server.abort();
    Ok(())
}
