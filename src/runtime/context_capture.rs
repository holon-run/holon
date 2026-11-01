//! Owner-coherent acquisition for loaded runtimes; projection consumes only values.

use super::*;
use crate::{
    context::{
        acquire_durable_context_with_work_queue, completion_work_item_ids,
        inputs::{
            captured_work_item, pending_task_input_roots, selected_completion_brief_ids,
            ContextInputs, ResidentContextInputs,
        },
        ContextAdmissionPlan, ContextConfig, ContextHistoryPreparation, ContextSnapshot,
    },
    domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding},
    prompt::{build_effective_prompt_from_context_snapshot, ToolPromptContext},
    runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey, InputWindowSelector, LEGACY_INPUT_WINDOW,
    },
    runtime::resident_work::{CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey},
    runtime::{
        resident_task::CapturedTasks,
        task_reads::{active_task_request, context_task_request},
    },
    runtime_db::{
        context_inputs::ContextDurableRead,
        input_evidence_snapshot::{
            InputEvidenceLoadRequest, InputEvidenceSnapshotError, InputWindowRequest,
        },
        task_snapshot::{validate_task_domain_observation_with_scope_tx, TaskSnapshotError},
        work_snapshot::{validate_work_domain_observation_with_scope_tx, WorkSnapshotError},
    },
    types::TurnOwner,
};

#[derive(Debug, thiserror::Error)]
#[error("context configuration changed during acquisition")]
struct ContextConfigurationChanged;

#[derive(Debug, thiserror::Error)]
#[error("context Work, Task or Trigger dependency changed during acquisition")]
struct ContextDependencyChanged;

#[derive(Debug, thiserror::Error)]
#[error("context preparation could not capture stable configuration and dependencies after three attempts")]
pub(super) struct ContextPreparationRetryExhausted;

struct PreparedLoadedContext {
    snapshot: ContextSnapshot,
    identity: AgentIdentityView,
    material: crate::prompt::ResourcePromptMaterial,
    tools: Option<(Vec<crate::tool::ToolSpec>, crate::tool::ApplyPatchSurface)>,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct ContextCaptureCheckpoint {
    pub(super) armed: AtomicBool,
    pub(super) before_resources: AtomicBool,
    pub(super) reached: Notify,
    pub(super) release: Notify,
}

impl RuntimeHandle {
    pub(super) async fn prepare_loaded_context_prompt(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        require_execution: bool,
    ) -> Result<EffectivePrompt> {
        let prepared = self
            .prepare_loaded_context_snapshot(
                message,
                continuation,
                fallback_model,
                require_execution,
                None,
            )
            .await?;
        self.project_loaded_context_prompt(prepared, fallback_model)
            .await
    }

    pub(super) async fn prepare_input_admission_tokens(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        admission: ContextAdmissionPlan<'_>,
    ) -> Result<usize> {
        let prepared = self
            .prepare_loaded_context_snapshot(message, continuation, None, false, Some(admission))
            .await?;
        crate::context::input_admission_tokens_from_snapshot(
            &prepared.snapshot,
            prepared.snapshot.prepared_budget(),
        )
    }

    async fn prepare_loaded_context_snapshot(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        require_execution: bool,
        admission: Option<ContextAdmissionPlan<'_>>,
    ) -> Result<PreparedLoadedContext> {
        let mut writer_retries = 0;
        let mut source_changes = 0usize;
        loop {
            self.ensure_context_resources().await?;
            match self
                .build_loaded_context_snapshot(
                    message,
                    continuation,
                    fallback_model,
                    require_execution,
                    admission,
                )
                .await
            {
                Err(error)
                    if writer_retries < 2
                        && error
                            .downcast_ref::<crate::host::context_resources::ContextResourceWritersActive>()
                            .is_some() =>
                {
                    // The failed capture has released its owner before refresh.
                    writer_retries += 1;
                    tracing::debug!(agent_id = %message.agent_id, writer_retries,
                        "re-entering explicit Context preparation after a resource writer raced capture");
                }
                Err(error)
                    if source_changes
                        < super::resource_refresh::RESOURCE_SOURCE_CHANGE_REPREPARES
                        && super::resource_refresh::is_source_changed_error(&error) =>
                {
                    // The published source moved between the preparation and
                    // the capture. Re-prepare on this bounded Context
                    // preparation path instead of returning the stale source.
                    source_changes += 1;
                    tracing::debug!(agent_id = %message.agent_id, source_changes,
                        "re-preparing Context after the resource source changed");
                }
                result => return result,
            }
        }
    }

    #[cfg(test)]
    #[inline(never)]
    pub(super) fn build_loaded_context_prompt<'a>(
        &'a self,
        message: &'a MessageEnvelope,
        continuation: Option<&'a ContinuationResolution>,
        fallback_model: Option<&'a ModelRouteRef>,
        require_execution: bool,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<EffectivePrompt>> + Send + 'a>>
    {
        Box::pin(async move {
            let prepared = self
                .build_loaded_context_snapshot(
                    message,
                    continuation,
                    fallback_model,
                    require_execution,
                    None,
                )
                .await?;
            self.project_loaded_context_prompt(prepared, fallback_model)
                .await
        })
    }

    #[inline(never)]
    fn build_loaded_context_snapshot<'a>(
        &'a self,
        message: &'a MessageEnvelope,
        continuation: Option<&'a ContinuationResolution>,
        fallback_model: Option<&'a ModelRouteRef>,
        require_execution: bool,
        admission: Option<ContextAdmissionPlan<'a>>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PreparedLoadedContext>> + Send + 'a>,
    > {
        Box::pin(self.build_loaded_context_snapshot_inner(
            message,
            continuation,
            fallback_model,
            require_execution,
            admission,
        ))
    }

