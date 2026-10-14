mod support;

use anyhow::Result;
use holon::{
    client::LocalClient,
    tail::{observe, TailOptions},
    types::{
        brief_created_event_for, AuditEvent, AuthorityClass, BriefKind, BriefRecord,
        ContinuationTriggerKind, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, TranscriptEntry, TranscriptEntryKind, TurnRecord, TurnTriggerSummary,
    },
};
use serde_json::{json, Value};
use tokio::time::{Duration, Instant};

// Real server bootstraps perform synchronous host/DB work. Serialize these
// fixtures so deadline assertions measure observation, not competing setup.
static REAL_SERVER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_private_agent_supports_conversation_brief_and_status_reads() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    host.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET visibility = 'private' WHERE agent_id = 'default'",
            [],
        )?;
        Ok(())
    })?;
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    assert_eq!(
        client.get_agent_compact("default").await?.agent.id,
        "default"
    );
    assert_eq!(client.agent_status("default").await?.agent.id, "default");
    assert_eq!(client.agent_briefs("default", 20).await?.len(), 1);
    assert_eq!(
        client.agent_brief("default", "legacy-brief").await?.text,
        "canonical brief body"
    );
    let batch = client
        .agent_briefs_batch_get("default", vec!["legacy-brief".into()])
        .await?;
    assert_eq!(batch.briefs.len(), 1);
    let mut output = Vec::new();
    observe(&client, "default", &options(), &mut output).await?;
    let lines = records(&output);
    assert!(lines
        .iter()
        .any(|line| line["text"] == "persisted progress before final"));
    let cursor = lines.last().unwrap()["cursor"].as_str().unwrap().to_owned();
    append_progress(&host, "late-private", "private child progress")?;
    let mut follow = options();
    follow.follow = true;
    follow.cursor = Some(cursor);
    follow.timeout_ms = 500;
    output.clear();
    observe(&client, "default", &follow, &mut output).await?;
    assert!(records(&output)
        .iter()
        .any(|line| line["text"] == "private child progress"));
    server.abort();
    host.shutdown().await?;
    Ok(())
}

fn options() -> TailOptions {
    TailOptions {
        limit: 20,
        include_progress: true,
        follow: false,
        cursor: None,
        timeout_ms: 2000,
        max_events: 100,
        max_bytes: 65536,
    }
}

