//! Short, reserved evidence transactions used by Host maintenance producers.

use super::*;
use crate::{
    runtime::{
        resident_input_evidence::{InputEvidenceScope, ResidentInputEvidence},
        resident_state::ResidentEpisodes,
        RuntimeMutationFacts,
    },
    runtime_db::{
        input_mutation_receipts::{InputMutationResult, OriginalInputMutationAttempt},
        prepared_input_evidence::{
            InputEvidenceWrite, PreparedEvidenceValue, PreparedExternalInputEvidence,
            PreparedExternalInputValue, PreparedInputRetention,
        },
        transitions::{InputMutationExecution, PostCommitEffects},
        RuntimeDbRetentionPolicy, RuntimeDbRetentionReport,
    },
    types::{AuditEvent, BriefRecord},
};

/// Maintenance ACKs deliberately cannot carry State, queue, or business
/// mutations. Those require their own compound owner installation protocol.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExternalInputEvidenceEffects {
    pub(crate) input_delta: crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta,
    pub(crate) work_delta: Option<crate::runtime::resident_work::CommittedWorkDelta>,
    pub(crate) task_delta: Option<crate::runtime::resident_task::CommittedTaskDelta>,
    pub(crate) task_material_budget_views:
        Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    pub(crate) briefs: Vec<BriefRecord>,
    pub(crate) audit_events: Vec<AuditEvent>,
    pub(crate) notify_memory_index: bool,
    pub(crate) notify_scheduler: bool,
}

impl From<ExternalInputEvidenceEffects> for PostCommitEffects {
    fn from(effects: ExternalInputEvidenceEffects) -> Self {
        Self {
            input_evidence_delta: effects.input_delta,
            work_delta: effects.work_delta,
            work_delta_requires_prepared_domain: true,
            task_delta: effects.task_delta,
            task_delta_requires_prepared_domain: true,
            task_material_budget_views: effects.task_material_budget_views,
            briefs: effects.briefs,
            audit_events: effects.audit_events,
            notify_memory_index: effects.notify_memory_index,
            notify_scheduler: effects.notify_scheduler,
            ..Default::default()
        }
    }
}

struct PreparedExternalInput {
    identity: IdentityMutationFacts,
    runtime: Option<RuntimeMutationFacts>,
    episodes: Option<Arc<ResidentEpisodes>>,
    inputs: Option<Arc<ResidentInputEvidence>>,
    work: Option<Arc<crate::runtime::resident_work::ResidentWorkDomain>>,
    tasks: Option<Arc<crate::runtime::resident_task::ResidentTaskDomain>>,
}

