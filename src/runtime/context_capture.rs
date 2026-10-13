//! Owner-coherent acquisition for loaded runtimes; projection consumes only values.

use super::*;
use crate::{
    context::{
        acquire_durable_context, completion_work_item_ids,
        inputs::{
            captured_work_item, selected_completion_brief_ids, ContextInputs, ResidentContextInputs,
        },
        observe_context_resources, ContextConfig, ContextHistoryPreparation,
        FilesystemContextResources,
    },
    domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding},
    prompt::{build_effective_prompt_from_context_snapshot, ToolPromptContext},
    runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey, InputWindowSelector, LEGACY_INPUT_WINDOW,
    },
    runtime::resident_work::{CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey},
    runtime_db::{
        context_inputs::ContextDurableRead,
        input_evidence_snapshot::{
            InputEvidenceLoadRequest, InputEvidenceSnapshotError, InputWindowRequest,
        },
        work_snapshot::{validate_work_domain_observation_tx, WorkSnapshotError},
    },
    types::TurnOwner,
};

#[derive(Debug, thiserror::Error)]
#[error("context configuration changed during acquisition")]
struct ContextConfigurationChanged;

#[derive(Debug, thiserror::Error)]
#[error("context Work or Trigger dependency changed during acquisition")]
struct ContextDependencyChanged;

#[derive(Debug, thiserror::Error)]
#[error("context preparation could not capture stable configuration and dependencies after three attempts")]
pub(super) struct ContextPreparationRetryExhausted;

#[cfg(test)]
#[derive(Default)]
pub(super) struct ContextCaptureCheckpoint {
    pub(super) armed: AtomicBool,
    pub(super) reached: Notify,
    pub(super) release: Notify,
}

impl RuntimeHandle {
    #[inline(never)]
    pub(super) fn build_loaded_context_prompt<'a>(
        &'a self,
        message: &'a MessageEnvelope,
        continuation: Option<&'a ContinuationResolution>,
        fallback_model: Option<&'a ModelRouteRef>,
        require_execution: bool,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<EffectivePrompt>> + Send + 'a>>
    {
        Box::pin(self.build_loaded_context_prompt_inner(
            message,
            continuation,
            fallback_model,
            require_execution,
        ))
    }

