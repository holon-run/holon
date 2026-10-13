//! Named Work command preparation; ordinary selectors read one captured view.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_work::{CanonicalWorkRecord, CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey},
};

#[derive(Clone, Copy)]
pub(super) enum WorkCommandReadMode {
    Focus { clear_blocker: bool },
    CompletionIntent,
    Completion,
}

pub(super) struct WorkCommandRead {
    pub state: Arc<AgentState>,
    pub work: CapturedWork,
    pub queue: Option<crate::work_item_scheduling::WorkQueueReadModel>,
    // Explicit execution-protocol command preparation adapter, separate from
    // resident Work/Wait/Frame reads. No SQL handle escapes this observation.
    pub protocol: Option<crate::domain::execution_protocol::ExecutionProtocolState>,
    pub owned: Arc<BudgetViewGuard>,
}
impl WorkCommandRead {
    pub fn active_frames(&self) -> Result<Vec<&WorkItemContinuationFrame>> {
        anyhow::ensure!(
            self.work.live_complete,
            "Work command live frontier is incomplete"
        );
        let mut frames = self
            .work
            .active_frame_ids
            .iter()
            .map(|id| {
                let key = WorkKey::new(WorkFamily::Frame, id);
                let value = self
                    .work
                    .lookup(&key)?
                    .context("active continuation is absent")?;
                match &value.record {
                    CanonicalWorkRecord::Frame(frame)
                        if frame.state == WorkItemContinuationState::Active =>
                    {
                        Ok(frame)
                    }
                    _ => Err(anyhow!(
                        "active continuation frontier differs from its record"
                    )),
                }
            })
            .collect::<Result<Vec<_>>>()?;
        frames.sort_by(|left, right| {
            right
                .updated_at
                .timestamp_millis()
                .cmp(&left.updated_at.timestamp_millis())
                .then_with(|| {
                    right
                        .created_at
                        .timestamp_millis()
                        .cmp(&left.created_at.timestamp_millis())
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(frames)
    }
    pub fn continuation_resume_source(
        &self,
        parent_work_item_id: &str,
        projected_parent: Option<&WorkItemRecord>,
        projected_wait_conditions: &[WaitConditionRecord],
    ) -> Result<(
        crate::domain::execution_protocol::WorkItemContinuationResumeSource,
        crate::domain::execution_protocol::WorkItemOutcome,
    )> {
        use crate::domain::execution_protocol::{
            continuation_resume_outcome, WorkItemContinuationResumeSource,
        };
        let durable_parent = self
            .work
            .work_item(parent_work_item_id)?
            .context("continuation parent WorkItem is missing")?;
        let parent = projected_parent
            .filter(|parent| parent.id == parent_work_item_id)
            .unwrap_or(durable_parent);
        anyhow::ensure!(
            parent.agent_id == self.state.id && parent.state == WorkItemState::Open,
            "continuation parent WorkItem is not open for this agent"
        );
        let mut parent_waits = self
            .active_waits_for(parent_work_item_id)?
            .into_iter()
            .map(|wait| wait.id.clone())
            .collect::<BTreeSet<_>>();
        for wait in projected_wait_conditions {
            parent_waits.remove(&wait.id);
            if wait.agent_id == self.state.id
                && wait.work_item_id.as_deref() == Some(parent_work_item_id)
                && wait.status == WaitConditionStatus::Active
            {
                parent_waits.insert(wait.id.clone());
            }
        }
        let source = WorkItemContinuationResumeSource {
            work_item_revision: parent.revision,
            blocked_by: parent.blocked_by.clone(),
            active_wait_ids: parent_waits.into_iter().collect(),
        };
        let outcome = continuation_resume_outcome(parent_work_item_id, &source)
            .map_err(|error| anyhow!(error))?;
        Ok((source, outcome))
    }
    pub fn active_waits_for(&self, work_item_id: &str) -> Result<Vec<&WaitConditionRecord>> {
        Ok(self
            .work
            .unresolved_wait_conditions()?
            .into_iter()
            .filter(|wait| {
                wait.status == WaitConditionStatus::Active
                    && wait.work_item_id.as_deref() == Some(work_item_id)
            })
            .collect())
    }
}
// An original StartedTurn or frozen completion intent, rather than a target
// Work's qualification alone, permits observing same-incarnation cleanup.
fn validate_completion_read_admission_tx(
    tx: &rusqlite::Transaction<'_>,
    scope: &super::super::resident_input_evidence::InputEvidenceScope,
    state: &AgentState,
    target: &WorkItemRecord,
) -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    let current = state.current_execution_binding.as_ref().filter(|binding| {
        binding.work_item_id.as_deref() == Some(target.id.as_str())
            && state.current_turn_id.as_deref() == Some(binding.turn_id.as_str())
            && state.current_run_id.is_some()
    });
    let (source, turn_id) = if let Some(binding) = current {
        (binding.source_message_id.as_str(), binding.turn_id.as_str())
    } else {
        let intent = target
            .completion_intent
            .as_ref()
            .filter(|intent| {
                target.state == WorkItemState::Completing && intent.work_item_id == target.id
            })
            .context("Deleting completion read has no original dispatch or completion intent")?;
        (
            intent
                .source_message_id
                .as_deref()
                .context("original completion source missing")?,
            intent
                .source_turn_id
                .as_deref()
                .context("original completion Turn missing")?,
        )
    };
    // Bounded addresses only: no source/Turn body is cloned by this observation.
    // Qualification below still verifies the canonical body/digest and namespace.
    let actual: bool = tx.query_row(
        "SELECT EXISTS(
          SELECT 1 FROM turn_records t
          JOIN conversation_input_assignments a ON a.turn_id=t.turn_id AND a.agent_id=t.agent_id
          JOIN messages m ON m.message_id=a.message_id AND m.agent_id=a.agent_id
          WHERE t.agent_id=?1 AND t.turn_id=?2 AND a.message_id=?3
            AND t.current_work_item_id=?4 AND json_extract(t.payload_json,'$.current_work_item_id')=?4
            AND json_extract(t.payload_json,'$.agent_id')=t.agent_id
            AND json_extract(t.payload_json,'$.turn_id')=t.turn_id
            AND json_extract(t.payload_json,'$.turn_index')=t.turn_index
            AND json_extract(t.payload_json,'$.run_id') IS t.run_id
            AND t.trigger_message_id=?3 AND json_extract(t.payload_json,'$.trigger.message_id')=?3
            AND EXISTS(SELECT 1 FROM json_each(t.payload_json,'$.input_message_ids') WHERE value=?3)
            AND m.evidence_id=m.message_id AND json_extract(m.payload_json,'$.id')=m.message_id
            AND json_extract(m.payload_json,'$.agent_id')=m.agent_id AND m.message_seq IS NOT NULL
            AND json_extract(m.payload_json,'$.message_seq')=m.message_seq
            AND (?5=0 OR (t.run_id IS ?6 AND t.turn_index=?7
              AND t.terminal_kind IS NULL AND json_type(t.payload_json,'$.terminal') IS NULL)))",
        rusqlite::params![scope.agent_id, turn_id, source, target.id,
            current.is_some(), state.current_run_id, state.turn_index],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        actual,
        "Deleting completion read differs from its actual admitted Work Turn"
    );
    crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
        tx,
        scope,
        BTreeSet::from([
            Key::new(Family::Message, source),
            Key::new(Family::Turn, turn_id),
        ]),
        BTreeSet::new(),
    )?;
    Ok(())
}

impl RuntimeHandle {
    #[inline(never)]
    pub(super) fn prepare_work_command_read<'a>(
        &'a self,
        target: &'a WorkItemRecord,
        mode: WorkCommandReadMode,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<WorkCommandRead>> + Send + 'a>>
    {
        Box::pin(async move {
            let target_scope = super::super::resident_input_evidence::InputEvidenceScope {
                agent_id: target.agent_id.clone(),
                identity_incarnation: self.instance_incarnation(),
            };
            let _target_owned = self.inner.input_evidence_budget.register_owned_bytes(
                &target_scope,
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: serialized_bytes(target)?,
                    metadata_estimate_bytes: 0,
                },
            )?;
            let mut owner = self.begin_mutation().await?;
            anyhow::ensure!(
                target.agent_id == owner.state.id,
                "Work command target scope differs"
            );
            let mut request = WorkCaptureRequest::current();
            if !matches!(mode, WorkCommandReadMode::Focus { .. }) {
                request.nonopen_limit = 0;
            }
            request
                .mandatory_roots
                .insert(WorkKey::new(WorkFamily::Work, &target.id));
            owner.prepare_owner_work_request(&request).await?;
            let read = owner.capture_work_read(&request)?;
            let actual = read
                .work
                .work_item(&target.id)?
                .context("Work command target is absent")?;
            if actual != target {
                return Err(RuntimeError::new(
                    RuntimeErrorDomain::Conflict,
                    "work_item_changed_during_preparation",
                    "the target WorkItem changed before command preparation",
                )
                .with_safe_context("work_item_id", &target.id)
                .into());
            }
            let (triggers, protocol) = self.inner.runtime_db.transaction(|tx| {
                let durable =
                    crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &read.state.id);
                let facts = durable.scope_facts()?;
                anyhow::ensure!(
                    facts.state.as_ref() == Some(read.state.as_ref())
                        && facts.control_revision.is_some()
                        && facts
                            .identity
                            .as_ref()
                            .is_some_and(|identity| identity.agent_id == read.work.scope.agent_id
                                && identity.incarnation == read.work.scope.identity_incarnation),
                    "Work command canonical scope/State differs from its captured root"
                );
                if !matches!(mode, WorkCommandReadMode::Focus { .. })
                    && facts.identity.as_ref().is_some_and(|identity| {
                        identity.status == crate::types::AgentRegistryStatus::Deleting
                    })
                {
                    validate_completion_read_admission_tx(
                        tx,
                        &read.work.scope,
                        &read.state,
                        target,
                    )?;
                    let proof = crate::runtime_db::work_snapshot::prepare_original_work_write_tx(
                        tx,
                        &read.work.scope,
                        &BTreeSet::from([WorkKey::new(WorkFamily::Work, &target.id)]),
                    )?;
                    proof.validate_domain_observation_tx(tx, read.work.revision)?;
                } else {
                    crate::runtime_db::work_snapshot::validate_work_domain_observation_tx(
                        tx,
                        &read.work.scope,
                        read.work.revision,
                    )?;
                }
                match mode {
                    WorkCommandReadMode::Focus { clear_blocker } => {
                        let needs_protocol = clear_blocker
                            || !read.work.active_frame_ids.is_empty()
                            || read
                                .state
                                .current_work_item_id
                                .as_deref()
                                .is_some_and(|id| id != target.id);
                        Ok((
                            durable.external_triggers()?,
                            if needs_protocol {
                                durable.execution_protocol_state(&read.state.id)?
                            } else {
                                None
                            },
                        ))
                    }
                    WorkCommandReadMode::CompletionIntent => Ok((Vec::new(), None)),
                    WorkCommandReadMode::Completion => Ok((
                        Vec::new(),
                        durable.execution_protocol_state(&read.state.id)?,
                    )),
                }
            })?;
            // Reserve before any selector clones the Work bodies. This covers
            // the command, cancelled Waits, frame expectations/final records,
            // returned prepared plan and its bounded commit-time copies.
            let body_bytes =
                read.work
                    .records
                    .iter()
                    .try_fold(0u64, |total, record| -> Result<u64> {
                        Ok(total.saturating_add(serialized_bytes(&record.record)?))
                    })?;
            let owned = self.inner.input_evidence_budget.register_owned_bytes(
                &read.work.scope,
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: body_bytes
                        .saturating_mul(8)
                        .saturating_add(serialized_bytes(read.state.as_ref())?.saturating_mul(4))
                        .saturating_add(serialized_bytes(&protocol)?.saturating_mul(3))
                        .saturating_add(serialized_bytes(&triggers)?.saturating_mul(2)),
                    metadata_estimate_bytes: 0,
                },
            )?;
            let queue = match mode {
                WorkCommandReadMode::Focus { .. } => {
                    Some(read.work.work_queue(&read.state, triggers)?)
                }
                WorkCommandReadMode::CompletionIntent | WorkCommandReadMode::Completion => None,
            };
            drop(owner);
            Ok(WorkCommandRead {
                state: read.state,
                work: read.work,
                queue,
                protocol,
                owned,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::resident_input_evidence::InputEvidenceScope,
        runtime_db::{
            evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
            input_evidence_snapshot::{
                register_committed_input_evidence_tx, CanonicalInputEvidence,
            },
            repositories::{
                committed_turn_record_tx, insert_new_work_item_tx, upsert_turn_record_tx,
            },
            work_snapshot::{
                prepare_work_domain_snapshot_tx, prepare_work_write_tx, register_work_writes_tx,
                WorkDomainLoadRequest,
            },
            RuntimeDb,
        },
        types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset,
            AgentRegistryStatus, AgentVisibility, AuthorityClass, MessageBody, MessageKind,
            MessageOrigin, Priority, TurnOwner, TurnRecord, TurnTriggerSummary,
            WorkItemExecutionBinding,
        },
    };

