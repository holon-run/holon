mod support;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use axum::{
    extract::{Extension, Request},
    http::StatusCode,
    middleware::{self, Next},
    routing::post,
    Json, Router,
};
use chrono::{TimeZone, Utc};
use holon::{
    config::ControlAuthMode,
    host::RuntimeHost,
    http::{self, AppState},
    provider::StubProvider,
    types::{
        brief_created_event_for, AuditEvent, AuthorityClass, BriefKind, BriefRecord,
        ContinuationTriggerKind, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, QueueEntryRecord, QueueEntryStatus, ToolExecutionRecord, ToolExecutionStatus,
        TurnNoBriefReason, TurnRecord, TurnTerminalKind, TurnTerminalSummary, TurnTriggerSummary,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{net::TcpListener, process::Command, sync::oneshot};

const AGENT_ID: &str = "web";

#[derive(Clone)]
struct ControlState {
    host: RuntimeHost,
}

#[derive(Debug, Deserialize)]
struct ControlRequest {
    action: String,
}

#[tokio::test]
#[ignore = "run through `make conversation-sdk-e2e` after building the TypeScript SDK"]
async fn typescript_sdk_runs_against_real_conversation_server() -> Result<()> {
    let retained_config = support::TestConfigBuilder::new()
        .with_control_auth_mode(ControlAuthMode::Auto)
        .build();
    let host = RuntimeHost::new_with_provider_and_event_bus_capacity_for_test(
        retained_config.config().clone(),
        Arc::new(StubProvider::new("unused")),
        1,
    )?;
    host.create_named_agent(AGENT_ID, None).await?;
    seed_conversation(&host)?;

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let request_log = Arc::clone(&requests);
    let control_state = Arc::new(ControlState { host: host.clone() });
    let app: Router = http::router(AppState::for_tcp(host.clone()))
        .route("/__test/control", post(control))
        .layer(Extension(control_state))
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let request_log = Arc::clone(&request_log);
            async move {
                request_log
                    .lock()
                    .expect("request log lock")
                    .push(request.uri().path().to_string());
                next.run(request).await
            }
        }));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .context("conversation SDK E2E server failed")
    });

    let package_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/conversation-sdk");
    let runner = package_dir.join("e2e/real-server.mjs");
    let built_sdk = package_dir.join("dist/index.js");
    anyhow::ensure!(
        built_sdk.exists(),
        "build the TypeScript SDK before running this test: npm --prefix packages/conversation-sdk run build"
    );
    let output = Command::new("node")
        .arg(&runner)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env(
            "HOLON_CONVERSATION_E2E_BASE_URL",
            format!("http://{addr}/api"),
        )
        .env(
            "HOLON_CONVERSATION_E2E_CONTROL_URL",
            format!("http://{addr}/__test/control"),
        )
        .output()
        .await
        .context("running the TypeScript conversation SDK E2E runner")?;

    let _ = shutdown_tx.send(());
    server
        .await
        .context("joining conversation SDK E2E server")??;
    host.shutdown().await?;

    if !output.status.success() {
        anyhow::bail!(
            "TypeScript conversation SDK E2E failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let requests = requests.lock().expect("request log lock").clone();
    for forbidden in [
        "/events",
        "/transcript",
        "/messages",
        "/tool-executions",
        "/tasks",
        "/artifacts",
    ] {
        assert!(
            requests.iter().all(|path| !path.contains(forbidden)),
            "SDK requested forbidden historical hydration path containing {forbidden}: {requests:?}"
        );
    }
    for expected in [
        "/api/handshake",
        "/api/agents/web/conversation",
        "/api/agents/web/conversation/stream",
        "/api/agents/web/turns/turn-active/activities",
        "/api/agents/web/briefs/brief-late",
    ] {
        assert!(
            requests.iter().any(|path| path == expected),
            "SDK did not request expected protocol path {expected}: {requests:?}"
        );
    }
    Ok(())
}

async fn control(
    Extension(state): Extension<Arc<ControlState>>,
    Json(request): Json<ControlRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    apply_control(&state.host, &request.action)
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

fn apply_control(host: &RuntimeHost, action: &str) -> Result<Value> {
    match action {
        "late_brief" => {
            let storage = host.agent_storage(AGENT_ID)?;
            let mut brief = BriefRecord::new(
                AGENT_ID,
                BriefKind::Result,
                "late deferred brief body",
                None,
                None,
            );
            brief.id = "brief-late".into();
            brief.turn_id = Some("turn-result".into());
            brief.turn_index = Some(3);
            brief.created_at = timestamp(30);
            let event = brief_created_event_for(&brief)?;
            storage.append_brief_with_created_event(&brief, &event)?;
            Ok(json!({"ok": true}))
        }
        "finish_active" => {
            let storage = host.agent_storage(AGENT_ID)?;
            let mut turn = host
                .runtime_db()
                .turn_records()
                .by_id(Some(AGENT_ID), "turn-active")?
                .context("turn-active missing")?;
            turn.terminal = Some(TurnTerminalSummary {
                kind: TurnTerminalKind::Completed,
                reason: None,
                no_brief_reason: Some(TurnNoBriefReason::ToolOnlyWait),
                completed_at: timestamp(40),
                duration_ms: 4_000,
            });
            storage.append_turn(&turn)?;
            append_event(
                &storage,
                "event-finish-active",
                "conversation_sdk_finish",
                json!({"turn_id": "turn-active"}),
                40,
            )?;
            Ok(json!({"ok": true}))
        }
        "retention" => {
            let storage = host.agent_storage(AGENT_ID)?;
            let events = (0..3)
                .map(|index| {
                    audit_event(
                        &format!("event-retention-{index}"),
                        "conversation_sdk_retention",
                        json!({"turn_id": "turn-result", "index": index}),
                        50 + index,
                    )
                })
                .collect::<Vec<_>>();
            storage.append_events(&events)?;
            let oldest_retained_seq = host
                .runtime_db()
                .audit_events()
                .max_event_seq(Some(AGENT_ID))?;
            host.runtime_db().transaction(|transaction| {
                transaction.execute(
                    "INSERT INTO audit_event_retention_watermarks (
                       scope_key, oldest_retained_seq
                     ) VALUES (?1, ?2)
                     ON CONFLICT(scope_key) DO UPDATE SET
                       oldest_retained_seq = MAX(
                         audit_event_retention_watermarks.oldest_retained_seq,
                         excluded.oldest_retained_seq
                       )",
                    rusqlite::params![
                        format!("agent:{AGENT_ID}"),
                        i64::try_from(oldest_retained_seq)?
                    ],
                )?;
                Ok(())
            })?;
            Ok(json!({"ok": true, "oldest_retained_seq": oldest_retained_seq}))
        }
        "burst" => {
            let storage = host.agent_storage(AGENT_ID)?;
            let events = (0..64)
                .map(|index| {
                    audit_event(
                        &format!("event-burst-{index}"),
                        "conversation_sdk_burst",
                        json!({"turn_id": "turn-result", "index": index}),
                        100 + index,
                    )
                })
                .collect::<Vec<_>>();
            storage.append_events(&events)?;
            Ok(json!({"ok": true, "events": events.len()}))
        }
        other => anyhow::bail!("unknown control action {other}"),
    }
}

fn seed_conversation(host: &RuntimeHost) -> Result<()> {
    let storage = host.agent_storage(AGENT_ID)?;
    let mut input = MessageEnvelope::new(
        AGENT_ID,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "inspect the runtime".into(),
        },
    );
    input.id = "message-active".into();
    input.created_at = timestamp(1);
    storage.append_message(&input)?;

    storage.append_turn(&terminal(
        turn("turn-oldest", 1),
        Some(TurnNoBriefReason::Aborted),
    ))?;
    storage.append_turn(&terminal(
        turn("turn-zero-brief", 2),
        Some(TurnNoBriefReason::ToolOnlyWait),
    ))?;
    storage.append_turn(&terminal(turn("turn-result", 3), None))?;
    let mut active = turn("turn-active", 4);
    active.input_message_ids = vec![input.id.clone()];
    active.tool_execution_ids = vec!["tool-active".into()];
    storage.append_turn(&active)?;
    storage.append_tool_execution(&ToolExecutionRecord {
        id: "tool-active".into(),
        agent_id: AGENT_ID.into(),
        work_item_id: None,
        turn_index: 4,
        turn_id: Some("turn-active".into()),
        tool_name: "ExecCommand".into(),
        created_at: timestamp(5),
        completed_at: Some(timestamp(6)),
        duration_ms: 1_000,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: json!({"cmd": "cat secret", "raw": "must-not-leak"}),
        output: json!({"artifact": "must-not-leak"}),
        summary: "inspected bounded state".into(),
        invocation_surface: None,
    })?;

    for (id, text, offset) in [
        ("brief-one", "first result", 10),
        ("brief-two", "second result", 11),
    ] {
        let mut brief = BriefRecord::new(AGENT_ID, BriefKind::Result, text, None, None);
        brief.id = id.into();
        brief.turn_id = Some("turn-result".into());
        brief.turn_index = Some(3);
        brief.created_at = timestamp(offset);
        let event = brief_created_event_for(&brief)?;
        storage.append_brief_with_created_event(&brief, &event)?;
    }
    storage.append_queue_entry(&QueueEntryRecord {
        message_id: "message-pending".into(),
        agent_id: AGENT_ID.into(),
        priority: Priority::Normal,
        status: QueueEntryStatus::Queued,
        created_at: timestamp(20),
        updated_at: timestamp(20),
    })?;
    for (offset, turn_id) in [
        (21, "turn-oldest"),
        (22, "turn-zero-brief"),
        (23, "turn-result"),
        (24, "turn-active"),
    ] {
        append_event(
            &storage,
            &format!("event-seed-{turn_id}"),
            "conversation_sdk_seed",
            json!({"turn_id": turn_id}),
            offset,
        )?;
    }
    Ok(())
}

