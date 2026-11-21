use super::*;
use crate::{
    provider::{ModelBlock, ModelToolCallKind},
    runtime::{
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey},
        tests::support::OriginalCommandPublicationProbe,
    },
};

#[derive(Default)]
struct CommandSourceProvider {
    commands: Vec<String>,
    calls: AtomicU64,
    fail_next: AtomicBool,
    commands_started: Notify,
    release_parent: Notify,
    failure_started: Notify,
    release_failure: Notify,
}

#[async_trait]
impl AgentProvider for CommandSourceProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            self.failure_started.notify_one();
            self.release_failure.notified().await;
            anyhow::bail!("host new parent fixture failure");
        }
        let blocks = if call == 0 {
            self.commands
                .iter()
                .enumerate()
                .map(|(index, cmd)| ModelBlock::ToolUse {
                    id: format!("old-source-command-{index}"),
                    name: "ExecCommand".into(),
                    input: json!({
                        "cmd": cmd,
                        "shell": "sh",
                        "login": false,
                        "yield_time_ms": 0,
                    }),
                    kind: ModelToolCallKind::Function,
                    provider_data: None,
                })
                .collect()
        } else {
            if call == 1 {
                self.commands_started.notify_one();
                self.release_parent.notified().await;
            }
            vec![ModelBlock::Text {
                text: "host source fixture done".into(),
            }]
        };
        let mut response = StubProvider::new("unused").complete_turn(request).await?;
        response.blocks = blocks;
        Ok(response)
    }
}

fn prompt(text: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: text.into() },
    )
}

#[tokio::test]
async fn original_command_host_terminals_keep_old_source_after_provider_failure() -> Result<()> {
    command_host_source_after_failure(false).await
}

#[tokio::test]
async fn original_command_host_terminals_keep_old_source_after_reader_pressure() -> Result<()> {
    command_host_source_after_failure(true).await
}

