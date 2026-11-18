use tempfile::{tempdir, TempDir};
use tokio::io::AsyncReadExt;

use super::*;
use crate::{CaptureSpec, ExecutionProfile, ExecutionScopeKind, ProcessPurpose};

fn effective_execution() -> (TempDir, EffectiveExecution) {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("workspace")).unwrap();
    let view = WorkspaceView::new(
        Some("ws-test".into()),
        dir.path().join("workspace"),
        dir.path().join("workspace"),
        dir.path().join("workspace"),
        None,
        None,
        crate::WorkspaceProjectionKind::CanonicalRoot,
        None,
    )
    .unwrap();
    (
        dir,
        EffectiveExecution {
            profile: ExecutionProfile::default(),
            workspace: view,
            scope: ExecutionScopeKind::AgentTurn,
            attached_workspaces: vec![],
        },
    )
}

#[tokio::test]
async fn runs_shell_and_argv_commands() {
    let (_dir, execution) = effective_execution();
    let system = LocalSystem::new();
    let shell = system
        .run(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Shell {
                    command: "printf shell".into(),
                    shell: Some("sh".into()),
                    login: false,
                },
                cwd: None,
                env: vec![],
                stdin: StdioSpec::Null,
                tty: false,
                capture: CaptureSpec::BOTH,
                timeout: None,
                purpose: ProcessPurpose::ToolExec,
            },
        )
        .await
        .unwrap();
    assert!(shell.exit_status.success());
    assert_eq!(String::from_utf8_lossy(&shell.stdout), "shell");

    let argv = system
        .run(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Argv {
                    program: "sh".into(),
                    args: vec!["-lc".into(), "printf argv".into()],
                },
                cwd: None,
                env: vec![],
                stdin: StdioSpec::Null,
                tty: false,
                capture: CaptureSpec::BOTH,
                timeout: None,
                purpose: ProcessPurpose::ToolExec,
            },
        )
        .await
        .unwrap();
    assert!(argv.exit_status.success());
    assert_eq!(String::from_utf8_lossy(&argv.stdout), "argv");
}

#[tokio::test]
async fn supports_file_round_trips() {
    let (_dir, execution) = effective_execution();
    let system = LocalSystem::new();
    system
        .write(
            &execution,
            "notes/demo.txt",
            FileContent::Text("before".into()),
            WriteOptions {
                create_parents: true,
            },
        )
        .await
        .unwrap();
    system
        .edit(
            &execution,
            "notes/demo.txt",
            "before",
            "after",
            EditOptions { replace_all: false },
        )
        .await
        .unwrap();
    let read = system.read(&execution, "notes/demo.txt").await.unwrap();
    assert_eq!(read.content, "after");
    let stat = system.stat(&execution, "notes/demo.txt").await.unwrap();
    assert!(stat.exists);
    system
        .rename(&execution, "notes/demo.txt", "notes/renamed.txt")
        .await
        .unwrap();
    system
        .remove(
            &execution,
            "notes/renamed.txt",
            RemoveOptions { recursive: false },
        )
        .await
        .unwrap();
    let stat = system.stat(&execution, "notes/renamed.txt").await.unwrap();
    assert!(!stat.exists);
}

#[tokio::test]
async fn spawns_and_stops_background_processes() {
    let (_dir, execution) = effective_execution();
    let system = LocalSystem::new();
    let mut process = system
        .spawn(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Shell {
                    command: "sleep 5".into(),
                    shell: Some("sh".into()),
                    login: false,
                },
                cwd: None,
                env: vec![],
                stdin: StdioSpec::Null,
                tty: false,
                capture: CaptureSpec::NONE,
                timeout: None,
                purpose: ProcessPurpose::CommandTask,
            },
        )
        .await
        .unwrap();
    assert!(process.try_status().await.unwrap().is_none());
    process.stop(StopSignal::Graceful).await.unwrap();
    let status = process.wait().await.unwrap();
    assert!(!status.success());
}

#[tokio::test]
async fn force_stops_background_process_without_grace_period() {
    let (_dir, execution) = effective_execution();
    let system = LocalSystem::new();
    let mut process = system
        .spawn(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Shell {
                    command: "trap '' TERM; sleep 5".into(),
                    shell: Some("sh".into()),
                    login: false,
                },
                cwd: None,
                env: vec![],
                stdin: StdioSpec::Null,
                tty: false,
                capture: CaptureSpec::NONE,
                timeout: None,
                purpose: ProcessPurpose::CommandTask,
            },
        )
        .await
        .unwrap();
    assert!(process.try_status().await.unwrap().is_none());
    process.stop(StopSignal::Force).await.unwrap();
    let status = process.wait().await.unwrap();
    assert!(!status.success());
}

#[tokio::test]
async fn spawns_processes_with_writable_piped_stdin() {
    let (_dir, execution) = effective_execution();
    let system = LocalSystem::new();
    let mut process = system
        .spawn(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Shell {
                    command: "IFS= read -r line; printf \"heard:%s\" \"$line\"".into(),
                    shell: Some("sh".into()),
                    login: false,
                },
                cwd: None,
                env: vec![],
                stdin: StdioSpec::Piped,
                tty: false,
                capture: CaptureSpec::BOTH,
                timeout: None,
                purpose: ProcessPurpose::CommandTask,
            },
        )
        .await
        .unwrap();
    process.write_stdin(b"hello\n").await.unwrap();
    let mut stdout = process.take_stdout().unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).await.unwrap();
    let status = process.wait().await.unwrap();
    assert!(status.success());
    assert_eq!(output, "heard:hello");
}

#[tokio::test]
async fn spawns_tty_processes_with_writable_terminal_input() {
    let (_dir, execution) = effective_execution();
    let system = LocalSystem::new();
    let mut process = system
        .spawn(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Shell {
                    command:
                        "stty -echo; printf ready; IFS= read -r line; printf \"heard:%s\" \"$line\""
                            .into(),
                    shell: Some("sh".into()),
                    login: false,
                },
                cwd: None,
                env: vec![],
                stdin: StdioSpec::Null,
                tty: true,
                capture: CaptureSpec::BOTH,
                timeout: None,
                purpose: ProcessPurpose::CommandTask,
            },
        )
        .await
        .unwrap();

    process.write_stdin(b"hello\n").await.unwrap();
    let mut stdout = process.take_stdout().unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).await.unwrap();
    let status = process.wait().await.unwrap();
    assert!(status.success());
    assert!(output.contains("readyheard:hello"));
    assert!(process.take_stderr().is_none());
}

#[tokio::test]
async fn read_allows_absolute_paths_outside_execution_root() {
    let (dir, execution) = effective_execution();
    let system = LocalSystem::new();
    let external = dir.path().join("user-skills/demo/SKILL.md");
    std::fs::create_dir_all(external.parent().unwrap()).unwrap();
    std::fs::write(&external, "skill body").unwrap();

    let read = system
        .read(&execution, external.to_string_lossy().as_ref())
        .await
        .unwrap();
    assert_eq!(read.path, external);
    assert_eq!(read.content, "skill body");
}
