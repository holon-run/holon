use super::*;

#[test]
fn workspace_state_stages_are_fixed_and_unique() {
    let stages = snapshot()
        .into_iter()
        .filter(|stage| stage.name.starts_with("workspace.state."))
        .map(|stage| stage.name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        stages,
        [
            "workspace.state.total",
            "workspace.state.agent_lock_wait",
            "workspace.state.agent_clone",
            "workspace.state.root_records",
            "workspace.state.workspace_records",
            "workspace.state.git_list",
            "workspace.state.git_status",
            "workspace.state.occupancies",
            "workspace.state.serialize",
            "workspace.state.render",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    assert_eq!(
        snapshot()
            .iter()
            .filter(|stage| stage.name.starts_with("workspace.state."))
            .count(),
        stages.len()
    );
}

#[test]
fn input_prepare_attribution_stages_are_fixed_and_unique() {
    let stages = snapshot()
        .into_iter()
        .filter(|stage| stage.name.starts_with("db.input.prepare."))
        .map(|stage| stage.name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        stages,
        [
            "db.input.prepare.queue",
            "db.input.prepare.episodes",
            "db.input.prepare.scheduling",
            "db.input.prepare.records",
            "db.input.prepare.current",
            "db.input.prepare.source.owner_change",
            "db.input.prepare.source.validated_recovery",
            "db.input.prepare.source.context",
            "db.input.prepare.source.generic",
            "db.input.prepare.closure.roots",
            "db.input.prepare.closure.records",
            "db.input.prepare.closure.edges",
            "db.input.prepare.closure.duplicate_edges",
            "db.input.prepare.family.message",
            "db.input.prepare.family.turn",
            "db.input.prepare.family.tool_execution",
            "db.input.prepare.family.brief",
            "db.input.prepare.family.transcript",
            "db.input.prepare.family.attempt",
            "db.input.prepare.family.outcome",
            "db.input.prepare.family.work_execution",
            "db.input.prepare.op.alias_read",
            "db.input.prepare.op.fingerprint_read",
            "db.input.prepare.op.decode",
            "db.input.prepare.op.hash",
            "db.input.prepare.op.sizing",
            "db.input.prepare.op.qualification_reread",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[test]
fn timer_records_attempt_rows_and_submillisecond_precision() {
    static STAGE: Stage = Stage::new("test");
    let mut timer = STAGE.start();
    timer.rows(7);
    timer.started = Instant::now() - std::time::Duration::from_micros(100);
    drop(timer);
    let snapshot = STAGE.snapshot();
    assert_eq!(snapshot.count, 1);
    assert_eq!(snapshot.rows, 7);
    assert!(snapshot.total_ns >= 100_000);
    assert_eq!(snapshot.total_ns, snapshot.max_ns);
}

#[test]
fn timer_records_failed_attempt_once() {
    static STAGE: Stage = Stage::new("test.failed");
    let attempt = || -> anyhow::Result<()> {
        let _timer = STAGE.start();
        anyhow::bail!("synthetic failure")
    };
    assert!(attempt().is_err());
    assert_eq!(STAGE.snapshot().count, 1);
}

#[tokio::test]
async fn timer_records_cancelled_attempt_once() {
    static STAGE: Stage = Stage::new("test.cancelled");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _timer = STAGE.start();
        started_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(STAGE.snapshot().count, 1);
}
