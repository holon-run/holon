use super::{router, AppState, ProjectionGate};
use crate::{
    diagnostics::io_probe::{run_isolated_test, IoProbe},
    runtime::RuntimeHandle,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{WorkItemPlanStatus, WorkspaceEntry},
};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

async fn uncached_state(
    base: &AppState,
    runtime: &RuntimeHandle,
    probe: &mut IoProbe,
) -> anyhow::Result<Value> {
    let mut state = base.clone();
    // A new gate guarantees no fresh or stale bytes from an earlier request.
    state.projection_gate = Arc::new(ProjectionGate::new(4, Duration::ZERO));
    let app = router(state);
    probe.reset();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/agents/{}/state", runtime.instance_agent_id()))
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let counters = probe.snapshot().counters;
    assert!(
        [
            counters.runtime_db_connection_opens,
            counters.sql_read_statements,
            counters.sql_write_statements,
            counters.sql_control_statements,
            counters.file_open_attempts,
            counters.file_read_attempts,
            counters.directory_read_attempts,
            counters.file_read_bytes,
            counters.file_errors,
        ]
        .into_iter()
        .all(|count| count == 0),
        "complete uncached HTTP request acquired storage: {counters:?}"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn workspace<'a>(state: &'a Value, id: &str) -> Option<&'a Value> {
    state["workspace"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["workspace_id"] == id)
}

#[tokio::test]
async fn workspace_context_contention_releases_projection_capacity_before_writer_finishes(
) -> anyhow::Result<()> {
    use super::{ProjectionFailure, ProjectionGateError, ProjectionKey};
    use axum::body::Bytes;
    use tokio::time::Instant;

    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.prepare_workspace_context()?;
    let before = db.reader_pool_stats_for_tests().checkout_attempts;
    let publication = db.hold_workspace_context_publication_for_test();
    let gate = Arc::new(ProjectionGate::new(1, Duration::ZERO));
    let budget = Duration::from_millis(50);
    let reader = db.clone();
    let failed = gate
        .run_owned(
            ProjectionKey::AgentState("contended-workspace".into()),
            Instant::now() + budget,
            budget,
            move |context| async move {
                context
                    .blocking(move |scope| {
                        reader
                            .with_read_scope(scope)
                            .with_workspace_context(|_| Ok(Bytes::from_static(b"unexpected")))
                            .map_err(super::error_response)
                            .map_err(ProjectionFailure::from)
                    })
                    .await?
            },
        )
        .await;

    // Keep the writer locked beyond the original read deadline. An abandoned
    // blocking reader must not retain this gate's only build permit.
    tokio::time::sleep(budget + Duration::from_millis(10)).await;
    let healthy = gate
        .run_owned(
            ProjectionKey::AgentsList(None),
            Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
            |_| async { Ok(Bytes::from_static(b"independent")) },
        )
        .await;
    // Release before asserting so a regressed blocking worker can still exit.
    drop(publication);

    let Err(ProjectionGateError::Build(failure)) = failed else {
        panic!("expected retryable workspace contention, got {failed:?}");
    };
    assert_eq!(failure.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failure.retryable(), Some(true));
    assert_ne!(failure.error_code(), Some("projection_deadline_exhausted"));
    assert_eq!(healthy.unwrap(), Bytes::from_static(b"independent"));
    assert_eq!(db.reader_pool_stats_for_tests().checkout_attempts, before);
    db.with_workspace_context(|_| Ok(()))
}

#[tokio::test]
async fn loaded_state_http_workspace_lifecycle_is_storage_free() -> anyhow::Result<()> {
    if run_isolated_test(
        module_path!(),
        "loaded_state_http_workspace_lifecycle_is_storage_free",
    ) {
        return Ok(());
    }
    let mut probe = IoProbe::start();
    let (home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    host.create_named_agent("workspace-peer", None).await?;
    let peer = host.get_public_agent("workspace-peer").await?;
    runtime.await_initial_resources_for_test().await;
    peer.await_initial_resources_for_test().await;
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    let peer_loop = peer.pause_run_loop_for_monitor_test().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let peer_refresh = peer.hold_resource_refresh_for_test().await?;
    let base = AppState::for_unix(host.clone());

    // Even the first loaded State request must not warm a durable read path.
    uncached_state(&base, &runtime, &mut probe).await?;
    let path = home.path().join("project");
    std::fs::create_dir(&path)?;
    let mut entry = WorkspaceEntry::new("http-workspace", path.clone(), Some("repository".into()));
    entry.workspace_alias = Some("original".into());
    peer.attach_workspace(&entry).await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    assert!(workspace(&state, &entry.workspace_id).is_none());
    let state = uncached_state(&base, &peer, &mut probe).await?;
    assert_eq!(
        workspace(&state, &entry.workspace_id).unwrap()["workspace_alias"],
        "original"
    );

    runtime.attach_workspace(&entry).await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    let attached = workspace(&state, &entry.workspace_id).unwrap();
    assert_eq!(attached["repo_name"], "repository");
    assert_eq!(attached["is_active"], false);
    assert!(attached["execution_root"].is_null());

    runtime
        .enter_workspace(
            &entry,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            Some(path),
            None,
        )
        .await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    let active = workspace(&state, &entry.workspace_id).unwrap();
    assert_eq!(active["is_active"], true);
    let root = active["execution_root_id"].as_str().unwrap().to_owned();
    let before_rename = state;

    // A peer updates the shared writer, not either Agent's captured State.
    entry.workspace_alias = Some("renamed-by-peer".into());
    entry.repo_name = Some("updated-repository".into());
    entry.updated_at += chrono::TimeDelta::seconds(1);
    peer.runtime_db().workspace_entries().upsert(&entry)?;
    for _ in 0..2 {
        let state = uncached_state(&base, &runtime, &mut probe).await?;
        let active = workspace(&state, &entry.workspace_id).unwrap();
        assert_eq!(active["workspace_alias"], "renamed-by-peer");
        assert_eq!(active["repo_name"], "updated-repository");
        assert_eq!(active["execution_root_id"], root);
    }
    assert_eq!(
        workspace(&before_rename, &entry.workspace_id).unwrap()["workspace_alias"],
        "original"
    );
    assert_eq!(
        workspace(&before_rename, &entry.workspace_id).unwrap()["repo_name"],
        "repository"
    );

    let work = runtime
        .create_work_item(
            "HTTP State commit".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    assert_eq!(state["work_items"][0]["id"], work.id);
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    assert_eq!(state["timers"][0]["id"], timer.id);
    runtime.cancel_timer(&timer.id).await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    assert!(state["timers"].as_array().unwrap().is_empty());

    runtime
        .activate_agent_home(WorkspaceAccessMode::SharedRead, None)
        .await?;
    assert!(peer
        .runtime_db()
        .execution_root_entries()
        .mark_removed(&root)?);
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    let inactive = workspace(&state, &entry.workspace_id).unwrap();
    assert_eq!(inactive["is_active"], false);
    assert!(inactive["execution_root_id"].is_null());
    runtime.detach_workspace(&entry.workspace_id).await?;
    let state = uncached_state(&base, &runtime, &mut probe).await?;
    assert!(workspace(&state, &entry.workspace_id).is_none());
    let state = uncached_state(&base, &peer, &mut probe).await?;
    assert_eq!(
        workspace(&state, &entry.workspace_id).unwrap()["workspace_alias"],
        "renamed-by-peer"
    );

    drop((refresh, peer_refresh, run_loop, peer_loop));
    host.shutdown().await?;
    Ok(())
}
