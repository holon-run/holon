use super::super::{
    mutation_owner::MutationOwnerError,
    resident_input_evidence::InputEvidenceReadError,
    resident_scheduling::SchedulingReadError,
    resident_task::TaskReadError,
    resident_work::{WorkCompletionDeliveries, WorkReadError},
    resource_observation::ResourceFreshness,
};
use super::*;
use crate::types::{
    AgentCapabilityFamily, AgentCapabilityPolicyRecord, AgentIdentityRecord, AgentPolicyEffect,
};

pub(in crate::runtime) struct PreviewToolPolicy {
    identity_revision: u64,
    generation: u64,
    tools: PreviewToolFamilies,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) struct PreviewToolFamilies(u8);

impl PreviewToolFamilies {
    fn bit(family: AgentCapabilityFamily) -> u8 {
        1 << match family {
            AgentCapabilityFamily::CoreAgent => 0,
            AgentCapabilityFamily::LocalEnvironment => 1,
            AgentCapabilityFamily::Web => 2,
            AgentCapabilityFamily::AgentCreation => 3,
            AgentCapabilityFamily::AuthorityExpanding => 4,
            AgentCapabilityFamily::ExternalTrigger => 5,
        }
    }

    pub(in crate::runtime) fn allows(self, family: AgentCapabilityFamily) -> bool {
        self.0 & Self::bit(family) != 0
    }

    fn from_policy(policy: Option<AgentCapabilityPolicyRecord>) -> Self {
        let Some(policy) = policy else {
            return Self(u8::MAX);
        };
        let mut allowed = 0;
        for rule in policy.rules {
            let bit = Self::bit(rule.family);
            match rule.effect {
                AgentPolicyEffect::Allow => allowed |= bit,
                AgentPolicyEffect::Deny => allowed &= !bit,
            }
        }
        Self(allowed)
    }
}

impl RuntimeHandle {
    pub(in crate::runtime) async fn prepare_prompt_preview(
        &self,
        message: &MessageEnvelope,
    ) -> Result<EffectivePrompt> {
        let retries = match ContextCaptureMode::Preview
            .measure(
                Stage::PreviewReadyCapture,
                Box::pin(self.capture_ready_prompt_preview(message)),
            )
            .await
        {
            Ok(prepared) => return self.project_loaded_context_prompt(prepared, None).await,
            Err(error) if preview_requires_preparation(&error) => ContextPreparationRetries {
                writers: usize::from(
                    error.is::<crate::host::context_resources::ContextResourceWritersActive>(),
                ),
                sources: usize::from(
                    super::super::resource_refresh::is_resource_preparation_conflict(&error),
                ),
            },
            Err(error) => return Err(error),
        };
        let prior = ContextCaptureMode::Preview
            .measure(
                Stage::PreviewSchedulerPreflight,
                self.preview_closure_decision(),
            )
            .await?;
        let continuation = ContinuationTrigger::from_message(message, None)
            .map(|trigger| resolve_continuation(&prior, &trigger));
        if self.agent_state().await?.status != AgentStatus::Stopped {
            self.ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
                .await?;
        }
        let prepared = self
            .prepare_loaded_context_snapshot(
                message,
                continuation.as_ref(),
                None,
                false,
                None,
                retries,
            )
            .await?;
        self.project_loaded_context_prompt(prepared, None).await
    }

    async fn capture_ready_prompt_preview(
        &self,
        message: &MessageEnvelope,
    ) -> Result<PreparedLoadedContext> {
        self.inner.runtime_db.check_read_scope()?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .ok_or(PreviewNeedsPreparation)?;
        if !self.preview_scheduler_has_facts(&root)? {
            return Err(PreviewNeedsPreparation.into());
        }
        let identity = root.identity.as_deref().ok_or(PreviewNeedsPreparation)?;
        let tool_policy = self
            .ready_preview_tool_policy(identity)
            .ok_or(PreviewNeedsPreparation)?;
        if self.inner.resources.registry.freshness()? != ResourceFreshness::Ready {
            return Err(PreviewNeedsPreparation.into());
        }
        let _admission = if root.state.status != AgentStatus::Stopped {
            let scheduling = root.scheduling.as_ref().ok_or(PreviewNeedsPreparation)?;
            anyhow::ensure!(
                scheduling.scope() == &self.inner.resources.scope,
                "preview Trigger scope differs"
            );
            if !scheduling
                .active_default_trigger()?
                .is_some_and(|trigger| trigger.token.is_some())
            {
                return Err(PreviewNeedsPreparation.into());
            }
            Some(self.execution_admission_lease()?)
        } else {
            None
        };
        let prior = ContextCaptureMode::Preview
            .measure_sync(Stage::PreviewSchedulerPreflight, || {
                self.preview_closure_from_root(&root)
            })?;
        let continuation = ContinuationTrigger::from_message(message, None)
            .map(|trigger| resolve_continuation(&prior, &trigger));
        let prepared = self
            .capture_loaded_context(
                ContextPublication::Ready(&root),
                ContextCaptureMode::Preview,
                message,
                continuation.as_ref(),
                None,
                false,
                None,
            )
            .await?;
        let current = self
            .inner
            .mutation_owner
            .capture()?
            .ok_or(PreviewNeedsPreparation)?;
        if !Arc::ptr_eq(&current, &root)
            || !self.preview_scheduler_has_facts(&root)?
            || self.ready_preview_tool_policy(identity) != Some(tool_policy)
        {
            return Err(PreviewNeedsPreparation.into());
        }
        Ok(prepared)
    }

