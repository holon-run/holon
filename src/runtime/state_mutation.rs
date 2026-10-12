//! Serialized private candidates and exact post-commit runtime installation.

use super::{
    mutation_owner::{IdentityMutationFacts, KnownCommit, MutationReservation, PublicationToken},
    *,
};
use crate::runtime_db::{persistence_certainty, PersistenceCertainty};

#[path = "state_mutation/prepared_evidence.rs"]
mod prepared_evidence;

#[derive(Debug, thiserror::Error)]
#[error("prepared input operation already has its original receipt")]
struct PreparedInputReplay(crate::runtime_db::input_mutation_receipts::InputMutationReceipt);

#[derive(Debug, thiserror::Error)]
#[error("agent mutation identity incarnation is no longer current: agent {agent_id}, expected incarnation {expected_incarnation}, actual incarnation {actual_incarnation}, status {status:?}")]
pub(super) struct RuntimeMutationIdentityRetired {
    agent_id: String,
    expected_incarnation: u64,
    actual_incarnation: u64,
    status: crate::types::AgentRegistryStatus,
}

#[derive(Default)]
pub(super) struct OwnedMutationTasks {
    closed: bool,
    drain: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl OwnedMutationTasks {
    pub(super) fn all_finished(&self) -> bool {
        self.handles.iter().all(|task| task.is_finished())
            && self
                .drain
                .try_lock()
                .is_ok_and(|tasks| tasks.iter().all(|task| task.is_finished()))
    }
}

#[derive(Debug, Clone)]
struct DeferredRunAbort {
    handle: CurrentRunAbortHandle,
    reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedControlCancellation {
    pub(super) key: crate::runtime_db::control_transition::ControlKey,
    pub(super) digest: String,
    pub(super) owner_epoch: u64,
    pub(super) facts: Arc<super::control::PreparedControlCancellation>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeMutationFacts {
    owner_epoch: u64,
    pub(crate) state: Arc<AgentState>,
    episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
    input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    queue: RuntimeQueue,
    _queue_budget_view: Option<Arc<super::input_evidence_budget::BudgetViewGuard>>,
    current_run_abort: Option<CurrentRunAbortHandle>,
    deferred_abort: Option<DeferredRunAbort>,
    control_cancellation: Option<SavedControlCancellation>,
    effects: crate::runtime_db::transitions::PostCommitEffects,
    recovered_cache:
        Option<Arc<crate::runtime_db::input_recovery_install::InputRecoveryInstallFacts>>,
}

impl RuntimeMutationFacts {
    pub(crate) fn for_cold_external_input(
        owner_epoch: u64,
        state: Arc<AgentState>,
        episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    ) -> Self {
        Self {
            owner_epoch,
            state,
            episodes,
            input_evidence,
            queue: RuntimeQueue::default(),
            _queue_budget_view: None,
            current_run_abort: None,
            deferred_abort: None,
            control_cancellation: None,
            effects: Default::default(),
            recovered_cache: None,
        }
    }

    pub(crate) fn with_external_input_commit(
        mut self,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
        effects: crate::runtime_db::transitions::PostCommitEffects,
    ) -> Result<Self> {
        if let Some(inputs) = &input_evidence {
            anyhow::ensure!(
                inputs.scope().agent_id == self.state.id,
                "external input commit changed its agent scope"
            );
        }
        self.input_evidence = input_evidence;
        self.recovered_cache = None;
        self.effects = effects;
        strip_input_evidence_payloads(&mut self.effects);
        Ok(self)
    }

    pub(crate) fn external_effects(&self) -> crate::runtime_db::transitions::PostCommitEffects {
        self.effects.clone()
    }
    pub(super) fn for_input_recovery(
        owner_epoch: u64,
        state: Arc<AgentState>,
        episodes: Arc<super::resident_state::ResidentEpisodes>,
        inputs: Arc<super::resident_input_evidence::ResidentInputEvidence>,
        messages: Vec<MessageEnvelope>,
        budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        original: Option<&Self>,
        outcome: &crate::runtime_db::input_mutation_receipts::InputMutationScopeOutcome,
        applied: bool,
        identity: Option<crate::types::AgentIdentityRecord>,
        cache: crate::runtime_db::input_recovery_install::InputRecoveryInstallFacts,
    ) -> Result<Self> {
        anyhow::ensure!(
            outcome.scope.agent_id == state.id
                && inputs.belongs_to(&outcome.scope)
                && cache.scope == outcome.scope
                && cache.keys == outcome.install_keys,
            "input recovery scope differs"
        );
        let mut queue = RuntimeQueue::default();
        for message in messages {
            anyhow::ensure!(message.agent_id == state.id, "input recovery queue differs");
            queue.push(message);
        }
        let queue_view = budget.register_queue_view(
            &outcome.scope,
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            queue.shared_messages(),
        )?;
        let current_run_abort = original
            .filter(|original| original.owner_epoch == owner_epoch)
            .and_then(|original| original.current_run_abort.clone())
            .filter(|handle| state.current_run_id.as_ref() == Some(&handle.run_id));
        Ok(Self {
            owner_epoch,
            state,
            episodes: Some(episodes),
            input_evidence: Some(inputs),
            queue,
            _queue_budget_view: Some(queue_view),
            current_run_abort,
            deferred_abort: applied
                .then(|| original.and_then(|original| original.deferred_abort.clone()))
                .flatten(),
            control_cancellation: original
                .and_then(|original| original.control_cancellation.clone()),
            recovered_cache: Some(Arc::new(cache)),
            effects: crate::runtime_db::transitions::PostCommitEffects {
                audit_events: outcome.audit_events.clone(),
                notify_memory_index: outcome.notify_memory_index,
                notify_scheduler: outcome.notify_scheduler,
                agent_identities: identity.into_iter().collect(),
                ..Default::default()
            },
        })
    }

    pub(super) fn suppress_observed_input_replay_publication(&mut self) {
        // Fresh current installation is still necessary. The new invocation
        // cannot reissue old notifications or signal a newly prepared abort.
        self.effects.audit_events.clear();
        self.effects.notify_memory_index = false;
        self.effects.notify_scheduler = false;
        self.effects.fault = None;
        self.deferred_abort = None;
        self.control_cancellation = None;
    }

    pub(super) fn for_control_recovery(
        owner_epoch: u64,
        state: Arc<AgentState>,
        episodes: Arc<super::resident_state::ResidentEpisodes>,
        facts: crate::runtime_db::control_transition::ControlRecoveryFacts,
        original_run: Option<CurrentRunAbortHandle>,
        cancellation: Option<SavedControlCancellation>,
    ) -> Result<Self> {
        anyhow::ensure!(
            facts
                .workspace_occupancy
                .as_ref()
                .is_none_or(|occupancy| occupancy.holder_agent_id == state.id),
            "Control recovery occupancy scope differs"
        );
        tracing::debug!(agent_id = %state.id, waits = facts.waits.len(),
            timer_wakes = facts.timer_wakes.len(),
            workspace_occupancy = facts.workspace_occupancy.is_some(),
            "installing coherent Control recovery facts");
        let mut queue = RuntimeQueue::default();
        for message in facts.current_queue_messages {
            anyhow::ensure!(
                message.agent_id == state.id,
                "Control recovery queue scope differs"
            );
            queue.push(message);
        }
        let current_run_abort =
            original_run.filter(|handle| state.current_run_id.as_ref() == Some(&handle.run_id));
        let mut effects = facts.transition.effects;
        strip_input_evidence_payloads(&mut effects);
        Ok(Self {
            owner_epoch,
            state,
            episodes: Some(episodes),
            input_evidence: None,
            queue,
            _queue_budget_view: None,
            current_run_abort,
            deferred_abort: None,
            control_cancellation: cancellation,
            effects,
            recovered_cache: None,
        })
    }
}

pub(super) struct RuntimeMutation {
    runtime: RuntimeHandle,
    reservations: Vec<MutationReservation>,
    primary_index: usize,
    metadata: Vec<Option<Arc<IdentityMutationFacts>>>,
    episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
    input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    candidate: Box<RuntimeAgent>,
    base: Box<RuntimeAgent>,
    deferred_abort: Option<DeferredRunAbort>,
    control_cancellation: Option<SavedControlCancellation>,
    measurement_started: Option<std::time::Instant>,
    _candidate_budget_views: Vec<Arc<super::input_evidence_budget::BudgetViewGuard>>,
}

impl std::ops::Deref for RuntimeMutation {
    type Target = RuntimeAgent;

    fn deref(&self) -> &Self::Target {
        &self.candidate
    }
}

impl std::ops::DerefMut for RuntimeMutation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.candidate
    }
}

impl RuntimeMutation {
    pub(super) fn input_evidence_scope(
        &self,
    ) -> super::resident_input_evidence::InputEvidenceScope {
        let reservation = &self.reservations[self.primary_index];
        super::resident_input_evidence::InputEvidenceScope {
            agent_id: reservation.agent_id().to_owned(),
            identity_incarnation: reservation.identity_incarnation(),
        }
    }