    async fn build_loaded_context_snapshot_inner(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        require_execution: bool,
        admission: Option<ContextAdmissionPlan<'_>>,
    ) -> Result<PreparedLoadedContext> {
        self.await_initial_resources().await;
        for attempt in 0..3 {
            match self
                .build_loaded_context_snapshot_once(
                    message,
                    continuation,
                    fallback_model,
                    require_execution,
                    admission,
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

    async fn build_loaded_context_snapshot_once(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        require_execution: bool,
        admission: Option<ContextAdmissionPlan<'_>>,
    ) -> Result<PreparedLoadedContext> {
        // RuntimeMutation owns a reservation and private copies, never a held
        // RuntimeAgent mutex. Remaining domain adapters stay explicit in D3.
        let mut mutation = self.begin_mutation().await?;
        // D3 Message writers publish the exact count with the accepted payload;
        // this consumer never repairs it by counting historical Message rows.
        // Derive from one immutable config publication, not the independently
        // updated legacy context_config cell used by provider reconfiguration.
        let config_version = self.inner.config_snapshot.load_full();
        // Complete metadata names only this binding's admitted settlements;
        // retry eligibility must not exclude still-Open CallerAdmitted facts.
        let (mandatory_roots, record_limit, byte_limit) =
            mutation.context_input_evidence_policy()?;
        let metadata_request = active_task_request();
        mutation
            .prepare_owner_task_request(&metadata_request)
            .await?;
        let metadata = mutation.capture_task_read(&metadata_request)?;
        anyhow::ensure!(
            metadata.token.identity_incarnation == metadata.tasks.scope.identity_incarnation
                && metadata.state.id == metadata.tasks.scope.agent_id,
            "context Task publication differs"
        );
        // Start the same phase before Current request keys or recovery decoding.
        // Only synchronous constructors enter it; preparation awaits never do.
        let resource_material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &metadata.tasks.scope,
        )?;
        let model = super::prepare_agent_model_state_for_turn(
            &config_version.model_catalog,
            &config_version.base_context_config,
            &mutation.state,
            fallback_model,
            &metadata.tasks.scope,
            &resource_material,
        )?;
        let config = resource_material.read(|| {
            // All other ContextConfig fields are scalar stack values.
            crate::diagnostics::resource_io::admit_cloned_value(
                &config_version.base_context_config.callback_base_url,
            )?;
            let mut config = config_version.base_context_config.clone();
            if config_version.provider_reconfig.is_some() {
                let policy = &model.model.resolved_policy;
                config.prompt_budget_estimated_tokens = policy.prompt_budget_estimated_tokens;
                config.compaction_trigger_estimated_tokens =
                    policy.compaction_trigger_estimated_tokens;
                config.compaction_keep_recent_estimated_tokens =
                    policy.compaction_keep_recent_estimated_tokens;
            }
            config.prompt_budget_estimated_tokens = config
                .prompt_budget_estimated_tokens
                .checked_sub(crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS)
                .ok_or_else(|| anyhow!("context budget cannot reserve runtime time"))?;
            if let Some(admission) = admission {
                config.prompt_budget_estimated_tokens = config
                    .prompt_budget_estimated_tokens
                    .min(admission.token_budget);
            }
            Ok(config)
        })?;
        let (task_request, _task_selector_budget) = context_task_request(
            &metadata.tasks,
            &metadata.state,
            &self.inner.input_evidence_budget,
            record_limit,
            byte_limit,
        )?;
        drop(metadata);
        mutation.prepare_owner_task_request(&task_request).await?;
        let planned_tasks = mutation.capture_task_read(&task_request)?.tasks;
        let work_request = {
            let _resource_io = resource_material.enter();
            crate::diagnostics::resource_io::admit_cloned_value(&(
                &mutation.state.current_work_item_id,
                mutation
                    .state
                    .current_execution_binding
                    .as_ref()
                    .and_then(|binding| binding.work_item_id.as_ref()),
            ))?;
            let mut request = context_work_capture_request(&mutation.state);
            if let Some(id) = admission.and_then(|plan| plan.work_item_id) {
                crate::diagnostics::resource_io::admit_cloned_value(&id)?;
                request
                    .mandatory_roots
                    .insert(WorkKey::new(WorkFamily::Work, id));
            }
            request
        };
        mutation
            .prepare_owner_work_request_with_material(&work_request, &resource_material)
            .await?;
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
        let history_policy =
            turn::context_history_preparation(&state.id, &model.model.effective_model);
        mutation
            .prepare_owner_scheduling_request_with_material(
                &super::resident_scheduling::SchedulingCaptureRequest::current(),
                &resource_material,
            )
            .await?;
        let state = &mutation.state;
        let planned_scheduling = {
            let _resource_io = resource_material.enter();
            mutation.capture_scheduling_read(
                &super::resident_scheduling::SchedulingCaptureRequest::current(),
            )?
        };
        let planned_work = mutation.capture_work(&work_request)?;
        anyhow::ensure!(
            planned_work.scope.agent_id == state.id,
            "context Work scope mismatch"
        );
        anyhow::ensure!(
            planned_scheduling.state.as_ref() == state
                && planned_scheduling.scheduling.scope == planned_work.scope,
            "context Scheduling publication differs"
        );
        let owned_triggers = planned_scheduling.scheduling.clone_trigger_records(
            &self.inner.input_evidence_budget,
            record_limit,
            byte_limit,
        )?;
        let triggers = &owned_triggers.records;
        let projection =
            planned_work.context_work_queue_with_material(state, triggers, &resource_material)?;
        let bound_work = admission
            .map(|plan| plan.work_item_id)
            .unwrap_or_else(|| {
                state
                    .current_execution_binding
                    .as_ref()
                    .and_then(|binding| binding.work_item_id.as_deref())
            })
            .map(|id| captured_work_item(&planned_work, id))
            .transpose()?
            .flatten();
        anyhow::ensure!(
            admission.and_then(|plan| plan.work_item_id).is_none() || bound_work.is_some(),
            "planned context owner is missing from the qualified Work capture"
        );
        let (completion_work_ids, mut input_request) = {
            let _resource_io = resource_material.enter();
            let ids = completion_work_item_ids(bound_work.as_ref(), &projection)?;
            let request = context_input_evidence_request_for_plan(
                state,
                (require_execution || admission.is_some()).then_some(message),
                &config,
                history_policy,
                mandatory_roots,
                record_limit,
                byte_limit,
                admission,
            )?;
            (ids, request)
        };
        let pending_task_sources = pending_task_input_roots(
            &planned_tasks,
            state,
            &message.id,
            &self.inner.input_evidence_budget,
            record_limit,
            byte_limit,
        )?;
        input_request
            .mandatory_roots
            .extend(pending_task_sources.roots);
        // Completion is its own owning path. Ordinary mandatory roots retain
        // their strict meaning even when they name the same Brief or source.
        {
            let _resource_io = resource_material.enter();
            input_request.selected_completion_brief_ids =
                selected_completion_brief_ids(&planned_work, &completion_work_ids)?;
        }
        // The owner adds the ordinary closure policy without changing this
        // frozen prompt selection. Do not copy its selectors a second time here.
        mutation
            .prepare_owner_input_request_with_material(
                &input_request,
                config.recent_episode_candidates,
                &resource_material,
            )
            .await?;
        let captured = mutation.capture_context_with_inputs_and_work_and_tasks(
            config.recent_episode_candidates,
            &input_request,
            &work_request,
            &task_request,
            &resource_material,
        )?;
        let work = captured
            .work
            .as_ref()
            .ok_or_else(|| crate::runtime::resident_work::WorkReadError::IncompleteLive)?;
        validate_work_selection(&planned_work, work)?;
        let tasks = captured
            .tasks
            .as_ref()
            .ok_or(crate::runtime::resident_task::TaskReadError::IncompleteActive)?;
        validate_task_selection(&planned_tasks, tasks)?;
        let state = captured.state.as_ref();
        let resident = captured.input_evidence.as_ref().ok_or_else(|| {
            InputEvidenceSnapshotError::NeedsPreparation(
                "loaded context input domain is not prepared".into(),
            )
        })?;

        let tools = if admission.is_none() {
            let (_, tools, surface, _, _) = self
                .provider_tool_selection_for_turn(fallback_model)
                .await?;
            Some((tools, surface))
        } else {
            None
        };
        anyhow::ensure!(
            resident.scope == planned_tasks.scope,
            "context request material scope differs"
        );
        #[cfg(test)]
        if self
            .inner
            .context_capture_checkpoint
            .before_resources
            .swap(false, Ordering::SeqCst)
        {
            self.inner.context_capture_checkpoint.reached.notify_one();
            self.inner
                .context_capture_checkpoint
                .release
                .notified()
                .await;
        }
        let observation =
            self.capture_context_resources(state, &config_version, &resource_material)?;

        let acquisition_timer = crate::diagnostics::attribution::CONTEXT_ACQUIRE.start();
        let (durable, control_revision) = {
            let _resource_io = resource_material.enter();
            let mut connection = self.inner.runtime_db.connection()?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let durable_inputs = ContextDurableRead::new(&transaction, &state.id);
            // The first SELECT fixes the SQLite view. All subsequent durable
            // context and history reads use this same borrowed transaction.
            let scope_read =
                durable_inputs.scope_read_using_decode(mutation.context_state_decode())?;
            let scope = scope_read.facts();
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
                scope.state.as_deref() == Some(state),
                "context State differs from the reserved publication"
            );
            let control_revision = scope
                .control_revision
                .ok_or_else(|| anyhow!("context control revision missing"))?;
            validate_work_domain_observation_with_scope_tx(
                &transaction,
                &work.scope,
                work.revision,
                &scope_read,
            )
            .map_err(context_work_observation_error)?;
            validate_task_domain_observation_with_scope_tx(
                &transaction,
                &tasks.scope,
                tasks.revision,
                &scope_read,
            )
            .map_err(context_task_observation_error)?;
            let scheduling = captured
                .scheduling
                .as_ref()
                .ok_or(super::resident_scheduling::SchedulingReadError::IncompleteLive)?;
            anyhow::ensure!(
                work.scope == planned_work.scope && work.revision == planned_work.revision,
                "context Work revision differs from its same-root plan"
            );
            anyhow::ensure!(
                scheduling.scope == planned_scheduling.scheduling.scope
                    && scheduling.revision == planned_scheduling.scheduling.revision,
                "context Scheduling revision differs from its same-root plan"
            );
            let inputs =
                ResidentContextInputs::new(&durable_inputs, resident, work, state, &triggers)?
                    .with_tasks(tasks)?;
            if require_execution {
                validate_context_execution(&inputs, state, message)?;
            }
            let entries = durable_inputs.workspace_entries()?;
            let mut execution = workspace::execution_snapshot_from_workspace_entries(
                &state.execution_profile,
                &workspace,
                &state.attached_workspaces,
                entries,
            )?;
            let roots =
                durable_inputs.execution_roots_for_workspaces(&state.attached_workspaces)?;
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(
                    roots
                        .len()
                        .checked_mul(std::mem::size_of::<crate::system::ExecutionRootRef>())
                        .ok_or_else(|| anyhow!("ResourceExhausted: execution root slots"))?,
                )?,
                roots.len(),
            )?;
            execution.execution_roots = roots
                .into_iter()
                .map(|entry| crate::system::ExecutionRootRef {
                    execution_root_id: entry.execution_root_id,
                    workspace_id: entry.workspace_id,
                    filesystem_path: entry.filesystem_path,
                })
                .collect();
            let durable = acquire_durable_context_with_work_queue(
                &inputs,
                state,
                std::borrow::Cow::Owned(execution),
                crate::context::ContextSnapshotSkills::Observed(observation.clone()),
                message,
                continuation,
                &config,
                None,
                history_policy,
                &captured,
                Some(projection),
                admission,
            )?;
            transaction.commit()?;
            (durable, control_revision)
        };

        let snapshot = {
            let _resource_io = resource_material.enter();
            let snapshot = durable.observe_loaded(observation.clone(), resource_material.guard()?);
            resource_material.finish()?;
            snapshot?
        };
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
            resource_generation = observation.generation(),
            resource_freshness = ?self.inner.resources.registry.freshness()?,
            resource_digest = %snapshot.resources().digest,
            "prepared context from resident input evidence and explicit remaining domain adapters"
        );
        drop(mutation);
        Ok(PreparedLoadedContext {
            snapshot,
            identity,
            material: crate::prompt::ResourcePromptMaterial {
                observation,
                allocation: Some(resource_material.guard()?),
                model: Some(model),
            },
            tools,
        })
    }

