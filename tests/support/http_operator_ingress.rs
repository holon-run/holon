// HTTP operator ingress integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
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
    attach_default_workspace, connect_addr, create_operator_transport_binding, git, init_git_repo,
    spawn_delivery_callback, spawn_server, spawn_server_for_host, spawn_server_with_config,
    spawn_server_with_runtime_config, spawn_unix_server, tempdir, test_config,
    test_config_with_paths, wait_until, DeliveryCallbackRecord,
};

pub async fn operator_ingress_records_remote_operator_provenance() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    create_operator_transport_binding(
        &client,
        &base,
        "opbind-ingress",
        "http://127.0.0.1:1/delivery",
    )
    .await?;

    let response = client
        .post(format!("{base}/control/agents/default/operator-ingress"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "text": "remote operator continue",
            "actor_id": "operator:jolestar",
            "binding_id": "opbind-ingress",
            "reply_route_id": "route-reply-1",
            "provider": "agentinbox",
            "upstream_provider": "telegram",
            "provider_message_ref": "telegram:msg:123",
            "correlation_id": "corr-remote-1",
            "causation_id": "cause-remote-1",
            "metadata": {
                "conversation_ref": "telegram:dm:operator"
            }
        }))
        .send()
        .await?;
    assert!(
        response.status().is_success(),
        "{:?}",
        response.text().await?
    );

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(messages.iter().any(|message| {
            matches!(&message.body, MessageBody::Text { text } if text == "remote operator continue")
        }))
    })
    .await?;

    let messages = runtime.storage().read_recent_messages(10)?;
    let message = messages
        .iter()
        .find(|message| {
            matches!(&message.body, MessageBody::Text { text } if text == "remote operator continue")
        })
        .expect("remote operator message should be stored");
    assert_eq!(message.kind, MessageKind::OperatorPrompt);
    assert_eq!(
        message.origin,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into())
        }
    );
    assert_eq!(message.trust, TrustLevel::TrustedOperator);
    assert_eq!(message.authority_class, AuthorityClass::OperatorInstruction);
    assert_eq!(
        message.delivery_surface,
        Some(MessageDeliverySurface::RemoteOperatorTransport)
    );
    assert_eq!(
        message.admission_context,
        Some(AdmissionContext::OperatorTransportAuthenticated)
    );
    assert_eq!(message.correlation_id.as_deref(), Some("corr-remote-1"));
    assert_eq!(message.causation_id.as_deref(), Some("cause-remote-1"));
    let transport = &message
        .metadata
        .as_ref()
        .expect("metadata should be present")["operator_transport"];
    assert_eq!(transport["binding_id"], "opbind-ingress");
    assert_eq!(transport["transport"], "agentinbox");
    assert_eq!(transport["reply_route_id"], "route-reply-1");
    assert_eq!(transport["provider"], "agentinbox");
    assert_eq!(
        transport["provider_identity_ref"],
        "agentinbox:operator:jolestar"
    );
    assert_eq!(transport["upstream_provider"], "telegram");
    assert_eq!(transport["provider_message_ref"], "telegram:msg:123");
    assert_eq!(
        transport["metadata"]["conversation_ref"],
        "telegram:dm:operator"
    );
    let bindings = runtime.latest_operator_transport_bindings().await?;
    let binding = bindings
        .iter()
        .find(|binding| binding.binding_id == "opbind-ingress")
        .expect("binding should be stored");
    assert!(binding.last_seen_at.is_some());

    server.abort();
    Ok(())
}