async fn command_host_source_after_failure(reader_pressure: bool) -> Result<()> {
    for recent_messages in [0, 1] {
        let home = tempdir()?;
        write_test_model_config(home.path());
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.user_home_dir = Some(home.path().to_path_buf());
        config.default_agent_id = "default".into();
        config.context_window_messages = recent_messages;
        config.context_window_briefs = 0;
        let fifos: Vec<_> = (0..2)
            .map(|index| home.path().join(format!("command-{index}.fifo")))
            .collect();
        for fifo in &fifos {
            assert!(std::process::Command::new("mkfifo")
                .arg(fifo)
                .status()?
                .success());
        }
        let provider = Arc::new(CommandSourceProvider {
            commands: fifos
                .iter()
                .enumerate()
                .map(|(index, fifo)| format!("read token < {fifo:?}; printf command-{index}"))
                .collect(),
            ..Default::default()
        });
        let host = RuntimeHost::new_with_provider(config, provider.clone())?;
        let runtime = host.default_runtime().await?;
        let source = runtime.enqueue(prompt("host old command source")).await?;
        tokio::time::timeout(
            Duration::from_secs(10),
            provider.commands_started.notified(),
        )
        .await
        .context("parent must execute both real Command tools")?;
        let tasks: Vec<_> = runtime
            .runtime_db()
            .tasks()
            .latest_for_agent("default", 10)?
            .into_iter()
            .filter(|task| task.kind == TaskKind::CommandTask)
            .collect();
        assert_eq!(tasks.len(), 2);
        let mut commands = Vec::new();
        for task in tasks {
            assert_eq!(task.status, TaskStatus::Running);
            let parent_turn_id = task.detail.as_ref().unwrap()["parent_turn_id"]
                .as_str()
                .context("Command parent Turn missing")?;
            let parent_turn = runtime
                .storage()
                .read_turn_by_id(parent_turn_id)?
                .context("Command parent Turn must remain canonical")?;
            assert!(parent_turn.input_message_ids.contains(&source.id));
            let probe = OriginalCommandPublicationProbe::capture(&runtime, &task.id).await?;
            let index = task.detail.as_ref().unwrap()["cmd"]
                .as_str()
                .and_then(|cmd| {
                    fifos
                        .iter()
                        .position(|fifo| cmd.contains(fifo.to_str().unwrap()))
                })
                .context("Command task must identify its original FIFO")?;
            commands.push((task, probe, index));
        }
        provider.release_parent.notify_one();
        wait_for_agent_inputs_settled(&runtime).await;
        for _ in 0..3 {
            runtime
                .enqueue(prompt("advance host bounded history"))
                .await?;
            wait_for_agent_inputs_settled(&runtime).await;
        }
        if reader_pressure {
            runtime.wait_for_bootstrap().await?;
            host.shutdown_runtime_recovery_coordinator().await;
            runtime.inject_operator_dispatch_reader_failure(false);
        } else {
            provider.fail_next.store(true, Ordering::SeqCst);
        }
        let failed = runtime.enqueue(prompt("host failing newer parent")).await?;
        if !reader_pressure {
            tokio::time::timeout(Duration::from_secs(10), provider.failure_started.notified())
                .await?;
            provider.release_failure.notify_one();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if reader_pressure {
                    if host
                        .inner
                        .runtimes
                        .read()
                        .await
                        .agents
                        .get("default")
                        .is_some_and(|entry| *entry.phase.borrow() == AgentRuntimePhase::Terminated)
                    {
                        let state = runtime
                            .runtime_db()
                            .agent_states()
                            .latest("default")?
                            .context("failure State missing")?;
                        assert!(state.last_runtime_failure.is_some());
                        assert!(state.current_run_id.is_none());
                        assert_eq!(
                            runtime
                                .runtime_db()
                                .queue_entries()
                                .latest(&failed.id)?
                                .context("failed dispatch Queue missing")?
                                .status,
                            QueueEntryStatus::Dequeued
                        );
                        let turn = runtime
                            .storage()
                            .read_recent_turns(1)?
                            .pop()
                            .context("failed actual dispatch Turn missing")?;
                        assert!(turn.input_message_ids.contains(&failed.id));
                        assert!(turn.terminal.is_none());
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                if let Some(turn) =
                    runtime
                        .storage()
                        .read_recent_turns(5)?
                        .into_iter()
                        .find(|turn| {
                            turn.input_message_ids.contains(&failed.id) && turn.terminal.is_some()
                        })
                {
                    assert_eq!(turn.terminal.unwrap().kind, TurnTerminalKind::Aborted);
                    if host
                        .runtime_db()
                        .agent_states()
                        .latest("default")?
                        .is_some_and(|state| {
                            state.last_runtime_failure.is_some() && state.current_run_id.is_none()
                        })
                    {
                        assert!(!provider.fail_next.load(Ordering::SeqCst));
                        return Ok::<_, anyhow::Error>(());
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("new parent failure must commit before old commands exit")??;
        let pause = if reader_pressure {
            None
        } else {
            Some(runtime.pause_run_loop_for_monitor_test().await)
        };
        let owner = host
            .inner
            .mutation_owners
            .current_owner("default", runtime.instance_incarnation())?;
        for (task, probe, index) in commands {
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio::fs::write(&fifos[index], b"go\n"),
            )
            .await??;
            tokio::time::timeout(Duration::from_secs(10), probe.finish(&runtime))
                .await
                .context("original Command must finish whole Host publication")??;
            // No State/Events read or extra prepare may repair this cut.
            let root = owner.capture()?.context("Host publication missing")?;
            let inputs = root
                .input_evidence
                .as_ref()
                .context("Host publication omitted Input")?
                .capture()?;
            assert!(inputs
                .lookup(&InputEvidenceKey::new(
                    InputEvidenceFamily::Message,
                    &source.id
                ))?
                .is_some());
            let result = runtime
                .storage()
                .read_recent_messages(20)?
                .into_iter()
                .find(|message| message.task_id.as_deref() == Some(task.id.as_str()))
                .context("original Command result missing")?;
            inputs.execution_interrupted_attempts_for_source(&result.id)?;
            assert!(root.work.is_some() && root.tasks.is_some() && root.scheduling.is_some());
            assert_eq!(
                runtime
                    .runtime_db()
                    .tasks()
                    .latest(&task.id)?
                    .unwrap()
                    .status,
                TaskStatus::Completed
            );
        }
        runtime.operator_state_read_projection(10, 10).await?;
        drop(pause);
        host.shutdown().await?;
    }
    Ok(())
}