fn records(output: &[u8]) -> Vec<Value> {
    std::str::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn append_progress(host: &holon::host::RuntimeHost, id: &str, text: &str) -> Result<()> {
    let storage = host.agent_storage("default")?;
    let mut entry = TranscriptEntry::new(
        "default",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        Some("input".into()),
        json!({
            "turn_id":"active",
            "blocks":[
                {"type":"thinking","thinking":"private reasoning"},
                {"type":"tool_use","name":"PrivateTool","input":{"private":"tool args"}},
                {"type":"text","text":text},
            ],
        }),
    );
    entry.id = id.into();
    storage.append_transcript_entry(&entry)?;
    storage.append_event(&AuditEvent::legacy(
        "tail_test_progress",
        json!({"turn_id":"active","transcript_id":id}),
    ))?;
    Ok(())
}

fn seed(host: &holon::host::RuntimeHost) -> Result<()> {
    let storage = host.agent_storage("default")?;
    let mut input = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "private operator prompt".into(),
        },
    );
    input.id = "input".into();
    storage.append_message(&input)?;
    let mut turn = TurnRecord::new("default", "active", 1);
    turn.input_message_ids = vec![input.id.clone()];
    turn.trigger = Some(TurnTriggerSummary {
        message_id: Some(input.id),
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
    storage.append_turn(&turn)?;
    append_progress(host, "round-one", "persisted progress before final")?;
    let mut brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "canonical brief body",
        None,
        None,
    );
    brief.id = "legacy-brief".into();
    storage.append_brief_with_created_event(&brief, &brief_created_event_for(&brief)?)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tail_reads_persisted_progress_before_final_without_waking_or_claiming() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    let before = client.get_agent_compact("default").await?;
    let briefs_before = client.agent_briefs("default", 20).await?;
    let mut output = Vec::new();
    observe(&client, "default", &options(), &mut output).await?;
    let lines = records(&output);
    assert!(lines
        .iter()
        .any(|record| record["text"] == "persisted progress before final"));
    assert!(lines
        .iter()
        .any(|record| record["text"] == "canonical brief body"));
    assert!(lines
        .iter()
        .any(|record| record["execution"]["kind"] == "active"));
    assert_eq!(lines.last().unwrap()["stop_reason"], "snapshot");
    let text = String::from_utf8(output)?;
    for forbidden in [
        "private reasoning",
        "private operator prompt",
        "tool args",
        "PrivateTool",
    ] {
        assert!(!text.contains(forbidden), "output leaked {forbidden}");
    }
    let after = client.get_agent_compact("default").await?;
    assert_eq!(before.agent.current_run_id, after.agent.current_run_id);
    assert_eq!(before.agent.pending, after.agent.pending);
    assert_eq!(before.agent.turn_index, after.agent.turn_index);
    assert_eq!(briefs_before, client.agent_briefs("default", 20).await?);
    assert!(host
        .runtime_db()
        .turn_records()
        .by_id(Some("default"), "active")?
        .unwrap()
        .terminal
        .is_none());
    server.abort();
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tail_resumes_conversation_cursor_and_observes_late_progress() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    let mut output = Vec::new();
    observe(&client, "default", &options(), &mut output).await?;
    let lines = records(&output);
    let cursor = lines.last().unwrap()["cursor"]
        .as_str()
        .unwrap()
        .to_string();
    append_progress(&host, "round-two", "late progress")?;
    let mut resume = options();
    resume.follow = true;
    resume.cursor = Some(cursor.clone());
    resume.timeout_ms = 500;
    output.clear();
    observe(&client, "default", &resume, &mut output).await?;
    let lines = records(&output);
    assert!(lines.iter().any(|record| record["text"] == "late progress"));
    let end = lines.last().unwrap();
    assert_eq!(end["stop_reason"], "timeout");
    assert_ne!(end["cursor"], cursor);
    assert!(end["reset_reason"].is_null());
    // Canonical activity upserts may revisit old identity/revision pairs;
    // observation consumers can replace them, never infer task membership.
    for line in &lines {
        assert_eq!(line["scope"], "agent");
        assert_eq!(line["agent_id"], "default");
        assert!(line.get("task_id").is_none());
    }
    server.abort();
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tail_budget_retains_checkpoint_and_output_is_serialized_byte_bounded() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    let snapshot: Value = client
        .get_json("/agents/default/conversation?limit=20")
        .await?;
    let cursor = snapshot["snapshot_cursor"].as_str().unwrap().to_string();
    append_progress(&host, "large-round", &"界".repeat(3000))?;
    let mut follow = options();
    follow.follow = true;
    follow.cursor = Some(cursor.clone());
    follow.max_bytes = 4096;
    let mut output = Vec::new();
    observe(&client, "default", &follow, &mut output).await?;
    let lines = records(&output);
    let end = lines.last().unwrap();
    assert_eq!(end["stop_reason"], "budget");
    assert_eq!(end["cursor"], cursor);
    assert_eq!(end["record_count"], 0);
    assert!(output.len() <= follow.max_bytes);
    server.abort();
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tail_deadline_covers_slow_bootstrap_without_prompting() -> Result<()> {
    let app = axum::Router::new().route(
        "/api/handshake",
        axum::routing::get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            axum::Json(json!({"capabilities":["agents.conversation-read.v1"]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = LocalClient::remote(
        support::test_config(),
        format!("http://{address}"),
        "test-token",
    )?;
    let mut follow = options();
    follow.follow = true;
    follow.timeout_ms = 50;
    let mut output = Vec::new();
    let start = Instant::now();
    observe(&client, "default", &follow, &mut output).await?;
    assert!(start.elapsed() < Duration::from_secs(1));
    let lines = records(&output);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["stop_reason"], "timeout");
    assert!(lines[0]["runtime_status"].is_null());
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_capability_fails_without_falling_back_to_trace() -> Result<()> {
    let app = axum::Router::new().route(
        "/api/handshake",
        axum::routing::get(|| async { axum::Json(json!({"capabilities":[]})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = LocalClient::remote(
        support::test_config(),
        format!("http://{address}"),
        "test-token",
    )?;
    let mut output = Vec::new();
    let error = observe(&client, "default", &options(), &mut output)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("agents.conversation-read.v1"));
    assert_eq!(records(&output)[0]["stop_reason"], "error");
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_reset_on_stream_attachment_is_an_explicit_receipt() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    let snapshot: Value = client
        .get_json("/agents/default/conversation?limit=20")
        .await?;
    append_progress(&host, "round-two", "new progress")?;
    append_progress(&host, "round-three", "more progress")?;
    let head = host
        .runtime_db()
        .audit_events()
        .max_event_seq(Some("default"))?;
    host.runtime_db().transaction(|tx| {
        tx.execute(
            "INSERT INTO audit_event_retention_watermarks (scope_key, oldest_retained_seq)
             VALUES ('agent:default', ?1)",
            [i64::try_from(head)?],
        )?;
        Ok(())
    })?;
    let mut follow = options();
    follow.follow = true;
    follow.cursor = Some(snapshot["snapshot_cursor"].as_str().unwrap().into());
    let mut output = Vec::new();
    observe(&client, "default", &follow, &mut output).await?;
    let lines = records(&output);
    let end = lines.last().unwrap();
    assert_eq!(end["stop_reason"], "reset_required");
    assert_eq!(end["reset_reason"], "conversation_reset_required");
    assert!(end["cursor"].is_null());
    server.abort();
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_turn_resume_exposes_a_bounded_window_not_a_false_gap() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    for index in 0..101 {
        append_progress(
            &host,
            &format!("old-{index}"),
            &format!("old progress {index}"),
        )?;
    }
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    let snapshot: Value = client
        .get_json("/agents/default/conversation?limit=20")
        .await?;
    append_progress(&host, "late", "latest progress")?;
    let mut follow = options();
    follow.follow = true;
    follow.cursor = Some(snapshot["snapshot_cursor"].as_str().unwrap().into());
    follow.max_events = 200;
    follow.max_bytes = 131072;
    follow.timeout_ms = 1000;
    let mut output = Vec::new();
    observe(&client, "default", &follow, &mut output).await?;
    let lines = records(&output);
    assert!(lines.iter().any(|line| line["text"] == "latest progress"));
    let end = lines.last().unwrap();
    assert_eq!(end["stop_reason"], "timeout");
    assert_eq!(end["resume_coverage_unknown"], true);
    assert_eq!(end["snapshot_truncated"], true);
    assert!(end["reset_reason"].is_null());
    server.abort();
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_canonical_activity_reports_coverage_and_does_not_advance_follow() -> Result<()> {
    let _guard = REAL_SERVER.lock().await;
    let (host, base, server) = support::spawn_server().await?;
    seed(&host)?;
    let client = LocalClient::remote(host.config().as_ref().clone(), base, "test-token")?;
    let snapshot: Value = client
        .get_json("/agents/default/conversation?limit=20")
        .await?;
    append_progress(&host, "missing", "will be retained as a missing source")?;
    host.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id = 'missing'",
            [],
        )?;
        Ok(())
    })?;
    let mut output = Vec::new();
    observe(&client, "default", &options(), &mut output).await?;
    let lines = records(&output);
    let end = lines.last().unwrap();
    assert_eq!(end["snapshot_truncated"], true);
    assert_eq!(end["activity_coverage"]["reason"], "retention_gap");
    let mut follow = options();
    follow.follow = true;
    follow.cursor = Some(snapshot["snapshot_cursor"].as_str().unwrap().into());
    output.clear();
    observe(&client, "default", &follow, &mut output).await?;
    let lines = records(&output);
    let end = lines.last().unwrap();
    assert_eq!(end["stop_reason"], "reset_required");
    assert_eq!(end["reset_reason"], "activity_coverage_unavailable");
    assert_eq!(end["cursor"], snapshot["snapshot_cursor"]);
    server.abort();
    host.shutdown().await?;
    Ok(())
}
