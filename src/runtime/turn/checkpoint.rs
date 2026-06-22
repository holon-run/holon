use crate::types::{TurnTerminalCheckpointRecord, TurnTerminalKind, TurnTerminalRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnLocalCheckpointMode {
    Full,
    Delta,
}

impl TurnLocalCheckpointMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Delta => "delta",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TurnLocalCheckpointRequest {
    pub(super) request_id: Option<String>,
    pub(super) mode: TurnLocalCheckpointMode,
    pub(super) prompt: String,
    pub(super) previous_checkpoint_round: Option<usize>,
    pub(super) anchor_changed_since_checkpoint: bool,
    pub(super) anchor_generation: u64,
    pub(super) base_round: Option<usize>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct TurnLocalCheckpointState {
    pub(super) latest: Option<TurnLocalCheckpointRecord>,
    pub(super) pending: Option<PendingCheckpointRequest>,
    pub(super) anchor_generation: u64,
}

#[derive(Debug, Clone)]
pub(super) struct PendingCheckpointRequest {
    pub(super) request_id: String,
    pub(super) mode: TurnLocalCheckpointMode,
    pub(super) requested_at_round: usize,
    pub(super) anchor_generation: u64,
    pub(super) base_round: Option<usize>,
    pub(super) text_fragments: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct TurnLocalCheckpointRecord {
    pub(super) request_id: String,
    pub(super) requested_at_round: usize,
    pub(super) response_round: Option<usize>,
    pub(super) source_turn_index: Option<u64>,
    pub(super) mode: TurnLocalCheckpointMode,
    pub(super) text: String,
    pub(super) anchor_generation: u64,
}

pub(super) fn checkpoint_state_from_last_terminal(
    terminal: Option<&TurnTerminalRecord>,
) -> TurnLocalCheckpointState {
    let Some(terminal) = terminal else {
        return TurnLocalCheckpointState::default();
    };
    if terminal.kind != TurnTerminalKind::Completed {
        return TurnLocalCheckpointState::default();
    }
    let Some(checkpoint) = terminal.checkpoint.as_ref() else {
        return TurnLocalCheckpointState::default();
    };
    TurnLocalCheckpointState {
        latest: Some(TurnLocalCheckpointRecord {
            request_id: checkpoint.request_id.clone(),
            requested_at_round: checkpoint.requested_at_round,
            response_round: None,
            source_turn_index: checkpoint.source_turn_index.or(Some(terminal.turn_index)),
            mode: TurnLocalCheckpointMode::Full,
            text: checkpoint.text.clone(),
            anchor_generation: checkpoint.checkpoint_anchor_generation,
        }),
        pending: None,
        anchor_generation: checkpoint.current_anchor_generation,
    }
}

pub(super) fn terminal_checkpoint_from_state(
    checkpoint_state: &TurnLocalCheckpointState,
    terminal_turn_index: u64,
) -> Option<TurnTerminalCheckpointRecord> {
    let latest = checkpoint_state.latest.as_ref()?;
    Some(TurnTerminalCheckpointRecord {
        request_id: latest.request_id.clone(),
        requested_at_round: latest.requested_at_round,
        response_round: latest.response_round,
        source_turn_index: latest.source_turn_index.or(Some(terminal_turn_index)),
        text: latest.text.clone(),
        checkpoint_anchor_generation: latest.anchor_generation,
        current_anchor_generation: checkpoint_state.anchor_generation,
    })
}

pub(super) fn turn_optional_id_matches(candidate: Option<&str>, turn_id: &str) -> bool {
    candidate.is_some_and(|candidate| {
        let candidate = candidate.trim();
        !candidate.is_empty() && candidate == turn_id
    })
}
