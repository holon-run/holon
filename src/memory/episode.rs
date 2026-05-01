use anyhow::Result;
use chrono::Utc;

use crate::{
    storage::AppStorage,
    types::{
        ActiveEpisodeBuilder, AgentState, ClosureDecision, ClosureOutcome, ContextEpisodeRecord,
        EpisodeBoundaryReason, MessageEnvelope, MessageKind, RuntimePosture, TurnMemoryDelta,
        WorkingMemorySnapshot,
    },
};

const EPISODE_FILE_LIMIT: usize = 10;
const EPISODE_COMMAND_LIMIT: usize = 8;
const EPISODE_VERIFICATION_LIMIT: usize = 8;
const EPISODE_DECISION_LIMIT: usize = 8;
const EPISODE_CARRY_FORWARD_LIMIT: usize = 8;
const EPISODE_TURN_HARD_CAP: u64 = 12;

pub fn refresh_episode_memory(
    storage: &AppStorage,
    agent: &mut AgentState,
    trigger: &MessageEnvelope,
    prior_closure: &ClosureDecision,
    current_closure: &ClosureDecision,
    previous_snapshot: &WorkingMemorySnapshot,
    current_snapshot: &WorkingMemorySnapshot,
    turn_delta: &TurnMemoryDelta,
) -> Result<bool> {
    let has_material_state =
        should_start_next_episode(current_snapshot, current_closure, turn_delta);
    let boundary = derive_boundary_reason(
        trigger,
        prior_closure,
        current_closure,
        previous_snapshot,
        current_snapshot,
        turn_delta,
        agent
            .working_memory
            .active_episode_builder
            .as_ref()
            .map(|builder| builder.start_turn_index)
            .unwrap_or(agent.turn_index),
        agent.turn_index,
    );

    let phase_snapshot = episode_phase_snapshot(previous_snapshot, current_snapshot);
    let message_count = agent.total_message_count;
    let mut changed = false;

    if boundary == Some(EpisodeBoundaryReason::ActiveWorkSwitched) {
        finalize_active_episode_before_merge(
            storage,
            agent,
            EpisodeBoundaryReason::ActiveWorkSwitched,
            previous_snapshot,
            message_count,
        )?;
        changed = true;

        if has_material_state {
            let mut next_builder = ActiveEpisodeBuilder::new_with_start(
                agent.id.clone(),
                current_snapshot,
                message_count,
                agent.turn_index.max(1),
            );
            merge_into_active_episode(
                &mut next_builder,
                turn_delta,
                current_snapshot,
                message_count,
            );
            agent.working_memory.active_episode_id = Some(next_builder.id.clone());
            agent.working_memory.active_episode_builder = Some(next_builder);
        } else {
            agent.working_memory.active_episode_id = None;
        }
        return Ok(changed);
    }

    if agent.working_memory.active_episode_builder.is_none() && has_material_state {
        let builder = ActiveEpisodeBuilder::new(agent, phase_snapshot, message_count);
        agent.working_memory.active_episode_id = Some(builder.id.clone());
        agent.working_memory.active_episode_builder = Some(builder);
        changed = true;
    }

    if let Some(builder) = agent.working_memory.active_episode_builder.as_mut() {
        merge_into_active_episode(builder, turn_delta, current_snapshot, message_count);
        agent.working_memory.active_episode_id = Some(builder.id.clone());
        changed = true;
    }

    if let Some(reason) = boundary {
        finalize_active_episode_before_merge(
            storage,
            agent,
            reason,
            previous_snapshot,
            message_count,
        )?;
        changed = true;

        if has_material_state {
            let next_builder = ActiveEpisodeBuilder::new_with_start(
                agent.id.clone(),
                current_snapshot,
                message_count,
                agent.turn_index.saturating_add(1).max(1),
            );
            agent.working_memory.active_episode_id = Some(next_builder.id.clone());
            agent.working_memory.active_episode_builder = Some(next_builder);
        } else {
            agent.working_memory.active_episode_id = None;
        }
    }

    Ok(changed)
}

fn episode_phase_snapshot<'a>(
    previous_snapshot: &'a WorkingMemorySnapshot,
    current_snapshot: &'a WorkingMemorySnapshot,
) -> &'a WorkingMemorySnapshot {
    if working_snapshot_is_empty(previous_snapshot) {
        current_snapshot
    } else {
        previous_snapshot
    }
}

