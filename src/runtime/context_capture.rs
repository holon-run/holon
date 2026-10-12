//! Owner-coherent acquisition for loaded runtimes; projection consumes only values.

use super::*;
use crate::{
    context::{
        acquire_durable_context, inputs::ContextInputs, observe_context_resources,
        FilesystemContextResources,
    },
    domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding},
    prompt::{build_effective_prompt_from_context_snapshot, ToolPromptContext},
    runtime_db::context_inputs::ContextDurableRead,
    types::TurnOwner,
};

#[derive(Debug, thiserror::Error)]
#[error("context configuration changed during acquisition")]
struct ContextConfigurationChanged;

#[derive(Debug, thiserror::Error)]
#[error("context preparation could not capture a stable configuration after three attempts")]
pub(super) struct ContextPreparationRetryExhausted;

#[cfg(test)]
#[derive(Default)]
pub(super) struct ContextCaptureCheckpoint {
    pub(super) armed: AtomicBool,
    pub(super) reached: Notify,
    pub(super) release: Notify,
}

impl RuntimeHandle {
    pub(super) async fn build_loaded_context_prompt(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        require_execution: bool,
    ) -> Result<EffectivePrompt> {
        for attempt in 0..3 {
            match self
                .build_loaded_context_prompt_once(
                    message,
                    continuation,
                    fallback_model,
                    require_execution,
                )
                .await
            {
                Ok(prompt) => return Ok(prompt),
                Err(error)
                    if error
                        .downcast_ref::<ContextConfigurationChanged>()
                        .is_some() =>
                {
                    tracing::debug!(agent_id = %message.agent_id, attempt = attempt + 1,
                        "retrying context acquisition after configuration publication changed");
                    tokio::task::yield_now().await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(
            anyhow::Error::new(ContextPreparationRetryExhausted).context(
                crate::runtime_error::RuntimeError::new(
                    crate::runtime_error::RuntimeErrorDomain::Conflict,
                    "context_preparation_retry_exhausted",
                    "Context preparation is waiting for a stable configuration",
                )
                .with_retryable(true),
            ),
        )
    }

    async fn build_loaded_context_prompt_once(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        require_execution: bool,
    ) -> Result<EffectivePrompt> {
        // RuntimeMutation owns a reservation and private copies, never a held
        // RuntimeAgent mutex. Existing domain adapters remain explicit in D1.
        let mut mutation = self.begin_mutation().await?;
        if require_execution && sync_agent_message_count(&self.inner.storage, &mut mutation.state)?
        {
            mutation.persist_state(&self.inner.storage).await?;
        }
        // Derive from one immutable config publication, not the independently
        // updated legacy context_config cell used by provider reconfiguration.
        let config_version = self.inner.config_snapshot.load_full();
        let mut config = if config_version.provider_reconfig.is_some() {
            config_version.model_catalog.resolved_context_config(
                &config_version.base_context_config,
                fallback_model.or(mutation.state.model_override.as_ref()),
            )
        } else {
            config_version.base_context_config.clone()
        };
        config.prompt_budget_estimated_tokens = config
            .prompt_budget_estimated_tokens
            .checked_sub(crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS)
            .ok_or_else(|| anyhow!("context budget cannot reserve runtime time"))?;
        let captured = mutation.capture_context(config.recent_episode_candidates)?;
        let state = captured.state.as_ref();
        anyhow::ensure!(state.id == message.agent_id, "context input agent mismatch");
        let identity_record = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(&state.id)?
            .ok_or_else(|| anyhow!("context identity missing"))?;
        let identity =
            AgentIdentityView::from_record(&identity_record, &self.inner.default_agent_id);
        let workspace = self.workspace_view_from_state(state)?;
        let model = self.model_state_for_turn(state, fallback_model);
        let history_policy = turn::context_history_preparation(&state.id, &model.effective_model);
        let (provider, tools, apply_patch_surface, _, _) = self
            .provider_tool_selection_for_turn(&identity, fallback_model)
            .await?;
        let prompt_tools = provider.prompt_tool_specs(&tools);
        let skills = self.skills_runtime_view_for_state(state, &identity).await?;

        let acquisition_timer = crate::diagnostics::attribution::CONTEXT_ACQUIRE.start();
        let (durable, execution, control_revision) = {
            let mut connection = self.inner.runtime_db.connection()?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let inputs = ContextDurableRead::new(&transaction, &state.id);
            // The first SELECT fixes the SQLite view. All subsequent durable
            // context and history reads use this same borrowed transaction.
            let scope = inputs.scope_facts()?;
            let durable_identity = scope
                .identity
                .as_ref()
                .ok_or_else(|| anyhow!("context identity missing"))?;
            anyhow::ensure!(
                durable_identity == &identity_record
                    && durable_identity.incarnation == captured.token.identity_incarnation
                    && durable_identity.status == crate::types::AgentRegistryStatus::Active,
                "context identity differs from the reserved publication"
            );
            anyhow::ensure!(
                scope.state.as_ref() == Some(state),
                "context State differs from the reserved publication"
            );
            let control_revision = scope
                .control_revision
                .ok_or_else(|| anyhow!("context control revision missing"))?;
            if require_execution {
                validate_context_execution(&inputs, state, message)?;
            }
            let entries = inputs
                .workspace_entries()?
                .into_iter()
                .map(|entry| (entry.workspace_id, entry.workspace_anchor))
                .collect::<Vec<_>>();
            let mut execution = workspace::execution_snapshot_from_workspace_entries(
                state.execution_profile.clone(),
                &workspace,
                &state.attached_workspaces,
                &entries,
            );
            execution.execution_roots = inputs
                .execution_roots_for_workspaces(&state.attached_workspaces)?
                .into_iter()
                .map(|entry| crate::system::ExecutionRootRef {
                    execution_root_id: entry.execution_root_id,
                    workspace_id: entry.workspace_id,
                    filesystem_path: entry.filesystem_path,
                })
                .collect();
            let durable = acquire_durable_context(
                &inputs,
                state,
                &execution,
                &skills,
                message,
                continuation,
                &config,
                None,
                history_policy,
                &captured,
            )?;
            transaction.commit()?;
            (durable, execution, control_revision)
        };

        // D1 observes resources afresh. This id labels the immutable observation;
        // it is not the later resource cache's invalidation generation.
        let observation = self
            .inner
            .context_resource_observation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| anyhow!("context resource observation generation exhausted"))?
            + 1;
        let agent_home = self.agent_home();
        let snapshot = observe_context_resources(
            durable,
            &FilesystemContextResources {
                data_dir: self.inner.storage.data_dir(),
                agent_home: &agent_home,
            },
            observation,
        )?;
        drop(acquisition_timer);
        #[cfg(test)]
        if self
            .inner
            .context_capture_checkpoint
            .armed
            .swap(false, Ordering::SeqCst)
        {
            self.inner.context_capture_checkpoint.reached.notify_one();
            self.inner
                .context_capture_checkpoint
                .release
                .notified()
                .await;
        }
        let loaded_agents_md = self.loaded_agents_md_for_state(state)?;
        let loaded_agent_memory = self.loaded_agent_memory_for_state()?;
        anyhow::ensure!(
            mutation
                .capture_context(config.recent_episode_candidates)?
                .token
                == captured.token,
            "context publication changed during acquisition"
        );
        if !Arc::ptr_eq(&config_version, &self.inner.config_snapshot.load_full()) {
            return Err(ContextConfigurationChanged.into());
        }
        tracing::debug!(
            agent_id = %state.id,
            snapshot_id = snapshot.id(),
            publication_generation = captured.token.generation,
            control_revision,
            resource_observation = snapshot.resources().generation,
            resource_digest = %snapshot.resources().digest,
            "prepared owner-coherent context with explicit legacy domain adapters"
        );
        drop(mutation);
        build_effective_prompt_from_context_snapshot(
            &snapshot,
            &execution.execution_root,
            &agent_home,
            &identity,
            loaded_agents_md,
            loaded_agent_memory,
            &prompt_tools,
            ToolPromptContext {
                apply_patch_surface: Some(apply_patch_surface),
            },
        )
    }
}

fn validate_context_execution(
    inputs: &impl ContextInputs,
    state: &AgentState,
    message: &MessageEnvelope,
) -> Result<()> {
    let binding = state
        .current_execution_binding
        .as_ref()
        .ok_or_else(|| anyhow!("context execution binding is no longer current"))?;
    anyhow::ensure!(
        binding.source_message_id == message.id
            && state.current_turn_id.as_deref() == Some(binding.turn_id.as_str()),
        "context execution input or turn changed"
    );
    let activation_id = binding
        .activation_id
        .as_ref()
        .ok_or_else(|| anyhow!("context execution has no canonical activation"))?;
    let protocol = inputs
        .execution_protocol_state(&state.id)?
        .ok_or_else(|| anyhow!("context canonical execution missing"))?;
    let attempt = protocol
        .attempts
        .get(activation_id)
        .ok_or_else(|| anyhow!("context canonical activation missing"))?;
    let owner = match &attempt.binding {
        ExecutionBinding::Conversation { interaction_id } => TurnOwner::Conversation {
            interaction_id: interaction_id.clone(),
        },
        ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
            work_item_id: work_item_id.clone(),
        },
        ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle {
            agent_id: agent_id.clone(),
        },
        ExecutionBinding::Command => TurnOwner::Command,
    };
    anyhow::ensure!(
        protocol.agent_id == state.id
            && attempt.agent_id == state.id
            && attempt.state == ExecutionAttemptState::Open
            && attempt.source_message_id.as_deref() == Some(message.id.as_str())
            && binding.owner.as_ref() == Some(&owner),
        "context canonical execution differs from the captured binding"
    );
    let turn = inputs
        .turn_by_id(&binding.turn_id)?
        .ok_or_else(|| anyhow!("context started Turn missing"))?;
    anyhow::ensure!(
        turn.agent_id == state.id
            && turn.turn_index == state.turn_index
            && turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                == Some(message.id.as_str()),
        "context started Turn differs from the captured binding"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

    fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
        let home = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "context-capture",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "context-capture".into(),
            context_config(),
        )
        .unwrap();
        (home, runtime)
    }

