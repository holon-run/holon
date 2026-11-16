use super::*;

async fn loaded_history(spec: SyncServerSpec) -> (SyncServer, TuiApp) {
    let server = sync_server(spec).await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
        },
        Duration::from_secs(6),
        "conversation ready",
    )
    .await;
    app.chat_max_scroll = 12;
    app.chat_scroll.scroll_with_key(KeyCode::Home, 12);
    app.maybe_begin_load_older_conversation();
    pump_until(
        &mut app,
        |app| {
            app.conversation
                .as_ref()
                .is_some_and(|model| model.turns.contains_key("t0"))
        },
        Duration::from_secs(3),
        "older history",
    )
    .await;
    (server, app)
}

fn assert_no_raw_pages(server: &SyncServer) {
    assert!(server
        .recorded(|uri| {
            uri.split('?')
                .next()
                .is_some_and(|path| path.ends_with("/events"))
        })
        .is_empty());
}

#[tokio::test]
async fn failed_diagnostics_leave_conversation_and_state_refresh_operational() {
    let (server, mut app) = loaded_history(SyncServerSpec::default()).await;
    let generation = app.conversation_generation;
    let cursor = app.conversation.as_ref().unwrap().cursor.clone();
    let scroll = app.chat_scroll.effective_scroll(12);
    app.overlay = OverlayState::Events {
        selected_event_id: None,
        detail_scroll: 0,
    };
    pump_until(
        &mut app,
        |app| server.count("/events/stream") == 1 && app.reconnect_deadline.is_some(),
        Duration::from_secs(3),
        "diagnostic failure",
    )
    .await;
    server.pending_count.store(3, Ordering::SeqCst);
    app.begin_bootstrap_selected_agent();
    pump_until(
        &mut app,
        |app| app.projection.as_ref().unwrap().session.pending_count == 3,
        Duration::from_secs(3),
        "state refresh despite unavailable diagnostics",
    )
    .await;
    assert_eq!(app.conversation_generation, generation);
    assert_eq!(app.conversation.as_ref().unwrap().cursor, cursor);
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t0"));
    assert_eq!(app.chat_scroll.effective_scroll(12), scroll);
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Streaming
    ));
    assert_no_raw_pages(&server);
}

#[tokio::test]
async fn live_diagnostics_do_not_reduce_state_and_cursor_expiry_preserves_history() {
    let (server, mut app) = loaded_history(SyncServerSpec {
        live_diagnostics: true,
        ..Default::default()
    })
    .await;
    let generation = app.conversation_generation;
    app.overlay = OverlayState::Events {
        selected_event_id: None,
        detail_scroll: 0,
    };
    pump_until(
        &mut app,
        |app| app.stream_task.is_some(),
        Duration::from_secs(3),
        "on-demand diagnostic connection",
    )
    .await;
    let old_stream_generation = app.stream_connect_request_id;
    let event = pipeline_event(
        "diagnostic-21",
        21,
        "default",
        "agent_state_changed",
        json!({"agent_id": "default", "from": "idle", "to": "running"}),
    );
    for sender in server.diagnostic_events.lock().unwrap().iter() {
        sender
            .send(serde_json::to_value(&event.data).unwrap())
            .unwrap();
    }
    pump_until(
        &mut app,
        |app| app.projection.as_ref().unwrap().cursor == Some(21),
        Duration::from_secs(3),
        "diagnostic payload",
    )
    .await;
    assert_eq!(
        app.projection.as_ref().unwrap().agent.agent.status,
        crate::types::AgentStatus::AwakeIdle
    );
    assert!(app.projection.as_ref().unwrap().stale_slices.is_empty());

    app.overlay = OverlayState::None;
    app.tick().await.unwrap();
    assert!(app.stream_task.is_none());
    app.runtime_tx
        .send(TuiRuntimeMessage::Disconnected {
            generation: old_stream_generation,
            agent_id: "default".into(),
            error: "late close".into(),
        })
        .unwrap();
    app.process_runtime_messages();
    assert!(app.reconnect_deadline.is_none());

    app.overlay = OverlayState::Events {
        selected_event_id: None,
        detail_scroll: 0,
    };
    pump_until(
        &mut app,
        |app| app.reconnect_deadline.is_some(),
        Duration::from_secs(3),
        "expired diagnostic cursor",
    )
    .await;
    assert_eq!(app.projection.as_ref().unwrap().cursor, None);
    pump_until(
        &mut app,
        |app| app.stream_task.is_some(),
        Duration::from_secs(3),
        "fresh live diagnostic stream",
    )
    .await;
    assert_eq!(server.count("/events/stream"), 3);
    assert_eq!(app.conversation_generation, generation);
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t0"));
    assert_eq!(
        server
            .recorded(|uri| uri == "/api/agents/default/conversation?limit=30")
            .len(),
        1
    );
    assert_no_raw_pages(&server);
}