fn merge_into_active_episode(
    builder: &mut ActiveEpisodeBuilder,
    turn_delta: &TurnMemoryDelta,
    current_snapshot: &WorkingMemorySnapshot,
    message_count: usize,
) {
    builder.latest_turn_index = turn_delta.turn_index.max(builder.latest_turn_index);
    builder.latest_message_count = message_count.max(builder.latest_message_count);

    merge_unique(
        &mut builder.working_set_files,
        &turn_delta.touched_files,
        EPISODE_FILE_LIMIT,
    );
    merge_unique(
        &mut builder.commands,
        &turn_delta.commands,
        EPISODE_COMMAND_LIMIT,
    );
    merge_unique(
        &mut builder.verification,
        &turn_delta.verification,
        EPISODE_VERIFICATION_LIMIT,
    );
    merge_unique(
        &mut builder.decisions,
        &turn_delta.decisions,
        EPISODE_DECISION_LIMIT,
    );
    merge_unique(
        &mut builder.carry_forward,
        &turn_delta.pending_followups,
        EPISODE_CARRY_FORWARD_LIMIT,
    );
    merge_unique(
        &mut builder.waiting_on,
        &turn_delta.waiting_on,
        EPISODE_CARRY_FORWARD_LIMIT,
    );

    if builder.current_work_item_id.is_none() {
        builder.current_work_item_id = current_snapshot.current_work_item_id.clone();
    }
    if builder.delivery_target.is_none() {
        builder.delivery_target = current_snapshot.delivery_target.clone();
    }
    if builder.work_summary.is_none() {
        builder.work_summary = current_snapshot.work_summary.clone();
    }
}

fn finalize_active_episode_before_merge(
    storage: &AppStorage,
    agent: &mut AgentState,
    boundary_reason: EpisodeBoundaryReason,
    previous_snapshot: &WorkingMemorySnapshot,
    message_count: usize,
) -> Result<()> {
    let builder = if let Some(builder) = agent.working_memory.active_episode_builder.take() {
        builder
    } else if working_snapshot_is_empty(previous_snapshot) {
        agent.working_memory.active_episode_id = None;
        return Ok(());
    } else {
        ActiveEpisodeBuilder::new_with_start(
            agent.id.clone(),
            previous_snapshot,
            message_count,
            agent.turn_index.saturating_sub(1).max(1),
        )
    };

    let record = finalize_episode(agent, builder, boundary_reason);
    storage.append_context_episode(&record)?;
    storage.append_event(&crate::types::AuditEvent::new(
        "episode_memory_finalized",
        serde_json::json!({
            "agent_id": agent.id,
            "episode_id": record.id,
            "boundary_reason": record.boundary_reason,
            "turn_range": [record.start_turn_index, record.end_turn_index],
            "work_item_id": record.current_work_item_id,
        }),
    ))?;
    agent.working_memory.active_episode_id = None;
    agent.working_memory.archived_episode_count += 1;
    agent.working_memory.compression_epoch =
        agent.working_memory.compression_epoch.saturating_add(1);
    Ok(())
}

fn derive_boundary_reason(
    trigger: &MessageEnvelope,
    prior_closure: &ClosureDecision,
    current_closure: &ClosureDecision,
    previous_snapshot: &WorkingMemorySnapshot,
    current_snapshot: &WorkingMemorySnapshot,
    turn_delta: &TurnMemoryDelta,
    builder_start_turn: u64,
    current_turn_index: u64,
) -> Option<EpisodeBoundaryReason> {
    if matches!(
        trigger.kind,
        MessageKind::TaskResult | MessageKind::TaskStatus
    ) && (turn_delta.active_work_changed
        || prior_closure.waiting_reason.is_some()
        || !turn_delta.pending_followups.is_empty()
        || !turn_delta.waiting_on.is_empty())
    {
        return Some(EpisodeBoundaryReason::TaskRejoined);
    }

    if !working_snapshot_is_empty(previous_snapshot)
        && (previous_snapshot.current_work_item_id != current_snapshot.current_work_item_id
            || previous_snapshot.delivery_target != current_snapshot.delivery_target
            || previous_snapshot.work_summary != current_snapshot.work_summary)
    {
        return Some(EpisodeBoundaryReason::ActiveWorkSwitched);
    }

    if current_closure.outcome == ClosureOutcome::Waiting
        && (prior_closure.outcome != ClosureOutcome::Waiting
            || prior_closure.waiting_reason != current_closure.waiting_reason
            || current_closure.runtime_posture == RuntimePosture::Sleeping)
    {
        return Some(EpisodeBoundaryReason::WaitBoundary);
    }

    if current_turn_index.saturating_sub(builder_start_turn) >= EPISODE_TURN_HARD_CAP {
        return Some(EpisodeBoundaryReason::HardTurnCap);
    }

    None
}