    /// Non-context planners derive their default policy once at this explicit
    /// boundary. Loaded context supplies its already frozen request directly.
    pub(super) async fn prepare_owner_inputs(
        &mut self,
        extra_mandatory_roots: std::collections::BTreeSet<
            super::resident_input_evidence::InputEvidenceKey,
        >,
        interrupted_sources: std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let version = self.runtime.inner.config_snapshot.load_full();
        let config = if version.provider_reconfig.is_some() {
            version.model_catalog.resolved_context_config(
                &version.base_context_config,
                self.candidate.state.model_override.as_ref(),
            )
        } else {
            version.base_context_config.clone()
        };
        let model = version
            .model_catalog
            .effective_model(self.candidate.state.model_override.as_ref());
        let history = super::turn::context_history_preparation(&self.candidate.state.id, &model);
        let (mut roots, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        roots.extend(extra_mandatory_roots);
        let mut request = super::context_capture::context_input_evidence_request(
            &self.candidate.state,
            None,
            &config,
            history,
            roots,
            record_limit,
            byte_limit,
        )?;
        request.interrupted_sources = interrupted_sources;
        self.prepare_owner_input_request(&request, config.recent_episode_candidates)
            .await
    }

    /// Same-reservation preparation preserves the caller's exact windows,
    /// budgets, recovery sources and Episode limit. It never reselects a model.
    #[inline(never)]
    pub(super) fn prepare_owner_input_request<'a>(
        &'a mut self,
        request: &'a crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        recent_episode_limit: usize,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_owner_input_request_inner(request, recent_episode_limit))
    }

    async fn prepare_owner_input_request_inner(
        &mut self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        recent_episode_limit: usize,
    ) -> Result<()> {
        use super::input_preparation::{
            prepare_reserved_input_domains, InputPreparation, InputPreparationCause,
        };
        use super::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        let root = self
            .runtime
            .inner
            .mutation_owner
            .capture()?
            .context("input preparation requires a published owner root")?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "input preparation cannot publish a private candidate"
        );
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        anyhow::ensure!(
            request.record_limit > 0
                && request.record_limit <= record_limit
                && request.byte_limit > 0
                && request.byte_limit <= byte_limit,
            "input preparation request exceeds the held owner's budget"
        );
        let mut request = request.clone();
        if let Some(inputs) = &self.input_evidence {
            request
                .mandatory_roots
                .extend(inputs.mandatory_roots().iter().cloned());
        }
        for message in self.candidate.queue.shared_messages() {
            request
                .mandatory_roots
                .insert(Key::new(Family::Message, &message.id));
        }
        if let Some(turn) = &self.candidate.state.current_turn_id {
            request.mandatory_roots.insert(Key::new(Family::Turn, turn));
        }
        if let Some(binding) = &self.candidate.state.current_execution_binding {
            request
                .mandatory_roots
                .insert(Key::new(Family::Message, &binding.source_message_id));
            request
                .mandatory_roots
                .insert(Key::new(Family::Turn, &binding.turn_id));
            if let Some(attempt) = &binding.activation_id {
                request
                    .mandatory_roots
                    .insert(Key::new(Family::Attempt, attempt));
            }
            if let Some(work) = &binding.work_item_id {
                request
                    .mandatory_roots
                    .insert(Key::new(Family::WorkExecution, work));
            }
        }
        // Owner is held; this chooses an explicit preparation policy only.
        // The preparation transaction revalidates exact scope and State.
        let scope = self.input_evidence_scope();
        let deleting = self
            .runtime
            .inner
            .runtime_db
            .agent_identities()
            .latest(&scope.agent_id)?
            .is_some_and(|identity| {
                identity.incarnation == scope.identity_incarnation
                    && identity.status == crate::types::AgentRegistryStatus::Deleting
            });
        let cause = if deleting {
            InputPreparationCause::ExistingAdmissionCleanup
        } else if self.input_evidence.is_none() {
            InputPreparationCause::ValidatedRecovery
        } else {
            InputPreparationCause::OwnerChange
        };
        let prepared = prepare_reserved_input_domains(
            &self.runtime.inner.runtime_db,
            &self.reservations[self.primary_index],
            &root,
            cause,
            &request,
            recent_episode_limit,
            &self.runtime.inner.input_evidence_budget,
        )?;
        let InputPreparation::Prepared(prepared) = prepared else {
            return Ok(());
        };
        anyhow::ensure!(
            prepared.original_token == root.token
                && prepared.facts.state.as_ref() == Some(prepared.state.as_ref()),
            "prepared input installation changed State"
        );
        self.episodes = Some(prepared.episodes);
        self.input_evidence = Some(prepared.inputs);
        self.candidate.queue = prepared.queue;
        self.candidate._queue_budget_view = Some(prepared.queue_budget_view);
        let facts = Arc::new(self.facts(Default::default()));
        let reservation = &mut self.reservations[self.primary_index];
        reservation.begin(crate::ids::audit_event_id())?;
        MutationReservation::committed_all(
            std::slice::from_mut(reservation),
            vec![KnownCommit::State {
                state: facts.state.clone(),
                episodes: facts.episodes.clone(),
                input_evidence: facts.input_evidence.clone(),
                runtime: Some(facts.clone()),
            }],
        )?;
        // Save exact installation facts synchronously before the first await.
        self.runtime.install_mutation_facts(&facts).await?;
        let observations = MutationReservation::publish_all_known(std::slice::from_mut(
            &mut self.reservations[self.primary_index],
        ))?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        Ok(())
    }

