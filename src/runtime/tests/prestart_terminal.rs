//! An accepted Claim must still settle when setup fails before StartedTurn.
use super::support::{context_config, CountingProvider};
use crate::{
    runtime::RuntimeHandle,
    types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        QueueEntryStatus, TurnTerminalKind,
    },
};
use anyhow::{ensure, Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

struct RuntimeTask(tokio::task::JoinHandle<Result<()>>);
impl Drop for RuntimeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn claimed_message_processing_start_failure_settles_without_a_started_turn() -> Result<()> {
    Box::pin(async {
        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let provider = Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "provider must not run before the injected setup failure",
        });
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_owned(),
            workspace.path().to_owned(),
            String::new(),
            provider.clone(),
            "default".into(),
            context_config(),
        )?;
        let source = runtime
            .enqueue(MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "accepted input whose setup fails before StartedTurn".into(),
                },
            ))
            .await?;
        let actual_source = runtime
            .storage()
            .read_message_by_id(&source.id)?
            .context("original admitted source missing")?;
        ensure!(actual_source.message_seq.is_some());
        ensure!(runtime.agent_state().await?.current_turn_id.is_none());
        runtime.runtime_db().transaction(|tx| {
            // The sole queued message fails at exactly the pre-Start audit.
            // Claim, error/terminal audits and all remaining SQL stay writable.
            tx.execute_batch(
                "CREATE TRIGGER fixture_fail_processing_start_once
                 BEFORE INSERT ON audit_events
                 WHEN NEW.kind='message_processing_started'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected_prestart_audit_failure');
                 END;",
            )?;
            Ok(())
        })?;
        let mut runner = RuntimeTask(tokio::spawn(runtime.clone().run()));
        let result: Result<()> = Box::pin(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if runner.0.is_finished() {
                        let output = (&mut runner.0).await;
                        anyhow::bail!(
                            "runtime exited before the original pre-Start failure settled: {output:?}"
                        );
                    }
                    if runtime
                        .runtime_db()
                        .queue_entries()
                        .latest(&source.id)?
                        .is_some_and(|entry| entry.status == QueueEntryStatus::Aborted)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .context("pre-Start error left the original claim unsettled")??;

            let events = runtime.storage().read_recent_events(usize::MAX)?;
            ensure!(events.iter().any(|event| {
                event.kind == "queue_entry_claimed"
                    && event.data["message_id"].as_str() == Some(source.id.as_str())
            }), "the fixture must exercise an actual original Claim");
            ensure!(!events.iter().any(|event| {
                event.kind == "message_processing_started" || event.kind == "turn_started"
            }), "the injected failure must precede actual StartedTurn");
            let failure = events.iter().find(|event| {
                event.kind == "runtime_error"
                    && event.data["message_id"].as_str() == Some(source.id.as_str())
            }).context("the original runtime_error was not durably recorded")?;
            ensure!(serde_json::to_string(&failure.data)?
                .contains("injected_prestart_audit_failure"),
                "error settlement must preserve the original setup failure");
            let turns = runtime.storage().read_recent_turns(16)?;
            let terminal = turns.iter().find_map(|turn| {
                turn.terminal.as_ref().filter(|terminal| {
                    terminal.kind == TurnTerminalKind::Aborted
                        && terminal.reason.as_deref() == Some("runtime_error")
                })
            }).context("the accepted queue failure has no canonical aborted terminal")?;
            ensure!(terminal.kind == TurnTerminalKind::Aborted);
            let state = runtime.agent_state().await?;
            ensure!(state.pending == 0 && state.current_run_id.is_none());
            ensure!(provider.call_count().await == 0,
                "setup failure must not call the provider");
            ensure!(!runtime.runtime_db().queue_entries().latest_all()?.iter().any(|entry| {
                entry.message_id == source.id && entry.status == QueueEntryStatus::Dequeued
            }));
            let checkpoints: usize = runtime.runtime_db().transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT COUNT(*) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2",
                    rusqlite::params!["default",runtime.inner.identity_incarnation],
                    |row| row.get(0),
                )?)
            })?;
            ensure!(checkpoints == 0,
                "claim-only failure must not fabricate a dispatch memory Start");
            // A successful service shutdown and joined loop prove that settling
            // this error did not merely leave an already-failed background task.
            runtime.request_service_shutdown().await?;
            tokio::time::timeout(Duration::from_secs(5), &mut runner.0)
                .await
                .context("runtime did not exit after explicit shutdown")???;
            Ok(())
        }).await;
        if !runner.0.is_finished() {
            runner.0.abort();
            let _ = (&mut runner.0).await;
        }
        result
    }).await
}
