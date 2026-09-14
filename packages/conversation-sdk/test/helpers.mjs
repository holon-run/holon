export const identity = Object.freeze({
  remote_id: "remote-a",
  agent_id: "web",
  generation: 1,
});

export function turn(
  turnId,
  turnIndex,
  revision = 1,
  overrides = {},
) {
  return {
    turn_id: turnId,
    key: {
      turn_index: turnIndex,
      turn_id: turnId,
    },
    revision,
    presentation_class: "operator",
    execution: { kind: "active" },
    result: { kind: "pending" },
    settled: false,
    attention: null,
    detail_coverage: { kind: "complete" },
    brief_ids: [],
    ...overrides,
  };
}

export function activity(
  id,
  eventSeq,
  revision = 1,
  overrides = {},
) {
  return {
    kind: "assistant",
    id,
    key: {
      event_seq: eventSeq,
      activity_id: id,
    },
    revision,
    summary: `activity ${id}`,
    ...overrides,
  };
}

export function summary(overrides = {}) {
  return {
    schema_version: 1,
    query_version: 1,
    runtime_id: "runtime-a",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    snapshot_through_seq: 10,
    event_head_seq: 10,
    oldest_retained_seq: 1,
    snapshot_cursor: "checkpoint-10",
    turns: [turn("turn-10", 10)],
    active_turns: [],
    pending_inputs: [],
    next_before_cursor: "history-before-10",
    has_more: true,
    ...overrides,
  };
}

export function detail(overrides = {}) {
  const selectedTurn = overrides.turn ?? turn("turn-10", 10);
  return {
    schema_version: 1,
    query_version: 1,
    runtime_id: "runtime-a",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    snapshot_through_seq: 10,
    event_head_seq: 10,
    oldest_retained_seq: 1,
    snapshot_cursor: "checkpoint-10",
    turn: selectedTurn,
    detail_revision: 1,
    activities: [activity("activity-10", 10)],
    coverage: { kind: "complete" },
    next_before_cursor: null,
    has_more: false,
    ...overrides,
  };
}

export function batch({
  from = 10,
  through = 11,
  checkpoint = `checkpoint-${through}`,
  mutations = [],
  begin = {},
  end = {},
} = {}) {
  const batchId = `conversation:${from}:${through}`;
  return {
    begin: {
      type: "batch_begin",
      batch_id: batchId,
      schema_version: 1,
      query_version: 1,
      runtime_id: "runtime-a",
      event_log_epoch: "epoch-a",
      visibility_scope_id: "scope-a",
      from_seq: from,
      through_seq: through,
      ...begin,
    },
    mutations,
    checkpoint: {
      type: "checkpoint",
      batch_id: batchId,
      event_log_epoch: "epoch-a",
      visibility_scope_id: "scope-a",
      through_seq: through,
      checkpoint,
      ...end,
    },
  };
}

export function sseFrame(event, data, id) {
  return {
    event,
    data: JSON.stringify(data),
    ...(id === undefined ? {} : { id }),
  };
}