    pub(super) fn capture_input_evidence(
        &self,
    ) -> Result<super::resident_input_evidence::CapturedInputEvidence> {
        self.input_evidence.as_ref().ok_or_else(|| {
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into())
        })?.capture()
    }

    pub(super) fn context_input_evidence_policy(
        &self,
    ) -> Result<(
        std::collections::BTreeSet<super::resident_input_evidence::InputEvidenceKey>,
        usize,
        usize,
    )> {
        let limits = self.runtime.inner.input_evidence_budget.limits();
        let bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("input evidence byte limit overflow")?;
        Ok((
            Default::default(),
            usize::try_from(limits.record_limit)?,
            usize::try_from(bytes)?,
        ))
    }

    pub(super) fn capture_context_with_inputs(
        &self,
        episode_limit: usize,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    ) -> Result<super::resident_state::CapturedAgentContext> {
        let mut captured = self.capture_context(episode_limit)?;
        captured.input_evidence = Some(self.input_evidence.as_ref().ok_or_else(|| {
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into())
        })?.capture_for_request(request)?);
        Ok(captured)
    }

    pub(super) fn defer_abort_current_run(&mut self, reason: impl Into<String>) {
        self.deferred_abort =
            self.candidate
                .current_run_abort
                .clone()
                .map(|handle| DeferredRunAbort {
                    handle,
                    reason: reason.into(),
                });
    }

    #[inline(never)]
    pub(super) fn persist_state<'a>(
        &'a mut self,
        _storage: &AppStorage,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.persist_state_inner(None))
    }

    pub(super) async fn persist_state_with_started_turn(&mut self, turn: TurnRecord) -> Result<()> {
        self.persist_state_inner(Some(turn)).await
    }

    async fn persist_state_inner(&mut self, started_turn: Option<TurnRecord>) -> Result<()> {
        self.validate_candidate_identity()?;
        if self.candidate.state == self.base.state && started_turn.is_none() {
            return self.install_transient().await;
        }
        let started = std::time::Instant::now();
        let event = AuditEvent::legacy(
            "agent_state_mutated",
            serde_json::json!({
                "agent_id": self.candidate.state.id,
            }),
        );
        let command = crate::runtime_db::transitions::AgentPostureTransitionCommand {
            agent_id: self.candidate.state.id.clone(),
            expected_incarnation: Some(self.runtime.inner.identity_incarnation),
            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(self.base.last_persisted_state.clone())),
                record: Box::new(self.candidate.state.clone()),
            },
            audit_events: vec![event.clone()],
            fault: self.runtime.take_transition_fault(),
        };
        let intended_state = command.agent_state.record.clone();
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::posture(
            self.input_evidence_scope(),
            event.id,
            Utc::now(),
            command,
            started_turn,
        )?;
        let result = self.commit_prepared_input_transition(prepared).await;
        crate::diagnostics::record_storage_persist_state(started.elapsed());
        let commit = result?;
        if !commit.applied && intended_state.as_ref() != &self.base.last_persisted_state {
            return Err(
                crate::runtime_db::RuntimeStateTransitionConflict::concurrent_mutation(
                    "agent_state",
                    &self.candidate.state.id,
                )
                .into(),
            );
        }
        self.runtime.apply_transition_commit(commit).await;
        Ok(())
    }

    // The closure adapts an existing typed repository command. It cannot
    // observe or mutate the live RuntimeAgent while SQL is in progress.
    #[inline(never)]
    pub(super) fn commit_transition<'a>(
        &'a mut self,
        operation_id: String,
        commit: impl FnOnce(&super::resident_input_evidence::InputEvidenceScope) -> Result<TransitionCommit>
            + Send
            + 'a,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        // Keep transaction future construction out of nested runtime polls.
        let scope = self.input_evidence_scope();
        Box::pin(self.commit_transition_inner(operation_id, None, move || commit(&scope)))
    }

    pub(super) async fn commit_prepared_queue(
        &mut self,
        operation_id: String,
        mut queue: crate::runtime_db::transitions::PreparedQueueTransition,
    ) -> Result<TransitionCommit> {
        use crate::runtime_db::transitions::QueueMutation;
        let entry = match &queue.command.mutation {
            QueueMutation::Consume(entry)
            | QueueMutation::Upsert(entry)
            | QueueMutation::CompareAndSet { record: entry, .. } => entry,
        };
        if matches!(
            entry.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) && !queue
            .command
            .message_evidence
            .iter()
            .any(|message| message.id == entry.message_id)
        {
            let message = if let Some(message) = self
                .candidate
                .queue
                .peek_next_matching(|message| message.id == entry.message_id)
                .or_else(|| {
                    self.base
                        .queue
                        .peek_next_matching(|message| message.id == entry.message_id)
                }) {
                message.clone()
            } else {
                use super::resident_input_evidence::{
                    InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
                };
                let captured = self.capture_input_evidence()?;
                let record = captured
                    .lookup(&Key::new(Family::Message, &entry.message_id))?
                    .context("pending mutation canonical Message was pruned")?;
                let PreparedInputEvidence::Message(message) = &record.record else {
                    bail!("pending Message has a different family");
                };
                message.clone()
            };
            queue.command.message_evidence.push(message);
        }
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::queue(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            queue,
        )?;
        self.commit_prepared_input_transition(prepared).await
    }

    pub(super) async fn commit_prepared_terminal(
        &mut self,
        operation_id: String,
        command: crate::runtime_db::transitions::TurnTerminalTransitionCommand,
    ) -> Result<TransitionCommit> {
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::terminal(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            command,
        )?;
        self.commit_prepared_input_transition(prepared).await
    }

    /// Freeze original command/targets before SQL. A receipt replay supplies no
    /// historical installation candidate and is resolved from a fresh read view.
    #[inline(never)]
    pub(super) fn commit_prepared_input_transition<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        Box::pin(async move {
            self.commit_prepared_input_with_result(prepared)
                .await
                .map(|(commit, _)| commit)
        })
    }

    pub(super) fn commit_prepared_queue_progress<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(
                        TransitionCommit,
                        Option<crate::runtime_db::transitions::QueueHeadNoProgressOutcome>,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::QueueHeadNoProgress { outcome } = result else {
                bail!("queue progress receipt has another result family");
            };
            Ok((commit, outcome))
        })
    }

    #[inline(never)]
    fn commit_prepared_input_with_result<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(
                        TransitionCommit,
                        crate::runtime_db::input_mutation_receipts::InputMutationResult,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(self.commit_prepared_input_transition_inner(prepared))
    }

    async fn commit_prepared_input_transition_inner(
        &mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<(
        TransitionCommit,
        crate::runtime_db::input_mutation_receipts::InputMutationResult,
    )> {
        use super::input_evidence_budget::{serialized_bytes, PendingAdmission};
        use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;
        use crate::runtime_db::transitions::InputMutationExecution;
        self.validate_candidate_identity()?;
        let scopes = self
            .reservations
            .iter()
            .map(
                |reservation| super::resident_input_evidence::InputEvidenceScope {
                    agent_id: reservation.agent_id().to_owned(),
                    identity_incarnation: reservation.identity_incarnation(),
                },
            )
            .collect::<Vec<_>>();
        let prepared = prepared.with_scope_group(scopes)?;
        let _owned = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &self.input_evidence_scope(),
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: prepared.owned_payload_bytes()?,
                    metadata_estimate_bytes: 0,
                },
            )?;
        let mut saved_runtime = vec![Arc::new(self.facts(Default::default()))];
        for (index, reservation) in self.reservations.iter().enumerate() {
            if index == self.primary_index {
                continue;
            }
            if let Some(bridge) = &self.runtime.inner.host_bridge {
                if let Some(runtime) = bridge.runtime_for_mutation(reservation).await? {
                    let state = self.metadata[index]
                        .as_ref()
                        .and_then(|facts| facts.state.clone())
                        .context("related loaded runtime has no State")?;
                    saved_runtime.push(Arc::new(
                        runtime
                            .prepare_external_input_facts(
                                reservation,
                                state,
                                reservation.prepared_episodes_for_state_only_commit()?,
                                reservation.prepared_inputs_for_state_only_commit()?,
                            )
                            .await?,
                    ));
                }
            }
        }
        let ledger = self.runtime.inner.input_evidence_budget.clone();
        let mut pending = Vec::new();
        for message in prepared.pending_messages()? {
            match ledger.reserve_pending(
                &self.input_evidence_scope(),
                &message.id,
                serialized_bytes(&message.body)?,
            ) {
                Ok(PendingAdmission::Reserved(ticket)) => pending.push((ledger.clone(), ticket)),
                Ok(PendingAdmission::ExistingAccepted) => {}
                Err(error) => {
                    for (ledger, ticket) in &pending {
                        ledger.definitely_not_committed(ticket)?;
                    }
                    return Err(error.into());
                }
            }
        }
        let operation_id = prepared.descriptor().command_nonce().to_owned();
        let pending_cleanup = pending.clone();
        let witness = match super::mutation_witness::MutationWitness::input_mutation(
            operation_id.clone(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            pending,
        )
        .and_then(|witness| witness.with_input_runtime(saved_runtime))
        {
            Ok(witness) => witness,
            Err(error) => {
                for (ledger, ticket) in &pending_cleanup {
                    ledger.definitely_not_committed(ticket)?;
                }
                return Err(error);
            }
        };
        let db = self.runtime.inner.runtime_db.clone();
        let scope = self.input_evidence_scope();
        let replay_observation = witness.clone();
        let mut original_result = None;
        let result = self
            .commit_transition_inner(operation_id, Some(witness), || {
                match db
                    .transitions_in_scope(&scope)
                    .commit_prepared_transition(&prepared)?
                {
                    InputMutationExecution::Committed { value, receipt } => {
                        original_result = Some(receipt.outcome.result);
                        Ok(value)
                    }
                    InputMutationExecution::Replayed { receipt } => {
                        replay_observation.mark_input_storage_replayed()?;
                        Err(PreparedInputReplay(receipt).into())
                    }
                }
            })
            .await;
        match result {
            Err(error) if error.downcast_ref::<PreparedInputReplay>().is_some() => {
                let receipt = error
                    .downcast::<PreparedInputReplay>()
                    .expect("checked replay")
                    .0;
                self.runtime
                    .reconcile_reserved_mutations(&mut self.reservations)
                    .await?;
                self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
                self.base = self.candidate.clone();
                let root = self.runtime.inner.mutation_owner.capture()?;
                self.episodes = root.as_ref().and_then(|root| root.episodes.clone());
                self.input_evidence = root.and_then(|root| root.input_evidence.clone());
                self.deferred_abort = None;
                Ok((
                    TransitionCommit {
                        applied: receipt.outcome.applied,
                        delivery_receipt: receipt.outcome.delivery_receipt,
                        ..Default::default()
                    },
                    receipt.outcome.result,
                ))
            }
            result => result.and_then(|commit| {
                Ok((
                    commit,
                    original_result.context("input transaction omitted its original result")?,
                ))
            }),
        }
    }

    pub(super) async fn commit_input_evidence<T: Send>(
        &mut self,
        operation_id: String,
        commit: impl FnOnce(
                &super::resident_input_evidence::InputEvidenceScope,
            )
                -> Result<crate::runtime_db::input_evidence_writes::InputEvidenceCommit<T>>
            + Send,
    ) -> Result<T> {
        let mut record = None;
        let committed = self
            .commit_transition(operation_id, |scope| {
                let acknowledged = commit(scope)?;
                record = Some(acknowledged.record);
                Ok(TransitionCommit {
                    applied: true,
                    effects: crate::runtime_db::transitions::PostCommitEffects {
                        agent_state: acknowledged.agent_state,
                        input_evidence_delta: acknowledged.input_delta,
                        notify_memory_index: true,
                        notify_scheduler: true,
                        ..Default::default()
                    },
                    ..Default::default()
                })
            })
            .await?;
        self.runtime.apply_transition_commit(committed).await;
        record.context("input evidence commit omitted its acknowledged result")
    }

    async fn commit_transition_inner(
        &mut self,
        operation_id: String,
        witness: Option<super::mutation_witness::MutationWitness>,
        commit: impl FnOnce() -> Result<TransitionCommit>,
    ) -> Result<TransitionCommit> {
        self.validate_candidate_identity()?;
        let input_witness = witness.as_ref().filter(|witness| {
            witness.kind() == super::mutation_witness::MutationWitnessKind::InputMutation
        });
        let keys_result = match witness.as_ref() {
            Some(witness) => MutationReservation::begin_all_with_witness(
                &mut self.reservations,
                operation_id,
                witness.clone(),
            ),
            None => MutationReservation::begin_all(&mut self.reservations, operation_id),
        };
        let keys = match keys_result {
            Ok(keys) => keys,
            Err(error) => {
                if let Some(witness) = input_witness {
                    witness.release_pending_not_committed(false)?;
                }
                return Err(error.into());
            }
        };
        if let Some(witness) = input_witness {
            if let Err(error) = witness.mark_pending_started() {
                witness.release_pending_not_committed(false)?;
                MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
                return Err(error);
            }
        }
        let result = commit();
        // The synchronous attempt has ended even when its COMMIT ACK is lost.
        // Persist this fact before any async installation or caller cancellation.
        if let Some(witness) = &witness {
            witness.finish_original_attempt();
        }
        let mut committed = match result {
            Ok(committed) => committed,
            Err(error) => {
                if persistence_certainty(&error)
                    == Some(PersistenceCertainty::DefinitelyNotCommitted)
                {
                    if let Some(witness) = input_witness {
                        witness.release_pending_not_committed(false)?;
                    }
                    MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
                    self.candidate = self.base.clone();
                    self.deferred_abort = None;
                    self.control_cancellation = None;
                } else if let Some(witness) = input_witness {
                    witness.mark_pending_unknown()?;
                }
                return Err(error);
            }
        };
        if let Some(witness) = input_witness {
            witness.mark_pending_result(committed.applied, false)?;
        }
        if !committed.applied && input_witness.is_none() {
            MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
            self.candidate = self.base.clone();
            self.deferred_abort = None;
            self.control_cancellation = None;
            return Ok(committed);
        }
        if !committed.applied {
            self.candidate = self.base.clone();
            self.deferred_abort = None;
            self.control_cancellation = None;
        }
        self.runtime.inner.input_evidence_budget.release_accepted(
            &self.input_evidence_scope(),
            &committed.effects.removed_queue_message_ids,
        )?;
        if let Some(mutation) = committed.effects.agent_state.as_ref() {
            anyhow::ensure!(
                mutation.record.id == self.candidate.state.id,
                "committed runtime state identity mismatch"
            );
            self.candidate.state = mutation.record.as_ref().clone();
            self.candidate.last_persisted_state = self.candidate.state.clone();
        } else {
            anyhow::ensure!(
                self.candidate.state == self.base.state,
                "typed commit omitted a changed agent state"
            );
        }
        for message_id in &committed.effects.removed_queue_message_ids {
            self.candidate
                .queue
                .pop_next_matching(|message| &message.id == message_id);
        }
        for message in &committed.effects.queued_messages {
            if self
                .candidate
                .queue
                .peek_next_matching(|queued| queued.id == message.id)
                .is_none()
            {
                self.candidate.queue.push(message.clone());
            }
        }
        let mut projection_error = None;
        if !committed.effects.context_episodes.is_empty()
            || !committed.effects.removed_context_episode_ids.is_empty()
        {
            if let Some(episodes) = self.episodes.as_ref() {
                match episodes.with_committed(
                    self.runtime.inner.identity_incarnation,
                    &committed.effects.context_episodes,
                    &committed.effects.removed_context_episode_ids,
                ) {
                    Ok(next) => self.episodes = next.map(Arc::new),
                    Err(error) => {
                        self.episodes = None;
                        projection_error = Some(error);
                    }
                }
            }
        }
        if let Some(inputs) = self.input_evidence.as_ref() {
            let next = runtime_input_mandatory_roots(
                inputs,
                &committed.effects.input_evidence_delta,
                &self.candidate.state,
                &self.candidate.queue,
            )
            .and_then(|mandatory| {
                inputs.with_committed(&committed.effects.input_evidence_delta, mandatory)
            });
            match next {
                Ok(next) => self.input_evidence = next.map(Arc::new),
                Err(error) => {
                    self.input_evidence = None;
                    projection_error.get_or_insert(error);
                }
            }
        }
        match self
            .runtime
            .inner
            .input_evidence_budget
            .register_queue_view(
                &self.input_evidence_scope(),
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                self.candidate.queue.shared_messages(),
            ) {
            Ok(view) => self.candidate._queue_budget_view = Some(view),
            Err(error) => {
                projection_error.get_or_insert(error.into());
            }
        }
        let facts = Arc::new(self.facts(committed.effects.clone()));
        // Retain all authoritative installation facts before the first await
        // after SQL. Cancellation leaves a known commit for the next owner call.
        self.record_committed_facts(facts.clone(), &committed.effects)?;
        if let Some(error) = projection_error {
            // SQL is acknowledged. Keep its exact state, queue and effects in
            // Known recovery even if a derived domain must be prepared again.
            return Err(
                error.context("runtime mutation committed; input publication requires preparation")
            );
        }
        if committed.effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            bail!(
                "runtime mutation committed but publication requires reconciliation: {}",
                keys[self.primary_index].operation_id
            );
        }
        self.runtime.install_known_group(&self.reservations).await?;
        let observations = MutationReservation::publish_all_known(&mut self.reservations)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        if let Some(cancellation) = self.control_cancellation.take() {
            self.runtime
                .resume_saved_control_cancellation(&cancellation)?;
        }
        strip_authoritative_effects(&mut committed.effects);
        if let Some(started) = self.measurement_started.take() {
            crate::diagnostics::io_probe::record_timing(
                crate::diagnostics::io_probe::TimingKind::CommitPublication,
                started.elapsed(),
            );
        }
        self.measurement_started = crate::diagnostics::io_probe::start_timer();
        Ok(committed)
    }

    pub(super) fn capture_context(
        &self,
        episode_limit: usize,
    ) -> Result<super::resident_state::CapturedAgentContext> {
        let root = self
            .runtime
            .inner
            .mutation_owner
            .capture()?
            .ok_or_else(|| anyhow!("runtime state has not been published"))?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "context capture cannot observe a private state candidate"
        );
        root.capture_context(episode_limit)
    }

    pub(super) async fn commit_episode_memory(
        &mut self,
        command: &crate::runtime_db::episode_memory::EpisodeMemoryTransitionCommand,
    ) -> Result<crate::runtime_db::episode_memory::EpisodeMemoryReceipt> {
        use crate::runtime_db::episode_memory::EpisodeMemoryCommit;
        let operation_id = crate::ids::audit_event_id();
        let witness = super::mutation_witness::MutationWitness::episode_memory(
            operation_id.clone(),
            command,
        )?;
        let db = self.runtime.inner.runtime_db.clone();
        let mut receipt = None;
        let commit = self
            .commit_transition_inner(operation_id, Some(witness), || {
                match db.commit_episode_memory(command)? {
                    EpisodeMemoryCommit::Applied {
                        receipt: saved,
                        mut transition,
                        episode,
                    } => {
                        receipt = Some(saved);
                        transition.effects.context_episodes.extend(episode);
                        Ok(transition)
                    }
                    EpisodeMemoryCommit::Replayed { receipt: saved } => {
                        // Return the original result without reinstalling its old candidate.
                        receipt = Some(saved);
                        Ok(TransitionCommit::default())
                    }
                }
            })
            .await?;
        self.runtime.apply_transition_commit(commit).await;
        receipt.ok_or_else(|| anyhow!("episode commit omitted its original result"))
    }

    pub(super) async fn commit_control(
        &mut self,
        prepared: &super::control::PreparedRuntimeControl,
    ) -> Result<crate::runtime_db::control_transition::ControlReceipt> {
        use crate::runtime_db::control_transition::ControlCommit;
        let command = &prepared.command;
        let saved = SavedControlCancellation {
            key: command.key().clone(),
            digest: command.command_digest().to_owned(),
            owner_epoch: self.reservations[self.primary_index].owner_epoch(),
            facts: prepared.cancellation.clone(),
        };
        self.runtime.inner.control_cancellations.retain_prepared(
            &saved.key,
            &saved.digest,
            saved.owner_epoch,
            saved.facts.clone(),
        )?;
        let operation_id = crate::ids::audit_event_id();
        let witness = super::mutation_witness::MutationWitness::control(
            operation_id.clone(),
            command,
            saved.facts.clone(),
            saved.owner_epoch,
        )?;
        self.control_cancellation = Some(saved.clone());
        if command.invocation().action == ControlAction::Stop {
            self.candidate.current_run_abort = None;
        }
        let db = self.runtime.inner.runtime_db.clone();
        let mut receipt = None;
        let result = self
            .commit_transition_inner(operation_id, Some(witness), || {
                match db.commit_control(command)? {
                    ControlCommit::Applied {
                        receipt: original,
                        delta,
                    } => {
                        validate_control_publication(&delta, &saved.key.agent_id)?;
                        receipt = Some(original);
                        Ok(delta.transition)
                    }
                    ControlCommit::Replayed { receipt: original } => {
                        receipt = Some(original);
                        Ok(TransitionCommit::default())
                    }
                }
            })
            .await;
        match result {
            Ok(commit) => {
                if !commit.applied {
                    self.runtime.inner.control_cancellations.release_prepared(
                        &saved.key,
                        &saved.digest,
                        saved.owner_epoch,
                    )?;
                }
                self.runtime.apply_transition_commit(commit).await;
                receipt.ok_or_else(|| anyhow!("Control commit omitted its original result"))
            }
            Err(error) => {
                if persistence_certainty(&error)
                    == Some(PersistenceCertainty::DefinitelyNotCommitted)
                {
                    self.runtime.inner.control_cancellations.release_prepared(
                        &saved.key,
                        &saved.digest,
                        saved.owner_epoch,
                    )?;
                }
                Err(error)
            }
        }
    }

    pub(super) async fn install_transient(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate.state == self.base.state
                && self.candidate.last_persisted_state == self.base.last_persisted_state,
            "transient installation cannot change durable agent state"
        );
        self.install_durable_readback().await
    }

    pub(super) async fn install_durable_readback(&mut self) -> Result<()> {
        self.validate_candidate_identity()?;
        anyhow::ensure!(
            self.candidate.state == self.candidate.last_persisted_state,
            "durable readback must install one exact persisted state"
        );
        MutationReservation::begin_all(&mut self.reservations, crate::ids::audit_event_id())?;
        let facts = Arc::new(self.facts(Default::default()));
        self.record_committed_facts(facts.clone(), &Default::default())?;
        self.runtime.install_known_group(&self.reservations).await?;
        let observations = MutationReservation::publish_all_known(&mut self.reservations)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        Ok(())
    }

    // A caller that owns a committed operation may finish its saved installation
    // without releasing the reservation or guessing the result of unknown SQL.
    pub(super) async fn reconcile_known_commit(&mut self) -> Result<bool> {
        let known = match MutationReservation::validate_group_known(&self.reservations) {
            Ok(known) => known,
            Err(mutation_owner::MutationOwnerError::NeedsReconcile(_)) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if known.iter().all(Option::is_none) {
            return Ok(false);
        }
        self.runtime
            .reconcile_reserved_mutations(&mut self.reservations)
            .await?;
        for (metadata, saved) in self.metadata.iter_mut().zip(known) {
            if let Some(KnownCommit::Metadata(facts)) = saved {
                *metadata = Some(facts);
            }
        }
        self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
        let root = self.runtime.inner.mutation_owner.capture()?;
        self.episodes = root.as_ref().and_then(|root| root.episodes.clone());
        self.input_evidence = root.as_ref().and_then(|root| root.input_evidence.clone());
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        self.control_cancellation = None;
        Ok(true)
    }

    fn validate_candidate_identity(&self) -> Result<()> {
        let agent_id = self.runtime.inner.mutation_owner.agent_id();
        anyhow::ensure!(
            self.candidate.state.id == agent_id && self.base.state.id == agent_id,
            "cannot write agent state for `{}` through storage scoped to `{agent_id}`",
            self.candidate.state.id
        );
        Ok(())
    }

    fn record_committed_facts(
        &mut self,
        facts: Arc<RuntimeMutationFacts>,
        effects: &crate::runtime_db::transitions::PostCommitEffects,
    ) -> Result<()> {
        let known = self
            .reservations
            .iter()
            .enumerate()
            .map(|(index, reservation)| {
                if index == self.primary_index {
                    return Ok(KnownCommit::State {
                        state: facts.state.clone(),
                        episodes: facts.episodes.clone(),
                        input_evidence: facts.input_evidence.clone(),
                        runtime: Some(facts.clone()),
                    });
                }
                let mut metadata = self.metadata[index]
                    .as_ref()
                    .ok_or_else(|| anyhow!("related runtime metadata snapshot is missing"))?
                    .as_ref()
                    .clone();
                if let Some(identity) = effects
                    .agent_identities
                    .iter()
                    .find(|identity| identity.agent_id == reservation.agent_id())
                {
                    anyhow::ensure!(
                        identity.incarnation == reservation.identity_incarnation(),
                        "committed related identity incarnation mismatch"
                    );
                    metadata.identity = Some(Arc::new(identity.clone()));
                }
                Ok(KnownCommit::Metadata(Arc::new(metadata)))
            })
            .collect::<Result<Vec<_>>>()?;
        let metadata = known
            .iter()
            .map(|known| match known {
                KnownCommit::Metadata(facts) => Some(facts.clone()),
                KnownCommit::State { .. } => None,
            })
            .collect();
        MutationReservation::committed_all(&mut self.reservations, known)?;
        self.metadata = metadata;
        Ok(())
    }

    pub(super) async fn restore_bootstrap_replay_message(
        &mut self,
        storage: &AppStorage,
        message: &MessageEnvelope,
    ) -> Result<()> {
        if self
            .candidate
            .queue
            .peek_next_matching(|queued| queued.id == message.id)
            .is_none()
        {
            self.candidate.queue.push(message.clone());
        }
        let queued_messages = self.candidate.queue.len();
        self.candidate.state.pending = queued_messages;
        scheduler_executor::apply_bootstrap_recovered_projection(
            &mut self.candidate.state,
            scheduler_executor::BootstrapRecoveryFacts { queued_messages },
        );
        self.persist_state(storage).await
    }

    fn facts(
        &self,
        mut effects: crate::runtime_db::transitions::PostCommitEffects,
    ) -> RuntimeMutationFacts {
        // The authoritative domain/queue already own the exact allocations.
        // Recovery notifications retain only their original event responsibility.
        strip_input_evidence_payloads(&mut effects);
        RuntimeMutationFacts {
            owner_epoch: self.reservations[self.primary_index].owner_epoch(),
            state: Arc::new(self.candidate.state.clone()),
            episodes: self.episodes.clone(),
            input_evidence: self.input_evidence.clone(),
            queue: self.candidate.queue.clone(),
            _queue_budget_view: self.candidate._queue_budget_view.clone(),
            current_run_abort: self.candidate.current_run_abort.clone(),
            deferred_abort: self.deferred_abort.clone(),
            control_cancellation: self.control_cancellation.clone(),
            effects,
            recovered_cache: None,
        }
    }
}

