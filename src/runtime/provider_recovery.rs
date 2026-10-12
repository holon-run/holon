//! Durable source resolution shared by recovery admission and context assembly.
#[cfg(test)]
use crate::storage::AppStorage;
use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};

use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionProtocolState,
        ExecutionSourceIdentity,
    },
    types::{MessageEnvelope, TurnOwner, TurnRecord, TurnTerminalKind},
};

/// Typed, provenance-checked raw source IDs; callers resolve records coherently.
pub(crate) fn continuity_sources(
    message: &MessageEnvelope,
) -> Result<Option<(String, String, TurnTerminalKind)>> {
    if !super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message) {
        return Ok(None);
    }
    let selection = super::turn::TurnModelSelection::from_message(message)?;
    let directive = selection
        .recovery
        .ok_or_else(|| anyhow!("provider recovery directive missing"))?;
    if directive.source_turn_id.is_empty()
        || directive.source_message_id.is_empty()
        || !matches!(
            directive.source_terminal_kind,
            TurnTerminalKind::DeferredToFallback | TurnTerminalKind::ProviderFailedNeedsRecovery
        )
        || message.source_refs.get("source_turn_id") != Some(&directive.source_turn_id)
        || message.source_refs.get("source_message_id") != Some(&directive.source_message_id)
        || message.causation_id.as_deref() != Some(directive.source_message_id.as_str())
    {
        bail!("provider recovery continuity source references mismatch");
    }
    Ok(Some((
        directive.source_turn_id,
        directive.source_message_id,
        directive.source_terminal_kind,
    )))
}