pub async fn operator_ingress_defaults_provider_provenance_from_binding() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    create_operator_transport_binding(
        &client,
        &base,
        "opbind-provider-default",
        "http://127.0.0.1:1/delivery",
    )
    .await?;

    let response = client
        .post(format!("{base}/control/agents/default/operator-ingress"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "text": "remote operator without provider",
            "actor_id": "operator:jolestar",
            "binding_id": "opbind-provider-default"
        }))
        .send()
        .await?;
    assert!(
        response.status().is_success(),
        "{:?}",
        response.text().await?
    );

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(10)?;
        Ok(messages.iter().any(|message| {
            matches!(&message.body, MessageBody::Text { text } if text == "remote operator without provider")
        }))
    })
    .await?;
    let messages = runtime.storage().read_recent_messages(10)?;
    let message = messages
        .iter()
        .find(|message| {
            matches!(&message.body, MessageBody::Text { text } if text == "remote operator without provider")
        })
        .expect("remote operator message should be stored");
    let transport = &message
        .metadata
        .as_ref()
        .expect("metadata should be present")["operator_transport"];
    assert_eq!(transport["provider"], "agentinbox");

    server.abort();
    Ok(())
}

pub async fn operator_ingress_requires_control_auth() -> Result<()> {
    let config = test_config_with_paths(
        tempdir().unwrap().keep(),
        tempdir().unwrap().keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    let (_host, base, server) = spawn_server_with_config(config).await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/operator-ingress"))
        .json(&serde_json::json!({
            "text": "unauthenticated remote operator",
            "actor_id": "operator:jolestar",
            "binding_id": "opbind-missing-auth"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

pub async fn operator_transport_binding_validates_delivery_auth_and_redacts_audit() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let missing_bearer = client
        .post(format!("{base}/control/agents/default/operator-bindings"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "binding_id": "opbind-missing-bearer-token",
            "transport": "agentinbox",
            "operator_actor_id": "operator:jolestar",
            "default_route_id": "route-default",
            "delivery_callback_url": "http://127.0.0.1:1/delivery",
            "delivery_auth": {
                "kind": "bearer"
            },
            "capabilities": {
                "text": true
            }
        }))
        .send()
        .await?;
    assert_eq!(missing_bearer.status(), reqwest::StatusCode::BAD_REQUEST);

    let hmac = client
        .post(format!("{base}/control/agents/default/operator-bindings"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "binding_id": "opbind-hmac",
            "transport": "agentinbox",
            "operator_actor_id": "operator:jolestar",
            "default_route_id": "route-default",
            "delivery_callback_url": "http://127.0.0.1:1/delivery",
            "delivery_auth": {
                "kind": "hmac",
                "key_id": "test-key"
            },
            "capabilities": {
                "text": true
            }
        }))
        .send()
        .await?;
    assert_eq!(hmac.status(), reqwest::StatusCode::BAD_REQUEST);

    create_operator_transport_binding(
        &client,
        &base,
        "opbind-redacted-audit",
        "http://127.0.0.1:1/delivery",
    )
    .await?;
    let events = runtime.storage().read_recent_events(20)?;
    let event = events
        .iter()
        .find(|event| {
            event.kind == "operator_transport_binding_upserted"
                && event.data["binding_id"] == "opbind-redacted-audit"
        })
        .expect("binding audit event should be stored");
    assert_eq!(event.data["delivery_auth"]["kind"], "bearer");
    assert!(event.data["delivery_auth"]["bearer_token"].is_null());

    server.abort();
    Ok(())
}

pub async fn operator_ingress_validates_binding_and_actor() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let missing = client
        .post(format!("{base}/control/agents/default/operator-ingress"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "text": "missing binding",
            "actor_id": "operator:jolestar",
            "binding_id": "opbind-does-not-exist"
        }))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::FORBIDDEN);

    create_operator_transport_binding(
        &client,
        &base,
        "opbind-actor-check",
        "http://127.0.0.1:1/delivery",
    )
    .await?;
    let wrong_actor = client
        .post(format!("{base}/control/agents/default/operator-ingress"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "text": "wrong actor",
            "actor_id": "operator:someone-else",
            "binding_id": "opbind-actor-check"
        }))
        .send()
        .await?;
    assert_eq!(wrong_actor.status(), reqwest::StatusCode::FORBIDDEN);

    let stored = runtime.storage().read_recent_messages(10)?;
    assert!(!stored.iter().any(|message| {
        matches!(
            &message.body,
            MessageBody::Text { text } if text == "missing binding" || text == "wrong actor"
        )
    }));

    server.abort();
    Ok(())
}

