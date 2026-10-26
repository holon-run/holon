//! Actual owner/common-commit replay, without a provider request or raw SQL setup.
use super::*;
use crate::{
    provider::StubProvider,
    runtime::{
        resident_input_evidence::InputEvidenceScope,
        scheduler_executor::{RunLoopPoll, ScheduledMessage, SchedulerDecisionExecutor},
    },
    runtime_db::{
        context_inputs::ContextDurableRead,
        dispatch_memory::{load_dispatch_memory_tx, DispatchMemoryError, DispatchMemoryRecord},
        episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
        input_mutation_receipts::ReceiptError,
        transitions::{
            AgentStateMutation, PreparedInputTransition, TurnTerminalTransitionCommand,
            WorkItemMutation, WorkItemTransitionCommand,
        },
    },
    types::{TurnTerminalKind, TurnTerminalRecord, TurnTerminalSummary, WorkItemPlanStatus},
};
use anyhow::ensure;
use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin};

struct Fixture {
    runtime: RuntimeHandle,
    scheduled: Option<ScheduledMessage>,
    _home: tempfile::TempDir,
    _workspace: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_owned(),
            workspace.path().to_owned(),
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            crate::runtime::tests::support::context_config(),
        )?;
        runtime
            .storage()
            .enable_event_bus(crate::storage::EventBus::new(64))?;
        Ok(Self {
            runtime,
            scheduled: None,
            _home: home,
            _workspace: workspace,
        })
    }
    fn scope(&self) -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: self.runtime.instance_incarnation(),
        }
    }
    fn work(&self) -> Pin<Box<impl Future<Output = Result<WorkItemRecord>> + Send + '_>> {
        Box::pin(async {
            let work = self
                .runtime
                .create_work_item(
                    "actual automatic memory Work".into(),
                    Some(WorkItemPlanStatus::Ready),
                    Some("Perform the scoped fixture update.".into()),
                    Vec::new(),
                )
                .await?;
            self.runtime.pick_work_item(work.id.clone()).await?;
            self.runtime
                .runtime_db()
                .work_items()
                .latest(&work.id)?
                .context("picked Work missing")
        })
    }
    fn start(&mut self) -> Pin<Box<impl Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async {
            ensure!(self.scheduled.is_none(), "fixture dispatch already started");
            self.runtime
                .enqueue(MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: Some("memory-fixture".into()),
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "perform this original dispatch".into(),
                    },
                ))
                .await?;
            let RunLoopPoll::Message(scheduled) =
                SchedulerDecisionExecutor::new(&self.runtime).poll().await?
            else {
                anyhow::bail!("actual source was not claimed");
            };
            self.runtime
                .begin_dispatch_turn(
                    &scheduled.message,
                    None,
                    None,
                    scheduled
                        .dispatch_plan
                        .execution_admission_provenance
                        .clone(),
                    &scheduled.dispatch_plan.prior_closure,
                )
                .await?;
            self.scheduled = Some(scheduled);
            assert!(matches!(
                self.memory()?,
                DispatchMemoryRecord::Started { .. }
            ));
            Ok(())
        })
    }
    fn memory(&self) -> Result<DispatchMemoryRecord> {
        let source = &self
            .scheduled
            .as_ref()
            .context("actual fixture dispatch missing")?
            .message;
        self.runtime.runtime_db().transaction(|tx| {
            let state = ContextDurableRead::new(tx, "default")
                .scope_facts()?
                .into_owned_state()?
                .context("State missing")?;
            let key = EpisodeMemoryKey {
                agent_id: "default".into(),
                identity_incarnation: self.scope().identity_incarnation,
                actual_dispatch_turn_id: state.current_turn_id.context("actual Turn missing")?,
            };
            load_dispatch_memory_tx(
                tx,
                &key,
                &EpisodeMemoryInvocation {
                    message_id: source.id.clone(),
                    turn_index: state.turn_index,
                },
            )?
            .context("original dispatch checkpoint missing")
        })
    }
    fn updated_work(&self, id: &str, objective: &str) -> Result<WorkItemRecord> {
        let mut work = self
            .runtime
            .runtime_db()
            .work_items()
            .latest(id)?
            .context("Work missing")?;
        work.revision += 1;
        work.updated_at += chrono::Duration::milliseconds(1);
        work.objective = objective.into();
        Ok(work)
    }
    fn work_command(&self, work: &WorkItemRecord, nonce: &str) -> WorkItemTransitionCommand {
        WorkItemTransitionCommand {
            agent_id: "default".into(),
            mutation: WorkItemMutation::Update {
                record: work.clone(),
                expected_revision: work.revision - 1,
            },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: vec![AuditEvent::legacy(
                "automatic_memory_work_updated",
                serde_json::json!({
                    "agent_id":"default", "work_item_id":work.id, "original_command":nonce,
                }),
            )],
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        }
    }
    fn plain_work(
        &self,
        nonce: &str,
        at: DateTime<Utc>,
        command: &WorkItemTransitionCommand,
    ) -> Result<PreparedInputTransition> {
        PreparedInputTransition::work(
            self.scope(),
            nonce.into(),
            at,
            command.clone(),
            Default::default(),
            false,
        )
    }
    fn terminal_command(&self, state: AgentState) -> Result<TurnTerminalTransitionCommand> {
        let turn_id = state
            .current_turn_id
            .clone()
            .context("actual Turn missing")?;
        let at = self.runtime.now();
        let terminal = TurnTerminalRecord {
            turn_id: turn_id.clone(),
            turn_index: state.turn_index,
            kind: TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: None,
            no_brief_reason: None,
            checkpoint: None,
            completed_at: at,
            duration_ms: 1,
        };
        let mut turn = self.runtime.runtime_db().transaction(|tx| {
            ContextDurableRead::new(tx, "default")
                .turn_by_id(&turn_id)?
                .context("actual Turn missing")
        })?;
        turn.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
        let mut after = state.clone();
        after.status = AgentStatus::AwakeIdle;
        after.current_run_id = None;
        after.last_turn_terminal = Some(terminal);
        Ok(TurnTerminalTransitionCommand {
            agent_id: "default".into(),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(state)),
                record: Box::new(after),
            },
            turn_record: turn,
            terminal_tool_executions: Vec::new(),
            audit_events: vec![AuditEvent::legacy(
                "automatic_memory_terminal",
                serde_json::json!({"agent_id":"default"}),
            )],
            fault: None,
        })
    }
    fn commit(
        &self,
        prepared: PreparedInputTransition,
    ) -> Pin<Box<impl Future<Output = Result<TransitionCommit>> + Send + '_>> {
        Box::pin(async move {
            let mut mutation = self.runtime.begin_mutation().await?;
            let commit = mutation.commit_prepared_input_transition(prepared).await?;
            drop(mutation);
            self.runtime.apply_transition_commit(commit.clone()).await;
            Ok(commit)
        })
    }
    fn finish(
        &self,
        nonce: &str,
    ) -> Pin<Box<impl Future<Output = Result<TurnTerminalTransitionCommand>> + Send + '_>> {
        let nonce = nonce.to_owned();
        Box::pin(async move {
            let command = self.terminal_command(self.runtime.agent_state().await?)?;
            self.commit(PreparedInputTransition::terminal(
                self.scope(),
                nonce,
                self.runtime.now(),
                command.clone(),
            )?)
            .await?;
            assert!(matches!(self.memory()?, DispatchMemoryRecord::Done { .. }));
            Ok(command)
        })
    }
    /// Actual durable rows, including original receipt/auto decoration. No hash
    /// substitutes for the functional/root/EventBus assertions below.
    fn durable_image(&self) -> Result<Vec<(String, Vec<String>)>> {
        self.runtime.runtime_db().transaction(|tx| {
            [
                ("agent_state", "SELECT payload_json FROM agent_states ORDER BY agent_id"),
                ("work", "SELECT payload_json FROM work_items ORDER BY work_item_id"),
                ("turn", "SELECT payload_json FROM turn_records ORDER BY turn_id"),
                ("memory", "SELECT payload_json FROM dispatch_memory_checkpoints ORDER BY agent_id,identity_incarnation,turn_id"),
                ("automatic", "SELECT payload_json FROM dispatch_memory_auto_commands ORDER BY command_nonce"),
                ("receipt", "SELECT prepared_json || outcome_json || committed_at FROM input_mutation_receipts ORDER BY command_nonce"),
                ("audit", "SELECT data_json FROM audit_events ORDER BY event_seq"),
                ("episode", "SELECT payload_json FROM context_episode_anchors ORDER BY episode_id"),
            ].into_iter().map(|(name, sql)| {
                let mut statement = tx.prepare(sql)?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((name.into(), rows))
            }).collect()
        })
    }
    fn auto_count(&self, nonce: &str) -> Result<u64> {
        self.runtime.runtime_db().transaction(|tx| {
            tx.query_row(
                "SELECT COUNT(*) FROM dispatch_memory_auto_commands WHERE command_nonce=?1",
                [nonce],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
    }
    fn assert_published_state(&self, expected: &AgentState) -> Result<()> {
        let root = self
            .runtime
            .inner
            .mutation_owner
            .capture()?
            .context("published root missing")?;
        assert_eq!(root.state.as_ref(), expected);
        assert_eq!(
            self.runtime
                .runtime_db()
                .agent_states()
                .latest("default")?
                .as_ref(),
            Some(expected)
        );
        Ok(())
    }
}

fn assert_replay_effects_empty(commit: &TransitionCommit) {
    // applied is the original result metadata; it is not a replay-effect flag.
    assert!(commit.effects.audit_events.is_empty());
    assert!(commit.effects.agent_state.is_none());
    assert!(commit.effects.work_delta.is_none());
    assert!(commit.effects.input_evidence_delta.records.is_empty());
    assert!(commit.effects.input_evidence_delta.removals.is_empty());
    assert!(commit
        .effects
        .input_evidence_delta
        .turn_transcript_keys
        .is_empty());
    assert!(commit
        .effects
        .input_evidence_delta
        .message_turn_keys
        .is_empty());
    let execution = &commit.effects.input_evidence_delta.execution;
    assert!(execution.initialized.is_none());
    assert!(execution.sources.is_empty());
    assert!(execution.open_attempts.is_empty());
    assert!(execution.nonterminal_work_items.is_empty());
    assert!(execution.work_items.is_empty());
    assert!(!commit.effects.notify_memory_index);
    assert!(!commit.effects.notify_scheduler);
}
fn drain_events(
    receiver: &mut tokio::sync::broadcast::Receiver<crate::storage::PublishedAuditEvent>,
) -> Vec<AuditEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push((*event.event).clone());
    }
    events
}

