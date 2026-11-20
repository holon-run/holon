use super::super::execution::{ExecutionSourceCoverage, SourceAttemptObservation};
use super::*;
use crate::domain::execution_protocol::{
    AdmittedFences, ConversationOutcome, ExecutionBinding, ExecutionOrigin, ExecutionOutcome,
    ExecutionPriority, ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity,
    ExecutionTrust,
};
use crate::types::{MessageBody, MessageKind, MessageOrigin, Priority};

fn qualified(record: PreparedInputEvidence) -> Arc<QualifiedInputEvidence> {
    let key = record.key();
    let mut qualified = turn(&key.id, 1).as_ref().clone();
    qualified.attempt_turn_keys =
        matches!(record, PreparedInputEvidence::Attempt(_)).then(BTreeSet::new);
    qualified.locator.key = key;
    qualified.record = record;
    qualified.turn_index = None;
    qualified.owner = None;
    Arc::new(qualified)
}

#[test]
fn mixed_mandatory_roots_preserve_execution_frontier_and_reject_missing_descendants() -> Result<()>
{
    let original = resident(true);
    let mut coverage = original.coverage.clone();
    let mut records = original.records.values().cloned().collect::<Vec<_>>();
    let mut message = MessageEnvelope::new(
        "agent-a",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "input".into(),
        },
    );
    message.id = "source".into();
    records.push(qualified(PreparedInputEvidence::Message(message)));
    coverage.message_turn_keys.insert("source".into(), None);
    coverage.mandatory_roots = BTreeSet::from([
        InputEvidenceKey::new(InputEvidenceFamily::Message, "source"),
        InputEvidenceKey::new(InputEvidenceFamily::Turn, "a"),
    ]);
    let mut attempts = BTreeMap::new();
    for (index, (id, state, turn, outcome, pruned)) in [
        (
            "settled",
            ExecutionAttemptState::Settled,
            Some("a"),
            true,
            false,
        ),
        (
            "interrupted",
            ExecutionAttemptState::Interrupted,
            Some("a"),
            true,
            false,
        ),
        (
            "no-outcome",
            ExecutionAttemptState::Interrupted,
            Some("a"),
            false,
            false,
        ),
        (
            "other-turn",
            ExecutionAttemptState::Settled,
            Some("b"),
            true,
            false,
        ),
        ("open", ExecutionAttemptState::Open, Some("a"), false, false),
        ("latest", ExecutionAttemptState::Open, None, false, false),
        (
            "pruned",
            ExecutionAttemptState::Settled,
            Some("a"),
            true,
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let attempt = ExecutionAttempt {
            input_projection: None,
            attempt_id: id.into(),
            agent_id: "agent-a".into(),
            source_message_id: Some("source".into()),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: "source".into(),
                },
                generation: index as u64 + 1,
            },
            binding: ExecutionBinding::AgentLifecycle {
                agent_id: "agent-a".into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::RuntimeRecovery,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state,
            run_id: None,
            turn_id: turn.map(str::to_owned),
            recovery_of_attempt_id: None,
            terminal_outcome_id: outcome.then(|| format!("outcome-{id}")),
            admitted_at: format!("2026-10-12T00:00:{index:02}Z"),
            terminal_at: outcome.then(|| "2026-10-12T00:01:00Z".into()),
        };
        let mut observation = SourceAttemptObservation::from_attempt(&attempt, index as u64 + 1);
        observation.pruned = pruned;
        attempts.insert(id.into(), observation);
        if !pruned {
            if let Some(outcome_id) = &attempt.terminal_outcome_id {
                records.push(qualified(PreparedInputEvidence::Outcome(
                    ExecutionOutcomeRecord {
                        outcome_id: outcome_id.clone(),
                        attempt_id: id.into(),
                        outcome: ExecutionOutcome::Conversation(
                            if state == ExecutionAttemptState::Interrupted {
                                ConversationOutcome::Interrupted {
                                    reason: "interrupted".into(),
                                }
                            } else {
                                ConversationOutcome::Replied
                            },
                        ),
                        created_at: "2026-10-12T00:01:00Z".into(),
                    },
                )));
            }
            records.push(qualified(PreparedInputEvidence::Attempt(attempt)));
        }
    }
    coverage.execution.sources.insert(
        "source".into(),
        ExecutionSourceCoverage {
            source_message_id: "source".into(),
            revision: 1,
            allocation_highwater: attempts.len() as u64,
            attempts,
        },
    );
    coverage.record_limit = 64;
    coverage.byte_limit = 65536;
    let resident = ResidentInputEvidence::bootstrap(scope(), records, coverage)?;
    let expected = [
        (InputEvidenceFamily::Message, "source"),
        (InputEvidenceFamily::Turn, "a"),
        (InputEvidenceFamily::Turn, "b"),
        (InputEvidenceFamily::Attempt, "latest"),
        (InputEvidenceFamily::Attempt, "settled"),
        (InputEvidenceFamily::Attempt, "interrupted"),
        (InputEvidenceFamily::Outcome, "outcome-settled"),
        (InputEvidenceFamily::Outcome, "outcome-interrupted"),
    ]
    .into_iter()
    .map(|(family, id)| InputEvidenceKey::new(family, id))
    .collect::<BTreeSet<_>>();
    assert_eq!(resident.required_keys()?, expected);
    assert_eq!(*resident.validated_required_keys()?, expected);
    for key in expected {
        if key == InputEvidenceKey::new(InputEvidenceFamily::Turn, "b") {
            continue; // Optional window record, not a mandatory descendant.
        }
        for proven_unavailable in [false, true] {
            let mut missing = resident.clone();
            missing.records.remove(&key);
            if proven_unavailable {
                missing.coverage.unavailable.insert(key.clone());
            }
            missing.validated_keys = None;
            assert!(
                missing.validated_required_keys().is_err(),
                "{key:?}, unavailable={proven_unavailable}"
            );
        }
    }
    Ok(())
}