#[tokio::test]
async fn failed_state_refresh_keeps_history_and_explicit_retry_obeys_backoff() {
    let (server, mut app) = loaded_history(SyncServerSpec::default()).await;
    let generation = app.conversation_generation;
    server.fail_state.store(true, Ordering::SeqCst);
    app.begin_bootstrap_selected_agent();
    pump_until(
        &mut app,
        |app| server.count("/api/agents/default/state") == 2 && !app.snapshot_refresh_in_flight,
        Duration::from_secs(3),
        "failed state refresh",
    )
    .await;
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t0"));
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Streaming
    ));
    server.pending_count.store(7, Ordering::SeqCst);
    app.begin_bootstrap_selected_agent();
    pump_for(&mut app, Duration::from_millis(200)).await;
    assert_eq!(server.count("/api/agents/default/state"), 2);
    pump_until(
        &mut app,
        |app| app.projection.as_ref().unwrap().session.pending_count == 7,
        Duration::from_secs(7),
        "bounded state retry",
    )
    .await;
    assert_eq!(app.conversation_generation, generation);
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t0"));
    server.assert_no_events_requests();
}

#[tokio::test]
async fn conversation_epoch_reset_discards_only_invalid_history_and_refreshes_state() {
    let (server, mut app) = loaded_history(SyncServerSpec::default()).await;
    let generation = app.conversation_generation;
    *server.snapshot_epoch.lock().unwrap() = "replacement-epoch".into();
    server.reset_next_stream.store(true, Ordering::SeqCst);
    server.pending_count.store(5, Ordering::SeqCst);
    server.events.lock().unwrap().clear();
    pump_until(
        &mut app,
        |app| {
            app.conversation
                .as_ref()
                .is_some_and(|model| model.state_refresh_head().1 == "replacement-epoch")
                && matches!(app.connection_state, TuiConnectionState::Streaming)
        },
        Duration::from_secs(7),
        "fresh snapshot after conversation reset",
    )
    .await;
    assert_eq!(app.conversation_generation, generation);
    assert!(!app.conversation.as_ref().unwrap().turns.contains_key("t0"));
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t1"));
    assert!(app.chat_scroll.is_following_tail());
    pump_until(
        &mut app,
        |app| app.projection.as_ref().unwrap().session.pending_count == 5,
        Duration::from_secs(6),
        "state convergence after epoch reset",
    )
    .await;
    assert_eq!(
        server
            .recorded(|uri| uri == "/api/agents/default/conversation?limit=30")
            .len(),
        2
    );
    server.assert_no_events_requests();
}

#[tokio::test]
async fn missed_hint_converges_on_selected_agent_idle_poll_without_restarting_conversation() {
    let (server, mut app) = loaded_history(SyncServerSpec::default()).await;
    let generation = app.conversation_generation;
    server.pending_count.store(9, Ordering::SeqCst);
    // Advance only the scheduler clock; the request still goes through tick and HTTP.
    let previous = Instant::now() - Duration::from_secs(31);
    app.state_refresh = Default::default();
    app.state_refresh.started(previous);
    app.state_refresh.settled(previous, true);
    pump_until(
        &mut app,
        |app| app.projection.as_ref().unwrap().session.pending_count == 9,
        Duration::from_secs(3),
        "idle fallback convergence without a stream hint",
    )
    .await;
    assert_eq!(app.conversation_generation, generation);
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t0"));
    assert_eq!(server.count("/api/agents/beta/state"), 0);
    assert_eq!(
        server
            .recorded(|uri| uri == "/api/agents/default/conversation?limit=30")
            .len(),
        1
    );
    server.assert_no_events_requests();
}