    async fn project_loaded_context_prompt(
        &self,
        prepared: PreparedLoadedContext,
        fallback_model: Option<&ModelRouteRef>,
    ) -> Result<EffectivePrompt> {
        let PreparedLoadedContext {
            snapshot,
            identity,
            material,
            tools,
        } = prepared;
        let observation = &material.observation;
        let loaded_agents_md =
            crate::prompt::PromptResource::observed(observation.clone(), |body| &body.guidance);
        let loaded_agent_memory =
            crate::prompt::PromptResource::observed(observation.clone(), |body| &body.memory);
        let (prompt_tools, apply_patch_surface) =
            tools.ok_or_else(|| anyhow!("admission snapshot has no execution tool selection"))?;
        let agent_home = self.agent_home();
        let media_defaults = self.media_tool_defaults(fallback_model).await?;
        let mut prompt = build_effective_prompt_from_context_snapshot(
            &snapshot,
            &snapshot.execution().execution_root,
            &agent_home,
            &identity,
            loaded_agents_md,
            loaded_agent_memory,
            &prompt_tools,
            ToolPromptContext {
                apply_patch_surface: Some(apply_patch_surface),
            },
        )?
        .with_media_tool_defaults(&media_defaults, &prompt_tools);
        prompt._resource_read_material = Some(material);
        Ok(prompt)
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

fn context_task_observation_error(error: anyhow::Error) -> anyhow::Error {
    if matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::RevisionMismatch { .. })
    ) {
        ContextDependencyChanged.into()
    } else {
        error
    }
}

