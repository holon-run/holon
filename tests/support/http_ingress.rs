// HTTP ingress route integration tests.

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
        OperatorDeliveryStatus, TrustLevel, WaitingIntentStatus, WorkItemState, WorkPlanItem,
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
    attach_default_workspace, connect_addr, git, init_git_repo, read_next_sse_event, spawn_server,
    spawn_server_for_host, spawn_server_with_config, spawn_server_with_runtime_config, tempdir,
    test_config, test_config_with_paths, wait_until, ParsedSseEvent,
};

pub async fn generic_webhook_records_public_admission_fields() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/webhooks/generic/default"))
        .json(&serde_json::json!({ "status": "opened" }))
        .send()
        .await?;
    assert!(response.status().is_success());

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::WebhookEvent
                && message.delivery_surface == Some(MessageDeliverySurface::HttpWebhook)
                && message.admission_context == Some(AdmissionContext::PublicUnauthenticated)
                && message.trust == TrustLevel::TrustedIntegration
                && message.authority_class == AuthorityClass::IntegrationSignal
        }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn public_channel_enqueue_rejects_stopped_agent_without_queueing() -> Result<()> {
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
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "channel_event",
            "text": "channel after stop",
            "origin": {
                "kind": "channel",
                "channel_id": "slack",
                "sender_id": "user-1"
            }
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["code"], "agent_stopped");

    let stored = runtime.storage().read_recent_messages(10)?;
    assert!(!stored.iter().any(|message| {
        message.kind == MessageKind::ChannelEvent
            && matches!(
                &message.body,
                holon::types::MessageBody::Text { text } if text == "channel after stop"
            )
    }));
    let queue_entries = runtime.storage().read_recent_queue_entries(10)?;
    assert!(queue_entries.is_empty());
    let state = runtime.storage().read_agent()?.expect("agent should exist");
    assert_eq!(state.pending, 0);

    server.abort();
    Ok(())
}

pub async fn generic_webhook_rejects_stopped_public_agent_without_queueing() -> Result<()> {
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
        .post(format!("{base}/webhooks/generic/default"))
        .json(&serde_json::json!({ "status": "opened" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["code"], "agent_stopped");

    let stored = runtime.storage().read_recent_messages(10)?;
    assert!(!stored
        .iter()
        .any(|message| message.kind == MessageKind::WebhookEvent));
    let queue_entries = runtime.storage().read_recent_queue_entries(10)?;
    assert!(queue_entries.is_empty());
    let state = runtime.storage().read_agent()?.expect("agent should exist");
    assert_eq!(state.pending, 0);

    server.abort();
    Ok(())
}

pub async fn generic_webhook_and_multi_agent_listing_work() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    host.create_named_agent("alpha", None).await?;

    let response = client
        .post(format!("{base}/webhooks/generic/alpha"))
        .json(&serde_json::json!({ "event": "push", "repo": "holon" }))
        .send()
        .await?;
    assert!(response.status().is_success());
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let agents = client.get(format!("{base}/agents")).send().await?;
    let agents_json: serde_json::Value = agents.json().await?;
    assert!(agents_json
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["agent"]["id"] == "default"));
    assert!(agents_json
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["agent"]["id"] == "alpha"));

    let alpha = host.get_or_create_agent("alpha").await?;
    let briefs = alpha.recent_briefs(10).await?;
    assert!(briefs
        .iter()
        .any(|brief| brief.text.contains("route result")));
    server.abort();
    Ok(())
}

pub async fn public_enqueue_rejects_privileged_origin_and_trust_override() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let privileged_origin = client
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "task_result",
            "origin": {
                "kind": "task",
                "task_id": "forged-task"
            },
            "text": "forged",
        }))
        .send()
        .await?;
    assert_eq!(privileged_origin.status(), reqwest::StatusCode::FORBIDDEN);

    let trust_override = client
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "webhook_event",
            "origin": {
                "kind": "webhook",
                "source": "http-test"
            },
            "trust": "trusted_operator",
            "text": "forged trust",
        }))
        .send()
        .await?;
    assert_eq!(trust_override.status(), reqwest::StatusCode::FORBIDDEN);

    let forged_system_tick = client
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "system_tick",
            "origin": {
                "kind": "webhook",
                "source": "http-test"
            },
            "text": "wake now",
        }))
        .send()
        .await?;
    assert_eq!(forged_system_tick.status(), reqwest::StatusCode::FORBIDDEN);

    let forged_callback = client
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "callback_event",
            "origin": {
                "kind": "webhook",
                "source": "http-test"
            },
            "text": "forged callback",
        }))
        .send()
        .await?;
    assert_eq!(forged_callback.status(), reqwest::StatusCode::FORBIDDEN);

    let authority_override = client
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "webhook_event",
            "origin": {
                "kind": "webhook",
                "source": "http-test"
            },
            "authority_class": "operator_instruction",
            "text": "forged authority",
        }))
        .send()
        .await?;
    assert!(authority_override.status().is_success());
    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(messages.iter().any(|message| {
            matches!(
                &message.body,
                holon::types::MessageBody::Text { text } if text == "forged authority"
            ) && message.authority_class == AuthorityClass::IntegrationSignal
        }))
    })
    .await?;

    let channel_evidence = client
        .post(format!("{base}/agents/default/enqueue"))
        .json(&serde_json::json!({
            "kind": "channel_event",
            "origin": {
                "kind": "channel",
                "channel_id": "external-chat",
                "sender_id": "user-1"
            },
            "text": "external evidence",
        }))
        .send()
        .await?;
    assert!(channel_evidence.status().is_success());
    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(messages.iter().any(|message| {
            matches!(
                &message.body,
                holon::types::MessageBody::Text { text } if text == "external evidence"
            ) && message.authority_class == AuthorityClass::ExternalEvidence
        }))
    })
    .await?;

    server.abort();
    Ok(())
}