    async fn build_loaded_context_prompt_inner(
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
                        .is_some()
                        || error.downcast_ref::<ContextDependencyChanged>().is_some() =>
                {
                    tracing::debug!(agent_id = %message.agent_id, attempt = attempt + 1,
                        "retrying context acquisition after a frozen dependency changed");
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
                    "Context preparation is waiting for stable configuration and dependencies",
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
        // RuntimeAgent mutex. Remaining domain adapters stay explicit in D3.
        let mut mutation = self.begin_mutation().await?;
        // D3 Message writers publish the exact count with the accepted payload;
        // this consumer never repairs it by counting historical Message rows.
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
        let work_request = context_work_capture_request(&mutation.state);
        mutation.prepare_owner_work_request(&work_request).await?;
        let state = &mutation.state;
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
        let (mandatory_roots, record_limit, byte_limit) =
            mutation.context_input_evidence_policy()?;
        let planned_work = mutation.capture_work(&work_request)?;
        anyhow::ensure!(
            planned_work.scope.agent_id == state.id,
            "context Work scope mismatch"
        );
        // The D6 Trigger adapter is explicit. Freeze its existing predicate/order
        // before selecting the bounded completion heads; revalidate below.
        let triggers = {
            let mut connection = self.inner.runtime_db.connection()?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let durable = ContextDurableRead::new(&transaction, &state.id);
            let facts = durable.scope_facts()?;
            anyhow::ensure!(
                facts.state.as_ref() == Some(state)
                    && facts.identity.as_ref() == Some(&identity_record)
                    && identity_record.incarnation == planned_work.scope.identity_incarnation,
                "context Work differs from the reserved publication"
            );
            validate_work_domain_observation_tx(
                &transaction,
                &planned_work.scope,
                planned_work.revision,
            )
            .map_err(context_work_observation_error)?;
            let triggers = durable.external_triggers()?;
            transaction.commit()?;
            triggers
        };
        let projection = planned_work.work_queue(state, triggers.clone())?;
        let bound_work = state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.work_item_id.as_deref())
            .map(|id| captured_work_item(&planned_work, id))
            .transpose()?
            .flatten();
        let completion_work_ids = completion_work_item_ids(bound_work.as_ref(), &projection);
        let mut input_request = context_input_evidence_request(
            state,
            require_execution.then_some(message),
            &config,
            history_policy,
            mandatory_roots,
            record_limit,
            byte_limit,
        )?;
        // Completion is its own owning path. Ordinary mandatory roots retain
        // their strict meaning even when they name the same Brief or source.
        input_request.selected_completion_brief_ids =
            selected_completion_brief_ids(&planned_work, &completion_work_ids)?;
        // Acquisition also preserves the independent ordinary closure policy.
        // Prompt selection still uses its original frozen request below.
        let mut acquisition_request = input_request.clone();
        super::scheduler::include_scheduler_brief_window(&mut acquisition_request.windows);
        mutation
            .prepare_owner_input_request(&acquisition_request, config.recent_episode_candidates)
            .await?;
        let captured = mutation.capture_context_with_inputs_and_work(
            config.recent_episode_candidates,
            &input_request,
            &work_request,
        )?;
        let work = captured
            .work
            .as_ref()
            .ok_or_else(|| crate::runtime::resident_work::WorkReadError::IncompleteLive)?;
        validate_work_selection(&planned_work, work)?;
        let state = captured.state.as_ref();
        let resident = captured.input_evidence.as_ref().ok_or_else(|| {
            InputEvidenceSnapshotError::NeedsPreparation(
                "loaded context input domain is not prepared".into(),
            )
        })?;

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
            let durable_inputs = ContextDurableRead::new(&transaction, &state.id);
            // The first SELECT fixes the SQLite view. All subsequent durable
            // context and history reads use this same borrowed transaction.
            let scope = durable_inputs.scope_facts()?;
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
            validate_work_domain_observation_tx(&transaction, &work.scope, work.revision)
                .map_err(context_work_observation_error)?;
            validate_trigger_selection(&triggers, &durable_inputs.external_triggers()?)?;
            let inputs =
                ResidentContextInputs::new(&durable_inputs, resident, work, state, &triggers)?;
            if require_execution {
                validate_context_execution(&inputs, state, message)?;
            }
            let entries = durable_inputs
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
            execution.execution_roots = durable_inputs
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
            "prepared context from resident input evidence and explicit remaining domain adapters"
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

/// Shared with bootstrap/writers: State focus and the actual execution owner
/// are named roots; the full live/128 policy remains explicit in the request.
pub(super) fn context_work_capture_request(state: &AgentState) -> WorkCaptureRequest {
    let mut request = WorkCaptureRequest::current();
    request.mandatory_roots.extend(
        state
            .current_work_item_id
            .iter()
            .chain(
                state
                    .current_execution_binding
                    .iter()
                    .filter_map(|binding| binding.work_item_id.as_ref()),
            )
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    request
}

fn context_work_observation_error(error: anyhow::Error) -> anyhow::Error {
    if matches!(
        error.downcast_ref::<WorkSnapshotError>(),
        Some(WorkSnapshotError::RevisionMismatch { .. })
    ) {
        ContextDependencyChanged.into()
    } else {
        error
    }
}

fn validate_work_selection(planned: &CapturedWork, captured: &CapturedWork) -> Result<()> {
    if planned.scope != captured.scope || planned.revision != captured.revision {
        return Err(ContextDependencyChanged.into());
    }
    Ok(())
}
fn validate_trigger_selection(
    planned: &[ExternalTriggerRecord],
    current: &[ExternalTriggerRecord],
) -> Result<()> {
    if planned != current {
        return Err(ContextDependencyChanged.into());
    }
    Ok(())
}

/// Shared by explicit preparation actors and loaded capture. It freezes only
/// policy and typed roots; it never hydrates, adopts hinted Turns or reads SQL.
pub(super) fn context_input_evidence_request(
    state: &AgentState,
    message: Option<&MessageEnvelope>,
    config: &ContextConfig,
    history: ContextHistoryPreparation,
    mut mandatory_roots: BTreeSet<InputEvidenceKey>,
    record_limit: usize,
    byte_limit: usize,
) -> Result<InputEvidenceLoadRequest> {
    anyhow::ensure!(
        record_limit > 0 && byte_limit > 0,
        "input evidence budget must be positive"
    );
    for requested in [config.recent_messages, config.recent_briefs]
        .into_iter()
        .chain(history.work_item_scoped_window)
    {
        if requested > LEGACY_INPUT_WINDOW {
            return Err(InputEvidenceSnapshotError::PolicyOutsideQualification {
                requested,
                maximum: LEGACY_INPUT_WINDOW,
            }
            .into());
        }
    }
    anyhow::ensure!(
        history.selector != crate::projection_eval::HistorySelector::WorkItemScoped
            || history.work_item_scoped_window.is_some(),
        "work-item history policy requires a frozen window before acquisition"
    );
    let mut recovery = false;
    if let Some(message) = message {
        anyhow::ensure!(message.agent_id == state.id, "context input agent mismatch");
        mandatory_roots.insert(InputEvidenceKey::new(Family::Message, &message.id));
        if let Some((turn_id, message_id, _)) = provider_recovery::continuity_sources(message)? {
            recovery = true;
            mandatory_roots.insert(InputEvidenceKey::new(Family::Turn, turn_id));
            mandatory_roots.insert(InputEvidenceKey::new(Family::Message, message_id));
        }
        // Message.turn_id is only a preallocated/replay hint. The resident
        // loader follows the canonical assignment edge, never this raw hint.
    }
    if let Some(binding) = &state.current_execution_binding {
        mandatory_roots.insert(InputEvidenceKey::new(
            Family::Message,
            &binding.source_message_id,
        ));
        mandatory_roots.insert(InputEvidenceKey::new(Family::Turn, &binding.turn_id));
        if let Some(attempt_id) = &binding.activation_id {
            mandatory_roots.insert(InputEvidenceKey::new(Family::Attempt, attempt_id));
        }
        if let Some(work_item_id) = &binding.work_item_id {
            mandatory_roots.insert(InputEvidenceKey::new(Family::WorkExecution, work_item_id));
        }
    }
    let mut windows = vec![
        InputWindowRequest {
            selector: InputWindowSelector::GlobalTurns,
            limit: config.recent_messages + 1,
        },
        InputWindowRequest {
            selector: InputWindowSelector::RecentBriefs,
            limit: config.recent_briefs,
        },
        InputWindowRequest {
            selector: InputWindowSelector::RecentTools,
            limit: config.recent_messages,
        },
        InputWindowRequest {
            selector: InputWindowSelector::RecentTranscript,
            limit: config.recent_messages,
        },
        InputWindowRequest {
            selector: InputWindowSelector::TrustedOperator,
            limit: 1,
        },
    ];
    let work_item_id = if let Some(binding) = &state.current_execution_binding {
        binding.work_item_id.as_ref()
    } else if recovery {
        None
    } else {
        state.current_work_item_id.as_ref()
    };
    if let (Some(work_item_id), Some(limit)) = (work_item_id, history.work_item_scoped_window) {
        windows.push(InputWindowRequest {
            selector: InputWindowSelector::OwnerTurns {
                owner: TurnOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                },
            },
            limit: limit + 1,
        });
    }
    Ok(InputEvidenceLoadRequest {
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: Default::default(),
        windows,
        mandatory_roots,
        record_limit,
        byte_limit,
    })
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
    use std::collections::BTreeMap;

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

    #[test]
    fn context_work_request_names_focus_and_actual_owner_without_reading_message_hints() {
        let mut state = AgentState::new("context-capture");
        state.current_work_item_id = Some("focused-work".into());
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            source_message_id: "actual-input".into(),
            turn_id: "actual-turn".into(),
            work_item_id: Some("actual-owner".into()),
            activation_id: None,
            admission_provenance: None,
            owner: None,
            claimed_work_revision: None,
        });
        let request = context_work_capture_request(&state);
        assert!(request.include_live);
        assert_eq!(
            request.nonopen_limit,
            crate::runtime::resident_work::WORK_NONOPEN_WINDOW
        );
        assert_eq!(
            request.mandatory_roots,
            BTreeSet::from([
                WorkKey::new(WorkFamily::Work, "focused-work"),
                WorkKey::new(WorkFamily::Work, "actual-owner")
            ])
        );
    }

