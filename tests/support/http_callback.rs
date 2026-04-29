// HTTP callback route integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::Result;
use holon::{
    client::{EventStreamRequest, LocalClient},
    config::{AppConfig, ControlAuthMode},
    daemon::RuntimeServiceHandle,
    host::RuntimeHost,
    http::{self, AppState},
    provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse, StubProvider},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentStatus, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, CommandTaskSpec, ContinuationClass, ControlAction,
        ExternalTriggerStatus, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
        OperatorDeliveryStatus, TrustLevel, WaitingIntentStatus, WorkItemStatus, WorkPlanItem,
        WorkPlanStepStatus,
    },
};
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::{sleep, Duration, Instant};
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use super::{
    attach_default_workspace, callback_path, callback_token, connect_addr, git, init_git_repo,
    spawn_delivery_callback, spawn_server, spawn_server_for_host, spawn_server_with_config,
    spawn_server_with_runtime_config, tempdir, test_config, test_config_with_paths, wait_until,
    DeliveryCallbackRecord,
};

pub async fn callback_enqueue_message_repeats_until_cancelled() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    runtime.control(ControlAction::Pause).await?;
    let mut callback_work = holon::types::WorkItemRecord::new(
        "default",
        "track CI callback delivery",
        WorkItemStatus::Active,
    );
    callback_work.summary = Some("keep callback watch anchored".into());
    runtime.storage().append_work_item(&callback_work)?;
    let capability = runtime
        .create_callback(
            "wait for CI callback".into(),
            "github".into(),
            "required_checks_passed".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::EnqueueMessage,
        )
        .await?;
    assert!(capability.trigger_url.contains("/callbacks/enqueue/"));
    let callback_path = callback_path(&capability.trigger_url);
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}{callback_path}"))
        .json(&serde_json::json!({ "status": "checks_passed" }))
        .send()
        .await?;
    assert!(first.status().is_success());
    let first_payload: serde_json::Value = first.json().await?;
    assert_eq!(first_payload["disposition"], "enqueued");
    assert_eq!(
        first_payload["external_trigger_id"].as_str(),
        Some(capability.external_trigger_id.as_str())
    );

    let second = client
        .post(format!("{base}{callback_path}"))
        .body("review approved")
        .header("content-type", "text/plain")
        .send()
        .await?;
    assert!(second.status().is_success());

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(20)?;
        let waiting = runtime.storage().latest_waiting_intents()?;
        let descriptors = runtime.storage().latest_external_triggers()?;
        Ok(messages
            .iter()
            .filter(|message| message.kind == MessageKind::CallbackEvent)
            .count()
            >= 2
            && waiting
                .first()
                .map(|item| item.trigger_count == 2)
                .unwrap_or(false)
            && descriptors
                .first()
                .map(|item| item.delivery_count == 2)
                .unwrap_or(false))
    })
    .await?;

    let messages = runtime.storage().read_recent_messages(20)?;
    let callback_message = messages
        .iter()
        .find(|message| message.kind == MessageKind::CallbackEvent)
        .expect("callback message");
    assert_eq!(
        callback_message.delivery_surface,
        Some(MessageDeliverySurface::HttpCallbackEnqueue)
    );
    assert_eq!(
        callback_message.admission_context,
        Some(AdmissionContext::ExternalTriggerCapability)
    );
    assert_eq!(
        callback_message.authority_class,
        AuthorityClass::IntegrationSignal
    );
    assert_eq!(
        callback_message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata["external_trigger_id"].as_str()),
        Some(capability.external_trigger_id.as_str())
    );

    let events = runtime.storage().read_recent_events(20)?;
    let delivered = events
        .iter()
        .rev()
        .find(|event| event.kind == "callback_delivered")
        .expect("callback delivered event");
    assert_eq!(delivered.data["origin"], "callback");
    assert_eq!(delivered.data["delivery_surface"], "http_callback_enqueue");
    assert_eq!(
        delivered.data["admission_context"],
        "external_trigger_capability"
    );
    assert_eq!(delivered.data["authority_class"], "integration_signal");
    assert_eq!(
        delivered.data["external_trigger_id"].as_str(),
        Some(capability.external_trigger_id.as_str())
    );

    runtime
        .cancel_waiting(&capability.waiting_intent_id)
        .await?;
    let revoked = client
        .post(format!("{base}{callback_path}"))
        .body("should fail")
        .header("content-type", "text/plain")
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::FORBIDDEN);

    let waiting = runtime.latest_waiting_intents().await?;
    let descriptors = runtime.latest_external_triggers().await?;
    assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
    assert_eq!(descriptors[0].status, ExternalTriggerStatus::Revoked);

    server.abort();
    Ok(())
}