/// Recheck an earlier authority rejection against the newly captured owner
/// view. None means the source is now valid and the scheduler must replan.
pub(crate) fn invalid_source_reason_from_captured(
    inputs: &super::resident_input_evidence::CapturedInputEvidence,
    message: &MessageEnvelope,
) -> Result<Option<String>> {
    let state = inputs
        .execution_state_if_initialized()?
        .unwrap_or_else(|| ExecutionProtocolState::empty(message.agent_id.clone()));
    match resolve_from_captured(inputs, message, &state) {
        Ok(_) => Ok(None),
        Err(error) if is_preparation_error(&error) => Err(error),
        Err(error) => Ok(Some(error.to_string())),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderRecoverySource {
    pub predecessor: ExecutionAttempt,
    pub source_turn: TurnRecord,
    pub root_message: MessageEnvelope,
}

#[cfg(test)]
pub(crate) fn resolve(
    storage: &AppStorage,
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
) -> Result<ProviderRecoverySource> {
    resolve_with_sources(
        message,
        state,
        |id| storage.read_message_by_id(id),
        |id| storage.read_turn_by_id(id),
    )
}

/// Shared authority/lineage validation over acquisition's read-only capability.
#[cfg(test)]
pub(crate) fn resolve_with_sources(
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
    read_message: impl FnMut(&str) -> Result<Option<MessageEnvelope>>,
    read_turn: impl FnMut(&str) -> Result<Option<TurnRecord>>,
) -> Result<ProviderRecoverySource> {
    // Preserve the existing admission/recovery contract outside context.
    resolve_with_source_relation(
        message,
        state,
        read_message,
        read_turn,
        |source, turn| Ok(source.turn_id.as_deref() == Some(turn.turn_id.as_str())),
        |_, _| Ok(()),
        |_| Ok(()),
    )
}

fn resolve_with_source_relation(
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
    mut read_message: impl FnMut(&str) -> Result<Option<MessageEnvelope>>,
    mut read_turn: impl FnMut(&str) -> Result<Option<TurnRecord>>,
    mut belongs_to_turn: impl FnMut(&MessageEnvelope, &TurnRecord) -> Result<bool>,
    mut require_complete_candidates: impl FnMut(&str, &str) -> Result<()>,
    mut validate_source_presence: impl FnMut(&MessageEnvelope) -> Result<()>,
) -> Result<ProviderRecoverySource> {
    let mut cursor = message.clone();
    let mut seen = BTreeSet::new();
    let mut first = None;
    let mut expected_binding = None;
    loop {
        if seen.len() >= 64 || !seen.insert(cursor.id.clone()) {
            bail!("provider recovery lineage is cyclic or exceeds its bound");
        }
        let selection = super::turn::TurnModelSelection::from_message(&cursor)
            .map_err(|_| anyhow!("provider recovery directive malformed"))?;
        let directive = selection
            .recovery
            .as_ref()
            .ok_or_else(|| anyhow!("provider recovery directive missing"))?;
        if state.agent_id != message.agent_id
            || cursor.agent_id != message.agent_id
            || !matches!(
                directive.source_terminal_kind,
                TurnTerminalKind::DeferredToFallback
                    | TurnTerminalKind::ProviderFailedNeedsRecovery
            )
            || cursor.source_refs.get("source_turn_id") != Some(&directive.source_turn_id)
            || cursor.source_refs.get("source_message_id") != Some(&directive.source_message_id)
            || cursor.causation_id.as_deref() != Some(directive.source_message_id.as_str())
        {
            bail!("provider recovery source references mismatch");
        }
        validate_source_presence(&cursor)?;
        let source_message = read_message(&directive.source_message_id)?
            .ok_or_else(|| anyhow!("provider recovery source message missing"))?;
        let source_turn = read_turn(&directive.source_turn_id)?
            .ok_or_else(|| anyhow!("provider recovery source turn missing"))?;
        if source_message.agent_id != message.agent_id
            || !belongs_to_turn(&source_message, &source_turn)?
            || source_turn.agent_id != message.agent_id
            || source_turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(source_message.id.as_str())
            || source_turn.terminal.as_ref().map(|terminal| terminal.kind)
                != Some(directive.source_terminal_kind)
        {
            bail!("provider recovery source turn mismatch");
        }
        require_complete_candidates(&source_message.id, &source_turn.turn_id)?;
        let mut attempts = state.attempts.values().filter(|attempt| {
            attempt.agent_id == message.agent_id
                && attempt.source_message_id.as_deref() == Some(source_message.id.as_str())
                && attempt.turn_id.as_deref() == Some(source_turn.turn_id.as_str())
                && matches!(
                    attempt.state,
                    ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
                )
                && attempt.terminal_outcome_id.as_ref().is_some_and(|id| {
                    state
                        .outcomes
                        .get(id)
                        .is_some_and(|outcome| outcome.attempt_id == attempt.attempt_id)
                })
        });
        let predecessor = attempts
            .next()
            .ok_or_else(|| anyhow!("provider recovery predecessor missing"))?;
        if attempts.next().is_some() {
            bail!("provider recovery predecessor ambiguous");
        }
        let owner = match &predecessor.binding {
            ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            ExecutionBinding::Conversation { interaction_id } => TurnOwner::Conversation {
                interaction_id: interaction_id.clone(),
            },
            ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            },
            ExecutionBinding::Command => TurnOwner::Command,
        };
        if source_turn.owner.as_ref() != Some(&owner)
            || cursor.work_item_id.as_deref() != owner.work_item_id()
            || expected_binding
                .as_ref()
                .is_some_and(|binding| binding != &predecessor.binding)
        {
            bail!("provider recovery binding mismatch");
        }
        expected_binding = Some(predecessor.binding.clone());
        if first.is_none() {
            first = Some((predecessor.clone(), source_turn));
        }
        if matches!(
            predecessor.source.identity,
            ExecutionSourceIdentity::RuntimeRecovery { .. }
        ) {
            let prior_id = predecessor
                .recovery_of_attempt_id
                .as_ref()
                .ok_or_else(|| anyhow!("provider recovery predecessor lineage missing"))?;
            let prior = state
                .attempts
                .get(prior_id)
                .ok_or_else(|| anyhow!("provider recovery prior attempt missing"))?;
            let prior_selection = super::turn::TurnModelSelection::from_message(&source_message)
                .map_err(|_| anyhow!("provider recovery prior directive malformed"))?;
            let prior_directive = prior_selection
                .recovery
                .as_ref()
                .ok_or_else(|| anyhow!("provider recovery prior directive missing"))?;
            if prior.source_message_id.as_deref()
                != Some(prior_directive.source_message_id.as_str())
                || prior.turn_id.as_deref() != Some(prior_directive.source_turn_id.as_str())
            {
                bail!("provider recovery predecessor lineage mismatch");
            }
            cursor = source_message;
        } else {
            if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(
                &source_message,
            ) {
                bail!("provider recovery root has inconsistent execution identity");
            }
            let (predecessor, source_turn) = first.expect("first source is resolved");
            return Ok(ProviderRecoverySource {
                predecessor,
                source_turn,
                root_message: source_message,
            });
        }
    }
}

/// Ordinary loaded admission uses one owner-captured finite domain. Source
/// indexes prove the candidate set complete; actual input assignment still
/// proves the Message→Turn relation, independently of payload replay hints.
pub(crate) fn resolve_from_captured(
    inputs: &super::resident_input_evidence::CapturedInputEvidence,
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
) -> Result<ProviderRecoverySource> {
    use super::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceReadError,
    };
    if inputs.scope.agent_id != message.agent_id || state.agent_id != message.agent_id {
        bail!("provider recovery captured scope mismatch");
    }
    resolve_with_source_relation(
        message,
        state,
        |id| captured_message(inputs, id),
        |id| captured_turn(inputs, id),
        |source, turn| {
            let assigned = inputs.message_turn_keys.get(&source.id).ok_or_else(|| {
                InputEvidenceReadError::NeedsPreparation(Key::new(Family::Message, &source.id))
            })?;
            Ok(assigned.as_deref() == Some(turn.turn_id.as_str()))
        },
        |source, turn| {
            inputs.execution_predecessor_candidates(source, turn)?;
            Ok(())
        },
        |cursor| inputs.reject_proven_missing_recovery_source(cursor),
    )
}