#[tokio::test]
async fn automatic_memory_runtime_deferred_replay_preserves_frozen_member_even_after_done(
) -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::new()?;
        let work = f.work().await?;
        f.start().await?;
        let at = f.runtime.now();
        let command = f.work_command(
            &f.updated_work(&work.id, "first detached change")?,
            "runtime-auto-deferred",
        );
        let mut receiver = f
            .runtime
            .storage()
            .subscribe_events()?
            .context("event receiver missing")?;
        let first = f
            .commit(f.plain_work("runtime-auto-deferred", at, &command)?)
            .await?;
        assert!(first.applied);
        assert!(drain_events(&mut receiver)
            .iter()
            .any(|event| event.kind == "automatic_memory_work_updated"));
        let pending = f.memory()?;
        assert!(matches!(pending, DispatchMemoryRecord::Pending { .. }));
        let before = f.durable_image()?;
        let replay = f
            .commit(f.plain_work("runtime-auto-deferred", at, &command)?)
            .await?;
        assert_replay_effects_empty(&replay);
        assert!(drain_events(&mut receiver).is_empty());
        assert_eq!(f.memory()?, pending);
        assert_eq!(f.durable_image()?, before);
        assert_eq!(f.auto_count("runtime-auto-deferred")?, 1);
        f.finish("runtime-auto-deferred-finish").await?;
        drain_events(&mut receiver);
        let done = f.memory()?;
        let later = f.runtime.agent_state().await?;
        let after_finish = f.durable_image()?;
        let replay = f
            .commit(f.plain_work("runtime-auto-deferred", at, &command)?)
            .await?;
        assert_replay_effects_empty(&replay);
        assert!(drain_events(&mut receiver).is_empty());
        assert_eq!(f.memory()?, done);
        assert_eq!(f.durable_image()?, after_finish);
        assert_eq!(f.runtime.agent_state().await?, later);
        f.assert_published_state(&later)?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn automatic_memory_runtime_finish_replay_preserves_later_owner_state_and_notifications(
) -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::new()?;
        f.work().await?;
        f.start().await?;
        let command = f.terminal_command(f.runtime.agent_state().await?)?;
        let at = f.runtime.now();
        let mut receiver = f
            .runtime
            .storage()
            .subscribe_events()?
            .context("event receiver missing")?;
        let first = f
            .commit(PreparedInputTransition::terminal(
                f.scope(),
                "runtime-auto-finish".into(),
                at,
                command.clone(),
            )?)
            .await?;
        assert!(first.applied);
        assert!(drain_events(&mut receiver)
            .iter()
            .any(|event| event.kind == "automatic_memory_terminal"));
        assert!(matches!(f.memory()?, DispatchMemoryRecord::Done { .. }));
        {
            let mut mutation = f.runtime.begin_mutation().await?;
            mutation.state.total_model_rounds += 7;
            mutation.state.last_wake_reason = Some("later legal owner State".into());
            mutation.persist_state(&f.runtime.inner.storage).await?;
        }
        drain_events(&mut receiver);
        let later = f.runtime.agent_state().await?;
        let before = f.durable_image()?;
        let replay = f
            .commit(PreparedInputTransition::terminal(
                f.scope(),
                "runtime-auto-finish".into(),
                at,
                command,
            )?)
            .await?;
        assert_replay_effects_empty(&replay);
        assert!(drain_events(&mut receiver).is_empty());
        assert_eq!(f.durable_image()?, before);
        assert_eq!(f.runtime.agent_state().await?, later);
        f.assert_published_state(&later)?;
        assert_eq!(f.auto_count("runtime-auto-finish")?, 1);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn automatic_memory_runtime_rejects_changed_business_same_nonce_without_touching_gate(
) -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::new()?;
        let work = f.work().await?;
        f.start().await?;
        let at = f.runtime.now();
        let command = f.work_command(
            &f.updated_work(&work.id, "original detached intent")?,
            "runtime-auto-conflict",
        );
        f.commit(f.plain_work("runtime-auto-conflict", at, &command)?)
            .await?;
        let before = f.durable_image()?;
        let pending = f.memory()?;
        let state = f.runtime.agent_state().await?;
        let mut changed = command.clone();
        let WorkItemMutation::Update { record, .. } = &mut changed.mutation else {
            unreachable!()
        };
        record.objective = "different typed business under original nonce".into();
        let error = f
            .commit(f.plain_work("runtime-auto-conflict", at, &changed)?)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<ReceiptError>(),
                Some(ReceiptError::CommandConflict)
            ),
            "{error:#}"
        );
        assert_eq!(f.durable_image()?, before);
        assert_eq!(f.memory()?, pending);
        assert_eq!(f.runtime.agent_state().await?, state);
        f.assert_published_state(&state)?;
        assert!(f.runtime.inner.mutation_owner.capture().is_ok());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn automatic_memory_runtime_plain_receipt_is_not_decorated_by_a_later_dispatch() -> Result<()>
{
    Box::pin(async {
        let mut f = Fixture::new()?;
        let work = f.work().await?;
        let at = f.runtime.now();
        let command = f.work_command(
            &f.updated_work(&work.id, "before any actual dispatch")?,
            "runtime-plain-work",
        );
        f.commit(f.plain_work("runtime-plain-work", at, &command)?)
            .await?;
        assert_eq!(f.auto_count("runtime-plain-work")?, 0);
        f.start().await?;
        let before = f.durable_image()?;
        let start = f.memory()?;
        let state = f.runtime.agent_state().await?;
        let mut receiver = f
            .runtime
            .storage()
            .subscribe_events()?
            .context("event receiver missing")?;
        let replay = f
            .commit(f.plain_work("runtime-plain-work", at, &command)?)
            .await?;
        assert_replay_effects_empty(&replay);
        assert!(drain_events(&mut receiver).is_empty());
        assert_eq!(f.auto_count("runtime-plain-work")?, 0);
        assert_eq!(f.memory()?, start);
        assert_eq!(f.durable_image()?, before);
        assert_eq!(f.runtime.agent_state().await?, state);
        f.assert_published_state(&state)?;
        Ok(())
    })
    .await
}