pub async fn callback_wake_only_routes_through_wake_hint() -> Result<()> {
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

    let capability = runtime
        .create_callback(
            "wake when PR changes".into(),
            "github".into(),
            "pr_state_changed".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await?;
    assert!(capability.trigger_url.contains("/callbacks/wake/"));
    let callback_path = callback_path(&capability.trigger_url);

    let response = client
        .post(format!("{base}{callback_path}"))
        .json(&serde_json::json!({
            "notification_type": "pr_changed",
            "repo": "holon"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["disposition"], "triggered");

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("route result"))
            && messages
                .iter()
                .any(|message| message.kind == MessageKind::SystemTick)
            && messages
                .iter()
                .all(|message| message.kind != MessageKind::CallbackEvent))
    })
    .await?;

    let events = runtime.storage().read_recent_events(20)?;
    let delivered = events
        .iter()
        .rev()
        .find(|event| event.kind == "callback_delivered")
        .expect("callback delivered event");
    assert_eq!(delivered.data["origin"], "callback");
    assert_eq!(delivered.data["delivery_surface"], "http_callback_wake");
    assert_eq!(
        delivered.data["admission_context"],
        "external_trigger_capability"
    );
    assert_eq!(delivered.data["authority_class"], "integration_signal");

    server.abort();
    Ok(())
}