    #[test]
    fn completion_cleanup_observation_requires_actual_original_source() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "completion-observation".into(),
            identity_incarnation: 1,
        };
        let mut identity = AgentIdentityRecord::new(
            &scope.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let mut target =
            WorkItemRecord::new(&scope.agent_id, "original target", WorkItemState::Open);
        target.id = "original-target".into();
        let mut unrelated = WorkItemRecord::new(
            &scope.agent_id,
            "another qualified Work",
            WorkItemState::Open,
        );
        unrelated.id = "unrelated-target".into();
        let state = db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            let state = AgentState::new(&scope.agent_id);
            upsert_agent_state_tx(tx, &state)?;
            let request = WorkDomainLoadRequest {
                capture: WorkCaptureRequest::current(),
                record_limit: 1024,
                byte_limit: 1024 * 1024,
            };
            prepare_work_domain_snapshot_tx(tx, &scope, &request)?.require_current()?;
            let keys = BTreeSet::from([
                WorkKey::new(WorkFamily::Work, &target.id),
                WorkKey::new(WorkFamily::Work, &unrelated.id),
            ]);
            let proof = prepare_work_write_tx(tx, &scope, &keys)?;
            insert_new_work_item_tx(tx, &target)?;
            insert_new_work_item_tx(tx, &unrelated)?;
            register_work_writes_tx(tx, &proof, &keys, &BTreeSet::new(), &state, &request)?;
            let source = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "finish original Work".into(),
                },
            );
            let (source, inserted) = append_message_tx(tx, &source)?;
            assert!(inserted && source.message_seq.is_some());
            let mut turn = TurnRecord::new(&scope.agent_id, "actual-original-turn", 1);
            turn.run_id = Some("original-run".into());
            turn.current_work_item_id = Some(target.id.clone());
            turn.owner = Some(TurnOwner::WorkItem {
                work_item_id: target.id.clone(),
            });
            turn.trigger = Some(TurnTriggerSummary::from_message(&source));
            turn.input_message_ids.push(source.id.clone());
            upsert_turn_record_tx(tx, &turn)?;
            let actual_turn = committed_turn_record_tx(tx, &scope.agent_id, &turn.turn_id)?.record;
            let mut state = state;
            state.turn_index = 1;
            state.total_message_count = 1;
            state.current_turn_id = Some(turn.turn_id.clone());
            state.current_run_id = turn.run_id.clone();
            state.current_execution_binding = Some(WorkItemExecutionBinding {
                activation_id: None,
                admission_provenance: None,
                source_message_id: source.id.clone(),
                turn_id: turn.turn_id,
                owner: turn.owner,
                work_item_id: Some(target.id.clone()),
                claimed_work_revision: Some(target.revision),
            });
            upsert_agent_state_tx(tx, &state)?;
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[
                    CanonicalInputEvidence::Message(source),
                    CanonicalInputEvidence::Turn(actual_turn),
                ],
            )?;
            Ok(state)
        })?;
        identity.status = AgentRegistryStatus::Deleting;
        db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            validate_completion_read_admission_tx(tx, &scope, &state, &target)?;
            assert!(validate_completion_read_admission_tx(tx, &scope, &state, &unrelated).is_err());
            let mut no_admission = state.clone();
            no_admission.current_execution_binding = None;
            assert!(
                validate_completion_read_admission_tx(tx, &scope, &no_admission, &target).is_err()
            );
            let mut fresh = target.clone();
            fresh.id = "new-work-after-deleting".into();
            assert!(validate_completion_read_admission_tx(tx, &scope, &state, &fresh).is_err());
            let mut forged = state.clone();
            forged
                .current_execution_binding
                .as_mut()
                .unwrap()
                .work_item_id = Some(unrelated.id.clone());
            assert!(
                validate_completion_read_admission_tx(tx, &scope, &forged, &unrelated).is_err()
            );
            let mut successor = identity.clone();
            successor.incarnation += 1;
            upsert_agent_identity_tx(tx, &successor)?;
            assert!(validate_completion_read_admission_tx(tx, &scope, &state, &target).is_err());
            Ok(())
        })?;
        Ok(())
    }
}