    #[test]
    fn frozen_trigger_and_work_changes_require_replanning_instead_of_reusing_old_heads() {
        let work = |revision| CapturedWork {
            scope: crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: "context-capture".into(),
                identity_incarnation: 1,
            },
            revision,
            live_complete: true,
            live_work_ids: BTreeSet::new(),
            unresolved_wait_ids: BTreeSet::new(),
            active_frame_ids: BTreeSet::new(),
            turn_wait_keys: BTreeMap::new(),
            task_wait_keys: BTreeMap::new(),
            trigger_wait_keys: BTreeMap::new(),
            nonopen: crate::runtime::resident_work::WorkHistoryCoverage {
                limit: crate::runtime::resident_work::WORK_NONOPEN_WINDOW,
                keys: Vec::new(),
                is_exhaustive: true,
            },
            latest_results: BTreeMap::new(),
            observations: BTreeMap::new(),
            records: Vec::new(),
            budget: None,
            budget_views: Vec::new(),
        };
        assert!(validate_work_selection(&work(1), &work(2))
            .unwrap_err()
            .downcast_ref::<ContextDependencyChanged>()
            .is_some());
        let trigger = ExternalTriggerRecord {
            external_trigger_id: "trigger".into(),
            target_agent_id: "context-capture".into(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            token: None,
            token_hash: "hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: Utc::now(),
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        };
        let mut changed = trigger.clone();
        changed.delivery_count = 1;
        assert!(validate_trigger_selection(&[trigger.clone()], &[changed])
            .unwrap_err()
            .downcast_ref::<ContextDependencyChanged>()
            .is_some());
        validate_trigger_selection(&[trigger.clone()], &[trigger]).unwrap();
    }