    pub(super) fn preview_completion_deliveries(
        &self,
        root: &resident_state::AgentSnapshot,
        work: &CapturedWork,
        work_ids: &BTreeSet<String>,
    ) -> Result<Option<Arc<WorkCompletionDeliveries>>> {
        let mut needed = work_ids
            .iter()
            .filter(|id| matches!(work.latest_results.get(*id), Some(None)))
            .peekable();
        if needed.peek().is_none() {
            return Ok(None);
        }
        let generation = self
            .inner
            .runtime_db
            .evidence()
            .delivery_summary_generation();
        let covered = root
            .work
            .as_ref()
            .and_then(|work| work.completion_deliveries.as_ref())
            .filter(|view| {
                view.generation == generation && needed.all(|id| view.summaries.contains_key(id))
            })
            .ok_or(PreviewNeedsPreparation)?;
        Ok(Some(covered.clone()))
    }

    pub(super) fn ready_preview_tool_policy(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Option<PreviewToolFamilies> {
        if self.inner.host_bridge.is_none() {
            return Some(PreviewToolFamilies(u8::MAX));
        }
        let published = self.inner.preview_tool_policy.load();
        let policy = published.as_ref()?;
        (policy.identity_revision == identity.revision
            && Some(policy.generation) == self.inner.runtime_db.capability_policy_generation())
        .then_some(policy.tools)
    }

    pub(super) fn prepare_preview_tool_policy(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<PreviewToolFamilies> {
        if let Some(tools) = self.ready_preview_tool_policy(identity) {
            return Ok(tools);
        }
        let db = &self.inner.runtime_db;
        let generation = db.capability_policy_generation();
        let policy = db
            .agent_canonical_relations()
            .prepare_capability_policy(identity)?;
        let tools = PreviewToolFamilies::from_policy(policy);
        if let Some(generation) =
            generation.filter(|generation| Some(*generation) == db.capability_policy_generation())
        {
            self.inner
                .preview_tool_policy
                .store(Some(Arc::new(PreviewToolPolicy {
                    identity_revision: identity.revision,
                    generation,
                    tools,
                })));
        }
        Ok(tools)
    }
}

fn preview_requires_preparation(error: &anyhow::Error) -> bool {
    error.is::<PreviewNeedsPreparation>()
        || error.is::<ContextDependencyChanged>()
        || error.is::<ContextConfigurationChanged>()
        || error.is::<InputEvidenceReadError>()
        || super::super::resource_refresh::is_resource_preparation_conflict(error)
        || error.is::<crate::host::context_resources::ContextResourceWritersActive>()
        || matches!(
            error.downcast_ref::<MutationOwnerError>(),
            Some(MutationOwnerError::NeedsReconcile(_))
        )
        || matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::NeedsPreparation(_))
        )
        || matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(
                WorkReadError::IncompleteLive
                    | WorkReadError::WindowExpansion { .. }
                    | WorkReadError::NeedsPreparation(_)
                    | WorkReadError::TurnWaitsNotPrepared(_)
                    | WorkReadError::TaskWaitsNotPrepared(_)
                    | WorkReadError::TriggerWaitsNotPrepared(_)
            )
        )
        || matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(
                TaskReadError::IncompleteActive
                    | TaskReadError::IncompleteResults
                    | TaskReadError::NeedsPreparation(_)
                    | TaskReadError::WindowExpansion
            )
        )
        || matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(
                SchedulingReadError::IncompleteLive
                    | SchedulingReadError::NeedsHistory
                    | SchedulingReadError::NeedsPreparation(_)
            )
        )
}
