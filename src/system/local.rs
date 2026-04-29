use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    process::{Child, Command},
    sync::{mpsc, Notify},
};

use super::{
    file::FileHost,
    process::{ProcessHost, ProcessOutput, RunningProcess},
    DirEntry, EditOptions, EffectiveExecution, FileContent, FileRead, FileStat, ProcessRequest,
    ProcessResult, ProgramInvocation, RemoveOptions, RunningProcessExitStatus, StdioSpec,
    StopSignal, WorkspaceView, WriteOptions,
};

#[derive(Debug, Default)]
pub struct LocalSystem;

impl LocalSystem {
    pub fn new() -> Self {
        Self
    }

    pub async fn open_output_file(&self, path: &Path) -> Result<tokio::fs::File> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open {}", path.display()))
    }

    fn build_command(
        &self,
        execution: &EffectiveExecution,
        req: &ProcessRequest,
    ) -> Result<Command> {
        let view = &execution.workspace;
        let cwd = self.command_cwd(view, req)?;
        let mut command = match &req.program {
            ProgramInvocation::Argv { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
            ProgramInvocation::Shell {
                command,
                shell,
                login,
            } => {
                let shell = shell
                    .as_deref()
                    .map(ToString::to_string)
                    .unwrap_or_else(default_shell_program);
                let mut child = Command::new(shell);
                child.arg(if *login { "-lc" } else { "-c" }).arg(command);
                child
            }
        };

        command.current_dir(&cwd).kill_on_drop(true);
        for (key, value) in &req.env {
            command.env(key, value);
        }
        command.stdin(stdio(req.stdin));
        command.stdout(if req.capture.stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stderr(if req.capture.stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        Ok(command)
    }

    fn build_pty_command(
        &self,
        execution: &EffectiveExecution,
        req: &ProcessRequest,
    ) -> Result<CommandBuilder> {
        let view = &execution.workspace;
        let cwd = self.command_cwd(view, req)?;
        let mut command = match &req.program {
            ProgramInvocation::Argv { program, args } => {
                let mut argv = Vec::with_capacity(args.len() + 1);
                argv.push(program.clone().into());
                argv.extend(args.iter().cloned().map(Into::into));
                CommandBuilder::from_argv(argv)
            }
            ProgramInvocation::Shell {
                command,
                shell,
                login,
            } => {
                let shell = shell
                    .as_deref()
                    .map(ToString::to_string)
                    .unwrap_or_else(default_shell_program);
                let mut builder = CommandBuilder::new(shell);
                builder.arg(if *login { "-lc" } else { "-c" });
                builder.arg(command);
                builder
            }
        };
        command.cwd(cwd.as_os_str());
        for (key, value) in &req.env {
            command.env(key, value);
        }
        Ok(command)
    }

    fn command_cwd(&self, view: &WorkspaceView, req: &ProcessRequest) -> Result<PathBuf> {
        let cwd = req.cwd.clone().unwrap_or_else(|| view.cwd().to_path_buf());
        self.resolve_existing_path(view, &cwd)
    }

    fn spawn_pty_process(
        &self,
        execution: &EffectiveExecution,
        req: ProcessRequest,
    ) -> Result<Box<dyn RunningProcess>> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize::default())
            .context("failed to allocate pty")?;
        let command = self.build_pty_command(execution, &req)?;
        let child = pair
            .slave
            .spawn_command(command)
            .context("failed to spawn pty process")?;
        let killer = child.clone_killer();
        let child = Arc::new(Mutex::new(child));
        let exit_state = Arc::new(Mutex::new(None));
        let exit_notify = Arc::new(Notify::new());
        let wait_child = Arc::clone(&child);
        let wait_exit_state = Arc::clone(&exit_state);
        let wait_exit_notify = Arc::clone(&exit_notify);
        tokio::task::spawn_blocking(move || {
            let result = match wait_child.lock() {
                Ok(mut child) => child
                    .wait()
                    .context("failed to wait for pty process")
                    .map(Into::into)
                    .map_err(|err| format!("{err:#}")),
                Err(_) => Err("failed to lock pty child".to_string()),
            };
            if let Ok(mut guard) = wait_exit_state.lock() {
                *guard = Some(result);
            }
            wait_exit_notify.notify_waiters();
        });
        let stdout = if req.capture.stdout || req.capture.stderr {
            Some(pipe_process_output(
                pair.master
                    .try_clone_reader()
                    .context("failed to clone pty reader")?,
            ))
        } else {
            None
        };
        let writer = Some(
            pair.master
                .take_writer()
                .context("failed to acquire pty writer")?,
        );
        Ok(Box::new(LocalPtyRunningProcess {
            child,
            killer: Arc::new(Mutex::new(killer)),
            stdout,
            writer: Arc::new(Mutex::new(writer)),
            exit_state,
            exit_notify,
        }))
    }

    fn resolve_existing_path(&self, view: &WorkspaceView, path: &Path) -> Result<PathBuf> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            let relative = path.to_string_lossy();
            view.resolve_path(relative.as_ref())
        }
    }
}