fn should_start_next_episode(
    snapshot: &WorkingMemorySnapshot,
    current_closure: &ClosureDecision,
    turn_delta: &TurnMemoryDelta,
) -> bool {
    !working_snapshot_is_empty(snapshot)
        || current_closure.outcome == ClosureOutcome::Waiting
        || !turn_delta.touched_files.is_empty()
        || !turn_delta.pending_followups.is_empty()
}

fn finalize_episode(
    agent: &AgentState,
    builder: ActiveEpisodeBuilder,
    boundary_reason: EpisodeBoundaryReason,
) -> ContextEpisodeRecord {
    let finalized_at = Utc::now();
    let summary = render_episode_summary(&builder);

    ContextEpisodeRecord {
        id: builder.id,
        agent_id: agent.id.clone(),
        workspace_id: agent
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.workspace_id.clone())
            .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string()),
        created_at: builder.started_at,
        finalized_at,
        start_turn_index: builder.start_turn_index,
        end_turn_index: builder.latest_turn_index,
        start_message_count: builder.start_message_count,
        end_message_count: builder.latest_message_count,
        boundary_reason,
        current_work_item_id: builder.current_work_item_id,
        delivery_target: builder.delivery_target,
        work_summary: builder.work_summary,
        scope_hints: builder.scope_hints,
        summary,
        working_set_files: builder.working_set_files,
        commands: builder.commands,
        verification: builder.verification,
        decisions: builder.decisions,
        carry_forward: builder.carry_forward,
        waiting_on: builder.waiting_on,
    }
}