pub(super) fn captured_message(
    inputs: &super::resident_input_evidence::CapturedInputEvidence,
    id: &str,
) -> Result<Option<MessageEnvelope>> {
    use super::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
    };
    match inputs.lookup(&Key::new(Family::Message, id))? {
        Some(record) => match &record.record {
            PreparedInputEvidence::Message(message) => Ok(Some(message.clone())),
            _ => bail!("captured Message family differs from its key"),
        },
        None => Ok(None),
    }
}

pub(super) fn captured_turn(
    inputs: &super::resident_input_evidence::CapturedInputEvidence,
    id: &str,
) -> Result<Option<TurnRecord>> {
    use super::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
    };
    match inputs.lookup(&Key::new(Family::Turn, id))? {
        Some(record) => match &record.record {
            PreparedInputEvidence::Turn(turn) => Ok(Some(turn.clone())),
            _ => bail!("captured Turn family differs from its key"),
        },
        None => Ok(None),
    }
}

/// Preparation errors describe the loaded domain, rather than invalid incoming
/// authority. A scheduler must not drop an accepted input for this condition.
pub(super) fn is_preparation_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<super::resident_input_evidence::InputEvidenceReadError>()
        .is_some()
        || error
            .downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(
            )
            .is_some()
}

#[cfg(test)]
pub(crate) fn resolve_for_context(
    storage: &AppStorage,
    message: &MessageEnvelope,
) -> Result<Option<ProviderRecoverySource>> {
    resolve_for_context_with_sources(
        message,
        || {
            let db = storage
                .runtime_db()?
                .ok_or_else(|| anyhow!("provider recovery database missing"))?;
            db.transitions()
                .load_execution_protocol_state_if_initialized(&message.agent_id)
        },
        |id| storage.read_message_by_id(id),
        |id| storage.read_turn_by_id(id),
        |id| crate::context::inputs::ContextInputs::assigned_turn_for_message(storage, id),
    )
}

pub(crate) fn resolve_for_context_with_sources(
    message: &MessageEnvelope,
    load_state: impl FnOnce() -> Result<Option<ExecutionProtocolState>>,
    read_message: impl FnMut(&str) -> Result<Option<MessageEnvelope>>,
    read_turn: impl FnMut(&str) -> Result<Option<TurnRecord>>,
    mut read_assignment: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<Option<ProviderRecoverySource>> {
    if !super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message) {
        return Ok(None);
    }
    let state =
        load_state()?.ok_or_else(|| anyhow!("provider recovery execution state missing"))?;
    resolve_with_source_relation(
        message,
        &state,
        read_message,
        read_turn,
        |source, turn| Ok(read_assignment(&source.id)?.as_deref() == Some(turn.turn_id.as_str())),
        |_, _| Ok(()),
        |_| Ok(()),
    )
    .map(Some)
}

// References are consumed only after durable resolution by context assembly.
pub(crate) fn continuity_turn_id(message: &MessageEnvelope) -> Option<&str> {
    super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message)
        .then(|| {
            message
                .source_refs
                .get("source_turn_id")
                .map(String::as_str)
        })
        .flatten()
}

#[cfg(test)]
mod context_assignment_tests {
    use super::*;
    use crate::{
        domain::execution_protocol::{
            self, AdmitExecution, AdmittedFences, ExecutionOrigin, ExecutionPriority,
            ExecutionProvenance, ExecutionSource, ExecutionTrust, InterruptExecution,
        },
        runtime_db::context_inputs::ContextDurableRead,
        types::{
            AdmissionContext, AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset,
            AgentVisibility, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
            MessageOrigin, Priority, TurnTerminalSummary, TurnTriggerSummary,
        },
    };