    #[tokio::test]
    async fn preview_preserves_published_posture_after_unprojected_history_append() {
        let (_home, runtime) = fixture();
        runtime
            .preview_prompt("warm ingress".into(), AuthorityClass::OperatorInstruction)
            .await
            .unwrap();
        let before = runtime.agent_state().await.unwrap();
        runtime
            .storage()
            .append_message(&MessageEnvelope::new(
                "context-capture",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "fixture history not yet reflected in posture count".into(),
                },
            ))
            .unwrap();
        assert!(runtime.storage().read_all_messages().unwrap().len() > before.total_message_count);
        runtime
            .preview_prompt(
                "inspect without changing posture".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        assert_eq!(runtime.agent_state().await.unwrap(), before);
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .agent_states()
                .latest(&before.id)
                .unwrap()
                .unwrap(),
            before
        );
        runtime.join_owned_mutations().await.unwrap();
    }

    #[tokio::test]
    async fn configuration_change_during_capture_reacquires_before_returning_prompt() {
        let (_home, runtime) = fixture();
        runtime
            .inner
            .context_capture_checkpoint
            .armed
            .store(true, Ordering::SeqCst);
        let preview = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .preview_prompt(
                        "freeze one config version".into(),
                        AuthorityClass::OperatorInstruction,
                    )
                    .await
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.inner.context_capture_checkpoint.reached.notified(),
        )
        .await
        .unwrap();
        let mut replacement = (*runtime.inner.config_snapshot.load_full()).clone();
        replacement.base_context_config.recent_messages = 3;
        runtime.inner.config_snapshot.store(Arc::new(replacement));
        runtime
            .inner
            .context_capture_checkpoint
            .release
            .notify_one();
        let prompt = tokio::time::timeout(std::time::Duration::from_secs(5), preview)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(prompt.frozen_recent_message_limit(), Some(3));
        assert_eq!(
            runtime
                .inner
                .context_resource_observation
                .load(Ordering::Relaxed),
            2
        );
        runtime.join_owned_mutations().await.unwrap();
    }

    #[tokio::test]
    async fn three_configuration_replacements_preserve_input_for_runtime_rebuild() {
        use crate::provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse};
        struct CountingProvider {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait::async_trait]
        impl AgentProvider for CountingProvider {
            async fn complete_turn(
                &self,
                request: ProviderTurnRequest,
            ) -> Result<ProviderTurnResponse> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                StubProvider::new("recovered after configuration stabilised")
                    .complete_turn(request)
                    .await
            }
            fn configured_model_refs(&self) -> Vec<String> {
                vec!["stub".into()]
            }
        }
        let home = tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let runtime = RuntimeHandle::new(
            "context-retry",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            provider.clone(),
            "context-retry".into(),
            context_config(),
        )
        .unwrap();
        let accepted = runtime
            .enqueue(MessageEnvelope::new(
                "context-retry",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: "retain this accepted input across configuration churn".into(),
                },
            ))
            .await
            .unwrap();
        runtime
            .inner
            .context_capture_checkpoint
            .armed
            .store(true, Ordering::SeqCst);
        let loop_task = tokio::spawn(runtime.clone().run());
        for attempt in 0..3 {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                runtime.inner.context_capture_checkpoint.reached.notified(),
            )
            .await
            .unwrap();
            let mut replacement = (*runtime.inner.config_snapshot.load_full()).clone();
            replacement.base_context_config.recent_messages = attempt + 3;
            runtime.inner.config_snapshot.store(Arc::new(replacement));
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            if attempt < 2 {
                runtime
                    .inner
                    .context_capture_checkpoint
                    .armed
                    .store(true, Ordering::SeqCst);
            }
            runtime
                .inner
                .context_capture_checkpoint
                .release
                .notify_one();
        }
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if runtime
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&accepted.id)
                    .unwrap()
                    .is_some_and(|entry| entry.status == QueueEntryStatus::Interrupted)
                {
                    break;
                }
                assert!(
                    !loop_task.is_finished(),
                    "run loop failed before preserving accepted input"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let events = runtime.storage().read_recent_events(2000).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "queue_entry_settled"
                && event.data["message_id"].as_str() == Some(accepted.id.as_str())
                && event.data["status"].as_str() == Some("interrupted")
                && event.data["reason"].as_str() == Some("context_preparation_retry_exhausted")));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let persisted = runtime
            .storage()
            .read_message_by_id(&accepted.id)
            .unwrap()
            .unwrap();
        assert_eq!(persisted.body, accepted.body);
        assert_eq!(persisted.authority_class, accepted.authority_class);
        let stable_config = runtime
            .inner
            .config_snapshot
            .load()
            .base_context_config
            .clone();
        runtime.request_service_shutdown().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), loop_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        runtime.join_owned_mutations().await.unwrap();
        drop(runtime);

        // Interrupted inputs re-enter through the existing bootstrap recovery
        // boundary; context acquisition does not add a background retry loop.
        let runtime = RuntimeHandle::new(
            "context-retry",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            provider.clone(),
            "context-retry".into(),
            stable_config,
        )
        .unwrap();
        let loop_task = tokio::spawn(runtime.clone().run());
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if runtime
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&accepted.id)
                    .unwrap()
                    .is_some_and(|entry| entry.status == QueueEntryStatus::Processed)
                {
                    break;
                }
                assert!(
                    !loop_task.is_finished(),
                    "restarted run loop failed before recovery"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let recovered_input = runtime
            .storage()
            .read_message_by_id(&accepted.id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered_input.body, accepted.body);
        assert_eq!(recovered_input.authority_class, accepted.authority_class);
        runtime.request_service_shutdown().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), loop_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        runtime.join_owned_mutations().await.unwrap();
    }

    #[test]
    fn exhausted_context_retry_preserves_accepted_input_for_recovery() {
        let error = anyhow::Error::new(ContextPreparationRetryExhausted).context(
            crate::runtime_error::RuntimeError::new(
                crate::runtime_error::RuntimeErrorDomain::Conflict,
                "context_preparation_retry_exhausted",
                "Context preparation is waiting for a stable configuration",
            )
            .with_retryable(true),
        );
        assert!(crate::runtime_error::describe_runtime_error(&error).retryable);
        for kind in [MessageKind::OperatorPrompt, MessageKind::TaskResult] {
            assert_eq!(
                super::super::runtime_error_queue_settlement(&kind, &error),
                (
                    QueueEntryStatus::Interrupted,
                    "context_preparation_retry_exhausted"
                )
            );
        }
    }
}
