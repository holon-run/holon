use super::*;
use crate::{config::AppConfig, host::RuntimeHost, provider::StubProvider};

#[tokio::test]
async fn actual_loaded_http_active_array_uses_canonical_records_with_task_state_identity_sql_forbidden(
) -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let agent = runtime.instance_agent_id().to_owned();
    let at: chrono::DateTime<Utc> = "2026-10-01T00:00:00.123Z".parse()?;
    for (id, status) in [
        ("http-active-b", crate::types::TaskStatus::Running),
        ("http-active-a", crate::types::TaskStatus::Queued),
        ("http-terminal", crate::types::TaskStatus::Completed),
    ] {
        let task = crate::types::TaskRecord {
            id: id.into(),
            agent_id: agent.clone(),
            kind: crate::types::TaskKind::CommandTask,
            status,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: Some(serde_json::json!({"canonical_detail":"kept"})),
            recovery: None,
        };
        crate::runtime::tests::support::persist_task_transition(
            &runtime,
            &task,
            "http_active_fixture",
        )
        .await?;
    }
    let expected = serde_json::to_value(runtime.storage().latest_active_task_records(1)?)?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO http_active_task_forbidden;
            ALTER TABLE agent_states RENAME TO http_active_state_forbidden;
            ALTER TABLE agent_identities RENAME TO http_active_identity_forbidden;",
        )?;
        Ok(())
    })?;
    let state = Arc::new(AppState::for_unix(host.clone()));
    let response = tasks(
        Path(agent),
        State(state),
        HeaderMap::new(),
        Query(LimitQuery { limit: Some(1) }),
    )
    .await
    .map_err(|(status, body)| anyhow::anyhow!("HTTP active list failed {status}: {}", body.0))?
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let actual: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(actual, expected);
    assert!(actual.is_array());
    assert_eq!(actual[0]["id"], "http-active-a");
    assert_eq!(actual[0]["status"], "queued");
    assert_eq!(actual[0]["detail"]["canonical_detail"], "kept");
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE http_active_task_forbidden RENAME TO tasks;
            ALTER TABLE http_active_state_forbidden RENAME TO agent_states;
            ALTER TABLE http_active_identity_forbidden RENAME TO agent_identities;",
        )?;
        Ok(())
    })?;
    host.shutdown().await?;
    Ok(())
}
