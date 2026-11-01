use super::*;
use crate::storage::PublishedAuditEvent;
use tokio::sync::broadcast;

fn check_notification(runtime: &RuntimeHandle, published: &PublishedAuditEvent) -> Result<()> {
    let scope = resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    assert_eq!(published.agent_id.as_deref(), Some(scope.agent_id.as_str()));
    let qualified = published
        .qualified
        .as_ref()
        .context("raw lifecycle Audit")?;
    qualified.validate(&scope, &runtime.inner.input_evidence_budget)?;
    assert!(std::ptr::eq(qualified.event(), &*published.event));
    Ok(())
}

fn drain(
    runtime: &RuntimeHandle,
    rx: &mut broadcast::Receiver<PublishedAuditEvent>,
) -> Result<Vec<String>> {
    let mut kinds = Vec::new();
    let mut previous = 0;
    loop {
        match rx.try_recv() {
            Ok(published) => {
                check_notification(runtime, &published)?;
                assert!(published.event.event_seq > previous);
                previous = published.event.event_seq;
                kinds.push(published.event.kind.clone());
            }
            Err(broadcast::error::TryRecvError::Empty) => return Ok(kinds),
            Err(error) => return Err(error.into()),
        }
    }
}

#[tokio::test]
async fn independent_audit_lifecycle_workspace_roundtrip_notifies_qualified_originals() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let bus = crate::storage::EventBus::new(32);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    let dir = tempfile::tempdir()?;
    let workspace = WorkspaceEntry::new("lifecycle-audit", dir.path().to_path_buf(), None);
    runtime.attach_workspace(&workspace).await?;
    let after_attach = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(after_attach
        .state
        .attached_workspaces
        .contains(&workspace.workspace_id));
    assert_eq!(
        runtime.storage().read_agent()?.as_ref(),
        Some(after_attach.state.as_ref())
    );
    runtime.detach_workspace(&workspace.workspace_id).await?;
    let after_detach = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(!after_detach
        .state
        .attached_workspaces
        .contains(&workspace.workspace_id));
    let kinds = drain(runtime, &mut rx)?;
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| kind.starts_with("workspace_"))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["workspace_attached", "workspace_detached"],
    );
    Ok(())
}

#[tokio::test]
async fn independent_audit_lifecycle_workspace_failure_preserves_prior_state_ack() -> Result<()> {
    for detach in [false, true] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let dir = tempfile::tempdir()?;
        let workspace = WorkspaceEntry::new("lifecycle-audit", dir.path().to_path_buf(), None);
        if detach {
            runtime.attach_workspace(&workspace).await?;
        }
        let bus = crate::storage::EventBus::new(32);
        runtime.storage().enable_event_bus(bus.clone())?;
        let mut rx = bus.subscribe();
        let kind = if detach {
            "workspace_detached"
        } else {
            "workspace_attached"
        };
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(&format!(
                "CREATE TRIGGER reject_workspace_audit BEFORE INSERT ON audit_events
                 WHEN NEW.kind='{kind}' BEGIN SELECT RAISE(ABORT,'workspace Audit rejected'); END;"
            ))?;
            Ok(())
        })?;
        let result = if detach {
            runtime.detach_workspace(&workspace.workspace_id).await
        } else {
            runtime.attach_workspace(&workspace).await
        };
        let error = result.unwrap_err();
        assert!(
            format!("{error:#}").contains("workspace Audit rejected"),
            "{error:#}"
        );
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(
            root.state
                .attached_workspaces
                .contains(&workspace.workspace_id),
            !detach
        );
        assert_eq!(
            runtime.storage().read_agent()?.as_ref(),
            Some(root.state.as_ref())
        );
        assert!(!drain(runtime, &mut rx)?
            .iter()
            .any(|observed| observed == kind));
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn independent_audit_lifecycle_closure_sleep_tick_shutdown_remains_qualified() -> Result<()> {
    for duration in [None, Some(20)] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let bus = crate::storage::EventBus::new(64);
        runtime.storage().enable_event_bus(bus.clone())?;
        let mut rx = bus.subscribe();
        runtime.record_closure_decision_event(None).await?;
        runtime
            .transition_to_sleep_with_runnable_override(duration, false)
            .await?;
        let kinds = drain(runtime, &mut rx)?;
        assert!(kinds.iter().any(|kind| kind == "closure_decided"));
        assert!(kinds
            .iter()
            .any(|kind| kind == "scheduler_posture_decision"));
        assert_eq!(
            kinds.iter().any(|kind| kind == "orphan_sleep_observed"),
            duration.is_none()
        );
        if duration.is_some() {
            harness.advance(std::time::Duration::from_millis(40)).await;
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let published = rx.recv().await?;
                    check_notification(runtime, &published)?;
                    if published.event.kind == "system_tick_emitted" {
                        return Ok::<_, anyhow::Error>(());
                    }
                }
            })
            .await??;
        }
        runtime.request_service_shutdown().await?;
        assert!(!runtime.accepts_execution_admission());
        assert!(drain(runtime, &mut rx)?
            .iter()
            .any(|kind| kind == "runtime_service_shutdown_requested"));
    }
    Ok(())
}

#[tokio::test]
async fn independent_audit_lifecycle_sleep_preserves_audit_before_state_failure_order() -> Result<()>
{
    use crate::runtime::scheduler_executor::{SchedulerDecisionExecutor, SleepTransitionBoundary};

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let before = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .state
        .clone();
    assert_ne!(before.status, AgentStatus::Asleep);
    let bus = crate::storage::EventBus::new(32);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_sleep_audit BEFORE INSERT ON audit_events
             WHEN NEW.kind='scheduler_posture_decision'
             BEGIN SELECT RAISE(ABORT,'sleep Audit rejected'); END;",
        )?;
        Ok(())
    })?;
    let error = SchedulerDecisionExecutor::new(runtime)
        .transition_to_sleep(None, SleepTransitionBoundary::LifecycleSleep)
        .await
        .err()
        .context("Audit refusal must stop sleep")?;
    assert!(
        format!("{error:#}").contains("sleep Audit rejected"),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().state,
        before
    );
    assert!(drain(runtime, &mut rx)?.is_empty());
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "DROP TRIGGER reject_sleep_audit;
             CREATE TRIGGER reject_sleep_state BEFORE UPDATE ON agent_states
             BEGIN SELECT RAISE(ABORT,'sleep State rejected'); END;",
        )?;
        Ok(())
    })?;
    let error = SchedulerDecisionExecutor::new(runtime)
        .transition_to_sleep(None, SleepTransitionBoundary::LifecycleSleep)
        .await
        .err()
        .context("State refusal must follow original Audit")?;
    assert!(
        format!("{error:#}").contains("sleep State rejected"),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().state,
        before
    );
    assert_eq!(
        runtime.storage().read_agent()?.as_ref(),
        Some(before.as_ref())
    );
    assert_eq!(drain(runtime, &mut rx)?, ["scheduler_posture_decision"]);
    Ok(())
}
