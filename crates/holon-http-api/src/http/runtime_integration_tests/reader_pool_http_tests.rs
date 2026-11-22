use anyhow::Result;
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_prompt_waits_for_preparation_reader_and_is_admitted_once() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"},"runtime":{"db_readers":{"ordinary_max_connections":3,"preparation_max_connections":1}}}"#,
    )?;
    let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.control_token = Some("secret".into());
    config.control_auth_mode = crate::config::ControlAuthMode::Required;
    let host = crate::host::RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("done")),
    )?;
    let runtime = host.default_runtime_bootstrapped_observed(None).await?;
    let agent = host.config().default_agent_id.clone();
    let db = host.runtime_db().clone();
    assert_eq!(db.reader_pool_stats_for_tests().capacity, 3);
    assert_eq!(db.preparation_pool_stats_for_tests().capacity, 1);
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, held) = mpsc::channel();
    let holder = tokio::task::spawn_blocking(move || {
        db.read_preparation_for_http_test(|_| {
            let _ = started.send(());
            held.recv_timeout(Duration::from_secs(10))?;
            Ok(())
        })
    });
    running.await?;
    let app = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
    let prompt = move || {
        Request::builder()
            .method("POST")
            .uri(format!("/api/control/agents/{agent}/prompt"))
            .header("authorization", "Bearer secret")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"text":"reader wait regression","client_request_id":"reader-wait-input"}"#,
            ))
            .unwrap()
    };
    let request = app.clone().oneshot(prompt());
    let pending = tokio::spawn(request);
    tokio::time::sleep(Duration::from_millis(350)).await;
    let premature = pending.is_finished();
    release.send(())?;
    holder.await??;
    let response = tokio::time::timeout(Duration::from_secs(10), pending)
        .await??
        .unwrap();
    let status = response.status();
    let first: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert!(
        !premature,
        "operator prompt failed before the preparation reader was released: {first}"
    );
    assert_eq!(status, StatusCode::OK, "{first}");
    let replay = app.oneshot(prompt()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: serde_json::Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), usize::MAX).await?)?;
    assert_eq!(first["message_id"], replay["message_id"]);
    assert_eq!(replay["disposition"], "duplicate");
    drop(runtime);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_prompt_settles_once_after_ordinary_reader_wait() -> Result<()> {
    use crate::types::QueueEntryStatus;
    use anyhow::Context as _;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"},"runtime":{"db_readers":{"ordinary_max_connections":1,"preparation_max_connections":1}}}"#,
    )?;
    let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = None;
    config.control_token = Some("secret".into());
    config.control_auth_mode = crate::config::ControlAuthMode::Required;
    let host = crate::host::RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("done")),
    )?;
    let runtime = host.default_runtime_bootstrapped_observed(None).await?;
    runtime.await_initial_resources_for_test().await;
    let agent = host.config().default_agent_id.clone();
    let db = host.runtime_db();
    assert_eq!(db.reader_pool_stats_for_tests().capacity, 1);
    assert_eq!(db.preparation_pool_stats_for_tests().capacity, 1);
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, held) = mpsc::channel();
    let holder_db = db.clone();
    let guard = Duration::from_secs(10);
    let holder = tokio::task::spawn_blocking(move || {
        holder_db.read_for_http_test(|_| {
            let _ = started.send(());
            held.recv_timeout(guard * 2)?;
            Ok(())
        })
    });
    tokio::time::timeout(guard, running)
        .await
        .context("hold ordinary lease")??;
    assert_eq!(db.reader_pool_stats_for_tests().idle_connections, 0);
    let app = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
    let prompt = move || {
        Request::builder()
            .method("POST")
            .uri(format!("/api/control/agents/{agent}/prompt"))
            .header("authorization", "Bearer secret")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"text":"reader wait regression","client_request_id":"reader-wait-input"}"#,
            ))
            .unwrap()
    };
    // The prompt's identity lookup uses the ordinary reader before admission.
    let before = db.reader_pool_stats_for_tests().checkout_attempts;
    let pending = tokio::spawn(app.clone().oneshot(prompt()));
    tokio::time::timeout(guard, async {
        let mut observe = tokio::time::interval(Duration::from_millis(1));
        while db.reader_pool_stats_for_tests().checkout_attempts == before && !pending.is_finished()
        {
            observe.tick().await;
        }
    })
    .await
    .context("observe operator prompt ordinary checkout")?;
    assert!(
        !pending.is_finished(),
        "operator prompt bypassed the held ordinary reader"
    );
    release.send(())?;
    tokio::time::timeout(guard, holder).await???;
    let response = tokio::time::timeout(guard, pending)
        .await
        .context("operator prompt response")??
        .unwrap();
    let status = response.status();
    let first: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["disposition"], "accepted");
    let message_id = first["message_id"]
        .as_str()
        .expect("accepted response has a canonical message ID");

    // All of this must be durable before the client ever resends the request.
    tokio::time::timeout(guard, async {
        let mut observe = tokio::time::interval(Duration::from_millis(5));
        loop {
            if db
                .queue_entries()
                .latest(message_id)?
                .is_some_and(|entry| entry.status == QueueEntryStatus::Processed)
            {
                break Ok::<_, anyhow::Error>(());
            }
            observe.tick().await;
        }
    })
    .await
    .context("settle canonical operator prompt queue entry")??;
    let turns = runtime.storage().read_recent_turns(20)?;
    let linked: Vec<_> = turns
        .iter()
        .filter(|turn| turn.input_message_ids.iter().any(|id| id == message_id))
        .collect();
    assert_eq!(linked.len(), 1, "one canonical turn for the admitted input");
    let turn = linked[0];
    assert_eq!(
        turn.trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_deref()),
        Some(message_id)
    );
    assert_eq!(
        turn.terminal.as_ref().map(|terminal| terminal.kind),
        Some(crate::types::TurnTerminalKind::Completed)
    );
    // A trusted operator prompt uses an original dispatch checkpoint, not an
    // idle input batch. Match the durable checkpoint to the actual turn.
    let checkpoints: u32 = db.read_for_http_test(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM dispatch_memory_checkpoints
             WHERE agent_id=?1 AND turn_id=?2 AND turn_index=?3 AND source_message_id=?4",
            rusqlite::params![turn.agent_id, turn.turn_id, turn.turn_index, message_id],
            |row| row.get(0),
        )?)
    })?;
    assert_eq!(checkpoints, 1, "one durable original dispatch checkpoint");
    let execution = crate::runtime::http_test_support::execution_protocol_state(
        &db,
        &host.config().default_agent_id,
    )?
    .expect("dispatch initializes canonical execution state");
    let attempts: Vec<_> = execution
        .attempts
        .values()
        .filter(|attempt| attempt.source_message_id.as_deref() == Some(message_id))
        .collect();
    assert_eq!(attempts.len(), 1);
    let attempt = attempts[0];
    assert_eq!(attempt.turn_id.as_deref(), Some(turn.turn_id.as_str()));
    assert_eq!(
        execution
            .outcomes
            .values()
            .filter(|outcome| outcome.attempt_id == attempt.attempt_id)
            .count(),
        1
    );
    let outcome_id = attempt
        .terminal_outcome_id
        .as_ref()
        .expect("attempt settled");
    assert_eq!(
        execution.outcomes[outcome_id].attempt_id,
        attempt.attempt_id
    );

    let replay = app.oneshot(prompt()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: serde_json::Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), usize::MAX).await?)?;
    assert_eq!(first["message_id"], replay["message_id"]);
    assert_eq!(replay["disposition"], "duplicate");
    drop(runtime);
    host.shutdown().await?;
    Ok(())
}