fn turn(turn_id: &str, turn_index: u64) -> TurnRecord {
    let mut record = TurnRecord::new(AGENT_ID, turn_id, turn_index);
    record.created_at = timestamp(i64::try_from(turn_index).expect("test turn index"));
    record.trigger = Some(TurnTriggerSummary {
        message_id: Some(format!("trigger-{turn_id}")),
        kind: MessageKind::OperatorPrompt,
        origin: MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        authority_class: AuthorityClass::OperatorInstruction,
        priority: Priority::Normal,
        trigger_kind: Some(ContinuationTriggerKind::OperatorInput),
        task_id: None,
    });
    record
}

fn terminal(mut record: TurnRecord, no_brief_reason: Option<TurnNoBriefReason>) -> TurnRecord {
    record.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason,
        completed_at: record.created_at + chrono::Duration::seconds(1),
        duration_ms: 1_000,
    });
    record
}

fn timestamp(offset: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0)
        .single()
        .expect("valid timestamp")
        + chrono::Duration::seconds(offset)
}

fn audit_event(id: &str, kind: &str, data: Value, offset: i64) -> AuditEvent {
    let mut event = AuditEvent::legacy(kind, data);
    event.id = id.into();
    event.created_at = timestamp(offset);
    event
}

fn append_event(
    storage: &holon::storage::AppStorage,
    id: &str,
    kind: &str,
    data: Value,
    offset: i64,
) -> Result<()> {
    storage.append_event(&audit_event(id, kind, data, offset))
}
