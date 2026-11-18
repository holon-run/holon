use super::*;
use crate::process::RegisteredProcessGroupQuiescence as Q;
fn execution() -> (tempfile::TempDir, EffectiveExecution) {
    let dir = tempfile::tempdir().unwrap();
    let view = WorkspaceView::new(
        None,
        dir.path().into(),
        dir.path().into(),
        dir.path().into(),
        None,
        None,
        super::super::WorkspaceProjectionKind::CanonicalRoot,
        None,
    )
    .unwrap();
    (
        dir,
        EffectiveExecution {
            profile: super::super::ExecutionProfile::default(),
            workspace: view,
            scope: super::super::ExecutionScopeKind::AgentTurn,
            attached_workspaces: vec![],
        },
    )
}
fn request(command: &str) -> ProcessRequest {
    ProcessRequest {
        program: ProgramInvocation::Shell {
            command: command.into(),
            shell: Some("sh".into()),
            login: false,
        },
        cwd: None,
        env: vec![],
        stdin: StdioSpec::Null,
        tty: false,
        capture: super::super::CaptureSpec::NONE,
        timeout: None,
        purpose: super::super::ProcessPurpose::CommandTask,
    }
}
#[tokio::test]
async fn original_local_group_is_nonempty_before_exit_and_empty_after_actual_wait() -> Result<()> {
    let (_dir, e) = execution();
    let system = LocalSystem::new();
    let mut process = system.spawn(&e, request("sleep 0.1")).await?;
    let group = process
        .registered_group_probe()
        .expect("real Local process has frozen pgid");
    assert_eq!(group.observe(), Q::NonEmpty);
    process.wait().await?;
    assert_eq!(group.observe(), Q::Empty);
    drop(process);
    assert_eq!(group.observe(), Q::Empty);
    Ok(())
}
#[tokio::test]
async fn foreground_wait_does_not_prove_background_same_group_empty() -> Result<()> {
    let (_dir, e) = execution();
    let system = LocalSystem::new();
    let mut process = system.spawn(&e, request("sleep 30 & exit 0")).await?;
    let group = process
        .registered_group_probe()
        .expect("real Local process has frozen pgid");
    assert!(process.wait().await?.success());
    assert_eq!(
        group.observe(),
        Q::NonEmpty,
        "background original group is still a producer"
    );
    // Test-owned subprocess cleanup only. No production proof comes from this
    // signal; an unreaped background child may correctly remain nonempty.
    process.stop(StopSignal::Force).await?;
    Ok(())
}
#[test]
fn invalid_or_unknown_frozen_group_grants_no_empty_proof() {
    use crate::process::RegisteredProcessGroupProbe;
    assert_eq!(
        LocalRegisteredProcessGroupProbe { frozen_pgid: 0 }.observe(),
        Q::Unknown
    );
    assert_eq!(
        LocalRegisteredProcessGroupProbe {
            frozen_pgid: u32::MAX
        }
        .observe(),
        Q::Unknown
    );
}