pub async fn operator_ingress_rejects_stopped_agent_without_queueing() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    create_operator_transport_binding(
        &client,
        &base,
        "opbind-stopped",
        "http://127.0.0.1:1/delivery",
    )
    .await?;

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
        .post(format!("{base}/control/agents/default/operator-ingress"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "text": "remote after stop",
            "actor_id": "operator:jolestar",
            "binding_id": "opbind-stopped"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["code"], "agent_stopped");

    let stored = runtime.storage().read_recent_messages(10)?;
    assert!(!stored.iter().any(|message| {
        matches!(
            &message.body,
            MessageBody::Text { text } if text == "remote after stop"
        )
    }));
    assert!(runtime.storage().read_recent_queue_entries(10)?.is_empty());

    server.abort();
    Ok(())
}

pub async fn operator_notification_delivery_callback_records_acceptance() -> Result<()> {
    let (callback_url, records, callback_server) =
        spawn_delivery_callback(axum::http::StatusCode::ACCEPTED).await?;
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    create_operator_transport_binding(&client, &base, "opbind-delivery-ok", &callback_url).await?;

    runtime
        .notify_operator("deliver this operator note".into())
        .await?;

    wait_until(|| {
        Ok(!records
            .lock()
            .expect("delivery callback records lock")
            .is_empty())
    })
    .await?;
    let callback = records
        .lock()
        .expect("delivery callback records lock")
        .first()
        .cloned()
        .expect("delivery callback should be recorded");
    assert_eq!(
        callback.authorization.as_deref(),
        Some("Bearer delivery-secret")
    );
    assert!(callback
        .idempotency_key
        .as_deref()
        .is_some_and(|key| key.starts_with("odi_")));
    assert_eq!(callback.payload["binding_id"], "opbind-delivery-ok");
    assert_eq!(callback.payload["route_id"], "route-default");
    assert_eq!(callback.payload["target_agent_id"], "default");
    assert_eq!(callback.payload["kind"], "operator_output");
    assert_eq!(callback.payload["text"], "deliver this operator note");

    let records = runtime.recent_operator_delivery_records(10).await?;
    let accepted = records
        .iter()
        .find(|record| record.status == OperatorDeliveryStatus::AcceptedByTransport)
        .expect("accepted delivery record should be stored");
    assert_eq!(accepted.binding_id, "opbind-delivery-ok");
    assert_eq!(
        accepted.transport_delivery_id.as_deref(),
        Some("ain_del_test")
    );

    server.abort();
    callback_server.abort();
    Ok(())
}

pub async fn operator_notification_delivery_callback_records_failed_submit() -> Result<()> {
    let (callback_url, records, callback_server) =
        spawn_delivery_callback(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await?;
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    create_operator_transport_binding(&client, &base, "opbind-delivery-fail", &callback_url)
        .await?;

    runtime
        .notify_operator("delivery failure should not fail the notification".into())
        .await?;

    wait_until(|| {
        Ok(!records
            .lock()
            .expect("delivery callback records lock")
            .is_empty())
    })
    .await?;
    let delivery_records = runtime.recent_operator_delivery_records(10).await?;
    let failed = delivery_records
        .iter()
        .find(|record| record.status == OperatorDeliveryStatus::FailedToSubmit)
        .expect("failed delivery record should be stored");
    assert_eq!(failed.binding_id, "opbind-delivery-fail");
    assert!(failed
        .failure_summary
        .as_deref()
        .is_some_and(|summary| summary.contains("HTTP 500")));
    let notifications = runtime.recent_operator_notifications(10).await?;
    assert!(notifications.iter().any(|notification| {
        notification.message == "delivery failure should not fail the notification"
    }));

    server.abort();
    callback_server.abort();
    Ok(())
}