fn render_episode_summary(builder: &ActiveEpisodeBuilder) -> String {
    let mut lines = Vec::new();
    if let Some(target) = builder
        .work_summary
        .as_ref()
        .or(builder.delivery_target.as_ref())
    {
        lines.push(format!("work: {}", truncate_line(target, 120)));
    }
    if !builder.scope_hints.is_empty() {
        lines.push(format!(
            "scope: {}",
            builder
                .scope_hints
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !builder.working_set_files.is_empty() {
        lines.push(format!(
            "files: {}",
            builder
                .working_set_files
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !builder.verification.is_empty() {
        lines.push(format!(
            "verification: {}",
            truncate_line(&builder.verification[0], 120)
        ));
    }
    if !builder.carry_forward.is_empty() {
        lines.push(format!(
            "carry-forward: {}",
            truncate_line(&builder.carry_forward[0], 120)
        ));
    }
    lines.join("\n")
}

fn merge_unique(target: &mut Vec<String>, additions: &[String], limit: usize) {
    for item in additions {
        if item.is_empty() || target.iter().any(|existing| existing == item) {
            continue;
        }
        if target.len() >= limit {
            break;
        }
        target.push(item.clone());
    }
}

fn truncate_line(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn working_snapshot_is_empty(snapshot: &WorkingMemorySnapshot) -> bool {
    snapshot == &WorkingMemorySnapshot::default()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::types::{
        ClosureOutcome, MessageBody, MessageOrigin, Priority, RuntimePosture, TrustLevel,
        WaitingReason,
    };

    fn message(kind: MessageKind) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            kind,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        )
    }

    fn closure(waiting: Option<WaitingReason>) -> ClosureDecision {
        ClosureDecision {
            outcome: if waiting.is_some() {
                ClosureOutcome::Waiting
            } else {
                ClosureOutcome::Completed
            },
            waiting_reason: waiting,
            work_signal: None,
            runtime_posture: if waiting.is_some() {
                RuntimePosture::Sleeping
            } else {
                RuntimePosture::Awake
            },
            evidence: Vec::new(),
        }
    }

    fn snapshot(work_id: &str, summary: &str) -> WorkingMemorySnapshot {
        WorkingMemorySnapshot {
            current_work_item_id: Some(work_id.into()),
            delivery_target: Some(summary.into()),
            work_summary: Some(summary.into()),
            ..WorkingMemorySnapshot::default()
        }
    }

    #[test]
    fn refresh_episode_memory_finalizes_on_active_work_switch() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.turn_index = 3;
        agent.total_message_count = 9;

        let previous = snapshot("work_a", "fix exporter");
        let current = snapshot("work_b", "review CI");
        let delta = TurnMemoryDelta {
            turn_index: 3,
            touched_files: vec!["src/export.rs".into()],
            commands: vec!["cargo test --test metrics_export".into()],
            verification: vec!["cargo test --test metrics_export passed".into()],
            pending_followups: vec!["run full suite".into()],
            ..TurnMemoryDelta::default()
        };

        let changed = refresh_episode_memory(
            &storage,
            &mut agent,
            &message(MessageKind::OperatorPrompt),
            &closure(None),
            &closure(None),
            &previous,
            &current,
            &delta,
        )
        .unwrap();

        assert!(changed);
        assert_eq!(agent.working_memory.archived_episode_count, 1);
        let episodes = storage.read_recent_context_episodes(4).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(
            episodes[0].boundary_reason,
            EpisodeBoundaryReason::ActiveWorkSwitched
        );
        assert_eq!(episodes[0].current_work_item_id.as_deref(), Some("work_a"));
        assert!(episodes[0].verification.is_empty());
        assert!(episodes[0].scope_hints.is_empty());
        let next_builder = agent
            .working_memory
            .active_episode_builder
            .as_ref()
            .expect("next builder should be present");
        assert!(next_builder
            .verification
            .iter()
            .any(|line| line.contains("passed")));
        assert!(next_builder.scope_hints.is_empty());
        assert_eq!(next_builder.current_work_item_id.as_deref(), Some("work_b"));
    }

    #[test]
    fn refresh_episode_memory_finalizes_on_wait_boundary() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.turn_index = 6;
        agent.total_message_count = 14;

        let previous = snapshot("work_a", "stabilize runtime");
        let current = WorkingMemorySnapshot {
            waiting_on: vec!["wait for reviewer".into()],
            pending_followups: vec!["resume after review".into()],
            ..previous.clone()
        };
        let delta = TurnMemoryDelta {
            turn_index: 6,
            decisions: vec!["defer follow-up until review lands".into()],
            pending_followups: current.pending_followups.clone(),
            waiting_on: current.waiting_on.clone(),
            ..TurnMemoryDelta::default()
        };

        refresh_episode_memory(
            &storage,
            &mut agent,
            &message(MessageKind::SystemTick),
            &closure(None),
            &closure(Some(crate::types::WaitingReason::AwaitingExternalChange)),
            &previous,
            &current,
            &delta,
        )
        .unwrap();

        let episodes = storage.read_recent_context_episodes(4).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(
            episodes[0].boundary_reason,
            EpisodeBoundaryReason::WaitBoundary
        );
        assert!(episodes[0]
            .carry_forward
            .iter()
            .any(|item| item.contains("resume after review")));
        assert!(episodes[0]
            .waiting_on
            .iter()
            .any(|item| item.contains("wait for reviewer")));
    }

    #[test]
    fn refresh_episode_memory_skips_empty_turns_without_material_state() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.turn_index = 2;
        agent.total_message_count = 4;

        let changed = refresh_episode_memory(
            &storage,
            &mut agent,
            &message(MessageKind::SystemTick),
            &closure(None),
            &closure(None),
            &WorkingMemorySnapshot::default(),
            &WorkingMemorySnapshot::default(),
            &TurnMemoryDelta {
                turn_index: 2,
                ..TurnMemoryDelta::default()
            },
        )
        .unwrap();

        assert!(!changed);
        assert!(agent.working_memory.active_episode_builder.is_none());
        assert!(storage.read_recent_context_episodes(4).unwrap().is_empty());
    }

    #[test]
    fn refresh_episode_memory_starts_next_builder_after_boundary_turn() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.turn_index = 5;
        agent.total_message_count = 11;

        let previous = snapshot("work_a", "finish exporter");
        let current = snapshot("work_b", "stabilize runtime");
        let delta = TurnMemoryDelta {
            turn_index: 5,
            active_work_changed: true,
            ..TurnMemoryDelta::default()
        };

        refresh_episode_memory(
            &storage,
            &mut agent,
            &message(MessageKind::OperatorPrompt),
            &closure(None),
            &closure(None),
            &previous,
            &current,
            &delta,
        )
        .unwrap();

        let episodes = storage.read_recent_context_episodes(4).unwrap();
        assert_eq!(episodes[0].end_turn_index, 4);
        assert_eq!(
            agent
                .working_memory
                .active_episode_builder
                .as_ref()
                .map(|builder| builder.start_turn_index),
            Some(5)
        );
    }
}
