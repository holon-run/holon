//! Pure preparation of one post-dispatch episode-memory transition.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::episode::{
    derive_boundary_reason, episode_phase_snapshot, finalize_episode_at,
    merge_into_active_episode_with_turn, should_start_next_episode, working_snapshot_is_empty,
};
use crate::types::{
    ActiveEpisodeBuilder, AgentState, AuditEvent, ClosureDecision, ContextEpisodeRecord,
    EpisodeBoundaryReason, MessageEnvelope, WorkingMemorySnapshot,
};

pub(crate) struct EpisodeMemoryPlanInput<'a> {
    /// Private state after the working-memory refresh has been prepared.
    pub agent: &'a AgentState,
    /// Durable dispatch binding survives reducer-only Stop clearing State fields.
    pub actual_dispatch_turn_id: &'a str,
    pub trigger: &'a MessageEnvelope,
    pub prior_closure: &'a ClosureDecision,
    pub current_closure: &'a ClosureDecision,
    pub previous_snapshot: &'a WorkingMemorySnapshot,
    pub current_snapshot: &'a WorkingMemorySnapshot,
}

/// Allocated once by the caller, before any retryable database callback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FrozenEpisodeMemoryValues {
    pub timestamp: DateTime<Utc>,
    pub initial_builder_id: String,
    pub fallback_builder_id: String,
    pub next_builder_id: String,
    pub audit_event_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryOutcome {
    pub episode_memory_changed: bool,
    pub business_fields_changed: bool,
    pub boundary_reason: Option<EpisodeBoundaryReason>,
    pub finalized_episode_id: Option<String>,
    pub previous_builder_id: Option<String>,
    pub active_builder_id: Option<String>,
    pub compression_epoch_before: u64,
    pub compression_epoch_after: u64,
    pub archived_episode_count_before: usize,
    pub archived_episode_count_after: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct EpisodeMemoryPlan {
    pub candidate: AgentState,
    pub finalized_episode: Option<ContextEpisodeRecord>,
    pub audit_events: Vec<AuditEvent>,
    pub outcome: EpisodeMemoryOutcome,
}

pub(crate) fn plan_episode_memory(
    input: EpisodeMemoryPlanInput<'_>,
    frozen: &FrozenEpisodeMemoryValues,
) -> EpisodeMemoryPlan {
    let mut candidate = input.agent.clone();
    let before = &input.agent.working_memory;
    let has_material_state =
        should_start_next_episode(input.current_snapshot, input.current_closure);
    let boundary = derive_boundary_reason(
        input.trigger,
        input.prior_closure,
        input.current_closure,
        input.previous_snapshot,
        input.current_snapshot,
        before
            .active_episode_builder
            .as_ref()
            .map(|builder| builder.start_turn_index)
            .unwrap_or(candidate.turn_index),
        candidate.turn_index,
    );
    let message_count = candidate.total_message_count;
    let mut changed = false;
    let mut finalized = None;

    if boundary == Some(EpisodeBoundaryReason::ActiveWorkSwitched) {
        finalized = finalize_active_episode(
            &mut candidate,
            EpisodeBoundaryReason::ActiveWorkSwitched,
            input.previous_snapshot,
            message_count,
            frozen,
        );
        changed = true;
        if has_material_state {
            let mut builder = frozen_builder(
                input.current_snapshot,
                message_count,
                candidate.turn_index.max(1),
                &frozen.next_builder_id,
                frozen.timestamp,
            );
            merge_into_active_episode_with_turn(
                &mut builder,
                candidate.turn_index,
                Some(input.actual_dispatch_turn_id),
                input.current_snapshot,
                message_count,
            );
            candidate.working_memory.active_episode_id = Some(builder.id.clone());
            candidate.working_memory.active_episode_builder = Some(builder);
        } else {
            candidate.working_memory.active_episode_id = None;
        }
    } else {
        if candidate.working_memory.active_episode_builder.is_none() && has_material_state {
            let builder = frozen_builder(
                episode_phase_snapshot(input.previous_snapshot, input.current_snapshot),
                message_count,
                candidate.turn_index.max(1),
                &frozen.initial_builder_id,
                frozen.timestamp,
            );
            candidate.working_memory.active_episode_id = Some(builder.id.clone());
            candidate.working_memory.active_episode_builder = Some(builder);
            changed = true;
        }
        if let Some(builder) = candidate.working_memory.active_episode_builder.as_mut() {
            merge_into_active_episode_with_turn(
                builder,
                candidate.turn_index,
                Some(input.actual_dispatch_turn_id),
                input.current_snapshot,
                message_count,
            );
            candidate.working_memory.active_episode_id = Some(builder.id.clone());
            changed = true;
        }
        if let Some(reason) = boundary {
            finalized = finalize_active_episode(
                &mut candidate,
                reason,
                input.previous_snapshot,
                message_count,
                frozen,
            );
            changed = true;
            if has_material_state {
                let builder = frozen_builder(
                    input.current_snapshot,
                    message_count,
                    candidate.turn_index.saturating_add(1).max(1),
                    &frozen.next_builder_id,
                    frozen.timestamp,
                );
                candidate.working_memory.active_episode_id = Some(builder.id.clone());
                candidate.working_memory.active_episode_builder = Some(builder);
            } else {
                candidate.working_memory.active_episode_id = None;
            }
        }
    }

    let audit_events = finalized
        .as_ref()
        .map(|record| AuditEvent {
            id: frozen.audit_event_id.clone(),
            event_seq: 0,
            event_log_epoch: String::new(),
            created_at: frozen.timestamp,
            kind: "episode_memory_finalized".into(),
            contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
            payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
            payload_schema_version: 1,
            data: serde_json::json!({
                "agent_id": candidate.id,
                "episode_id": record.id,
                "boundary_reason": record.boundary_reason,
                "turn_range": [record.start_turn_index, record.end_turn_index],
                "work_item_id": record.current_work_item_id,
            }),
        })
        .into_iter()
        .collect();
    let outcome = EpisodeMemoryOutcome {
        episode_memory_changed: changed,
        business_fields_changed: candidate != *input.agent,
        boundary_reason: boundary,
        finalized_episode_id: finalized.as_ref().map(|record| record.id.clone()),
        previous_builder_id: before
            .active_episode_builder
            .as_ref()
            .map(|builder| builder.id.clone()),
        active_builder_id: candidate.working_memory.active_episode_id.clone(),
        compression_epoch_before: before.compression_epoch,
        compression_epoch_after: candidate.working_memory.compression_epoch,
        archived_episode_count_before: before.archived_episode_count,
        archived_episode_count_after: candidate.working_memory.archived_episode_count,
    };
    EpisodeMemoryPlan {
        candidate,
        finalized_episode: finalized,
        audit_events,
        outcome,
    }
}

fn finalize_active_episode(
    candidate: &mut AgentState,
    reason: EpisodeBoundaryReason,
    previous_snapshot: &WorkingMemorySnapshot,
    message_count: usize,
    frozen: &FrozenEpisodeMemoryValues,
) -> Option<ContextEpisodeRecord> {
    let builder = match candidate.working_memory.active_episode_builder.take() {
        Some(builder) => builder,
        None if working_snapshot_is_empty(previous_snapshot) => {
            candidate.working_memory.active_episode_id = None;
            return None;
        }
        None => frozen_builder(
            previous_snapshot,
            message_count,
            candidate.turn_index.saturating_sub(1).max(1),
            &frozen.fallback_builder_id,
            frozen.timestamp,
        ),
    };
    let record = finalize_episode_at(candidate, builder, reason, frozen.timestamp);
    candidate.working_memory.active_episode_id = None;
    candidate.working_memory.archived_episode_count += 1;
    candidate.working_memory.compression_epoch =
        candidate.working_memory.compression_epoch.saturating_add(1);
    Some(record)
}

fn frozen_builder(
    snapshot: &WorkingMemorySnapshot,
    message_count: usize,
    start_turn_index: u64,
    id: &str,
    timestamp: DateTime<Utc>,
) -> ActiveEpisodeBuilder {
    ActiveEpisodeBuilder {
        id: id.to_owned(),
        started_at: timestamp,
        start_turn_index,
        latest_turn_index: start_turn_index,
        start_message_count: message_count,
        latest_message_count: message_count,
        current_work_item_id: snapshot.current_work_item_id.clone(),
        objective: snapshot.objective.clone(),
        work_summary: snapshot.work_summary.clone(),
        scope_hints: snapshot.scope_hints.clone(),
        working_set_files: snapshot.working_set_files.clone(),
        carry_forward: snapshot.pending_followups.clone(),
        waiting_on: snapshot.waiting_on.clone(),
        source_turn_ids: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AuthorityClass, ClosureOutcome, MessageBody, MessageKind, MessageOrigin, Priority,
        RuntimePosture, WaitingReason,
    };

    pub(super) fn frozen() -> FrozenEpisodeMemoryValues {
        FrozenEpisodeMemoryValues {
            timestamp: "2026-10-12T00:00:00Z".parse().unwrap(),
            initial_builder_id: "episode-initial".into(),
            fallback_builder_id: "episode-fallback".into(),
            next_builder_id: "episode-next".into(),
            audit_event_id: "audit-finalized".into(),
        }
    }

    fn closure(waiting: bool) -> ClosureDecision {
        ClosureDecision {
            outcome: if waiting {
                ClosureOutcome::Waiting
            } else {
                ClosureOutcome::Completed
            },
            waiting_reason: waiting.then_some(WaitingReason::AwaitingOperatorInput),
            work_signal: None,
            runtime_posture: if waiting {
                RuntimePosture::Sleeping
            } else {
                RuntimePosture::Awake
            },
            evidence: Vec::new(),
        }
    }

    fn snapshot(id: &str) -> WorkingMemorySnapshot {
        WorkingMemorySnapshot {
            current_work_item_id: Some(id.into()),
            objective: Some(id.into()),
            work_summary: Some(id.into()),
            ..Default::default()
        }
    }

    fn trigger(kind: MessageKind) -> MessageEnvelope {
        MessageEnvelope::new(
            "agent-a",
            kind,
            MessageOrigin::System {
                subsystem: "test".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        )
    }

    #[test]
    fn empty_dispatch_is_a_pure_noop_plan() {
        let agent = AgentState::new("agent-a");
        let trigger = trigger(MessageKind::InternalFollowup);
        let closure = closure(false);
        let snapshot = WorkingMemorySnapshot::default();
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger,
                prior_closure: &closure,
                current_closure: &closure,
                previous_snapshot: &snapshot,
                current_snapshot: &snapshot,
            },
            &frozen(),
        );
        assert_eq!(plan.candidate, agent);
        assert!(!plan.outcome.business_fields_changed);
        assert!(plan.finalized_episode.is_none());
        assert!(plan.audit_events.is_empty());
    }

    #[test]
    fn active_work_switch_finalizes_before_merging_current_turn() {
        let frozen = frozen();
        let previous = snapshot("work-a");
        let current = snapshot("work-b");
        let mut agent = AgentState::new("agent-a");
        agent.turn_index = 3;
        agent.current_turn_id = Some("turn-3".into());
        let mut builder = frozen_builder(&previous, 2, 1, "episode-prior", frozen.timestamp);
        builder.latest_turn_index = 2;
        builder.source_turn_ids = vec!["turn-1".into(), "turn-2".into()];
        agent.working_memory.active_episode_id = Some(builder.id.clone());
        agent.working_memory.active_episode_builder = Some(builder);
        let trigger = trigger(MessageKind::InternalFollowup);
        let closure = closure(false);
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger,
                prior_closure: &closure,
                current_closure: &closure,
                previous_snapshot: &previous,
                current_snapshot: &current,
            },
            &frozen,
        );
        let episode = plan.finalized_episode.as_ref().unwrap();
        assert_eq!(episode.end_turn_index, 2);
        assert_eq!(episode.source_turn_ids, ["turn-1", "turn-2"]);
        assert_eq!(episode.current_work_item_id.as_deref(), Some("work-a"));
        let next = plan
            .candidate
            .working_memory
            .active_episode_builder
            .as_ref()
            .unwrap();
        assert_eq!(next.start_turn_index, 3);
        assert_eq!(next.source_turn_ids, ["turn-3"]);
        assert_eq!(plan.outcome.compression_epoch_after, 1);
        assert_eq!(plan.outcome.archived_episode_count_after, 1);
        let repeated = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger,
                prior_closure: &closure,
                current_closure: &closure,
                previous_snapshot: &previous,
                current_snapshot: &current,
            },
            &frozen,
        );
        assert_eq!(plan.candidate, repeated.candidate);
        assert_eq!(plan.finalized_episode, repeated.finalized_episode);
        assert_eq!(plan.audit_events, repeated.audit_events);
    }

    #[test]
    fn wait_boundary_merges_current_turn_before_finalizing() {
        let mut agent = AgentState::new("agent-a");
        agent.turn_index = 4;
        agent.current_turn_id = Some("turn-4".into());
        let snapshot = snapshot("work-a");
        let trigger = trigger(MessageKind::InternalFollowup);
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger,
                prior_closure: &closure(false),
                current_closure: &closure(true),
                previous_snapshot: &snapshot,
                current_snapshot: &snapshot,
            },
            &frozen(),
        );
        let episode = plan.finalized_episode.unwrap();
        assert_eq!(episode.boundary_reason, EpisodeBoundaryReason::WaitBoundary);
        assert_eq!(episode.source_turn_ids, ["turn-4"]);
        let next = plan
            .candidate
            .working_memory
            .active_episode_builder
            .unwrap();
        assert_eq!(next.start_turn_index, 5);
        assert!(next.source_turn_ids.is_empty());
    }

    #[test]
    fn stop_cleared_state_still_records_the_actual_dispatch_turn() {
        let mut agent = AgentState::new("agent-a");
        agent.turn_index = 4;
        agent.current_turn_id = None;
        let snapshot = snapshot("work-a");
        let trigger = trigger(MessageKind::Control);
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: "stop-dispatch-4",
                trigger: &trigger,
                prior_closure: &closure(false),
                current_closure: &closure(true),
                previous_snapshot: &snapshot,
                current_snapshot: &snapshot,
            },
            &frozen(),
        );
        let episode = plan.finalized_episode.as_ref().unwrap();
        assert_eq!(episode.source_turn_ids, ["stop-dispatch-4"]);
        assert_eq!(episode.source_refs, ["turn:stop-dispatch-4"]);
        assert!(plan.candidate.current_turn_id.is_none());
    }

    #[test]
    fn task_rejoin_precedes_active_work_switch() {
        let mut agent = AgentState::new("agent-a");
        agent.turn_index = 8;
        agent.current_turn_id = Some("turn-8".into());
        let previous = snapshot("work-a");
        let current = snapshot("work-b");
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger(MessageKind::TaskResult),
                prior_closure: &closure(true),
                current_closure: &closure(false),
                previous_snapshot: &previous,
                current_snapshot: &current,
            },
            &frozen(),
        );
        assert_eq!(
            plan.finalized_episode.unwrap().boundary_reason,
            EpisodeBoundaryReason::TaskRejoined
        );
        assert_eq!(
            plan.candidate
                .working_memory
                .active_episode_builder
                .unwrap()
                .start_turn_index,
            9
        );
    }

    #[test]
    fn hard_turn_cap_preserves_the_existing_boundary_and_bounded_source_list() {
        let mut agent = AgentState::new("agent-a");
        let snapshot = snapshot("work-a");
        agent.turn_index = 13;
        agent.current_turn_id = Some("turn-13".into());
        let mut builder = frozen_builder(&snapshot, 1, 1, "episode-prior", frozen().timestamp);
        builder.latest_turn_index = 12;
        builder.source_turn_ids = (1..=12).map(|index| format!("turn-{index}")).collect();
        agent.working_memory.active_episode_id = Some(builder.id.clone());
        agent.working_memory.active_episode_builder = Some(builder);
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger(MessageKind::InternalFollowup),
                prior_closure: &closure(false),
                current_closure: &closure(false),
                previous_snapshot: &snapshot,
                current_snapshot: &snapshot,
            },
            &frozen(),
        );
        let episode = plan.finalized_episode.unwrap();
        assert_eq!(episode.boundary_reason, EpisodeBoundaryReason::HardTurnCap);
        assert_eq!(episode.end_turn_index, 13);
        assert_eq!(episode.source_turn_ids.len(), 12);
        assert!(!episode.source_turn_ids.iter().any(|id| id == "turn-13"));
        assert_eq!(
            plan.candidate
                .working_memory
                .active_episode_builder
                .unwrap()
                .start_turn_index,
            14
        );
    }

    #[test]
    fn fallback_work_switch_archive_preserves_empty_legacy_source_evidence() {
        let mut agent = AgentState::new("agent-a");
        agent.turn_index = 2;
        agent.current_turn_id = Some("turn-2".into());
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &agent,
                actual_dispatch_turn_id: agent
                    .current_turn_id
                    .as_deref()
                    .unwrap_or("dispatch-test"),
                trigger: &trigger(MessageKind::InternalFollowup),
                prior_closure: &closure(false),
                current_closure: &closure(false),
                previous_snapshot: &snapshot("work-a"),
                current_snapshot: &snapshot("work-b"),
            },
            &frozen(),
        );
        let episode = plan.finalized_episode.unwrap();
        assert_eq!(episode.id, "episode-fallback");
        assert_eq!((episode.start_turn_index, episode.end_turn_index), (1, 1));
        assert!(episode.source_turn_ids.is_empty());
        assert!(episode.source_refs.is_empty());
        assert_eq!(episode.generated_by.unwrap().model, None);
    }
}
