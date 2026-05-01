use super::super::*;
use super::support::*;

#[tokio::test]
async fn runtime_fires_overdue_timer_after_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    storage
        .append_timer(&TimerRecord {
            id: "timer-recover".into(),
            agent_id: "default".into(),
            created_at: Utc::now(),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer recovered".into()),
            next_fire_at: Some(Utc::now() - chrono::Duration::milliseconds(5)),
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-recover" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test]
async fn runtime_recovers_active_timer_without_next_fire_at() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    storage
        .append_timer(&TimerRecord {
            id: "timer-missing-next-fire".into(),
            agent_id: "default".into(),
            created_at: Utc::now() - chrono::Duration::milliseconds(20),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer fallback".into()),
            next_fire_at: None,
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer fallback done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-missing-next-fire" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test]
async fn schedule_timer_rejects_unrepresentable_duration() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let result = runtime.schedule_timer(u64::MAX, None, None).await;
    assert!(result.is_err());
}