impl RuntimeHost {
    /// The typed original transaction is frozen before the complete owner group
    /// begins. Replays install current facts through the same fresh resolver.
    pub(crate) async fn commit_external_input_evidence(
        &self,
        reservations: &mut [MutationReservation],
        command: PreparedExternalInputEvidence,
    ) -> Result<InputMutationResult> {
        let identities = self.external_identity_facts(reservations)?;
        let scopes = reservations
            .iter()
            .map(|reservation| InputEvidenceScope {
                agent_id: reservation.agent_id().to_owned(),
                identity_incarnation: reservation.identity_incarnation(),
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            command.descriptor().scopes() == scopes,
            "external prepared input differs from the complete reserved group"
        );
        let _owned = match &command {
            PreparedExternalInputEvidence::BriefCreated(command) => {
                Some(self.inner.input_evidence_budget.register_owned_bytes(
                    command.primary_scope(),
                    crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
                    crate::runtime::input_evidence_budget::OwnedBytesEstimate {
                        payload_bytes: command.owned_payload_bytes()?,
                        metadata_estimate_bytes: 0,
                    },
                )?)
            }
            PreparedExternalInputEvidence::Retention(_) => None,
        };
        let mut prepared = Vec::with_capacity(reservations.len());
        for ((reservation, identity), scope) in reservations.iter().zip(identities).zip(&scopes) {
            anyhow::ensure!(
                identity.identity.as_ref().is_some_and(|identity| {
                    identity.agent_id == scope.agent_id
                        && identity.incarnation == scope.identity_incarnation
                        && identity.status == AgentRegistryStatus::Active
                }),
                "external input writer requires its current active identity"
            );
            let episodes = reservation.prepared_episodes_for_state_only_commit()?;
            let inputs = reservation.prepared_inputs_for_state_only_commit()?;
            let work = reservation.prepared_work_for_state_only_commit()?;
            let tasks = reservation.prepared_tasks_for_state_only_commit()?;
            anyhow::ensure!(
                tasks.as_ref().is_none_or(|tasks| tasks.scope() == scope),
                "prepared external Task has a different namespace"
            );
            anyhow::ensure!(
                work.as_ref().is_none_or(|work| work.scope() == scope),
                "prepared external Work has a different namespace"
            );
            anyhow::ensure!(
                inputs
                    .as_ref()
                    .is_none_or(|inputs| inputs.belongs_to(scope)),
                "prepared external input domain has a different namespace"
            );
            let loaded = self.bridge().runtime_for_mutation(reservation).await?;
            let runtime = if let Some(state) = identity.state.as_ref() {
                Some(match loaded {
                    Some(runtime) => {
                        runtime
                            .prepare_external_input_facts(
                                reservation,
                                state.clone(),
                                episodes.clone(),
                                inputs.clone(),
                            )
                            .await?
                    }
                    None => RuntimeMutationFacts::for_cold_external_input(
                        reservation.owner_epoch(),
                        state.clone(),
                        episodes.clone(),
                        inputs.clone(),
                    ),
                })
            } else {
                anyhow::ensure!(
                    loaded.is_none(),
                    "loaded evidence owner has no durable State"
                );
                None
            };
            prepared.push(PreparedExternalInput {
                identity,
                runtime,
                episodes,
                inputs,
                work,
                tasks,
            });
        }
        let original_runtime = prepared
            .iter()
            .filter_map(|scope| {
                scope
                    .runtime
                    .as_ref()
                    .map(|runtime| Arc::new(runtime.clone()))
            })
            .collect();
        let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
            command.descriptor().command_nonce().to_owned(),
            OriginalInputMutationAttempt::new(command.descriptor().clone()),
            Vec::new(),
        )?
        .with_input_runtime(original_runtime)?;
        let limits = self.inner.input_evidence_budget.limits();
        let work_request = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("external Work byte limit overflow")?,
            )?,
        };
        let task_request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: work_request.record_limit,
            byte_limit: work_request.byte_limit,
        };
        MutationReservation::begin_all_with_witness(
            reservations,
            witness.operation_id().to_owned(),
            witness.clone(),
        )?;
        let committed = self
            .runtime_db()
            .commit_prepared_external_input_evidence_with_task_budget(
                &command,
                &work_request,
                &task_request,
                &self.inner.input_evidence_budget,
            );
        if matches!(&committed, Ok(InputMutationExecution::Replayed { .. })) {
            witness.mark_input_storage_replayed()?;
        }
        witness.finish_original_attempt();
        let (result, effects) = match committed {
            Ok(InputMutationExecution::Replayed {
                receipt,
                _material_budget_views,
            }) => {
                self.reconcile_external_mutations(reservations).await?;
                return Ok(receipt.outcome.result);
            }
            Ok(InputMutationExecution::Committed { value, receipt }) => {
                let effects = match value {
                    PreparedExternalInputValue::BriefCreated(ack) => {
                        anyhow::ensure!(
                            ack.agent_state.is_none(),
                            "Host notice cannot change State"
                        );
                        let PreparedEvidenceValue::BriefCreated(actual) = ack.record else {
                            bail!("Host notice ACK has another evidence family");
                        };
                        let scope = command
                            .descriptor()
                            .scopes()
                            .iter()
                            .find(|scope| scope.agent_id == actual.brief.agent_id)
                            .context("Host notice ACK omitted its original owner")?
                            .clone();
                        vec![(
                            scope,
                            ExternalInputEvidenceEffects {
                                input_delta: ack.input_delta,
                                work_delta: ack.work_delta,
                                task_delta: ack.task_delta,
                                task_material_budget_views: ack.task_material_budget_views,
                                briefs: vec![actual.brief],
                                audit_events: if actual.event_inserted {
                                    vec![actual.event]
                                } else {
                                    Vec::new()
                                },
                                notify_memory_index: true,
                                notify_scheduler: true,
                            },
                        )]
                    }
                    PreparedExternalInputValue::Retention(ack) => {
                        let mut effects = std::collections::BTreeMap::new();
                        for (scope, input_delta) in ack.input_deltas {
                            let key = (scope.agent_id.clone(), scope.identity_incarnation);
                            anyhow::ensure!(
                                effects
                                    .insert(
                                        key,
                                        (
                                            scope,
                                            ExternalInputEvidenceEffects {
                                                input_delta,
                                                notify_memory_index: true,
                                                notify_scheduler: true,
                                                ..Default::default()
                                            }
                                        )
                                    )
                                    .is_none(),
                                "retention ACK repeated its Input scope"
                            );
                        }
                        for (scope, work_delta) in ack.work_deltas {
                            let key = (scope.agent_id.clone(), scope.identity_incarnation);
                            let (_, effect) = effects.entry(key).or_insert_with(|| {
                                (
                                    scope,
                                    ExternalInputEvidenceEffects {
                                        notify_memory_index: true,
                                        notify_scheduler: true,
                                        ..Default::default()
                                    },
                                )
                            });
                            anyhow::ensure!(
                                effect.work_delta.replace(work_delta).is_none(),
                                "retention ACK repeated its Work scope"
                            );
                        }
                        for (scope, task_delta, guards) in ack.task_deltas {
                            let key = (scope.agent_id.clone(), scope.identity_incarnation);
                            let (_, effect) = effects.entry(key).or_insert_with(|| {
                                (
                                    scope,
                                    ExternalInputEvidenceEffects {
                                        notify_memory_index: true,
                                        notify_scheduler: true,
                                        ..Default::default()
                                    },
                                )
                            });
                            anyhow::ensure!(
                                effect.task_delta.replace(task_delta).is_none(),
                                "retention ACK repeated its Task scope"
                            );
                            effect.task_material_budget_views.extend(guards);
                        }
                        effects.into_values().collect()
                    }
                };
                (receipt.outcome.result, effects)
            }
            Err(error) => {
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    MutationReservation::definitely_not_committed_all(reservations)?;
                }
                return Err(error);
            }
        };
        let mut per_scope = scopes
            .iter()
            .map(|_| ExternalInputEvidenceEffects::default())
            .collect::<Vec<_>>();
        let mut preparation_error = None;
        let mut seen = HashSet::new();
        for (scope, effects) in effects {
            if let Some(index) = scopes.iter().position(|expected| expected == &scope) {
                if !seen.insert(index) {
                    preparation_error =
                        Some(anyhow!("external input ACK repeats a reserved scope"));
                    continue;
                }
                per_scope[index] = effects;
            } else {
                preparation_error = Some(anyhow!("external input ACK has an unreserved scope"));
            }
        }
        let mut known = Vec::with_capacity(reservations.len());
        for (prepared, effects) in prepared.into_iter().zip(per_scope) {
            let effects: PostCommitEffects = effects.into();
            let Some(runtime) = prepared.runtime else {
                // An unmaterialized cold identity has no resident domain to
                // update. Notice producers reject this case before SQL, since
                // original audit effects require a durable State carrier.
                if !effects.audit_events.is_empty() {
                    preparation_error =
                        Some(anyhow!("cold audit publication requires durable State"));
                }
                known.push(KnownCommit::Metadata(Arc::new(prepared.identity)));
                continue;
            };
            let mut inputs = if let Some(inputs) = prepared.inputs {
                match inputs.with_committed(
                    &effects.input_evidence_delta,
                    inputs.mandatory_roots().clone(),
                ) {
                    Ok(inputs) => inputs.map(Arc::new),
                    Err(error) => {
                        preparation_error = Some(error);
                        None
                    }
                }
            } else {
                None
            };
            let mut work = match (prepared.work, effects.work_delta.as_ref()) {
                (Some(work), Some(delta)) => match work.with_committed(delta) {
                    Ok(work) => Some(Arc::new(work)),
                    Err(error) => {
                        preparation_error = Some(error);
                        None
                    }
                },
                (work, None) => work,
                (None, Some(_)) => None,
            };
            let tasks = match (prepared.tasks, effects.task_delta.as_ref()) {
                (Some(tasks), Some(delta)) => match tasks.with_committed(delta) {
                    Ok(tasks) => Some(Arc::new(tasks)),
                    Err(error) => {
                        preparation_error = Some(error);
                        None
                    }
                },
                (tasks, None) => tasks,
                (None, Some(_)) => None,
            };
            // A failed projection preparation stays explicitly unprepared;
            // it cannot erase our knowledge that SQL returned an exact ACK.
            let runtime = match runtime.clone().with_external_input_commit(
                inputs.clone(),
                work.clone(),
                tasks.clone(),
                effects.clone(),
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    preparation_error = Some(error);
                    inputs = None;
                    work = None;
                    runtime.with_external_input_commit(None, None, tasks.clone(), effects)?
                }
            };
            let runtime = Arc::new(runtime);
            known.push(KnownCommit::State {
                work,

                tasks,
                state: runtime.state.clone(),
                episodes: prepared.episodes,
                input_evidence: inputs,
                runtime: Some(runtime),
            });
        }
        // No await separates SQL ACK and saving the entire recovery group.
        MutationReservation::committed_all(reservations, known)?;
        if let Some(error) = preparation_error {
            return Err(error);
        }
        #[cfg(test)]
        if self
            .inner
            .external_publication_fault
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            bail!("external input mutation committed but publication requires reconciliation");
        }
        self.reconcile_external_mutations(reservations).await?;
        Ok(result)
    }

    pub(crate) async fn persist_cold_brief_evidence(&self, brief: BriefRecord) -> Result<()> {
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            let mut reservations = host
                .reserve_agent_mutations(&[brief.agent_id.clone()])
                .await?;
            anyhow::ensure!(
                host.external_identity_facts(&reservations)?[0]
                    .state
                    .is_some(),
                "cold Brief publication requires a durable State"
            );
            let event = crate::types::brief_created_event_for(&brief)?;
            let storage = host.agent_storage(&brief.agent_id)?;
            let scope = InputEvidenceScope {
                agent_id: reservations[0].agent_id().to_owned(),
                identity_incarnation: reservations[0].identity_incarnation(),
            };
            let command =
                PreparedExternalInputEvidence::brief_created(storage.prepare_input_evidence(
                    scope,
                    event.id.clone(),
                    brief.created_at,
                    InputEvidenceWrite::BriefCreated { brief, event },
                )?)?;
            host.commit_external_input_evidence(&mut reservations, command)
                .await
                .map(|_| ())
        })
        .await
    }

    pub(crate) async fn run_scoped_retention_pass(
        &self,
        policy: RuntimeDbRetentionPolicy,
        now: chrono::DateTime<Utc>,
    ) -> Result<RuntimeDbRetentionReport> {
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            let scopes = host
                .runtime_db()
                .retention_input_scopes(policy.clone(), now)?;
            let report = if scopes.is_empty() {
                // The original maintenance transaction revalidates that no
                // affected active namespace needs an owner reservation.
                host.runtime_db()
                    .commit_retention_pass_in_scopes(policy, now, &[])?
                    .committed
                    .report
            } else {
                let mut reservations = host
                    .reserve_mutation_scopes(
                        &scopes
                            .iter()
                            .map(|scope| (scope.agent_id.clone(), scope.identity_incarnation))
                            .collect::<Vec<_>>(),
                    )
                    .await?;
                let command =
                    PreparedExternalInputEvidence::Retention(PreparedInputRetention::new(
                        scopes,
                        crate::ids::audit_event_id(),
                        chrono::Utc::now(),
                        policy,
                        now,
                    )?);
                let result = host
                    .commit_external_input_evidence(&mut reservations, command)
                    .await?;
                let InputMutationResult::Retention { report } = result else {
                    bail!("retention ACK has another result family");
                };
                *report
            };
            // Vacuum/report I/O cannot repeat canonical deletion and never
            // holds an owner reservation across the longer maintenance task.
            let db = host.runtime_db().clone();
            tokio::task::spawn_blocking(move || db.complete_retention_maintenance(report))
                .await
                .context("retention maintenance worker stopped")?
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentState, BriefKind, BriefRecord};

    fn cold_host() -> (tempfile::TempDir, RuntimeHost, String) {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        config.user_home_dir = Some(home.path().to_path_buf());
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("unused")),
        )
        .unwrap();
        let identity = host.ensure_default_agent_identity().unwrap();
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&AgentState::new(&identity.agent_id))
            .unwrap();
        (home, host, identity.agent_id)
    }

    #[tokio::test]
    async fn cold_brief_known_publication_fault_replays_only_original_notifications() {
        let (_home, host, agent_id) = cold_host();
        let identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&agent_id, identity.incarnation)
            .unwrap();
        let mut receiver = host.subscribe_events();
        let brief = BriefRecord::new(&agent_id, BriefKind::Ack, "cold exact ACK", None, None);
        host.fail_next_external_mutation_publication();
        let error = host
            .persist_cold_brief_evidence(brief.clone())
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("committed but publication requires reconciliation"));
        assert!(owner.capture().is_err());
        assert!(receiver.try_recv().is_err());
        let actual = host
            .agent_storage(&agent_id)
            .unwrap()
            .read_brief_by_id(&brief.id)
            .unwrap()
            .unwrap();
        assert!(actual.created_event_seq.is_some());

        let reservations = host
            .reserve_agent_mutations(&[agent_id.clone()])
            .await
            .unwrap();
        drop(reservations);
        assert!(owner.capture().is_ok());
        let published = receiver.try_recv().unwrap();
        assert_eq!(published.event.kind, "brief_created");
        assert_eq!(Some(published.event.event_seq), actual.created_event_seq);
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            host.agent_storage(&agent_id)
                .unwrap()
                .read_recent_events(32)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "brief_created")
                .count(),
            1
        );
        assert!(
            host.inner.runtimes.read().await.agents.is_empty(),
            "a cold notice does not activate its agent"
        );
        host.persist_cold_brief_evidence(brief).await.unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "an observed original replay cannot publish again"
        );
        assert_eq!(
            host.runtime_db()
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            1
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn external_result_known_ack_preserves_work_until_original_publication_recovers() {
        use crate::runtime::resident_work::WorkCaptureRequest;
        let (_home, host, agent_id) = cold_host();
        let runtime = Box::pin(host.default_runtime()).await.unwrap();
        let work = Box::pin(runtime.create_work_item(
            "external Result owner".into(),
            None,
            None,
            Vec::new(),
        ))
        .await
        .unwrap();
        let identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&agent_id, identity.incarnation)
            .unwrap();
        let before = owner.capture().unwrap().unwrap();
        let mut brief = BriefRecord::new(
            &agent_id,
            BriefKind::Result,
            "external actual result",
            None,
            None,
        );
        brief.work_item_id = Some(work.id.clone());
        // Keep the actual reserved group until its Known facts are inspected;
        // the loaded run loop may otherwise reconcile immediately after return.
        let mut reservations = Box::pin(host.reserve_agent_mutations(&[agent_id.clone()]))
            .await
            .unwrap();
        let event = crate::types::brief_created_event_for(&brief).unwrap();
        let scope = InputEvidenceScope {
            agent_id: agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let command = PreparedExternalInputEvidence::brief_created(
            runtime
                .storage()
                .prepare_input_evidence(
                    scope,
                    event.id.clone(),
                    brief.created_at,
                    InputEvidenceWrite::BriefCreated {
                        brief: brief.clone(),
                        event,
                    },
                )
                .unwrap(),
        )
        .unwrap();
        host.fail_next_external_mutation_publication();
        let publication_error =
            Box::pin(host.commit_external_input_evidence(&mut reservations, command))
                .await
                .unwrap_err();
        assert!(
            owner.capture().is_err(),
            "external Result error before Known publication: {publication_error:#}"
        );
        {
            let recovery = reservations[0].recovery().unwrap().unwrap();
            let KnownCommit::State {
                work: Some(known), ..
            } = recovery.known.unwrap()
            else {
                panic!("known external Result ACK omitted Work");
            };
            let captured = known.capture(&WorkCaptureRequest::current()).unwrap();
            assert_eq!(
                captured.latest_results[&work.id].as_ref().unwrap().brief_id,
                brief.id
            );
        }
        Box::pin(host.reconcile_external_mutations(&mut reservations))
            .await
            .unwrap();
        drop(reservations);
        let after = owner.capture().unwrap().unwrap();
        let captured = after
            .work
            .as_ref()
            .unwrap()
            .capture(&WorkCaptureRequest::current())
            .unwrap();
        assert_eq!(
            captured.latest_results[&work.id].as_ref().unwrap().brief_id,
            brief.id
        );
        assert!(before
            .work
            .as_ref()
            .unwrap()
            .capture(&WorkCaptureRequest::current())
            .unwrap()
            .latest_results[&work.id]
            .is_none());
        Box::pin(host.persist_cold_brief_evidence(brief))
            .await
            .unwrap();
        assert_eq!(
            runtime.storage().latest_work_item(&work.id).unwrap(),
            Some(work)
        );
        Box::pin(host.shutdown()).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_cold_brief_caller_leaves_owned_publication_to_host_shutdown() {
        let (_home, host, agent_id) = cold_host();
        let identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&agent_id, identity.incarnation)
            .unwrap();
        let reservation = owner.reserve().await.unwrap();
        let brief = BriefRecord::new(&agent_id, BriefKind::Ack, "cold owned ACK", None, None);
        let caller_host = host.clone();
        let caller_brief = brief.clone();
        let caller =
            tokio::spawn(
                async move { caller_host.persist_cold_brief_evidence(caller_brief).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !host
                    .inner
                    .external_mutation_tasks
                    .lock()
                    .unwrap()
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(reservation);
        host.shutdown().await.unwrap();
        let actual = host
            .agent_storage(&agent_id)
            .unwrap()
            .read_brief_by_id(&brief.id)
            .unwrap()
            .unwrap();
        assert!(actual.created_event_seq.is_some());
        assert_eq!(
            host.agent_storage(&agent_id)
                .unwrap()
                .read_recent_events(32)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "brief_created")
                .count(),
            1
        );
        assert!(owner.capture().is_ok());
    }

    #[tokio::test]
    async fn retention_known_publication_fault_preserves_the_complete_scope_group() {
        let (_home, host, first) = cold_host();
        let second_identity = AgentIdentityRecord::new(
            "retention-peer",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.runtime_db()
            .agent_identities()
            .upsert(&second_identity)
            .unwrap();
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&AgentState::new(&second_identity.agent_id))
            .unwrap();
        let scopes = [&first, &second_identity.agent_id]
            .into_iter()
            .map(|agent_id| InputEvidenceScope {
                agent_id: agent_id.clone(),
                identity_incarnation: 1,
            })
            .collect::<Vec<_>>();
        let owners = scopes
            .iter()
            .map(|scope| {
                host.inner
                    .mutation_owners
                    .current_owner(&scope.agent_id, scope.identity_incarnation)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let now = Utc::now();
        let mut old_ids = Vec::new();
        let mut live_ids = Vec::new();
        for scope in &scopes {
            for old in [true, false] {
                let mut entry = TranscriptEntry::new(
                    &scope.agent_id,
                    crate::types::TranscriptEntryKind::IncomingMessage,
                    None,
                    None,
                    json!({}),
                );
                entry.created_at = if old {
                    now - chrono::Duration::days(100)
                } else {
                    now
                };
                host.runtime_db()
                    .transcript_entries()
                    .append_in_scope(scope, &entry)
                    .unwrap();
                if old {
                    old_ids.push(entry.id);
                } else {
                    live_ids.push(entry.id);
                }
            }
        }
        let policy = RuntimeDbRetentionPolicy {
            enabled: true,
            transcript_entries_min_rows: 1,
            tool_executions_min_rows: 1,
            ..Default::default()
        };
        host.fail_next_external_mutation_publication();
        let error = host
            .run_scoped_retention_pass(policy, now)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("committed but publication requires reconciliation"));
        assert!(owners.iter().all(|owner| owner.capture().is_err()));
        for id in &old_ids {
            assert!(host
                .runtime_db()
                .transcript_entries()
                .by_id(None, id)
                .unwrap()
                .is_none());
        }
        for id in &live_ids {
            assert!(host
                .runtime_db()
                .transcript_entries()
                .by_id(None, id)
                .unwrap()
                .is_some());
        }
        let reservations = host
            .reserve_mutation_scopes(
                &scopes
                    .iter()
                    .map(|scope| (scope.agent_id.clone(), scope.identity_incarnation))
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
        assert!(owners.iter().all(|owner| owner.capture().is_ok()));
        for (scope, old_id) in scopes.iter().zip(&old_ids) {
            let unavailable: bool = host.runtime_db().connection().unwrap().query_row(
                "SELECT availability='pruned' FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='transcript' AND record_id=?3",
                rusqlite::params![scope.agent_id, scope.identity_incarnation, old_id], |row| row.get(0)).unwrap();
            assert!(unavailable);
        }
        drop(reservations);
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retention_empty_inventory_protects_deleting_evidence_and_preserves_unknown_gate() {
        let (_home, host, agent_id) = cold_host();
        let mut identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let scope = InputEvidenceScope {
            agent_id: agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let now = Utc::now();
        let mut old_id = String::new();
        for old in [true, false] {
            let mut entry = TranscriptEntry::new(
                &agent_id,
                crate::types::TranscriptEntryKind::IncomingMessage,
                None,
                None,
                json!({}),
            );
            entry.created_at = if old {
                now - chrono::Duration::days(100)
            } else {
                now
            };
            host.runtime_db()
                .transcript_entries()
                .append_in_scope(&scope, &entry)
                .unwrap();
            if old {
                old_id = entry.id;
            }
        }
        let owner = host
            .bridge()
            .mutation_owner_for(&agent_id, identity.incarnation)
            .unwrap();
        // Model a deletion transaction that changed durable identity but lost its
        // ACK. Retention must neither guess that outcome nor touch its gate.
        let key = {
            let mut reservation = owner.reserve().await.unwrap();
            let key = reservation
                .begin("retention-original-unknown".into())
                .unwrap();
            identity.status = AgentRegistryStatus::Deleting;
            identity.updated_at += chrono::Duration::milliseconds(1);
            host.runtime_db()
                .agent_identities()
                .upsert(&identity)
                .unwrap();
            key
        };
        let policy = RuntimeDbRetentionPolicy {
            enabled: true,
            transcript_entries_min_rows: 1,
            tool_executions_min_rows: 1,
            ..Default::default()
        };
        assert!(host
            .runtime_db()
            .retention_input_scopes(policy.clone(), now)
            .unwrap()
            .is_empty());
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            host.run_scoped_retention_pass(policy, now),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.transcript_entries.deleted_rows, 0);
        assert_eq!(
            report
                .transcript_entries
                .protection_reasons
                .get("identity_deleting"),
            Some(&1)
        );
        assert!(host
            .runtime_db()
            .transcript_entries()
            .by_id(None, &old_id)
            .unwrap()
            .is_some());
        let availability: String = host.runtime_db().connection().unwrap().query_row(
            "SELECT availability FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='transcript' AND record_id=?3",
            rusqlite::params![agent_id, identity.incarnation, old_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(availability, "present");
        assert!(
            matches!(owner.capture(), Err(MutationOwnerError::NeedsReconcile(pending)) if pending == key)
        );
        {
            let reservation = owner.reserve().await.unwrap();
            let recovery = reservation.recovery().unwrap().unwrap();
            assert_eq!(recovery.key, key);
            assert!(recovery.known.is_none());
            assert!(recovery.witness.is_none());
        }
        host.shutdown().await.unwrap();
        assert!(
            matches!(owner.capture(), Err(MutationOwnerError::NeedsReconcile(pending)) if pending == key)
        );
    }

    #[tokio::test]
    async fn external_prepared_group_mismatch_is_rejected_before_sql() {
        let (_home, host, agent_id) = cold_host();
        let identity = host.agent_identity_record(&agent_id).unwrap().unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&agent_id, identity.incarnation)
            .unwrap();
        let mut reservations = host
            .reserve_agent_mutations(&[agent_id.clone()])
            .await
            .unwrap();
        let brief = BriefRecord::new(
            &agent_id,
            BriefKind::Ack,
            "wrong original group",
            None,
            None,
        );
        let event = crate::types::brief_created_event_for(&brief).unwrap();
        let storage = host.agent_storage(&agent_id).unwrap();
        let mut receiver = host.subscribe_events();
        let command = PreparedExternalInputEvidence::brief_created(
            storage
                .prepare_input_evidence(
                    InputEvidenceScope {
                        agent_id: agent_id.clone(),
                        identity_incarnation: identity.incarnation + 1,
                    },
                    event.id.clone(),
                    chrono::Utc::now(),
                    InputEvidenceWrite::BriefCreated { brief, event },
                )
                .unwrap(),
        )
        .unwrap();
        let error = host
            .commit_external_input_evidence(&mut reservations, command)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("complete reserved group"));
        assert!(owner.capture().is_ok());
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            host.runtime_db()
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        drop(reservations);
        host.shutdown().await.unwrap();
    }
}