fn runtime_input_mandatory_roots(
    inputs: &super::resident_input_evidence::ResidentInputEvidence,
    delta: &super::resident_input_evidence::CommittedInputEvidenceDelta,
    state: &AgentState,
    queue: &RuntimeQueue,
) -> Result<std::collections::BTreeSet<super::resident_input_evidence::InputEvidenceKey>> {
    use super::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
    };
    use crate::domain::execution_protocol::{ExecutionAttemptState, WorkItemExecutionState};
    let captured = inputs.capture()?;
    let mut records = captured
        .records
        .into_iter()
        .map(|record| (record.locator.key.clone(), record))
        .collect::<std::collections::BTreeMap<_, _>>();
    for removed in &delta.removals {
        records.remove(&removed.key);
    }
    for record in &delta.records {
        records.insert(record.locator.key.clone(), record.clone());
    }
    let mut roots = queue
        .shared_messages()
        .map(|message| Key::new(Family::Message, &message.id))
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(turn_id) = &state.current_turn_id {
        roots.insert(Key::new(Family::Turn, turn_id));
    }
    if let Some(binding) = &state.current_execution_binding {
        roots.insert(Key::new(Family::Message, &binding.source_message_id));
        roots.insert(Key::new(Family::Turn, &binding.turn_id));
        if let Some(id) = &binding.activation_id {
            roots.insert(Key::new(Family::Attempt, id));
        }
        if let Some(id) = &binding.work_item_id {
            roots.insert(Key::new(Family::WorkExecution, id));
        }
    }
    for (key, record) in records {
        match &record.record {
            PreparedInputEvidence::Attempt(attempt)
                if attempt.state != ExecutionAttemptState::Settled =>
            {
                roots.insert(key);
            }
            PreparedInputEvidence::WorkExecution { record, .. }
                if !matches!(record.state, WorkItemExecutionState::Terminal { .. }) =>
            {
                roots.insert(key);
            }
            _ => {}
        }
    }
    Ok(roots)
}