pub async fn callback_wake_only_rejects_stopped_public_agent_without_side_effects() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let capability = runtime
        .create_callback(
            "wake when PR changes".into(),
            "github".into(),
            "pr_state_changed".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await?;
    let callback_path = callback_path(&capability.trigger_url);

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
        .post(format!("{base}{callback_path}"))
        .json(&serde_json::json!({
            "notification_type": "pr_changed",
            "repo": "holon"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["code"], "agent_stopped");

    let waiting = runtime.latest_waiting_intents().await?;
    let descriptors = runtime.latest_external_triggers().await?;
    assert_eq!(waiting[0].trigger_count, 0);
    assert_eq!(descriptors[0].delivery_count, 0);

    let events = runtime.storage().read_recent_events(20)?;
    assert!(!events.iter().any(|event| event.kind == "wake_requested"));
    assert!(!events
        .iter()
        .any(|event| event.kind == "system_tick_emitted"));

    server.abort();
    Ok(())
}

pub async fn unknown_callback_token_is_rejected() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/callbacks/enqueue/not-a-real-token"))
        .json(&serde_json::json!({ "hello": "world" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

pub async fn callback_mode_mismatch_is_rejected() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let capability = runtime
        .create_callback(
            "wait for CI callback".into(),
            "github".into(),
            "required_checks_passed".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::EnqueueMessage,
        )
        .await?;
    let token = callback_token(&capability.trigger_url);
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/callbacks/wake/{token}"))
        .json(&serde_json::json!({ "status": "checks_passed" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

pub async fn invalid_json_callback_body_returns_bad_request() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let capability = runtime
        .create_callback(
            "wait for CI callback".into(),
            "github".into(),
            "required_checks_passed".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::EnqueueMessage,
        )
        .await?;
    let callback_path = callback_path(&capability.trigger_url);
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}{callback_path}"))
        .header("content-type", "application/json")
        .body("{invalid json")
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    server.abort();
    Ok(())
}

pub async fn wake_callback_without_content_type_accepts_binary_body() -> Result<()> {
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

    let capability = runtime
        .create_callback(
            "wake when binary payload arrives".into(),
            "agentinbox".into(),
            "binary_notification".into(),
            Some("interest/bin".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await?;
    let callback_path = callback_path(&capability.trigger_url);

    let response = client
        .post(format!("{base}{callback_path}"))
        .body(vec![0_u8, 159, 255, 42])
        .send()
        .await?;
    assert!(response.status().is_success());

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("wake_hint"))
                    .and_then(|value| value.get("body"))
                    .and_then(|value| value.get("value"))
                    .and_then(|value| value.get("body_base64"))
                    .and_then(|value| value.as_str())
                    .is_some()
        }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn callback_enqueue_rejects_stopped_public_agent_after_restart() -> Result<()> {
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
    let runtime = host.default_runtime().await?;
    runtime.control(ControlAction::Pause).await?;
    let mut callback_work = holon::types::WorkItemRecord::new(
        "default",
        "track public callback delivery",
        WorkItemStatus::Active,
    );
    callback_work.summary = Some("keep public callback watch anchored".into());
    runtime.storage().append_work_item(&callback_work)?;
    let (base, server) = spawn_server_for_host(host.clone()).await?;
    let client = reqwest::Client::new();

    let capability = runtime
        .create_callback(
            "wait for CI callback".into(),
            "github".into(),
            "required_checks_passed".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::EnqueueMessage,
        )
        .await?;
    let callback_path = callback_path(&capability.trigger_url);

    let first = client
        .post(format!("{base}{callback_path}"))
        .json(&serde_json::json!({ "status": "checks_passed" }))
        .send()
        .await?;
    assert!(first.status().is_success());
    let first_payload: serde_json::Value = first.json().await?;
    assert_eq!(first_payload["disposition"], "enqueued");

    wait_until(|| {
        let waiting = runtime.storage().latest_waiting_intents()?;
        let descriptors = runtime.storage().latest_external_triggers()?;
        Ok(waiting
            .first()
            .map(|item| item.trigger_count == 1)
            .unwrap_or(false)
            && descriptors
                .first()
                .map(|item| item.delivery_count == 1)
                .unwrap_or(false))
    })
    .await?;

    server.abort();
    runtime.control(holon::types::ControlAction::Stop).await?;
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_agent()?
            .map(|agent| agent.status == holon::types::AgentStatus::Stopped)
            .unwrap_or(false))
    })
    .await?;
    drop(runtime);
    drop(host);

    let host2 =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("route result")))?;
    let runtime2 = host2.default_runtime().await?;
    let (base2, server2) = spawn_server_for_host(host2.clone()).await?;

    let waiting = runtime2.latest_waiting_intents().await?;
    let descriptors = runtime2.latest_external_triggers().await?;
    assert_eq!(waiting.len(), 1);
    assert_eq!(descriptors.len(), 1);
    assert_eq!(waiting[0].id, capability.waiting_intent_id);
    assert_eq!(waiting[0].trigger_count, 1);
    assert_eq!(descriptors[0].delivery_count, 1);

    let second = client
        .post(format!("{base2}{callback_path}"))
        .json(&serde_json::json!({ "status": "still_works" }))
        .send()
        .await?;
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
    let second_payload: serde_json::Value = second.json().await?;
    assert_eq!(second_payload["code"], "agent_stopped");
    assert!(second_payload["error"]
        .as_str()
        .unwrap_or_default()
        .contains("resume first"));

    wait_until(|| {
        let messages = runtime2.storage().read_recent_messages(20)?;
        let descriptors = runtime2.storage().latest_external_triggers()?;
        Ok(messages
            .iter()
            .filter(|message| message.kind == MessageKind::CallbackEvent)
            .count()
            == 1
            && descriptors
                .first()
                .map(|item| item.delivery_count == 1)
                .unwrap_or(false))
    })
    .await?;

    runtime2
        .cancel_waiting(&capability.waiting_intent_id)
        .await?;

    server2.abort();
    Ok(())
}