fn validate_task_selection(planned: &CapturedTasks, captured: &CapturedTasks) -> Result<()> {
    if planned.scope != captured.scope || planned.revision != captured.revision {
        return Err(ContextDependencyChanged.into());
    }
    Ok(())
}

fn validate_work_selection(planned: &CapturedWork, captured: &CapturedWork) -> Result<()> {
    if planned.scope != captured.scope || planned.revision != captured.revision {
        return Err(ContextDependencyChanged.into());
    }
    Ok(())
}
#[cfg(test)]
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
    mandatory_roots: BTreeSet<InputEvidenceKey>,
    record_limit: usize,
    byte_limit: usize,
) -> Result<InputEvidenceLoadRequest> {
    context_input_evidence_request_for_plan(
        state,
        message,
        config,
        history,
        mandatory_roots,
        record_limit,
        byte_limit,
        None,
    )
}

fn context_input_evidence_request_for_plan(
    state: &AgentState,
    message: Option<&MessageEnvelope>,
    config: &ContextConfig,
    history: ContextHistoryPreparation,
    mut mandatory_roots: BTreeSet<InputEvidenceKey>,
    record_limit: usize,
    byte_limit: usize,
    admission: Option<ContextAdmissionPlan<'_>>,
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
    if let Some(message) = message {
        anyhow::ensure!(message.agent_id == state.id, "context input agent mismatch");
    }
    crate::diagnostics::resource_io::admit_cloned_value(&(
        message.map(|message| &message.id),
        &state.current_execution_binding,
    ))?;
    let mut recovery = false;
    if let Some(message) = message {
        mandatory_roots.insert(InputEvidenceKey::new(Family::Message, &message.id));
        if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message) {
            if let Some(directive) = message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("provider_recovery"))
            {
                crate::diagnostics::resource_io::admit_serialized_value(directive)?;
            }
        }
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
    let windows = [
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
    crate::diagnostics::resource_io::admit_cloned_value(&windows)?;
    let mut windows = Vec::from(windows);
    let work_item_id = if let Some(admission) = admission {
        admission.work_item_id
    } else if let Some(binding) = &state.current_execution_binding {
        binding.work_item_id.as_deref()
    } else if recovery {
        None
    } else {
        state.current_work_item_id.as_deref()
    };
    if let (Some(work_item_id), Some(limit)) = (work_item_id, history.work_item_scoped_window) {
        crate::diagnostics::resource_io::admit_cloned_value(&work_item_id)?;
        windows.push(InputWindowRequest {
            selector: InputWindowSelector::OwnerTurns {
                owner: TurnOwner::WorkItem {
                    work_item_id: work_item_id.to_owned(),
                },
            },
            limit: limit + 1,
        });
    }
    Ok(InputEvidenceLoadRequest {
        query: None,
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
    let attempt = inputs
        .execution_attempt_by_id(&state.id, activation_id)?
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
        attempt.agent_id == state.id
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
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
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
    fn admission_request_uses_planned_owner_history_without_proposed_execution_roots() {
        let mut state = AgentState::new("context-capture");
        state.current_work_item_id = Some("focused-work".into());
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            source_message_id: "actual-input".into(),
            turn_id: "actual-turn".into(),
            work_item_id: Some("actual-owner".into()),
            activation_id: Some("actual-attempt".into()),
            admission_provenance: None,
            owner: None,
            claimed_work_revision: None,
        });
        let mut message = MessageEnvelope::new(
            &state.id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "plan a new admission".into(),
            },
        );
        message.turn_id = Some("unpublished-turn".into());
        let request = context_input_evidence_request_for_plan(
            &state,
            Some(&message),
            &context_config(),
            ContextHistoryPreparation {
                selector: crate::projection_eval::HistorySelector::WorkItemScoped,
                work_item_scoped_window: Some(8),
                window_source: "test",
            },
            BTreeSet::new(),
            4096,
            64 * 1024 * 1024,
            Some(ContextAdmissionPlan {
                work_item_id: Some("planned-owner"),
                token_budget: 8192,
            }),
        )
        .unwrap();
        for (family, id) in [
            (Family::Message, message.id.as_str()),
            (Family::Message, "actual-input"),
            (Family::Turn, "actual-turn"),
            (Family::Attempt, "actual-attempt"),
            (Family::WorkExecution, "actual-owner"),
        ] {
            assert!(request
                .mandatory_roots
                .contains(&InputEvidenceKey::new(family, id)));
        }
        assert!(!request
            .mandatory_roots
            .contains(&InputEvidenceKey::new(Family::Turn, "unpublished-turn")));
        assert!(!request.mandatory_roots.contains(&InputEvidenceKey::new(
            Family::WorkExecution,
            "planned-owner",
        )));
        let owners = request
            .windows
            .iter()
            .filter_map(|window| match &window.selector {
                InputWindowSelector::OwnerTurns { owner } => Some(owner),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            owners,
            vec![&TurnOwner::WorkItem {
                work_item_id: "planned-owner".into(),
            }]
        );
    }

    #[tokio::test]
    async fn admission_snapshot_keeps_real_publication_and_resources_for_planned_owner(
    ) -> Result<()> {
        let (_home, runtime) = fixture();
        let focus = runtime
            .create_work_item(
                "A".into(),
                Some(crate::types::WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        let owner = runtime
            .create_work_item(
                "B".repeat(96),
                Some(crate::types::WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        runtime.pick_work_item(focus.id.clone()).await?;
        let message = runtime
            .enqueue(MessageEnvelope::new(
                "context-capture",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "complete instruction".into(),
                },
            ))
            .await?;
        let before = runtime.agent_state().await?;
        let focused_tokens = runtime
            .prepare_input_admission_tokens(
                &message,
                None,
                ContextAdmissionPlan {
                    work_item_id: Some(&focus.id),
                    token_budget: usize::MAX,
                },
            )
            .await?;
        let prepared = runtime
            .prepare_loaded_context_snapshot(
                &message,
                None,
                None,
                false,
                Some(ContextAdmissionPlan {
                    work_item_id: Some(&owner.id),
                    token_budget: usize::MAX,
                }),
            )
            .await?;
        let published = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(prepared.snapshot.agent(), &before);
        assert!(std::ptr::eq(
            prepared.snapshot.agent(),
            published.state.as_ref()
        ));
        assert!(Arc::ptr_eq(
            prepared.snapshot.resource_allocation().unwrap(),
            prepared.material.allocation.as_ref().unwrap(),
        ));
        assert!(Arc::ptr_eq(
            prepared.snapshot.resident_resources().unwrap(),
            &prepared.material.observation,
        ));
        let owner_tokens = crate::context::input_admission_tokens_from_snapshot(
            &prepared.snapshot,
            prepared.snapshot.prepared_budget(),
        )?;
        assert!(owner_tokens < focused_tokens);
        assert!(crate::context::build_context_from_snapshot(
            &prepared.snapshot,
            prepared.snapshot.prepared_budget(),
        )
        .is_err());
        assert_eq!(runtime.agent_state().await?, before);
        Ok(())
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
    fn current_request_rejects_large_binding_before_key_construction() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let mut state = AgentState::new("context-capture");
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            source_message_id: "x".repeat(4096),
            turn_id: "turn".into(),
            work_item_id: None,
            activation_id: None,
            admission_provenance: None,
            owner: None,
            claimed_work_revision: None,
        });
        let build = || {
            context_input_evidence_request(
                &state,
                None,
                &context_config(),
                ContextHistoryPreparation::default(),
                BTreeSet::new(),
                65536,
                64 * 1024 * 1024,
            )
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &InputEvidenceScope {
                agent_id: state.id.clone(),
                identity_incarnation: 1,
            },
            20_000,
            65536,
        )?;
        {
            let _active = phase.enter();
            let error = build().unwrap_err();
            assert!(error.to_string().contains("ResourceExhausted"));
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let request = build()?;
        assert!(request.mandatory_roots.contains(&InputEvidenceKey::new(
            Family::Message,
            state
                .current_execution_binding
                .as_ref()
                .unwrap()
                .source_message_id
                .clone(),
        )));
        Ok(())
    }

    #[test]
    fn current_request_admits_recovery_value_before_clone_and_decode() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
            types::{AdmissionContext, MessageDeliverySurface},
        };
        let state = AgentState::new("context-capture");
        let mut message = MessageEnvelope::new(
            &state.id,
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "model_lineage_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: String::new(),
            },
        );
        message.delivery_surface = Some(MessageDeliverySurface::RuntimeSystem);
        message.admission_context = Some(AdmissionContext::RuntimeOwned);
        message.metadata = Some(serde_json::json!({"provider_recovery": "x".repeat(4096)}));
        let build = || {
            context_input_evidence_request(
                &state,
                Some(&message),
                &context_config(),
                ContextHistoryPreparation::default(),
                BTreeSet::new(),
                65536,
                64 * 1024 * 1024,
            )
        };
        assert!(build().unwrap_err().to_string().contains("invalid type"));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &InputEvidenceScope {
                agent_id: state.id.clone(),
                identity_incarnation: 1,
            },
            20_000,
            65536,
        )?;
        {
            let _active = phase.enter();
            assert!(build()
                .unwrap_err()
                .to_string()
                .contains("ResourceExhausted"));
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn current_request_acquisition_clone_uses_the_constructor_phase_remainder() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::{admit_cloned_value, ResourceReadMaterial},
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let state = AgentState::new("context-capture");
        let build = || {
            context_input_evidence_request(
                &state,
                None,
                &context_config(),
                ContextHistoryPreparation::default(),
                BTreeSet::new(),
                65536,
                64 * 1024 * 1024,
            )
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: state.id.clone(),
            identity_incarnation: 1,
        };
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        {
            let _active = phase.enter();
            let request = build()?;
            admit_cloned_value(&request)?;
        }
        let (bytes, records) = phase.finish()?;
        drop(phase);
        let exact = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        {
            let _active = exact.enter();
            let request = build()?;
            admit_cloned_value(&request)?;
            let acquisition = request.clone();
            assert_eq!(acquisition.windows.len(), request.windows.len());
            assert!(admit_cloned_value(&request).is_err());
        }
        assert!(exact.finish().is_err());
        drop(exact);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
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

    pub(super) fn publish_prompt_history<'a>(
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
                            effects: Box::new(PostCommitEffects {
                                agent_state: Some(AgentStateMutation {
                                    expected: Some(Box::new(expected.clone())),
                                    record: Box::new(actual_state),
                                }),
                                input_evidence_delta,
                                messages,
                                turns,
                                ..Default::default()
                            }),
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
                query: None,
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
            // public preparation must also retain the fixed scheduler dependency.
            mutation
                .prepare_owner_input_request(&narrow, config.recent_episode_candidates + 1)
                .await
                .unwrap();
            let inputs = mutation.capture_input_evidence().unwrap();
            assert_eq!(
                inputs
                    .window(
                        &InputWindowSelector::RecentBriefs,
                        scheduler::SCHEDULER_BRIEF_LIMIT,
                    )
                    .unwrap()
                    .len(),
                brief_count,
            );
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
        // Named acquisition must preserve the scheduler dependency without
        // expanding either configured prompt history window.
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
        let mut preview = tokio::spawn({
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
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                _ = runtime.inner.context_capture_checkpoint.reached.notified() => {}
                result = &mut preview => match result {
                    Ok(Err(error)) => panic!("preview failed before capture checkpoint: {error:#}"),
                    Ok(Ok(_)) => panic!("preview returned before capture checkpoint"),
                    Err(error) => panic!("preview task failed before capture checkpoint: {error}"),
                }
            }
        })
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
        // A configuration retry reuses the same immutable resource observation.
        assert_eq!(
            prompt
                ._resource_read_material
                .as_ref()
                .unwrap()
                .observation
                .epoch(),
            1
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
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
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
        assert!(events.iter().all(|event| event.kind != "turn_started"));
        assert!(runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("context-retry")
            .unwrap()
            .is_none_or(|execution| execution.attempts.values().all(|attempt| {
                attempt.source_message_id.as_deref() != Some(accepted.id.as_str())
            })));
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
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
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

#[cfg(test)]
mod resource_read_tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        host::RuntimeHost,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
            tests::support::{context_config, StubProvider},
        },
    };

    fn resource_phase(
        identity: &AgentIdentityView,
        bytes: u64,
        records: u64,
    ) -> Result<(InputEvidenceBudgetLedger, ResourceReadMaterial)> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits {
            history_per_agent_bytes: bytes - 1,
            pending_per_agent_bytes: 1,
            record_limit: records,
            ..Default::default()
        })?;
        let material = ResourceReadMaterial::new(
            &ledger,
            &InputEvidenceScope {
                agent_id: identity.agent_id.clone(),
                identity_incarnation: identity.incarnation,
            },
        )?;
        Ok((ledger, material))
    }

    fn assert_resource_quota(error: &anyhow::Error) {
        let descriptor = crate::runtime_error::describe_runtime_error(error);
        assert_eq!(
            descriptor.domain,
            crate::runtime_error::RuntimeErrorDomain::Io
        );
        assert_eq!(descriptor.code, "context_resource_exhausted", "{error:#}");
        assert!(descriptor.retryable);
    }

    async fn resource_host() -> Result<(tempfile::TempDir, RuntimeHost, RuntimeHandle)> {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.user_home_dir = None;
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
        let runtime = host.default_runtime().await?;
        Ok((home, host, runtime))
    }

    #[tokio::test]
    async fn actual_runtime_shared_registry_preserves_cache_on_cold_and_changed_scan_quota(
    ) -> Result<()> {
        let (_home, host, runtime) = resource_host().await?;
        let identity = runtime.agent_identity_view().await?;
        let state = runtime.inner.agent.lock().await.state.clone();
        let root = runtime.agent_home().join("skills");
        let skill = root.join("sample/SKILL.md");
        let retained = root.join("retained/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap())?;
        std::fs::create_dir_all(retained.parent().unwrap())?;
        std::fs::write(
            &retained,
            "---\nname: Retained\ndescription: unchanged\n---",
        )?;
        std::fs::File::create(&skill)?.set_len(128 * 1024)?;
        let registry = host.skills_registry();
        let original_catalog = registry.read().await.catalog();
        let original_roots = registry.read().await.roots().to_vec();

        // Exercise the actual host-bridge sync, not a separate registry helper.
        for (bytes, records) in [(4096, 16_384), (512 * 1024, 64), (512 * 1024, 16_384)] {
            let (ledger, phase) = resource_phase(&identity, bytes, records)?;
            let error = runtime
                .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
                .await
                .err()
                .expect("cold fingerprint/optional file quota must fail the Runtime read");
            assert_resource_quota(&error);
            assert_resource_quota(&phase.finish().unwrap_err());
            drop(phase);
            assert_eq!(registry.read().await.catalog(), original_catalog);
            assert_eq!(registry.read().await.roots(), original_roots);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }

        std::fs::write(&skill, "---\nname: Sample\ndescription: original\n---")?;
        let (ledger, phase) = resource_phase(&identity, 64 * 1024 * 1024, 65_536)?;
        let view = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await?;
        assert_eq!(view.discoverable_skills.len(), 2);
        drop((view, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        registry.write().await.record_watcher_started(&root);
        let original_catalog = registry.read().await.catalog();
        let original_roots = registry.read().await.roots().to_vec();

        for oversized in [false, true] {
            if oversized {
                // The reader normally skips this error; its quota latch must win.
                std::fs::write(&skill, vec![0xff; 128 * 1024])?;
            }
            let bytes = if oversized { 512 * 1024 } else { 4096 };
            let (ledger, phase) = resource_phase(&identity, bytes, 16_384)?;
            let error = runtime
                .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
                .await
                .err()
                .expect("fingerprint None or an optional omission must not install a candidate");
            assert_resource_quota(&error);
            assert_resource_quota(&phase.finish().unwrap_err());
            drop(phase);
            assert_eq!(registry.read().await.catalog(), original_catalog);
            assert_eq!(registry.read().await.roots(), original_roots);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }

        // A fresh normal scope sees the changed input, rather than a poisoned cache.
        std::fs::write(&skill, "---\nname: Sample\ndescription: recovered\n---")?;
        let (ledger, phase) = resource_phase(&identity, 64 * 1024 * 1024, 65_536)?;
        let view = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await?;
        assert_eq!(view.discoverable_skills.len(), 2);
        assert!(view
            .discoverable_skills
            .iter()
            .any(|entry| entry.name == "Sample" && entry.description == "recovered"));
        drop((view, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);

        // Ordinary invalid UTF-8 is still skipped, without retaining stale entries.
        std::fs::write(&skill, [0xff])?;
        let (ledger, phase) = resource_phase(&identity, 64 * 1024 * 1024, 65_536)?;
        let view = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await?;
        assert_eq!(view.discoverable_skills.len(), 1);
        assert_eq!(view.discoverable_skills[0].name, "Retained");
        assert!(registry.read().await.roots().iter().any(|registration| {
            registration.root_path == root
                && matches!(
                    registration.scan_status,
                    crate::types::SkillRootScanStatus::Scanned { .. }
                )
        }));
        drop((view, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[tokio::test]
    async fn actual_runtime_shared_registry_two_root_sync_is_atomic_on_late_quota() -> Result<()> {
        let (_home, host, runtime) = resource_host().await?;
        let identity = runtime.agent_identity_view().await?;
        let state = runtime.inner.agent.lock().await.state.clone();
        let roots = crate::skills::base::SKILL_ROOT_SUFFIXES
            .iter()
            .take(2)
            .map(|suffix| runtime.agent_home().join(suffix))
            .collect::<Vec<_>>();
        let write = |root: &Path, name: &str, description: &str| -> Result<()> {
            let path = root.join("sample/SKILL.md");
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(
                path,
                format!("---\nname: {name}\ndescription: {description}\n---"),
            )?;
            Ok(())
        };
        write(&roots[0], "First", "original")?;
        write(&roots[1], "Second", "original")?;
        runtime
            .skills_runtime_view_for_state(&state, &identity)
            .await?;
        let registry = host.skills_registry();
        // The test-only snapshot equality includes the private fingerprint map.
        let before = registry.read().await.clone();
        assert_eq!(before.roots().len(), 2);
        write(&roots[0], "First", "valid changed first root")?;
        std::fs::File::create(roots[1].join("sample/SKILL.md"))?.set_len(2 * 1024 * 1024)?;

        // Verify the first changed root fits on its own, without using this copy
        // in the production path or charging an entire registry clone.
        let first = before.roots()[0].clone();
        let mut first_only = before.clone();
        let (ledger, phase) = resource_phase(&identity, 1024 * 1024, 65_536)?;
        {
            let _entry = phase.enter();
            first_only.sync_effective_roots([first])?;
        }
        phase.finish()?;
        assert!(first_only.catalog().iter().any(|entry| {
            entry.name == "First" && entry.description == "valid changed first root"
        }));
        drop((first_only, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);

        let (ledger, phase) = resource_phase(&identity, 1024 * 1024, 65_536)?;
        let error = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await
            .err()
            .expect("the second effective root must exhaust this material");
        assert_resource_quota(&error);
        assert_resource_quota(&phase.finish().unwrap_err());
        drop(phase);
        assert_eq!(
            *registry.read().await,
            before,
            "roots, catalog and fingerprints"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);

        write(&roots[1], "Second", "recovered second root")?;
        let view = runtime
            .skills_runtime_view_for_state(&state, &identity)
            .await?;
        assert!(view.discoverable_skills.iter().any(|entry| {
            entry.name == "First" && entry.description == "valid changed first root"
        }));
        assert!(view.discoverable_skills.iter().any(|entry| {
            entry.name == "Second" && entry.description == "recovered second root"
        }));
        Ok(())
    }

    #[tokio::test]
    async fn actual_runtime_shared_registry_preserves_warm_catalog_on_clone_quota() -> Result<()> {
        let (_home, host, runtime) = resource_host().await?;
        let identity = runtime.agent_identity_view().await?;
        let state = runtime.inner.agent.lock().await.state.clone();
        let skill = runtime.agent_home().join("skills/sample/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap())?;
        std::fs::write(
            &skill,
            format!(
                "---\nname: Sample\ndescription: {}\n---",
                "x".repeat(128 * 1024)
            ),
        )?;
        let (ledger, phase) = resource_phase(&identity, 64 * 1024 * 1024, 65_536)?;
        let original = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await?
            .discoverable_skills;
        assert_eq!(original.len(), 1);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let registry = host.skills_registry();
        let original_roots = registry.read().await.roots().to_vec();
        let (ledger, phase) = resource_phase(&identity, 512 * 1024, 16_384)?;
        let error = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await
            .err()
            .expect("unchanged cached entries must be admitted before cloning");
        assert_resource_quota(&error);
        assert_resource_quota(&phase.finish().unwrap_err());
        drop(phase);
        assert_eq!(registry.read().await.catalog(), *original);
        assert_eq!(registry.read().await.roots(), original_roots);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let (ledger, phase) = resource_phase(&identity, 64 * 1024 * 1024, 65_536)?;
        let recovered = runtime
            .skills_runtime_view_for_state_with_material(&state, &identity, Some(&phase))
            .await?;
        assert_eq!(recovered.discoverable_skills, original);
        drop((recovered, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[tokio::test]
    async fn loaded_context_trigger_copy_keeps_original_phase_through_prompt_consumption(
    ) -> Result<()> {
        let (_home, host, runtime) = resource_host().await?;
        super::tests::publish_prompt_history(&runtime, 1).await;
        let capability = runtime
            .default_external_trigger(CallbackDeliveryMode::WakeHint)
            .await?;
        let selected = runtime.latest_external_triggers().await?;
        assert!(selected.iter().any(|record| record.external_trigger_id
            == capability.external_trigger_id
            && record.target_agent_id == capability.target_agent_id));
        drop(selected);
        let ledger = runtime.inner.input_evidence_budget.clone();
        let prompt = runtime
            .preview_prompt(
                "TRIGGER_COPY_ORIGINAL_PHASE".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(prompt
            .rendered_context_attachment
            .contains("TRIGGER_COPY_ORIGINAL_PHASE"));
        let material = prompt._resource_read_material.as_ref().unwrap();
        let original = Arc::downgrade(material.allocation.as_ref().unwrap());
        let observation = Arc::downgrade(&material.observation);
        let before_clone = ledger.report()?.observed_bytes;
        let clone = prompt.clone();
        assert!(ledger.report()?.observed_bytes > before_clone);
        let copied = Arc::downgrade(
            clone
                ._resource_read_material
                .as_ref()
                .unwrap()
                .allocation
                .as_ref()
                .unwrap(),
        );
        host.shutdown().await?;
        drop((runtime, host));
        let with_both = ledger.report()?.observed_bytes;
        drop(prompt);
        assert!(original.upgrade().is_some());
        assert!(copied.upgrade().is_some());
        assert!(observation.upgrade().is_some());
        // Shared history still owns the original acquisition as well as the copy.
        assert_eq!(ledger.report()?.observed_bytes, with_both);
        assert!(clone
            .rendered_context_attachment
            .contains("TRIGGER_COPY_ORIGINAL_PHASE"));
        let with_clone = ledger.report()?.observed_bytes;
        drop(clone);
        assert!(original.upgrade().is_none());
        assert!(copied.upgrade().is_none());
        assert!(observation.upgrade().is_none());
        assert!(ledger.report()?.observed_bytes < with_clone);
        Ok(())
    }

    #[tokio::test]
    async fn current_input_loaded_prompt_retains_writer_material_through_final_clone() -> Result<()>
    {
        let (_home, host, runtime) = resource_host().await?;
        super::tests::publish_prompt_history(&runtime, 1).await;
        let ledger = runtime.inner.input_evidence_budget.clone();
        let prompt = runtime
            .preview_prompt(
                "CURRENT_INPUT_LAST_HOLDER".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        assert!(prompt
            .rendered_context_attachment
            .contains("CURRENT_INPUT_LAST_HOLDER"));
        assert!(prompt
            .rendered_context_attachment
            .contains("history fixture input 0"));
        assert!(prompt
            .rendered_context_attachment
            .contains("scheduler-window-sentinel-0"));
        let material = prompt._resource_read_material.as_ref().unwrap();
        let original = Arc::downgrade(material.allocation.as_ref().unwrap());
        let observation = Arc::downgrade(&material.observation);
        let before_clone = ledger.report()?.observed_bytes;
        let clone = prompt.clone();
        assert!(ledger.report()?.observed_bytes > before_clone);
        let copied = Arc::downgrade(
            clone
                ._resource_read_material
                .as_ref()
                .unwrap()
                .allocation
                .as_ref()
                .unwrap(),
        );
        host.shutdown().await?;
        drop((runtime, host));
        let with_both = ledger.report()?.observed_bytes;
        drop(prompt);
        assert!(original.upgrade().is_some());
        assert!(copied.upgrade().is_some());
        assert!(observation.upgrade().is_some());
        // Shared history still owns the original acquisition as well as the copy.
        assert_eq!(ledger.report()?.observed_bytes, with_both);
        assert!(clone
            .rendered_context_attachment
            .contains("CURRENT_INPUT_LAST_HOLDER"));
        assert!(clone
            .rendered_context_attachment
            .contains("history fixture input 0"));
        let with_clone = ledger.report()?.observed_bytes;
        drop(clone);
        assert!(original.upgrade().is_none());
        assert!(copied.upgrade().is_none());
        assert!(observation.upgrade().is_none());
        assert!(ledger.report()?.observed_bytes < with_clone);
        Ok(())
    }

    #[tokio::test]
    async fn completion_loaded_prompt_keeps_original_material_until_final_clone() -> Result<()> {
        let (_home, host, runtime) = resource_host().await?;
        let work = runtime
            .create_work_item(
                "completion consumer".into(),
                Some(crate::types::WorkItemPlanStatus::NeedsInput),
                None,
                Vec::new(),
            )
            .await?;
        let completed = runtime
            .complete_work_item_with_report(
                work.id.clone(),
                WorkItemCompletionAuthority::Control,
                "ORIGINAL_COMPLETION_BODY".into(),
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Vec::new(),
            )
            .await?
            .into_record();
        assert_eq!(completed.state, WorkItemState::Completed);
        runtime.inner.storage.append_delivery_summary(
            &crate::types::DeliverySummaryRecord::new(
                runtime.agent_id().await?,
                &work.id,
                "LEGACY_MUST_NOT_REPLACE_RESULT",
                None,
                None,
            ),
        )?;
        let ledger = runtime.inner.input_evidence_budget.clone();
        let prompt = runtime
            .preview_prompt(
                "read completion".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        assert!(prompt
            .rendered_context_attachment
            .contains("ORIGINAL_COMPLETION_BODY"));
        assert!(!prompt
            .rendered_context_attachment
            .contains("LEGACY_MUST_NOT_REPLACE_RESULT"));
        let material = prompt._resource_read_material.as_ref().unwrap();
        let original = Arc::downgrade(material.allocation.as_ref().unwrap());
        let observation = Arc::downgrade(&material.observation);
        let clone = prompt.clone();
        let copied = Arc::downgrade(
            clone
                ._resource_read_material
                .as_ref()
                .unwrap()
                .allocation
                .as_ref()
                .unwrap(),
        );
        host.shutdown().await?;
        drop((runtime, host));
        let with_both = ledger.report()?.observed_bytes;
        drop(prompt);
        assert!(original.upgrade().is_none());
        assert!(copied.upgrade().is_some());
        assert!(observation.upgrade().is_some());
        assert!(ledger.report()?.observed_bytes < with_both);
        assert!(clone
            .rendered_context_attachment
            .contains("ORIGINAL_COMPLETION_BODY"));
        let with_clone = ledger.report()?.observed_bytes;
        drop(clone);
        assert!(copied.upgrade().is_none());
        assert!(observation.upgrade().is_none());
        assert!(ledger.report()?.observed_bytes < with_clone);
        Ok(())
    }

    #[tokio::test]
    async fn loaded_context_keeps_one_resource_guard_through_prompt_clones() -> Result<()> {
        let home = tempfile::tempdir()?;
        let runtime = RuntimeHandle::new(
            "resource-prompt",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "resource-prompt".into(),
            context_config(),
        )?;
        // Fixture resources are local, independent of the machine's skill catalog.
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
        std::fs::write(home.path().join("AGENTS.md"), "original bounded guidance")?;
        std::fs::create_dir_all(home.path().join("skills/sample"))?;
        std::fs::write(
            home.path().join("skills/sample/SKILL.md"),
            "---\nname: Sample\ndescription: bounded catalog\n---\nbody",
        )?;
        let prompt = runtime
            .preview_prompt("inspect".into(), AuthorityClass::OperatorInstruction)
            .await?;
        assert_eq!(
            prompt
                .loaded_agents_md
                .agent_source
                .as_ref()
                .unwrap()
                .content,
            "original bounded guidance"
        );
        let weak = Arc::downgrade(
            prompt
                ._resource_read_material
                .as_ref()
                .expect("actual loaded resource material")
                .allocation
                .as_ref()
                .unwrap(),
        );
        let clone = prompt.clone();
        let clone_weak = Arc::downgrade(
            clone
                ._resource_read_material
                .as_ref()
                .unwrap()
                .allocation
                .as_ref()
                .unwrap(),
        );
        drop(prompt);
        assert!(
            weak.upgrade().is_none(),
            "each owned prompt copy has its own admitted material"
        );
        assert!(clone_weak.upgrade().is_some());
        assert!(clone
            .rendered_system_prompt
            .contains("original bounded guidance"));
        drop(clone);
        assert!(
            clone_weak.upgrade().is_none(),
            "the final prompt releases its original file material"
        );
        Ok(())
    }

    #[tokio::test]
    async fn actual_context_capture_preserves_typed_quota_errors_in_each_read_phase() -> Result<()>
    {
        let home = tempfile::tempdir()?;
        let runtime = RuntimeHandle::new(
            "resource-quota",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "resource-quota".into(),
            context_config(),
        )?;
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
        for relative in [
            "skills/sample/SKILL.md",
            "notes/oversized.md",
            "AGENTS.md",
            "memory/self.md",
        ] {
            let path = home.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap())?;
            // Sparse length admission must reject before allocating the body.
            std::fs::File::create(&path)?.set_len(64 * 1024 * 1024 + 1)?;
            // External file fixtures require the explicit refresh boundary now.
            assert!(runtime.refresh_context_resources().await.is_err());
            let error = runtime
                .preview_prompt("inspect".into(), AuthorityClass::OperatorInstruction)
                .await
                .err()
                .expect("resource quota must reach the actual capture caller");
            let descriptor = crate::runtime_error::describe_runtime_error(&error);
            assert_eq!(
                descriptor.domain,
                crate::runtime_error::RuntimeErrorDomain::Io,
                "{relative}: {error:#}"
            );
            assert_eq!(descriptor.code, "context_resource_exhausted", "{relative}");
            assert!(descriptor.retryable, "{relative}");
            std::fs::remove_file(path)?;
        }
        // A genuine non-quota reader error must not be rewritten as exhaustion.
        std::fs::write(home.path().join("AGENTS.md"), [0xff])?;
        assert!(runtime.refresh_context_resources().await.is_err());
        let error = runtime
            .preview_prompt("inspect".into(), AuthorityClass::OperatorInstruction)
            .await
            .err()
            .expect("invalid guidance must remain a reader error");
        assert_ne!(
            crate::runtime_error::describe_runtime_error(&error).code,
            "context_resource_exhausted"
        );
        std::fs::remove_file(home.path().join("AGENTS.md"))?;
        runtime.refresh_context_resources().await?;
        let work = runtime
            .create_work_item(
                "resource plan".into(),
                Some(crate::types::WorkItemPlanStatus::NeedsInput),
                None,
                Vec::new(),
            )
            .await?;
        runtime.pick_work_item(work.id.clone()).await?;
        runtime.refresh_context_resources().await?;
        let plan = crate::work_item_plan::plan_path(runtime.inner.storage.data_dir(), &work.id);
        std::fs::File::create(plan)?.set_len(64 * 1024 * 1024 + 1)?;
        let error = runtime
            .refresh_context_resources()
            .await
            .err()
            .expect("explicit plan observation must preserve a quota failure");
        assert_resource_quota(&error);
        let error = runtime
            .preview_prompt("inspect".into(), AuthorityClass::OperatorInstruction)
            .await
            .err()
            .expect("loaded plan capture must not hide the observed quota failure");
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(
            descriptor.domain,
            crate::runtime_error::RuntimeErrorDomain::Io,
            "{error:#}"
        );
        assert_eq!(descriptor.code, "context_resource_exhausted");
        assert!(descriptor.retryable);
        Ok(())
    }
}