fn strip_input_evidence_payloads(effects: &mut crate::runtime_db::transitions::PostCommitEffects) {
    effects.input_evidence_delta = Default::default();
    effects.messages.clear();
    effects.queued_messages.clear();
    effects.turns.clear();
    effects.tool_executions.clear();
    effects.briefs.clear();
    effects.transcript_entries.clear();
    effects.execution_protocol = None;
}

fn strip_authoritative_effects(effects: &mut crate::runtime_db::transitions::PostCommitEffects) {
    strip_input_evidence_payloads(effects);
    effects.agent_state = None;
    effects.queued_messages.clear();
    effects.removed_queue_message_ids.clear();
    effects.work_items.clear();
    effects.tasks.clear();
    effects.timers.clear();
    effects.external_triggers.clear();
    effects.agent_identities.clear();
    effects.context_episodes.clear();
    effects.removed_context_episode_ids.clear();
}

impl RuntimeHandle {
    pub(crate) async fn prepare_external_input_facts(
        &self,
        reservation: &MutationReservation,
        state: Arc<AgentState>,
        episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    ) -> Result<RuntimeMutationFacts> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        anyhow::ensure!(
            state.id == reservation.agent_id(),
            "external input state scope differs"
        );
        let agent = self.inner.agent.lock().await;
        anyhow::ensure!(
            agent.state == *state,
            "external input preparation saw a different runtime state"
        );
        Ok(RuntimeMutationFacts {
            owner_epoch: reservation.owner_epoch(),
            state,
            episodes,
            input_evidence,
            queue: agent.queue.clone(),
            _queue_budget_view: agent._queue_budget_view.clone(),
            current_run_abort: agent.current_run_abort.clone(),
            deferred_abort: None,
            control_cancellation: None,
            effects: Default::default(),
            recovered_cache: None,
        })
    }

    pub(crate) async fn commit_input_evidence<T: Send + 'static>(
        &self,
        operation_id: String,
        commit: impl FnOnce(
                &super::resident_input_evidence::InputEvidenceScope,
            )
                -> Result<crate::runtime_db::input_evidence_writes::InputEvidenceCommit<T>>
            + Send
            + 'static,
    ) -> Result<T> {
        let admission = self.execution_admission_lease()?;
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = async {
                let mut mutation = runtime.begin_mutation().await?;
                mutation.commit_input_evidence(operation_id, commit).await
            }
            .await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("input evidence worker ended before returning its result")?
    }

    pub(crate) fn resume_saved_control_cancellation(
        &self,
        saved: &SavedControlCancellation,
    ) -> Result<()> {
        if let Some(facts) = self.inner.control_cancellations.confirm_prepared(
            &saved.key,
            &saved.digest,
            saved.owner_epoch,
        )? {
            anyhow::ensure!(
                Arc::ptr_eq(&facts, &saved.facts),
                "Control original targets changed"
            );
        } else {
            self.inner.control_cancellations.retain_prepared(
                &saved.key,
                &saved.digest,
                saved.owner_epoch,
                saved.facts.clone(),
            )?;
            self.inner.control_cancellations.confirm_prepared(
                &saved.key,
                &saved.digest,
                saved.owner_epoch,
            )?;
        }
        saved.facts.signal();
        saved
            .facts
            .start_completion(self, &saved.key, &saved.digest, saved.owner_epoch);
        Ok(())
    }

    pub(super) async fn resume_original_control_cancellation(
        &self,
        receipt: &crate::runtime_db::control_transition::ControlReceipt,
    ) -> Result<()> {
        let mut reservations =
            mutation_owner::MutationOwner::reserve_all(vec![self.inner.mutation_owner.clone()])
                .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        let original_plan = self
            .inner
            .control_cancellations
            .prepared_for_receipt(&receipt.key, &receipt.command_digest)?;
        drop(reservations);
        if let Some((owner_epoch, facts)) = original_plan {
            self.resume_saved_control_cancellation(&SavedControlCancellation {
                key: receipt.key.clone(),
                digest: receipt.command_digest.clone(),
                owner_epoch,
                facts,
            })?;
        } else {
            self.reconcile_loaded_control_obligations().await?;
        }
        Ok(())
    }

    // Save the original physical targets before reconcile_all clears its ticket.
    // The caller may signal this plan only after the complete group publishes.
    pub(crate) fn control_cancellation_for_known(
        &self,
        reservation: &MutationReservation,
        known: &KnownCommit,
    ) -> Result<Option<SavedControlCancellation>> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        if let KnownCommit::State {
            runtime: Some(facts),
            ..
        } = known
        {
            if facts.owner_epoch == reservation.owner_epoch() {
                if let Some(saved) = &facts.control_cancellation {
                    return Ok(Some(saved.clone()));
                }
            }
        }
        let Some(recovery) = reservation.recovery()? else {
            return Ok(None);
        };
        let Some(witness) = recovery.witness else {
            return Ok(None);
        };
        let super::mutation_witness::MutationWitnessFamily::Control {
            key,
            command_digest,
            ..
        } = witness.family()
        else {
            return Ok(None);
        };
        let Some(facts) = witness.control_cancellation() else {
            return Ok(None);
        };
        Ok(Some(SavedControlCancellation {
            key: key.clone(),
            digest: command_digest.clone(),
            owner_epoch: witness
                .control_owner_epoch()
                .ok_or_else(|| anyhow!("Control original owner epoch missing"))?,
            facts,
        }))
    }

    pub(crate) fn spawn_owned_mutation(
        &self,
        mutation: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let mut tasks = self
            .inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned");
        if tasks.closed {
            return Err(super::execution_admission::closed_error());
        }
        tasks.handles.retain(|task| !task.is_finished());
        // Registration and closing share one gate. Shutdown cannot miss a task
        // spawned immediately after taking the previous handles to drain.
        tasks.handles.push(tokio::spawn(mutation));
        Ok(())
    }

    pub(super) fn close_mutation_admission(&self) {
        self.inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned")
            .closed = true;
    }

    pub(crate) async fn join_owned_mutations(&self) -> Result<()> {
        let drain = {
            let mut tasks = self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned");
            tasks.closed = true;
            tasks.drain.clone()
        };
        let mut draining = drain.lock().await;
        draining.extend(std::mem::take(
            &mut self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned")
                .handles,
        ));
        let mut failure = None;
        while let Some(task) = draining.last_mut() {
            // The supervisor retains the handle across await. Cancelling this
            // drainer leaves it for the next drainer rather than detaching it.
            let result = task.await;
            draining.pop();
            if let Err(error) = result {
                failure.get_or_insert_with(|| anyhow::Error::new(error));
            }
        }
        drop(draining);
        let reconciled = self.reconcile_posture_observation().await;
        failure.map_or(reconciled, Err)
    }

    // Physical retirement preserves durable Task/Timer intent for startup
    // recovery. It stops and joins only this instance's original producers.
    pub(crate) async fn join_retiring_resources(&self) -> Result<()> {
        self.inner.control_cancellations.cancel_local_producers();
        self.join_owned_mutations().await?;
        // Accepted registration workers may have reached their original lease
        // after the first snapshot; local closing also fences later attachment.
        self.inner.control_cancellations.cancel_local_producers();
        self.inner.control_cancellations.drain_retiring().await?;
        let mut draining = self.inner.owned_task_drain.lock().await;
        loop {
            draining.extend(std::mem::take(
                &mut *self
                    .inner
                    .owned_task_handles
                    .lock()
                    .expect("owned tasks poisoned"),
            ));
            let Some(task) = draining.last_mut() else {
                break;
            };
            // These are cancellable sleep/discovery observers. Resource-owning
            // Task/Command/Timer monitors are joined by the registry above.
            task.abort();
            let result = task.await;
            draining.pop();
            if let Err(error) = result {
                if !error.is_cancelled() {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn instance_owner_epoch(&self) -> u64 {
        self.inner.mutation_owner.owner_epoch()
    }

    #[inline(never)]
    pub(super) fn begin_mutation(
        &self,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + '_>>
    {
        Box::pin(self.begin_mutation_with_owners(vec![self.inner.mutation_owner.clone()]))
    }

    pub(super) fn validate_mutation_identity(&self) -> Result<()> {
        if let Some(identity) = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(self.inner.mutation_owner.agent_id())?
        {
            if identity.incarnation != self.inner.identity_incarnation
                || identity.status == crate::types::AgentRegistryStatus::Deleted
            {
                return Err(RuntimeMutationIdentityRetired {
                    agent_id: identity.agent_id,
                    expected_incarnation: self.inner.identity_incarnation,
                    actual_incarnation: identity.incarnation,
                    status: identity.status,
                }
                .into());
            }
        }
        Ok(())
    }

    #[inline(never)]
    pub(super) fn begin_mutation_with_agent_deletion<'a>(
        &'a self,
        deletion: Option<&'a crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + 'a>>
    {
        Box::pin(self.begin_mutation_with_agent_deletion_inner(deletion))
    }

    async fn begin_mutation_with_agent_deletion_inner(
        &self,
        deletion: Option<&crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> Result<RuntimeMutation> {
        let Some(deletion) = deletion else {
            return self.begin_mutation().await;
        };
        let identity = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(&deletion.agent_id)?;
        let Some(identity) = identity else {
            return self.begin_mutation().await;
        };
        let owner = if identity.agent_id == self.inner.mutation_owner.agent_id()
            && identity.incarnation == self.inner.identity_incarnation
        {
            self.inner.mutation_owner.clone()
        } else {
            match self.inner.host_bridge.as_ref() {
                Some(bridge) => {
                    bridge.mutation_owner_for(&identity.agent_id, identity.incarnation)?
                }
                None => self
                    .inner
                    .standalone_related_mutation_owners
                    .current_owner(&identity.agent_id, identity.incarnation)?,
            }
        };
        self.begin_mutation_with_owners(vec![self.inner.mutation_owner.clone(), owner])
            .await
    }

    async fn begin_mutation_with_owners(
        &self,
        owners: Vec<mutation_owner::MutationOwner>,
    ) -> Result<RuntimeMutation> {
        let measurement_started = crate::diagnostics::io_probe::start_timer();
        // reserve_all also acquires every member of a pending recovery group.
        let requested = owners.clone();
        let mut reservations = mutation_owner::MutationOwner::reserve_all(owners).await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        // Recovery can add scopes from an older operation. After atomic recovery
        // the new command owns only its explicit write set.
        reservations.retain(|reservation| {
            requested
                .iter()
                .any(|owner| reservation.validate_owner(owner).is_ok())
        });
        let primary_index = reservations
            .iter()
            .position(|reservation| {
                reservation
                    .validate_owner(&self.inner.mutation_owner)
                    .is_ok()
            })
            .ok_or_else(|| anyhow!("runtime mutation reservation is missing"))?;
        let observation = self.inner.mutation_owner.capture()?;
        if let Some(observation) = observation.as_ref() {
            anyhow::ensure!(
                observation.state.id == self.inner.mutation_owner.agent_id(),
                "published runtime state identity mismatch"
            );
        }
        self.validate_mutation_identity()?;
        let metadata = reservations
            .iter()
            .enumerate()
            .map(|(index, reservation)| {
                if index == primary_index {
                    return Ok(None);
                }
                let identity = self
                    .inner
                    .runtime_db
                    .agent_identities()
                    .latest(reservation.agent_id())?;
                if let Some(identity) = identity.as_ref() {
                    anyhow::ensure!(
                        identity.incarnation == reservation.identity_incarnation(),
                        "related mutation identity incarnation is no longer current"
                    );
                }
                let state = self
                    .inner
                    .runtime_db
                    .agent_states()
                    .latest(reservation.agent_id())?;
                Ok(Some(Arc::new(IdentityMutationFacts {
                    identity: identity.map(Arc::new),
                    state: state.map(Arc::new),
                })))
            })
            .collect::<Result<Vec<_>>>()?;
        let candidate = Box::new(self.inner.agent.lock().await.clone());
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let mut candidate_budget_views =
            vec![self.inner.input_evidence_budget.register_queue_view(
                &scope,
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                candidate.queue.shared_messages(),
            )?];
        if let Some(inputs) = observation
            .as_ref()
            .and_then(|root| root.input_evidence.as_ref())
        {
            candidate_budget_views.push(self.inner.input_evidence_budget.register_view(
                &scope,
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                &inputs.budget_records(),
            )?);
        }
        let mutation = RuntimeMutation {
            runtime: self.clone(),
            reservations,
            primary_index,
            metadata,
            episodes: observation.as_ref().and_then(|root| root.episodes.clone()),
            input_evidence: observation.and_then(|root| root.input_evidence.clone()),
            base: candidate.clone(),
            candidate,
            deferred_abort: None,
            control_cancellation: None,
            measurement_started,
            _candidate_budget_views: candidate_budget_views,
        };
        Ok(mutation)
    }

    async fn install_mutation_facts(&self, facts: &RuntimeMutationFacts) -> Result<()> {
        // The reservation remains held; state/cache readers can only see the
        // old publication or the complete new publication after this block.
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: facts.state.id.clone(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        if let Some(current) = &facts.recovered_cache {
            anyhow::ensure!(
                current.scope == scope,
                "recovered cache installation scope differs"
            );
        }
        let queue_budget_view = self.inner.input_evidence_budget.register_queue_view(
            &scope,
            super::input_evidence_budget::InputMemoryCategory::Current,
            facts.queue.shared_messages(),
        )?;
        {
            let mut agent = self.inner.agent.lock().await;
            agent.state = facts.state.as_ref().clone();
            agent.last_persisted_state = facts.state.as_ref().clone();
            agent.queue = facts.queue.clone();
            agent._queue_budget_view = Some(queue_budget_view);
            agent.current_run_abort = facts.current_run_abort.clone();
            if let Some(abort) = &facts.deferred_abort {
                if let Ok(mut reason) = abort.handle.reason.lock() {
                    *reason = abort.reason.clone();
                }
                abort.handle.token.cancel();
            }
        }
        let mut cache = self.inner.projection_cache.lock().await;
        if let Some(current) = &facts.recovered_cache {
            for id in &current.keys.work_items {
                cache.work_items.remove(id);
            }
            for id in &current.keys.tasks {
                cache.tasks.remove(id);
            }
            for id in &current.keys.timers {
                cache.timers.remove(id);
            }
            for id in &current.keys.external_triggers {
                cache.external_triggers.remove(id);
            }
            for record in &current.work_items {
                cache.upsert_work_item(record.clone());
            }
            for record in &current.tasks {
                cache.upsert_task(record.clone());
            }
            for record in &current.timers {
                cache.upsert_timer(record.clone());
            }
            for record in &current.external_triggers {
                cache.upsert_external_trigger(record.clone());
            }
        }
        for record in &facts.effects.work_items {
            cache.upsert_work_item(record.clone());
        }
        for record in &facts.effects.tasks {
            cache.upsert_task(record.clone());
        }
        for record in &facts.effects.timers {
            cache.upsert_timer(record.clone());
        }
        for record in &facts.effects.external_triggers {
            cache.upsert_external_trigger(record.clone());
        }
        drop(cache);
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            for identity in facts
                .effects
                .agent_identities
                .iter()
                .filter(|identity| identity.agent_id == self.inner.mutation_owner.agent_id())
            {
                bridge
                    .install_mutation_metadata(
                        &identity.agent_id,
                        self.inner.identity_incarnation,
                        &IdentityMutationFacts {
                            identity: Some(Arc::new(identity.clone())),
                            state: Some(facts.state.clone()),
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn reconcile_posture_observation(&self) -> Result<()> {
        let mut reservations =
            mutation_owner::MutationOwner::reserve_all(vec![self.inner.mutation_owner.clone()])
                .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        Ok(())
    }

    async fn install_known_group(&self, reservations: &[MutationReservation]) -> Result<()> {
        let known = MutationReservation::validate_group_known(reservations)?;
        for (reservation, saved) in reservations.iter().zip(&known) {
            let Some(saved) = saved else {
                continue;
            };
            if let KnownCommit::Metadata(metadata) = saved {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    bridge
                        .install_mutation_metadata(
                            reservation.agent_id(),
                            reservation.identity_incarnation(),
                            metadata,
                        )
                        .await?;
                }
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match self.inner.host_bridge.as_ref() {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                runtime.install_saved_mutation(reservation, saved).await?;
            }
        }
        Ok(())
    }

    // The slice must contain the whole pending group. No member becomes readable
    // until every saved installation has completed and all gates validate.
    pub(crate) async fn reconcile_reserved_mutations(
        &self,
        reservations: &mut [MutationReservation],
    ) -> Result<bool> {
        let version = self.inner.config_snapshot.load_full();
        super::input_recovery::resolve_input_recovery(
            &self.inner.runtime_db,
            reservations,
            &self.inner.input_evidence_budget,
            &version.base_context_config,
            &version.model_catalog,
        )?;
        super::control_recovery::resolve_control_recovery(
            &self.inner.runtime_db,
            reservations,
            self.current_context_config()
                .await
                .recent_episode_candidates,
            Some(self),
        )
        .await?;
        super::episode_recovery::resolve_episode_recovery(
            &self.inner.runtime_db,
            reservations,
            self.current_context_config()
                .await
                .recent_episode_candidates,
        )?;
        let known = MutationReservation::validate_group_known(reservations).map_err(|error| {
            match error {
                mutation_owner::MutationOwnerError::NeedsReconcile(key) => anyhow!(
                    "runtime commit outcome is unknown; no durable receipt proves the original operation {}", key.operation_id),
                other => other.into(),
            }
        })?;
        if known.iter().all(Option::is_none) {
            return Ok(false);
        }
        let mut notifications = Vec::new();
        let mut cancellations = Vec::new();
        for (reservation, saved) in reservations.iter().zip(&known) {
            let Some(saved) = saved else {
                continue;
            };
            if let Some(recovery) = reservation.recovery()? {
                tracing::warn!(agent_id = %recovery.key.agent_id,
                    operation_id = %recovery.key.operation_id, phase = ?recovery.phase,
                    original_outcome_known = true,
                    "restoring committed runtime mutation group without replay");
            }
            if let KnownCommit::Metadata(metadata) = saved {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    bridge
                        .install_mutation_metadata(
                            reservation.agent_id(),
                            reservation.identity_incarnation(),
                            metadata,
                        )
                        .await?;
                }
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match self.inner.host_bridge.as_ref() {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                if let Some(plan) = runtime.control_cancellation_for_known(reservation, saved)? {
                    cancellations.push((runtime.clone(), plan));
                }
                if let Some(effects) = runtime.install_saved_mutation(reservation, saved).await? {
                    notifications.push((runtime, effects));
                }
            }
        }
        let observations = MutationReservation::reconcile_all(reservations, known)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        for (runtime, plan) in cancellations {
            runtime.resume_saved_control_cancellation(&plan)?;
        }
        for (runtime, effects) in notifications {
            runtime.reissue_mutation_notifications(effects).await;
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) async fn prepare_known_host_installation_fixture(
        &self,
    ) -> Result<(AgentState, CancellationToken, String)> {
        let agent_id = self.agent_state().await?.id;
        let message = self
            .enqueue(MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: "known Host installation fixture".into(),
                },
            ))
            .await?;
        let token = CancellationToken::new();
        {
            let mut mutation = self.begin_mutation().await?;
            mutation.state.status = AgentStatus::AwakeRunning;
            mutation.state.current_run_id = Some("host-installation-fixture".into());
            mutation.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: "host-installation-fixture".into(),
                token: token.clone(),
                reason: Arc::new(StdMutex::new("fixture-active".into())),
            });
            mutation.persist_state(&self.inner.storage).await?;
        }
        let mut mutation = self.begin_mutation().await?;
        mutation.defer_abort_current_run("fixture-committed-abort");
        mutation.current_run_abort = None;
        mutation.queue.clear();
        mutation.state.pending = 0;
        mutation.state.status = AgentStatus::AwakeIdle;
        mutation.state.current_run_id = None;
        let expected_state = mutation.state.clone();
        let expected_queue = self
            .inner
            .runtime_db
            .queue_entries()
            .latest(&message.id)?
            .context("fixture queue entry is missing")?;
        let mut aborted_queue = expected_queue.clone();
        aborted_queue.status = QueueEntryStatus::Aborted;
        aborted_queue.updated_at = self.now();
        let event = AuditEvent::legacy("host_installation_fixture", serde_json::json!({}));
        let command = crate::runtime_db::transitions::QueueTransitionCommand {
            agent_id,
            operation: crate::runtime_db::transitions::QueueOperation::Discard,
            mutation: crate::runtime_db::transitions::QueueMutation::Upsert(aborted_queue),
            scheduler_claim_work_item: None,
            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(mutation.last_persisted_state.clone())),
                record: Box::new(expected_state.clone()),
            }),
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: vec![event.clone()],
            notify_scheduler: false,
            fault: Some(TransitionFaultPoint::BeforeCacheUpdate),
            brief_evidence: Vec::new(),
        };
        let result = mutation
            .commit_transition(event.id, |scope| {
                self.inner
                    .runtime_db
                    .transitions_in_scope(scope)
                    .commit_queue(&command)
            })
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => bail!("fixture must leave publication pending"),
        };
        anyhow::ensure!(
            error
                .to_string()
                .contains("committed but publication requires reconciliation"),
            "fixture must reach the known publication fault: {error:#}"
        );
        let recovery = mutation.reservations[mutation.primary_index]
            .recovery()?
            .context("fixture recovery facts are missing")?;
        anyhow::ensure!(
            recovery.committed_state.as_deref() == Some(&expected_state)
                && recovery.committed_runtime.is_some(),
            "fixture must retain exact known runtime facts"
        );
        Ok((expected_state, token, message.id))
    }

    #[cfg(test)]
    pub(crate) async fn host_installation_snapshot(&self) -> (AgentState, usize, bool) {
        let agent = self.inner.agent.lock().await;
        (
            agent.state.clone(),
            agent.queue.len(),
            agent.current_run_abort.is_none(),
        )
    }

    pub(crate) fn instance_agent_id(&self) -> &str {
        self.inner.mutation_owner.agent_id()
    }

    pub(crate) fn owns_mutation_reservation(&self, reservation: &MutationReservation) -> bool {
        reservation
            .validate_owner(&self.inner.mutation_owner)
            .is_ok()
    }

    pub(crate) async fn install_saved_mutation(
        &self,
        reservation: &MutationReservation,
        known: &KnownCommit,
    ) -> Result<Option<crate::runtime_db::transitions::PostCommitEffects>> {
        // Old-runtime handles and abort tokens do not cross an owner replacement.
        if reservation
            .validate_owner(&self.inner.mutation_owner)
            .is_err()
        {
            return Ok(None);
        }
        match known {
            KnownCommit::State {
                state,
                runtime: facts,
                ..
            } => {
                if let Some(facts) = facts
                    .as_ref()
                    .filter(|facts| facts.owner_epoch == reservation.owner_epoch())
                {
                    self.install_mutation_facts(facts).await?;
                    return Ok(Some(facts.effects.clone()));
                }
                self.install_exact_durable_readback(state).await?;
            }
            KnownCommit::Metadata(metadata) => {
                if let Some(state) = &metadata.state {
                    self.install_exact_durable_readback(state).await?;
                }
            }
        }
        Ok(None)
    }

    pub(crate) async fn reissue_mutation_notifications(
        &self,
        mut effects: crate::runtime_db::transitions::PostCommitEffects,
    ) {
        strip_authoritative_effects(&mut effects);
        if effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            effects.fault = None;
        }
        // Reissue only rediscoverable notifications, never business SQL.
        self.apply_transition_commit(TransitionCommit {
            applied: true,
            effects,
            delivery_receipt: None,
        })
        .await;
    }

    async fn install_exact_durable_readback(&self, state: &Arc<AgentState>) -> Result<()> {
        let mut agent = self.inner.agent.lock().await;
        anyhow::ensure!(
            agent.state == agent.last_persisted_state,
            "runtime recovery cannot discard unpersisted state"
        );
        anyhow::ensure!(
            agent
                .current_run_abort
                .as_ref()
                .is_none_or(|handle| { state.current_run_id.as_ref() == Some(&handle.run_id) }),
            "runtime recovery conflicts with a live run handle"
        );
        agent.state = state.as_ref().clone();
        agent.last_persisted_state = state.as_ref().clone();
        Ok(())
    }
}

fn validate_control_publication(
    delta: &crate::runtime_db::control_transition::ControlCommittedDelta,
    agent_id: &str,
) -> Result<()> {
    let effects = &delta.transition.effects;
    anyhow::ensure!(
        delta.state.id == agent_id
            && effects
                .agent_state
                .as_ref()
                .is_none_or(|change| change.record.as_ref() == &delta.state)
            && effects.tasks == delta.tasks
            && effects.timers == delta.timers
            && effects.external_triggers == delta.external_triggers
            && effects.messages == delta.result_messages
            && effects.turns == delta.turns
            && effects.execution_protocol == delta.execution_protocol,
        "Control publication differs from its canonical committed facts"
    );
    anyhow::ensure!(
        effects
            .removed_queue_message_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            == delta
                .queue_entries
                .iter()
                .map(|entry| &entry.message_id)
                .collect(),
        "Control publication omitted a canonical queue removal"
    );
    anyhow::ensure!(
        delta
            .waits
            .iter()
            .all(|wait| wait.agent_id == agent_id && wait.status == WaitConditionStatus::Cancelled)
            && delta
                .timer_wakes
                .iter()
                .all(|wake| wake.status == "cancelled")
            && delta
                .workspace_occupancy
                .as_ref()
                .is_none_or(|occupancy| occupancy.holder_agent_id == agent_id),
        "Control publication cancellation scope differs"
    );
    tracing::debug!(
        agent_id,
        waits = delta.waits.len(),
        timer_wakes = delta.timer_wakes.len(),
        workspace_occupancy = delta.workspace_occupancy.is_some(),
        "validated canonical Control cancellation facts"
    );
    Ok(())
}

fn trace_publication(token: PublicationToken) {
    tracing::trace!(
        identity_incarnation = token.identity_incarnation,
        owner_epoch = token.owner_epoch,
        publication_generation = token.generation,
        "published committed posture observation"
    );
}

#[cfg(test)]
#[path = "state_mutation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "state_mutation/context_preparation_tests.rs"]
mod context_preparation_tests;