    #[test]
    fn context_continuity_uses_same_sql_assignment_and_never_falls_back_to_the_message_hint() {
        let directory = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(directory.path()).unwrap();
        let db = storage.runtime_db().unwrap().unwrap();
        db.agent_identities()
            .upsert(&AgentIdentityRecord::new(
                "default",
                AgentKind::Default,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))
            .unwrap();
        let mut source = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original provider input".into(),
            },
        );
        source.turn_id = Some("unstarted-preallocation-hint".into());
        storage.append_message(&source).unwrap();
        source = storage.read_message_by_id(&source.id).unwrap().unwrap();
        let now = chrono::Utc::now();
        let mut turn = TurnRecord::new("default", "actual-provider-turn", 1);
        turn.owner = Some(TurnOwner::AgentLifecycle {
            agent_id: "default".into(),
        });
        turn.trigger = Some(TurnTriggerSummary::from_message(&source));
        turn.input_message_ids = vec![source.id.clone()];
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::DeferredToFallback,
            reason: None,
            no_brief_reason: None,
            completed_at: now,
            duration_ms: 1,
        });
        storage.append_turn(&turn).unwrap();
        let sequence = source.message_seq.unwrap();
        let admitted = execution_protocol::admit_execution(
            &ExecutionProtocolState::empty("default"),
            &AdmitExecution {
                attempt: ExecutionAttempt {
                    attempt_id: "original-provider-attempt".into(),
                    agent_id: "default".into(),
                    source_message_id: Some(source.id.clone()),
                    source: ExecutionSource {
                        identity: ExecutionSourceIdentity::QueueMessage {
                            message_id: source.id.clone(),
                        },
                        generation: sequence,
                    },
                    binding: ExecutionBinding::AgentLifecycle {
                        agent_id: "default".into(),
                    },
                    provenance: ExecutionProvenance {
                        origin: ExecutionOrigin::Operator,
                        trust: ExecutionTrust::OperatorInstruction,
                        priority: ExecutionPriority::Normal,
                        correlation_id: None,
                        causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: sequence,
                        work_item_source_revision: None,
                        work_item_generation: None,
                        rejoin: None,
                        agent_control_revision: 1,
                        host_registry_revision: 1,
                    },
                    state: ExecutionAttemptState::Open,
                    run_id: None,
                    turn_id: Some(turn.turn_id.clone()),
                    recovery_of_attempt_id: None,
                    terminal_outcome_id: None,
                    admitted_at: now.to_rfc3339(),
                    terminal_at: None,
                },
            },
        )
        .unwrap()
        .state;
        let protocol = execution_protocol::interrupt_execution(
            &admitted,
            &InterruptExecution {
                attempt_id: "original-provider-attempt".into(),
                outcome_id: "provider-failure-outcome".into(),
                reason: "provider fallback".into(),
                interrupted_at: now.to_rfc3339(),
            },
        )
        .unwrap()
        .state;
        db.transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &protocol))
            .unwrap();
        let mut recovery = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "model_lineage_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "continue original provider input".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        recovery.causation_id = Some(source.id.clone());
        recovery
            .source_refs
            .insert("source_message_id".into(), source.id.clone());
        recovery
            .source_refs
            .insert("source_turn_id".into(), turn.turn_id.clone());
        recovery.metadata = Some(serde_json::json!({"provider_recovery": {
            "fallback_model_ref":"anthropic/claude-sonnet-4-6", "source_message_id": source.id,
            "source_turn_id":turn.turn_id, "source_terminal_kind":"deferred_to_fallback", "source_round":1
        }}));
        let mut connection = db.connection().unwrap();
        let tx = connection.transaction().unwrap();
        let inputs = ContextDurableRead::new(&tx, "default");
        let resolved = resolve_for_context_with_sources(
            &recovery,
            || inputs.execution_protocol_state("default"),
            |id| inputs.message_by_id(id),
            |id| inputs.turn_by_id(id),
            |id| inputs.assigned_turn_for_message(id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.source_turn.turn_id, turn.turn_id);
        assert_eq!(
            resolved.root_message.turn_id.as_deref(),
            Some("unstarted-preallocation-hint")
        );
        tx.commit().unwrap();
        // The loaded resolver consumes canonical rows qualified in one SQLite
        // view. The misleading payload hint remains unchanged for display.
        use crate::{
            runtime::resident_input_evidence::{
                InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
            },
            runtime_db::input_evidence_snapshot::{
                prepare_input_evidence_snapshot_tx, register_committed_input_evidence_tx,
                CanonicalInputEvidence, InputEvidenceBootstrapSnapshot, InputEvidenceLoadRequest,
            },
        };
        let captured = db
            .transaction(|tx| {
                register_committed_input_evidence_tx(
                    tx,
                    &InputEvidenceScope {
                        agent_id: "default".into(),
                        identity_incarnation: 1,
                    },
                    &[
                        CanonicalInputEvidence::Message(source.clone()),
                        CanonicalInputEvidence::Turn(
                            ContextDurableRead::new(tx, "default")
                                .turn_by_id(&turn.turn_id)?
                                .ok_or_else(|| {
                                    anyhow!("provider fixture canonical Turn missing")
                                })?,
                        ),
                        CanonicalInputEvidence::Attempt(
                            protocol.attempts["original-provider-attempt"].clone(),
                        ),
                        CanonicalInputEvidence::Outcome(
                            protocol.outcomes["provider-failure-outcome"].clone(),
                        ),
                    ],
                )?;
                let snapshot = prepare_input_evidence_snapshot_tx(
                    tx,
                    &InputEvidenceScope {
                        agent_id: "default".into(),
                        identity_incarnation: 1,
                    },
                    &InputEvidenceLoadRequest {
                        interrupted_sources: Default::default(),
                        windows: Vec::new(),
                        mandatory_roots: BTreeSet::from([
                            Key::new(Family::Message, &source.id),
                            Key::new(Family::Turn, &turn.turn_id),
                            Key::new(Family::Attempt, "original-provider-attempt"),
                            Key::new(Family::Outcome, "provider-failure-outcome"),
                        ]),
                        record_limit: 64,
                        byte_limit: 256 * 1024,
                    },
                )?;
                let InputEvidenceBootstrapSnapshot::Current(snapshot) = snapshot else {
                    bail!("provider fixture did not capture its current namespace");
                };
                snapshot.resident.capture()
            })
            .unwrap();
        let captured_protocol = captured.execution_state_if_initialized().unwrap().unwrap();
        let loaded = resolve_from_captured(&captured, &recovery, &captured_protocol).unwrap();
        assert_eq!(loaded.predecessor.attempt_id, "original-provider-attempt");
        assert_eq!(loaded.root_message.turn_id, source.turn_id);

        let mut incomplete = captured;
        let saved_sources = std::mem::take(&mut incomplete.execution.sources);
        let error = resolve_from_captured(&incomplete, &recovery, &captured_protocol).unwrap_err();
        assert!(is_preparation_error(&error), "{error:#}");
        incomplete.execution.sources = saved_sources;
        let saved_attempt = incomplete
            .records
            .iter()
            .position(|record| {
                record.locator.key == Key::new(Family::Attempt, "original-provider-attempt")
            })
            .map(|index| incomplete.records.remove(index))
            .unwrap();
        let error = resolve_from_captured(&incomplete, &recovery, &captured_protocol).unwrap_err();
        assert!(is_preparation_error(&error), "{error:#}");
        incomplete.records.push(saved_attempt);
        incomplete.message_turn_keys.remove(&source.id);
        let error = resolve_from_captured(&incomplete, &recovery, &captured_protocol).unwrap_err();
        assert!(is_preparation_error(&error), "{error:#}");
        incomplete.message_turn_keys.insert(source.id.clone(), None);
        assert!(resolve_from_captured(&incomplete, &recovery, &captured_protocol).is_err());

        // Admission compatibility is intentionally unchanged in this context slice.
        assert!(resolve(&storage, &recovery, &protocol).is_err());
        source.turn_id = Some(turn.turn_id.clone());
        db.transaction(|tx| {
            crate::runtime_db::evidence::upsert_message_tx(tx, &source)?;
            tx.execute(
                "DELETE FROM conversation_input_assignments WHERE message_id=?1",
                [&source.id],
            )?;
            Ok(())
        })
        .unwrap();
        let tx = connection.transaction().unwrap();
        let inputs = ContextDurableRead::new(&tx, "default");
        assert!(
            resolve_for_context_with_sources(
                &recovery,
                || inputs.execution_protocol_state("default"),
                |id| inputs.message_by_id(id),
                |id| inputs.turn_by_id(id),
                |id| inputs.assigned_turn_for_message(id)
            )
            .is_err(),
            "matching raw hint hid the missing actual assignment"
        );
    }
}
