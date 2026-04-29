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
    config::{AppConfig, ControlAuthMode},
    daemon::RuntimeServiceHandle,
    host::RuntimeHost,
    http::{self, AppState},
    provider::{AgentProvider, StubProvider},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentStatus, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, CommandTaskSpec, ContinuationClass, ControlAction,
        MessageDeliverySurface, MessageKind, MessageOrigin, TrustLevel, WorkItemStatus,
        WorkPlanItem, WorkPlanStepStatus,
    },
};
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::time::{sleep, Duration, Instant};

use super::{
    attach_default_workspace, connect_addr, git, init_git_repo, spawn_server,
    spawn_server_for_host, spawn_server_with_config, spawn_server_with_runtime_config,
    spawn_unix_server, tempdir, test_config, test_config_with_paths, unix_request, wait_until,
    RuntimeFailureProvider,
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
    assert!(state_payload["session"].is_object());
    assert!(state_payload["tasks"].is_array());
    assert!(state_payload["transcript_tail"].is_array());
    assert!(state_payload["briefs_tail"].is_array());
    assert!(state_payload["timers"].is_array());
    assert!(state_payload["work_items"].is_array());
    assert!(state_payload["work_plan"].is_null());
    assert!(state_payload["waiting_intents"].is_array());
    assert!(state_payload["external_triggers"].is_array());
    assert!(state_payload["workspace"].is_object());
    assert!(state_payload["cursor"].is_string());

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

    let work_item = runtime
        .update_work_item(
            None,
            "bootstrap active work".into(),
            WorkItemStatus::Active,
            Some("active projection work".into()),
            Some("shape /state".into()),
            None,
        )
        .await?;
    runtime
        .update_work_plan(
            work_item.id.clone(),
            vec![WorkPlanItem {
                step: "expand /state".into(),
                status: WorkPlanStepStatus::InProgress,
            }],
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
            .any(|item| item.id == work_item.id && item.status != WorkItemStatus::Completed),
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
                    && item["status"] != serde_json::Value::String("completed".into())
            }))
            .unwrap_or(false),
        "raw snapshot missing expected work item: {}",
        raw_snapshot
    );

    let snapshot = local_client.agent_state_snapshot("default").await?;
    assert!(!snapshot.briefs_tail.is_empty());
    assert_eq!(
        snapshot.brief.as_ref().map(|brief| brief.id.clone()),
        snapshot.briefs_tail.last().map(|brief| brief.id.clone())
    );
    assert!(snapshot
        .timers
        .iter()
        .any(|timer| timer.summary.as_deref() == Some("state timer")));
    assert!(snapshot
        .work_items
        .iter()
        .any(|item| item.id == work_item.id && item.status != WorkItemStatus::Completed));
    assert_eq!(
        snapshot
            .work_plan
            .as_ref()
            .map(|plan| plan.work_item_id.clone()),
        Some(work_item.id.clone())
    );
    assert_eq!(snapshot.waiting_intents.len(), 1);
    assert_eq!(snapshot.external_triggers.len(), 1);
    assert_eq!(
        snapshot.external_triggers[0].external_trigger_id,
        snapshot.waiting_intents[0].external_trigger_id
    );
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
    assert!(snapshot.cursor.is_some());

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
                        && !continuation.model_visible
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

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        let events = runtime.storage().read_recent_events(20)?;
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::OperatorPrompt
                && message.delivery_surface == Some(MessageDeliverySurface::HttpControlPrompt)
                && message.admission_context == Some(AdmissionContext::LocalProcess)
                && message.authority_class == AuthorityClass::OperatorInstruction
        }) && events.iter().any(|event| {
            event.kind == "message_admitted"
                && event.data["delivery_surface"] == "http_control_prompt"
                && event.data["admission_context"] == "local_process"
                && event.data["authority_class"] == "operator_instruction"
        }))
    })
    .await?;

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

pub async fn stopped_status_includes_lifecycle_resume_guidance() -> Result<()> {
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
    assert_eq!(payload["lifecycle"]["resume_required"], true);
    assert_eq!(payload["lifecycle"]["accepts_external_messages"], false);
    assert_eq!(payload["lifecycle"]["wake_requires_resume"], true);
    assert_eq!(
        payload["lifecycle"]["resume_cli_hint"],
        "holon control resume --agent default"
    );
    assert_eq!(
        payload["lifecycle"]["resume_control_path"],
        "/control/agents/default/control"
    );
    assert!(payload["lifecycle"]["operator_hint"]
        .as_str()
        .unwrap_or_default()
        .contains("resume before new prompts or wakes"));

    server.abort();
    Ok(())
}

pub async fn control_wake_rejects_stopped_agent_with_resume_guidance() -> Result<()> {
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
        .contains("holon control resume --agent default"));

    let events = runtime.storage().read_recent_events(20)?;
    assert!(!events.iter().any(|event| event.kind == "wake_requested"));
    assert!(!events
        .iter()
        .any(|event| event.kind == "system_tick_emitted"));

    server.abort();
    Ok(())
}

pub async fn control_resume_restores_live_runtime_loop_for_stopped_agent() -> Result<()> {
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
        .json(&serde_json::json!({ "text": "before resume" }))
        .send()
        .await?;
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);

    let resumed = client
        .post(format!("{base}/control/agents/default/control"))
        .json(&serde_json::json!({ "action": "resume" }))
        .send()
        .await?;
    assert!(resumed.status().is_success());

    let accepted = client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "after resume" }))
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
                    holon::types::MessageBody::Text { text } if text == "after resume"
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
    assert!(payload["agent_model_overrides"].as_array().is_some());
    assert!(payload["pid"].as_u64().unwrap_or_default() > 0);
    assert_eq!(payload["activity"]["state"], "idle");
    assert_eq!(payload["activity"]["active_agent_count"], 1);
    assert_eq!(payload["activity"]["active_task_count"], 0);
    assert_eq!(payload["activity"]["processing_agent_count"], 0);
    assert_eq!(payload["activity"]["waiting_agent_count"], 0);

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
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = runtime.agent_state().await?;
        if !state.active_task_ids.is_empty() {
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
            TrustLevel::TrustedOperator,
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