    #[test]
    fn input_request_preserves_hints_without_adopting_unstarted_turns() {
        let state = AgentState::new("context-capture");
        for kind in [MessageKind::OperatorPrompt, MessageKind::TaskResult] {
            let mut message = MessageEnvelope::new(
                "context-capture",
                kind,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "accepted before a Turn exists".into(),
                },
            );
            message.turn_id = Some("preallocated-not-started".into());
            message
                .source_refs
                .insert("unvalidated".into(), "turn:arbitrary-body-ref".into());
            let request = context_input_evidence_request(
                &state,
                Some(&message),
                &context_config(),
                ContextHistoryPreparation::default(),
                BTreeSet::new(),
                128,
                1024 * 1024,
            )
            .unwrap();
            assert_eq!(
                request.mandatory_roots,
                BTreeSet::from([InputEvidenceKey::new(Family::Message, &message.id)])
            );
        }
        let request = context_input_evidence_request(
            &state,
            None,
            &context_config(),
            ContextHistoryPreparation::default(),
            BTreeSet::new(),
            128,
            1024 * 1024,
        )
        .unwrap();
        assert!(
            request.mandatory_roots.is_empty(),
            "synthetic preview text is not a durable Message"
        );
    }

    #[test]
    fn input_request_freezes_current_binding_and_both_history_windows() {
        let mut state = AgentState::new("context-capture");
        state.current_work_item_id = Some("queue-owner-must-not-leak".into());
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("attempt-original".into()),
            admission_provenance: None,
            claimed_work_revision: None,
            source_message_id: "message-original".into(),
            turn_id: "turn-original".into(),
            work_item_id: Some("execution-owner".into()),
            owner: Some(TurnOwner::WorkItem {
                work_item_id: "execution-owner".into(),
            }),
        });
        let history = ContextHistoryPreparation {
            selector: crate::projection_eval::HistorySelector::WorkItemScoped,
            work_item_scoped_window: Some(64),
            window_source: "frozen-test-policy",
        };
        let config = context_config();
        let request = context_input_evidence_request(
            &state,
            None,
            &config,
            history,
            BTreeSet::from([InputEvidenceKey::new(
                Family::Message,
                "accepted-queue-root",
            )]),
            512,
            2 * 1024 * 1024,
        )
        .unwrap();
        for (family, id) in [
            (Family::Message, "accepted-queue-root"),
            (Family::Message, "message-original"),
            (Family::Turn, "turn-original"),
            (Family::Attempt, "attempt-original"),
            (Family::WorkExecution, "execution-owner"),
        ] {
            assert!(request
                .mandatory_roots
                .contains(&InputEvidenceKey::new(family, id)));
        }
        assert!(request
            .windows
            .iter()
            .any(|window| window.selector == InputWindowSelector::GlobalTurns
                && window.limit == config.recent_messages + 1));
        assert!(request.windows.iter().any(|window| window.selector
            == InputWindowSelector::OwnerTurns {
                owner: TurnOwner::WorkItem {
                    work_item_id: "execution-owner".into()
                }
            }
            && window.limit == 65));
        assert!(!request
            .windows
            .iter()
            .any(|window| matches!(&window.selector,
            InputWindowSelector::OwnerTurns {owner:TurnOwner::WorkItem {work_item_id}}
                if work_item_id=="queue-owner-must-not-leak")));
    }

    #[test]
    fn input_request_rejects_public_policy_expansion_before_capture() {
        let state = AgentState::new("context-capture");
        for (messages, briefs, owner_window) in
            [(513, 8, None), (12, 513, None), (12, 8, Some(513))]
        {
            let mut config = context_config();
            config.recent_messages = messages;
            config.recent_briefs = briefs;
            let history = ContextHistoryPreparation {
                work_item_scoped_window: owner_window,
                ..ContextHistoryPreparation::default()
            };
            let error = context_input_evidence_request(
                &state,
                None,
                &config,
                history,
                BTreeSet::new(),
                1024,
                1024 * 1024,
            )
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::PolicyOutsideQualification {
                    requested: 513,
                    maximum: 512
                })
            ));
        }
        let mut config = context_config();
        config.recent_messages = 512;
        let request = context_input_evidence_request(
            &state,
            None,
            &config,
            ContextHistoryPreparation::default(),
            BTreeSet::new(),
            1024,
            1024 * 1024,
        )
        .unwrap();
        assert_eq!(
            request.windows[0].limit, 513,
            "one already-qualified current candidate is distinct from policy expansion"
        );
    }

    fn publish_prompt_history<'a>(
        runtime: &'a RuntimeHandle,
        count: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            use crate::runtime::resident_input_evidence::{
                InputEvidenceFamily as Family, InputEvidenceKey as Key,
            };
            use crate::runtime_db::{
                evidence::{
                    append_message_tx, committed_brief_tx, insert_brief_evidence_tx,
                    upsert_agent_state_tx,
                },
                input_evidence_snapshot::{
                    register_committed_input_evidence_tx, validate_existing_input_evidence_tx,
                    CanonicalInputEvidence as Fact,
                },
                repositories::{committed_turn_record_tx, upsert_turn_record_tx},
                transitions::{AgentStateMutation, PostCommitEffects, TransitionCommit},
            };
            let mut mutation = runtime.begin_mutation().await.unwrap();
            let expected = mutation.state.clone();
            let created_at = Utc::now();
            let committed = mutation
                .commit_transition(crate::ids::audit_event_id(), |scope| {
                    runtime.inner.runtime_db.transaction(|tx| {
                        let mut facts = Vec::new();
                        let mut messages = Vec::new();
                        let mut turns = Vec::new();
                        for index in 0..count {
                            let turn_id = format!("window-history-turn-{index}");
                            let mut message = MessageEnvelope::new(
                                &expected.id,
                                MessageKind::OperatorPrompt,
                                MessageOrigin::Operator {
                                    actor_id: None,
                                    actor_display_name: None,
                                },
                                AuthorityClass::OperatorInstruction,
                                Priority::Normal,
                                MessageBody::Text {
                                    text: format!("history fixture input {index}"),
                                },
                            );
                            message.turn_id = Some(turn_id.clone());
                            message.created_at =
                                created_at + chrono::Duration::seconds(index as i64);
                            let mut brief = BriefRecord::new(
                                &expected.id,
                                crate::types::BriefKind::Result,
                                format!("scheduler-window-sentinel-{index}"),
                                Some(message.id.clone()),
                                None,
                            );
                            brief.turn_id = Some(turn_id.clone());
                            brief.turn_index = Some(index as u64 + 1);
                            brief.created_at = message.created_at;
                            for key in [
                                Key::new(Family::Message, &message.id),
                                Key::new(Family::Brief, &brief.id),
                                Key::new(Family::Turn, &turn_id),
                            ] {
                                validate_existing_input_evidence_tx(tx, scope, &key)?;
                            }
                            let actual_message = append_message_tx(tx, &message)?.0;
                            insert_brief_evidence_tx(tx, &brief)?;
                            let actual_brief = committed_brief_tx(tx, &expected.id, &brief.id)?;
                            let mut turn = crate::types::TurnRecord::new(
                                &expected.id,
                                &turn_id,
                                index as u64 + 1,
                            );
                            turn.created_at = actual_message.created_at;
                            turn.input_message_ids = vec![actual_message.id.clone()];
                            turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
                                &actual_message,
                            ));
                            turn.produced_brief_ids = vec![actual_brief.id.clone()];
                            turn.terminal = Some(crate::types::TurnTerminalSummary {
                                kind: TurnTerminalKind::Completed,
                                reason: None,
                                no_brief_reason: None,
                                completed_at: turn.created_at,
                                duration_ms: 0,
                            });
                            upsert_turn_record_tx(tx, &turn)?;
                            let actual_turn =
                                committed_turn_record_tx(tx, &expected.id, &turn_id)?.record;
                            facts.extend([
                                Fact::Message(actual_message.clone()),
                                Fact::Brief(actual_brief),
                                Fact::Turn(actual_turn.clone()),
                            ]);
                            messages.push(actual_message);
                            turns.push(actual_turn);
                        }
                        let payload: String = tx.query_row(
                            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                            [&expected.id],
                            |row| row.get(0),
                        )?;
                        let mut state: AgentState = serde_json::from_str(&payload)?;
                        anyhow::ensure!(state == expected, "history fixture State changed");
                        state.turn_index = state.turn_index.max(count as u64);
                        let message_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
                            [&expected.id],
                            |row| row.get(0),
                        )?;
                        state.total_message_count = usize::try_from(message_count)?;
                        upsert_agent_state_tx(tx, &state)?;
                        let payload: String = tx.query_row(
                            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                            [&expected.id],
                            |row| row.get(0),
                        )?;
                        let actual_state: AgentState = serde_json::from_str(&payload)?;
                        let input_evidence_delta =
                            register_committed_input_evidence_tx(tx, scope, &facts)?;
                        Ok(TransitionCommit {
                            applied: true,
                            effects: PostCommitEffects {
                                agent_state: Some(AgentStateMutation {
                                    expected: Some(Box::new(expected.clone())),
                                    record: Box::new(actual_state),
                                }),
                                input_evidence_delta,
                                messages,
                                turns,
                                ..Default::default()
                            },
                            ..Default::default()
                        })
                    })
                })
                .await
                .unwrap();
            drop(mutation);
            runtime.apply_transition_commit(committed).await;
        })
    }

    #[tokio::test]
    async fn named_preparation_preserves_scheduler_briefs_and_prompt_window() {
        let (_home, runtime) = fixture();
        let config = context_config();
        let brief_count = config.recent_briefs + 1;
        assert_eq!(config.recent_messages, config.recent_briefs);
        // Recent Briefs render through their actual Turn; independent Turns
        // keep the oldest result outside both original prompt windows.
        publish_prompt_history(&runtime, brief_count).await;
        let narrow = {
            let mut mutation = runtime.begin_mutation().await.unwrap();
            let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy().unwrap();
            let narrow = InputEvidenceLoadRequest {
                windows: vec![InputWindowRequest {
                    selector: InputWindowSelector::RecentBriefs,
                    limit: 1,
                }],
                mandatory_roots: Default::default(),
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                record_limit,
                byte_limit,
            };
            // A larger Episode request forces real named sidecar preparation;
            // the generic actor must continue preserving this exact narrow policy.
            mutation
                .prepare_owner_input_request(&narrow, config.recent_episode_candidates + 1)
                .await
                .unwrap();
            let inputs = mutation.capture_input_evidence().unwrap();
            assert!(matches!(
                inputs.window(&InputWindowSelector::RecentBriefs, scheduler::SCHEDULER_BRIEF_LIMIT),
                Err(super::super::resident_input_evidence::InputEvidenceReadError::WindowNotPrepared)
            ));
            narrow
        };
        let message = MessageEnvelope::new(
            "context-capture",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "inspect frozen prompt window".into(),
            },
        );
        // The synthetic preview's ordinary closure preflight correctly rejects
        // our deliberately narrowed domain. Exercise the named acquisition
        // directly, which is the production boundary being repaired here.
        let prompt = runtime
            .build_loaded_context_prompt(&message, None, None, false)
            .await
            .unwrap();
        assert!(!prompt
            .rendered_context_attachment
            .contains("scheduler-window-sentinel-0"));
        assert!(prompt
            .rendered_context_attachment
            .contains(&format!("scheduler-window-sentinel-{}", brief_count - 1)));
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let inputs = mutation.capture_input_evidence().unwrap();
        assert_eq!(
            inputs
                .window(
                    &InputWindowSelector::RecentBriefs,
                    scheduler::SCHEDULER_BRIEF_LIMIT
                )
                .unwrap()
                .len(),
            brief_count,
        );
        drop(inputs);
        // The non-context production wrapper has the same independent policy;
        // it must not shrink the window on a later owner/domain reprepare.
        mutation
            .prepare_owner_input_request(&narrow, config.recent_episode_candidates + 2)
            .await
            .unwrap();
        mutation
            .prepare_owner_inputs(Default::default(), Default::default())
            .await
            .unwrap();
        let inputs = mutation.capture_input_evidence().unwrap();
        assert_eq!(
            inputs
                .window(
                    &InputWindowSelector::RecentBriefs,
                    scheduler::SCHEDULER_BRIEF_LIMIT
                )
                .unwrap()
                .len(),
            brief_count,
        );
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
        let mut loop_task = tokio::spawn(runtime.clone().run());
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
                if loop_task.is_finished() {
                    panic!(
                        "run loop failed before preserving accepted input: {:?}",
                        (&mut loop_task).await
                    );
                }
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