fn memory_completion_counts(f: &Fixture) -> Result<(u64, u64)> {
    f.runtime.runtime_db().transaction(|tx| {
        let done = tx.query_row(
            "SELECT COUNT(*) FROM dispatch_memory_checkpoints WHERE state='done'",
            [],
            |row| row.get(0),
        )?;
        let d1 = tx.query_row("SELECT COUNT(*) FROM episode_memory_results", [], |row| {
            row.get(0)
        })?;
        Ok((done, d1))
    })
}

#[tokio::test]
async fn automatic_memory_runtime_rejects_corrupt_audit_millisecond_and_rolls_back_finish(
) -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::new()?;
        let work = f.work().await?;
        f.start().await?;
        let command = f.work_command(
            &f.updated_work(&work.id, "actual member before corrupt audit")?,
            "runtime-corrupt-audit-member",
        );
        f.commit(f.plain_work("runtime-corrupt-audit-member", f.runtime.now(), &command)?)
            .await?;
        let pending = f.memory()?;
        assert!(matches!(pending, DispatchMemoryRecord::Pending { .. }));
        // Deliberate corruption only: the ordinary writer always projects this
        // column with timestamp(event.created_at). Retain its actual JSON body.
        let (event_id, corrupted_at) = f.runtime.runtime_db().transaction(|tx| {
            let (id, at): (String, String) = tx.query_row(
                "SELECT audit_event_id,created_at FROM audit_events WHERE kind='automatic_memory_work_updated' ORDER BY event_seq DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let corrupted = crate::runtime_db::repositories::timestamp(
                DateTime::parse_from_rfc3339(&at)?.with_timezone(&Utc)
                    + chrono::Duration::milliseconds(1),
            );
            assert_ne!(corrupted, at);
            assert_eq!(tx.execute(
                "UPDATE audit_events SET created_at=?1 WHERE audit_event_id=?2",
                rusqlite::params![corrupted, id],
            )?, 1);
            Ok((id, corrupted))
        })?;
        let state = f.runtime.agent_state().await?;
        let root = f.runtime.inner.mutation_owner.capture()?.context("root missing")?;
        let before = f.durable_image()?;
        let completed = memory_completion_counts(&f)?;
        assert_eq!(completed, (0, 0));
        let source_id = &f.scheduled.as_ref().unwrap().message.id;
        let queue = f.runtime.runtime_db().queue_entries().latest(source_id)?;
        let mut events = f.runtime.storage().subscribe_events()?.context("events missing")?;

        let error = f.finish("runtime-corrupt-audit-finish").await.unwrap_err();
        assert!(format!("{error:#}").contains("memory audit adapter columns and payload disagree"),
            "{error:#}");
        assert_eq!(f.memory()?, pending);
        assert_eq!(f.durable_image()?, before);
        assert_eq!(memory_completion_counts(&f)?, completed);
        assert_eq!(f.runtime.runtime_db().queue_entries().latest(source_id)?, queue);
        assert_eq!(f.runtime.agent_state().await?, state);
        f.assert_published_state(&state)?;
        assert!(Arc::ptr_eq(&root,
            &f.runtime.inner.mutation_owner.capture()?.context("root missing after rollback")?));
        assert!(drain_events(&mut events).is_empty());
        assert_eq!(f.runtime.runtime_db().transaction(|tx| {
            tx.query_row("SELECT created_at FROM audit_events WHERE audit_event_id=?1",
                [&event_id], |row| row.get::<_, String>(0)).map_err(Into::into)
        })?, corrupted_at);
        Ok(())
    }).await
}

