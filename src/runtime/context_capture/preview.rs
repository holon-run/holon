use super::super::{
    mutation_owner::MutationOwnerError,
    resident_input_evidence::InputEvidenceReadError,
    resident_scheduling::SchedulingReadError,
    resident_task::TaskReadError,
    resident_work::{WorkCompletionDeliveries, WorkReadError},
    resource_observation::ResourceFreshness,
};
use super::*;
use crate::diagnostics::io_probe::ReadyContextOutcome;
use crate::types::{
    AgentCapabilityFamily, AgentCapabilityPolicyRecord, AgentIdentityRecord, AgentPolicyEffect,
};

pub(in crate::runtime) struct ContextToolPolicy {
    identity_revision: u64,
    generation: u64,
    tools: ContextToolFamilies,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) struct ContextToolFamilies(u8);

impl ContextToolFamilies {
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
            .measure_ready(
                Stage::PreviewReadyCapture,
                Box::pin(self.capture_ready_prompt_preview(message)),
                ready_capture_outcome,
            )
            .await
        {
            Ok(prepared) => return self.project_loaded_context_prompt(prepared, None).await,
            Err(error) if ready_context_requires_preparation(&error) => {
                ContextPreparationRetries::after(&error)
            }
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

    pub(super) async fn capture_ready_turn_context(
        &self,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        fallback_model: Option<&ModelRouteRef>,
        admission: Option<ContextAdmissionPlan<'_>>,
        expected: Option<&Arc<resident_state::AgentSnapshot>>,
    ) -> Result<PreparedLoadedContext> {
        let db = &self.inner.runtime_db;
        db.check_read_scope()?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .ok_or(ContextNeedsPreparation::Publication)?;
        if expected.is_some_and(|planned| !Arc::ptr_eq(planned, &root)) {
            return Err(AdmissionContextChanged.into());
        }
        let identity = root
            .identity
            .as_deref()
            .ok_or(ContextNeedsPreparation::Identity)?;
        anyhow::ensure!(
            identity.agent_id == message.agent_id
                && identity.agent_id == root.state.id
                && identity.incarnation == self.inner.identity_incarnation
                && identity.incarnation == root.token.identity_incarnation
                && identity.status == crate::types::AgentRegistryStatus::Active,
            "Context identity is outside the active publication scope"
        );
        if self.inner.resources.registry.freshness()? != ResourceFreshness::Ready {
            return Err(ContextNeedsPreparation::Resources.into());
        }
        let (mode, tools) = if admission.is_some() {
            (ContextCaptureMode::Admission, None)
        } else {
            (
                ContextCaptureMode::Execution,
                Some(
                    self.ready_context_tool_policy(identity)
                        .ok_or(ContextNeedsPreparation::ToolPolicy)?,
                ),
            )
        };
        let prepared = self
            .capture_loaded_context(
                ContextPublication::Ready(&root),
                mode,
                message,
                continuation,
                fallback_model,
                admission.is_none(),
                admission,
            )
            .await?;
        let current = self
            .inner
            .mutation_owner
            .capture()?
            .ok_or(ContextNeedsPreparation::Publication)?;
        if !Arc::ptr_eq(&current, &root) {
            return if expected.is_some() {
                Err(AdmissionContextChanged.into())
            } else {
                Err(ContextDependencyChanged.into())
            };
        }
        if tools.is_some_and(|tools| self.ready_context_tool_policy(identity) != Some(tools)) {
            return Err(ContextNeedsPreparation::ToolPolicy.into());
        }
        db.check_read_scope()?;
        Ok(prepared)
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
            .ok_or(ContextNeedsPreparation::Publication)?;
        if !self.preview_scheduler_has_facts(&root)? {
            return Err(ContextNeedsPreparation::SchedulerCoverage.into());
        }
        let identity = root
            .identity
            .as_deref()
            .ok_or(ContextNeedsPreparation::Identity)?;
        let tool_policy = self
            .ready_context_tool_policy(identity)
            .ok_or(ContextNeedsPreparation::ToolPolicy)?;
        if self.inner.resources.registry.freshness()? != ResourceFreshness::Ready {
            return Err(ContextNeedsPreparation::Resources.into());
        }
        let _admission = if root.state.status != AgentStatus::Stopped {
            let scheduling = root
                .scheduling
                .as_ref()
                .ok_or(ContextNeedsPreparation::SchedulerCoverage)?;
            anyhow::ensure!(
                scheduling.scope() == &self.inner.resources.scope,
                "preview Trigger scope differs"
            );
            if !scheduling
                .active_default_trigger()?
                .is_some_and(|trigger| trigger.token.is_some())
            {
                return Err(ContextNeedsPreparation::SchedulerCoverage.into());
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
            .ok_or(ContextNeedsPreparation::Publication)?;
        if !Arc::ptr_eq(&current, &root) {
            return Err(ContextNeedsPreparation::Publication.into());
        }
        if !self.preview_scheduler_has_facts(&root)? {
            return Err(ContextNeedsPreparation::SchedulerCoverage.into());
        }
        if self.ready_context_tool_policy(identity) != Some(tool_policy) {
            return Err(ContextNeedsPreparation::ToolPolicy.into());
        }
        Ok(prepared)
    }

    pub(super) fn ready_completion_deliveries(
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
            .ok_or(ContextNeedsPreparation::CompletionDelivery)?;
        Ok(Some(covered.clone()))
    }

    pub(super) fn ready_context_tool_policy(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Option<ContextToolFamilies> {
        if self.inner.host_bridge.is_none() {
            return Some(ContextToolFamilies(u8::MAX));
        }
        let published = self.inner.context_tool_policy.load();
        let policy = published.as_ref()?;
        (policy.identity_revision == identity.revision
            && Some(policy.generation) == self.inner.runtime_db.capability_policy_generation())
        .then_some(policy.tools)
    }

    pub(super) fn prepare_context_tool_policy(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<ContextToolFamilies> {
        if let Some(tools) = self.ready_context_tool_policy(identity) {
            return Ok(tools);
        }
        let db = &self.inner.runtime_db;
        let generation = db.capability_policy_generation();
        let policy = db
            .agent_canonical_relations()
            .prepare_capability_policy(identity)?;
        let tools = ContextToolFamilies::from_policy(policy);
        if let Some(generation) =
            generation.filter(|generation| Some(*generation) == db.capability_policy_generation())
        {
            self.inner
                .context_tool_policy
                .store(Some(Arc::new(ContextToolPolicy {
                    identity_revision: identity.revision,
                    generation,
                    tools,
                })));
        }
        Ok(tools)
    }
}

pub(super) fn ready_capture_outcome<T>(result: &Result<T>) -> ReadyContextOutcome {
    use super::super::resource_observation::ResourceObservationError;
    use ReadyContextOutcome as Outcome;

    let Err(error) = result else {
        return Outcome::Ready;
    };
    if let Some(need) = error.downcast_ref::<ContextNeedsPreparation>() {
        return match need {
            ContextNeedsPreparation::Publication => Outcome::Publication,
            ContextNeedsPreparation::Identity => Outcome::Identity,
            ContextNeedsPreparation::Resources => Outcome::ResourcesNotReady,
            ContextNeedsPreparation::ToolPolicy => Outcome::ToolPolicy,
            ContextNeedsPreparation::SchedulerCoverage => Outcome::SchedulerCoverage,
            ContextNeedsPreparation::Episodes => Outcome::Episodes,
            ContextNeedsPreparation::CompletionDelivery => Outcome::CompletionDelivery,
        };
    }
    if error.is::<AdmissionContextChanged>() {
        return Outcome::Publication;
    }
    if !ready_context_requires_preparation(error) {
        return Outcome::Failed;
    }
    if error.is::<MutationOwnerError>() {
        return Outcome::PendingRecovery;
    }
    if error.is::<crate::host::context_resources::ContextResourceWritersActive>() {
        return Outcome::ResourceWriter;
    }
    if let Some(resource) = error.downcast_ref::<ResourceObservationError>() {
        return match resource {
            ResourceObservationError::SourceChanged => Outcome::ResourceSourceChanged,
            ResourceObservationError::NotReady(_) => Outcome::ResourcesNotReady,
            _ => Outcome::DependenciesChanged,
        };
    }
    if error.is::<InputEvidenceReadError>() || error.is::<InputEvidenceSnapshotError>() {
        return Outcome::Input;
    }
    if error.is::<WorkReadError>() {
        return Outcome::Work;
    }
    if error.is::<TaskReadError>() {
        return Outcome::Task;
    }
    if error.is::<SchedulingReadError>() {
        return Outcome::Scheduling;
    }
    Outcome::DependenciesChanged
}

pub(super) fn ready_context_requires_preparation(error: &anyhow::Error) -> bool {
    error.is::<ContextNeedsPreparation>()
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