#[async_trait]
impl ProcessHost for LocalSystem {
    async fn run(
        &self,
        execution: &EffectiveExecution,
        req: ProcessRequest,
    ) -> Result<ProcessResult> {
        let mut command = self.build_command(execution, &req)?;
        let started = Instant::now();
        let output = match req.timeout {
            Some(timeout) => tokio::time::timeout(timeout, command.output())
                .await
                .context("process timed out")??,
            None => command.output().await?,
        };
        Ok(ProcessResult {
            exit_status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
            duration: started.elapsed(),
        })
    }

    async fn spawn(
        &self,
        execution: &EffectiveExecution,
        req: ProcessRequest,
    ) -> Result<Box<dyn RunningProcess>> {
        if req.tty {
            return self.spawn_pty_process(execution, req);
        }
        let mut command = self.build_command(execution, &req)?;
        command.stdout(if req.capture.stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stderr(if req.capture.stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let child = command.spawn().context("failed to spawn process")?;
        Ok(Box::new(LocalRunningProcess { child }))
    }
}

#[async_trait]
impl FileHost for LocalSystem {
    async fn read(&self, execution: &EffectiveExecution, path: &str) -> Result<FileRead> {
        let view = &execution.workspace;
        let resolved = view.resolve_read_path(path)?;
        let content = fs::read_to_string(&resolved)
            .await
            .with_context(|| format!("failed to read {}", resolved.display()))?;
        Ok(FileRead {
            path: resolved,
            content,
        })
    }

    async fn write(
        &self,
        execution: &EffectiveExecution,
        path: &str,
        content: FileContent,
        opts: WriteOptions,
    ) -> Result<()> {
        let view = &execution.workspace;
        let resolved = view.resolve_path(path)?;
        if opts.create_parents {
            if let Some(parent) = resolved.parent() {
                fs::create_dir_all(parent).await?;
            }
        }
        match content {
            FileContent::Text(text) => fs::write(&resolved, text.as_bytes()).await?,
            FileContent::Bytes(bytes) => fs::write(&resolved, bytes).await?,
        }
        Ok(())
    }

    async fn edit(
        &self,
        execution: &EffectiveExecution,
        path: &str,
        old_string: &str,
        new_string: &str,
        opts: EditOptions,
    ) -> Result<()> {
        let view = &execution.workspace;
        let resolved = view.resolve_path(path)?;
        let content = fs::read_to_string(&resolved)
            .await
            .with_context(|| format!("failed to read {}", resolved.display()))?;
        let matches = content.match_indices(old_string).count();
        if matches == 0 {
            return Err(anyhow!("old_string not found in {}", resolved.display()));
        }
        if matches > 1 && !opts.replace_all {
            return Err(anyhow!(
                "old_string is not unique in {}; use replace_all or a more specific old_string",
                resolved.display()
            ));
        }
        let updated = if opts.replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        fs::write(&resolved, updated.as_bytes())
            .await
            .with_context(|| format!("failed to write {}", resolved.display()))?;
        Ok(())
    }

    async fn list(
        &self,
        execution: &EffectiveExecution,
        path: Option<&str>,
    ) -> Result<Vec<DirEntry>> {
        let view = &execution.workspace;
        let resolved = view.resolve_optional_path(path)?;
        let mut entries = fs::read_dir(&resolved)
            .await
            .with_context(|| format!("failed to list {}", resolved.display()))?;
        let mut results = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            results.push(DirEntry {
                path: entry.path(),
                is_dir: metadata.is_dir(),
            });
        }
        Ok(results)
    }

    async fn create_dir_all(&self, execution: &EffectiveExecution, path: &Path) -> Result<()> {
        let view = &execution.workspace;
        let resolved = self.resolve_existing_path(view, path)?;
        fs::create_dir_all(&resolved)
            .await
            .with_context(|| format!("failed to create {}", resolved.display()))
    }

    async fn remove(
        &self,
        execution: &EffectiveExecution,
        path: &str,
        opts: RemoveOptions,
    ) -> Result<()> {
        let view = &execution.workspace;
        let resolved = view.resolve_path(path)?;
        let metadata = fs::metadata(&resolved)
            .await
            .with_context(|| format!("failed to stat {}", resolved.display()))?;
        if metadata.is_dir() {
            if opts.recursive {
                fs::remove_dir_all(&resolved).await?;
            } else {
                fs::remove_dir(&resolved).await?;
            }
        } else {
            fs::remove_file(&resolved).await?;
        }
        Ok(())
    }

    async fn rename(&self, execution: &EffectiveExecution, from: &str, to: &str) -> Result<()> {
        let view = &execution.workspace;
        let source = view.resolve_path(from)?;
        let target = view.resolve_path(to)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::rename(&source, &target).await.with_context(|| {
            format!(
                "failed to rename {} to {}",
                source.display(),
                target.display()
            )
        })
    }

    async fn stat(&self, execution: &EffectiveExecution, path: &str) -> Result<FileStat> {
        let view = &execution.workspace;
        let resolved = view.resolve_path(path)?;
        match fs::metadata(&resolved).await {
            Ok(metadata) => Ok(FileStat {
                path: resolved,
                exists: true,
                is_file: metadata.is_file(),
                is_dir: metadata.is_dir(),
                len: Some(metadata.len()),
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(FileStat {
                path: resolved,
                exists: false,
                is_file: false,
                is_dir: false,
                len: None,
            }),
            Err(err) => Err(err).with_context(|| format!("failed to stat {}", resolved.display())),
        }
    }
}

struct LocalRunningProcess {
    child: Child,
}

struct LocalPtyRunningProcess {
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    stdout: Option<Box<dyn ProcessOutput>>,
    writer: Arc<Mutex<Option<Box<dyn std::io::Write + Send>>>>,
    exit_state: Arc<Mutex<Option<std::result::Result<RunningProcessExitStatus, String>>>>,
    exit_notify: Arc<Notify>,
}

#[async_trait]
impl RunningProcess for LocalRunningProcess {
    fn id(&self) -> String {
        self.child
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn take_stdout(&mut self) -> Option<Box<dyn ProcessOutput>> {
        self.child
            .stdout
            .take()
            .map(|stdout| Box::new(stdout) as Box<dyn ProcessOutput>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn ProcessOutput>> {
        self.child
            .stderr
            .take()
            .map(|stderr| Box::new(stderr) as Box<dyn ProcessOutput>)
    }

    async fn write_stdin(&mut self, data: &[u8]) -> Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("process stdin is not available"))?;
        stdin
            .write_all(data)
            .await
            .context("failed to write to process stdin")?;
        stdin.flush().await.context("failed to flush process stdin")
    }

    async fn wait(&mut self) -> Result<RunningProcessExitStatus> {
        self.child
            .wait()
            .await
            .context("failed to wait for process")
            .map(Into::into)
    }

    async fn try_status(&mut self) -> Result<Option<RunningProcessExitStatus>> {
        self.child
            .try_wait()
            .context("failed to query process status")
            .map(|status| status.map(Into::into))
    }

    async fn stop(&mut self, _signal: StopSignal) -> Result<()> {
        self.child.start_kill().context("failed to stop process")
    }
}

#[async_trait]
impl RunningProcess for LocalPtyRunningProcess {
    fn id(&self) -> String {
        self.child
            .lock()
            .ok()
            .and_then(|child| child.process_id())
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn take_stdout(&mut self) -> Option<Box<dyn ProcessOutput>> {
        self.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<Box<dyn ProcessOutput>> {
        None
    }

    async fn write_stdin(&mut self, data: &[u8]) -> Result<()> {
        let writer = Arc::clone(&self.writer);
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut guard = writer
                .lock()
                .map_err(|_| anyhow!("failed to lock pty writer"))?;
            let writer = guard
                .as_mut()
                .ok_or_else(|| anyhow!("process stdin is not available"))?;
            use std::io::Write as _;
            writer
                .write_all(&data)
                .context("failed to write to pty stdin")?;
            writer.flush().context("failed to flush pty stdin")
        })
        .await
        .context("pty stdin writer task failed")?
    }

    async fn wait(&mut self) -> Result<RunningProcessExitStatus> {
        loop {
            if let Some(result) = cached_exit_status(&self.exit_state)? {
                return result;
            }
            self.exit_notify.notified().await;
        }
    }

    async fn try_status(&mut self) -> Result<Option<RunningProcessExitStatus>> {
        cached_exit_status(&self.exit_state).and_then(Option::transpose)
    }

    async fn stop(&mut self, _signal: StopSignal) -> Result<()> {
        let killer = Arc::clone(&self.killer);
        tokio::task::spawn_blocking(move || {
            let mut killer = killer
                .lock()
                .map_err(|_| anyhow!("failed to lock pty killer"))?;
            killer.kill().context("failed to stop pty process")
        })
        .await
        .context("pty stop task failed")?
    }
}

impl From<portable_pty::ExitStatus> for RunningProcessExitStatus {
    fn from(status: portable_pty::ExitStatus) -> Self {
        if let Some(signal) = status.signal() {
            Self::new(None, Some(signal.to_string()))
        } else {
            Self::new(Some(status.exit_code() as i32), None)
        }
    }
}

fn cached_exit_status(
    exit_state: &Arc<Mutex<Option<std::result::Result<RunningProcessExitStatus, String>>>>,
) -> Result<Option<Result<RunningProcessExitStatus>>> {
    let guard = exit_state
        .lock()
        .map_err(|_| anyhow!("failed to lock pty exit state"))?;
    Ok(guard.as_ref().map(|result| match result {
        Ok(status) => Ok(status.clone()),
        Err(err) => Err(anyhow!(err.clone())),
    }))
}

fn default_shell_program() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if shell.contains("bash") {
            return "bash".to_string();
        }
        if shell.contains("zsh") {
            return "zsh".to_string();
        }
        if shell.contains("sh") {
            return "sh".to_string();
        }
    }
    "sh".to_string()
}

fn stdio(spec: StdioSpec) -> Stdio {
    match spec {
        StdioSpec::Null => Stdio::null(),
        StdioSpec::Inherit => Stdio::inherit(),
        StdioSpec::Piped => Stdio::piped(),
    }
}

fn pipe_process_output(reader: Box<dyn std::io::Read + Send>) -> Box<dyn ProcessOutput> {
    let (mut writer, reader_stream) = tokio::io::duplex(8 * 1024);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
    tokio::task::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buffer = [0u8; 4096];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            if tx.blocking_send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    Box::new(reader_stream)
}

#[cfg(test)]
mod tests {
    use tempfile::{tempdir, TempDir};
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::system::{CaptureSpec, ExecutionProfile, ExecutionScopeKind, ProcessPurpose};

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
        process.stop(StopSignal::Kill).await.unwrap();
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
}