#[tokio::test]
async fn automatic_memory_runtime_missing_started_seed_cannot_bypass_as_claim_only() -> Result<()> {
    Box::pin(async {
        let mut f = Fixture::new()?;
        f.work().await?;
        f.start().await?;
        let state = f.runtime.agent_state().await?;
        assert!(state.current_turn_id.is_some());
        assert!(state.current_execution_binding.is_some());
        let source = f.scheduled.as_ref().unwrap().message.clone();
        let mut command = f.terminal_command(state.clone())?;
        let terminal = command.agent_state.record.last_turn_terminal.as_mut()
            .context("original actual terminal missing")?;
        terminal.kind = TurnTerminalKind::Aborted;
        terminal.reason = Some("fixture terminal after corrupted Start".into());
        terminal.no_brief_reason = Some(crate::types::TurnNoBriefReason::Aborted);
        let terminal = terminal.clone();
        command.turn_record.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
        let transition = crate::runtime::turn::TurnTerminalTransition {
            terminal,
            turn_record: f.runtime.own_produced_turn_record(command.turn_record).await?,
            prepared_work_item_completion: None,
            prepared_wait_for: None,
            terminal_tool_executions: Vec::new(),
        };
        let turn_id = state.current_turn_id.as_deref().unwrap();
        // Deliberately remove only the original seed. Keep the actual Turn,
        // source assignment, binding, claim and original Posture receipt intact.
        f.runtime.runtime_db().transaction(|tx| {
            assert_eq!(tx.execute(
                "DELETE FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
                rusqlite::params![f.scope().agent_id, f.scope().identity_incarnation, turn_id],
            )?, 1);
            assert!(!tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM dispatch_memory_legacy_dispatches WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3)",
                rusqlite::params![f.scope().agent_id, f.scope().identity_incarnation, turn_id],
                |row| row.get::<_, bool>(0),
            )?);
            Ok(())
        })?;
        let before = f.durable_image()?;
        let root = f.runtime.inner.mutation_owner.capture()?.context("root missing")?;
        assert_eq!(memory_completion_counts(&f)?, (0, 0));
        let queue = f.runtime.runtime_db().queue_entries().latest(&source.id)?;
        assert_eq!(queue.as_ref().unwrap().status, QueueEntryStatus::Dequeued);
        let mut events = f.runtime.storage().subscribe_events()?.context("events missing")?;

        // Use the real terminal Queue pipeline, including its original execution
        // settlement facts. Aborted/Settle alone must not authorize NoMemory.
        let error = f.runtime.commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: source.agent_id.clone(),
                priority: source.priority.clone(),
                status: QueueEntryStatus::Aborted,
                created_at: source.created_at,
                updated_at: f.runtime.now(),
            },
            Vec::new(),
            true,
            Some(&transition),
        ).await.unwrap_err();
        assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),
            Some(DispatchMemoryError::MissingStart)), "{error:#}");
        assert_eq!(f.durable_image()?, before);
        assert_eq!(memory_completion_counts(&f)?, (0, 0));
        assert_eq!(f.runtime.runtime_db().queue_entries().latest(&source.id)?, queue);
        assert_eq!(f.runtime.agent_state().await?, state);
        f.assert_published_state(&state)?;
        assert!(Arc::ptr_eq(&root,
            &f.runtime.inner.mutation_owner.capture()?.context("root missing after rejection")?));
        assert!(drain_events(&mut events).is_empty());
        Ok(())
    }).await
}
