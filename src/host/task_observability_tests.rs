use super::*;
use crate::{
    runtime::tests::support::host_backed_test_runtime,
    types::{TaskTargetKind, TaskTargetRef},
};

async fn original_target(loaded: bool) -> Result<(tempfile::TempDir, RuntimeHost, TaskTargetRef)> {
    let (home, host, parent) = host_backed_test_runtime().await;
    parent.wait_for_bootstrap().await?;
    let identity = host.create_named_agent("observed-invocation", None).await?;
    let target = TaskTargetRef {
        kind: TaskTargetKind::ChildAgent,
        agent_id: identity.agent_id,
        incarnation: Some(identity.incarnation),
    };
    if loaded {
        let child = host.get_public_agent(&target.agent_id).await?;
        child.wait_for_bootstrap().await?;
        assert!(host.bridge().child_observability(&target).await?.is_some());
        let mut legacy = target.clone();
        legacy.incarnation = None;
        assert!(host.bridge().child_observability(&legacy).await?.is_some());
    } else {
        host.unload_runtime(&target.agent_id).await;
    }
    Ok((home, host, target))
}

fn replace_identity(host: &RuntimeHost, target: &TaskTargetRef) -> Result<TaskTargetRef> {
    let mut identity = host
        .agent_identity_record(&target.agent_id)?
        .context("original identity")?;
    identity.incarnation += 1;
    identity.revision += 1;
    host.runtime_db().agent_identities().upsert(&identity)?;
    // Publish the replacement to the host registry as well as SQL.
    host.append_agent_identity(&identity)?;
    Ok(TaskTargetRef {
        incarnation: Some(identity.incarnation),
        ..target.clone()
    })
}

#[tokio::test]
async fn pinned_task_observability_rejects_replaced_identity_and_loaded_runtime() -> Result<()> {
    let (_home, host, target) = original_target(true).await?;
    let replacement = replace_identity(&host, &target)?;
    assert!(host.bridge().child_observability(&target).await?.is_none());
    let mut legacy = target.clone();
    legacy.incarnation = None;
    assert!(host.bridge().child_observability(&legacy).await?.is_none());
    // Even a task bound to the new identity cannot observe an old loaded runtime.
    assert!(host
        .bridge()
        .child_observability(&replacement)
        .await?
        .is_none());
    assert_eq!(
        host.try_get_loaded_runtime(&target.agent_id)
            .await
            .context("original runtime stays loaded")?
            .instance_incarnation(),
        target.incarnation.unwrap()
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pinned_task_observability_rejects_cold_replacement_without_preparing_new_scope(
) -> Result<()> {
    let (_home, host, target) = original_target(false).await?;
    replace_identity(&host, &target)?;
    // No Scheduling root was prepared for the replacement incarnation.
    assert!(host.bridge().child_observability(&target).await?.is_none());
    let mut legacy = target.clone();
    legacy.incarnation = None;
    assert!(host.bridge().child_observability(&legacy).await?.is_none());
    assert!(host
        .try_get_loaded_runtime(&target.agent_id)
        .await
        .is_none());
    host.shutdown().await?;
    Ok(())
}
